//! The proxy's side of the web UI: what its server functions call.

use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{FutureExt, future::BoxFuture, stream::BoxStream};
use strata_admin_ui::{
    api::{
        ApiKeyInfo, ApiKeysView, CreatedKey, HistoryView, KeepAwakeChoice, OutputMessage, Overview,
        PcSetupCommand, Session, SettingEntry, SettingValue, SettingsView, Setup, Source,
        StrataJobForm, StrataPage,
    },
    labels::{NO_AGENT, NO_MAC, NOT_CONFIGURED},
    server::{Backend, SharedBackend},
};

use crate::{
    agent::SleepAnswer,
    api_keys::KeyInfo,
    auth::{AuthError, SessionState},
    clock,
    events::EventKind,
    overview, settings,
    state::AppState,
};

/// Detail of events caused by a button on the overview.
const BY_UI: &str = "from the web UI";

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
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: self
                .0
                .cfg()
                .data_dir
                .as_ref()
                .map(|d| d.display().to_string()),
        }
    }

    fn save_settings(&self, values: &[SettingValue]) -> Result<(), (Option<String>, String)> {
        let secret = |key: &str| crate::config::find_field(key).is_some_and(|f| f.secret);
        let values: Vec<(&str, &str)> = values
            .iter()
            .filter(|v| !(secret(&v.key) && v.value.trim().is_empty()))
            .map(|v| (v.key.as_str(), v.value.as_str()))
            .collect();
        self.0
            .settings
            .set_many(&values)
            .map_err(|(key, e)| (key, e.to_string()))?;
        for (key, _) in values {
            self.0
                .events
                .record(EventKind::SettingSaved, Some(key.to_string()));
        }
        Ok(())
    }

    fn reset_setting(&self, key: &str) -> Result<(), String> {
        self.0.settings.reset(key).map_err(|e| e.to_string())?;
        self.0
            .events
            .record(EventKind::SettingReset, Some(key.to_string()));
        Ok(())
    }

    fn setup(&self) -> Setup {
        overview::setup(&self.0)
    }

    fn overview(&self) -> BoxStream<'static, Overview> {
        overview::watch(&self.0)
    }

    fn strata_output(&self) -> BoxStream<'static, OutputMessage> {
        crate::strata_output::watch(&self.0)
    }

    fn wake(&self) -> Result<(), String> {
        let cfg = self.0.cfg();
        if cfg.strata_url.is_none() {
            return Err(NOT_CONFIGURED.into());
        }
        if cfg.pc_mac.is_none() {
            return Err(NO_MAC.into());
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
                return Err(NO_AGENT.to_string());
            }
            state
                .events
                .record(EventKind::SleepRequested, Some(BY_UI.into()));
            let answer = state.request_sleep(&agent).await;
            state.overview.refresh_now();
            match answer {
                Ok(SleepAnswer::Accepted) => Ok(()),
                Ok(SleepAnswer::Refused(reason)) => Err(format!("Refused: {reason}")),
                Err(_) => Err("The PC service does not answer.".to_string()),
            }
        }
        .boxed()
    }

    fn set_gaming(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move {
            state.overview.refresh_now();
            let result = state
                .set_gaming(enabled, BY_UI)
                .await
                .map_err(|e| e.to_string());
            if result.is_ok() {
                let kind = if enabled {
                    EventKind::GamingOn
                } else {
                    EventKind::GamingOff
                };
                state.events.record(kind, Some(BY_UI.into()));
            }
            state.overview.refresh_now();
            result
        }
        .boxed()
    }

    fn keep_awake(&self, choice: KeepAwakeChoice) -> Result<(), String> {
        let now = SystemTime::now();
        let until = match choice {
            KeepAwakeChoice::OneHour => now + Duration::from_secs(3600),
            KeepAwakeChoice::FourHours => now + Duration::from_secs(4 * 3600),
            KeepAwakeChoice::EightHours => now + Duration::from_secs(8 * 3600),
            KeepAwakeChoice::Morning => {
                UNIX_EPOCH + Duration::from_secs(clock::next_morning_unix())
            }
            KeepAwakeChoice::Off => {
                if self.0.snapshot().keep_awake_until.is_some() {
                    self.0
                        .events
                        .record(EventKind::KeepAwakeEnded, Some(BY_UI.into()));
                }
                self.0.set_keep_awake(None);
                return Ok(());
            }
        };
        self.0.set_keep_awake(Some(until));
        let text = clock::local_text(clock::unix(until));
        self.0.events.record(
            EventKind::KeepAwakeSet,
            Some(format!("until {text}, {BY_UI}")),
        );
        Ok(())
    }

    fn api_keys(&self) -> ApiKeysView {
        ApiKeysView {
            persistent: self.0.api_keys.persistent(),
            keys: self.0.api_keys.list().into_iter().map(key_info).collect(),
        }
    }

    fn create_api_key(&self, name: &str) -> Result<CreatedKey, String> {
        let (key, info) = self.0.api_keys.create(name).map_err(|e| e.to_string())?;
        self.0
            .events
            .record(EventKind::ApiKeyCreated, Some(info.name.clone()));
        self.0.overview.refresh_now();
        Ok(CreatedKey {
            key,
            info: key_info(info),
        })
    }

    fn revoke_api_key(&self, name: &str, prefix: &str) -> Result<(), String> {
        self.0
            .api_keys
            .revoke(name, prefix)
            .map_err(|e| e.to_string())?;
        self.0
            .events
            .record(EventKind::ApiKeyRevoked, Some(name.to_string()));
        self.0.overview.refresh_now();
        Ok(())
    }

    fn strata(&self) -> BoxFuture<'static, StrataPage> {
        let state = Arc::clone(&self.0);
        async move { crate::strata::page(&state).await }.boxed()
    }

    fn start_strata_job(&self, form: StrataJobForm) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move { crate::strata::start_job(&state, form).await }.boxed()
    }

    fn cancel_strata_job(&self) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move { crate::strata::cancel(&state).await }.boxed()
    }

    fn pc_setup_command(&self, host: &str) -> Result<PcSetupCommand, String> {
        self.0.pc_setup.command(host)
    }

    fn update_pc_service(&self) -> BoxFuture<'static, Result<(), String>> {
        let state = Arc::clone(&self.0);
        async move { crate::pc_setup::update(&state, BY_UI).await }.boxed()
    }

    fn history(&self, days: u32) -> BoxFuture<'static, HistoryView> {
        let state = Arc::clone(&self.0);
        async move { crate::history::view(&state, days).await }.boxed()
    }
}

fn key_info(k: KeyInfo) -> ApiKeyInfo {
    ApiKeyInfo {
        name: k.name,
        prefix: k.prefix,
        created: k.created,
        last_used: k.last_used,
        created_text: clock::local_text(k.created),
        last_used_text: k.last_used.map(clock::local_text),
    }
}
