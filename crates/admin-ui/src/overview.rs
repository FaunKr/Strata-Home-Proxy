//! The overview: the PC's state with its main button, gaming mode and keep
//! awake, tiles for Strata, requests, sleep and the PC service, the latest
//! events, and a checklist while the proxy is not fully set up.

use leptos::prelude::*;

use crate::{
    api::{
        ForceSleep, KeepAwake, KeepAwakeChoice, Overview, PcServiceView, PcView, PowerView,
        SetGaming, Setup, SleepAnswer, SleepNow, SleepView, StrataView, Wake, error_text,
        get_setup,
    },
    confirm::{Confirm, Question},
    hardware::HardwareCard,
    history::WeekCard,
    icons::Icon,
    labels::{event_class, event_label, pc_badge, strata_badge, strata_job_label},
};

/// The buttons' actions; made once per page, so a result stays visible
/// while updates come in.
#[derive(Clone, Copy)]
struct Actions {
    wake: ServerAction<Wake>,
    sleep: ServerAction<SleepNow>,
    force_sleep: ServerAction<ForceSleep>,
    gaming: ServerAction<SetGaming>,
    keep: ServerAction<KeepAwake>,
    /// The keep-awake choice pressed last, to mark it while it lasts.
    picked: RwSignal<Option<KeepAwakeChoice>>,
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

    /// The proxy's clock now (Unix seconds), counted on from the latest
    /// overview; `None` until one arrived.
    pub fn now(&self) -> Option<u64> {
        let since = self.since.get();
        self.overview.with(|o| o.as_ref().map(|o| o.now + since))
    }
}

#[component]
pub fn OverviewPage() -> impl IntoView {
    let live = expect_context::<Live>();
    let actions = Actions {
        wake: ServerAction::new(),
        sleep: ServerAction::new(),
        force_sleep: ServerAction::new(),
        gaming: ServerAction::new(),
        keep: ServerAction::new(),
        picked: RwSignal::new(None),
    };
    view! {
        <div class="page-head">
            <h1>"Overview"</h1>
        </div>
        <Checklist live />
        {move || match live.overview.get() {
            None if live.lost.get() => {
                view! {
                    <p class="warning" role="status">
                        "Connection to the proxy lost, reconnecting …"
                    </p>
                }
                    .into_any()
            }
            None => view! { <p class="hint">"Loading …"</p> }.into_any(),
            Some(o) => view! { <Body overview=o live actions /> }.into_any(),
        }}
    }
}

/// "Finish setup", until everything is done. Before the live stream
/// arrives it shows what the server knew when the page was drawn.
#[component]
fn Checklist(live: Live) -> impl IntoView {
    let first = OnceResource::new(get_setup());
    view! {
        <Transition>
            {move || Suspend::new(async move {
                let first = first.await.ok();
                move || {
                    live.overview
                        .with(|o| o.as_ref().map(|o| o.setup))
                        .or(first)
                        .filter(|s| !s.complete())
                        .map(|setup| view! { <SetupCard setup /> })
                }
            })}
        </Transition>
    }
}

