//! The proxy's side of the web UI: what its server functions call.

use std::{net::IpAddr, sync::Arc};

use strata_admin_ui::{
    api::Session,
    server::{Backend, SharedBackend},
};

use crate::{
    auth::{AuthError, SessionState},
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
            self.0
                .events
                .record(crate::events::EventKind::PasswordChanged, None);
        }
        result.map_err(|e: AuthError| e.to_string())
    }
}
