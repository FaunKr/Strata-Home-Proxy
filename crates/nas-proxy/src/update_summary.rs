//! What a Strata update brings (#141): the incoming commits the PC service
//! reports, shown on the Version tab with GitHub links where the remote is
//! on github.com, and a summary of them that Strata writes itself.
//!
//! The Proxy asks Strata straight, not through its own client path: the
//! request is housekeeping, so it never wakes the PC, restarts the
//! Leerlaufzeit or writes an Ereignis. It asks only an idle Strata with its
//! model loaded, once per upstream commit, and after a failure not again
//! for an hour. The summary is kept in `DATA_DIR` until the update is
//! installed, so it survives a restart and shows while the PC sleeps.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_admin_ui::api::{StrataUpdate, UpdateCommit, UpdateSummary};
use strata_pc_protocol::{ChangedFile, Incoming};

use crate::{clock, files::write_private, overview, state::AppState};

/// The summary for the newest upstream commit, in `DATA_DIR`.
const FILE: &str = "strata-update-summary.json";

/// Top-level folders the fallback names at most.
const FALLBACK_AREAS: usize = 5;
/// Bullet points of a summary at most.
const MAX_LINES: usize = 6;
/// Changed files the prompt lists at most, those with the most lines first.
const PROMPT_FILES: usize = 100;
/// How long Strata may take for a summary.
const LIMIT: Duration = Duration::from_secs(120);
/// After a failed attempt, the same upstream commit waits this long.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

/// How long Strata may take, and how long a failed upstream commit waits.
struct Timing {
    limit: Duration,
    retry_after: Duration,
}
/// Strata's context when `/health` does not say.
const DEFAULT_CONTEXT: u64 = 8192;

const INSTRUCTIONS: &str = "You summarize the changes in an update of Strata, a local LLM engine, \
for the person who runs it on their PC. Answer with 3 to 6 short bullet points in English, \
each line starting with \"- \". Put first anything they have to do themselves, such as \
running setup again, downloading models again or changing settings. No headings, no other text.";

/// A summary Strata wrote.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Written {
    /// The installed version it was written for.
    installed: Option<String>,
    upstream: String,
    lines: Vec<String>,
    model: String,
    /// Unix seconds.
    written_at: u64,
}

impl Written {
    fn summary(self) -> UpdateSummary {
        UpdateSummary::Written {
            lines: self.lines,
            model: self.model,
        }
    }
}

pub struct UpdateSummaries {
    dir: Option<PathBuf>,
    /// The last incoming commits with the installed version they were
    /// reported for.
    last: Mutex<Option<(Option<String>, Incoming)>>,
    written: Mutex<Option<Written>>,
    /// The upstream commit whose summary Strata writes now.
    writing: Mutex<Option<String>>,
    /// The last failed attempt.
    failed: Mutex<Option<(String, Instant)>>,
    /// `LIMIT` and `RETRY_AFTER`; shorter in tests.
    timing: Mutex<Timing>,
}

/// What the Strata page needs to know about Strata right now.
pub struct Now<'a> {
    pub installed: Option<&'a str>,
    pub update_available: Option<bool>,
    pub gaming: bool,
    /// A Strata job runs.
    pub job: bool,
}

