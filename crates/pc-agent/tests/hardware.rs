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
    *s.fake.hardware.lock().unwrap() = Hardware {
        gpu: None,
        cpu: Cpu {
            load_pct: None,
            ram_used_bytes: Some(21_400_000_000),
            ram_total_bytes: Some(34_359_738_368),
        },
    };
    let status = status_json(&s).await;
    assert_eq!(
        status["hardware"],
        json!({
            "gpu": null,
            "cpu": {"load_pct": null, "ram_used_bytes": 21_400_000_000u64, "ram_total_bytes": 34_359_738_368u64}
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

#[tokio::test]
async fn status_reports_temperature_fan_and_power() {
    let s = setup("hardware-perf").await;
    *s.fake.hardware.lock().unwrap() = Hardware {
        gpu: Some(Gpu {
            temp_c: Some(61.5),
            fan_rpm: Some(0),
            power_pct: Some(41.0),
            ..Gpu::default()
        }),
        cpu: Cpu::default(),
    };
    let gpu = &status_json(&s).await["hardware"]["gpu"];
    assert_eq!(gpu["temp_c"], 61.5);
    // A stopped fan is 0, not unreadable.
    assert_eq!(gpu["fan_rpm"], 0);
    assert_eq!(gpu["power_pct"], 41.0);
    assert_eq!(gpu["load_pct"], Value::Null);
}

/// The real reading, in the CI job `windows`: gets through D3DKMT and PDH
/// without a crash and reads CPU and RAM. The runner has no GPU, so its
/// values may stay empty.
#[cfg(windows)]
#[test]
fn windows_reads_cpu_and_ram() {
    use strata_pc_agent::hardware::read;
    // The first call starts the sampler; CPU load needs a second sample 2 s later.
    read();
    std::thread::sleep(std::time::Duration::from_millis(4500));
    let cpu = read().cpu;
    let load = cpu.load_pct.expect("CPU-Last");
    assert!((0.0..=100.0).contains(&load), "CPU-Last {load}");
    let total = cpu.ram_total_bytes.expect("RAM gesamt");
    let used = cpu.ram_used_bytes.expect("RAM belegt");
    assert!(0 < used && used <= total, "RAM {used} von {total}");
}
