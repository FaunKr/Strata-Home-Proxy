//! `GET /strata-log`: the Strata output as server-sent events, one message
//! as JSON per `data:`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StrataOutputMessage {
    /// The PC service started Strata, at this Unix time; the lines before
    /// stay.
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
        reason: StrataOutputNotice,
    },
}

/// Load lines kept per model: the last ones of its latest load.
pub const LOAD_LINES: usize = 100;

/// A model's load lines: the Strata output of its latest load, from the
/// start of a Strata the PC service started until `/health` answered or
/// that Strata ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadLines {
    /// When the load ended, as Unix time.
    pub at: u64,
    /// At most the last [`LOAD_LINES`] finished lines, oldest first.
    pub lines: Vec<String>,
    /// Whether Strata answered `/health` at the end; `false` when it ended
    /// before, e.g. after a crash.
    #[serde(default)]
    pub answered: bool,
}

/// Why there is no output to follow, or `Running` when there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrataOutputNotice {
    Running,
    Gaming,
    NotRunning,
    /// Strata answers, but the PC service did not start it, so it cannot read
    /// it.
    NotStartedByAgent,
}
