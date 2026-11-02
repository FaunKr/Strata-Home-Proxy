//! The metrics endpoint: `GET /metrics` and `GET /metrics/prometheus` in
//! Prometheus' text format (docs/metrics.md). The proxy answers them itself,
//! never forwards them, never wakes the PC and never counts them as
//! activity: Prometheus scrapes every few seconds. Strata's own lines come
//! along while Strata is up, and its two tool-call counters always, at the
//! last value read.

use std::{collections::BTreeMap, fmt::Write as _, sync::Arc};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};

use strata_pc_protocol::{Gpu, Hardware};

use crate::{
    expert_tiers::{self, Tier},
    lifecycle::PcState,
    proxy::{self, Flavor},
    public_name,
    request_totals::{self, Outcome, Total},
    state::AppState,
};

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/metrics", get(scrape))
        .route("/metrics/prometheus", get(scrape))
}

async fn scrape(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(checked) = proxy::client_keys(&headers).find_map(|key| st.api_keys.check(key)) else {
        return proxy::unauthorized(Flavor::OpenAi);
    };
    let hiding = {
        let cfg = st.cfg();
        checked.model_name.hides(cfg.hide_model).then(|| Hiding {
            public: cfg.public_model_name.clone(),
            images: st.models.image_models(),
        })
    };
    let scrape_data = Scrape::read(&st).await;
    let mut resp = render(&scrape_data, hiding.as_ref()).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
    resp
}

/// The public model name a key that hides the model sees, and the image
/// models that keep their names.
pub struct Hiding {
    pub public: String,
    pub images: Vec<String>,
}

impl Hiding {
    /// A model label's value for this key: empty stays empty.
    fn model(&self, model: &str) -> String {
        if model.is_empty() || self.images.iter().any(|i| i == model) {
            model.to_string()
        } else {
            self.public.clone()
        }
    }
}

/// What one scrape shows, read without waking the PC.
#[derive(Clone, Debug, Default)]
pub struct Scrape {
    pub version: String,
    pub pc: Option<PcState>,
    pub tuning: bool,
    pub strata_up: bool,
    /// The model the PC service's last report marks as active.
    pub active_model: Option<String>,
    /// `None` without a power plug or without a latest reading.
    pub power_watts: Option<f64>,
    pub totals: Vec<Total>,
    /// Request energy per model in Wh; empty without a power plug.
    pub request_energy_wh: Vec<(String, f64)>,
    /// Output tokens per expert tier and model, over all days.
    pub expert_tokens: BTreeMap<String, expert_tiers::TierTokens>,
    /// `None` without a power plug.
    pub pc_energy_joules: Option<f64>,
    pub wake_packets: u64,
    /// The PC service's last hardware readings, while the PC is awake or
    /// gaming and they are recent.
    pub hardware: Option<Hardware>,
    /// Strata's tool-call counters: the last values read.
    pub tool_calls: ToolCalls,
    /// Strata's Prometheus text while it is up and answers.
    pub strata: Option<String>,
}

const EXPERT_TOKENS_HELP: &str = "Output tokens by where their routed experts ran: \
     the GPU's expert cache (vram), copied over PCIe (pcie), computed by the CPU (cpu).";

const UNREAD: &str = "strata:tool_calls_unread_total";
const UNREAD_HELP: &str =
    "<tool_call> blocks returned as text (an undeclared name or a form the template does not use)";
const REPAIRED: &str = "strata:tool_args_repaired_total";
const REPAIRED_HELP: &str =
    "Tool call arguments (a JSON array or object) that were damaged and read as written";

/// Strata's two tool-call counters, which it prints only on
/// `/metrics/prometheus`: 0 until the proxy read a value since its start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolCalls {
    pub unread: u64,
    pub repaired: u64,
}

impl ToolCalls {
    /// These counters with each one `text` prints replaced by its value
    /// there, also a lower one; one it does not print keeps its value.
    pub fn updated_from(self, text: &str) -> Self {
        Self {
            unread: sample(text, UNREAD).unwrap_or(self.unread),
            repaired: sample(text, REPAIRED).unwrap_or(self.repaired),
        }
    }
}

