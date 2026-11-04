//! The tab "Transport check" of the Strata area: the request bodies that
//! broke on their way to Strata, newest first, with where they broke, and
//! the switch that saves them.

use leptos::prelude::*;

use crate::api::{BrokenBodyRow, BrokenSide, DeleteBrokenBodies, RelayCount, get_transport_check};
use crate::labels::STRATA_TRANSPORT;
use crate::settings::SettingsSections;

#[component]
pub fn TransportTab() -> impl IntoView {
    let delete = ServerAction::<DeleteBrokenBodies>::new();
    let page = Resource::new(move || delete.version().get(), |_| get_transport_check());
    view! {
        <SettingsSections sections=&[&STRATA_TRANSPORT] />
        <section class="card">
            <div class="card-head">
                <h2>"Broken request bodies"</h2>
            </div>
            <p class="muted small">
                "Each JSON request body that broke on its way to Strata, newest first, the last 100. "
                "Arrival: broken when it reached the proxy, which refused it. Network: the PC service got other bytes than the proxy sent. Strata: it went on whole, and Strata refused it as invalid JSON."
            </p>
            <Transition fallback=|| view! { <p class="muted">"Loading…"</p> }>
                {move || Suspend::new(async move {
                    match page.await {
                        Ok(view) => {
                            let any_saved = view.rows.iter().any(|r| r.saved);
                            let button = any_saved
                                .then(|| {
                                    view! {
                                        <button
                                            class="btn"
                                            on:click=move |_| {
                                                delete.dispatch(DeleteBrokenBodies {});
                                            }
                                        >
                                            "Delete saved bodies"
                                        </button>
                                    }
                                });
                            if view.rows.is_empty() {
                                view! { <p class="muted">"No broken request bodies."</p> }
                                    .into_any()
                            } else {
                                view! {
                                    {button}
                                    <div class="broken-rows">
                                        {view.rows.into_iter().map(|row| view! { <BrokenRow row /> }).collect_view()}
                                    </div>
                                }
                                    .into_any()
                            }
                        }
                        Err(e) => view! { <p class="error">{e.to_string()}</p> }.into_any(),
                    }
                })}
            </Transition>
        </section>
    }
}

#[component]
fn BrokenRow(row: BrokenBodyRow) -> impl IntoView {
    let download = row.saved.then(|| {
        view! {
            <a class="btn" href=format!("/admin/broken-body/{}", row.id) download="">
                "Download body"
            </a>
        }
    });
    let sent = row.sent_bytes.map(|bytes| {
        view! {
            <dt>"Sent by the proxy"</dt>
            <dd>{sent_text(bytes, row.sent_chars)}</dd>
        }
    });
    let relay = row.relay.map(|relay| {
        view! {
            <dt>"PC service"</dt>
            <dd>{relay_text(relay)}</dd>
        }
    });
    view! {
        <article class="broken-row">
            <h3>
                <span class=side_class(row.side)>{side_text(row.side)}</span>
                " "
                {row.time.clone()}
            </h3>
            <dl class="facts">
                <dt>"Key"</dt>
                <dd>{row.key.clone()}</dd>
                <dt>"Path"</dt>
                <dd>{row.path.clone()}</dd>
                <dt>"Content-Length"</dt>
                <dd>{announced_text(row.announced)}</dd>
                <dt>"Received"</dt>
                <dd>{format!("{} bytes", row.received)}</dd>
                <dt>"Valid on arrival"</dt>
                <dd>{if row.side == BrokenSide::Arrival { "no" } else { "yes" }}</dd>
                {sent}
                {relay}
                <dt>"Breaks at"</dt>
                <dd>{break_text(&row)}</dd>
                <dt>"Message"</dt>
                <dd>{row.message.clone()}</dd>
            </dl>
            {download}
        </article>
    }
}

/// The side's name on its badge.
pub fn side_text(side: BrokenSide) -> &'static str {
    match side {
        BrokenSide::Arrival => "Arrival",
        BrokenSide::Network => "Network",
        BrokenSide::Strata => "Strata",
    }
}

fn side_class(side: BrokenSide) -> &'static str {
    match side {
        BrokenSide::Strata => "badge is-warn",
        _ => "badge is-bad",
    }
}

pub fn announced_text(announced: Option<u64>) -> String {
    announced.map_or_else(|| "none".to_string(), |n| format!("{n} bytes"))
}

pub fn sent_text(bytes: u64, chars: Option<u64>) -> String {
    match chars {
        Some(chars) => format!("{bytes} bytes, {chars} characters"),
        None => format!("{bytes} bytes"),
    }
}

/// Where the JSON breaks: a byte of what arrived, or the character of
/// Strata's message, out of what the proxy sent.
pub fn break_text(row: &BrokenBodyRow) -> String {
    match row.side {
        BrokenSide::Arrival => format!("byte {} of {}", row.break_at, row.received),
        _ => match row.sent_chars {
            Some(chars) => format!("character {} of {chars} sent", row.break_at),
            None => format!("character {}", row.break_at),
        },
    }
}

/// What the PC service's relay counted.
pub fn relay_text(relay: RelayCount) -> String {
    match relay {
        RelayCount::NotMeasured => "not measured (PC service older than 1.5.0)".to_string(),
        RelayCount::Measured {
            received,
            passed,
            checksum_matches,
        } => {
            let checksum = match checksum_matches {
                Some(true) => ", checksum matches",
                Some(false) => ", checksum differs",
                None => "",
            };
            format!("received {received} bytes, passed on {passed}{checksum}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_pc_service_reads_not_measured() {
        assert_eq!(
            relay_text(RelayCount::NotMeasured),
            "not measured (PC service older than 1.5.0)"
        );
    }

    #[test]
    fn a_differing_checksum_is_named() {
        let text = relay_text(RelayCount::Measured {
            received: 10,
            passed: 10,
            checksum_matches: Some(false),
        });
        assert!(text.contains("checksum differs"), "{text}");
    }

    #[test]
    fn each_side_has_its_name() {
        let names: Vec<&str> = BrokenSide::ALL.iter().map(|s| side_text(*s)).collect();
        assert_eq!(names, ["Arrival", "Network", "Strata"]);
    }

    #[test]
    fn a_body_without_content_length_reads_none() {
        assert_eq!(announced_text(None), "none");
    }
}
