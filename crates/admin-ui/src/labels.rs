//! How the pages name and explain things.

use crate::api::{EventKind, Overview, PcView, StrataJobKind, StrataView};

/// Messages that the proxy and the overview both show.
pub const NOT_CONFIGURED: &str = "The Strata address is missing, the proxy is not set up.";
pub const NO_MAC: &str = "No MAC address is known, so the PC cannot be woken.";
pub const NO_AGENT: &str = "No PC service is set up (PC_AGENT_URL).";
pub const NO_ANSWER: &str = "The PC service does not answer.";
pub const TOO_OLD_TO_FORCE: &str =
    "The PC service is too old for this, update it in the PC section.";
pub const TOO_OLD_FOR_KEY: &str =
    "The PC service is too old for the Strata key, update it in the PC section.";

pub struct Label {
    pub title: &'static str,
    pub help: &'static str,
}

pub fn label(key: &str) -> Label {
    let (title, help) = match key {
        "STRATA_URL" => (
            "Strata address",
            "Base URL of Strata on the PC, e.g. http://192.168.1.50:8080.",
        ),
        "PC_MAC" => (
            "MAC address",
            "Network card that receives the wake packet. A MAC the PC service reports takes precedence.",
        ),
        "WOL_ADDR" => (
            "Wake address",
            "Where the wake packet goes, usually the broadcast address on port 9, e.g. 192.168.1.255:9.",
        ),
        "PC_AGENT_URL" => (
            "PC service address",
            "E.g. http://192.168.1.50:8091. Without it there is no gaming mode and no automatic sleep.",
        ),
        "PC_AGENT_TOKEN" => (
            "PC service token",
            "The same secret as in strata-pc-agent.toml. Never shown.",
        ),
        "SHELLY_URL" => (
            "Power plug address",
            "Optional Shelly plug the PC is plugged into, e.g. 192.168.1.228. The overview then shows the PC's power draw.",
        ),
        "CODE_BENCH_URL" => (
            "Code benchmark address",
            "Optional: where the proxy reaches the code-bench container, e.g. http://127.0.0.1:8096. Without it there is no code benchmark.",
        ),
        "UPDATE_SUMMARY_TIMEOUT_SECS" => (
            "Update summary limit",
            "How long Strata may take to write the summary of an update. Empty uses the suggestion below.",
        ),
        "CODE_BENCH_TASK_TIMEOUT_SECS" => (
            "Code benchmark limit per task",
            "How long opencode may work on each task of the code benchmark; what it wrote by then is still scored. Empty uses the suggestion below.",
        ),
        "PLUG_SAMPLE_SECS" => (
            "Power history interval",
            "Time between two readings of the power plug kept for the history.",
        ),
        "IDLE_TIMEOUT_SECS" => (
            "Idle time before sleep",
            "Without requests for this long, the PC goes to sleep.",
        ),
        "WAKE_TIMEOUT_SECS" => (
            "Longest wake wait",
            "How long a request waits at most for the PC to wake.",
        ),
        "WOL_RESEND_SECS" => (
            "Resend wake packet",
            "Time between two wake packets while the PC does not answer.",
        ),
        "HEALTH_POLL_MS" => (
            "Check interval while waking",
            "Time between two calls of /health while the PC wakes.",
        ),
        "PING_INTERVAL_SECS" => (
            "Ping interval",
            "Silence in a stream after which a ping is sent.",
        ),
        "IDLE_CHECK_SECS" => (
            "Idle check",
            "Time between two checks whether the PC can sleep.",
        ),
        "SLEEP_RETRY_SECS" => (
            "Ask again after refusal",
            "Wait before asking again after the PC refused to sleep.",
        ),
        "SLEEP_SETTLE_SECS" => (
            "Settle time",
            "After the PC accepted sleep, Strata counts as gone for this long.",
        ),
        "PROBE_TIMEOUT_MS" => (
            "Probe timeout",
            "Time limit for /health and calls to the PC service.",
        ),
        "MAX_BODY_BYTES" => ("Largest request", "Requests above this size are refused."),
        "LISTEN_ADDR" => ("Proxy address", "Address and port the proxy listens on."),
        "POWER_AWAKE_WATTS" => (
            "Power while awake",
            "What the PC draws while awake. The history uses it to estimate energy when no power plug measures it.",
        ),
        "POWER_ASLEEP_WATTS" => (
            "Power while asleep",
            "What the PC draws while asleep. The history uses it to estimate energy when no power plug measures it.",
        ),
        "PRICE_CT_PER_KWH" => (
            "Electricity price",
            "Price per kWh in cents, e.g. 32.5. Turns the PC's energy use into cost.",
        ),
        _ => ("", ""),
    };
    Label {
        title: if title.is_empty() { "Unknown" } else { title },
        help,
    }
}

