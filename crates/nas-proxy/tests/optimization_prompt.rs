//! The optimization prompt: an English text per installed model for an AI
//! chat, built by the proxy from what the PC service reported.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;
use strata_pc_protocol::{
    CpuFacts, GpuFacts, Incoming, LoadLines, MachineFacts, NeverRule, PresetState, RamStick,
    SamplingField, SamplingSource, SamplingSupport, StrataOptions, StrataPreset, StrataPresets,
    StrataSampling, StrataTune, TuneMode, TuneUse, TunerOption, TunerTry, WindowsFacts,
};

async fn prompt(b: &mut Browser, model: &str) -> String {
    let resp = b
        .call("strata-optimization-prompt", &[("model", model)])
        .await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn asks_for_three_blocks_as_the_fields_take_them() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].user_args = Some(vec!["--threads".into(), "8".into()]);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    let answer = text
        .split("\n## Your answer\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no answer section:\n{text}"));
    for part in [
        "exactly three code blocks, in this order, each ready to paste as it is",
        "1. Engine options: a `text` block with the whole new `user_args`, one option with its value per line (`--name value`), for the *Engine options* field in my web UI.",
        "2. Engine environment: a `text` block with the whole new `env`, one `NAME=value` per line, for the *Engine environment* field.",
        "3. Sampling: a `json` block with the whole new `sampling` object of the run config.",
        "Leave a block empty when nothing in its field should change.",
    ] {
        assert!(answer.contains(part), "{part} missing:\n{answer}");
    }
    assert!(
        text.contains(
            "only environment variables from the list under *Engine environment variables*"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn a_strata_that_keeps_no_options_by_hand_gets_an_empty_options_block() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].user_args = None;
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains(
            "1. Engine options: an empty `text` block: my Strata version keeps no engine options set by hand."
        ),
        "{text}"
    );
    assert!(!text.contains("the whole new `user_args`"), "{text}");
}

#[tokio::test]
async fn lists_the_model_s_engine_environment() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].env = Some(
        [("STRATA_PIN_BY_NEED", "1"), ("STRATA_NOTE", "a b=c")]
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .into(),
    );
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains("```text\nSTRATA_NOTE=a b=c\nSTRATA_PIN_BY_NEED=1\n```"),
        "{text}"
    );

    let mut report = strata_info(true);
    report.info.models[0].env = Some(Default::default());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("### Engine environment\n\n"), "{text}");
    assert!(text.contains("None set."), "{text}");
    // Without the tuner's table there is nothing to suggest from.
    assert!(
        text.contains("did not report the variables Strata's tuner knows, so suggest none"),
        "{text}"
    );
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
    assert!(
        text.contains("### Speed benchmark\n\n- not run yet\n"),
        "{text}"
    );
    assert!(
        text.contains("### Code benchmark\n\n- not run yet\n"),
        "{text}"
    );
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
        text.contains(": 38.5 tokens/s answering, 1210 tokens/s reading a 4K prompt. "),
        "{text}"
    );
    assert!(!text.contains("90.0"), "{text}");
    assert!(
        text.contains(
            ": 8 of 10 hidden tests passed over 2 programming tasks with opencode. Settings not recorded\n"
        ),
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
        text.contains(": failed: Strata did not answer. Settings not recorded\n"),
        "{text}"
    );
    assert!(
        text.contains(": failed: model did not load. Settings not recorded\n"),
        "{text}"
    );
    assert_eq!(runs_under(&text, "Speed benchmark").len(), 1, "{text}");
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
        tries: vec![
            TunerTry {
                label: "Shorter speculative window (3 tokens)".into(),
                answer: "placement".into(),
                args: vec!["--spec".into(), "3".into()],
                families: vec!["flash-next".into()],
                ..Default::default()
            },
            TunerTry {
                label: "Pin memory only when needed".into(),
                answer: "exact".into(),
                env: [("STRATA_PIN_BY_NEED".to_string(), Some("1".to_string()))].into(),
                families: vec!["flash-next".into()],
                ..Default::default()
            },
        ],
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
        "### Engine environment variables\n\nThe variables Strata's tuner knows; suggest no others.",
        "- `STRATA_PIN_BY_NEED=1`: Pin memory only when needed, for flash-next: exact",
        "- never `STRATA_SH_STREAM`, on PCs with gfx12: it halves decode speed",
        "- `--spec` at_most 2, for flash-next: a 2-token window",
        "```text\nstrata generate --pack DIR [options]\n  --kv-grow  grow\n```",
    ] {
        assert!(text.contains(part), "{part} missing:\n{text}");
    }
}

