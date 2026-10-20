//! The Strata output for the web UI: the PC service's `GET /strata-log`,
//! followed only while a page holds the stream and only while the PC is
//! awake anyway. Never wakes the PC and never counts as activity.

use std::sync::Arc;

use futures_util::{StreamExt, stream::BoxStream};
use strata_admin_ui::api::{OutputMessage, OutputNotice};
use strata_pc_protocol::{StrataOutputMessage, StrataOutputNotice};
use tokio::sync::mpsc;

use crate::{clock, state::AppState};

/// A message of the PC service as the page shows it.
fn into_ui(message: StrataOutputMessage) -> OutputMessage {
    match message {
        StrataOutputMessage::Started { at } => OutputMessage::Line {
            text: format!("Strata restarted, {}", clock::local_text(at)),
        },
        StrataOutputMessage::Line { text } => OutputMessage::Line { text },
        StrataOutputMessage::Replace { text } => OutputMessage::Replace { text },
        StrataOutputMessage::Notice { reason } => OutputMessage::Notice {
            notice: match reason {
                StrataOutputNotice::Running => OutputNotice::Running,
                StrataOutputNotice::Gaming => OutputNotice::Gaming,
                StrataOutputNotice::NotRunning => OutputNotice::NotRunning,
                StrataOutputNotice::NotStartedByAgent => OutputNotice::NotStartedByAgent,
            },
        },
    }
}

/// The Strata output for one page, for as long as it holds the stream.
pub fn watch(state: &Arc<AppState>) -> BoxStream<'static, OutputMessage> {
    let (tx, rx) = mpsc::channel(64);
    let state = Arc::clone(state);
    tokio::spawn(async move {
        tokio::select! {
            () = follow(&state, &tx) => {}
            () = tx.closed() => {}
        }
    });
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) })
        .boxed()
}

/// Sends a notice unless it is the one shown already.
async fn notice(
    tx: &mpsc::Sender<OutputMessage>,
    shown: &mut Option<OutputNotice>,
    n: OutputNotice,
) {
    if *shown != Some(n) {
        *shown = Some(n);
        let _ = tx.send(OutputMessage::Notice { notice: n }).await;
    }
}

async fn follow(state: &AppState, tx: &mpsc::Sender<OutputMessage>) {
    let mut shown = None;
    loop {
        let agent = state.agent();
        if !agent.configured() {
            notice(tx, &mut shown, OutputNotice::NoAgent).await;
        } else if state.ask_pc(&agent).await.is_none() {
            notice(tx, &mut shown, OutputNotice::Asleep).await;
        } else {
            match agent.strata_log().await {
                Some(Ok(resp)) => {
                    // The PC sends its kept lines and its own notice anew.
                    shown = None;
                    if tx.send(OutputMessage::Reset).await.is_err() {
                        return;
                    }
                    if !forward(resp, tx).await {
                        return;
                    }
                }
                Some(Err(reqwest::StatusCode::NOT_FOUND)) => {
                    notice(tx, &mut shown, OutputNotice::TooOld).await;
                }
                Some(Err(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN)) => {
                    notice(tx, &mut shown, OutputNotice::Refused).await;
                }
                // Went to sleep between the two calls, most likely.
                _ => notice(tx, &mut shown, OutputNotice::Asleep).await,
            }
        }
        tokio::time::sleep(state.cfg().health_poll).await;
    }
}

/// Hands the PC's messages on until its stream ends; false once the page is gone.
async fn forward(resp: reqwest::Response, tx: &mpsc::Sender<OutputMessage>) -> bool {
    let mut body = resp.bytes_stream();
    // Bytes, not text: a chunk may end inside a character.
    let mut buf = Vec::new();
    while let Some(Ok(chunk)) = body.next().await {
        buf.extend_from_slice(&chunk);
        while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = buf.drain(..end + 2).collect();
            let event = String::from_utf8_lossy(&event);
            let Some(data) = event.lines().find_map(|l| l.strip_prefix("data:")) else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<StrataOutputMessage>(data.trim()) else {
                continue;
            };
            if tx.send(into_ui(message)).await.is_err() {
                return false;
            }
        }
    }
    true
}
