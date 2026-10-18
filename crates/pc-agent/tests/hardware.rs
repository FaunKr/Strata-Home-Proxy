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

#[tokio::test]
async fn status_reports_the_gpu() {
    let s = setup("hardware-gpu").await;
    *s.fake.hardware.lock().unwrap() = Hardware {
        gpu: Some(Gpu {
            load_pct: Some(37.0),
            vram_used_bytes: Some(12_670_000_000),
            vram_total_bytes: Some(17_163_091_968),
            ..Gpu::default()
        }),
        cpu: Cpu::default(),
    };
    let status = status_json(&s).await;
    assert_eq!(
        status["hardware"]["gpu"],
        json!({
            "load_pct": 37.0, "temp_c": null, "vram_used_bytes": 12_670_000_000u64,
            "vram_total_bytes": 17_163_091_968u64, "fan_rpm": null, "power_pct": null
        })
    );
}
