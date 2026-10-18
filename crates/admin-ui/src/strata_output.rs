//! The page "Strata-Ausgabe": what Strata writes to its console on the PC,
//! live from the proxy. It follows the newest line until one scrolls up.

use leptos::{html::Pre, prelude::*};

#[cfg(feature = "hydrate")]
use crate::api::OutputMessage;
use crate::api::OutputNotice;

/// Lines the page keeps, like `KEPT` of the PC service's Strata-Ausgabe.
#[cfg(feature = "hydrate")]
const KEPT: usize = 500;

#[component]
pub fn StrataOutputPage() -> impl IntoView {
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
        <h1>"Strata-Ausgabe"</h1>
        <p><a href="/admin/">"Zurück zur Übersicht"</a></p>
        {move || lost.get().then(|| view! {
            <p class="warning" role="status">"Verbindung unterbrochen, verbinde neu …"</p>
        })}
        {move || notice.get().and_then(notice_text).map(|text| view! {
            <p class="warning" role="status">{text}</p>
        })}
        <pre class="lines output" node_ref=pre on:scroll=on_scroll>
            {move || lines.with(|l| l.join("\n"))}
        </pre>
        {move || (!at_end.get()).then(|| view! {
            <p class="hint">"Angehalten. Ganz nach unten scrollen, um wieder mitzulaufen."</p>
        })}
    }
}

#[cfg(feature = "hydrate")]
fn apply(
    message: OutputMessage,
    lines: RwSignal<Vec<String>>,
    notice: RwSignal<Option<OutputNotice>>,
) {
    match message {
        OutputMessage::Reset => lines.update(Vec::clear),
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
            "Der PC schläft. Die Ausgabe erscheint, sobald er wach ist; diese Seite weckt ihn nicht."
        }
        OutputNotice::NoAgent => "Kein PC-Dienst eingerichtet, die Ausgabe kommt über ihn.",
        OutputNotice::TooOld => {
            "Der PC-Dienst kennt die Strata-Ausgabe nicht. Bitte den PC-Dienst aktualisieren."
        }
        OutputNotice::Gaming => "Gaming-Modus: Strata ist beendet.",
        OutputNotice::NotRunning => "Strata läuft nicht.",
        OutputNotice::NotStartedByAgent => {
            "Strata wurde nicht vom PC-Dienst gestartet, deshalb gibt es keine Ausgabe. Nach dem nächsten Start durch den PC-Dienst erscheint sie hier."
        }
    })
}
