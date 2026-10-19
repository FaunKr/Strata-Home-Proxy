//! Der Verlauf im Bereich *PC*: die Stunden, die der PC wach, im Gaming-Modus,
//! schlafend oder unbeobachtet verbracht hat. Der Proxy rechnet die Tage aus;
//! diese Seite zeichnet nur, was er sagt.

use leptos::prelude::*;

use crate::{
    api::{HistoryDay, HistoryTotals, HistoryView, get_history},
    labels::HISTORY_STATES,
};

/// Die Bereiche, aus denen die Seite wählt.
const RANGES: &[(u32, &str)] = &[(7, "Last 7 days"), (30, "Last 30 days")];

#[component]
pub fn HistoryPage() -> impl IntoView {
    let range = RwSignal::new(RANGES[0].0);
    let history = Resource::new(
        move || range.get(),
        |days| async move { get_history(days).await },
    );
    view! {
        <Transition fallback=|| view! { <p class="hint">"Loading …"</p> }>
            {move || Suspend::new(async move {
                match history.await {
                    Ok(view) => {
                        let caption = RANGES
                            .iter()
                            .find(|(days, _)| *days == range.get())
                            .map_or(RANGES[0].1, |(_, name)| *name);
                        // Ohne PC-Dienst sieht der Proxy nur, was er selbst
                        // auslöst: der Verlauf bleibt lückenhaft.
                        let without_agent = (!view.agent_configured).then(|| {
                            view! {
                                <p class="warning" role="status">
                                    "Without a PC service on your PC, the proxy only records what it causes itself. A complete history needs it."
                                </p>
                            }
                        });
                        view! {
                            {without_agent}
                            <section class="card" aria-label="Awake and asleep">
                                <div class="card-head">
                                    <h2>"Awake and asleep"</h2>
                                    <span class="muted small">{caption}</span>
                                </div>
                                <div class="chips" role="group" aria-label="Range">
                                    {RANGES
                                        .iter()
                                        .map(|(days, name)| preset(range, *days, name))
                                        .collect::<Vec<_>>()}
                                </div>
                                {shown(view)}
                            </section>
                        }
                            .into_any()
                    }
                    Err(_) => {
                        view! { <p class="error" role="alert">"The history could not be read."</p> }
                            .into_any()
                    }
                }
            })}
        </Transition>
    }
}

/// Ein Bereich, den die Seite sich holen kann.
fn preset(range: RwSignal<u32>, days: u32, name: &'static str) -> impl IntoView {
    let pressed = move || days == range.get();
    view! {
        <button
            type="button"
            class="chip"
            aria-pressed=move || if pressed() { "true" } else { "false" }
            on:click=move |_| range.set(days)
        >
            {name}
        </button>
    }
}

/// Die Balken und die Summe, oder ein leerer Verlauf, wenn noch nichts zu
/// sehen ist.
fn shown(view: HistoryView) -> AnyView {
    if view.totals.awake_h + view.totals.gaming_h + view.totals.asleep_h <= 0.0 {
        return view! {
            <p class="muted">
                "Nothing to show yet: the proxy has not seen your PC in this range. Every hour counts as unknown until it does."
            </p>
            <p class="hint">
                "The history fills itself as the proxy runs. Wake the PC or let it sleep, and the first hours appear here."
            </p>
        }
        .into_any();
    }
    view! {
        <Bars days=view.days.clone() />
        <ul class="legend">
            {HISTORY_STATES
                .iter()
                .map(|(name, class)| {
                    view! {
                        <li>
                            <span class=format!("swatch {class}")></span>
                            {*name}
                        </li>
                    }
                })
                .collect::<Vec<_>>()}
        </ul>
        <Totals totals=view.totals />
    }
    .into_any()
}

/// Ein Balken pro Tag, der älteste zuerst.
#[component]
fn Bars(days: Vec<HistoryDay>) -> impl IntoView {
    let named = days.len() <= 10;
    view! {
        <div
            class="bars"
            role="img"
            aria-label="One bar per day, filled with the hours the PC spent awake, gaming, asleep and unobserved"
        >
            {days
                .into_iter()
                .map(|day| {
                    view! { <Bar day named /> }
                })
                .collect::<Vec<_>>()}
        </div>
    }
}

#[component]
fn Bar(day: HistoryDay, named: bool) -> impl IntoView {
    // Der Tag selbst ist der Maßstab, damit ein Tag mit Zeitumstellung
    // gefüllt ist und keiner zu viel wird.
    let total = (day.awake_h + day.gaming_h + day.asleep_h + day.unknown_h).max(1.0);
    let share = |hours: f64| format!("{:.3}%", hours / total * 100.0);
    let title = format!(
        "{}: {} h awake, {} h gaming, {} h asleep, {} h unknown",
        day.label,
        hours(day.awake_h),
        hours(day.gaming_h),
        hours(day.asleep_h),
        hours(day.unknown_h),
    );
    // "18 Oct" from "Sun 18 Oct": the weekday does not fit under a bar.
    let label = day
        .label
        .split_once(' ')
        .map_or(day.label.clone(), |(_, rest)| rest.to_string());
    view! {
        <div class="bar">
            <div class="bar-column" title=title>
                <div class="seg seg-awake" style:height=share(day.awake_h)></div>
                <div class="seg seg-gaming" style:height=share(day.gaming_h)></div>
                <div class="seg seg-asleep" style:height=share(day.asleep_h)></div>
                <div class="seg seg-unknown" style:height=share(day.unknown_h)></div>
            </div>
            {named
                .then(|| view! { <span class="day-label mono">{label}</span> })}
        </div>
    }
}

#[component]
fn Totals(totals: HistoryTotals) -> impl IntoView {
    view! {
        <dl class="facts">
            <dt>"Awake"</dt>
            <dd>{format!("{} h", hours(totals.awake_h))}</dd>
            <dt>"Gaming"</dt>
            <dd>{format!("{} h", hours(totals.gaming_h))}</dd>
            <dt>"Asleep"</dt>
            <dd>{format!("{} h", hours(totals.asleep_h))}</dd>
            <dt>"Unknown"</dt>
            <dd>{format!("{} h", hours(totals.unknown_h))}</dd>
        </dl>
        <p class="muted small">
            "Unknown is time the proxy could not observe, such as before the first event, around a restart of the proxy, and after a wake-up that did not finish. It is never counted as awake or as asleep."
        </p>
    }
}

fn hours(value: f64) -> String {
    format!("{value:.1}")
}
