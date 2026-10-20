//! Setting up the PC service from the NAS: the program to download, the
//! one-time code and the sign-up of a new PC. None of it wakes the PC.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;

/// A stand-in for `strata-pc-agent.exe` in a throwaway folder.
fn fake_exe(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("strata-proxy-exe-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("strata-pc-agent.exe");
    std::fs::write(&exe, b"MZ fake program").unwrap();
    exe
}

#[tokio::test]
async fn program_downloads_without_login_and_without_waking() {
    let s = setup(false, |_| {}).await;
    s.state.pc_setup.set_program(fake_exe("download"));
    let resp = reqwest::get(format!("{}/admin/agent/strata-pc-agent.exe", s.url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/octet-stream");
    assert!(
        resp.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("strata-pc-agent.exe")
    );
    assert_eq!(&resp.bytes().await.unwrap()[..], b"MZ fake program");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_program_is_not_found() {
    let s = setup(false, |_| {}).await;
    s.state
        .pc_setup
        .set_program(std::env::temp_dir().join("does-not-exist/strata-pc-agent.exe"));
    let resp = reqwest::get(format!("{}/admin/agent/strata-pc-agent.exe", s.url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn setup_calls_do_not_postpone_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(600)).await;
    s.state.pc_setup.set_program(fake_exe("idle"));
    for _ in 0..8 {
        reqwest::get(format!("{}/admin/agent/strata-pc-agent.exe", s.url))
            .await
            .unwrap();
        script(&s, "?code=WRONG").await;
        register(&s, "WRONG", 8091).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

/// The command the setup page shows, and its code.
async fn command(b: &mut Browser) -> (String, String) {
    let resp = b.call("pc-setup-command", &[]).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    (
        body["command"].as_str().unwrap().to_string(),
        body["code"].as_str().unwrap().to_string(),
    )
}

async fn register(s: &Setup, code: &str, port: u16) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/admin/agent/register", s.url))
        .json(&serde_json::json!({"code": code, "port": port}))
        .send()
        .await
        .unwrap()
}

/// The port of the fake PC service the rig set up.
fn agent_port(s: &Setup) -> u16 {
    let url = s.state.cfg().pc_agent_url.clone().unwrap();
    url.rsplit(':').next().unwrap().parse().unwrap()
}

#[tokio::test]
async fn command_needs_login() {
    let s = setup(false, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(b.call("pc-setup-command", &[]).await.status(), 401);
}

#[tokio::test]
async fn command_points_at_the_address_the_page_was_opened_on() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let (command, code) = command(&mut b).await;
    let host = s.url.strip_prefix("http://").unwrap();
    assert_eq!(
        command,
        format!("irm http://{host}/admin/agent/install.ps1?code={code} | iex")
    );
    assert!(code.len() >= 6, "{code}");
}

#[tokio::test]
async fn registering_enters_token_and_address_of_the_pc_service() {
    let s = setup(false, |c| {
        c.strata_api_key = Some("strata-secret".into());
        c.pc_agent_url = None;
    })
    .await;
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    let resp = register(&s, &code, 8091).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let token = body["token"].as_str().unwrap();
    assert!(token.len() >= 32, "{token}");
    assert_eq!(body["nas_url"], s.url);
    assert_eq!(body["strata_api_key"], "strata-secret");

    let cfg = s.state.cfg();
    assert_eq!(cfg.pc_agent_token.as_deref(), Some(token));
    assert_eq!(cfg.pc_agent_url.as_deref(), Some("http://127.0.0.1:8091"));
    // Entered like values from the settings page: "Reset" drops them.
    s.state.settings.reset("PC_AGENT_TOKEN").unwrap();
    assert_eq!(s.state.cfg().pc_agent_token, None);
    assert!(kinds(&s.state).contains(&strata_nas_proxy::events::EventKind::PcServiceRegistered));
}

#[tokio::test]
async fn proxy_reaches_the_registered_pc_service() {
    let s = setup(true, |_| {}).await;
    let port = agent_port(&s);
    s.state.settings.reset("PC_AGENT_URL").ok();
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    assert_eq!(register(&s, &code, port).await.status(), 200);
    assert_eq!(
        s.state.cfg().pc_agent_url,
        Some(format!("http://127.0.0.1:{port}"))
    );
    s.pc.gaming.store(true, Ordering::SeqCst);
    let resp = post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()["x-strata-proxy-status"], "gaming");
}

#[tokio::test]
async fn a_code_works_once() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    assert_eq!(register(&s, &code, 8091).await.status(), 200);
    let token = s.state.cfg().pc_agent_token.clone();
    let resp = register(&s, &code, 8092).await;
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("new command"),
        "{body}"
    );
    assert_eq!(s.state.cfg().pc_agent_token, token);
    assert_eq!(
        s.state.cfg().pc_agent_url.as_deref(),
        Some("http://127.0.0.1:8091")
    );
}

#[tokio::test]
async fn a_new_command_replaces_the_old_code() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let (_, old) = command(&mut b).await;
    let (_, new) = command(&mut b).await;
    assert_ne!(old, new);
    assert_eq!(register(&s, &old, 8091).await.status(), 403);
    assert_eq!(register(&s, &new, 8091).await.status(), 200);
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let s = setup(false, |_| {}).await;
    s.state
        .pc_setup
        .set_code_lifetime(Duration::from_millis(100));
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(register(&s, &code, 8091).await.status(), 403);
    assert_eq!(s.state.cfg().pc_agent_token, None);
}

#[tokio::test]
async fn a_wrong_code_is_refused() {
    let s = setup(false, |_| {}).await;
    assert_eq!(register(&s, "WRONG", 8091).await.status(), 403);
}

#[tokio::test]
async fn registering_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    assert_eq!(register(&s, &code, 8091).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn setup_page_offers_the_command() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/pc").await;
    assert!(html.contains("Install on a PC"), "{html}");
    assert!(html.contains("Create install command"), "{html}");
    // The service card waits for the live overview.
    assert!(html.contains("PC service"), "{html}");
}

async fn script(s: &Setup, query: &str) -> (u16, String) {
    let resp = reqwest::get(format!("{}/admin/agent/install.ps1{query}", s.url))
        .await
        .unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

#[tokio::test]
async fn script_comes_with_nas_address_and_code() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let (_, code) = command(&mut b).await;
    let (status, text) = script(&s, &format!("?code={code}")).await;
    assert_eq!(status, 200);
    assert!(
        text.contains(&format!("[string]$NasUrl = \"{}\"", s.url)),
        "{text}"
    );
    assert!(
        text.contains(&format!("[string]$Code = \"{code}\"")),
        "{text}"
    );
    assert!(text.contains("[switch]$Uninstall,"), "{text}");
    // Loading the script leaves the code for the sign-up, also after the
    // script restarts itself with administrator rights.
    assert_eq!(script(&s, &format!("?code={code}")).await.0, 200);
    assert_eq!(register(&s, &code, 8091).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn script_without_a_valid_code_only_explains() {
    let s = setup(false, |_| {}).await;
    for query in ["", "?code=WRONG"] {
        let (status, text) = script(&s, query).await;
        // Windows PowerShell shows no body for errors, so the explanation
        // comes as a script that prints it.
        assert_eq!(status, 200, "{query}");
        assert!(text.contains("new command"), "{text}");
        assert!(!text.contains("Register-ScheduledTask"), "{text}");
    }
}

#[tokio::test]
async fn uninstall_script_needs_no_code() {
    let s = setup(false, |_| {}).await;
    let (status, text) = script(&s, "?uninstall").await;
    assert_eq!(status, 200);
    assert!(text.contains("[switch]$Uninstall = $true,"), "{text}");
    assert!(
        text.contains(&format!("[string]$NasUrl = \"{}\"", s.url)),
        "{text}"
    );
    assert!(text.contains("[string]$Code = \"\""), "{text}");
}
