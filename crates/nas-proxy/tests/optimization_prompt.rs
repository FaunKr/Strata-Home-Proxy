//! The optimization prompt: an English text per installed model for an AI
//! chat, built by the proxy from what the PC service reported.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;
use strata_pc_protocol::{
    CpuFacts, GpuFacts, Incoming, LoadLines, MachineFacts, NeverRule, RamStick, SamplingField,
    SamplingSource, SamplingSupport, StrataOptions, StrataSampling, TunerOption, TunerTry,
    WindowsFacts,
};

async fn prompt(b: &mut Browser, model: &str) -> String {
    let resp = b
        .call("strata-optimization-prompt", &[("model", model)])
        .await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn carries_the_model_and_the_task() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for part in [
        "https://github.com/Niko1221/Strata",
        "477ac13",
        "RAM: 64.0 GB",
        "IQ3_XXS",
        "81.0 GB",
        "ready-to-paste `user_args` list and `sampling` block",
        "most tokens per second",
        "one reason for each change",
    ] {
        assert!(text.contains(part), "{part} missing:\n{text}");
    }
}

#[tokio::test]
async fn needs_a_login() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = Browser::new(&s.url);
    let resp = b
        .call("strata-optimization-prompt", &[("model", "IQ3_XXS")])
        .await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn every_installed_model_offers_it() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report
        .info
        .models
        .push(model("coder-IQ1_M", Some("coder"), 30_000_000_000, false));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(html.matches(">Optimization prompt<").count(), 2, "{html}");
}

#[tokio::test]
async fn a_model_that_is_not_installed_has_none() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call("strata-optimization-prompt", &[("model", "IQ3_S")])
        .await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("IQ3_S is not installed")
    );
}

#[tokio::test]
async fn says_not_run_yet_without_benchmarks() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("- Speed benchmark: not run yet"), "{text}");
    assert!(text.contains("- Code benchmark: not run yet"), "{text}");
    // Live data needs no date.
    assert!(!text.contains("was read on"), "{text}");
}

#[tokio::test]
async fn carries_both_benchmarks_of_the_model() {
    let dir = data_dir("prompt-benchmarks");
    std::fs::create_dir_all(&dir).unwrap();
    let task = |name: &str, passed: u32| json!({"name": name, "title": name, "state": "done", "passed": passed, "total": 5});
    let runs = json!([
        {"model": "IQ3_XXS", "state": "done", "started": 1_760_000_000u64,
         "ended": 1_760_000_600u64, "tasks": [task("a", 5), task("b", 3)]},
        {"model": "coder-IQ1_M", "state": "done", "started": 1_760_000_000u64,
         "tasks": [task("a", 1)]}
    ]);
    std::fs::write(dir.join("code-bench.json"), runs.to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "38.5 Token/s");
    run.decode_tps = Some(38.5);
    run.prompt_tps = Some(1210.0);
    run.context = Some(65536);
    run.version = Some("477ac13".into());
    report.info.benchmarks = vec![benchmark("coder-IQ1_M", "90.0 Token/s"), run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains(
            "- Speed benchmark: 38.5 tokens/s answering, 1210 tokens/s reading a 4K prompt, \
             at 65536 tokens context, Strata 477ac13, run on "
        ),
        "{text}"
    );
    assert!(!text.contains("90.0"), "{text}");
    assert!(
        text.contains("- Code benchmark: 8 of 10 hidden tests passed over 2 programming tasks"),
        "{text}"
    );
}

#[tokio::test]
async fn leaves_out_paths_keys_and_the_git_remote() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("0123456789abcdef0123456789abcdef");
    let mut report = strata_info(true);
    report.update = Some(Incoming {
        upstream: "fedcba9876543210".into(),
        total: 0,
        commits: vec![],
        files: vec![],
        remote: Some("https://example.org/someone/strata-fork.git".into()),
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for secret in [
        "Strata-data",
        "0123456789abcdef",
        "strata-fork",
        s.key.as_str(),
        "10:20:30",
        "10-20-30",
        // Free disk space is a live reading.
        "500.0",
    ] {
        assert!(!text.contains(secret), "{secret} in:\n{text}");
    }
}

#[tokio::test]
async fn a_sleeping_pc_gets_the_last_data_with_its_date_and_stays_asleep() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("IQ3_XXS"), "{text}");
    assert!(text.contains("This data was read on 20"), "{text}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn works_after_a_restart_while_the_pc_sleeps() {
    let dir = data_dir("prompt-restart");
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let url = serve_proxy(restarted(&s)).await;
    let mut b = logged_in_again(&url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("RAM: 64.0 GB"), "{text}");
    assert!(text.contains("This data was read on"), "{text}");
}

