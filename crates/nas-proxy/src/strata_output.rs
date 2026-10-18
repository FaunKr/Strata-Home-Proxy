//! The Strata-Ausgabe for the web UI: the PC service's `GET /strata-log`,
//! followed only while a page holds the stream and only while the PC is
//! awake anyway. Never wakes the PC and never counts as activity.

use std::sync::Arc;

use futures_util::{StreamExt, stream::BoxStream};
use serde::Deserialize;
use strata_admin_ui::api::{OutputMessage, OutputNotice};
use tokio::sync::mpsc;

use crate::{clock, state::AppState};

/// A message of the PC service (see `docs/pc-agent-api.md`).
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PcMessage {
    Started { at: u64 },
    Line { text: String },
    Replace { text: String },
    Notice { reason: PcNotice },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PcNotice {
    Running,
    Gaming,
    NotRunning,
    NotStartedByAgent,
}

impl PcMessage {
    fn into_ui(self) -> OutputMessage {
        match self {
            Self::Started { at } => OutputMessage::Line {
                text: format!("Strata neu gestartet, {}", clock::local_text(at)),
            },
            Self::Line { text } => OutputMessage::Line { text },
            Self::Replace { text } => OutputMessage::Replace { text },
            Self::Notice { reason } => OutputMessage::Notice {
                notice: match reason {
                    PcNotice::Running => OutputNotice::Running,
                    PcNotice::Gaming => OutputNotice::Gaming,
                    PcNotice::NotRunning => OutputNotice::NotRunning,
                    PcNotice::NotStartedByAgent => OutputNotice::NotStartedByAgent,
                },
            },
        }
    }
}

/// The Strata-Ausgabe for one page, for as long as it holds the stream.
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
        } else if state.pc_status(&agent).await.is_none() {
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
                Some(Err(status)) if status == reqwest::StatusCode::NOT_FOUND => {
                    notice(tx, &mut shown, OutputNotice::TooOld).await;
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
            let Ok(message) = serde_json::from_str::<PcMessage>(data.trim()) else {
                continue;
            };
            if tx.send(message.into_ui()).await.is_err() {
                return false;
            }
        }
    }
    true
}
