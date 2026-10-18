//! Hardwarewerte in `/status`, read through the fake platform.

mod common;

use common::*;
use serde_json::{Value, json};

async fn status_json(s: &Setup) -> Value {
    client()
        .get(format!("{}/status", s.url))
        .bearer_auth("geheim")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn status_reports_cpu_and_ram() {
    let s = setup("hardware").await;
    *s.fake.hardware.lock().unwrap() = Hardware {
        gpu: None,
        cpu: Cpu {
            load_pct: Some(12.5),
            ram_used_bytes: Some(21_400_000_000),
            ram_total_bytes: Some(34_359_738_368),
        },
    };
    let status = status_json(&s).await;
    assert_eq!(
        status["hardware"],
        json!({
            "gpu": null,
            "cpu": {"load_pct": 12.5, "ram_used_bytes": 21_400_000_000u64, "ram_total_bytes": 34_359_738_368u64}
        })
    );
}

#[tokio::test]
async fn unreadable_values_are_null() {
    let s = setup("hardware-null").await;
    let status = status_json(&s).await;
    assert_eq!(
        status["hardware"],
        json!({
            "gpu": null,
            "cpu": {"load_pct": null, "ram_used_bytes": null, "ram_total_bytes": null}
        })
    );
}