#[tokio::test]
async fn a_failed_last_run_is_no_not_run_yet() {
    let dir = data_dir("prompt-failed-runs");
    std::fs::create_dir_all(&dir).unwrap();
    let runs = json!([
        {"model": "IQ3_XXS", "state": "failed", "error": "model did not load",
         "started": 1_760_000_000u64, "tasks": []}
    ]);
    std::fs::write(dir.join("code-bench.json"), runs.to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "");
    run.error = Some("Strata did not answer".into());
    report.info.benchmarks = vec![run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains("- Speed benchmark: the last run on "),
        "{text}"
    );
    assert!(text.contains("failed: Strata did not answer"), "{text}");
    assert!(
        text.contains("- Code benchmark: the last run on "),
        "{text}"
    );
    assert!(text.contains("failed: model did not load"), "{text}");
}

#[tokio::test]
async fn shows_family_context_and_greedy_sampling() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    let model = &mut report.info.models[0];
    model.context = Some(65536);
    model.loop_recovery = Some("recover".into());
    model.sampling = Some(StrataSampling {
        support: SamplingSupport::Config,
        thinking: vec![SamplingField {
            name: "temperature".into(),
            value: 0.6,
            source: SamplingSource::Config,
        }],
        instruct: vec![],
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for line in [
        "- Family: Qwen3.8-Flash-Next (qwen)",
        "- Context: 65536 tokens",
        "- Loop recovery: recover",
        "- Sampling with thinking: temperature 0.6 (config)",
        "- Sampling without thinking: greedy",
        "- VRAM: not reported",
    ] {
        assert!(text.contains(line), "{line} missing:\n{text}");
    }
}

fn machine() -> MachineFacts {
    let stick = RamStick {
        size_bytes: Some(32 << 30),
        kind: Some("DDR5".into()),
        rated_mts: Some(6000),
        configured_mts: Some(4800),
    };
    MachineFacts {
        ram_sticks: vec![stick.clone(), stick],
        ram_slots: Some(4),
        gpus: vec![GpuFacts {
            name: Some("NVIDIA GeForce RTX 4080".into()),
            vram_bytes: Some(16 << 30),
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
    }
}

#[tokio::test]
async fn carries_what_the_pc_is() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.machine = Some(machine());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for line in [
        "- OS: Windows 11 Pro 24H2 build 26100.4652\n",
        "- CPU: AMD Ryzen 9 7950X 16-Core Processor (16 cores, 32 threads, 4501 MHz base clock, AVX2 yes, AVX-512 yes)\n",
        "- RAM: 64 GB, 2 of 4 slots filled\n",
        "  - Stick: 32 GB, DDR5, rated 6000 MT/s, running at 4800 MT/s\n",
        "- Graphics card: NVIDIA GeForce RTX 4080 (16.0 GB VRAM, driver 32.0.15.6094, PCIe gen 4 x16, gen 1 x16 when read; an idle card lowers its link)\n",
        "- Drive with the models: NVMe SSD\n",
    ] {
        assert!(text.contains(line), "{line} missing:\n{text}");
    }
    assert_eq!(text.matches("  - Stick:").count(), 2, "{text}");
    assert!(!text.contains("VRAM: not reported"), "{text}");
}

#[tokio::test]
async fn leaves_out_what_the_pc_service_could_not_read() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.machine = Some(MachineFacts {
        cpu: CpuFacts {
            name: Some("Intel Core i7".into()),
            ..CpuFacts::default()
        },
        gpus: vec![GpuFacts {
            name: Some("Radeon".into()),
            ..GpuFacts::default()
        }],
        ..MachineFacts::default()
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("- CPU: Intel Core i7\n"), "{text}");
    assert!(text.contains("- RAM: 64.0 GB\n"), "{text}");
    assert!(text.contains("- Graphics card: Radeon\n"), "{text}");
    for gone in ["- OS:", "Stick", "Drive with the models", "PCIe", "driver"] {
        assert!(!text.contains(gone), "{gone} shown:\n{text}");
    }
}

#[tokio::test]
async fn carries_the_run_config_and_the_chat_settings() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    let config = "{\n \"exe\": \"strata.exe\",\n \"args\": [\n  \"--native\",\n  \"model.gguf\"\n ],\n \"port\": 8080\n}";
    report.info.models[0].config = Some(config.into());
    report.info.models[0].chat_settings = Some("{\n \"temperature\": 0.6\n}".into());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains(&format!("```json\n{config}\n```")), "{text}");
    assert!(
        text.contains("### Chat settings\n\nWhat Strata's web app keeps"),
        "{text}"
    );
    assert!(text.contains("\"temperature\": 0.6"), "{text}");
}

