//! The PC service reports its MAC; the proxy learns it and wakes with it.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;
use strata_nas_proxy::{events::EventKind, settings::Source};

const REPORTED: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

/// Waits until the idle loop has asked the PC service and learned the MAC.
async fn learned(s: &Setup) {
    for _ in 0..100 {
        if s.state.cfg().pc_mac == Some(REPORTED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("MAC nicht gelernt: {:?}", s.state.cfg().pc_mac);
}

#[tokio::test]
async fn reported_mac_is_learned_and_beats_pc_mac() {
    let s = setup(true, |_| {}).await;
    *s.pc.report_mac.lock().unwrap() = Some("AA-BB-CC-DD-EE-FF".into());
    learned(&s).await;
    // A few more status rounds with the same MAC.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let learned = kinds(&s.state)
        .into_iter()
        .filter(|k| *k == EventKind::MacLearned)
        .count();
    assert_eq!(learned, 1, "the same MAC again is not news");
    let mac = s
        .state
        .settings
        .entries()
        .into_iter()
        .find(|e| e.field.key == "PC_MAC")
        .unwrap();
    assert_eq!(mac.source, Source::Reported);

    // Asleep: the next request wakes it with the reported MAC, not PC_MAC.
    *s.pc.wake_mac.lock().unwrap() = Some(REPORTED);
    s.pc.awake.store(false, Ordering::SeqCst);
    *s.pc.report_mac.lock().unwrap() = None;
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(*s.pc.last_mac.lock().unwrap(), Some(REPORTED));
    // A status without MAC keeps the learned one.
    assert_eq!(s.state.cfg().pc_mac, Some(REPORTED));
}

#[tokio::test]
async fn proxy_without_pc_mac_learns_it_from_the_pc() {
    let s = setup(true, |c| c.pc_mac = None).await;
    *s.pc.report_mac.lock().unwrap() = Some("AA:BB:CC:DD:EE:FF".into());
    learned(&s).await;
}
