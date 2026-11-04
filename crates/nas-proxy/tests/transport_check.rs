//! The Transport check: where a JSON request body broke on its way to
//! Strata. The proxy refuses a body that arrived broken, records one Strata
//! refused as invalid JSON with what it sent and what the PC service's relay
//! counted, and shows them on the tab *Transport check*; with
//! `SAVE_BROKEN_BODIES` it keeps the broken bodies.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};

const CHAT: &str = "/v1/chat/completions";
const JSON: &str = "application/json";
/// A body cut right after a field name, as the bodies Strata refused were.
const CUT: &[u8] = br#"{"model": "x", "messages""#;
/// Strata's refusal of such a body (`json.loads` in `serve/server.py`).
const STRATA_REFUSAL: &str = "Expecting ':' delimiter: line 1 column 294771 (char 294770)";

fn valid() -> Vec<u8> {
    serde_json::to_vec(&json!({"model": "x", "messages": [{"role": "user", "content": "café"}]}))
        .unwrap()
}

async fn error_message(resp: reqwest::Response) -> String {
    let body: Value = resp.json().await.unwrap();
    body["error"]["message"].as_str().unwrap().to_string()
}

/// The tab's rows, newest first.
async fn rows(b: &mut Browser) -> Vec<Value> {
    let resp = b.call("transport-check", &[]).await;
    assert_eq!(resp.status(), 200);
    let view: Value = resp.json().await.unwrap();
    view["rows"].as_array().unwrap().clone()
}

async fn only_row(b: &mut Browser) -> Value {
    let rows = rows(b).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows[0].clone()
}

async fn saving_on(s: &Setup) -> Browser {
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save_settings(&mut b, &[("SAVE_BROKEN_BODIES", "true")])
            .await
            .status(),
        200
    );
    b
}

async fn download(b: &Browser, id: u64) -> reqwest::Response {
    let mut req = reqwest::Client::new().get(format!("{}/admin/broken-body/{id}", b.base));
    if let Some(c) = &b.cookie {
        req = req.header("cookie", c);
    }
    req.send().await.unwrap()
}

/// A PC whose Strata runs in relay mode; the proxy has seen its status.
async fn relayed() -> Setup {
    setup_relayed(true, |_| {}).await
}

#[tokio::test]
async fn a_broken_body_is_refused_by_the_proxy_with_bytes_received_and_announced() {
    let s = setup(true, |_| {}).await;
    let resp = post_raw(&s, CHAT, JSON, CUT, None).await;
    assert_eq!(resp.status(), 400);
    let message = error_message(resp).await;
    let n = CUT.len();
    assert_eq!(
        message,
        format!(
            "The request body arrived as invalid JSON at byte {n} of {n} received (Content-Length {n}): the proxy did not pass it on."
        )
    );
}

#[tokio::test]
async fn a_broken_body_without_content_length_says_so() {
    let s = setup(true, |_| {}).await;
    let resp = post_raw(&s, CHAT, JSON, CUT, Some(None)).await;
    assert_eq!(resp.status(), 400);
    let message = error_message(resp).await;
    assert!(message.contains("(no Content-Length)"), "{message}");
}

#[tokio::test]
async fn a_broken_body_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_broken_body_shows_as_refused_in_the_request_log() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, "/admin/api/requests-stream").await;
    live.until("all", |m| m["kind"] == "all").await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let row = live
        .until("finished row", |m| {
            m["kind"] == "row" && m["row"]["state"] == "finished"
        })
        .await["row"]
        .clone();
    assert_eq!(
        row["outcome"],
        json!({"kind": "refused", "reason": "broken_body"}),
        "{row}"
    );
}

#[tokio::test]
async fn a_body_that_is_not_json_by_content_type_passes_unchecked() {
    let s = setup(true, |_| {}).await;
    let resp = post_raw(&s, CHAT, "text/plain", CUT, None).await;
    // Strata's own answer: the fake reads only JSON.
    assert_ne!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(!text.contains("arrived as invalid JSON"), "{text}");
    assert_eq!(
        s.pc.api_requests.load(Ordering::SeqCst),
        0,
        "the fake reads only JSON"
    );
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    let mut b = logged_in(&s.url).await;
    assert!(rows(&mut b).await.is_empty());
}

#[tokio::test]
async fn a_valid_body_passes_unchanged_and_makes_no_row() {
    let s = setup(true, |_| {}).await;
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 200);
    let bodies = s.pc.strata_bodies.lock().unwrap().clone();
    let sent: Value = serde_json::from_slice(&valid()).unwrap();
    assert_eq!(bodies, vec![(CHAT.to_string(), sent)]);
    let mut b = logged_in(&s.url).await;
    assert!(rows(&mut b).await.is_empty());
}

