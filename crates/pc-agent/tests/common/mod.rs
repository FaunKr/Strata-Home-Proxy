//! A fake PC and a fake Strata on localhost for the service tests.
#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing,
};
use serde_json::{Value, json};
#[allow(unused_imports)]
pub use strata_pc_agent::{
    Agent, Config, PcStatus, Peer,
    agent::router,
    command::{CommandSpec, Running},
    hardware::{Cpu, Gpu, Hardware},
    install::testing::SetupLeaves,
    platform::Platform,
};
use tokio::{net::TcpListener, sync::mpsc, sync::oneshot};

/// How a fake command behaves.
#[derive(Clone, Default)]
pub struct FakeCommand {
    pub lines: Vec<String>,
    /// Pause before each line.
    pub pause: Duration,
    /// Exit code; `None` means 0.
    pub exit: Option<i32>,
    /// Keeps running after its lines until cancelled.
    pub hang: bool,
    /// Keeps running even when cancelled, like a stuck process.
    pub ignores_cancel: bool,
    /// Files (path, content) it leaves behind when it succeeds, unless they exist.
    pub creates: Vec<(PathBuf, String)>,
}

#[derive(Default)]
pub struct Fake {
    pub idle: Mutex<Duration>,
    pub suspends: AtomicUsize,
    pub starts: AtomicUsize,
    pub stops: AtomicUsize,
    pub strata_up: AtomicBool,
    /// A started Strata does not answer `/health`.
    pub strata_stays_down: AtomicBool,
    /// Starting Strata fails.
    pub start_fails: AtomicBool,
    pub strata_busy: AtomicBool,
    pub mac: Mutex<Option<[u8; 6]>>,
    pub mac_lookups: AtomicUsize,
    /// Program and arguments of the last start.
    pub started: Mutex<Option<(PathBuf, Vec<String>)>>,
    /// What the last stop was told to end.
    pub stopped: Mutex<Option<Vec<String>>>,
    /// Fake commands by program name without `.exe` (`git`, `START-HERE.bat`);
    /// `git <subcommand>` answers that subcommand only, ahead of `git`.
    pub commands: Mutex<HashMap<String, FakeCommand>>,
    /// Every command run, in order.
    pub ran: Mutex<Vec<CommandSpec>>,
    pub cancels: AtomicUsize,
    /// Git is not installed until winget installs it.
    pub git_missing: AtomicBool,
    pub ram: AtomicU64,
    pub disk_free: Mutex<Option<u64>>,
    /// Ends the fake Strata started last, as stopping it ends its output.
    pub strata_cancel: Mutex<Option<strata_pc_agent::command::Cancel>>,
    /// The Authorization header of the last `/health` call.
    pub health_auth: Mutex<Option<String>>,
    /// Content of the program the last update put in place.
    pub replaced: Mutex<Option<Vec<u8>>>,
    /// Putting the new program in place fails with this.
    pub replace_fails: Mutex<Option<String>>,
    /// Times the service ended itself after an update.
    pub exits: AtomicUsize,
    /// What reading the hardware gives.
    pub hardware: Mutex<Hardware>,
}

impl Fake {
    pub fn command(&self, program: &str, command: FakeCommand) {
        self.commands
            .lock()
            .unwrap()
            .insert(program.to_string(), command);
    }

    /// The runs of `program`, by file name.
    pub fn runs_of(&self, program: &str) -> Vec<CommandSpec> {
        self.ran
            .lock()
            .unwrap()
            .iter()
            .filter(|c| file_name(&c.program) == program)
            .cloned()
            .collect()
    }
}

/// The program's name without `.exe`: under Windows the service may run
/// `git.exe` by its full path where elsewhere it runs `git`.
fn file_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".exe").map_or(name.clone(), String::from)
}

pub struct FakePlatform(pub Arc<Fake>);