#[tokio::test]
async fn a_fence_in_the_chat_settings_does_not_end_its_block() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].chat_settings = Some("{\n \"system\": \"```py\\nx\\n```\"\n}".into());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("````json\n{"), "{text}");
    assert!(text.contains("}\n````\n"), "{text}");
}

#[tokio::test]
async fn leaves_out_the_chat_settings_without_the_file() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].config = Some("{\n \"exe\": \"strata.exe\"\n}".into());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("### Run config"), "{text}");
    assert!(!text.contains("### Chat settings"), "{text}");
}

#[tokio::test]
async fn carries_the_load_lines_or_says_none_were_seen() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].load_lines = Some(LoadLines {
        at: 1_760_000_000,
        lines: vec!["loading experts".into(), "listening on 0.0.0.0:8080".into()],
        answered: true,
    });
    report
        .info
        .models
        .push(model("swift-IQ2_XS", Some("swift"), 30_000_000_000, false));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains("```text\nloading experts\nlistening on 0.0.0.0:8080\n```"),
        "{text}"
    );
    assert!(text.contains("until it answered"), "{text}");
    assert!(!text.contains("failed"), "{text}");
    let text = prompt(&mut b, "swift-IQ2_XS").await;
    assert!(
        text.contains("### Load lines\n\nNo load seen yet."),
        "{text}"
    );
}

#[tokio::test]
async fn a_load_that_ended_without_answering_reads_as_failed() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].load_lines = Some(LoadLines {
        at: 1_760_000_000,
        lines: vec!["out of VRAM".into()],
        answered: false,
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains("until it ended without answering, so this load failed"),
        "{text}"
    );
    assert!(text.contains("```text\nout of VRAM\n```"), "{text}");
}

#[tokio::test]
async fn carries_strata_s_options_when_the_pc_service_read_them() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    *s.pc.strata_info.lock().unwrap() = Some(report.clone());
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(!text.contains("### Options the tuner knows"), "{text}");
    assert!(!text.contains("### Engine help"), "{text}");

    let option = |flag: &str, answer: &str| TunerOption {
        flag: flag.into(),
        takes_value: true,
        answer: answer.into(),
        ..Default::default()
    };
    report.info.options = Some(StrataOptions {
        version: "0.2.2".into(),
        tuner: vec![
            TunerOption {
                min: Some("0.0".into()),
                max: Some("0.55".into()),
                ..option("--pcie-frac", "placement")
            },
            TunerOption {
                choices: vec!["3".into(), "4".into()],
                ..option("--spec", "placement")
            },
            TunerOption {
                takes_value: false,
                ..option("--kv-grow", "exact")
            },
            TunerOption {
                choices: vec!["a|b".into()],
                ..option("--odd", "exact\n## not a heading")
            },
        ],
        never: vec![
            NeverRule {
                setting: "STRATA_SH_STREAM".into(),
                env: true,
                when: vec!["gfx12".into()],
                reason: "it halves decode speed on this graphics card".into(),
                ..Default::default()
            },
            NeverRule {
                setting: "--spec".into(),
                test: Some("at_most".into()),
                limit: Some("2".into()),
                families: vec!["flash-next".into()],
                reason: "a 2-token window was 4% to 11% slower".into(),
                ..Default::default()
            },
        ],
        tries: vec![TunerTry {
            label: "Shorter speculative window (3 tokens)".into(),
            answer: "placement".into(),
            args: vec!["--spec".into(), "3".into()],
            families: vec!["flash-next".into()],
        }],
        engine_help: Some("strata generate --pack DIR [options]\n  --kv-grow  grow".into()),
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let text = prompt(&mut b, "IQ3_XXS").await;
    for part in [
        "| `--pcie-frac` | 0.0 to 0.55 | placement |",
        "| `--spec` | 3, 4 | placement |",
        "| `--kv-grow` | on or off | exact |",
        "| `--odd` | a\\|b | exact ## not a heading |",
        "- Shorter speculative window (3 tokens) (`--spec 3`), for flash-next: placement",
        "- environment variable `STRATA_SH_STREAM`, on PCs with gfx12: it halves decode speed",
        "- `--spec` at_most 2, for flash-next: a 2-token window",
        "```text\nstrata generate --pack DIR [options]\n  --kv-grow  grow\n```",
    ] {
        assert!(text.contains(part), "{part} missing:\n{text}");
    }
}
