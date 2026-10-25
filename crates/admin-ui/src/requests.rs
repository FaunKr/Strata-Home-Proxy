//! The tab "Requests" of the Strata area: the request log, live from the
//! proxy, newest first. On a narrow screen each row shows the essentials
//! and opens on tap.

use leptos::prelude::*;

#[cfg(feature = "hydrate")]
use crate::api::RequestLogMessage;
use crate::api::{RequestAnswer, RequestApi, RequestOutcome, RequestRow, RequestStart};

/// Finished rows the tab keeps, like the proxy's `LIMIT`.
#[cfg(feature = "hydrate")]
const KEPT: usize = 1000;

#[component]
pub fn RequestsTab() -> impl IntoView {
    let rows = RwSignal::new(Vec::<RequestRow>::new());
    let loaded = RwSignal::new(false);
    let lost = RwSignal::new(false);
    // The rows opened on a narrow screen, by id.
    let open = RwSignal::new(Vec::<u64>::new());
    #[cfg(feature = "hydrate")]
    crate::stream::follow(
        "/admin/api/requests-stream",
        move |text| {
            if let Ok(message) = serde_json::from_str::<RequestLogMessage>(&text) {
                rows.update(|r| apply(r, message));
                loaded.set(true);
            }
        },
        lost,
    );
    view! {
        <section class="card">
            <div class="card-head">
                <h2>"Request log"</h2>
                {move || {
                    let (text, class) = if lost.get() {
                        ("Reconnecting", "badge is-warn")
                    } else {
                        ("Live", "badge is-ok")
                    };
                    view! {
                        <span class=class>
                            <span class="dot"></span>
                            {text}
                        </span>
                    }
                }}
            </div>
            <p class="muted small">
                "Each request through the proxy that made the model work, newest first. Only numbers, never the text."
            </p>
            {move || {
                if !loaded.get() {
                    view! { <p class="muted">"Loading…"</p> }.into_any()
                } else if rows.with(Vec::is_empty) {
                    view! { <p class="muted">"No requests yet."</p> }.into_any()
                } else {
                    view! { <RequestTable rows open /> }.into_any()
                }
            }}
        </section>
    }
}

#[component]
fn RequestTable(rows: RwSignal<Vec<RequestRow>>, open: RwSignal<Vec<u64>>) -> impl IntoView {
    view! {
        <div class="request-table">
            <table>
                <thead>
                    <tr>
                        <th scope="col">"Time"</th>
                        <th scope="col">"Key"</th>
                        <th scope="col" class="wide">"Model"</th>
                        <th scope="col" class="wide">"API"</th>
                        <th scope="col">"Type"</th>
                        <th scope="col" class="wide num">"Prompt"</th>
                        <th scope="col" class="num">"Output"</th>
                        <th scope="col" class="wide num">"Prefill tok/s"</th>
                        <th scope="col" class="num">"Gen. tok/s"</th>
                        <th scope="col" class="wide num">"First token"</th>
                        <th scope="col" class="wide num">"Duration"</th>
                        <th scope="col" class="wide num">"Wait"</th>
                        <th scope="col" class="wide">"Status"</th>
                    </tr>
                </thead>
                <For
                    each=move || rows.get()
                    key=|row| (row.id, row.duration_ms, row.outcome)
                    children=move |row| view! { <RequestLine row open /> }
                />
            </table>
        </div>
    }
}