/// The value of the sample line of `name` without labels in `text`.
fn sample(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let value: f64 = line
            .strip_prefix(name)?
            .strip_prefix(' ')?
            .trim()
            .parse()
            .ok()?;
        (value.is_finite() && value >= 0.0).then_some(value as u64)
    })
}

impl Scrape {
    async fn read(st: &Arc<AppState>) -> Self {
        let cfg = st.cfg();
        let plug = cfg.shelly_url.is_some();
        let pc = st.pc_state();
        let tuning = st.tuning();
        let strata_up = match pc {
            PcState::Asleep | PcState::Waking => false,
            _ if tuning || st.strata().is_none() => false,
            _ => st.strata_ready().await,
        };
        let (strata, llamacpp_text) = match (strata_up, st.strata()) {
            (true, Some(client)) => {
                let (text, llamacpp_text) =
                    tokio::join!(client.metrics_text(), client.metrics_prometheus_text());
                (text.ok(), llamacpp_text.ok())
            }
            _ => (None, None),
        };
        let tool_calls = st.note_tool_calls(llamacpp_text.as_deref());
        let db = st.events.database();
        let (totals, request_energy_wh, expert_tokens) = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let totals = request_totals::totals(&db).unwrap_or_else(|e| {
                tracing::warn!("request totals not read: {e}");
                Vec::new()
            });
            let energy = crate::request_energy::all_time_wh(&db).unwrap_or_else(|e| {
                tracing::warn!("request energy not read: {e}");
                Vec::new()
            });
            let experts = expert_tiers::totals(&db).unwrap_or_else(|e| {
                tracing::warn!("expert tiers not read: {e}");
                BTreeMap::new()
            });
            (totals, energy, experts)
        })
        .await
        .unwrap_or_default();
        let pc_energy_joules = if plug {
            Some(st.events.pc_energy(cfg.plug_sample.as_secs()).await)
        } else {
            None
        };
        // Two idle checks: one missed answer does not drop the lines.
        let hardware = match pc {
            PcState::Awake | PcState::Gaming => st.hardware_within(cfg.idle_check * 2),
            _ => None,
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pc: Some(pc),
            tuning,
            strata_up,
            active_model: st
                .strata
                .last_info()
                .and_then(|info| crate::request_energy::active(&info).map(str::to_string)),
            power_watts: st.plug_latest().filter(|_| plug),
            totals,
            request_energy_wh: if plug { request_energy_wh } else { Vec::new() },
            expert_tokens,
            pc_energy_joules,
            wake_packets: st.events.wake_packets().await,
            hardware,
            tool_calls,
            strata,
        }
    }
}

/// One metric family: its lines under one `# HELP` and `# TYPE`.
struct Family<'a> {
    out: &'a mut String,
}

impl<'a> Family<'a> {
    fn new(out: &'a mut String, name: &str, kind: &str, help: &str) -> Self {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} {kind}");
        Self { out }
    }

    fn line(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.out.push_str(name);
        let set: Vec<String> = labels
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k}=\"{}\"", public_name::escape_label(v)))
            .collect();
        if !set.is_empty() {
            let _ = write!(self.out, "{{{}}}", set.join(","));
        }
        let _ = writeln!(self.out, " {value}");
    }
}

const PC_STATES: [(PcState, &str); 5] = [
    (PcState::Unknown, "unknown"),
    (PcState::Asleep, "asleep"),
    (PcState::Waking, "waking"),
    (PcState::Awake, "awake"),
    (PcState::Gaming, "gaming"),
];

/// One hardware gauge: its name, help and the reading it shows, if known.
struct HardwareGauge {
    name: &'static str,
    help: &'static str,
    of: fn(&Hardware) -> Option<f64>,
}

