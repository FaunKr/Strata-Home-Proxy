//! Password and sessions for the web UI.
//!
//! The default password is `admin`, or `UI_PASSWORD` if set. Until an own
//! password is saved, every login leads to "change password" and nothing else
//! works. The own password is kept as an Argon2 hash in `DATA_DIR`; deleting
//! that file brings the default back. Sessions are signed cookies valid for 30
//! days; changing the password ends all other sessions.

use std::{
    collections::HashMap,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng, rand_core::RngCore},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::files::write_private;

pub const DEFAULT_PASSWORD: &str = "admin";
pub const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
pub const MIN_PASSWORD_LEN: usize = 8;
/// Failed logins from one address before it has to wait.
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
const MAX_TRACKED_CLIENTS: usize = 1000;

const PASSWORD_FILE: &str = "admin-password.json";
const KEY_FILE: &str = "session-key";
const REVOKED_FILE: &str = "revoked-sessions.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Anonymous,
    /// Logged in, but the default password must be changed first.
    MustChange,
    Ready,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    WrongPassword,
    /// Too many failed attempts; try again after this long.
    Throttled(Duration),
    NotLoggedIn,
    WeakPassword,
    /// The new password could not be written to `DATA_DIR`.
    NotSaved,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "Falsches Passwort."),
            Self::Throttled(wait) => write!(
                f,
                "Zu viele Fehlversuche. Bitte in {} s erneut versuchen.",
                wait.as_secs().max(1)
            ),
            Self::NotLoggedIn => write!(f, "Nicht angemeldet."),
            Self::NotSaved => write!(
                f,
                "Das neue Passwort konnte nicht gespeichert werden. Das alte gilt weiter."
            ),
            Self::WeakPassword => write!(
                f,
                "Das neue Passwort braucht mindestens {MIN_PASSWORD_LEN} Zeichen und darf nicht das Standardpasswort sein."
            ),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    hash: String,
    /// Raised on every change; sessions of an older generation are invalid.
    generation: u64,
}

struct Inner {
    stored: Option<Stored>,
    failures: HashMap<IpAddr, (u32, Option<Instant>)>,
    /// Logged-out tokens with their expiry (Unix seconds), so a copied cookie
    /// stops working too.
    revoked: HashMap<String, u64>,
}

pub struct Auth {
    dir: Option<PathBuf>,
    default_password: String,
    key: [u8; 32],
    inner: Mutex<Inner>,
}

