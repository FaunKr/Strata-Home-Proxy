//! The Strata-Ausgabe: what a Strata started by the service writes to its
//! console. The last lines stay in memory for `GET /strata-log`, the lines of
//! the current run also in `strata.log` next to the service's own log.

use std::{
    collections::VecDeque,
    fs::File,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tokio::sync::broadcast;

/// Lines kept in memory, separators included.
pub const KEPT: usize = 500;
const LOG_FILE: &str = "strata.log";

/// One message of `GET /strata-log` (see `docs/pc-agent-api.md`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// The service started Strata, at this Unix time.
    Started {
        at: u64,
    },
    Line {
        text: String,
    },
    /// Takes the place of the last line, as a progress bar does in a console.
    Replace {
        text: String,
    },
    Notice {
        reason: Notice,
    },
}

/// Why there is no output to follow, or `Running` when there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    Running,
    Gaming,
    NotRunning,
    /// Strata answers, but the service did not start it, so it cannot read it.
    NotStartedByAgent,
}

enum Entry {
    Started(u64),
    Line(String),
}

impl Entry {
    fn message(&self) -> Message {
        match self {
            Entry::Started(at) => Message::Started { at: *at },
            Entry::Line(text) => Message::Line { text: text.clone() },
        }
    }
}

struct Inner {
    entries: VecDeque<Entry>,
    /// The last line ended with `\r`: the next one takes its place.
    replaceable: bool,
    /// Counts the starts; output of an earlier run is dropped.
    run: u64,
    /// The output of the current run is still open.
    reading: bool,
    file: Option<File>,
}

pub struct StrataOutput {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<Message>,
    file: PathBuf,
}

impl StrataOutput {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                replaceable: false,
                run: 0,
                reading: false,
                file: None,
            }),
            tx: broadcast::channel(4 * KEPT).0,
            file: state_dir.join(LOG_FILE),
        }
    }

    /// Strata was started: a separator, and `strata.log` begins anew. The
    /// returned run goes with each of its lines.
    pub fn started(&self) -> u64 {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut inner = self.inner.lock().unwrap();
        inner.run += 1;
        inner.reading = true;
        inner.replaceable = false;
        inner.file = File::create(&self.file)
            .inspect_err(|e| tracing::warn!("{} nicht anlegbar: {e}", self.file.display()))
            .ok();
        self.add(&mut inner, Entry::Started(at));
        inner.run
    }

    /// A line of `run` as the command reader hands it: ending in `\r` if a
    /// progress bar rewrites it.
    pub fn push(&self, run: u64, raw: &str) {
        let (raw, rewritten) = match raw.strip_suffix('\r') {
            Some(raw) => (raw, true),
            None => (raw, false),
        };
        let text = clean(raw);
        let mut inner = self.inner.lock().unwrap();
        if inner.run != run {
            return;
        }
        if inner.replaceable
            && let Some(Entry::Line(last)) = inner.entries.back_mut()
        {
            last.clone_from(&text);
            let _ = self.tx.send(Message::Replace { text: text.clone() });
        } else {
            self.add(&mut inner, Entry::Line(text.clone()));
        }
        inner.replaceable = rewritten;
        // Only finished lines go to the file; a progress bar's states would fill it.
        if !rewritten
            && let Some(file) = inner.file.as_mut()
            && let Err(e) = writeln!(file, "{text}")
        {
            tracing::warn!("{LOG_FILE} nicht geschrieben: {e}");
            inner.file = None;
        }
    }

    /// The output of `run` closed: that Strata ended.
    pub fn ended(&self, run: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.run == run {
            inner.reading = false;
        }
    }

    /// A Strata started by the service still writes output.
    pub fn reading(&self) -> bool {
        self.inner.lock().unwrap().reading
    }

    /// The last `n` lines kept, oldest first.
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut lines: Vec<String> = inner
            .entries
            .iter()
            .rev()
            .map_while(|e| match e {
                Entry::Line(text) => Some(text.clone()),
                Entry::Started(_) => None,
            })
            .take(n)
            .collect();
        lines.reverse();
        lines
    }

    /// The kept output, and everything that follows it.
    pub fn subscribe(&self) -> (Vec<Message>, broadcast::Receiver<Message>) {
        let inner = self.inner.lock().unwrap();
        let backlog = inner.entries.iter().map(Entry::message).collect();
        (backlog, self.tx.subscribe())
    }

    fn add(&self, inner: &mut Inner, entry: Entry) {
        let _ = self.tx.send(entry.message());
        if inner.entries.len() == KEPT {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }
}

/// The text without colour codes and other control characters.
fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters up to a final byte from @ to ~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC (e.g. a window title): up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn clean_drops_escape_sequences() {
        assert_eq!(
            clean("\u{1b}[1;32mok\u{1b}[0m \u{1b}]0;Titel\u{7}da\u{1b}[K"),
            "ok da"
        );
        assert_eq!(clean("a\tb\u{8}"), "a\tb");
    }
}
