//! Client for the service on the PC (see `docs/pc-agent-api.md`).

use reqwest::Client;
use serde::Deserialize;
use strata_admin_ui::api::{StrataInfo, StrataJob};

use crate::config::Config;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PcStatus {
    pub gaming_mode: bool,
    #[serde(default)]
    pub strata_running: bool,
    /// MAC address of the PC's network card, if the service reports it.
    #[serde(default)]
    pub mac: Option<String>,
    /// A Strata job (setup, download, update, benchmark) runs.
    #[serde(default)]
    pub strata_job: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SleepAnswer {
    Accepted,
    Refused(String),
}

/// A refusal's body: `{"reason": "..."}`.
#[derive(Debug, Deserialize)]
struct SleepBody {
    #[serde(default)]
    reason: Option<String>,
}

/// The reason in a refusal, or the status if there is none.
async fn reason(resp: reqwest::Response) -> String {
    let status = resp.status();
    resp.json::<SleepBody>()
        .await
        .ok()
        .and_then(|b| b.reason)
        .unwrap_or_else(|| format!("Der PC-Dienst hat abgelehnt (HTTP {status})."))
}

pub struct AgentClient {
    base: Option<String>,
    token: Option<String>,
    http: Client,
}

impl AgentClient {
    pub fn new(cfg: &Config, http: Client) -> Self {
        Self {
            base: cfg.pc_agent_url.clone(),
            token: cfg.pc_agent_token.clone(),
            http,
        }
    }

    pub fn configured(&self) -> bool {
        self.base.is_some()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Option<reqwest::RequestBuilder> {
        let base = self.base.as_ref()?;
        let mut req = self.http.request(method, format!("{base}{path}"));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        Some(req)
    }

    /// `None` when the service is not configured or does not answer, which
    /// usually means the PC is asleep.
    pub async fn status(&self) -> Option<PcStatus> {
        let resp = self
            .request(reqwest::Method::GET, "/status")?
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    /// Switches gaming mode: on ends Strata and blocks sleep, off starts it again.
    pub async fn set_gaming(&self, enabled: bool) -> anyhow::Result<()> {
        let req = self
            .request(reqwest::Method::POST, "/gaming")
            .ok_or_else(|| anyhow::anyhow!("PC_AGENT_URL ist nicht gesetzt"))?;
        let resp = req
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Der PC-Dienst hat abgelehnt (HTTP {}).", resp.status());
        }
        Ok(())
    }

    pub async fn request_sleep(&self) -> anyhow::Result<SleepAnswer> {
        let req = self
            .request(reqwest::Method::POST, "/sleep")
            .ok_or_else(|| anyhow::anyhow!("PC_AGENT_URL ist nicht gesetzt"))?;
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(SleepAnswer::Accepted);
        }
        let reason = resp
            .json::<SleepBody>()
            .await
            .ok()
            .and_then(|b| b.reason)
            .unwrap_or_else(|| format!("HTTP {status}"));
        Ok(SleepAnswer::Refused(reason))
    }

    /// Strata on the PC; `None` if the service does not answer.
    pub async fn strata(&self) -> Option<StrataInfo> {
        let resp = self
            .request(reqwest::Method::GET, "/strata")?
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    /// The running or last Strata job.
    pub async fn strata_job(&self) -> Option<StrataJob> {
        let resp = self
            .request(reqwest::Method::GET, "/strata/job")?
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<Option<StrataJob>>().await.ok().flatten()
    }

    /// Starts a Strata job; `Err` carries the service's reason for refusing.
    pub async fn start_strata_job(&self, body: &serde_json::Value) -> Result<StrataJob, String> {
        let req = self
            .request(reqwest::Method::POST, "/strata/jobs")
            .ok_or_else(|| "PC_AGENT_URL ist nicht gesetzt".to_string())?;
        let resp = req
            .json(body)
            .send()
            .await
            .map_err(|_| "Der PC-Dienst antwortet nicht.".to_string())?;
        if !resp.status().is_success() {
            return Err(reason(resp).await);
        }
        resp.json()
            .await
            .map_err(|e| format!("Antwort des PC-Dienstes nicht lesbar: {e}"))
    }

    pub async fn cancel_strata_job(&self) -> Result<(), String> {
        let req = self
            .request(reqwest::Method::POST, "/strata/job/cancel")
            .ok_or_else(|| "PC_AGENT_URL ist nicht gesetzt".to_string())?;
        let resp = req
            .send()
            .await
            .map_err(|_| "Der PC-Dienst antwortet nicht.".to_string())?;
        if !resp.status().is_success() {
            return Err(reason(resp).await);
        }
        Ok(())
    }
}
