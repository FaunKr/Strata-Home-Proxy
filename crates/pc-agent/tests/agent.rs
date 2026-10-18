//! Service tests with a fake PC and a fake Strata on localhost.

use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde_json::{Value, json};
use strata_pc_agent::{Agent, Config, Peer, Status, agent::router, platform::Platform};
use tokio::net::TcpListener;

#[derive(Default)]
struct Fake {
    idle: Mutex<Duration>,
    suspends: AtomicUsize,
    starts: AtomicUsize,
    stops: AtomicUsize,
    strata_up: AtomicBool,
    strata_busy: AtomicBool,
    mac: Mutex<Option<[u8; 6]>>,
    mac_lookups: AtomicUsize,
}

struct FakePlatform(Arc<Fake>);

impl Platform for FakePlatform {
    fn user_idle(&self) -> Duration {
        *self.0.idle.lock().unwrap()
    }
    fn suspend(&self) -> anyhow::Result<()> {
        self.0.suspends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn start_strata(&self, _: &Path, _: &[String]) -> anyhow::Result<()> {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        self.0.strata_up.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop_strata(&self, _: &[String]) -> anyhow::Result<()> {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        self.0.strata_up.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn mac_for(&self, _local: std::net::IpAddr) -> Option<[u8; 6]> {
        self.0.mac_lookups.fetch_add(1, Ordering::SeqCst);
        *self.0.mac.lock().unwrap()
    }
}

async fn health(State(f): State<Arc<Fake>>) -> StatusCode {
    if f.strata_up.load(Ordering::SeqCst) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn slots(State(f): State<Arc<Fake>>) -> Json<Value> {
    Json(json!([{ "id": 0, "is_processing": f.strata_busy.load(Ordering::SeqCst) }]))
}

async fn serve(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<Peer>(),
        )
        .await
        .unwrap()
    });
    addr
}

struct Setup {
    fake: Arc<Fake>,
    agent: Arc<Agent>,
    url: String,
    _dir: TempDir,
}

/// A throwaway folder for the gaming flag.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(strata: SocketAddr, dir: &Path) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: "geheim".into(),
        allowed_clients: vec!["192.168.1.10".parse().unwrap()],
        strata_url: format!("http://{strata}"),
        strata_api_key: None,
        strata_start: "START-HERE.bat".into(),
        strata_args: vec![],
        strata_processes: vec!["llama-server.exe".into()],
        user_idle_minutes: 10,
        auto_start: true,
        fresh_strata_after_standby: false,
        restart_after_secs: 0,
        load_time_secs: 1,
        state_dir: Some(dir.to_path_buf()),
        mac: None,
    }
}

async fn setup(name: &str) -> Setup {
    setup_with(name, |_| {}).await
}