/// Takes in what the PC service just reported, starts a summary if one is
/// due, and returns what the Version tab shows.
pub fn noted(
    state: &Arc<AppState>,
    now: Now<'_>,
    incoming: Option<Incoming>,
) -> Option<StrataUpdate> {
    let summaries = &state.strata.update;
    let incoming = incoming.filter(|_| now.update_available == Some(true));
    summaries.drop_outdated(
        now.installed,
        now.update_available,
        incoming.as_ref().map(|i| i.upstream.as_str()),
    );
    let Some(incoming) = incoming else {
        *summaries.last.lock().unwrap() = None;
        return None;
    };
    *summaries.last.lock().unwrap() = Some((now.installed.map(String::from), incoming.clone()));
    let upstream = &incoming.upstream;
    let fallback = fallback(&incoming);
    let written = summaries
        .written
        .lock()
        .unwrap()
        .clone()
        .filter(|w| w.upstream == *upstream);
    let summary = if let Some(w) = written {
        w.summary()
    } else if summaries.writing.lock().unwrap().as_ref() == Some(upstream) {
        UpdateSummary::Writing { fallback }
    } else if summaries.recently_failed(upstream) {
        UpdateSummary::Unavailable { fallback }
    } else if now.gaming || now.job {
        summaries.fail(upstream, "Gaming-Modus or a Strata job");
        UpdateSummary::Unavailable { fallback }
    } else if start(state, now.installed, incoming.clone()) {
        UpdateSummary::Writing { fallback }
    } else {
        UpdateSummary::Unavailable { fallback }
    };
    Some(view(now.installed, &incoming, summary))
}

impl UpdateSummaries {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let written = dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self {
            dir,
            last: Mutex::new(None),
            written: Mutex::new(written),
            writing: Mutex::new(None),
            failed: Mutex::new(None),
            timing: Mutex::new(Timing {
                limit: LIMIT,
                retry_after: RETRY_AFTER,
            }),
        }
    }

    /// What the Version tab shows while the PC does not answer: the stored
    /// summary, with the commits if they are still in memory.
    pub fn stale(&self) -> Option<StrataUpdate> {
        let last = self.last.lock().unwrap().clone();
        let written = self.written.lock().unwrap().clone();
        match (written, last) {
            (Some(w), Some((installed, incoming))) if w.upstream == incoming.upstream => {
                Some(view(installed.as_deref(), &incoming, w.summary()))
            }
            // Newer commits than the summary's: it is outdated.
            (_, Some((installed, incoming))) => Some(view(
                installed.as_deref(),
                &incoming,
                UpdateSummary::Unavailable {
                    fallback: fallback(&incoming),
                },
            )),
            (Some(w), None) => Some(StrataUpdate {
                summary: w.summary(),
                commits: Vec::new(),
                left_out: 0,
                compare_url: None,
            }),
            (None, None) => None,
        }
    }

    /// Forgets the summary once its update is installed or upstream moved on:
    /// only the newest upstream commit's is kept.
    fn drop_outdated(
        &self,
        installed: Option<&str>,
        update_available: Option<bool>,
        upstream: Option<&str>,
    ) {
        let mut written = self.written.lock().unwrap();
        let outdated = written.as_ref().is_some_and(|w| {
            update_available == Some(false)
                || installed.is_some_and(|i| !i.is_empty() && w.upstream.starts_with(i))
                || upstream.is_some_and(|u| u != w.upstream)
        });
        if outdated {
            *written = None;
            self.save(None);
        }
    }

    fn keep(&self, w: Written) {
        // Under the lock, so a drop cannot slip between file and memory.
        let mut written = self.written.lock().unwrap();
        self.save(Some(&w));
        *written = Some(w);
    }

    fn save(&self, w: Option<&Written>) {
        let Some(dir) = &self.dir else {
            return;
        };
        let saved = match w {
            Some(w) => write_private(
                dir,
                FILE,
                &serde_json::to_vec(w).expect("summary serializes"),
            ),
            None => std::fs::remove_file(dir.join(FILE)).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            }),
        };
        if let Err(e) = saved {
            tracing::error!("{FILE} nicht gespeichert: {e}");
        }
    }

    /// Changes how long Strata may take and how long a failure waits; for tests.
    fn limit(&self) -> Duration {
        self.timing.lock().unwrap().limit
    }

    pub fn set_timing(&self, limit: Duration, retry_after: Duration) {
        *self.timing.lock().unwrap() = Timing { limit, retry_after };
    }

    fn recently_failed(&self, upstream: &str) -> bool {
        let retry_after = self.timing.lock().unwrap().retry_after;
        self.failed
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(u, at)| u == upstream && at.elapsed() < retry_after)
    }

    fn fail(&self, upstream: &str, why: &str) {
        tracing::info!("Keine Zusammenfassung des Strata-Updates: {why}");
        *self.failed.lock().unwrap() = Some((upstream.to_string(), Instant::now()));
    }
}

