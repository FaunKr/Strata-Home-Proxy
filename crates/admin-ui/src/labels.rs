//! How the pages name and explain things.

use crate::api::{EventKind, Overview, PcView, StrataJobKind, StrataView};

/// Messages that the proxy and the overview both show.
pub const NOT_CONFIGURED: &str = "Die Strata-Adresse fehlt, der Proxy ist nicht eingerichtet.";
pub const NO_MAC: &str = "Keine MAC-Adresse bekannt, der PC kann nicht geweckt werden.";
pub const NO_AGENT: &str = "Kein PC-Dienst eingerichtet (PC_AGENT_URL).";

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
        "STRATA_API_KEY" => (
            "Strata API key",
            "The same key as in Strata. The proxy adds it to every request to Strata and uses it for its own calls; clients use their own API keys. Never shown.",
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
    keys: &["PC_MAC", "WOL_ADDR", "PC_AGENT_URL", "PC_AGENT_TOKEN"],
    advanced: &[],
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
    help: "Where Strata runs, and the key the proxy uses to talk to it.",
    keys: &["STRATA_URL", "STRATA_API_KEY"],
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

/// The unit a value is entered in, shown inside its field.
pub fn unit(key: &str) -> Option<&'static str> {
    if key.ends_with("_SECS") {
        Some("s")
    } else if key.ends_with("_MS") {
        Some("ms")
    } else if key.ends_with("_BYTES") {
        Some("bytes")
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
        | EventKind::WakeSent
        | EventKind::WakeRequested
        | EventKind::KeepAwakeSet
        | EventKind::StrataJobCancelled => "is-warn",
        EventKind::SleepAccepted | EventKind::SleepRequested => "is-idle",
        _ => "is-ok",
    }
}

/// What a Strata job does, for the history and the page.
pub fn strata_job_label(kind: StrataJobKind) -> &'static str {
    match kind {
        StrataJobKind::Install => "Einrichtung",
        StrataJobKind::AddModel => "Modell installieren",
        StrataJobKind::Activate => "Modell aktivieren",
        StrataJobKind::Delete => "Modell löschen",
        StrataJobKind::Update => "Update",
        StrataJobKind::Benchmark => "Benchmark",
        StrataJobKind::SetKey => "Strata-Key wechseln",
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
        (StrataView::Busy { .. }, _) => ("Busy", "is-ok"),
    }
}