impl FakePlatform {
    /// The fake command `name` as a running command.
    fn fake_run(&self, name: &str) -> Running {
        let command = self
            .0
            .commands
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default();
        let (tx, lines) = mpsc::unbounded_channel();
        let (exit_tx, exit) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        tokio::spawn(async move {
            let stopped = || flag.load(Ordering::SeqCst);
            let wait = |d: Duration| {
                let flag = Arc::clone(&flag);
                async move {
                    let mut left = d;
                    while !left.is_zero() && !flag.load(Ordering::SeqCst) {
                        let step = left.min(Duration::from_millis(10));
                        tokio::time::sleep(step).await;
                        left -= step;
                    }
                }
            };
            for line in command.lines {
                wait(command.pause).await;
                if stopped() {
                    break;
                }
                let _ = tx.send(line);
            }
            while command.ignores_cancel || (command.hang && !stopped()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            drop(tx);
            if stopped() {
                let _ = exit_tx.send(None);
                return;
            }
            let code = command.exit.unwrap_or(0);
            if code == 0 {
                for (path, content) in command.creates {
                    // Another run of the same program leaves what is there.
                    if path.exists() {
                        continue;
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).unwrap();
                    }
                    std::fs::write(path, content).unwrap();
                }
            }
            let _ = exit_tx.send(Some(code));
        });
        let counter = Arc::clone(&self.0);
        Running::new(
            lines,
            exit,
            Arc::new(move || {
                if !cancelled.swap(true, Ordering::SeqCst) {
                    counter.cancels.fetch_add(1, Ordering::SeqCst);
                }
            }),
        )
    }
}