/// Starts writing the summary; false while another one is being written.
fn start(state: &Arc<AppState>, installed: Option<&str>, incoming: Incoming) -> bool {
    {
        let mut writing = state.strata.update.writing.lock().unwrap();
        if writing.is_some() {
            return false;
        }
        *writing = Some(incoming.upstream.clone());
    }
    let state = Arc::clone(state);
    let installed = installed.map(String::from);
    tokio::spawn(async move {
        let summaries = &state.strata.update;
        match ask(&state, &incoming).await {
            Ok((lines, model)) => summaries.keep(Written {
                installed,
                upstream: incoming.upstream.clone(),
                lines,
                model,
                written_at: clock::unix_now(),
            }),
            Err(why) => summaries.fail(&incoming.upstream, &why),
        }
        *summaries.writing.lock().unwrap() = None;
    });
    true
}

/// Strata's `/health` as far as the summary needs it.
#[derive(Deserialize)]
struct Health {
    #[serde(default)]
    loaded: bool,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_context: Option<u64>,
}

/// Asks an idle Strata with its model loaded for the bullet points.
async fn ask(state: &AppState, incoming: &Incoming) -> Result<(Vec<String>, String), String> {
    let cfg = state.cfg();
    let strata = cfg.strata_url.as_ref().ok_or("no Strata address")?;
    let status = state
        .pc_status(&state.agent())
        .await
        .ok_or("the PC service does not answer")?;
    if status.gaming_mode || status.strata_job {
        return Err("Gaming-Modus or a Strata job".into());
    }
    let auth = cfg.strata_authorization();
    let mut req = state.probe_client().get(format!("{strata}/health"));
    if let Some(auth) = &auth {
        req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("/health answers {}", resp.status()));
    }
    let health: Health = resp.json().await.map_err(|e| e.to_string())?;
    if !health.loaded {
        return Err("the model is not loaded".into());
    }
    let metrics = overview::metrics(state)
        .await
        .ok_or("no metrics from Strata")?;
    if metrics.processing > 0 || metrics.deferred > 0 {
        return Err("Strata is busy".into());
    }
    let context = health.max_context.unwrap_or(DEFAULT_CONTEXT);
    let mut body = json!({
        "messages": [
            {"role": "system", "content": INSTRUCTIONS},
            {"role": "user", "content": prompt(incoming, context)},
        ],
        "reasoning_effort": "medium",
        "stream": false,
    });
    if let Some(model) = &health.model {
        body["model"] = json!(model);
    }
    let mut req = state
        .upstream
        .post(format!("{strata}/v1/chat/completions"))
        .timeout(state.strata.update.limit())
        .json(&body);
    if let Some(auth) = auth {
        req = req.header(reqwest::header::AUTHORIZATION, auth);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Strata answers {}", resp.status()));
    }
    let answer: Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = answer["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    let lines = bullets(content);
    if lines.is_empty() {
        return Err("the answer has no bullet points".into());
    }
    let model = answer["model"]
        .as_str()
        .map(String::from)
        .or(health.model)
        .unwrap_or_else(|| "unknown model".into());
    Ok((lines, model))
}