async fn setup_with(name: &str, tweak: impl FnOnce(&mut Config)) -> Setup {
    let fake = Arc::new(Fake::default());
    fake.strata_up.store(true, Ordering::SeqCst);
    *fake.idle.lock().unwrap() = Duration::from_secs(3600);
    let strata = serve(
        Router::new()
            .route("/health", get(health))
            .route("/slots", get(slots))
            .with_state(fake.clone()),
    )
    .await;
    let dir = std::env::temp_dir().join(format!("strata-pc-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = config(strata, &dir);
    tweak(&mut cfg);
    let agent = Agent::new(cfg, Box::new(FakePlatform(fake.clone())));
    let addr = serve(router(agent.clone())).await;
    Setup {
        fake,
        agent,
        url: format!("http://{addr}"),
        _dir: TempDir(dir),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn sleep_answer(s: &Setup) -> (u16, Value) {
    let resp = client()
        .post(format!("{}/sleep", s.url))
        .bearer_auth("geheim")
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

#[tokio::test]
async fn token_is_required() {
    let s = setup("token").await;
    let resp = client()
        .get(format!("{}/status", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client()
        .get(format!("{}/status", s.url))
        .bearer_auth("falsch")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let status: Status = client()
        .get(format!("{}/status", s.url))
        .bearer_auth("geheim")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!status.gaming_mode);
    assert!(status.strata_running);
}

#[tokio::test]
async fn sleep_is_accepted_when_nothing_holds_it() {
    let s = setup("accept").await;
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 200);
    assert_eq!(body["accepted"], true);
    // The answer goes out first, standby follows a moment later.
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 0);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fresh_strata_after_standby_restarts_it_around_sleep() {
    let mut s = setup("fresh").await;
    let mut cfg = s.agent.cfg.clone();
    cfg.fresh_strata_after_standby = true;
    s.agent = Agent::new(cfg, Box::new(FakePlatform(s.fake.clone())));
    let addr = serve(router(s.agent.clone())).await;
    s.url = format!("http://{addr}");

    let (code, _) = sleep_answer(&s).await;
    assert_eq!(code, 200);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);

    // A resume the watchdog notices right after does not start a second copy.
    s.agent.resumed();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sleep_is_refused_while_someone_works() {
    let s = setup("user").await;
    *s.fake.idle.lock().unwrap() = Duration::from_secs(30);
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("arbeitet"));
}

#[tokio::test]
async fn sleep_is_refused_while_strata_works() {
    let s = setup("busy").await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("Strata"));
}

#[tokio::test]
async fn gaming_mode_stops_strata_blocks_sleep_and_survives_restart() {
    let s = setup("gaming").await;
    let status: Status = client()
        .post(format!("{}/gaming", s.url))
        .bearer_auth("geheim")
        .json(&json!({"enabled": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status.gaming_mode);
    assert!(!status.strata_running);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);

    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("Gaming"));

    // The watchdog leaves Strata off while gaming.
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);

    // A new service process (e.g. after a reboot) still knows gaming is on.
    let again = Agent::new(s.agent.cfg.clone(), Box::new(FakePlatform(s.fake.clone())));
    assert!(again.gaming());

    s.agent.set_gaming(false);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert!(s.fake.strata_up.load(Ordering::SeqCst));
    let again = Agent::new(s.agent.cfg.clone(), Box::new(FakePlatform(s.fake.clone())));
    assert!(!again.gaming());
}

#[tokio::test]
async fn watchdog_restarts_unresponsive_strata_after_load_time() {
    let s = setup("watchdog").await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    // First pass only notes the failure; the next one (restart_after_secs = 0) restarts.
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);

    // While the model loads (load_time_secs = 1) there is no second restart.
    s.fake.strata_up.store(false, Ordering::SeqCst);
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn healthy_strata_is_left_alone() {
    let s = setup("healthy").await;
    for _ in 0..3 {
        s.agent.watchdog_tick().await;
    }
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

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
async fn status_reports_the_mac_of_the_network_card() {
    let s = setup("mac").await;
    *s.fake.mac.lock().unwrap() = Some([0xaa, 0xbb, 0xcc, 1, 2, 3]);
    assert_eq!(status_json(&s).await["mac"], "AA:BB:CC:01:02:03");
    // Looked up once, then reused.
    assert_eq!(status_json(&s).await["mac"], "AA:BB:CC:01:02:03");
    assert_eq!(s.fake.mac_lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn status_leaves_out_an_unknown_mac() {
    let s = setup("no-mac").await;
    let status = status_json(&s).await;
    assert!(status.get("mac").is_none(), "{status}");
    assert_eq!(status["gaming_mode"], false);
}

#[tokio::test]
async fn configured_mac_replaces_the_found_one() {
    let s = setup_with("mac-config", |c| c.mac = Some("11-22-33-44-55-66".into())).await;
    *s.fake.mac.lock().unwrap() = Some([9; 6]);
    assert_eq!(status_json(&s).await["mac"], "11:22:33:44:55:66");
    assert_eq!(s.fake.mac_lookups.load(Ordering::SeqCst), 0);
}