impl Platform for FakePlatform {
    fn user_idle(&self) -> Duration {
        *self.0.idle.lock().unwrap()
    }
    fn suspend(&self) -> anyhow::Result<()> {
        self.0.suspends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    /// Runs the fake command named like the start script for Strata's output.
    fn start_strata(&self, program: &Path, args: &[String]) -> anyhow::Result<Running> {
        if self.0.start_fails.load(Ordering::SeqCst) {
            anyhow::bail!("{} not found", program.display());
        }
        *self.0.started.lock().unwrap() = Some((program.to_path_buf(), args.to_vec()));
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        if !self.0.strata_stays_down.load(Ordering::SeqCst) {
            self.0.strata_up.store(true, Ordering::SeqCst);
        }
        let run = self.fake_run(&file_name(program));
        *self.0.strata_cancel.lock().unwrap() = Some(run.canceller());
        Ok(run)
    }
    fn stop_strata(&self, processes: &[String]) -> anyhow::Result<()> {
        *self.0.stopped.lock().unwrap() = Some(processes.to_vec());
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        self.0.strata_up.store(false, Ordering::SeqCst);
        if let Some(cancel) = self.0.strata_cancel.lock().unwrap().take() {
            cancel();
        }
        Ok(())
    }
    fn run(&self, spec: &CommandSpec) -> anyhow::Result<Running> {
        let fake = Arc::clone(&self.0);
        fake.ran.lock().unwrap().push(spec.clone());
        let name = file_name(&spec.program);
        if name == "git" && fake.git_missing.load(Ordering::SeqCst) {
            anyhow::bail!("git nicht gefunden");
        }
        if name == "winget" {
            fake.git_missing.store(false, Ordering::SeqCst);
        }
        if name == "git"
            && let Some(sub) = spec.args.first().map(|a| format!("git {a}"))
            && fake.commands.lock().unwrap().contains_key(&sub)
        {
            return Ok(self.fake_run(&sub));
        }
        Ok(self.fake_run(&name))
    }
    fn ram_bytes(&self) -> u64 {
        self.0.ram.load(Ordering::SeqCst)
    }
    fn disk_free(&self, _path: &Path) -> Option<u64> {
        *self.0.disk_free.lock().unwrap()
    }
    fn mac_for(&self, _local: std::net::IpAddr) -> Option<[u8; 6]> {
        self.0.mac_lookups.fetch_add(1, Ordering::SeqCst);
        *self.0.mac.lock().unwrap()
    }
    fn replace_program(&self, downloaded: &Path) -> anyhow::Result<()> {
        if let Some(e) = self.0.replace_fails.lock().unwrap().clone() {
            anyhow::bail!(e);
        }
        *self.0.replaced.lock().unwrap() = Some(std::fs::read(downloaded)?);
        std::fs::remove_file(downloaded)?;
        Ok(())
    }
    fn exit(&self) {
        self.0.exits.fetch_add(1, Ordering::SeqCst);
    }
    fn hardware(&self) -> Hardware {
        self.0.hardware.lock().unwrap().clone()
    }
}

async fn health(State(f): State<Arc<Fake>>, headers: HeaderMap) -> StatusCode {
    *f.health_auth.lock().unwrap() = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if f.strata_up.load(Ordering::SeqCst) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn slots(State(f): State<Arc<Fake>>) -> Json<Value> {
    Json(json!([{ "id": 0, "is_processing": f.strata_busy.load(Ordering::SeqCst) }]))
}

pub async fn serve(router: Router) -> SocketAddr {
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

pub struct Setup {
    pub fake: Arc<Fake>,
    pub agent: Arc<Agent>,
    pub url: String,
    pub dir: PathBuf,
    _dir: TempDir,
}

impl Setup {
    /// A new service process on the same folders, as after a restart.
    pub fn restarted(&self) -> Arc<Agent> {
        Agent::new(
            self.agent.cfg.clone(),
            Box::new(FakePlatform(self.fake.clone())),
        )
    }
}

/// A throwaway folder for the gaming flag and Strata.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn config(strata: SocketAddr, dir: &Path) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: "geheim".into(),
        allowed_clients: vec!["192.168.1.10".parse().unwrap()],
        strata_url: format!("http://{strata}"),
        strata_api_key: None,
        strata_start: Some("START-HERE.bat".into()),
        strata_args: vec![],
        strata_processes: vec!["llama-server.exe".into()],
        user_idle_minutes: 10,
        auto_start: true,
        fresh_strata_after_standby: false,
        restart_after_secs: 0,
        load_time_secs: 1,
        context_start_secs: 1,
        stop_settle_ms: 10,
        cancel_wait_secs: 30,
        state_dir: Some(dir.to_path_buf()),
        mac: None,
        nas_url: None,
        strata_repo: "https://example.org/Strata.git".into(),
        strata_dir: dir.join("Strata"),
        strata_data_dir: dir.join("Strata-data"),
    }
}

pub async fn setup(name: &str) -> Setup {
    setup_with(name, |_| {}).await
}

pub async fn setup_with(name: &str, tweak: impl FnOnce(&mut Config)) -> Setup {
    let fake = Arc::new(Fake::default());
    fake.strata_up.store(true, Ordering::SeqCst);
    *fake.idle.lock().unwrap() = Duration::from_secs(3600);
    fake.ram.store(64 << 30, Ordering::SeqCst);
    *fake.disk_free.lock().unwrap() = Some(500_000_000_000);
    let strata = serve(
        Router::new()
            .route("/health", routing::get(health))
            .route("/slots", routing::get(slots))
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
        dir: dir.clone(),
        _dir: TempDir(dir),
    }
}

pub fn client() -> reqwest::Client {
    reqwest::Client::new()
}

pub async fn sleep_answer(s: &Setup) -> (u16, Value) {
    let resp = client()
        .post(format!("{}/sleep", s.url))
        .bearer_auth("geheim")
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

pub async fn post(s: &Setup, path: &str, body: Value) -> (u16, Value) {
    let resp = client()
        .post(format!("{}{path}", s.url))
        .bearer_auth("geheim")
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.json().await.unwrap_or(Value::Null),
    )
}

pub async fn get(s: &Setup, path: &str) -> Value {
    client()
        .get(format!("{}{path}", s.url))
        .bearer_auth("geheim")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The job once `done` holds for it.
pub async fn job_until(s: &Setup, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..200 {
        last = get(s, "/strata/job").await;
        if done(&last) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what} nicht erreicht, zuletzt: {last}");
}

pub async fn finished(s: &Setup) -> Value {
    job_until(s, "Ende des Vorgangs", |j| j["state"] != "running").await
}

/// The service's Strata-Ordner, to add to or look at.
pub fn strata_ordner(s: &Setup) -> SetupLeaves {
    SetupLeaves::at(&s.agent.cfg.strata_dir, &s.agent.cfg.strata_data_dir)
}

/// A model config's fields, read through the service's own module.
pub fn read_config(s: &Setup, id: &str) -> Value {
    let model = strata_ordner(s).installation().model(id).unwrap();
    Value::Object(model.fields().clone())
}
