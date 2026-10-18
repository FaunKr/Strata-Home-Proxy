//! The overview page: state of PC and Strata, live from the proxy, and the
//! buttons to wake, sleep, switch gaming mode and keep the PC awake.

use leptos::prelude::*;

use crate::{
    api::{
        KeepAwake, KeepAwakeChoice, Overview, PcView, SetGaming, SleepNow, SleepView, StrataView,
        Wake, error_text,
    },
    labels::{NO_MAC, event_label},
};

/// The buttons' actions; made once per page, so a result stays visible
/// while updates come in.
#[derive(Clone, Copy)]
struct Actions {
    wake: ServerAction<Wake>,
    sleep: ServerAction<SleepNow>,
    gaming: ServerAction<SetGaming>,
    keep: ServerAction<KeepAwake>,
}

/// The latest overview, and seconds counted since it arrived.
#[derive(Clone, Copy)]
struct Live {
    overview: RwSignal<Option<Overview>>,
    since: RwSignal<u64>,
    lost: RwSignal<bool>,
}

#[component]
pub fn OverviewPage() -> impl IntoView {
    let live = Live {
        overview: RwSignal::new(None),
        since: RwSignal::new(0),
        lost: RwSignal::new(false),
    };
    let actions = Actions {
        wake: ServerAction::new(),
        sleep: ServerAction::new(),
        gaming: ServerAction::new(),
        keep: ServerAction::new(),
    };
    #[cfg(feature = "hydrate")]
    client::connect(live);
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
        <section class="panel cards">
            <div>
                <h2>"PC"</h2>
                <p class="state">{pc_text(overview.pc)}</p>
            </div>
            <div>
                <h2>"Strata"</h2>
                <p class="state">{strata_text(&overview.strata)}</p>
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
    use std::{cell::RefCell, rc::Rc, time::Duration};

    use leptos::{
        prelude::*,
        task::spawn_local,
        wasm_bindgen::{JsCast, closure::Closure},
    };
    use send_wrapper::SendWrapper;
    use web_sys::{EventSource, MessageEvent};

    use super::{Live, Overview};
    use crate::api::{Session, get_session};

    const STREAM: &str = "/admin/api/overview-stream";
    const RETRY: Duration = Duration::from_secs(5);

    /// The open stream with its handlers, dropped together.
    struct Open {
        source: EventSource,
        _on_message: Closure<dyn FnMut(MessageEvent)>,
        _on_error: Closure<dyn FnMut()>,
    }

    enum Connection {
        /// Shown, with the stream if one is open.
        Shown(Option<Open>),
        /// The page is gone: no new connection, and no signals to set.
        Gone,
    }

    /// Browser-only state, shared with the handlers.
    type Shared = Rc<RefCell<Connection>>;

    /// Follows the overview stream while the page is shown. The browser
    /// reconnects by itself after a dropped connection; after a refused one
    /// it tries again every few seconds, or shows the login when the session
    /// has ended.
    pub fn connect(live: Live) {
        let shared: Shared = Rc::new(RefCell::new(Connection::Shown(None)));
        open(live, Rc::clone(&shared));
        let ticker = set_interval_with_handle(
            move || live.since.update(|s| *s += 1),
            Duration::from_secs(1),
        )
        .ok();
        let shared = SendWrapper::new(shared);
        on_cleanup(move || {
            let gone = std::mem::replace(&mut *shared.borrow_mut(), Connection::Gone);
            if let Connection::Shown(Some(o)) = gone {
                o.source.close();
            }
            if let Some(t) = ticker {
                t.clear();
            }
        });
    }

    fn open(live: Live, shared: Shared) {
        let mut slot = shared.borrow_mut();
        let Connection::Shown(current) = &mut *slot else {
            return;
        };
        if let Some(old) = current.take() {
            old.source.close();
        }
        let Ok(source) = EventSource::new(STREAM) else {
            return;
        };
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
            let Some(text) = ev.data().as_string() else {
                return;
            };
            if let Ok(overview) = serde_json::from_str::<Overview>(&text) {
                live.overview.set(Some(overview));
                live.since.set(0);
                live.lost.set(false);
            }
        });
        source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        let watched = source.clone();
        let again = Rc::clone(&shared);
        let on_error = Closure::<dyn FnMut()>::new(move || {
            if matches!(*again.borrow(), Connection::Gone) {
                return;
            }
            live.lost.set(true);
            if watched.ready_state() == EventSource::CLOSED {
                let again = Rc::clone(&again);
                spawn_local(async move {
                    match get_session().await {
                        Ok(Session::Anonymous | Session::MustChange) => {
                            let _ = window().location().reload();
                        }
                        _ => set_timeout(move || open(live, again), RETRY),
                    }
                });
            }
        });
        source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        *current = Some(Open {
            source,
            _on_message: on_message,
            _on_error: on_error,
        });
    }
}
