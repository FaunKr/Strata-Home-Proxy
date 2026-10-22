//! The power of the power plug recorded at fixed times (`PLUG_SAMPLE_SECS`),
//! against the simulated PC and its plug. The rig samples every 100 ms.

use crate::common;

use std::{
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::*;
use serde_json::json;
use strata_nas_proxy::events::Sample;

async fn samples(s: &Setup) -> Vec<Sample> {
    s.state.events.samples(0, u64::MAX).await
}

/// Waits up to 5 s for a sample that matches; panics naming `what` otherwise.
async fn sample_until(s: &Setup, what: &str, check: impl Fn(&Sample) -> bool) -> Sample {
    for _ in 0..100 {
        if let Some(found) = samples(s).await.into_iter().find(|x| check(x)) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no sample with {what}, last: {:?}", samples(s).await);
}

/// Long enough for many rounds of the sample loop.
async fn several_rounds() {
    tokio::time::sleep(Duration::from_millis(1000)).await;
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn samples_the_power_while_a_plug_is_set() {
    let s = setup(true, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 87.4;
    let before = now();
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let first = sample_until(&s, "87.4 W", |x| x.watts == 87.4).await;
    assert!((before..=now()).contains(&first.ts), "time {}", first.ts);
    *s.pc.plug_watts.lock().unwrap() = 312.0;
    sample_until(&s, "312 W", |x| x.watts == 312.0).await;
    // A sample is no event.
    assert!(own_kinds(&s.state).is_empty(), "{:?}", kinds(&s.state));
}

#[tokio::test]
async fn samples_a_gen1_plug_too() {
    let s = setup(false, |_| {}).await;
    s.pc.plug_gen1.store(true, Ordering::SeqCst);
    *s.pc.plug_watts.lock().unwrap() = 1.2;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    sample_until(&s, "1.2 W", |x| x.watts == 1.2).await;
}

#[tokio::test]
async fn no_samples_without_a_plug() {
    let s = setup(true, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 87.0;
    several_rounds().await;
    assert!(samples(&s).await.is_empty());
    // A plug set up later is sampled without a restart.
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    sample_until(&s, "87 W", |x| x.watts == 87.0).await;
    // And no longer once it is removed again.
    s.state.settings.set("SHELLY_URL", "").unwrap();
    // A read in progress may still land.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let count = samples(&s).await.len();
    several_rounds().await;
    assert_eq!(samples(&s).await.len(), count);
}

#[tokio::test]
async fn a_failing_plug_leaves_no_samples_and_no_events() {
    let s = setup(true, |_| {}).await;
    s.pc.plug_down.store(true, Ordering::SeqCst);
    *s.pc.plug_watts.lock().unwrap() = 87.0;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let events = own_kinds(&s.state);
    several_rounds().await;
    assert!(samples(&s).await.is_empty());
    assert_eq!(own_kinds(&s.state), events);
    s.pc.plug_down.store(false, Ordering::SeqCst);
    sample_until(&s, "87 W", |x| x.watts == 87.0).await;
}

#[tokio::test]
async fn a_plug_that_is_not_there_leaves_no_samples() {
    // Nothing listens on port 9 of localhost.
    let s = setup(true, |_| {}).await;
    s.state.settings.set("SHELLY_URL", "127.0.0.1:9").unwrap();
    several_rounds().await;
    assert!(samples(&s).await.is_empty());
}

#[tokio::test]
async fn a_hanging_plug_leaves_no_samples_and_holds_back_nothing() {
    // Each round waits for the plug's probe to run out.
    let s = setup(true, |c| c.probe_timeout = Duration::from_millis(500)).await;
    s.pc.plug_hangs.store(true, Ordering::SeqCst);
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    // Requests go through while the plug hangs.
    assert_eq!(post_json(&s, "/v1/messages", json!({})).await.status(), 200);
    // The overview still follows the PC; the plug would hang forever.
    s.pc.gaming.store(true, Ordering::SeqCst);
    live.until("Gaming", |v| v["pc"] == "Gaming").await;
    assert!(samples(&s).await.is_empty());
    // The loop goes on once the plug answers again.
    *s.pc.plug_watts.lock().unwrap() = 55.0;
    s.pc.plug_hangs.store(false, Ordering::SeqCst);
    sample_until(&s, "55 W", |x| x.watts == 55.0).await;
}
