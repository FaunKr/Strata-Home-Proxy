//! Hardwarewerte on the overview: what the PC service reports about GPU and
//! CPU, against the simulated PC.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};

fn cpu_only() -> Value {
    json!({
        "gpu": null,
        "cpu": {"load_pct": 12.5, "ram_used_bytes": 21_400_000_000u64, "ram_total_bytes": 34_359_738_368u64}
    })
}

#[tokio::test]
async fn shows_the_values_of_an_awake_pc() {
    let s = setup(true, |_| {}).await;
    *s.pc.hardware.lock().unwrap() = Some(cpu_only());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("Hardwarewerte", |v| v["hardware"]["Values"].is_object())
        .await;
    assert_eq!(v["hardware"]["Values"], cpu_only());
}

#[tokio::test]
async fn a_sleeping_pc_has_none() {
    let s = setup(false, |_| {}).await;
    *s.pc.hardware.lock().unwrap() = Some(cpu_only());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC schläft", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["hardware"], "Unavailable");
}

#[tokio::test]
async fn values_go_once_the_pc_sleeps() {
    let s = setup(true, |_| {}).await;
    *s.pc.hardware.lock().unwrap() = Some(cpu_only());
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Hardwarewerte", |v| v["hardware"]["Values"].is_object())
        .await;
    assert_eq!(b.call("sleep-now", &[]).await.status(), 200);
    let v = live.until("PC schläft", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["hardware"], "Unavailable");
}

#[tokio::test]
async fn gaming_mode_keeps_the_values() {
    let s = setup(true, |_| {}).await;
    *s.pc.hardware.lock().unwrap() = Some(cpu_only());
    s.pc.gaming.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("Gaming", |v| v["pc"] == "Gaming").await;
    assert_eq!(v["hardware"]["Values"], cpu_only());
}

#[tokio::test]
async fn unreadable_values_stay_empty() {
    let s = setup(true, |_| {}).await;
    let partly = json!({
        "gpu": null,
        "cpu": {"load_pct": null, "ram_used_bytes": 21_400_000_000u64, "ram_total_bytes": null}
    });
    *s.pc.hardware.lock().unwrap() = Some(partly.clone());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("Hardwarewerte", |v| v["hardware"]["Values"].is_object())
        .await;
    assert_eq!(v["hardware"]["Values"], partly);
}

#[tokio::test]
async fn an_older_pc_service_is_unsupported_until_it_reports() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC wach", |v| v["pc"] == "Awake").await;
    assert_eq!(v["hardware"], "Unsupported");
    *s.pc.hardware.lock().unwrap() = Some(cpu_only());
    let v = live
        .until("Hardwarewerte", |v| v["hardware"]["Values"].is_object())
        .await;
    assert_eq!(v["hardware"]["Values"], cpu_only());
}
