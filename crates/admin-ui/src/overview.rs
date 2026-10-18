//! The overview page: state of PC and Strata, live from the proxy, and the
//! buttons to wake, sleep, switch gaming mode and keep the PC awake.

use leptos::prelude::*;

use crate::{
    api::{
        KeepAwake, KeepAwakeChoice, Overview, PcServiceView, PcView, SetGaming, SleepNow,
        SleepView, StrataView, UpdatePcService, Wake, error_text,
    },
    labels::{NO_MAC, event_label, strata_job_label},
};

/// The buttons' actions; made once per page, so a result stays visible
/// while updates come in.
#[derive(Clone, Copy)]
struct Actions {
    wake: ServerAction<Wake>,
    sleep: ServerAction<SleepNow>,
    gaming: ServerAction<SetGaming>,
    keep: ServerAction<KeepAwake>,
    update: ServerAction<UpdatePcService>,
}

/// The latest overview, and seconds counted since it arrived.
#[derive(Clone, Copy)]
pub(crate) struct Live {
    pub overview: RwSignal<Option<Overview>>,
    since: RwSignal<u64>,
    pub lost: RwSignal<bool>,
}

impl Live {
    /// Follows the overview stream while the page that calls it is shown.
    pub fn follow() -> Self {
        let live = Self {
            overview: RwSignal::new(None),
            since: RwSignal::new(0),
            lost: RwSignal::new(false),
        };
        #[cfg(feature = "hydrate")]
        client::connect(live);
        live
    }
}

#[component]
pub fn OverviewPage() -> impl IntoView {
    let live = Live::follow();
    let actions = Actions {
        wake: ServerAction::new(),
        sleep: ServerAction::new(),
        gaming: ServerAction::new(),
        keep: ServerAction::new(),
        update: ServerAction::new(),
    };
    view! {
        <h1>"Übersicht"</h1>
        {move || match live.overview.get() {
            None if live.lost.get() => {
                view! { <p class="warning" role="status">"Verbindung zum Proxy unterbrochen, verbinde neu …"</p> }
                    .into_any()
            }
            None => view! { <p>"Lädt …"</p> }.into_any(),
            Some(o) => view! { <Body overview=o live actions /> }.into_any(),
        }}
    }
}