#[component]
fn SetupCard(setup: Setup) -> impl IntoView {
    let items = [
        (
            "Strata address",
            setup.configured,
            "Set address",
            "/admin/strata/connection",
        ),
        (
            "MAC address of the PC",
            setup.mac_known,
            "Set MAC",
            "/admin/pc",
        ),
        ("PC service", setup.agent_configured, "Install", "/admin/pc"),
        (
            "API key for your clients",
            setup.api_key_exists,
            "Create key",
            "/admin/access",
        ),
        (
            "Keep changes across restarts (DATA_DIR)",
            setup.persistent,
            "How to",
            "/admin/system",
        ),
    ];
    let done = items.iter().filter(|i| i.1).count();
    let total = items.len();
    view! {
        <section class="card checklist" aria-labelledby="setup-heading">
            <div class="card-head">
                <h2 id="setup-heading">"Finish setup"</h2>
                <span class="muted small">{format!("{done} of {total} done")}</span>
            </div>
            <progress value=done max=total aria-hidden="true"></progress>
            <ul>
                {items
                    .into_iter()
                    .map(|(label, done, action, href)| {
                        view! {
                            <li class=if done { "check done" } else { "check todo" }>
                                <span class="tick">
                                    {done.then(|| view! { <Icon icon=Icon::Check size=14 /> })}
                                </span>
                                <span class="label">
                                    {label}
                                    <span class="visually-hidden">
                                        {if done { " (done)" } else { " (open)" }}
                                    </span>
                                </span>
                                {(!done).then(|| view! { <a href=href>{action}</a> })}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </section>
    }
}

#[component]
fn Body(overview: Overview, live: Live, actions: Actions) -> impl IntoView {
    let lost = move || {
        live.lost.get().then(|| {
            view! {
                <p class="warning" role="status">
                    "Connection lost, the display may be out of date."
                </p>
            }
        })
    };
    let job = overview.strata_job.clone().map(|j| {
        let percent = j.percent.map(|p| format!(", {p} %")).unwrap_or_default();
        view! {
            <p class="warning" role="status">
                {format!("Strata job running: {}{percent}. ", strata_job_label(j.kind))}
                <a href="/admin/strata">"View"</a>
            </p>
        }
    });
    let events = overview.events.clone();
    let hardware = (overview.setup.agent_configured || overview.power != PowerView::NotConfigured)
        .then(|| {
            let hardware = overview
                .setup
                .agent_configured
                .then(|| overview.hardware.clone());
            view! { <HardwareCard hardware pc=overview.pc power=overview.power.clone() /> }
        });
    view! {
        {lost}
        {job}
        <Hero overview=overview.clone() actions />
        <div class="quick">
            <GamingCard overview=overview.clone() actions />
            <KeepAwakeCard overview=overview.clone() actions />
        </div>
        <Tiles overview=overview.clone() live />
        <WeekCard week=overview.week.clone() />
        {hardware}
        <section class="card" aria-labelledby="events-heading">
            <h2 id="events-heading">"Recent events"</h2>
            {if events.is_empty() {
                view! { <p class="hint">"None yet."</p> }.into_any()
            } else {
                view! {
                    <ul class="events">
                        {events
                            .into_iter()
                            .map(|e| {
                                view! {
                                    <li>
                                        <span class="time">{e.time}</span>
                                        <span class=format!("ev-dot {}", event_class(e.kind))></span>
                                        <span>
                                            {event_label(e.kind)}
                                            {e
                                                .detail
                                                .map(|d| {
                                                    view! { <span class="hint">{format!(" · {d}")}</span> }
                                                })}
                                        </span>
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

/// The PC's state, large, with the button that fits it.
#[component]
fn Hero(overview: Overview, actions: Actions) -> impl IntoView {
    let Actions {
        wake,
        sleep,
        force_sleep,
        ..
    } = actions;
    let confirm = expect_context::<Confirm>();
    let pc = overview.pc;
    let (_, class) = pc_badge(pc);
    let (title, sub) = match pc {
        PcView::Asleep => (
            "Asleep".to_string(),
            "The next request wakes it automatically.".to_string(),
        ),
        PcView::Waking => (
            "Waking up …".to_string(),
            "Wake packet sent, waiting for Strata to answer.".to_string(),
        ),
        PcView::Awake => ("Awake".to_string(), strata_sentence(&overview.strata)),
        PcView::Gaming => (
            "Awake, gaming".to_string(),
            "Strata is paused while gaming mode is on.".to_string(),
        ),
        PcView::Unknown => (
            "Unknown".to_string(),
            "The proxy has not reached the PC yet.".to_string(),
        ),
    };
    let setup = overview.setup;
    let can_wake = setup.mac_known && setup.configured;
    let button = match pc {
        PcView::Asleep | PcView::Unknown => view! {
            <button
                type="button"
                class="hero-btn"
                disabled=move || !can_wake || wake.pending().get()
                on:click=move |_| {
                    wake.dispatch(Wake {});
                }
            >
                <Icon icon=Icon::Power size=18 />
                "Wake PC"
            </button>
        }
        .into_any(),
        PcView::Waking => view! {
            <button type="button" class="hero-btn" disabled>
                "Waking …"
            </button>
        }
        .into_any(),
        PcView::Awake => {
            let agent = setup.agent_configured;
            view! {
                <button
                    type="button"
                    class="secondary hero-btn"
                    disabled=move || !agent || sleep.pending().get()
                    on:click=move |_| {
                        sleep.dispatch(SleepNow {});
                    }
                >
                    <Icon icon=Icon::Moon size=18 />
                    "Sleep now"
                </button>
            }
            .into_any()
        }
        PcView::Gaming => {
            let agent = setup.agent_configured;
            view! {
                <button
                    type="button"
                    class="secondary hero-btn"
                    disabled=move || !agent || force_sleep.pending().get()
                    on:click=move |_| ask_force_sleep(confirm, force_sleep, "Gaming mode is on")
                >
                    <Icon icon=Icon::Moon size=18 />
                    "Force sleep"
                </button>
            }
            .into_any()
        }
    };
    let icon = if pc == PcView::Asleep {
        Icon::Moon
    } else {
        Icon::Monitor
    };
    view! {
        <section class="card hero" aria-label="PC">
            <div class="hero-main">
                <div class=format!("hero-icon {class}")>
                    <Icon icon size=26 />
                </div>
                <div>
                    <p class="eyebrow">"Your PC"</p>
                    <p class="hero-state">{title}</p>
                    <p class="muted">{sub}</p>
                </div>
            </div>
            <div>{button}</div>
            {result_text(wake.value())}
            <SleepResult
                sleep
                force_sleep
                offer=pc == PcView::Awake && setup.agent_configured
            />
            <ForceSleepResult force_sleep asleep=pc == PcView::Asleep />
        </section>
    }
}

/// What Sleep now answered; under a refusal, Force sleep while `offer`
/// holds (the PC is awake, and the gaming state has its own button).
#[component]
fn SleepResult(
    sleep: ServerAction<SleepNow>,
    force_sleep: ServerAction<ForceSleep>,
    offer: bool,
) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    move || {
        let reason = match sleep.value().get()? {
            Ok(SleepAnswer::Accepted) => return None,
            Ok(SleepAnswer::Refused { reason }) => reason,
            Err(e) => {
                return Some(
                    view! { <p class="error" role="alert">{error_text(&e)}</p> }.into_any(),
                );
            }
        };
        let text = format!("Refused: {reason}");
        let button = offer.then(|| {
            view! {
                <button
                    type="button"
                    class="secondary"
                    disabled=move || force_sleep.pending().get()
                    on:click=move |_| ask_force_sleep(confirm, force_sleep, &reason)
                >
                    "Force sleep"
                </button>
            }
        });
        Some(
            view! {
                <div class="refusal">
                    <p class="error" role="alert">{text}</p>
                    {button}
                </div>
            }
            .into_any(),
        )
    }
}

/// What Force sleep answered: its error, or, once the PC counts as
/// asleep, that it was forced to.
#[component]
fn ForceSleepResult(force_sleep: ServerAction<ForceSleep>, asleep: bool) -> impl IntoView {
    move || match force_sleep.value().get()? {
        Ok(()) => asleep.then(|| {
            view! {
                <p class="hint" role="status">
                    "Forced to sleep; the next request wakes the PC."
                </p>
            }
            .into_any()
        }),
        Err(e) => Some(view! { <p class="error" role="alert">{error_text(&e)}</p> }.into_any()),
    }
}

/// Opens the confirm dialog for a forced sleep that overrides `reason`.
fn ask_force_sleep(confirm: Confirm, force_sleep: ServerAction<ForceSleep>, reason: &str) {
    confirm.ask(
        Question {
            title: "Force the PC to sleep?".into(),
            text: format!(
                "{reason}. The PC sleeps anyway: open requests fail, and a running Strata job is cancelled (downloaded parts are kept)."
            ),
            confirm: "Force sleep",
            danger: true,
        },
        move || {
            force_sleep.dispatch(ForceSleep {});
        },
    );
}

#[component]
fn GamingCard(overview: Overview, actions: Actions) -> impl IntoView {
    let gaming = actions.gaming;
    let busy = matches!(overview.strata, StrataView::Busy { .. });
    let agent = overview.setup.agent_configured;
    let on = overview.pc == PcView::Gaming;
    let confirm = expect_context::<Confirm>();
    view! {
        <section class="card">
            <div class="toggle-row">
                <button
                    type="button"
                    class="switch"
                    role="switch"
                    aria-checked=if on { "true" } else { "false" }
                    aria-labelledby="gaming-heading"
                    disabled=move || !agent || gaming.pending().get()
                    on:click=move |_| {
                        let enabled = !on;
                        let switch = move || {
                            gaming.dispatch(SetGaming { enabled });
                        };
                        if enabled && busy {
                            confirm
                                .ask(
                                    Question {
                                        title: "Switch on gaming mode?".into(),
                                        text: "Strata is working on a request right now. Gaming mode stops Strata, so the request fails."
                                            .into(),
                                        confirm: "Switch on",
                                        danger: false,
                                    },
                                    switch,
                                );
                        } else {
                            switch();
                        }
                    }
                >
                    <span class="knob"></span>
                </button>
                <div>
                    <h2 id="gaming-heading">"Gaming mode"</h2>
                    <p class="muted small">
                        {if agent {
                            "Pauses Strata so games get the GPU. Requests get 503 until you switch it off."
                        } else {
                            "Needs the PC service."
                        }}
                    </p>
                    {move || {
                        gaming
                            .pending()
                            .get()
                            .then(|| {
                                view! {
                                    <p class="hint" role="status">
                                        {if on { "Switching off …" } else { "Switching on …" }}
                                    </p>
                                }
                            })
                    }}
                </div>
            </div>
            {result_text(gaming.value())}
        </section>
    }
}

#[component]
fn KeepAwakeCard(overview: Overview, actions: Actions) -> impl IntoView {
    let Actions { keep, picked, .. } = actions;
    let kept = overview.keep_awake_until.clone();
    let active = kept.is_some();
    let chip = move |choice: KeepAwakeChoice, text: &'static str| {
        let pressed = move || active && picked.get() == Some(choice);
        view! {
            <button
                type="button"
                class="chip"
                aria-pressed=move || if pressed() { "true" } else { "false" }
                disabled=move || keep.pending().get()
                on:click=move |_| {
                    picked.set(Some(choice));
                    keep.dispatch(KeepAwake { choice });
                }
            >
                {text}
            </button>
        }
    };
    let text = match &kept {
        Some(until) => format!("Awake until {until}, even across a restart of the proxy."),
        None => format!(
            "Off. The PC sleeps after {} without requests.",
            idle_text(overview.idle_timeout_secs)
        ),
    };
    view! {
        <section class="card">
            <h2 id="keep-heading">"Keep awake"</h2>
            <div class="chips" role="group" aria-labelledby="keep-heading">
                {chip(KeepAwakeChoice::OneHour, "1 h")}
                {chip(KeepAwakeChoice::FourHours, "4 h")}
                {chip(KeepAwakeChoice::EightHours, "8 h")}
                {chip(KeepAwakeChoice::Morning, "Until 8:00")}
                {active.then(|| chip(KeepAwakeChoice::Off, "Stop"))}
            </div>
            <p class="muted small">{text}</p>
            {result_text(keep.value())}
        </section>
    }
}

#[component]
fn Tiles(overview: Overview, live: Live) -> impl IntoView {
    let (strata_value, _) = strata_badge(&overview);
    let strata_sub = strata_sentence(&overview.strata);
    let (last_value, last_sub) = match overview.last_request.clone() {
        Some(t) => (t, "Last API request"),
        None => ("None yet".to_string(), "Since the proxy started"),
    };
    let service = overview.pc_service.clone();
    view! {
        <div class="tiles">
            <Tile label="Strata" value=strata_value.to_string() sub=strata_sub>
                <a class="small" href="/admin/strata/output">
                    "Output"
                </a>
            </Tile>
            <Tile label="Last request" value=last_value sub=last_sub.to_string() />
            <section class="card tile">
                <h2>"Sleep"</h2>
                <p class="tile-value">
                    <SleepText sleep=overview.sleep.clone() now=overview.now since=live.since />
                </p>
            </section>
            <ServiceTile service agent=overview.setup.agent_configured />
        </div>
    }
}

#[component]
fn Tile(
    label: &'static str,
    value: String,
    sub: String,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <section class="card tile">
            <h2>{label}</h2>
            <p class="tile-value">{value}</p>
            <p class="muted small">{sub}</p>
            {children.map(|c| view! { <div>{c()}</div> })}
        </section>
    }
}

#[component]
fn ServiceTile(service: Option<PcServiceView>, agent: bool) -> impl IntoView {
    let (value, sub, update) = match service {
        Some(s) => {
            let value = s
                .version
                .as_ref()
                .map_or_else(|| "Old version".to_string(), |v| format!("Version {v}"));
            let sub = if s.updating {
                "Updating …".to_string()
            } else if s.update_available {
                s.available.as_ref().map_or_else(
                    || "An update is available".to_string(),
                    |v| format!("Version {v} is available"),
                )
            } else {
                "Up to date".to_string()
            };
            (value, sub, s.update_available && !s.updating)
        }
        None if agent => (
            "Not reached yet".to_string(),
            "Reports once the PC is awake".to_string(),
            false,
        ),
        None => (
            "Not installed".to_string(),
            "No gaming mode, no automatic sleep".to_string(),
            false,
        ),
    };
    view! {
        <Tile label="PC service" value sub>
            {update.then(|| view! { <a class="small" href="/admin/pc">"Update on PC page"</a> })}
        </Tile>
    }
}

/// What Strata does, as a sentence.
pub(crate) fn strata_sentence(strata: &StrataView) -> String {
    match strata {
        StrataView::Offline => "Strata is not running.".into(),
        StrataView::Loading => "Strata is loading the model.".into(),
        StrataView::Unloaded => "Strata is ready and loads the model with the next request.".into(),
        StrataView::Ready => "Strata is ready.".into(),
        StrataView::Busy {
            processing,
            deferred,
            tokens_per_sec,
        } => {
            let mut text = match processing {
                1 => "Strata is working on 1 request".to_string(),
                n => format!("Strata is working on {n} requests"),
            };
            if let Some(waiting) = deferred.filter(|n| *n > 0) {
                text.push_str(&format!(", {waiting} waiting"));
            }
            if let Some(tps) = tokens_per_sec {
                text.push_str(&format!(", {tps:.0} tokens/s"));
            }
            text.push('.');
            text
        }
    }
}

/// `15 min`, `2 h` or `1 h 30 min`.
fn idle_text(secs: u64) -> String {
    let mins = secs.div_ceil(60);
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
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
            l if l > 0 => format!("In {}", duration_text(l)),
            _ => "Now".to_string(),
        },
        SleepView::Asleep => "Asleep".into(),
        SleepView::Waking => "Waking up".into(),
        SleepView::Gaming => "Not while gaming".into(),
        SleepView::KeptAwake { until } => format!("Kept awake until {until}"),
        SleepView::Busy => "Request running".into(),
        SleepView::Refused { reason, retry_at } => {
            format!(
                "Refused: {reason}, next try in {}",
                duration_text(left(*retry_at))
            )
        }
        SleepView::NoAgent => "Never (no PC service)".into(),
        SleepView::Unknown => "PC state unknown".into(),
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
