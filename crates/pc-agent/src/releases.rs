//! What a Strata update brings, for the Strata Terminal: the commits and
//! files between the installed build and the latest release, from GitHub's
//! REST API without a token. Windows' own `curl.exe` asks, so the service
//! needs no TLS of its own.

use serde_json::Value;
use strata_pc_protocol::{ChangedFile, Incoming, IncomingCommit};

use crate::command::CommandSpec;

/// Strata's repository on GitHub, where `install.ps1` comes from too.
pub const REPOSITORY: &str = "https://github.com/Niko1221/Strata";
const API: &str = "https://api.github.com/repos/Niko1221/Strata";

/// Most commits the page carries.
const MAX_COMMITS: usize = 200;

/// A GET of `url` that prints the answer, or fails on an HTTP error.
fn get(url: &str) -> CommandSpec {
    CommandSpec {
        program: "curl.exe".into(),
        args: [
            url,
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "60",
            "--header",
            "Accept: application/vnd.github+json",
        ]
        .map(String::from)
        .to_vec(),
        ..Default::default()
    }
}

/// The latest release; read it with [`latest_tag`].
pub fn latest_release() -> CommandSpec {
    get(&format!("{API}/releases/latest"))
}

/// The commits and files from `commit` to `tag`; read them with
/// [`incoming`].
pub fn compare(commit: &str, tag: &str) -> CommandSpec {
    get(&format!("{API}/compare/{commit}...{tag}"))
}

fn answer(output: &[String]) -> Option<Value> {
    serde_json::from_str(&output.join("\n")).ok()
}

/// The tag of the latest release.
pub fn latest_tag(output: &[String]) -> Option<String> {
    answer(output)?["tag_name"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(String::from)
}

/// What the compare answer brings, `None` when it brings no commits. GitHub
/// lists the commits oldest first; the page wants the newest first.
pub fn incoming(output: &[String]) -> Option<Option<Incoming>> {
    let answer = answer(output)?;
    let total = answer["total_commits"].as_u64()?;
    let listed = answer["commits"].as_array()?;
    if total == 0 {
        return Some(None);
    }
    let upstream = listed.last()?["sha"].as_str()?.to_string();
    let commits = listed
        .iter()
        .rev()
        .take(MAX_COMMITS)
        .filter_map(|c| {
            let message = c["commit"]["message"].as_str().unwrap_or_default();
            let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
            Some(IncomingCommit {
                hash: c["sha"].as_str()?.to_string(),
                time: unix_seconds(c["commit"]["committer"]["date"].as_str()?)?,
                subject: subject.trim().to_string(),
                body: body.trim().to_string(),
            })
        })
        .collect();
    let files = answer["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    Some(ChangedFile {
                        path: f["filename"].as_str()?.to_string(),
                        added: f["additions"].as_u64().unwrap_or(0),
                        removed: f["deletions"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Some(Incoming {
        upstream,
        total,
        commits,
        files,
        remote: Some(REPOSITORY.to_string()),
    }))
}

/// `2026-10-21T09:07:39Z` as Unix seconds.
fn unix_seconds(text: &str) -> Option<u64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, min, s) = (t.next()??, t.next()??, t.next()??);
    // Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + min * 60 + s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn the_latest_tag_comes_from_the_release() {
        let out = lines(
            "{\n  \"url\": \"https://api.github.com/repos/Niko1221/Strata/releases/1\",\n  \"tag_name\": \"v0.2.2\",\n  \"name\": \"Strata 0.2.2\"\n}",
        );
        assert_eq!(latest_tag(&out).as_deref(), Some("v0.2.2"));
        assert_eq!(
            latest_tag(&lines("curl: (22) The requested URL returned error: 403")),
            None
        );
    }

    // The shape of GitHub's "Compare two commits" answer, shortened.
    const COMPARE: &str = r#"{
  "status": "ahead",
  "ahead_by": 2,
  "behind_by": 0,
  "total_commits": 2,
  "commits": [
    {"sha": "76f90e58aaaa", "commit": {"author": {"date": "2026-10-20T18:00:00Z"}, "committer": {"date": "2026-10-20T18:01:00Z"}, "message": "Fix #3425: the memory tiers count RAM + VRAM less 5 GB\n\nsetup.py and tools/strata_mcp.py, one constant each"}},
    {"sha": "011c9e60bbbb", "commit": {"author": {"date": "2026-10-21T09:07:39Z"}, "committer": {"date": "2026-10-21T09:07:39Z"}, "message": "README: RX 6000 cards get their own engine package in 0.2.2"}}
  ],
  "files": [
    {"filename": "setup.py", "status": "modified", "additions": 12, "deletions": 3, "changes": 15},
    {"filename": "docs/logo.png", "status": "modified", "additions": 0, "deletions": 0, "changes": 0}
  ]
}"#;

    #[test]
    fn the_compare_answer_fills_the_incoming_commits_newest_first() {
        let incoming = incoming(&lines(COMPARE)).unwrap().unwrap();
        assert_eq!(incoming.total, 2);
        assert_eq!(incoming.upstream, "011c9e60bbbb");
        assert_eq!(incoming.commits[0].hash, "011c9e60bbbb");
        assert_eq!(incoming.commits[0].time, 1_792_573_659);
        assert_eq!(
            incoming.commits[1].subject,
            "Fix #3425: the memory tiers count RAM + VRAM less 5 GB"
        );
        assert_eq!(
            incoming.commits[1].body,
            "setup.py and tools/strata_mcp.py, one constant each"
        );
        assert_eq!(
            incoming.files[0],
            ChangedFile {
                path: "setup.py".into(),
                added: 12,
                removed: 3
            }
        );
        assert_eq!(incoming.remote.as_deref(), Some(REPOSITORY));
    }

    #[test]
    fn nothing_new_or_no_answer() {
        let same =
            lines(r#"{"status": "identical", "total_commits": 0, "commits": [], "files": []}"#);
        assert_eq!(incoming(&same), Some(None));
        assert_eq!(
            incoming(&lines("curl: (6) Could not resolve host: api.github.com")),
            None
        );
    }

    #[test]
    fn github_dates_are_unix_seconds() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix_seconds("2026-10-21"), None);
    }
}
