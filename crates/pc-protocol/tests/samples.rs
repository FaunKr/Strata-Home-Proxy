//! The wire format against stored answers of the PC service: each sample in
//! `samples/` is what today's PC service sends or takes. A type that stops
//! parsing one, or writes it back differently, breaks the proxy or an older
//! PC service.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use strata_pc_protocol::*;

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/samples/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Parses the sample and writes it back unchanged.
fn same<T: Serialize + DeserializeOwned>(name: &str) -> T {
    let wire = sample(name);
    let parsed: T = serde_json::from_value(wire.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wire, "{name}");
    parsed
}

#[test]
fn status() {
    let status: PcStatus = same("status.json");
    assert_eq!(status.version.as_deref(), Some("0.1.0+558db97"));
    let gpu = status.hardware.unwrap().gpu.unwrap();
    assert_eq!(gpu.fan_rpm, Some(0));
    assert_eq!(status.strata_key_generation, Some(7));
    assert_eq!(status.relay, Some(Relay { on: false }));
}

#[test]
fn strata_key() {
    let key: StrataKey = same("strata-key.json");
    assert_eq!(key.generation, 7);
    assert_eq!(key.api_key.len(), 64);
}

#[test]
fn sleep_and_update_answers() {
    assert!(same::<Answer>("sleep-accepted.json").accepted);
    let refused: Answer = same("sleep-refused.json");
    assert_eq!(refused, Answer::refused("User active"));
    assert!(same::<Answer>("update-accepted.json").accepted);
    assert_eq!(
        same::<Refused>("update-refused.json"),
        Refused::new("A Strata job is running")
    );
}

#[test]
fn a_refusal_reads_from_any_refusing_answer() {
    // The Proxy reads every refusal the same way, a sleep refusal included.
    let refused: Refused = serde_json::from_value(sample("sleep-refused.json")).unwrap();
    assert_eq!(refused.reason.as_deref(), Some("User active"));
    same::<Refused>("strata-jobs-refused.json");
}

#[test]
fn forced_sleep_and_restart_answers() {
    let slept: ForcedSleep = same("sleep-force.json");
    assert!(slept.user_active && slept.strata_busy && !slept.gaming_mode);
    assert_eq!(slept.cancelled_job, Some(StrataJobKind::AddModel));
    assert_eq!(
        same::<Refused>("sleep-force-refused.json"),
        Refused::new("The PC service is updating")
    );
    let restarted: ForcedRestart = same("strata-kill.json");
    assert_eq!(restarted.cancelled_job, Some(StrataJobKind::Activate));
    assert!(restarted.started);
    same::<Refused>("strata-kill-refused.json");
    // Without a cancelled job the field stays out.
    let calm = ForcedRestart {
        cancelled_job: None,
        started: false,
    };
    assert_eq!(
        serde_json::to_value(&calm).unwrap(),
        json!({"started": false})
    );
}

#[test]
fn gaming_request() {
    assert!(same::<GamingRequest>("gaming-request.json").enabled);
}

#[test]
fn strata_report_splits_into_the_page_and_the_incoming_commits() {
    let report: StrataReport = same("strata.json");
    let (info, incoming) = report.into_parts();
    let incoming = incoming.unwrap();
    assert_eq!(incoming.total, 2);
    assert_eq!(incoming.files.len(), 3);
    assert_eq!(info.models[0].context, Some(262_144));
    let options = info.options.as_ref().unwrap();
    assert_eq!(options.tuner[1].choices, ["3", "4"]);
    assert!(options.never[0].env);
    assert_eq!(options.tries[0].args, ["--spec", "3"]);
    let tune = info.models[0].last_tune.as_ref().unwrap();
    assert_eq!(tune.mode, TuneMode::Quick);
    assert_eq!(info.models[1].tunable, Some(false));
    let latest = info.models[0]
        .presets
        .as_ref()
        .unwrap()
        .latest
        .as_ref()
        .unwrap();
    assert_eq!(latest.state, Some(PresetState::Soft));
    let sampling = info.models[0].sampling.as_ref().unwrap();
    assert_eq!(sampling.support, SamplingSupport::ModelCard);
    assert!(info.families[0].pictures);
    assert_eq!(info.models[0].pictures, Some(Pictures::Off));
    assert_eq!(info.models[1].pictures, None);
    for (state, name) in [
        (Pictures::Off, "off"),
        (Pictures::Cpu, "cpu"),
        (Pictures::Gpu, "gpu"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Pictures>(json!(name)).unwrap(),
            state
        );
    }
    assert_eq!(sampling.thinking[2].source, SamplingSource::ChatSettings);
    assert_eq!(
        info.benchmarks[1].error.as_deref(),
        Some("Strata did not answer")
    );
    // What the page gets has no incoming commits.
    assert!(serde_json::to_value(&info).unwrap().get("update").is_none());
}

