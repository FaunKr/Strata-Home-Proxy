//! The PC service's HTTP API (see `docs/pc-agent-api.md`): what each endpoint
//! takes and answers. The PC service writes these types, the proxy reads them,
//! and the web UI shows Strata, jobs and hardware readings with them.
//!
//! The PC service updates itself from the NAS, so an older one often answers
//! a newer proxy. Hence:
//!
//! - Fields are only ever added, never renamed or removed.
//! - A response field added after the first version has a default, so an
//!   older PC service's answer still parses.
//! - An optional request field stays out when unset: the PC service refuses
//!   request fields it does not know. Job kinds without fields (`update`,
//!   `benchmark`, `compare_models`) ignore extra ones.
//!
//! `tests/samples` holds an answer of today's PC service per endpoint; a
//! PC service test checks that they show every field it sends.

mod job;
mod machine;
mod options;
mod output;
mod status;
mod strata;

pub use job::{StrataJob, StrataJobKind, StrataJobRequest, StrataJobState};
pub use machine::{CpuFacts, GpuFacts, MachineFacts, RamStick, WindowsFacts};
pub use options::{NeverRule, StrataOptions, TunerOption, TunerTry};
pub use output::{LOAD_LINES, LoadLines, StrataOutputMessage, StrataOutputNotice};
pub use status::{
    Answer, Cpu, ForcedRestart, ForcedSleep, GamingRequest, Gpu, Hardware, PcStatus, Refused,
    StrataKey,
};
pub use strata::{
    ChangedFile, Incoming, IncomingCommit, MAX_SWITCHED_CONTEXT, SamplingField, SamplingSource,
    SamplingSupport, StrataBenchmark, StrataFamily, StrataInfo, StrataInstall, StrataModel,
    StrataReport, StrataSampling, StrataSize,
};