fn preset(active: bool, state: Option<PresetState>, reasons: &[&str]) -> StrataPreset {
    StrataPreset {
        id: "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6".into(),
        name: "IQ3_XXS 64K – Full speed".into(),
        active,
        state,
        reasons: reasons.iter().map(|r| r.to_string()).collect(),
        gain_pct: Some(12.4),
        settings: vec!["--kv-resident 65536".into(), "--kv-grow".into()],
        decode_tps: Some(35.1),
        base_tps: Some(31.2),
    }
}

/// The prompt for IQ3_XXS with these presets and last tune.
async fn tuning_prompt(presets: Option<StrataPresets>, tune: Option<StrataTune>) -> String {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].presets = presets;
    report.info.models[0].last_tune = tune;
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    prompt(&mut b, "IQ3_XXS").await
}

fn latest(preset: StrataPreset) -> Option<StrataPresets> {
    Some(StrataPresets {
        latest: Some(preset),
        other_context: false,
    })
}

#[tokio::test]
async fn an_active_preset_comes_with_its_settings_and_wins_over_the_config() {
    let text = tuning_prompt(latest(preset(true, Some(PresetState::Fresh), &[])), None).await;
    for part in [
        "### Strata's tuning",
        "- Preset: \"IQ3_XXS 64K – Full speed\" (`--kv-resident 65536`, `--kv-grow`; \
         measured 31.2 → 35.1 tokens/s answering, +12%), active",
        "its settings win over `args` and `user_args`",
        "- Last tune: not run yet",
    ] {
        assert!(text.contains(part), "{part} missing:\n{text}");
    }
    assert!(!text.contains("re-check"), "{text}");

    let text = tuning_prompt(
        latest(preset(
            true,
            Some(PresetState::Soft),
            &["the graphics driver changed"],
        )),
        None,
    )
    .await;
    assert!(
        text.contains("Strata offers a re-check, because the graphics driver changed"),
        "{text}"
    );
}

#[tokio::test]
async fn a_preset_not_applied_or_missing_for_the_context_says_so() {
    let text = tuning_prompt(
        latest(preset(
            true,
            Some(PresetState::Hard),
            &["the graphics card changed"],
        )),
        None,
    )
    .await;
    assert!(
        text.contains(
            "active but not applied, because the graphics card changed: the run config runs as it is"
        ),
        "{text}"
    );
    assert!(!text.contains("win over"), "{text}");

    let text = tuning_prompt(latest(preset(false, None, &[])), None).await;
    assert!(text.contains("saved but not applied"), "{text}");

    let other = StrataPresets {
        latest: None,
        other_context: true,
    };
    let text = tuning_prompt(Some(other), None).await;
    assert!(text.contains("- Preset: none for this context"), "{text}");

    let text = tuning_prompt(None, None).await;
    assert!(text.contains("- Preset: unknown"), "{text}");
}

#[tokio::test]
async fn carries_the_last_tune() {
    let tune = StrataTune {
        at: 1_792_448_900,
        mode: TuneMode::Thorough,
        usage: TuneUse::Chat,
        code: "faster".into(),
        base_tps: Some(31.2),
        tuned_tps: Some(35.1),
        gain_pct: Some(12.4),
        changed: vec!["--kv-grow".into()],
        message: None,
        preset: Some("3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6".into()),
        preset_name: Some("IQ3_XXS 64K – Full speed".into()),
    };
    let text = tuning_prompt(None, Some(tune.clone())).await;
    assert!(
        text.contains(
            "(thorough, for chat): faster, 31.2 → 35.1 tokens/s with `--kv-grow`, \
             saved as preset \"IQ3_XXS 64K – Full speed\""
        ),
        "{text}"
    );
    let busy = StrataTune {
        code: "busy_pc".into(),
        message: Some("Your PC was too busy to compare settings.".into()),
        ..tune
    };
    let text = tuning_prompt(None, Some(busy)).await;
    assert!(
        text.contains("busy_pc: Your PC was too busy to compare settings."),
        "{text}"
    );
}