/// A group of settings shown together in an area, saved with one button.
pub struct SectionDef {
    pub id: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub keys: &'static [&'static str],
    /// Rarely changed; folded away under "Advanced".
    pub advanced: &'static [&'static str],
}

pub const PC_CONNECTION: SectionDef = SectionDef {
    id: "pc-connection",
    title: "Connection",
    help: "How the proxy reaches your PC. The install command fills in the PC service values.",
    keys: &[
        "PC_MAC",
        "WOL_ADDR",
        "PC_AGENT_URL",
        "PC_AGENT_TOKEN",
        "SHELLY_URL",
    ],
    advanced: &["PLUG_SAMPLE_SECS"],
};

pub const PC_WAKE_SLEEP: SectionDef = SectionDef {
    id: "pc-wake-sleep",
    title: "Wake & sleep",
    help: "When the PC goes to sleep, and how long requests wait for it to wake.",
    keys: &["IDLE_TIMEOUT_SECS"],
    advanced: &[
        "WAKE_TIMEOUT_SECS",
        "WOL_RESEND_SECS",
        "HEALTH_POLL_MS",
        "IDLE_CHECK_SECS",
        "SLEEP_RETRY_SECS",
        "SLEEP_SETTLE_SECS",
    ],
};

pub const STRATA_CONNECTION: SectionDef = SectionDef {
    id: "strata-connection",
    title: "Connection",
    help: "Where Strata runs.",
    keys: &["STRATA_URL", "CODE_BENCH_URL"],
    advanced: &[],
};

pub const STRATA_TIMEOUTS: SectionDef = SectionDef {
    id: "strata-timeouts",
    title: "Timeouts",
    help: "How long work on the model may take. Empty fields use a suggestion from the slowest installed model in Compare models.",
    keys: &[
        "UPDATE_SUMMARY_TIMEOUT_SECS",
        "CODE_BENCH_TASK_TIMEOUT_SECS",
    ],
    advanced: &[],
};

pub const SYSTEM: SectionDef = SectionDef {
    id: "system",
    title: "Proxy",
    help: "Network and limits of the proxy itself.",
    keys: &[
        "LISTEN_ADDR",
        "MAX_BODY_BYTES",
        "PING_INTERVAL_SECS",
        "PROBE_TIMEOUT_MS",
    ],
    advanced: &[],
};

pub const ENERGY: SectionDef = SectionDef {
    id: "energy",
    title: "Energy",
    help: "What the PC draws and what power costs, for the energy use and cost in the history.",
    keys: &[
        "POWER_AWAKE_WATTS",
        "POWER_ASLEEP_WATTS",
        "PRICE_CT_PER_KWH",
    ],
    advanced: &[],
};

/// The unit a value is entered in, shown inside its field.
pub fn unit(key: &str) -> Option<&'static str> {
    if key.ends_with("_SECS") {
        Some("s")
    } else if key.ends_with("_MS") {
        Some("ms")
    } else if key.ends_with("_BYTES") {
        Some("bytes")
    } else if key.ends_with("_WATTS") {
        Some("W")
    } else if key.ends_with("_CT_PER_KWH") {
        Some("ct/kWh")
    } else {
        None
    }
}

