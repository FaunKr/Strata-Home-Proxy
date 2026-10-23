//! The history in the *PC* area: the hours the PC spent awake, in gaming mode,
//! asleep or unobserved. The proxy works out the days; this page only draws
//! what it says.

use leptos::prelude::*;

use crate::{
    api::{
        Energy, HistoryBucket, HistoryDay, HistoryTotals, HistoryView, PowerSettings, get_history,
    },
    labels::HISTORY_STATES,
};

/// The ranges the page picks from, as `get_history` takes them.
const RANGES: &[(&str, &str)] = &[
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
    ("12m", "Last 12 months"),
    ("all", "All"),
];

/// Up to this many bars, each has its name below it.
const NAMED_BARS: usize = 14;

#[component]
pub fn HistoryPage() -> impl IntoView {
    let range = RwSignal::new(RANGES[0].0.to_string());
    let history = Resource::new(
        move || range.get(),
        |range| async move { get_history(range).await },
    );
    view! {
        <Transition fallback=|| view! { <p class="hint">"Loading …"</p> }>
            {move || Suspend::new(async move {
                match history.await {
                    Ok(view) => {
                        let caption = caption(&range.get(), &view);
                        // Without a PC service the proxy only sees what it
                        // causes itself: the history has gaps.
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
                                        .map(|(key, name)| preset(range, key, name))
                                        .collect::<Vec<_>>()}
                                    <a
                                        class="button secondary"
                                        href=move || format!("/admin/history.csv?range={}", range.get())
                                        download=""
                                    >
                                        "Download CSV"
                                    </a>
                                </div>
                                <Picker range from=view.from.clone() to=view.to.clone() />
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

/// What stands above the bars: the range and how long a bar is.
fn caption(range: &str, view: &HistoryView) -> String {
    let name = RANGES.iter().find(|(key, _)| *key == range).map_or_else(
        || format!("{} – {}", view.from, view.to),
        |(_, name)| name.to_string(),
    );
    match view.bucket {
        HistoryBucket::Day => name,
        HistoryBucket::Week => format!("{name}, by week"),
        HistoryBucket::Month => format!("{name}, by month"),
    }
}

/// A range the page can fetch.
fn preset(range: RwSignal<String>, key: &'static str, name: &'static str) -> impl IntoView {
    let pressed = move || range.with(|r| r == key);
    view! {
        <button
            type="button"
            class="chip"
            aria-pressed=move || if pressed() { "true" } else { "false" }
            on:click=move |_| range.set(key.to_string())
        >
            {name}
        </button>
    }
}

/// Two days between which the page shows the history, both included.
#[component]
fn Picker(range: RwSignal<String>, from: String, to: String) -> impl IntoView {
    let from = RwSignal::new(from);
    let to = RwSignal::new(to);
    let show = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (a, b) = (from.get(), to.get());
        if !a.is_empty() && !b.is_empty() {
            range.set(format!("{a}..{b}"));
        }
    };
    view! {
        <form class="row-form" aria-label="From – to" on:submit=show>
            <label class="inline" for="history-from">"From"</label>
            <input
                id="history-from"
                type="date"
                prop:value=move || from.get()
                on:input=move |ev| from.set(event_target_value(&ev))
            />
            <label class="inline" for="history-to">"to"</label>
            <input
                id="history-to"
                type="date"
                prop:value=move || to.get()
                on:input=move |ev| to.set(event_target_value(&ev))
            />
            <button type="submit">"Show"</button>
        </form>
    }
}

/// The last 7 days on the Overview, with the way to the whole history.
#[component]
pub fn WeekCard(week: HistoryView) -> impl IntoView {
    let totals = week.totals;
    let seen = totals.awake_h + totals.gaming_h + totals.asleep_h + totals.energy.kwh > 0.0;
    let summary = if seen {
        summary_line(&totals)
    } else {
        "Nothing recorded yet".to_string()
    };
    view! {
        <section class="card week" aria-labelledby="week-heading">
            <div class="card-head">
                <h2 id="week-heading">"Last 7 days"</h2>
                <a href="/admin/pc/history" class="button secondary">"History"</a>
            </div>
            <p class="week-summary">{summary}</p>
            {seen.then(|| view! { <Bars days=week.days.clone() /> })}
        </section>
    }
}

/// The bars and the sum, or an empty history when there is nothing to see
/// yet.
fn shown(view: HistoryView) -> AnyView {
    let totals = view.totals;
    // The page shows measured energy even over unknown time.
    if totals.awake_h + totals.gaming_h + totals.asleep_h + totals.energy.kwh <= 0.0 {
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
        <Totals totals=view.totals power=view.power />
        <PerBar days=view.days />
    }
    .into_any()
}

/// Every bar with its numbers, also for phones without a tooltip.
#[component]
fn PerBar(days: Vec<HistoryDay>) -> impl IntoView {
    view! {
        <details class="per-bar">
            <summary>"Each bar"</summary>
            <div class="table-scroll">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">""</th>
                            <th scope="col">"Awake"</th>
                            <th scope="col">"Energy"</th>
                            <th scope="col">"Cost"</th>
                            <th scope="col">"Saved"</th>
                            <th scope="col">""</th>
                        </tr>
                    </thead>
                    <tbody>
                        {days
                            .into_iter()
                            .rev()
                            .map(|day| {
                                let e = day.energy;
                                view! {
                                    <tr>
                                        <th scope="row">{day.label}</th>
                                        <td>{format!("{} h", hours(day.awake_h + day.gaming_h))}</td>
                                        <td>{kwh(e.kwh)}</td>
                                        <td>{eur(e.eur)}</td>
                                        <td>{eur(e.saved_eur)}</td>
                                        <td class="muted">{e.source.word()}</td>
                                    </tr>
                                }
                            })
                            .collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>
        </details>
    }
}


fn kwh(value: f64) -> String {
    format!("{value:.2} kWh")
}

fn eur(value: f64) -> String {
    format!("€{value:.2}")
}

/// "4.2 h awake · 0.64 kWh · €0.23 saved", as the Overview and the page say it.
fn summary_line(totals: &HistoryTotals) -> String {
    format!(
        "{} h awake · {} · {} saved",
        hours(totals.awake_h + totals.gaming_h),
        kwh(totals.energy.kwh),
        eur(totals.energy.saved_eur),
    )
}

/// One bar per day, week or month, the oldest first. When there are many,
/// only the first and the last name stand below them.
#[component]
fn Bars(days: Vec<HistoryDay>) -> impl IntoView {
    let named = days.len() <= NAMED_BARS;
    let ends = (!named && !days.is_empty()).then(|| {
        let first = days[0].label.clone();
        let last = days[days.len() - 1].label.clone();
        view! {
            <div class="bars-axis mono">
                <span>{first}</span>
                <span>{last}</span>
            </div>
        }
    });
    view! {
        <div
            class="bars"
            class:dense=!named
            role="img"
            aria-label="One bar per day, week or month, filled with the hours the PC spent awake, gaming, asleep and unobserved"
        >
            {days
                .into_iter()
                .map(|day| {
                    view! { <Bar day named /> }
                })
                .collect::<Vec<_>>()}
        </div>
        {ends}
    }
}

#[component]
fn Bar(day: HistoryDay, named: bool) -> impl IntoView {
    // The bar itself is the scale, so a day with a clock change or a cut-off
    // month is filled and none overflows.
    let total = (day.awake_h + day.gaming_h + day.asleep_h + day.unknown_h).max(1.0);
    let share = |hours: f64| format!("{:.3}%", hours / total * 100.0);
    let Energy {
        kwh: used,
        eur: cost,
        saved_eur,
        source: from,
        ..
    } = day.energy;
    let title = format!(
        "{}: {} h awake, {} h gaming, {} h asleep, {} h unknown; {}, {}, {} saved ({})",
        day.label,
        hours(day.awake_h),
        hours(day.gaming_h),
        hours(day.asleep_h),
        hours(day.unknown_h),
        kwh(used),
        eur(cost),
        eur(saved_eur),
        from.word(),
    );
    // "18 Oct" from a day's "Sun 18 Oct": the weekday does not fit under a
    // bar. A week ("12 Oct") and a month ("Oct 2026") stay as they are.
    let label = match day.label.split(' ').collect::<Vec<_>>()[..] {
        [_, date, month] => format!("{date} {month}"),
        _ => day.label.clone(),
    };
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
fn Totals(totals: HistoryTotals, power: PowerSettings) -> impl IntoView {
    let e = totals.energy;
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
            <dt>"Energy"</dt>
            <dd>{format!("{} ({})", kwh(e.kwh), e.source.word())}</dd>
            <dt>"Cost"</dt>
            <dd>{eur(e.eur)}</dd>
            <dt>"Saved"</dt>
            <dd>{format!("{} · {} (estimated)", kwh(e.saved_kwh), eur(e.saved_eur))}</dd>
        </dl>
        <p class="muted small">
            {format!(
                "€ use today's price, {} ct/kWh, for the whole range. Estimated energy and all savings use {} W awake and {} W asleep (System → Energy); measured hours come from the power plug.",
                number(power.price_ct_per_kwh),
                number(power.awake_watts),
                number(power.asleep_watts),
            )}
        </p>
        <p class="muted small">
            "Unknown is time the proxy could not observe, such as before the first event, around a restart of the proxy, and after a wake-up that did not finish. It is never counted as awake or as asleep."
        </p>
    }
}

/// "75" or "2.5": without decimals where there are none.
fn number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn hours(value: f64) -> String {
    format!("{value:.1}")
}