impl Auth {
    /// `ui_password` replaces the default password `admin`.
    pub fn new(data_dir: Option<PathBuf>, ui_password: Option<String>) -> Self {
        let stored = data_dir.as_ref().and_then(|dir| {
            let text = std::fs::read_to_string(dir.join(PASSWORD_FILE)).ok()?;
            match serde_json::from_str::<Stored>(&text) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("{PASSWORD_FILE} nicht lesbar, Standardpasswort gilt: {e}");
                    None
                }
            }
        });
        // Without an own password (first start, or the file was deleted to
        // reset it) a new key ends every old session; otherwise a cookie from
        // before the reset could come back once the generation count repeats.
        let key = match (&data_dir, &stored) {
            (Some(dir), Some(_)) => load_or_create_key(dir),
            (Some(dir), None) => new_key(dir),
            (None, _) => random_key(),
        };
        let revoked = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(REVOKED_FILE)).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            dir: data_dir,
            default_password: ui_password
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| DEFAULT_PASSWORD.to_string()),
            key,
            inner: Mutex::new(Inner {
                stored,
                failures: HashMap::new(),
                revoked,
            }),
        }
    }

    pub fn session_state(&self, token: Option<&str>) -> SessionState {
        let inner = self.inner.lock().unwrap();
        let generation = inner.stored.as_ref().map_or(0, |s| s.generation);
        match token.and_then(|t| self.valid_generation(&inner, t)) {
            Some(g) if g == generation && inner.stored.is_some() => SessionState::Ready,
            Some(g) if g == generation => SessionState::MustChange,
            _ => SessionState::Anonymous,
        }
    }

    /// Checks the password and returns a session token.
    pub fn login(&self, password: &str, client: IpAddr) -> Result<String, AuthError> {
        let mut inner = self.inner.lock().unwrap();
        self.check_password(&mut inner, password, client)?;
        let generation = inner.stored.as_ref().map_or(0, |s| s.generation);
        Ok(self.issue_token(generation))
    }

    /// Sets a new own password. Ends all other sessions and returns a new
    /// token for the caller.
    pub fn change_password(
        &self,
        token: &str,
        current: &str,
        new: &str,
        client: IpAddr,
    ) -> Result<String, AuthError> {
        let mut inner = self.inner.lock().unwrap();
        let generation = inner.stored.as_ref().map_or(0, |s| s.generation);
        if self.valid_generation(&inner, token) != Some(generation) {
            return Err(AuthError::NotLoggedIn);
        }
        if new.chars().count() < MIN_PASSWORD_LEN
            || new == self.default_password
            || new == DEFAULT_PASSWORD
        {
            return Err(AuthError::WeakPassword);
        }
        // Counts towards the same limit as logins: a stolen cookie must not
        // allow guessing the password either.
        self.check_password(&mut inner, current, client)?;
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(new.as_bytes(), &salt)
            .expect("hashing works")
            .to_string();
        let stored = Stored {
            hash,
            generation: generation + 1,
        };
        if let Some(dir) = &self.dir {
            let text = serde_json::to_string(&stored).expect("serializes");
            write_private(dir, PASSWORD_FILE, text.as_bytes()).map_err(|e| {
                tracing::warn!("Passwort nicht gespeichert: {e}");
                AuthError::NotSaved
            })?;
        }
        inner.stored = Some(stored);
        Ok(self.issue_token(generation + 1))
    }

    /// Compares with the own password, or the default one while none is
    /// saved, and slows down repeated failures from one address.
    fn check_password(
        &self,
        inner: &mut Inner,
        password: &str,
        client: IpAddr,
    ) -> Result<(), AuthError> {
        let now = Instant::now();
        if let Some((_, Some(until))) = inner.failures.get(&client)
            && *until > now
        {
            return Err(AuthError::Throttled(*until - now));
        }
        let ok = match &inner.stored {
            Some(stored) => verify_hash(&stored.hash, password),
            None => constant_time_eq(password.as_bytes(), self.default_password.as_bytes()),
        };
        if ok {
            inner.failures.remove(&client);
            return Ok(());
        }
        if inner.failures.len() >= MAX_TRACKED_CLIENTS {
            // Forget addresses that are not locked out, so the map stays small.
            inner
                .failures
                .retain(|_, (_, until)| until.is_some_and(|u| u > now));
        }
        let entry = inner.failures.entry(client).or_insert((0, None));
        if entry.1.is_some_and(|until| until <= now) {
            *entry = (0, None);
        }
        entry.0 += 1;
        if entry.0 >= MAX_FAILURES {
            entry.1 = Some(now + LOCKOUT);
        }
        Err(AuthError::WrongPassword)
    }

    /// Ends one session, also for copies of its cookie.
    pub fn logout(&self, token: &str) {
        let Some((expires, _)) = self.verify_token(token) else {
            return;
        };
        let mut inner = self.inner.lock().unwrap();
        let now = unix_now();
        inner.revoked.retain(|_, exp| *exp >= now);
        inner.revoked.insert(token.to_string(), expires);
        if let Some(dir) = &self.dir {
            let text = serde_json::to_string(&inner.revoked).expect("serializes");
            if let Err(e) = write_private(dir, REVOKED_FILE, text.as_bytes()) {
                tracing::warn!("Abmeldung nicht gespeichert: {e}");
            }
        }
    }

    /// The generation of a token that is valid and not logged out.
    fn valid_generation(&self, inner: &Inner, token: &str) -> Option<u64> {
        if inner.revoked.contains_key(token) {
            return None;
        }
        self.verify_token(token).map(|(_, generation)| generation)
    }

    /// Token: `expires.generation.nonce.signature`, signed with the session
    /// key. The nonce tells sessions apart, so one can be logged out alone.
    fn issue_token(&self, generation: u64) -> String {
        let expires = unix_now() + SESSION_LIFETIME.as_secs();
        let mut nonce = [0u8; 8];
        OsRng.fill_bytes(&mut nonce);
        let payload = format!("{expires}.{generation}.{}", hex(&nonce));
        format!("{payload}.{}", hex(&self.sign(&payload)))
    }

    /// Expiry and generation of a correctly signed, unexpired token.
    fn verify_token(&self, token: &str) -> Option<(u64, u64)> {
        let (payload, signature) = token.rsplit_once('.')?;
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&unhex(signature)?).ok()?;
        let mut parts = payload.split('.');
        let (expires, generation) = (parts.next()?, parts.next()?);
        let expires = expires.parse::<u64>().ok()?;
        if expires < unix_now() {
            return None;
        }
        Some((expires, generation.parse().ok()?))
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length works")
    }

    fn sign(&self, payload: &str) -> Vec<u8> {
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

fn verify_hash(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

fn new_key(dir: &Path) -> [u8; 32] {
    let key = random_key();
    if let Err(e) = write_private(dir, KEY_FILE, &key) {
        tracing::warn!("Sitzungsschlüssel nicht gespeichert: {e}");
    }
    key
}

/// The session key survives restarts, so logins do too.
fn load_or_create_key(dir: &Path) -> [u8; 32] {
    if let Ok(bytes) = std::fs::read(dir.join(KEY_FILE))
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return key;
    }
    new_key(dir)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn default_password_must_be_changed() {
        let auth = Auth::new(None, None);
        assert_eq!(auth.session_state(None), SessionState::Anonymous);
        let token = auth.login("admin", IP).unwrap();
        assert_eq!(auth.session_state(Some(&token)), SessionState::MustChange);
        assert_eq!(
            auth.change_password(&token, "admin", "kurz", IP),
            Err(AuthError::WeakPassword)
        );
        let token = auth
            .change_password(&token, "admin", "ein langes Passwort", IP)
            .unwrap();
        assert_eq!(auth.session_state(Some(&token)), SessionState::Ready);
        assert_eq!(auth.login("admin", IP), Err(AuthError::WrongPassword));
        assert!(auth.login("ein langes Passwort", IP).is_ok());
    }

    #[test]
    fn tampered_tokens_are_rejected() {
        let auth = Auth::new(None, Some("start123".into()));
        assert_eq!(auth.login("admin", IP), Err(AuthError::WrongPassword));
        let token = auth.login("start123", IP).unwrap();
        let (payload, sig) = token.rsplit_once('.').unwrap();
        let forged = format!("9{payload}.{sig}");
        assert_eq!(auth.session_state(Some(&forged)), SessionState::Anonymous);
        assert_eq!(auth.session_state(Some("unsinn")), SessionState::Anonymous);
    }

    #[test]
    fn repeated_failures_are_throttled() {
        let auth = Auth::new(None, None);
        for _ in 0..MAX_FAILURES {
            assert_eq!(auth.login("falsch", IP), Err(AuthError::WrongPassword));
        }
        assert!(matches!(
            auth.login("admin", IP),
            Err(AuthError::Throttled(_))
        ));
        let other: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(auth.login("admin", other).is_ok());
    }
}

#[cfg(test)]
mod logout_tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn logout_ends_copies_of_the_cookie() {
        let auth = Auth::new(None, None);
        let token = auth.login("admin", IP).unwrap();
        let other = auth.login("admin", IP).unwrap();
        auth.logout(&token);
        assert_eq!(auth.session_state(Some(&token)), SessionState::Anonymous);
        assert_eq!(auth.session_state(Some(&other)), SessionState::MustChange);
    }
}