/// One request: its row, and the details a narrow screen shows on tap.
#[component]
fn RequestLine(row: RequestRow, open: RwSignal<Vec<u64>>) -> impl IntoView {
    let id = row.id;
    let is_open = move || open.with(|o| o.contains(&id));
    let toggle = move |_| {
        open.update(|o| match o.iter().position(|&x| x == id) {
            Some(i) => {
                o.remove(i);
            }
            None => o.push(id),
        })
    };
    let model = row.model.clone().unwrap_or_else(|| "–".into());
    let details = [
        ("Model", model.clone()),
        ("API", api_label(row.api).to_string()),
        ("Prompt", prompt_text(&row)),
        ("Prefill", speed(row.prefill_tok_s())),
        ("First token", millis(row.first_token_ms)),
        ("Duration", millis(Some(row.duration_ms))),
        ("Wait", millis(Some(row.wait_ms))),
        ("Status", outcome_text(row.outcome)),
    ];
    view! {
        <tbody class="request" class:open=is_open>
            <tr on:click=toggle>
                <td>{row.time.clone()}</td>
                <td>{row.key.clone()}</td>
                <td class="wide model">{model}</td>
                <td class="wide">{api_label(row.api)}</td>
                <td class="type">{type_label(&row)}</td>
                <td class="wide num">
                    {count(row.prompt_tokens)}
                    {row
                        .cached_tokens
                        .filter(|&c| c > 0)
                        .map(|c| view! { <span class="sub">{format!("{c} cached")}</span> })}
                </td>
                <td class="num">{count(row.output_tokens)}</td>
                <td class="wide num">{rate(row.prefill_tok_s())}</td>
                <td class="num">{rate(row.generation_tok_s())}</td>
                <td class="wide num">{millis(row.first_token_ms)}</td>
                <td class="wide num">{millis(Some(row.duration_ms))}</td>
                <td class="wide num">{millis(Some(row.wait_ms))}</td>
                <td class="wide" class:bad=row.outcome != RequestOutcome::Done>
                    {outcome_text(row.outcome)}
                </td>
            </tr>
            <tr class="request-more">
                <td colspan="13">
                    <dl>
                        {details
                            .into_iter()
                            .map(|(term, value)| view! {
                                <dt>{term}</dt>
                                <dd>{value}</dd>
                            })
                            .collect::<Vec<_>>()}
                    </dl>
                </td>
            </tr>
        </tbody>
    }
}

/// Takes a message of the stream into the rows, newest first.
#[cfg(feature = "hydrate")]
fn apply(rows: &mut Vec<RequestRow>, message: RequestLogMessage) {
    match message {
        RequestLogMessage::All { rows: all } => *rows = all,
        RequestLogMessage::Row { row } => {
            match rows.iter().position(|r| r.id == row.id) {
                Some(i) => rows[i] = *row,
                None => {
                    let at = rows
                        .iter()
                        .position(|r| r.id < row.id)
                        .unwrap_or(rows.len());
                    rows.insert(at, *row);
                }
            }
            rows.truncate(KEPT);
        }
    }
}

fn api_label(api: RequestApi) -> &'static str {
    match api {
        RequestApi::Chat => "chat",
    }
}

/// "prompt → tool call", or the part that is known.
pub fn type_label(row: &RequestRow) -> String {
    let start = row.start.map(|s| match s {
        RequestStart::Prompt => "prompt",
        RequestStart::ToolResult => "tool result",
        RequestStart::Continue => "continue",
    });
    let answer = row.answer.map(|a| match a {
        RequestAnswer::Text => "text",
        RequestAnswer::ToolCall => "tool call",
    });
    match (start, answer) {
        (Some(s), Some(a)) => format!("{s} → {a}"),
        (Some(x), None) | (None, Some(x)) => x.to_string(),
        (None, None) => "–".into(),
    }
}

fn prompt_text(row: &RequestRow) -> String {
    match (row.prompt_tokens, row.cached_tokens) {
        (Some(p), Some(c)) if c > 0 => format!("{p} ({c} cached)"),
        (p, _) => count(p),
    }
}

fn count(n: Option<u64>) -> String {
    n.map_or_else(|| "–".into(), |n| n.to_string())
}

/// A speed in a column whose head names the unit.
fn rate(tok_s: Option<f64>) -> String {
    tok_s.map_or_else(|| "–".into(), |v| format!("{v:.1}"))
}

fn speed(tok_s: Option<f64>) -> String {
    tok_s.map_or_else(|| "–".into(), |v| format!("{v:.1} tok/s"))
}

fn millis(ms: Option<u64>) -> String {
    match ms {
        None => "–".into(),
        Some(ms) if ms < 1000 => format!("{ms} ms"),
        Some(ms) if ms < 60_000 => format!("{:.1} s", ms as f64 / 1000.0),
        Some(ms) => format!("{} min {} s", ms / 60_000, ms % 60_000 / 1000),
    }
}

fn outcome_text(outcome: RequestOutcome) -> String {
    match outcome {
        RequestOutcome::Done => "done".into(),
        RequestOutcome::CutOff => "cut off at max tokens".into(),
        RequestOutcome::StrataError { status } => format!("Strata error {status}"),
        RequestOutcome::NoAnswer => "no answer from Strata".into(),
        RequestOutcome::Lost => "lost".into(),
        RequestOutcome::CancelledByClient => "cancelled by client".into(),
    }
}
