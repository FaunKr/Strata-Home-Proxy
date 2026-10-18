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

pub const DEFAULT_PASSWORD: &str = "admin";
pub const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
pub const MIN_PASSWORD_LEN: usize = 8;
/// Failed logins from one address before it has to wait.
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

const PASSWORD_FILE: &str = "admin-password.json";
const KEY_FILE: &str = "session-key";

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
        let key = data_dir
            .as_deref()
            .map(load_or_create_key)
            .unwrap_or_else(random_key);
        Self {
            dir: data_dir,
            default_password: ui_password
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| DEFAULT_PASSWORD.to_string()),
            key,
            inner: Mutex::new(Inner {
                stored,
                failures: HashMap::new(),
            }),
        }
    }

    pub fn session_state(&self, token: Option<&str>) -> SessionState {
        let inner = self.inner.lock().unwrap();
        let generation = inner.stored.as_ref().map_or(0, |s| s.generation);
        match token.and_then(|t| self.verify_token(t)) {
            Some(g) if g == generation && inner.stored.is_some() => SessionState::Ready,
            Some(g) if g == generation => SessionState::MustChange,
            _ => SessionState::Anonymous,
        }
    }

    /// Checks the password and returns a session token.
    pub fn login(&self, password: &str, client: IpAddr) -> Result<String, AuthError> {
        let mut inner = self.inner.lock().unwrap();
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
        if !ok {
            let entry = inner.failures.entry(client).or_insert((0, None));
            if entry.1.is_some_and(|until| until <= now) {
                *entry = (0, None);
            }
            entry.0 += 1;
            if entry.0 >= MAX_FAILURES {
                entry.1 = Some(now + LOCKOUT);
            }
            return Err(AuthError::WrongPassword);
        }
        inner.failures.remove(&client);
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
    ) -> Result<String, AuthError> {
        if self.session_state(Some(token)) == SessionState::Anonymous {
            return Err(AuthError::NotLoggedIn);
        }
        if new.chars().count() < MIN_PASSWORD_LEN
            || new == self.default_password
            || new == DEFAULT_PASSWORD
        {
            return Err(AuthError::WeakPassword);
        }
        let mut inner = self.inner.lock().unwrap();
        let ok = match &inner.stored {
            Some(stored) => verify_hash(&stored.hash, current),
            None => constant_time_eq(current.as_bytes(), self.default_password.as_bytes()),
        };
        if !ok {
            return Err(AuthError::WrongPassword);
        }
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(new.as_bytes(), &salt)
            .expect("hashing works")
            .to_string();
        let generation = inner.stored.as_ref().map_or(0, |s| s.generation) + 1;
        let stored = Stored { hash, generation };
        if let Some(dir) = &self.dir {
            let text = serde_json::to_string(&stored).expect("serializes");
            if let Err(e) = write_private(dir, PASSWORD_FILE, text.as_bytes()) {
                tracing::warn!("Passwort nicht gespeichert: {e}");
            }
        }
        inner.stored = Some(stored);
        Ok(self.issue_token(generation))
    }

    /// Token: `expires.generation.signature`, signed with the session key.
    fn issue_token(&self, generation: u64) -> String {
        let expires = unix_now() + SESSION_LIFETIME.as_secs();
        let payload = format!("{expires}.{generation}");
        format!("{payload}.{}", hex(&self.sign(&payload)))
    }

    /// The generation of a valid, unexpired token.
    fn verify_token(&self, token: &str) -> Option<u64> {
        let (payload, signature) = token.rsplit_once('.')?;
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&unhex(signature)?).ok()?;
        let (expires, generation) = payload.split_once('.')?;
        if expires.parse::<u64>().ok()? < unix_now() {
            return None;
        }
        generation.parse().ok()
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

/// The session key survives restarts, so logins do too.
fn load_or_create_key(dir: &Path) -> [u8; 32] {
    if let Ok(bytes) = std::fs::read(dir.join(KEY_FILE))
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return key;
    }
    let key = random_key();
    if let Err(e) = write_private(dir, KEY_FILE, &key) {
        tracing::warn!("Sitzungsschlüssel nicht gespeichert: {e}");
    }
    key
}

fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dir.join(name))
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
            auth.change_password(&token, "admin", "kurz"),
            Err(AuthError::WeakPassword)
        );
        let token = auth
            .change_password(&token, "admin", "ein langes Passwort")
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
