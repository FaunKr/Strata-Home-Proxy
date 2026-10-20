//! The power of the power plug on the Overview, against the simulated PC and
//! its plug.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;

#[tokio::test]
async fn without_a_plug_there_is_no_power() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC awake", |v| v["pc"] == "Awake").await;
    assert_eq!(v["power"], "NotConfigured");
}

#[tokio::test]
async fn shows_the_power_of_a_gen2_plug_live() {
    let s = setup(true, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 87.4;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("power", |v| v["power"]["Watts"] == 87.4).await;
    *s.pc.plug_watts.lock().unwrap() = 312.0;
    live.until("new power", |v| v["power"]["Watts"] == 312.0)
        .await;
}

#[tokio::test]
async fn shows_the_power_while_the_pc_sleeps() {
    let s = setup(false, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 1.2;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("power while asleep", |v| v["power"]["Watts"] == 1.2)
        .await;
    assert_eq!(v["pc"], "Asleep");
}

#[tokio::test]
async fn the_plug_is_set_up_on_the_settings_page_by_its_bare_address() {
    let s = setup(true, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 87.0;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("without a power plug", |v| v["power"] == "NotConfigured")
        .await;
    let resp = save_settings(&mut b, &[("SHELLY_URL", &s.plug)]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.state.cfg().shelly_url, Some(format!("http://{}", s.plug)));
    live.until("power", |v| v["power"]["Watts"] == 87.0).await;
}

#[tokio::test]
async fn the_plug_is_set_up_by_a_full_url() {
    let s = setup(true, |_| {}).await;
    *s.pc.plug_watts.lock().unwrap() = 87.0;
    let mut b = logged_in(&s.url).await;
    let url = format!("http://{}/", s.plug);
    let resp = save_settings(&mut b, &[("SHELLY_URL", &url)]).await;
    assert_eq!(resp.status(), 200);
    let mut live = Live::open(&b).await;
    live.until("power", |v| v["power"]["Watts"] == 87.0).await;
}

#[tokio::test]
async fn shows_the_power_of_a_gen1_plug() {
    let s = setup(true, |_| {}).await;
    s.pc.plug_gen1.store(true, Ordering::SeqCst);
    *s.pc.plug_watts.lock().unwrap() = 64.5;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("power Gen1", |v| v["power"]["Watts"] == 64.5)
        .await;
}

#[tokio::test]
async fn an_unreachable_plug_is_shown_and_comes_back() {
    let s = setup(true, |_| {}).await;
    s.pc.plug_down.store(true, Ordering::SeqCst);
    *s.pc.plug_watts.lock().unwrap() = 87.0;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("unreachable", |v| v["power"] == "Unreachable")
        .await;
    assert_eq!(v["pc"], "Awake");
    assert_eq!(v["strata"], "Ready");
    s.pc.plug_down.store(false, Ordering::SeqCst);
    live.until("back again", |v| v["power"]["Watts"] == 87.0)
        .await;
}

#[tokio::test]
async fn a_plug_that_is_not_there_is_unreachable() {
    // Nothing listens on port 9 of localhost.
    let s = setup(true, |_| {}).await;
    s.state.settings.set("SHELLY_URL", "127.0.0.1:9").unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("unreachable", |v| v["power"] == "Unreachable")
        .await;
}

#[tokio::test]
async fn a_hanging_plug_holds_back_neither_pc_nor_strata() {
    let s = setup(true, |_| {}).await;
    s.pc.plug_hangs.store(true, Ordering::SeqCst);
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("unreachable", |v| v["power"] == "Unreachable")
        .await;
    assert_eq!(v["pc"], "Awake");
    assert_eq!(v["strata"], "Ready");
    s.pc.gaming.store(true, Ordering::SeqCst);
    let v = live.until("Gaming", |v| v["pc"] == "Gaming").await;
    assert_eq!(v["power"], "Unreachable");
}

#[tokio::test]
async fn an_unreachable_plug_records_no_event() {
    let s = setup(true, |_| {}).await;
    s.pc.plug_down.store(true, Ordering::SeqCst);
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("unreachable", |v| v["power"] == "Unreachable")
        .await;
    let before = kinds(&s.state);
    let rounds = s.state.overview.rounds();
    eventually("more rounds", || s.state.overview.rounds() >= rounds + 5).await;
    assert_eq!(kinds(&s.state), before);
}
