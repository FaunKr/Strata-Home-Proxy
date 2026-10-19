//! What a Strata update brings (#141): the incoming commits the PC service
//! reports, shown on the Version tab with GitHub links where the remote is
//! on github.com, and a summary of them that Strata writes itself.
//!
//! The Proxy asks Strata straight, not through its own client path: the
//! request is housekeeping, so it never wakes the PC, restarts the
//! Leerlaufzeit or writes an Ereignis. It asks only an idle Strata with its
//! model loaded, once per upstream commit, and after a failure not again
//! for an hour.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{Value, json};
use strata_admin_ui::api::{StrataUpdate, UpdateCommit, UpdateSummary};

use crate::{clock, overview, state::AppState};

/// The PC service's account of the incoming commits (`update` on `/strata`).
#[derive(Clone, Debug, Deserialize)]
pub struct Incoming {
    /// Full hash of the upstream commit.
    pub upstream: String,
    /// All incoming commits; `commits` may hold fewer.
    pub total: u64,
    /// Newest first.
    pub commits: Vec<IncomingCommit>,
    pub files: Vec<ChangedFile>,
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IncomingCommit {
    pub hash: String,
    pub time: u64,
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

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
/// Strata's context when `/health` does not say.
const DEFAULT_CONTEXT: u64 = 8192;

const INSTRUCTIONS: &str = "You summarize the changes in an update of Strata, a local LLM engine, \
for the person who runs it on their PC. Answer with 3 to 6 short bullet points in English, \
each line starting with \"- \". Put first anything they have to do themselves, such as \
running setup again, downloading models again or changing settings. No headings, no other text.";

/// A summary Strata wrote.
#[derive(Clone, Debug)]
struct Written {
    upstream: String,
    lines: Vec<String>,
    model: String,
}

#[derive(Default)]
pub struct UpdateSummaries {
    /// The last incoming commits with the installed version they were
    /// reported for.
    last: Mutex<Option<(Option<String>, Incoming)>>,
    written: Mutex<Option<Written>>,
    /// The upstream commit whose summary Strata writes now.
    writing: Mutex<Option<String>>,
    /// The last failed attempt.
    failed: Mutex<Option<(String, Instant)>>,
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
        UpdateSummary::Written {
            lines: w.lines,
            model: w.model,
        }
    } else if summaries.writing.lock().unwrap().as_ref() == Some(upstream) {
        UpdateSummary::Writing { fallback }
    } else if summaries.recently_failed(upstream) {
        UpdateSummary::Unavailable { fallback }
    } else if now.gaming || now.job {
        summaries.fail(upstream, "Strata is ended or a job runs");
        UpdateSummary::Unavailable { fallback }
    } else {
        start(state, incoming.clone());
        UpdateSummary::Writing { fallback }
    };
    Some(view(now.installed, &incoming, summary))
}

impl UpdateSummaries {
    fn recently_failed(&self, upstream: &str) -> bool {
        self.failed
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(u, at)| u == upstream && at.elapsed() < RETRY_AFTER)
    }

    fn fail(&self, upstream: &str, why: &str) {
        tracing::info!("Keine Zusammenfassung des Strata-Updates: {why}");
        *self.failed.lock().unwrap() = Some((upstream.to_string(), Instant::now()));
    }
}

fn start(state: &Arc<AppState>, incoming: Incoming) {
    {
        let mut writing = state.strata.update.writing.lock().unwrap();
        if writing.is_some() {
            return;
        }
        *writing = Some(incoming.upstream.clone());
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let summaries = &state.strata.update;
        match ask(&state, &incoming).await {
            Ok((lines, model)) => {
                *summaries.written.lock().unwrap() = Some(Written {
                    upstream: incoming.upstream.clone(),
                    lines,
                    model,
                })
            }
            Err(why) => summaries.fail(&incoming.upstream, &why),
        }
        *summaries.writing.lock().unwrap() = None;
    });
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
        .timeout(LIMIT)
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

/// The bullet points of an answer, without their markers; the rest of the
/// text and any thinking before `</think>` is left out.
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
