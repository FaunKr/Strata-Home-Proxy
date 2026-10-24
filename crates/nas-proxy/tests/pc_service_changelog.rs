//! The PC service card lists what an update brings, from the changelog:
//! the versions above the installed one up to the one on the NAS, a step
//! on the PC where the minor number goes up.

use crate::common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};

const CHANGELOG: &str = "\
# A title and words for people, before the first version

## 0.4.1

- Four-one fixes

## 0.4.0

- Run the install command again
- Four-zero adds

## 0.3.2

- Three-two adds

## 0.3.0

- Change the config

## 0.2.0

- Two adds
";

/// An awake PC whose service runs `installed`, with `available` in the
/// image and the test changelog.
async fn update(name: &str, installed: Option<&str>, available: &str) -> Setup {
    let s = setup(true, |_| {}).await;
    s.state.pc_setup.set_program(agent_program(name, available));
    s.state.pc_setup.set_changelog(CHANGELOG).unwrap();
    *s.pc.version.lock().unwrap() = installed.map(str::to_string);
    s
}

/// The PC service view once the service reported, from the live overview.
async fn service(s: &Setup) -> Value {
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("PC service", |v| !v["pc_service"].is_null())
        .await;
    v["pc_service"].clone()
}

/// Version and needs-a-step of each listed change.
fn listed(service: &Value) -> Vec<(String, bool)> {
    service["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["version"].as_str().unwrap().to_string(),
                c["needs_step"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, bool)]) -> Vec<(String, bool)> {
    list.iter().map(|(v, s)| (v.to_string(), *s)).collect()
}

#[tokio::test]
async fn only_versions_above_the_installed_one_up_to_the_available_one_are_listed() {
    let s = update("between", Some("0.3.0+alt1234"), "0.4.0+neu5678").await;
    let v = service(&s).await;
    assert_eq!(v["update_available"], true);
    assert_eq!(listed(&v), pairs(&[("0.4.0", true), ("0.3.2", false)]));
    assert_eq!(
        v["changes"][0]["lines"],
        json!(["Run the install command again", "Four-zero adds"])
    );
    assert_eq!(v["only_internal_changes"], false);
}

#[tokio::test]
async fn a_service_on_0_1_0_gets_every_version() {
    let s = update("from-0-1-0", Some("0.1.0+alt1234"), "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert_eq!(
        listed(&v),
        pairs(&[
            ("0.4.1", false),
            ("0.4.0", true),
            ("0.3.2", false),
            ("0.3.0", true),
            ("0.2.0", true),
        ])
    );
}

#[tokio::test]
async fn a_service_without_a_version_gets_every_version() {
    let s = update("no-version", None, "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert!(v["version"].is_null());
    assert_eq!(listed(&v).len(), 5);
    assert_eq!(listed(&v)[4], ("0.2.0".to_string(), true));
}

#[tokio::test]
async fn a_step_counts_against_the_installed_version() {
    // 0.3.2 to 0.4.0 needs a step; 0.4.0 to 0.4.1 does not.
    let s = update("step", Some("0.3.2+alt1234"), "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert_eq!(listed(&v), pairs(&[("0.4.1", false), ("0.4.0", true)]));

    let s = update("no-step", Some("0.4.0+alt1234"), "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert_eq!(listed(&v), pairs(&[("0.4.1", false)]));
}

#[tokio::test]
async fn an_update_within_one_version_brings_only_internal_changes() {
    let s = update("internal", Some("0.4.1+alt1234"), "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert_eq!(v["update_available"], true);
    assert_eq!(listed(&v), vec![]);
    assert_eq!(v["only_internal_changes"], true);
}

#[tokio::test]
async fn an_up_to_date_service_shows_no_changes() {
    let s = update("current", Some("0.4.1+neu5678"), "0.4.1+neu5678").await;
    let v = service(&s).await;
    assert_eq!(v["update_available"], false);
    assert_eq!(listed(&v), vec![]);
    assert_eq!(v["only_internal_changes"], false);
}

#[tokio::test]
async fn an_older_program_on_the_nas_lists_nothing() {
    let s = update("older", Some("0.4.1+alt1234"), "0.3.0+neu5678").await;
    let v = service(&s).await;
    assert_eq!(v["update_available"], true);
    assert_eq!(listed(&v), vec![]);
    assert_eq!(v["only_internal_changes"], false);
}

#[tokio::test]
async fn the_changes_stay_while_an_update_runs() {
    let s = update("running", Some("0.4.0+alt1234"), "0.4.1+neu5678").await;
    // The service accepts and then reports nothing new for a while.
    s.state.pc_setup.set_update_timeout(Duration::from_secs(60));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC service", |v| !v["pc_service"].is_null())
        .await;
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    let v = live
        .until("updating", |v| v["pc_service"]["updating"] == true)
        .await;
    assert_eq!(listed(&v["pc_service"]), pairs(&[("0.4.1", false)]));
}