#[tokio::test]
async fn a_broken_body_makes_a_row_on_the_transport_check() {
    let s = setup(true, |_| {}).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    let n = CUT.len() as u64;
    assert_eq!(row["side"], "arrival", "{row}");
    assert_eq!(row["key"], "Test", "{row}");
    assert_eq!(row["path"], CHAT, "{row}");
    assert_eq!(row["announced"], n, "{row}");
    assert_eq!(row["received"], n, "{row}");
    assert_eq!(row["break_at"], n, "{row}");
    assert!(row["message"].as_str().unwrap().contains("EOF"), "{row}");
    assert_eq!(row["saved"], false, "{row}");
}

#[tokio::test]
async fn a_strata_refusal_of_a_valid_body_makes_a_strata_row_with_what_was_sent() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    let resp = post_raw(&s, CHAT, JSON, &valid(), None).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(error_message(resp).await, STRATA_REFUSAL);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    let body = valid();
    let chars = String::from_utf8(body.clone()).unwrap().chars().count() as u64;
    assert_eq!(row["side"], "strata", "{row}");
    assert_eq!(row["message"], STRATA_REFUSAL, "{row}");
    assert_eq!(row["break_at"], 294770, "{row}");
    assert_eq!(row["sent_bytes"], body.len() as u64, "{row}");
    assert_eq!(row["sent_chars"], chars, "{row}");
    assert_ne!(row["sent_bytes"], row["sent_chars"], "a body with é");
    assert_eq!(row["relay"], Value::Null, "{row}");
}

#[tokio::test]
async fn a_strata_refusal_with_another_message_makes_no_row() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_refuses_body("messages must be a non-empty list: send at least one message");
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    assert!(rows(&mut b).await.is_empty());
}

#[tokio::test]
async fn a_relayed_body_that_lost_bytes_makes_a_network_row() {
    let s = relayed().await;
    s.pc.set_relay_drops_bytes(5);
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    let sent = valid().len() as u64;
    assert_eq!(row["side"], "network", "{row}");
    assert_eq!(
        row["relay"],
        json!({"kind": "measured", "received": sent - 5, "passed": sent - 5, "checksum_matches": false}),
        "{row}"
    );
}

#[tokio::test]
async fn a_relayed_body_that_arrived_whole_makes_a_strata_row() {
    let s = relayed().await;
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    let sent = valid().len() as u64;
    assert_eq!(row["side"], "strata", "{row}");
    assert_eq!(
        row["relay"],
        json!({"kind": "measured", "received": sent, "passed": sent, "checksum_matches": true}),
        "{row}"
    );
}

#[tokio::test]
async fn the_relay_gets_the_checksum_of_the_body_as_sent() {
    let s = relayed().await;
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 200);
    assert_eq!(
        s.pc.relay_checksum.lock().unwrap().clone(),
        Some(sha256(&valid()))
    );
}

#[tokio::test]
async fn without_relay_mode_no_checksum_header_is_sent() {
    let s = setup(true, |_| {}).await;
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 200);
    assert_eq!(s.pc.relay_requests.load(Ordering::SeqCst), 0);
    assert_eq!(*s.pc.relay_checksum.lock().unwrap(), None);
}

#[tokio::test]
async fn an_older_pc_service_shows_not_measured() {
    let s = relayed().await;
    s.pc.relay_measures.store(false, Ordering::SeqCst);
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    assert_eq!(post_raw(&s, CHAT, JSON, &valid(), None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    assert_eq!(row["side"], "strata", "{row}");
    assert_eq!(row["relay"], json!({"kind": "not_measured"}), "{row}");
}

#[tokio::test]
async fn the_rows_stay_over_a_restart() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("transport-restart"))).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let before = rows(&mut b).await;
    assert_eq!(before.len(), 1);

    let url = serve_proxy(restarted(&s)).await;
    let mut b = logged_in_again(&url).await;
    assert_eq!(rows(&mut b).await, before);
}

#[tokio::test]
async fn only_the_last_100_rows_are_kept() {
    let s = setup(true, |_| {}).await;
    for _ in 0..101 {
        assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    }
    let mut b = logged_in(&s.url).await;
    let rows = rows(&mut b).await;
    assert_eq!(rows.len(), 100);
    let ids: Vec<u64> = rows.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(ids[0], 101, "newest first: {ids:?}");
    assert_eq!(ids[99], 2, "the first one gone: {ids:?}");
}

#[tokio::test]
async fn bodies_are_not_saved_while_the_switch_is_off() {
    let s = setup(true, |_| {}).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    assert_eq!(row["saved"], false, "{row}");
    assert_eq!(
        download(&b, row["id"].as_u64().unwrap()).await.status(),
        404
    );
}

