//! Hardware readings in `/status` and the machine facts in `/strata`, read
//! through the fake platform.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::machine::{CpuFacts, GpuFacts, MachineFacts, RamStick, WindowsFacts};

async fn status_json(s: &Setup) -> Value {
    client()
        .get(format!("{}/status", s.url))
        .bearer_auth("secret")
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

/// The real reading, in the manual workflow `windows.yml`: gets through D3DKMT and PDH
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
    let total = cpu.ram_total_bytes.expect("RAM total");
    let used = cpu.ram_used_bytes.expect("RAM belegt");
    assert!(0 < used && used <= total, "RAM {used} of {total}");
}

#[tokio::test]
async fn strata_reports_the_machine_facts_read_once() {
    let s = setup("machine-facts").await;
    *s.fake.machine.lock().unwrap() = MachineFacts {
        ram_sticks: vec![RamStick {
            size_bytes: Some(32 << 30),
            kind: Some("DDR5".into()),
            rated_mts: Some(6000),
            configured_mts: Some(4800),
        }],
        ram_slots: Some(4),
        gpus: vec![GpuFacts {
            name: Some("NVIDIA GeForce RTX 4090".into()),
            vram_bytes: Some(24 << 30),
            driver_version: Some("32.0.15.6094".into()),
            pcie_max_gen: Some(4),
            pcie_max_lanes: Some(16),
            pcie_gen: Some(1),
            pcie_lanes: Some(16),
        }],
        cpu: CpuFacts {
            name: Some("AMD Ryzen 9 7950X 16-Core Processor".into()),
            cores: Some(16),
            threads: Some(32),
            base_mhz: Some(4501),
            avx2: Some(true),
            avx512: Some(true),
        },
        windows: WindowsFacts {
            edition: Some("Windows 11 Pro".into()),
            version: Some("24H2".into()),
            build: Some("26100.4652".into()),
        },
        model_drive: Some("nvme".into()),
    };
    let first = get(&s, "/strata").await;
    get(&s, "/strata").await;
    let machine = &first["machine"];
    assert_eq!(machine["ram_slots"], 4);
    assert_eq!(machine["ram_sticks"][0]["kind"], "DDR5");
    assert_eq!(machine["gpus"][0]["name"], "NVIDIA GeForce RTX 4090");
    assert_eq!(machine["gpus"][0]["pcie_max_gen"], 4);
    assert_eq!(machine["cpu"]["threads"], 32);
    assert_eq!(machine["windows"]["build"], "26100.4652");
    assert_eq!(machine["model_drive"], "nvme");
    assert_eq!(s.fake.machine_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn facts_without_a_graphics_card_are_not_read_on_every_request() {
    let s = setup("machine-facts-incomplete").await;
    get(&s, "/strata").await;
    get(&s, "/strata").await;
    assert_eq!(s.fake.machine_reads.load(Ordering::SeqCst), 1);
}
