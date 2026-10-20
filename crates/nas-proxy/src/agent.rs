//! Client for the service on the PC (see `docs/pc-agent-api.md`).

use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use strata_pc_protocol::{
    ForcedRestart, ForcedSleep, GamingRequest, Incoming, PcStatus, Refused, StrataInfo, StrataJob,
    StrataJobRequest, StrataReport,
};

use crate::config::Config;

const LOG_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const LOG_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);

#[derive(Debug, PartialEq, Eq)]
pub enum SleepAnswer {
    Accepted,
    Refused(String),
}

/// The reason in a refusal, or the status if there is none.
async fn reason(resp: reqwest::Response) -> String {
    let status = resp.status();
    resp.json::<Refused>()
        .await
        .ok()
        .and_then(|b| b.reason)
        .unwrap_or_else(|| format!("The PC service refused (HTTP {status})."))
}

/// The service loads the new program from the NAS before it answers.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(330);

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateAnswer {
    Accepted,
    /// The service said no, or failed; it runs on as before.
    Refused(String),
    /// No answer in time: the service may still be loading and finish.
    NoAnswer,
}

/// The PC service's answer to a forced sleep or a forced Strata restart.
#[derive(Debug, PartialEq, Eq)]
pub enum Forced<T> {
    Done(T),
    /// The service said no, with its reason.
    Refused(String),
    /// A service from before the endpoint (404).
    TooOld,
    NoAnswer,
}

pub const TOO_OLD_FOR_UPDATE: &str = "This PC service is too old to update from the web UI. Install it once more with the command from the PC area.";

pub struct AgentClient {
    base: Option<String>,
    token: Option<String>,
    http: Client,
    /// A forced Strata restart waits for a cancelled job to end and for the
    /// ended Strata to free its port (`Config::force_timeout`).
    force_timeout: Duration,
}

impl AgentClient {
    pub fn new(cfg: &Config, http: Client) -> Self {
        Self {
            base: cfg.pc_agent_url.clone(),
            token: cfg.pc_agent_token.clone(),
            http,
            force_timeout: cfg.force_timeout,
        }
    }

    pub fn configured(&self) -> bool {
        self.base.is_some()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Option<reqwest::RequestBuilder> {
        self.request_with(&self.http, method, path)
    }

    fn request_with(
        &self,
        http: &Client,
        method: reqwest::Method,
        path: &str,
    ) -> Option<reqwest::RequestBuilder> {
        let base = self.base.as_ref()?;
        let mut req = http.request(method, format!("{base}{path}"));
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
            .ok_or_else(|| anyhow::anyhow!("PC_AGENT_URL is not set"))?;
        let resp = req.json(&GamingRequest { enabled }).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("The PC service refused (HTTP {}).", resp.status());
        }
        Ok(())
    }

    pub async fn request_sleep(&self) -> anyhow::Result<SleepAnswer> {
        let req = self
            .request(reqwest::Method::POST, "/sleep")
            .ok_or_else(|| anyhow::anyhow!("PC_AGENT_URL is not set"))?;
        let resp = req.send().await?;
        if resp.status().is_success() {
            return Ok(SleepAnswer::Accepted);
        }
        let reason = reason(resp).await;
        Ok(SleepAnswer::Refused(reason))
    }

    /// A forced sleep: standby follows unless the service updates itself.
    pub async fn force_sleep(&self) -> Forced<ForcedSleep> {
        self.force("/sleep/force").await
    }

    /// A forced Strata restart: kills Strata and starts it again.
    pub async fn force_restart(&self) -> Forced<ForcedRestart> {
        self.force("/strata/kill").await
    }

    async fn force<T: DeserializeOwned>(&self, path: &str) -> Forced<T> {
        let Some(req) = self.request(reqwest::Method::POST, path) else {
            return Forced::Refused("PC_AGENT_URL is not set".into());
        };
        let Ok(resp) = req.timeout(self.force_timeout).send().await else {
            return Forced::NoAnswer;
        };
        if resp.status() == StatusCode::NOT_FOUND {
            return Forced::TooOld;
        }
        if !resp.status().is_success() {
            return Forced::Refused(reason(resp).await);
        }
        match resp.json().await {
            Ok(answer) => Forced::Done(answer),
            Err(e) => Forced::Refused(format!("The PC service's answer is unreadable: {e}")),
        }
    }

    /// Strata on the PC; `None` if the service does not answer.
    /// Strata as the service reports it, with the incoming commits apart:
    /// they stay on the NAS (the page gets them through `update_summary`).
    pub async fn strata(&self) -> Option<(StrataInfo, Option<Incoming>)> {
        let resp = self
            .request(reqwest::Method::GET, "/strata")?
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        Some(resp.json::<StrataReport>().await.ok()?.into_parts())
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
    pub async fn start_strata_job(&self, request: &StrataJobRequest) -> Result<StrataJob, String> {
        let req = self
            .request(reqwest::Method::POST, "/strata/jobs")
            .ok_or_else(|| "PC_AGENT_URL is not set".to_string())?;
        let resp = req
            .json(request)
            .send()
            .await
            .map_err(|_| "The PC service does not answer.".to_string())?;
        if !resp.status().is_success() {
            return Err(reason(resp).await);
        }
        resp.json()
            .await
            .map_err(|e| format!("The PC service's answer is unreadable: {e}"))
    }

    /// The Strata output as the service streams it: `None` without an
    /// answer, `Err` with the status of a refusal.
    pub async fn strata_log(&self) -> Option<Result<reqwest::Response, reqwest::StatusCode>> {
        // Its own client: the stream stays open far longer than a probe may
        // take. The service sends a keep-alive every 15 s; a PC that went to
        // standby mid-stream sends nothing, and the read times out.
        let http = Client::builder()
            .connect_timeout(LOG_CONNECT_TIMEOUT)
            .read_timeout(LOG_READ_TIMEOUT)
            .build()
            .ok()?;
        let resp = self
            .request_with(&http, reqwest::Method::GET, "/strata-log")?
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return Some(Err(resp.status()));
        }
        Some(Ok(resp))
    }

    pub async fn cancel_strata_job(&self) -> Result<(), String> {
        let req = self
            .request(reqwest::Method::POST, "/strata/job/cancel")
            .ok_or_else(|| "PC_AGENT_URL is not set".to_string())?;
        let resp = req
            .send()
            .await
            .map_err(|_| "The PC service does not answer.".to_string())?;
        if !resp.status().is_success() {
            return Err(reason(resp).await);
        }
        Ok(())
    }

    /// Asks the service to put the program the NAS carries in place of its
    /// own and restart.
    pub async fn update(&self) -> UpdateAnswer {
        let Some(req) = self.request(reqwest::Method::POST, "/update") else {
            return UpdateAnswer::Refused("PC_AGENT_URL is not set".into());
        };
        let Ok(resp) = req.timeout(UPDATE_TIMEOUT).send().await else {
            return UpdateAnswer::NoAnswer;
        };
        if resp.status() == StatusCode::NOT_FOUND {
            return UpdateAnswer::Refused(TOO_OLD_FOR_UPDATE.into());
        }
        if !resp.status().is_success() {
            return UpdateAnswer::Refused(reason(resp).await);
        }
        UpdateAnswer::Accepted
    }
}
