//! How the settings page names and explains each value.

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
            "Nur damit die Übersicht /metrics lesen kann (laufende und wartende Anfragen, Token/s). Wird nie angezeigt.",
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
pub fn event_label(kind: &str) -> &'static str {
    match kind {
        "wake_sent" => "Weckpaket gesendet",
        "awake" => "Aufgewacht",
        "wake_timeout" => "Nicht rechtzeitig aufgewacht",
        "sleep_accepted" => "Schlafen angenommen",
        "sleep_refused" => "Schlafen abgelehnt",
        "gaming_rejected" => "Anfrage abgewiesen: Gaming-Modus",
        "mac_learned" => "MAC-Adresse gemeldet",
        "wake_requested" => "Wecken angefordert",
        "sleep_requested" => "Schlafen angefordert",
        "gaming_on" => "Gaming an",
        "gaming_off" => "Gaming aus",
        "keep_awake_set" => "Wach halten",
        "keep_awake_ended" => "Wach halten beendet",
        _ => "Ereignis",
    }
}
