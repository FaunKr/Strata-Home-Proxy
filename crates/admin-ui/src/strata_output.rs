//! The tab "Output" of the Strata area: what Strata writes to its console on
//! the PC, live from the proxy. It follows the newest line until one
//! scrolls up.

use leptos::{html::Pre, prelude::*};

#[cfg(feature = "hydrate")]
use crate::api::OutputMessage;
use crate::api::OutputNotice;

/// Lines the tab keeps, like `KEPT` of the PC service's Strata-Ausgabe.
#[cfg(feature = "hydrate")]
const KEPT: usize = 500;

#[component]
pub fn OutputTab() -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let notice = RwSignal::new(None::<OutputNotice>);
    let lost = RwSignal::new(false);
    let pre = NodeRef::<Pre>::new();
    // Scrolled to the end: new lines keep it there.
    let at_end = RwSignal::new(true);
    #[cfg(feature = "hydrate")]
    {
        crate::stream::follow(
            "/admin/api/strata-output-stream",
            move |text| {
                if let Ok(message) = serde_json::from_str::<OutputMessage>(&text) {
                    apply(message, lines, notice);
                }
            },
            lost,
        );
        Effect::new(move |_| {
            lines.track();
            // After the browser drew the new text, so the end is the new end.
            request_animation_frame(move || {
                if at_end.get_untracked()
                    && let Some(pre) = pre.get_untracked()
                {
                    pre.set_scroll_top(pre.scroll_height());
                }
            });
        });
    }
    let on_scroll = move |_| {
        if let Some(pre) = pre.get() {
            at_end.set(pre.scroll_top() + pre.client_height() >= pre.scroll_height() - 4);
        }
    };
    view! {
        <section class="card">
            <div class="card-head">
                <h2>"Console output"</h2>
                {move || {
                    let waiting = notice.with(|n| n.is_some_and(|n| n != OutputNotice::Running));
                    let (text, class) = if lost.get() {
                        ("Reconnecting", "badge is-warn")
                    } else if waiting {
                        ("Waiting", "badge is-idle")
                    } else if at_end.get() {
                        ("Live", "badge is-ok")
                    } else {
                        ("Paused", "badge is-idle")
                    };
                    view! {
                        <span class=class>
                            <span class="dot"></span>
                            {text}
                        </span>
                    }
                }}
            </div>
            <p class="muted small">"What Strata prints on the PC, newest at the bottom."</p>
            {move || {
                notice
                    .get()
                    .and_then(notice_text)
                    .map(|text| view! { <p class="warning" role="status">{text}</p> })
            }}
            <pre class="lines output" node_ref=pre on:scroll=on_scroll tabindex="0">
                {move || lines.with(|l| l.join("\n"))}
            </pre>
            {move || {
                (!at_end.get())
                    .then(|| {
                        view! {
                            <p class="hint">"Paused. Scroll to the bottom to follow again."</p>
                        }
                    })
            }}
        </section>
    }
}

#[cfg(feature = "hydrate")]
fn apply(
    message: OutputMessage,
    lines: RwSignal<Vec<String>>,
    notice: RwSignal<Option<OutputNotice>>,
) {
    match message {
        OutputMessage::Reset => {
            lines.update(Vec::clear);
            notice.set(None);
        }
        OutputMessage::Line { text } => lines.update(|l| {
            if l.len() == KEPT {
                l.remove(0);
            }
            l.push(text);
        }),
        OutputMessage::Replace { text } => lines.update(|l| match l.last_mut() {
            Some(last) => *last = text,
            None => l.push(text),
        }),
        OutputMessage::Notice { notice: n } => notice.set(Some(n)),
    }
}

fn notice_text(notice: OutputNotice) -> Option<&'static str> {
    Some(match notice {
        OutputNotice::Running => return None,
        OutputNotice::Asleep => {
            "The PC is asleep. The output appears once it is awake; this tab does not wake it."
        }
        OutputNotice::NoAgent => "No PC service is set up, and the output comes through it.",
        OutputNotice::TooOld => {
            "The PC service does not know the output yet. Please update the PC service."
        }
        OutputNotice::Refused => {
            "The PC service refuses the proxy. Check PC_AGENT_TOKEN and allowed_clients."
        }
        OutputNotice::Gaming => "Gaming mode: Strata is stopped.",
        OutputNotice::NotRunning => "Strata is not running.",
        OutputNotice::NotStartedByAgent => {
            "Strata was not started by the PC service, so there is no output. It appears here after the next start by the PC service."
        }
    })
}
