//! The PC-Dienst's HTTP API (see `docs/pc-agent-api.md`): what each endpoint
//! takes and answers. The PC-Dienst writes these types, the Proxy reads them,
//! and the Weboberfläche shows Strata, jobs and Hardwarewerte with them.
//!
//! The PC-Dienst updates itself from the NAS, so an older one often answers
//! a newer Proxy. Hence:
//!
//! - Fields are only ever added, never renamed or removed.
//! - A response field added after the first version has a default, so an
//!   older PC-Dienst's answer still parses.
//! - An optional request field stays out when unset: the PC-Dienst refuses
//!   request fields it does not know. Job kinds without fields (`update`,
//!   `benchmark`, `compare_models`) ignore extra ones.
//!
//! `tests/samples` holds an answer of today's PC-Dienst per endpoint; a
//! PC-Dienst test checks that they show every field it sends.

mod job;
mod output;
mod status;
mod strata;

pub use job::{StrataJob, StrataJobKind, StrataJobRequest, StrataJobState};
pub use output::{StrataOutputMessage, StrataOutputNotice};
pub use status::{
    Answer, Cpu, ForcedRestart, ForcedSleep, GamingRequest, Gpu, Hardware, PcStatus, Refused,
};
pub use strata::{
    ChangedFile, Incoming, IncomingCommit, MAX_SWITCHED_CONTEXT, SamplingField, SamplingSource,
    SamplingSupport, StrataBenchmark, StrataFamily, StrataInfo, StrataModel, StrataReport,
    StrataSampling, StrataSize,
};