/// The commits and changed files, trimmed to fit about half the context:
/// commit bodies go first, then the oldest commits.
fn prompt(incoming: &Incoming, context: u64) -> String {
    // About three characters a token; the other half is for the answer.
    let budget = (context / 2 * 3) as usize;
    let mut files: Vec<&ChangedFile> = incoming.files.iter().collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.added + f.removed));
    let mut file_text: String = files
        .iter()
        .take(PROMPT_FILES)
        .map(|f| format!("{} +{} -{}\n", f.path, f.added, f.removed))
        .collect();
    if files.len() > PROMPT_FILES {
        file_text += &format!("and {} more files\n", files.len() - PROMPT_FILES);
    }
    let build = |with_bodies: bool, commits: usize| {
        let mut text =
            String::from("Incoming commits since the installed version, newest first:\n\n");
        for c in &incoming.commits[..commits] {
            text += &format!("- {}\n", c.subject);
            if with_bodies && !c.body.is_empty() {
                for line in c.body.lines() {
                    text += &format!("  {line}\n");
                }
            }
        }
        let left_out = incoming.total.saturating_sub(commits as u64);
        if left_out > 0 {
            text += &format!("\n({left_out} older commits are left out.)\n");
        }
        text += "\nChanged files (lines added and removed):\n";
        text += &file_text;
        text
    };
    let all = incoming.commits.len();
    let with_bodies = build(true, all);
    if with_bodies.len() <= budget {
        return with_bodies;
    }
    let mut commits = all;
    loop {
        let text = build(false, commits);
        if text.len() <= budget || commits <= 1 {
            return text;
        }
        commits -= 1;
    }
}

/// The bullet points of an answer, without their markers and bold; the rest
/// of the text and any thinking before `</think>` is left out.
fn bullets(content: &str) -> Vec<String> {
    let answer = content.rsplit("</think>").next().unwrap_or(content);
    answer
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = ["- ", "* ", "• "]
                .iter()
                .find_map(|m| line.strip_prefix(m))
                .or_else(|| {
                    let (n, rest) = line.split_once(". ")?;
                    n.chars().all(|c| c.is_ascii_digit()).then_some(rest)
                })?;
            // Shown as plain text, so Markdown emphasis would show as stars.
            let rest = rest.replace("**", "").replace("__", "");
            let rest = rest.trim();
            (!rest.is_empty()).then(|| rest.to_string())
        })
        .take(MAX_LINES)
        .collect()
}

fn view(installed: Option<&str>, incoming: &Incoming, summary: UpdateSummary) -> StrataUpdate {
    let github = incoming.remote.as_deref().and_then(github_base);
    StrataUpdate {
        summary,
        commits: incoming
            .commits
            .iter()
            .map(|c| UpdateCommit {
                short: c.hash.chars().take(7).collect(),
                subject: c.subject.clone(),
                date_text: clock::local_date(c.time),
                url: github.as_ref().map(|g| format!("{g}/commit/{}", c.hash)),
            })
            .collect(),
        left_out: incoming.total.saturating_sub(incoming.commits.len() as u64),
        compare_url: github
            .zip(installed)
            .map(|(g, from)| format!("{g}/compare/{from}...{}", incoming.upstream)),
    }
}

/// How many commits, and the top-level folders with the most changed lines.
pub fn fallback(incoming: &Incoming) -> Vec<String> {
    let n = incoming.total;
    let mut lines = vec![format!(
        "{n} new commit{} since the installed version.",
        if n == 1 { "" } else { "s" }
    )];
    let mut areas: Vec<(String, u64)> = Vec::new();
    for file in &incoming.files {
        let area = match file.path.split_once('/') {
            Some((top, _)) => format!("{top}/"),
            None => file.path.clone(),
        };
        let changed = file.added + file.removed;
        match areas.iter_mut().find(|(a, _)| *a == area) {
            Some((_, sum)) => *sum += changed,
            None => areas.push((area, changed)),
        }
    }
    // Stable: equal counts keep the PC service's order.
    areas.sort_by_key(|(_, lines)| std::cmp::Reverse(*lines));
    if !areas.is_empty() {
        let names: Vec<_> = areas
            .into_iter()
            .take(FALLBACK_AREAS)
            .map(|(a, _)| a)
            .collect();
        lines.push(format!("Changes in {}.", names.join(", ")));
    }
    lines
}

/// `https://github.com/<owner>/<repo>` for a remote on github.com.
fn github_base(remote: &str) -> Option<String> {
    let path = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("https://github.com/{owner}/{repo}"))
}