#[tokio::test]
async fn tuned_settings_lose_their_paths_and_a_loss_shows_as_such() {
    let mut odd = preset(true, Some(PresetState::Hard), &[]);
    odd.settings = vec![
        "--draft-model D:\\Strata-data\\models\\draft.gguf".into(),
        "STRATA_CACHE=C:/Users/faun/cache".into(),
        "--split 1/2".into(),
    ];
    odd.gain_pct = Some(-2.6);
    let text = tuning_prompt(latest(odd), None).await;
    assert!(
        text.contains("`--draft-model draft.gguf`, `STRATA_CACHE=cache`, `--split 1/2`"),
        "{text}"
    );
    assert!(
        !text.contains("Strata-data") && !text.contains("Users"),
        "{text}"
    );
    assert!(text.contains("-3%") && !text.contains("+-"), "{text}");
    assert!(
        text.contains("active but not applied: the run config runs as it is"),
        "{text}"
    );

    let mut both = latest(preset(true, Some(PresetState::Fresh), &[])).unwrap();
    both.other_context = true;
    let text = tuning_prompt(Some(both), None).await;
    assert!(
        text.contains("; the last tune was for other settings"),
        "{text}"
    );
}

/// The lines of the prompt's list under `heading`.
fn runs_under(text: &str, heading: &str) -> Vec<String> {
    text.split(&format!("### {heading}\n\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no {heading} in:\n{text}"))
        .lines()
        .take_while(|l| l.starts_with("- "))
        .map(str::to_string)
        .collect()
}

/// `n` finished code benchmark runs of IQ3_XXS, the oldest first, each
/// with an engine environment of its own.
fn code_runs(n: u64) -> serde_json::Value {
    let task = json!({"name": "a", "title": "a", "state": "done", "passed": 1, "total": 5});
    json!(
        (0..n)
            .map(|i| json!({"id": i + 1, "model": "IQ3_XXS", "state": "done",
            "started": 1_760_000_000u64 + i * 100, "ended": 1_760_000_050u64 + i * 100,
            "tasks": [task.clone()],
            "settings": {"context": 65536, "env": {"GGML_X": i.to_string()}}}))
            .collect::<Vec<_>>()
    )
}

