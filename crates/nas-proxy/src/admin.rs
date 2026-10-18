//! The proxy's side of the web UI: what its server functions call.

use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{FutureExt, future::BoxFuture, stream::BoxStream};
use strata_admin_ui::{
    api::{Overview, Session, SettingEntry, SettingsView, Source},
    server::{Backend, SharedBackend},
};

use crate::{
    agent::SleepAnswer,
    auth::{AuthError, SessionState},
    clock,
    events::EventKind,
    overview, settings,
    state::AppState,
};

/// Detail of events caused by a button on the overview.
const BY_UI: &str = "über Weboberfläche";

struct UiBackend(Arc<AppState>);

pub fn backend(state: &Arc<AppState>) -> SharedBackend {
    Arc::new(UiBackend(Arc::clone(state)))
}

impl Backend for UiBackend {
    fn session(&self, token: Option<&str>) -> Session {
        match self.0.auth.session_state(token) {
            SessionState::Anonymous => Session::Anonymous,
            SessionState::MustChange => Session::MustChange,
            SessionState::Ready => Session::Ready,
        }
    }

    fn login(&self, password: &str, client: IpAddr) -> Result<String, String> {
        self.0
            .auth
            .login(password, client)
            .map_err(|e| e.to_string())
    }

    fn change_password(
        &self,
        token: &str,
        current: &str,
        new: &str,
        client: IpAddr,
    ) -> Result<String, String> {
        let result = self.0.auth.change_password(token, current, new, client);
        if result.is_ok() {
            self.0.events.record(EventKind::PasswordChanged, None);
        }
        result.map_err(|e: AuthError| e.to_string())
    }

    fn logout(&self, token: &str) {
        self.0.auth.logout(token);
    }

    fn settings(&self) -> SettingsView {
        let entries = self
            .0
            .settings
            .entries()
            .into_iter()
            .map(|e| SettingEntry {
                key: e.field.key.to_string(),
                value: e.value,
                source: match e.source {
                    settings::Source::Reported => Source::Reported,
                    settings::Source::Saved => Source::Saved,
                    settings::Source::Env => Source::Env,
                    settings::Source::Default => Source::Default,
                },
                saved: e.saved,
                restart: e.field.restart,
                secret: e.field.secret,
                optional: e.field.optional,
            })
            .collect();
        SettingsView {
            persistent: self.0.settings.persistent(),
            configured: self.0.cfg().strata_url.is_some(),
            entries,
        }
    }

    fn save_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.settings.set(key, value).map_err(|e| e.to_string())?;
        self.0
            .events
            .record(EventKind::SettingSaved, Some(key.to_string()));
        Ok(())
    }

    fn reset_setting(&self, key: &str) -> Result<(), String> {
        self.0.settings.reset(key).map_err(|e| e.to_string())?;
        self.0
            .events
            .record(EventKind::SettingReset, Some(key.to_string()));
        Ok(())
    }

    fn overview(&self) -> BoxStream<'static, Overview> {
        overview::watch(&self.0)
    }

    fn wake(&self) -> Result<(), String> {
        let cfg = self.0.cfg();
        if cfg.strata_url.is_none() {
            return Err("Die Strata-Adresse fehlt, der Proxy ist nicht eingerichtet.".into());
        }
        if cfg.pc_mac.is_none() {
            return Err("Keine MAC-Adresse bekannt, der PC kann nicht geweckt werden.".into());
        }
        self.0
            .events
            .record(EventKind::WakeRequested, Some(BY_UI.into()));
        let state = Arc::clone(&self.0);
        tokio::spawn(async move {
            state.overview.refresh_now();
            if state.ensure_awake().await.is_ok() {
                // Woken on purpose: count the idle time from now.
                state.touch();
            }
            state.overview.refresh_now();
        });
        Ok(())
    }

    fn sleep_now(&self) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move {
            let agent = state.agent();
            if !agent.configured() {
                return Err("Kein PC-Dienst eingerichtet (PC_AGENT_URL).".to_string());
            }
            state
                .events
                .record(EventKind::SleepRequested, Some(BY_UI.into()));
            let answer = state.request_sleep(&agent).await;
            state.overview.refresh_now();
            match answer {
                Ok(SleepAnswer::Accepted) => Ok(()),
                Ok(SleepAnswer::Refused(reason)) => Err(format!("Abgelehnt: {reason}")),
                Err(_) => Err("Der PC-Dienst antwortet nicht.".to_string()),
            }
        }
        .boxed()
    }

    fn set_gaming(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move {
            let kind = if enabled {
                EventKind::GamingOn
            } else {
                EventKind::GamingOff
            };
            state.events.record(kind, Some(BY_UI.into()));
            state.overview.refresh_now();
            let result = state.set_gaming(enabled).await.map_err(|e| e.to_string());
            state.overview.refresh_now();
            result
        }
        .boxed()
    }

    fn keep_awake(&self, choice: &str) -> Result<(), String> {
        let now = SystemTime::now();
        let until = match choice {
            "1h" => now + Duration::from_secs(3600),
            "4h" => now + Duration::from_secs(4 * 3600),
            "8h" => now + Duration::from_secs(8 * 3600),
            "morning" => UNIX_EPOCH + Duration::from_secs(clock::next_morning_unix()),
            "off" => {
                self.0.set_keep_awake(None);
                self.0
                    .events
                    .record(EventKind::KeepAwakeEnded, Some(BY_UI.into()));
                return Ok(());
            }
            _ => return Err(format!("Unbekannte Auswahl {choice:?}.")),
        };
        self.0.set_keep_awake(Some(until));
        let text = clock::local_text(clock::unix(until));
        self.0.events.record(
            EventKind::KeepAwakeSet,
            Some(format!("bis {text}, {BY_UI}")),
        );
        Ok(())
    }
}