/// How the overview names an event from the history.
pub fn event_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::WakeSent => "Wake packet sent",
        EventKind::Awake => "PC woke up",
        EventKind::WakeTimeout => "PC did not wake in time",
        EventKind::SleepAccepted => "PC went to sleep",
        EventKind::SleepRefused => "PC refused to sleep",
        EventKind::GamingRejected => "Request refused: gaming mode",
        EventKind::PcSeenAwake => "PC seen awake",
        EventKind::PcSeenAsleep => "PC seen asleep",
        EventKind::PcSeenGaming => "PC seen in gaming mode",
        EventKind::ProxyStarted => "Proxy started",
        EventKind::ProxyStopped => "Proxy stopped",
        EventKind::PasswordChanged => "Password changed",
        EventKind::SettingSaved => "Setting saved",
        EventKind::ApiKeyCreated => "API key created",
        EventKind::ApiKeyRevoked => "API key revoked",
        EventKind::SettingReset => "Setting reset",
        EventKind::MacLearned => "MAC address reported",
        EventKind::WakeRequested => "Wake requested",
        EventKind::SleepRequested => "Sleep requested",
        EventKind::GamingOn => "Gaming mode on",
        EventKind::GamingOff => "Gaming mode off",
        EventKind::KeepAwakeSet => "Keep awake",
        EventKind::KeepAwakeEnded => "Keep awake ended",
        EventKind::StrataJobStarted => "Strata job started",
        EventKind::StrataJobDone => "Strata job done",
        EventKind::StrataJobFailed => "Strata job failed",
        EventKind::StrataJobCancelled => "Strata job cancelled",
        EventKind::StrataKeyChanged => "Strata key changed",
        EventKind::PcServiceRegistered => "PC service set up",
        EventKind::PcServiceUpdateRequested => "PC service update requested",
        EventKind::PcServiceUpdated => "PC service updated",
        EventKind::PcServiceUpdateFailed => "PC service update failed",
        EventKind::ForcedSleep => "Forced sleep",
        EventKind::ForcedStrataRestart => "Forced Strata restart",
        EventKind::UpdateSummaryRequested => "Update summary requested",
    }
}

/// The color of an event's dot; the text says the same.
pub fn event_class(kind: EventKind) -> &'static str {
    match kind {
        EventKind::WakeTimeout | EventKind::StrataJobFailed | EventKind::PcServiceUpdateFailed => {
            "is-bad"
        }
        EventKind::SleepRefused
        | EventKind::GamingRejected
        | EventKind::GamingOn
        | EventKind::PcSeenGaming
        | EventKind::WakeSent
        | EventKind::WakeRequested
        | EventKind::KeepAwakeSet
        | EventKind::StrataJobCancelled
        | EventKind::ForcedSleep
        | EventKind::ForcedStrataRestart => "is-warn",
        EventKind::SleepAccepted
        | EventKind::SleepRequested
        | EventKind::PcSeenAsleep
        | EventKind::ProxyStarted
        | EventKind::ProxyStopped => "is-idle",
        _ => "is-ok",
    }
}

/// The states the history shows, in the order of its bars and its legend:
/// their name and the class of their color.
pub const HISTORY_STATES: &[(&str, &str)] = &[
    ("Awake", "seg-awake"),
    ("Gaming", "seg-gaming"),
    ("Asleep", "seg-asleep"),
    ("Unknown", "seg-unknown"),
];

/// What a Strata job does, for the history and the page.
pub fn strata_job_label(kind: StrataJobKind) -> &'static str {
    match kind {
        StrataJobKind::Install => "Setting up Strata",
        StrataJobKind::AddModel => "Loading model",
        StrataJobKind::Activate => "Making model active",
        StrataJobKind::Delete => "Deleting model",
        StrataJobKind::Update => "Updating Strata",
        StrataJobKind::Benchmark => "Benchmark",
        StrataJobKind::SetKey => "New Strata key",
        StrataJobKind::SetSpeedProjection => "Switching speed projection",
        StrataJobKind::SetContext => "Changing context",
        StrataJobKind::CompareModels => "Comparing models",
        StrataJobKind::SwitchToTerminal => "Switching to the Strata Terminal",
        StrataJobKind::RemoveOldStrataFolder => "Removing the old Strata folder",
    }
}

/// The PC's state as a badge: text and color class.
pub fn pc_badge(pc: PcView) -> (&'static str, &'static str) {
    match pc {
        PcView::Unknown => ("Unknown", "is-idle"),
        PcView::Asleep => ("Asleep", "is-idle"),
        PcView::Waking => ("Waking", "is-warn"),
        PcView::Awake => ("Awake", "is-ok"),
        PcView::Gaming => ("Gaming", "is-ok"),
    }
}

/// Strata's state as a badge: text and color class.
pub fn strata_badge(overview: &Overview) -> (&'static str, &'static str) {
    match (&overview.strata, overview.pc) {
        (StrataView::Offline, PcView::Gaming) => ("Paused", "is-warn"),
        (StrataView::Offline, PcView::Waking) => ("Starting", "is-warn"),
        (StrataView::Offline, PcView::Awake) => ("Offline", "is-bad"),
        (StrataView::Offline, _) => ("Offline", "is-idle"),
        (StrataView::Loading, _) => ("Loading", "is-warn"),
        (StrataView::Unloaded, _) => ("Idle", "is-ok"),
        (StrataView::Ready, _) => ("Ready", "is-ok"),
        (StrataView::Busy { .. }, _) => ("Busy", "is-warn"),
    }
}