#[tokio::test]
async fn a_body_broken_on_arrival_is_saved_as_received() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let row = only_row(&mut b).await;
    assert_eq!(row["saved"], true, "{row}");
    let resp = download(&b, row["id"].as_u64().unwrap()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), CUT);
}

#[tokio::test]
async fn a_body_strata_refused_is_saved_as_sent() {
    let s = setup(true, |c| c.hide_model = true).await;
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    let mut b = saving_on(&s).await;
    // The real model's id comes from `/health` before the request.
    s.state.strata_ready().await;
    let resp = post_raw(&s, CHAT, JSON, &valid(), None).await;
    assert_eq!(resp.status(), 400);
    let row = only_row(&mut b).await;
    let saved = download(&b, row["id"].as_u64().unwrap())
        .await
        .bytes()
        .await
        .unwrap();
    let saved: Value = serde_json::from_slice(&saved).unwrap();
    // Renamed from the public name to the running model, as sent.
    assert_ne!(saved["model"], "x", "{saved}");
    assert_eq!(saved["messages"][0]["content"], "café", "{saved}");
}

#[tokio::test]
async fn only_the_last_10_bodies_are_kept() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    for _ in 0..11 {
        assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    }
    let rows = rows(&mut b).await;
    assert_eq!(rows.len(), 11);
    let saved: Vec<bool> = rows.iter().map(|r| r["saved"].as_bool().unwrap()).collect();
    let mut expected = vec![true; 10];
    expected.push(false);
    assert_eq!(saved, expected, "the oldest body gone, its row kept");
}

#[tokio::test]
async fn a_saved_body_downloads_as_kept() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    let body = "{\"a\": \"é\", \"b\" 1}".as_bytes();
    assert_eq!(post_raw(&s, CHAT, JSON, body, None).await.status(), 400);
    let row = only_row(&mut b).await;
    let id = row["id"].as_u64().unwrap();
    let resp = download(&b, id).await;
    assert_eq!(resp.status(), 200);
    let disposition = resp.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disposition.contains(&format!("broken-body-{id}.json")),
        "{disposition}"
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), body);
}

#[tokio::test]
async fn deleting_saved_bodies_keeps_the_rows() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    assert_eq!(b.call("delete-broken-bodies", &[]).await.status(), 200);
    let row = only_row(&mut b).await;
    assert_eq!(row["saved"], false, "{row}");
    assert_eq!(
        download(&b, row["id"].as_u64().unwrap()).await.status(),
        404
    );
}

#[tokio::test]
async fn switching_saving_off_deletes_the_saved_bodies() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    assert_eq!(
        save_settings(&mut b, &[("SAVE_BROKEN_BODIES", "false")])
            .await
            .status(),
        200
    );
    let row = only_row(&mut b).await;
    assert_eq!(row["saved"], false, "{row}");
}

#[tokio::test]
async fn the_transport_check_needs_the_login() {
    let s = setup(true, |_| {}).await;
    let mut b = saving_on(&s).await;
    assert_eq!(post_raw(&s, CHAT, JSON, CUT, None).await.status(), 400);
    let id = only_row(&mut b).await["id"].as_u64().unwrap();
    let mut anonymous = Browser::new(&s.url);
    assert_ne!(anonymous.call("transport-check", &[]).await.status(), 200);
    assert_ne!(
        anonymous.call("delete-broken-bodies", &[]).await.status(),
        200
    );
    assert_eq!(download(&anonymous, id).await.status(), 401);
    // An API key opens none of it.
    let with_key = reqwest::Client::new()
        .post(format!("{}/admin/api/transport-check", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_ne!(with_key.status(), 200);
    let with_key = reqwest::Client::new()
        .get(format!("{}/admin/broken-body/{id}", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(with_key.status(), 401);
    assert_eq!(only_row(&mut b).await["saved"], true);
}

#[tokio::test]
async fn a_streamed_request_strata_refused_after_a_wake_up_makes_a_strata_row() {
    let s = setup(false, |_| {}).await;
    s.pc.set_strata_refuses_body(STRATA_REFUSAL);
    let body = serde_json::to_vec(&json!({"model": "x", "stream": true, "messages": []})).unwrap();
    let resp = post_raw(&s, CHAT, JSON, &body, None).await;
    // The status line went out with the first ping; the refusal follows as an event.
    let text = resp.text().await.unwrap();
    assert!(text.contains("294770"), "{text}");
    let mut b = logged_in(&s.url).await;
    let row = only_row(&mut b).await;
    assert_eq!(row["side"], "strata", "{row}");
    assert_eq!(row["sent_bytes"], body.len() as u64, "{row}");
}
