//! The relay's view of a request body on its way to Strata: the bytes it got
//! from the proxy, the bytes it handed on, and their SHA-256, so the proxy's
//! Transport check can tell a body the network cut from one Strata refused
//! (`docs/pc-agent-api.md`, *`/relay/…`*).

use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderValue},
};
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use strata_pc_protocol::{RELAY_BODY_PASSED, RELAY_BODY_RECEIVED, RELAY_BODY_SHA256_MATCH};

#[derive(Default)]
struct Tally {
    received: u64,
    passed: u64,
    hash: Sha256,
    /// The body was read to its end: all of its Content-Length, or the
    /// stream's end.
    ended: bool,
}

/// What the relay counts of one body while it streams.
#[derive(Clone, Default)]
pub struct RelayBody {
    tally: Arc<Mutex<Tally>>,
}

impl RelayBody {
    /// `body` as it goes on to Strata, counted on the way. `announced` is
    /// the proxy's Content-Length: a body that ends short of it is logged.
    pub fn count<S, E>(
        &self,
        body: S,
        announced: Option<u64>,
    ) -> impl Stream<Item = Result<Bytes, E>> + Send + 'static
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let tally = Arc::clone(&self.tally);
        let done = Arc::clone(&self.tally);
        body.map(move |chunk| {
            let mut t = tally.lock().unwrap();
            match &chunk {
                Ok(bytes) => {
                    t.received += bytes.len() as u64;
                    t.hash.update(bytes);
                    t.passed += bytes.len() as u64;
                    // The client stops reading once it has sent the
                    // Content-Length, so the stream's own end may never come.
                    if announced.is_some_and(|n| t.received >= n) {
                        t.ended = true;
                    }
                }
                Err(e) => tracing::warn!(
                    "Relay: the proxy's request body ended after {} of {} bytes: {e}",
                    t.received,
                    announced.map_or_else(|| "unknown".to_string(), |n| n.to_string()),
                ),
            }
            Some(chunk)
        })
        .chain(futures_util::stream::once(async move {
            done.lock().unwrap().ended = true;
            None
        }))
        .filter_map(futures_util::future::ready)
    }

    /// The headers for Strata's answer: the counts, and whether the body has
    /// `expected` (the proxy's SHA-256 in hex) when it sent one. None while
    /// the body has not ended.
    pub fn headers(&self, expected: Option<&str>) -> HeaderMap {
        let mut out = HeaderMap::new();
        let t = self.tally.lock().unwrap();
        if !t.ended {
            return out;
        }
        out.insert(RELAY_BODY_RECEIVED, HeaderValue::from(t.received));
        out.insert(RELAY_BODY_PASSED, HeaderValue::from(t.passed));
        if let Some(expected) = expected {
            let got = hex(&t.hash.clone().finalize());
            let answer = if got.eq_ignore_ascii_case(expected.trim()) {
                "yes"
            } else {
                "no"
            };
            out.insert(RELAY_BODY_SHA256_MATCH, HeaderValue::from_static(answer));
        }
        out
    }
}

/// Lower-case hex, as the proxy writes its SHA-256.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
