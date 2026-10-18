//! The proxy's side of the web UI: what its server functions call.

use std::{net::IpAddr, sync::Arc};

use strata_admin_ui::{
    api::{Session, SettingEntry, SettingsView, Source},
    server::{Backend, SharedBackend},
};

use crate::{
    auth::{AuthError, SessionState},
    events::EventKind,
    settings,
    state::AppState,
};

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

    fn change_password(&self, token: &str, current: &str, new: &str) -> Result<String, String> {
        let result = self.0.auth.change_password(token, current, new);
        if result.is_ok() {
            self.0.events.record(EventKind::PasswordChanged, None);
        }
        result.map_err(|e: AuthError| e.to_string())
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
}
