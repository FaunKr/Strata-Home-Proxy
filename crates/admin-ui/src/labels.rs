//! How the pages name and explain things.

use crate::api::{EventKind, StrataJobKind};

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
            "Strata-Adresse",
            "Basis-URL von Strata auf dem PC, z. B. http://192.168.1.50:8080.",
        ),
        "STRATA_API_KEY" => (
            "API-Key von Strata",
            "Derselbe Key wie in Strata. Der Proxy setzt ihn bei jeder Anfrage an Strata ein und nutzt ihn für seine eigenen Aufrufe (z. B. die Übersicht); Clients nutzen ihre eigenen API-Keys. Wird nie angezeigt.",
        ),
        "PC_MAC" => (
            "MAC-Adresse des PCs",
            "Netzwerkkarte, die das Weckpaket empfängt. Meldet der PC-Dienst seine MAC, gilt diese.",
        ),
        "WOL_ADDR" => (
            "Weckadresse",
            "Wohin das Weckpaket geht, meist die Broadcast-Adresse mit Port 9, z. B. 192.168.1.255:9.",
        ),
        "PC_AGENT_URL" => (
            "Adresse des PC-Dienstes",
            "z. B. http://192.168.1.50:8091. Ohne ihn kein Gaming-Modus und kein automatisches Schlafen.",
        ),
        "PC_AGENT_TOKEN" => (
            "Token des PC-Dienstes",
            "Dasselbe Geheimnis wie in strata-pc-agent.toml. Wird nie angezeigt.",
        ),
        "IDLE_TIMEOUT_SECS" => (
            "Leerlauf bis zum Schlafen",
            "Sekunden ohne Anfrage, bevor der PC schlafen soll.",
        ),
        "WAKE_TIMEOUT_SECS" => (
            "Längste Weckwartezeit",
            "Sekunden, die eine Anfrage höchstens auf das Aufwachen wartet.",
        ),
        "WOL_RESEND_SECS" => (
            "Weckpaket wiederholen",
            "Sekunden zwischen zwei Weckpaketen, solange der PC nicht antwortet.",
        ),
        "HEALTH_POLL_MS" => (
            "Prüfabstand beim Aufwachen",
            "Millisekunden zwischen zwei Abfragen von /health.",
        ),
        "PING_INTERVAL_SECS" => (
            "Ping-Abstand",
            "Sekunden Stille in einem Stream, nach denen ein Ping gesendet wird.",
        ),
        "IDLE_CHECK_SECS" => (
            "Leerlauf-Prüfung",
            "Sekunden zwischen zwei Prüfungen, ob der PC schlafen kann.",
        ),
        "SLEEP_RETRY_SECS" => (
            "Erneut fragen nach Ablehnung",
            "Sekunden, bis der PC nach einem abgelehnten Schlafen erneut gefragt wird.",
        ),
        "SLEEP_SETTLE_SECS" => (
            "Einschlafzeit",
            "Sekunden nach einem angenommenen Schlafen, in denen Strata als weg gilt.",
        ),
        "PROBE_TIMEOUT_MS" => (
            "Zeitlimit für Abfragen",
            "Millisekunden für /health und den PC-Dienst.",
        ),
        "MAX_BODY_BYTES" => (
            "Größte Anfrage",
            "Bytes, die eine Anfrage höchstens haben darf.",
        ),
        "LISTEN_ADDR" => (
            "Adresse des Proxys",
            "Adresse und Port, auf denen der Proxy lauscht.",
        ),
        _ => ("", ""),
    };
    Label {
        title: if title.is_empty() { "Unbekannt" } else { title },
        help,
    }
}

/// How the overview names an event from the history.
pub fn event_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::WakeSent => "Weckpaket gesendet",
        EventKind::Awake => "Aufgewacht",
        EventKind::WakeTimeout => "Nicht rechtzeitig aufgewacht",
        EventKind::SleepAccepted => "Schlafen angenommen",
        EventKind::SleepRefused => "Schlafen abgelehnt",
        EventKind::GamingRejected => "Anfrage abgewiesen: Gaming-Modus",
        EventKind::PasswordChanged => "Passwort geändert",
        EventKind::SettingSaved => "Einstellung gespeichert",
        EventKind::ApiKeyCreated => "API-Key angelegt",
        EventKind::ApiKeyRevoked => "API-Key widerrufen",
        EventKind::SettingReset => "Einstellung zurückgesetzt",
        EventKind::MacLearned => "MAC-Adresse gemeldet",
        EventKind::WakeRequested => "Wecken angefordert",
        EventKind::SleepRequested => "Schlafen angefordert",
        EventKind::GamingOn => "Gaming an",
        EventKind::GamingOff => "Gaming aus",
        EventKind::KeepAwakeSet => "Wach halten",
        EventKind::KeepAwakeEnded => "Wach halten beendet",
        EventKind::StrataJobStarted => "Strata-Vorgang gestartet",
        EventKind::StrataJobDone => "Strata-Vorgang fertig",
        EventKind::StrataJobFailed => "Strata-Vorgang fehlgeschlagen",
        EventKind::StrataJobCancelled => "Strata-Vorgang abgebrochen",
        EventKind::StrataKeyChanged => "Strata-Key gewechselt",
        EventKind::PcServiceRegistered => "PC-Dienst eingerichtet",
        EventKind::PcServiceUpdateRequested => "Update des PC-Dienstes angestoßen",
        EventKind::PcServiceUpdated => "PC-Dienst aktualisiert",
        EventKind::PcServiceUpdateFailed => "Update des PC-Dienstes fehlgeschlagen",
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