#[tokio::test]
async fn lists_the_last_five_runs_of_each_newest_first_with_their_settings() {
    let dir = data_dir("prompt-five-runs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("code-bench.json"), code_runs(6).to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut b = logged_in(&s.url).await;
    // Six speed runs, each seen by the proxy; the first predates its history.
    for i in 0..6u64 {
        let mut report = strata_info(true);
        report.info.models[0].env = Some([("GGML_X".to_string(), i.to_string())].into());
        report.info.models[0].presets = Some(StrataPresets {
            latest: Some(StrataPreset {
                id: "p1".into(),
                name: "coding quick".into(),
                active: true,
                state: None,
                reasons: vec![],
                gain_pct: None,
                settings: vec!["n_gpu_layers 40".into()],
                decode_tps: None,
                base_tps: None,
            }),
            other_context: false,
        });
        let mut run = benchmark("IQ3_XXS", "");
        run.at = 1_760_000_000 + i * 100;
        run.result = format!("{}.0 Token/s", 30 + i);
        run.decode_tps = Some((30 + i) as f64);
        run.context = Some(65536);
        report.info.benchmarks = vec![run];
        *s.pc.strata_info.lock().unwrap() = Some(report);
        b.call("strata", &[]).await;
    }
    let text = prompt(&mut b, "IQ3_XXS").await;
    let speed = runs_under(&text, "Speed benchmark");
    assert_eq!(speed.len(), 5, "{text}");
    assert!(speed[0].contains(": 35.0 tokens/s answering. Settings: 65536 tokens context; Strata 477ac13; preset \"coding quick\" (`n_gpu_layers 40`); engine environment `GGML_X=5`"), "{text}");
    assert!(speed[4].contains("31.0 tokens/s"), "{text}");
    assert!(!text.contains("30.0 tokens/s"), "{text}");

    let code = runs_under(&text, "Code benchmark");
    assert_eq!(code.len(), 5, "{text}");
    assert!(code[0].contains("engine environment `GGML_X=5`"), "{text}");
    assert!(code[4].contains("engine environment `GGML_X=1`"), "{text}");
    assert!(!text.contains("`GGML_X=0`"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn failed_runs_count_toward_the_five() {
    let dir = data_dir("prompt-failed-five");
    std::fs::create_dir_all(&dir).unwrap();
    let mut runs = code_runs(5);
    runs.as_array_mut()
        .unwrap()
        .push(json!({"id": 9, "model": "IQ3_XXS",
        "state": "failed", "error": "model did not load",
        "started": 1_760_001_000u64, "ended": 1_760_001_000u64, "tasks": []}));
    std::fs::write(dir.join("code-bench.json"), runs.to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    let code = runs_under(&text, "Code benchmark");
    assert_eq!(code.len(), 5, "{text}");
    assert!(
        code[0].contains(": failed: model did not load. Settings not recorded"),
        "{text}"
    );
    assert!(!text.contains("`GGML_X=0`"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_deleted_run_is_left_out() {
    let dir = data_dir("prompt-deleted");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("code-bench.json"), code_runs(2).to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "38.5 Token/s");
    run.decode_tps = Some(38.5);
    report.info.benchmarks = vec![run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let page: serde_json::Value = b.call("strata", &[]).await.json().await.unwrap();
    let speed_id = page["benchmarks"][0]["runs"][0]["id"].to_string();
    let resp = b
        .call(
            "benchmark-delete-run",
            &[("benchmark", "speed"), ("id", &speed_id)],
        )
        .await;
    assert_eq!(resp.status(), 200);
    // The newest code run, the second.
    let resp = b
        .call(
            "benchmark-delete-run",
            &[("benchmark", "code"), ("id", "2")],
        )
        .await;
    assert_eq!(resp.status(), 200);

    // The PC service still reports the deleted speed run.
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert_eq!(
        runs_under(&text, "Speed benchmark"),
        ["- not run yet"],
        "{text}"
    );
    let code = runs_under(&text, "Code benchmark");
    assert_eq!(code.len(), 1, "{text}");
    assert!(code[0].contains("`GGML_X=0`"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_run_from_before_the_history_says_what_it_measured_with() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "38.5 Token/s");
    run.decode_tps = Some(38.5);
    run.context = Some(65536);
    run.speed_projection = Some(false);
    run.version = Some("477ac13".into());
    report.info.benchmarks = vec![run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    let speed = runs_under(&text, "Speed benchmark");
    assert!(
        speed[0].ends_with(
            ": 38.5 tokens/s answering. Settings not recorded; 65536 tokens context; \
             speed projection off; Strata 477ac13"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn a_failed_speed_run_after_a_measurement_is_a_line_of_its_own() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "38.5 Token/s");
    run.decode_tps = Some(38.5);
    run.context = Some(32768);
    report.info.benchmarks = vec![run.clone()];
    *s.pc.strata_info.lock().unwrap() = Some(report.clone());
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    // The PC service keeps the measurement next to the error, here one
    // with a path and backticks.
    run.error = Some("could not load C:\\Users\\me\\Strata-data\\x.gguf: `oom`".into());
    report.info.models[0].context = Some(131072);
    report.info.benchmarks = vec![run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let text = prompt(&mut b, "IQ3_XXS").await;
    let speed = runs_under(&text, "Speed benchmark");
    assert_eq!(speed.len(), 2, "{text}");
    // The failure ran with the model's settings, not the measurement's.
    assert!(
        speed[0]
            .contains(": failed: could not load x.gguf: 'oom'. Settings: 131072 tokens context"),
        "{text}"
    );
    assert!(speed[1].contains(": 38.5 tokens/s answering. "), "{text}");
    assert!(!text.contains("Strata-data"), "{text}");
}
