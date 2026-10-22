//! Apply, Undo and Turn off of a model's preset from the Strata page, against the
//! simulated PC and its service.

use crate::common;

use common::*;
use serde_json::json;
use strata_nas_proxy::events::EventKind;
use strata_pc_protocol::StrataJobState;

const PRESET: &str = "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6";

async fn preset_job(b: &mut Browser, kind: &str) -> reqwest::Response {
    b.call(
        "strata-job",
        &[("kind", kind), ("model", "IQ3_XXS"), ("preset", PRESET)],
    )
    .await
}

#[tokio::test]
async fn apply_and_undo_reach_the_pc_service_and_are_events() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    for (kind, event) in [
        ("apply_preset", EventKind::PresetApplied),
        ("undo_preset", EventKind::PresetUndone),
    ] {
        assert_eq!(preset_job(&mut b, kind).await.status(), 200, "{kind}");
        let body = s.pc.job_bodies.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            body,
            json!({"kind": kind, "model": "IQ3_XXS", "preset": PRESET})
        );
        s.pc.update_job(|j| j.state = StrataJobState::Done);
        eventually("the preset event", || {
            s.state
                .events
                .recent()
                .iter()
                .any(|e| e.kind == event && e.detail.as_deref() == Some("IQ3_XXS"))
        })
        .await;
    }
    // Their own events in place of "Strata job done".
    assert!(!own_kinds(&s.state).contains(&EventKind::StrataJobDone));
}

#[tokio::test]
async fn a_preset_job_needs_its_preset() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[("kind", "apply_preset"), ("model", "IQ3_XXS")],
        )
        .await;
    assert_ne!(resp.status(), 200);
    assert!(s.pc.job_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn turning_a_preset_off_reaches_the_pc_service() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[("kind", "preset_off"), ("model", "IQ3_XXS")],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body = s.pc.job_bodies.lock().unwrap().last().cloned().unwrap();
    assert_eq!(body, json!({"kind": "preset_off", "model": "IQ3_XXS"}));
}