#[component]
fn Body(overview: Overview, live: Live, actions: Actions) -> impl IntoView {
    let lost = move || {
        live.lost
            .get()
            .then(|| view! { <p class="warning" role="status">"Verbindung unterbrochen, die Anzeige ist nicht aktuell."</p> })
    };
    let warnings = [
        (!overview.configured).then_some(
            "Der Proxy ist noch nicht eingerichtet: die Strata-Adresse fehlt (Einstellungen).",
        ),
        (!overview.mac_known).then_some(NO_MAC),
        (!overview.agent_configured).then_some(
            "Kein PC-Dienst eingerichtet: kein Gaming-Modus und kein automatisches Schlafen.",
        ),
    ];
    let no_key = (!overview.api_key_exists).then(|| {
        view! {
            <p class="warning" role="status">
                "Noch kein API-Key angelegt: jeder Client bekommt 401. "
                <a href="/admin/api-keys">"Key anlegen"</a>
            </p>
        }
    });
    let job = overview.strata_job.clone().map(|j| {
        let percent = j.percent.map(|p| format!(", {p} %")).unwrap_or_default();
        view! {
            <p class="warning" role="status">
                {format!("Strata-Vorgang läuft: {}{percent}. ", strata_job_label(j.kind))}
                <a href="/admin/strata">"Ansehen"</a>
            </p>
        }
    });
    let last_request = overview
        .last_request
        .clone()
        .unwrap_or_else(|| "noch keine seit dem Start".to_string());
    let events = overview.events.clone();
    view! {
        {lost}
        {warnings
            .into_iter()
            .flatten()
            .map(|w| view! { <p class="warning">{w}</p> })
            .collect_view()}
        {no_key}
        {job}
        <section class="panel cards">
            <div>
                <h2>"PC"</h2>
                <p class="state">{pc_text(overview.pc)}</p>
            </div>
            <div>
                <h2>"Strata"</h2>
                <p class="state">{strata_text(&overview.strata)}</p>
                <a class="button secondary" href="/admin/strata-log">"Strata-Ausgabe"</a>
            </div>
            <div>
                <h2>"Letzte Anfrage"</h2>
                <p class="state">{last_request}</p>
            </div>
            <div>
                <h2>"Schlafen"</h2>
                <p class="state">
                    <SleepText sleep=overview.sleep.clone() now=overview.now since=live.since />
                </p>
            </div>
        </section>
        {overview
            .pc_service
            .clone()
            .map(|service| view! { <PcService service pc=overview.pc update=actions.update /> })}
        <Controls overview actions />
        <section class="panel">
            <h2>"Letzte Ereignisse"</h2>
            {if events.is_empty() {
                view! { <p class="hint">"Noch keine."</p> }.into_any()
            } else {
                view! {
                    <ul class="events">
                        {events
                            .into_iter()
                            .map(|e| {
                                view! {
                                    <li>
                                        <span class="time">{e.time}</span>
                                        " "
                                        {event_label(e.kind)}
                                        {e.detail.map(|d| view! { <span class="hint">{format!(" ({d})")}</span> })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                }
                    .into_any()
            }}
        </section>
    }
}

/// The PC service's version, and the update button when the NAS carries
/// another one.
#[component]
fn PcService(
    service: PcServiceView,
    pc: PcView,
    update: ServerAction<UpdatePcService>,
) -> impl IntoView {
    let awake = matches!(pc, PcView::Awake | PcView::Gaming);
    let version = service
        .version
        .clone()
        .unwrap_or_else(|| "alt, ohne Versionsangabe".to_string());
    let hint = if service.updating {
        Some("Update läuft, der PC-Dienst startet neu …".to_string())
    } else if service.update_available && awake {
        Some(match &service.available {
            Some(v) => format!("Update verfügbar: {v}"),
            None => "Update verfügbar".to_string(),
        })
    } else {
        None
    };
    let button = (service.update_available && !service.updating).then(|| {
        view! {
            <div class="actions">
                <button
                    disabled=move || !awake || update.pending().get()
                    on:click=move |_| {
                        update.dispatch(UpdatePcService {});
                    }
                >
                    {move || match (awake, update.pending().get()) {
                        (false, _) => "Update (PC schläft)",
                        (true, true) => "Update wird geladen …",
                        (true, false) => "Update",
                    }}
                </button>
            </div>
        }
    });
    view! {
        <section class="panel">
            <h2>"PC-Dienst"</h2>
            <p>{format!("Version {version}")}</p>
            {hint.map(|h| view! { <p class="warning" role="status">{h}</p> })}
            {service
                .update_error
                .map(|e| view! { <p class="error" role="alert">{format!("Letztes Update fehlgeschlagen: {e}")}</p> })}
            {button}
            {result_text(update.value())}
        </section>
    }
}

fn pc_text(pc: PcView) -> &'static str {
    match pc {
        PcView::Unknown => "unbekannt",
        PcView::Asleep => "schläft",
        PcView::Waking => "wacht auf …",
        PcView::Awake => "wach",
        PcView::Gaming => "Gaming-Modus",
    }
}

fn strata_text(strata: &StrataView) -> String {
    match strata {
        StrataView::Offline => "offline".into(),
        StrataView::Loading => "lädt …".into(),
        StrataView::Unloaded => "bereit, Modell lädt bei der nächsten Anfrage".into(),
        StrataView::Ready => "bereit".into(),
        StrataView::Busy {
            processing,
            deferred,
            tokens_per_sec,
        } => {
            let mut text = match processing {
                1 => "bearbeitet 1 Anfrage".to_string(),
                n => format!("bearbeitet {n} Anfragen"),
            };
            if let Some(waiting) = deferred.filter(|n| *n > 0) {
                text.push_str(&format!(", {waiting} wartend"));
            }
            if let Some(tps) = tokens_per_sec {
                text.push_str(&format!(", {tps:.0} Token/s"));
            }
            text
        }
    }
}

/// `4:05` or `1:02:03`.
fn duration_text(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The countdown, counted down in the browser between two updates.
#[component]
fn SleepText(sleep: SleepView, now: u64, since: RwSignal<u64>) -> impl IntoView {
    let left = move |at: u64| at as i64 - (now + since.get()) as i64;
    move || match &sleep {
        SleepView::At(at) => match left(*at) {
            l if l > 0 => format!("in {}", duration_text(l)),
            _ => "gleich".to_string(),
        },
        SleepView::Asleep => "PC schläft".into(),
        SleepView::Waking => "PC wacht auf".into(),
        SleepView::Gaming => "Gaming-Modus".into(),
        SleepView::KeptAwake { until } => format!("Wach gehalten bis {until}"),
        SleepView::Busy => "Anfrage läuft".into(),
        SleepView::Refused { reason, retry_at } => {
            format!(
                "Abgelehnt: {reason}, nächster Versuch in {}",
                duration_text(left(*retry_at))
            )
        }
        SleepView::NoAgent => "nie (kein PC-Dienst)".into(),
        SleepView::Unknown => "Zustand des PCs unbekannt".into(),
    }
}

/// Asks first, but only when Strata is working on a request.
fn confirmed(busy: bool, question: &str) -> bool {
    if !busy {
        return true;
    }
    #[cfg(feature = "hydrate")]
    {
        window().confirm_with_message(question).unwrap_or(false)
    }
    #[cfg(not(feature = "hydrate"))]
    {
        let _ = question;
        true
    }
}

fn result_text(
    value: impl Get<Value = Option<Result<(), ServerFnError>>> + Send + Sync + 'static,
) -> impl IntoView {
    move || {
        value
            .get()
            .and_then(|r| r.err())
            .map(|e| view! { <p class="error" role="alert">{error_text(&e)}</p> })
    }
}

#[component]
fn Controls(overview: Overview, actions: Actions) -> impl IntoView {
    let Actions {
        wake,
        sleep,
        gaming,
        keep,
        update: _,
    } = actions;
    let busy = matches!(overview.strata, StrataView::Busy { .. });
    let pc = overview.pc;
    let agent = overview.agent_configured;
    let can_wake =
        overview.mac_known && overview.configured && matches!(pc, PcView::Asleep | PcView::Unknown);
    let can_sleep = agent && pc == PcView::Awake;
    let gaming_on = pc == PcView::Gaming;
    let kept = overview.keep_awake_until.clone();
    let keep_button = move |choice: KeepAwakeChoice, text: &'static str| {
        view! {
            <button
                class="secondary"
                disabled=move || keep.pending().get()
                on:click=move |_| {
                    keep.dispatch(KeepAwake { choice });
                }
            >
                {text}
            </button>
        }
    };
    view! {
        <section class="panel">
            <h2>"Steuerung"</h2>
            <div class="actions">
                <button
                    disabled=move || !can_wake || wake.pending().get()
                    on:click=move |_| {
                        wake.dispatch(Wake {});
                    }
                >
                    "Jetzt wecken"
                </button>
                <button
                    disabled=move || !can_sleep || sleep.pending().get()
                    on:click=move |_| {
                        if confirmed(busy, "Strata bearbeitet gerade eine Anfrage. Trotzdem schlafen legen?") {
                            sleep.dispatch(SleepNow {});
                        }
                    }
                >
                    "Jetzt schlafen legen"
                </button>
                <button
                    disabled=move || !agent || gaming.pending().get()
                    on:click=move |_| {
                        let enabled = !gaming_on;
                        if !enabled
                            || confirmed(busy, "Strata bearbeitet gerade eine Anfrage, Gaming beendet Strata. Trotzdem einschalten?")
                        {
                            gaming.dispatch(SetGaming { enabled });
                        }
                    }
                >
                    {move || match (gaming.pending().get(), gaming_on) {
                        (true, false) => "Gaming wird eingeschaltet …",
                        (true, true) => "Gaming wird ausgeschaltet …",
                        (false, false) => "Gaming an",
                        (false, true) => "Gaming aus",
                    }}
                </button>
            </div>
            {result_text(wake.value())}
            {result_text(sleep.value())}
            {result_text(gaming.value())}
            <h3>"Wach halten"</h3>
            {kept.map(|until| view! { <p class="hint">{format!("Bis {until}, auch über einen Neustart.")}</p> })}
            <div class="actions">
                {keep_button(KeepAwakeChoice::OneHour, "1 h")}
                {keep_button(KeepAwakeChoice::FourHours, "4 h")}
                {keep_button(KeepAwakeChoice::EightHours, "8 h")}
                {keep_button(KeepAwakeChoice::Morning, "bis morgen 8 Uhr")}
                {overview.keep_awake_until.is_some().then(|| keep_button(KeepAwakeChoice::Off, "beenden"))}
            </div>
            {result_text(keep.value())}
        </section>
    }
}

/// The live connection in the browser.
#[cfg(feature = "hydrate")]
mod client {
    use std::time::Duration;

    use leptos::prelude::*;

    use super::{Live, Overview};

    /// Follows the overview stream while the page is shown, and counts the
    /// seconds since the last overview.
    pub fn connect(live: Live) {
        crate::stream::follow(
            "/admin/api/overview-stream",
            move |text| {
                if let Ok(overview) = serde_json::from_str::<Overview>(&text) {
                    live.overview.set(Some(overview));
                    live.since.set(0);
                }
            },
            live.lost,
        );
        let ticker = set_interval_with_handle(
            move || live.since.update(|s| *s += 1),
            Duration::from_secs(1),
        )
        .ok();
        on_cleanup(move || {
            if let Some(t) = ticker {
                t.clear();
            }
        });
    }
}
