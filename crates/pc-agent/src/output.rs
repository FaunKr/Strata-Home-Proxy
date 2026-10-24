//! The Strata output: what a Strata started by the service writes to its
//! console. The last lines stay in memory for `GET /strata-log`, the lines of
//! the current run also in `strata.log` next to the service's own log. The
//! lines of each model's latest load stay in `load-lines.json` there.

use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub use strata_pc_protocol::{LOAD_LINES, LoadLines, StrataOutputMessage, StrataOutputNotice};
use tokio::sync::broadcast;

/// Lines kept in memory, separators included.
pub const KEPT: usize = 500;
const LOG_FILE: &str = "strata.log";
const LOADS_FILE: &str = "load-lines.json";

enum Entry {
    Started(u64),
    Line(String),
}

impl Entry {
    fn message(&self) -> StrataOutputMessage {
        match self {
            Entry::Started(at) => StrataOutputMessage::Started { at: *at },
            Entry::Line(text) => StrataOutputMessage::Line { text: text.clone() },
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
    /// The model the current run loads and its lines so far, until
    /// `/health` answers or the run ends.
    load: Option<(String, VecDeque<String>)>,
    /// Each model's latest load, as in `load-lines.json`.
    loads: BTreeMap<String, LoadLines>,
}

pub struct StrataOutput {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<StrataOutputMessage>,
    file: PathBuf,
    loads_file: PathBuf,
    /// Writes of `load-lines.json` one after the other, outside `inner`.
    saving: Mutex<()>,
}

impl StrataOutput {
    pub fn new(state_dir: PathBuf) -> Self {
        let loads_file = state_dir.join(LOADS_FILE);
        // Missing before the first load; a broken file counts as none.
        let loads = std::fs::read_to_string(&loads_file)
            .ok()
            .and_then(|text| {
                serde_json::from_str(&text)
                    .inspect_err(|e| tracing::warn!("{} not read: {e}", loads_file.display()))
                    .ok()
            })
            .unwrap_or_default();
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                replaceable: false,
                run: 0,
                reading: false,
                file: None,
                load: None,
                loads,
            }),
            tx: broadcast::channel(4 * KEPT).0,
            file: state_dir.join(LOG_FILE),
            loads_file,
            saving: Mutex::new(()),
        }
    }

    /// Strata was started, loading `model` if it is known: a separator, and
    /// `strata.log` begins anew. The returned run goes with each of its lines.
    pub fn started(&self, model: Option<String>) -> u64 {
        let at = now();
        let mut inner = self.inner.lock().unwrap();
        inner.run += 1;
        inner.reading = true;
        inner.replaceable = false;
        // A load still open never came up: kept as one that did not answer.
        let saved = end_load(&mut inner, false);
        inner.load = model.map(|model| (model, VecDeque::new()));
        inner.file = File::create(&self.file)
            .inspect_err(|e| tracing::warn!("{} cannot be created: {e}", self.file.display()))
            .ok();
        self.add(&mut inner, Entry::Started(at));
        let run = inner.run;
        drop(inner);
        if saved {
            self.save();
        }
        run
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
            let _ = self
                .tx
                .send(StrataOutputMessage::Replace { text: text.clone() });
        } else {
            self.add(&mut inner, Entry::Line(text.clone()));
        }
        inner.replaceable = rewritten;
        // Only finished lines go to the file and the load lines; a progress
        // bar's states would fill them.
        if rewritten {
            return;
        }
        if let Some((_, lines)) = inner.load.as_mut() {
            if lines.len() == LOAD_LINES {
                lines.pop_front();
            }
            lines.push_back(text.clone());
        }
        if let Some(file) = inner.file.as_mut()
            && let Err(e) = writeln!(file, "{text}")
        {
            tracing::warn!("{LOG_FILE} not written: {e}");
            inner.file = None;
        }
    }

    /// The output of `run` closed: that Strata ended, and a load still open
    /// with it, as one that did not answer.
    pub fn ended(&self, run: u64) {
        let saved = {
            let mut inner = self.inner.lock().unwrap();
            if inner.run != run {
                return;
            }
            inner.reading = false;
            end_load(&mut inner, false)
        };
        if saved {
            self.save();
        }
    }

    /// `run` answers `/health`: its load is over.
    pub fn loaded(&self, run: u64) {
        let saved = {
            let mut inner = self.inner.lock().unwrap();
            inner.run == run && end_load(&mut inner, true)
        };
        if saved {
            self.save();
        }
    }

    /// Whether the load of `run` is still open.
    pub fn loading(&self, run: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.run == run && inner.load.is_some()
    }

    /// The service stops Strata: a load still open is cut short by it and
    /// says nothing about the model, so the model keeps its earlier lines.
    pub fn stopping(&self) {
        self.inner.lock().unwrap().load = None;
    }

    /// `model` was deleted: its load lines go with it.
    pub fn forget_load(&self, model: &str) {
        let removed = {
            let loads = &mut self.inner.lock().unwrap().loads;
            let before = loads.len();
            loads.retain(|id, _| !id.eq_ignore_ascii_case(model));
            loads.len() != before
        };
        if removed {
            self.save();
        }
    }

    /// The lines of `model`'s latest load, if one was seen. Ids match in
    /// any letter case: a start before Strata's catalog was read knows the
    /// model by its run config's lower-case name.
    pub fn load_lines(&self, model: &str) -> Option<LoadLines> {
        let inner = self.inner.lock().unwrap();
        inner
            .loads
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(model))
            .map(|(_, load)| load.clone())
    }

    /// Writes the kept loads to `load-lines.json`, without holding up the
    /// output while the disk is slow.
    fn save(&self) {
        let _one_at_a_time = self.saving.lock().unwrap();
        let text =
            serde_json::to_string(&self.inner.lock().unwrap().loads).expect("load lines serialize");
        if let Err(e) = crate::write_replacing(&self.loads_file, &text) {
            tracing::warn!("{} not written: {e}", self.loads_file.display());
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
    pub fn subscribe(
        &self,
    ) -> (
        Vec<StrataOutputMessage>,
        broadcast::Receiver<StrataOutputMessage>,
    ) {
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

/// Keeps the open load as its model's latest; whether there was one.
fn end_load(inner: &mut Inner, answered: bool) -> bool {
    let Some((model, lines)) = inner.load.take() else {
        return false;
    };
    let load = LoadLines {
        at: now(),
        lines: lines.into(),
        answered,
    };
    inner.loads.retain(|id, _| !id.eq_ignore_ascii_case(&model));
    inner.loads.insert(model, load);
    true
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
            clean("\u{1b}[1;32mok\u{1b}[0m \u{1b}]0;Title\u{7}there\u{1b}[K"),
            "ok there"
        );
        assert_eq!(clean("a\tb\u{8}"), "a\tb");
    }
}
