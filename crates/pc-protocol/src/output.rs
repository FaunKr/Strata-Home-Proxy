//! `GET /strata-log`: the Strata-Ausgabe as server-sent events, one message
//! as JSON per `data:`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StrataOutputMessage {
    /// The PC-Dienst started Strata, at this Unix time; the lines before
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

/// Why there is no output to follow, or `Running` when there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrataOutputNotice {
    Running,
    Gaming,
    NotRunning,
    /// Strata answers, but the PC-Dienst did not start it, so it cannot read
    /// it.
    NotStartedByAgent,
}