#[test]
fn jobs() {
    let done: StrataJob = same("strata-job.json");
    assert_eq!(done.state, StrataJobState::Done);
    assert_eq!(done.ended, Some(1_792_448_800));
    // `/strata/job/cancel` answers with the job, or refuses.
    same::<Option<StrataJob>>("strata-job.json");
    same::<Refused>("strata-job-cancel-refused.json");
    let running: StrataJob = same("strata-job-running.json");
    assert_eq!(running.kind, StrataJobKind::AddModel);
    assert_eq!(running.eta_secs, Some(321));
    assert!(running.speeds.is_empty());
    // A running tune with its measurements so far.
    let tune: StrataJob = same("strata-job-tune.json");
    assert_eq!(tune.speeds.len(), 2);
    assert_eq!(tune.speeds[0].label, "base");
}

#[test]
fn every_job_kind_has_a_request() {
    let requests: Vec<StrataJobRequest> = same("strata-jobs-requests.json");
    let mut kinds: Vec<_> = requests.iter().map(StrataJobRequest::kind).collect();
    kinds.dedup();
    assert_eq!(kinds, StrataJobKind::ALL);
}

#[test]
fn optional_request_fields_stay_out_when_unset() {
    // An older PC service refuses fields it does not know.
    let install = StrataJobRequest::Install {
        family: "swift".into(),
        size: "IQ3_XXS".into(),
        context: 65536,
        api_key: "k".into(),
        experimental_speed_projection: false,
        pictures: false,
    };
    assert_eq!(
        serde_json::to_value(&install).unwrap(),
        json!({"kind": "install", "family": "swift", "size": "IQ3_XXS", "context": 65536, "api_key": "k"})
    );
    let add = StrataJobRequest::AddModel {
        family: "coder".into(),
        size: "IQ1_M".into(),
        context: 32768,
        api_key: None,
        experimental_speed_projection: false,
        pictures: true,
    };
    assert_eq!(
        serde_json::to_value(&add).unwrap(),
        json!({"kind": "add_model", "family": "coder", "size": "IQ1_M", "context": 32768, "pictures": true})
    );
}

#[test]
fn the_pc_dienst_refuses_unknown_request_fields() {
    let unknown = json!({"kind": "add_model", "family": "coder", "size": "IQ1_M", "context": 32768, "newer_field": 1});
    assert!(serde_json::from_value::<StrataJobRequest>(unknown).is_err());
}

#[test]
fn strata_output() {
    let messages: Vec<StrataOutputMessage> = same("strata-output.json");
    assert_eq!(
        messages[3],
        StrataOutputMessage::Notice {
            reason: StrataOutputNotice::NotStartedByAgent
        }
    );
}

// --- An older PC service: fields added since are missing ---

#[test]
fn an_older_status_still_parses() {
    let status: PcStatus = serde_json::from_value(json!({"gaming_mode": true})).unwrap();
    assert!(status.gaming_mode);
    assert_eq!(status.version, None);
    assert_eq!(status.hardware, None);
    assert_eq!(status.mac, None);
    assert_eq!(status.relay, None);
}

#[test]
fn an_older_strata_answer_still_parses() {
    let old = json!({
        "installed": true,
        "models": [{"id": "IQ3_XXS", "disk_bytes": 1, "active": true}],
        "families": [{"id": "qwen", "title": "Qwen"}],
        "catalog": [{"id": "IQ3_XXS", "family": "qwen", "size": "IQ3_XXS", "download_gb": 75.8,
            "ram_gb": 60, "fits": true, "partly_downloaded": false}],
        "contexts": [65536],
        "default_context": 65536,
        "ram_gb": 64.0,
        "data_dir": "C:\\Strata-data",
        "benchmark": {"model": "IQ3_XXS", "result": "38.5 Token/s", "at": 1},
    });
    let (info, incoming) = serde_json::from_value::<StrataReport>(old)
        .unwrap()
        .into_parts();
    assert!(incoming.is_none());
    let model = &info.models[0];
    assert_eq!(model.speed_projection, None);
    assert_eq!(model.loop_recovery, None);
    assert_eq!(model.sampling, None);
    assert_eq!(model.pictures, None);
    assert!(!info.families[0].speed_projection);
    assert!(!info.families[0].pictures);
    assert!(info.benchmarks.is_empty());
    assert_eq!(info.benchmark.unwrap().decode_tps, None);
    assert!(!info.catalog[0].memory_with_vram);
    assert_eq!(info.vram_gb, None);
}

#[test]
fn an_older_job_still_parses() {
    let job: StrataJob =
        serde_json::from_value(json!({"kind": "install", "state": "running"})).unwrap();
    assert_eq!(job.id, None);
    assert_eq!(job.started, 0);
    assert!(job.lines.is_empty());
}

#[test]
fn the_pc_dienst_sends_no_time_text() {
    let mut bench: StrataBenchmark = serde_json::from_value(json!(
        {"model": "IQ3_XXS", "result": "38.5 Token/s", "at": 1}
    ))
    .unwrap();
    assert_eq!(bench.at_text, "");
    assert!(
        serde_json::to_value(&bench)
            .unwrap()
            .get("at_text")
            .is_none()
    );
    bench.at_text = "19.10. 14:00".into();
    assert_eq!(
        serde_json::to_value(&bench).unwrap()["at_text"],
        "19.10. 14:00"
    );
}