/// An `f32` reading as written, not as its nearest `f64` (`182.3`, not
/// `182.3000030517578`).
fn wide(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

fn gpu(h: &Hardware) -> Option<&Gpu> {
    h.gpu.as_ref()
}

const HARDWARE: [HardwareGauge; 10] = [
    HardwareGauge {
        name: "strata_proxy_gpu_load_ratio",
        help: "The GPU's load, 0 to 1.",
        of: |h| gpu(h)?.load_pct.map(|p| wide(p) / 100.0),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_temperature_celsius",
        help: "The GPU's temperature.",
        of: |h| gpu(h)?.temp_c.map(wide),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_vram_used_bytes",
        help: "VRAM in use.",
        of: |h| gpu(h)?.vram_used_bytes.map(|b| b as f64),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_vram_total_bytes",
        help: "VRAM in total.",
        of: |h| gpu(h)?.vram_total_bytes.map(|b| b as f64),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_fan_rpm",
        help: "The GPU's fan speed; 0 while the fan stands still.",
        of: |h| gpu(h)?.fan_rpm.map(f64::from),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_power_ratio",
        help: "The GPU's power as a share of its power limit, 0 to 1.",
        of: |h| gpu(h)?.power_pct.map(|p| wide(p) / 100.0),
    },
    HardwareGauge {
        name: "strata_proxy_gpu_power_watts",
        help: "The GPU chip's power (AMD's ASIC power).",
        of: |h| gpu(h)?.power_watts.map(wide),
    },
    HardwareGauge {
        name: "strata_proxy_cpu_load_ratio",
        help: "The CPU's load, 0 to 1.",
        of: |h| h.cpu.load_pct.map(|p| wide(p) / 100.0),
    },
    HardwareGauge {
        name: "strata_proxy_ram_used_bytes",
        help: "RAM in use.",
        of: |h| h.cpu.ram_used_bytes.map(|b| b as f64),
    },
    HardwareGauge {
        name: "strata_proxy_ram_total_bytes",
        help: "RAM in total.",
        of: |h| h.cpu.ram_total_bytes.map(|b| b as f64),
    },
];

/// One line per known hardware reading.
fn render_hardware(out: &mut String, hardware: &Hardware) {
    for gauge in HARDWARE {
        if let Some(value) = (gauge.of)(hardware) {
            Family::new(out, gauge.name, "gauge", gauge.help).line(gauge.name, &[], value);
        }
    }
}

/// One token counter: its name, help and the sum it shows.
struct TokenCounter {
    name: &'static str,
    help: &'static str,
    of: fn(&Total) -> u64,
}

const TOKENS: [TokenCounter; 3] = [
    TokenCounter {
        name: "strata_proxy_prompt_tokens_total",
        help: "Prompt tokens of finished requests.",
        of: |t| t.prompt_tokens,
    },
    TokenCounter {
        name: "strata_proxy_cached_tokens_total",
        help: "The part of the prompt tokens Strata held already.",
        of: |t| t.cached_tokens,
    },
    TokenCounter {
        name: "strata_proxy_output_tokens_total",
        help: "Output tokens of finished requests.",
        of: |t| t.output_tokens,
    },
];

/// The answer to a scrape: the proxy's own lines, then Strata's.
pub fn render(s: &Scrape, hiding: Option<&Hiding>) -> String {
    let model = |m: &str| hiding.map_or_else(|| m.to_string(), |h| h.model(m));
    let mut out = String::new();

    let name = "strata_proxy_info";
    Family::new(&mut out, name, "gauge", "The proxy's product version.").line(
        name,
        &[("version", &s.version)],
        1.0,
    );

    let name = "strata_proxy_pc_state";
    let mut f = Family::new(&mut out, name, "gauge", "1 for the PC's current state.");
    for (state, label) in PC_STATES {
        f.line(
            name,
            &[("state", label)],
            f64::from(u8::from(s.pc == Some(state))),
        );
    }

    let name = "strata_proxy_tuning";
    Family::new(&mut out, name, "gauge", "1 while a tune runs on the PC.").line(
        name,
        &[],
        f64::from(u8::from(s.tuning)),
    );

    let name = "strata_proxy_strata_up";
    let active = s
        .active_model
        .as_deref()
        .filter(|_| s.strata_up)
        .map(model)
        .unwrap_or_default();
    Family::new(&mut out, name, "gauge", "1 while Strata answers.").line(
        name,
        &[("model", &active)],
        f64::from(u8::from(s.strata_up)),
    );

    if let Some(watts) = s.power_watts {
        let name = "strata_proxy_power_watts";
        Family::new(&mut out, name, "gauge", "The power plug's latest reading.").line(
            name,
            &[],
            watts,
        );
    }

    if let Some(hardware) = &s.hardware {
        render_hardware(&mut out, hardware);
    }

    // Series that share all labels for this key add up into one line.
    let mut requests: BTreeMap<(String, String, Outcome), u64> = BTreeMap::new();
    for t in &s.totals {
        *requests
            .entry((t.key.clone(), model(&t.model), t.outcome))
            .or_default() += t.requests;
    }
    if !requests.is_empty() {
        let name = "strata_proxy_requests_total";
        let mut f = Family::new(
            &mut out,
            name,
            "counter",
            "Finished requests through the proxy.",
        );
        for ((key, m, outcome), n) in &requests {
            f.line(
                name,
                &[("key", key), ("model", m), ("outcome", outcome.label())],
                *n as f64,
            );
        }
        for counter in TOKENS {
            let mut sums: BTreeMap<(String, String), u64> = BTreeMap::new();
            for t in &s.totals {
                *sums.entry((t.key.clone(), model(&t.model))).or_default() += (counter.of)(t);
            }
            let mut f = Family::new(&mut out, counter.name, "counter", counter.help);
            for ((key, m), n) in &sums {
                f.line(counter.name, &[("key", key), ("model", m)], *n as f64);
            }
        }
    }

    let mut energy: BTreeMap<String, f64> = BTreeMap::new();
    for (m, wh) in &s.request_energy_wh {
        *energy.entry(model(m)).or_default() += wh * 3600.0;
    }
    if !energy.is_empty() {
        let name = "strata_proxy_request_energy_joules_total";
        let mut f = Family::new(&mut out, name, "counter", "Request energy per model.");
        for (m, joules) in &energy {
            f.line(name, &[("model", m)], *joules);
        }
    }

    let mut experts: BTreeMap<String, expert_tiers::TierTokens> = BTreeMap::new();
    for (m, tokens) in &s.expert_tokens {
        experts.entry(model(m)).or_default().add(tokens);
    }
    if !experts.is_empty() {
        let name = "strata_proxy_expert_tokens_total";
        let mut f = Family::new(&mut out, name, "counter", EXPERT_TOKENS_HELP);
        for (m, tokens) in &experts {
            for tier in Tier::ALL {
                f.line(
                    name,
                    &[("model", m), ("tier", tier.label())],
                    tokens.get(tier),
                );
            }
        }
    }

    if let Some(joules) = s.pc_energy_joules {
        let name = "strata_proxy_pc_energy_joules_total";
        Family::new(
            &mut out,
            name,
            "counter",
            "The PC's energy from the power plug's readings.",
        )
        .line(name, &[], joules);
    }

    let name = "strata_proxy_wake_packets_total";
    Family::new(&mut out, name, "counter", "Wake packets the proxy sent.").line(
        name,
        &[],
        s.wake_packets as f64,
    );

    for (name, help, value) in [
        (UNREAD, UNREAD_HELP, s.tool_calls.unread),
        (REPAIRED, REPAIRED_HELP, s.tool_calls.repaired),
    ] {
        Family::new(&mut out, name, "counter", help).line(name, &[], value as f64);
    }

    if let Some(text) = &s.strata {
        match hiding {
            Some(h) => out.push_str(&public_name::prometheus(&h.public, &h.images, text)),
            None => out.push_str(text),
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}
