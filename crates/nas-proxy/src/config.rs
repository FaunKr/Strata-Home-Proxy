//! Configuration: built-in defaults, overridden by environment variables. The
//! settings page can override each value again (see `settings`).

use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct Config {
    /// Address the API proxy listens on (`LISTEN_ADDR`).
    pub listen_addr: SocketAddr,
    /// Base URL of Strata on the PC, e.g. `http://192.168.1.50:8080` (`STRATA_URL`).
    /// Without it the proxy is not set up yet and answers API requests with 503.
    pub strata_url: Option<String>,
    /// MAC address of the PC's network card (`PC_MAC`). Without it the PC
    /// cannot be woken.
    pub pc_mac: Option<[u8; 6]>,
    /// Where the magic packet is sent (`WOL_ADDR`), usually the LAN broadcast on port 9.
    pub wol_addr: SocketAddr,
    /// Base URL of the PC service (`PC_AGENT_URL`). Without it the proxy cannot
    /// detect gaming mode and never puts the PC to sleep.
    pub pc_agent_url: Option<String>,
    /// Bearer token for the PC service (`PC_AGENT_TOKEN`).
    pub pc_agent_token: Option<String>,
    /// Base URL of the power plug the PC is plugged into, a Shelly plug
    /// (`SHELLY_URL`). Without it the overview shows no power.
    pub shelly_url: Option<String>,
    /// Base URL of the code benchmark's runner, the container `code-bench`
    /// (`CODE_BENCH_URL`), e.g. `http://127.0.0.1:8096`. Without it there is
    /// no code benchmark.
    pub code_bench_url: Option<String>,
    /// How long Strata may take for the update summary
    /// (`UPDATE_SUMMARY_TIMEOUT_SECS`); without it the suggestion from
    /// `timeouts` applies.
    pub update_summary_timeout: Option<Duration>,
    /// How long opencode may work on one code benchmark task
    /// (`CODE_BENCH_TASK_TIMEOUT_SECS`); without it the suggestion applies.
    pub code_bench_task_timeout: Option<Duration>,
    /// How often the power of the power plug is recorded for the history
    /// (`PLUG_SAMPLE_SECS`).
    pub plug_sample: Duration,
    /// Time without requests before the PC is asked to sleep (`IDLE_TIMEOUT_SECS`).
    pub idle_timeout: Duration,
    /// Longest time a request waits for the PC to wake up (`WAKE_TIMEOUT_SECS`).
    pub wake_timeout: Duration,
    /// How often the magic packet is repeated while waiting (`WOL_RESEND_SECS`).
    pub wol_resend: Duration,
    /// How often `/health` is polled while waiting (`HEALTH_POLL_MS`).
    pub health_poll: Duration,
    /// Silence on a stream after which a ping event is sent (`PING_INTERVAL_SECS`).
    pub ping_interval: Duration,
    /// How often the idle check runs (`IDLE_CHECK_SECS`).
    pub idle_check: Duration,
    /// Wait after the PC refused to sleep before asking again (`SLEEP_RETRY_SECS`).
    pub sleep_retry: Duration,
    /// After an accepted sleep request, Strata is treated as gone for this long
    /// even if it still answers, so no request lands on a PC that is shutting down
    /// (`SLEEP_SETTLE_SECS`).
    pub sleep_settle: Duration,
    /// Timeout for `/health` and PC service calls (`PROBE_TIMEOUT_MS`).
    pub probe_timeout: Duration,
    /// How long a forced sleep or forced Strata restart may take on the PC
    /// service. Above its `cancel_wait_secs` (60 s) plus the pause after
    /// stopping Strata, so a slow but sound answer still arrives.
    pub force_timeout: Duration,
    /// Largest accepted request body in bytes (`MAX_BODY_BYTES`).
    pub max_body_bytes: usize,
    /// Watts the PC draws while awake (`POWER_AWAKE_WATTS`). The history
    /// estimates energy from it when no power plug measures the PC.
    pub power_awake_watts: f64,
    /// Watts the PC draws while asleep (`POWER_ASLEEP_WATTS`), for the same
    /// estimate.
    pub power_asleep_watts: f64,
    /// Electricity price in cents per kWh (`PRICE_CT_PER_KWH`), to turn energy
    /// into cost.
    pub price_ct_per_kwh: f64,
    /// Directory for the event history and saved settings (`DATA_DIR`); in
    /// memory only if unset. Not changeable on the settings page, since the
    /// settings themselves live there.
    pub data_dir: Option<PathBuf>,
    /// Replaces the default password `admin` of the web UI (`UI_PASSWORD`).
    /// Not on the settings page; the password is changed there instead.
    pub ui_password: Option<String>,
}

/// A value that can be set by environment variable and on the settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: &'static str,
    /// Takes effect only after a restart.
    pub restart: bool,
    /// Never shown or sent back in clear text.
    pub secret: bool,
    /// Can be left empty.
    pub optional: bool,
}

const fn field(key: &'static str) -> Field {
    Field {
        key,
        restart: false,
        secret: false,
        optional: false,
    }
}

const fn optional(key: &'static str) -> Field {
    Field {
        optional: true,
        ..field(key)
    }
}

/// Every setting except `DATA_DIR`, in the order the settings page shows them.
pub const FIELDS: &[Field] = &[
    optional("STRATA_URL"),
    optional("PC_MAC"),
    field("WOL_ADDR"),
    optional("PC_AGENT_URL"),
    Field {
        secret: true,
        ..optional("PC_AGENT_TOKEN")
    },
    optional("SHELLY_URL"),
    optional("CODE_BENCH_URL"),
    optional("UPDATE_SUMMARY_TIMEOUT_SECS"),
    optional("CODE_BENCH_TASK_TIMEOUT_SECS"),
    field("PLUG_SAMPLE_SECS"),
    field("IDLE_TIMEOUT_SECS"),
    field("WAKE_TIMEOUT_SECS"),
    field("WOL_RESEND_SECS"),
    field("HEALTH_POLL_MS"),
    field("PING_INTERVAL_SECS"),
    field("IDLE_CHECK_SECS"),
    field("SLEEP_RETRY_SECS"),
    field("SLEEP_SETTLE_SECS"),
    field("PROBE_TIMEOUT_MS"),
    field("MAX_BODY_BYTES"),
    field("POWER_AWAKE_WATTS"),
    field("POWER_ASLEEP_WATTS"),
    field("PRICE_CT_PER_KWH"),
    Field {
        restart: true,
        ..field("LISTEN_ADDR")
    },
];

pub fn find_field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".parse().unwrap(),
            strata_url: None,
            pc_mac: None,
            wol_addr: "255.255.255.255:9".parse().unwrap(),
            pc_agent_url: None,
            pc_agent_token: None,
            shelly_url: None,
            code_bench_url: None,
            update_summary_timeout: None,
            code_bench_task_timeout: None,
            plug_sample: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(900),
            wake_timeout: Duration::from_secs(240),
            wol_resend: Duration::from_secs(10),
            health_poll: Duration::from_millis(1000),
            ping_interval: Duration::from_secs(15),
            idle_check: Duration::from_secs(15),
            sleep_retry: Duration::from_secs(60),
            sleep_settle: Duration::from_secs(30),
            probe_timeout: Duration::from_millis(2000),
            force_timeout: Duration::from_secs(90),
            max_body_bytes: 64 * 1024 * 1024,
            power_awake_watts: 75.0,
            power_asleep_watts: 3.0,
            price_ct_per_kwh: 30.0,
            data_dir: None,
            ui_password: None,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let mut cfg = Self::default();
        for f in FIELDS {
            if let Some(raw) = env_value(f.key) {
                cfg.apply(f.key, &raw)
                    .map_err(|e| anyhow::anyhow!("{}={raw:?} is invalid: {e}", f.key))?;
            }
        }
        cfg.data_dir = env_value("DATA_DIR").map(PathBuf::from);
        cfg.ui_password = env_value("UI_PASSWORD");
        if env_value(crate::settings::STRATA_API_KEY).is_some() {
            tracing::warn!(
                "STRATA_API_KEY is ignored: the proxy fetches the Strata key from the PC service"
            );
        }
        Ok(cfg)
    }

    /// The keys from `FIELDS` that are set in the environment.
    pub fn env_keys() -> Vec<&'static str> {
        FIELDS
            .iter()
            .filter(|f| env_value(f.key).is_some())
            .map(|f| f.key)
            .collect()
    }

    /// Sets one value from its text form, as in the environment. An empty
    /// text clears an optional value.
    pub fn apply(&mut self, key: &str, raw: &str) -> Result<()> {
        let raw = raw.trim();
        let field = find_field(key).with_context(|| format!("unknown setting {key}"))?;
        if raw.is_empty() && !field.optional {
            bail!("must not be empty");
        }
        let text = (!raw.is_empty()).then(|| raw.to_string());
        match key {
            "LISTEN_ADDR" => self.listen_addr = address(raw)?,
            "STRATA_URL" => self.strata_url = text.map(|u| url(&u)).transpose()?,
            "PC_MAC" => self.pc_mac = text.map(|m| parse_mac(&m)).transpose()?,
            "WOL_ADDR" => self.wol_addr = address(raw)?,
            "PC_AGENT_URL" => self.pc_agent_url = text.map(|u| url(&u)).transpose()?,
            "PC_AGENT_TOKEN" => self.pc_agent_token = text,
            "SHELLY_URL" => self.shelly_url = text.map(|u| plug_url(&u)).transpose()?,
            "CODE_BENCH_URL" => self.code_bench_url = text.map(|u| url(&u)).transpose()?,
            "UPDATE_SUMMARY_TIMEOUT_SECS" => {
                self.update_summary_timeout = text.map(|t| limit(&t)).transpose()?
            }
            "CODE_BENCH_TASK_TIMEOUT_SECS" => {
                self.code_bench_task_timeout = text.map(|t| limit(&t)).transpose()?
            }
            "PLUG_SAMPLE_SECS" => self.plug_sample = positive(secs(raw)?)?,
            "IDLE_TIMEOUT_SECS" => self.idle_timeout = secs(raw)?,
            "WAKE_TIMEOUT_SECS" => self.wake_timeout = secs(raw)?,
            "WOL_RESEND_SECS" => self.wol_resend = positive(secs(raw)?)?,
            "HEALTH_POLL_MS" => self.health_poll = positive(millis(raw)?)?,
            "PING_INTERVAL_SECS" => self.ping_interval = positive(secs(raw)?)?,
            "IDLE_CHECK_SECS" => self.idle_check = positive(secs(raw)?)?,
            "SLEEP_RETRY_SECS" => self.sleep_retry = secs(raw)?,
            "SLEEP_SETTLE_SECS" => self.sleep_settle = secs(raw)?,
            "PROBE_TIMEOUT_MS" => self.probe_timeout = positive(millis(raw)?)?,
            "MAX_BODY_BYTES" => self.max_body_bytes = number(raw, "bytes")?,
            "POWER_AWAKE_WATTS" => self.power_awake_watts = positive_decimal(raw, "watts")?,
            "POWER_ASLEEP_WATTS" => self.power_asleep_watts = decimal(raw, "watts")?,
            "PRICE_CT_PER_KWH" => self.price_ct_per_kwh = positive_decimal(raw, "ct/kWh")?,
            _ => unreachable!("every field in FIELDS is handled"),
        }
        Ok(())
    }

    /// The text form of one value, as `apply` reads it; `None` when unset.
    pub fn value(&self, key: &str) -> Option<String> {
        let secs = |d: Duration| Some(d.as_secs().to_string());
        let millis = |d: Duration| Some(d.as_millis().to_string());
        match key {
            "LISTEN_ADDR" => Some(self.listen_addr.to_string()),
            "STRATA_URL" => self.strata_url.clone(),
            "PC_MAC" => self.pc_mac.map(format_mac),
            "WOL_ADDR" => Some(self.wol_addr.to_string()),
            "PC_AGENT_URL" => self.pc_agent_url.clone(),
            "PC_AGENT_TOKEN" => self.pc_agent_token.clone(),
            "SHELLY_URL" => self.shelly_url.clone(),
            "CODE_BENCH_URL" => self.code_bench_url.clone(),
            "UPDATE_SUMMARY_TIMEOUT_SECS" => self.update_summary_timeout.and_then(secs),
            "CODE_BENCH_TASK_TIMEOUT_SECS" => self.code_bench_task_timeout.and_then(secs),
            "PLUG_SAMPLE_SECS" => secs(self.plug_sample),
            "IDLE_TIMEOUT_SECS" => secs(self.idle_timeout),
            "WAKE_TIMEOUT_SECS" => secs(self.wake_timeout),
            "WOL_RESEND_SECS" => secs(self.wol_resend),
            "HEALTH_POLL_MS" => millis(self.health_poll),
            "PING_INTERVAL_SECS" => secs(self.ping_interval),
            "IDLE_CHECK_SECS" => secs(self.idle_check),
            "SLEEP_RETRY_SECS" => secs(self.sleep_retry),
            "SLEEP_SETTLE_SECS" => secs(self.sleep_settle),
            "PROBE_TIMEOUT_MS" => millis(self.probe_timeout),
            "MAX_BODY_BYTES" => Some(self.max_body_bytes.to_string()),
            "POWER_AWAKE_WATTS" => Some(self.power_awake_watts.to_string()),
            "POWER_ASLEEP_WATTS" => Some(self.power_asleep_watts.to_string()),
            "PRICE_CT_PER_KWH" => Some(self.price_ct_per_kwh.to_string()),
            _ => None,
        }
    }
}

fn env_value(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn number<T: std::str::FromStr>(raw: &str, unit: &str) -> Result<T> {
    raw.parse()
        .map_err(|_| anyhow::anyhow!("expects a whole number ({unit})"))
}

fn address(raw: &str) -> Result<SocketAddr> {
    raw.parse()
        .map_err(|_| anyhow::anyhow!("expects an IP address with port, e.g. 192.168.1.255:9"))
}

fn secs(raw: &str) -> Result<Duration> {
    Ok(Duration::from_secs(number(raw, "seconds")?))
}

fn millis(raw: &str) -> Result<Duration> {
    Ok(Duration::from_millis(number(raw, "milliseconds")?))
}

/// A time limit for work on the PC; below 10 s nothing could finish.
fn limit(raw: &str) -> Result<Duration> {
    let d = secs(raw)?;
    if d < Duration::from_secs(10) {
        bail!("must be at least 10 seconds");
    }
    Ok(d)
}

fn positive(d: Duration) -> Result<Duration> {
    if d.is_zero() {
        bail!("must be greater than 0");
    }
    Ok(d)
}

/// A finite number above 0; decimals with a point, e.g. 32.5.
fn positive_decimal(raw: &str, unit: &str) -> Result<f64> {
    let n = decimal(raw, unit)?;
    if n == 0.0 {
        bail!("must be greater than 0");
    }
    Ok(n)
}

/// A number of `unit` that may be 0, e.g. a PC that draws nothing asleep.
fn decimal(raw: &str, unit: &str) -> Result<f64> {
    let n: f64 = raw
        .trim()
        .parse()
        .ok()
        .filter(|n: &f64| n.is_finite())
        .with_context(|| format!("expects a number ({unit}), e.g. 32.5"))?;
    if n < 0.0 {
        bail!("must not be negative");
    }
    Ok(n)
}

fn url(raw: &str) -> Result<String> {
    if !(raw.starts_with("http://") || raw.starts_with("https://")) {
        bail!("must start with http:// or https://");
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// A URL, or a bare host like `192.168.1.228`, which means `http://` on it.
fn plug_url(raw: &str) -> Result<String> {
    if raw.contains("://") {
        url(raw)
    } else {
        url(&format!("http://{raw}"))
    }
}

/// Parses `AA:BB:CC:DD:EE:FF` or `AA-BB-CC-DD-EE-FF` (the format Windows prints).
pub fn parse_mac(raw: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = raw.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("MAC address {raw:?} does not have six parts");
    }
    let mut mac = [0u8; 6];
    for (byte, part) in mac.iter_mut().zip(parts) {
        *byte = u8::from_str_radix(part, 16)
            .with_context(|| format!("MAC address {raw:?} contains {part:?}"))?;
    }
    Ok(mac)
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.map(|b| format!("{b:02X}")).join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_mac_formats() {
        let expected = [0xaa, 0xbb, 0xcc, 0x01, 0x02, 0x03];
        assert_eq!(parse_mac("AA:BB:CC:01:02:03").unwrap(), expected);
        assert_eq!(parse_mac("aa-bb-cc-01-02-03").unwrap(), expected);
        assert!(parse_mac("aa:bb:cc").is_err());
        assert!(parse_mac("zz:bb:cc:01:02:03").is_err());
        assert_eq!(format_mac(expected), "AA:BB:CC:01:02:03");
    }

    #[test]
    fn every_field_round_trips() {
        let mut cfg = Config::default();
        cfg.apply("STRATA_URL", "http://pc:8080/").unwrap();
        cfg.apply("PC_MAC", "aa-bb-cc-01-02-03").unwrap();
        cfg.apply("PC_AGENT_URL", "http://pc:8091").unwrap();
        cfg.apply("PC_AGENT_TOKEN", "secret").unwrap();
        cfg.apply("SHELLY_URL", "192.168.1.228").unwrap();
        cfg.apply("CODE_BENCH_URL", "http://127.0.0.1:8096")
            .unwrap();
        cfg.apply("UPDATE_SUMMARY_TIMEOUT_SECS", "300").unwrap();
        cfg.apply("CODE_BENCH_TASK_TIMEOUT_SECS", "10").unwrap();
        for f in FIELDS {
            let text = cfg
                .value(f.key)
                .unwrap_or_else(|| panic!("{} is missing", f.key));
            let mut copy = Config::default();
            copy.apply(f.key, &text).unwrap();
            assert_eq!(copy.value(f.key), Some(text), "{}", f.key);
        }
        assert_eq!(cfg.strata_url.as_deref(), Some("http://pc:8080"));
        assert_eq!(cfg.shelly_url.as_deref(), Some("http://192.168.1.228"));
    }

    #[test]
    fn rejects_bad_values() {
        let mut cfg = Config::default();
        assert!(cfg.apply("IDLE_TIMEOUT_SECS", "zehn").is_err());
        assert!(cfg.apply("IDLE_TIMEOUT_SECS", "").is_err());
        assert!(cfg.apply("IDLE_CHECK_SECS", "0").is_err());
        assert!(cfg.apply("PLUG_SAMPLE_SECS", "0").is_err());
        assert!(cfg.apply("STRATA_URL", "pc:8080").is_err());
        assert!(cfg.apply("WOL_ADDR", "255.255.255.255").is_err());
        assert!(cfg.apply("DATA_DIR", "/data").is_err());
        assert!(cfg.apply("UPDATE_SUMMARY_TIMEOUT_SECS", "9").is_err());
        assert!(cfg.apply("CODE_BENCH_TASK_TIMEOUT_SECS", "1.5").is_err());
        cfg.apply("CODE_BENCH_TASK_TIMEOUT_SECS", "").unwrap();
        assert_eq!(cfg.code_bench_task_timeout, None);
        cfg.apply("PC_AGENT_URL", "http://pc").unwrap();
        cfg.apply("PC_AGENT_URL", "").unwrap();
        assert_eq!(cfg.pc_agent_url, None);
    }

    #[test]
    fn power_and_price_have_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.power_awake_watts, 75.0);
        assert_eq!(cfg.power_asleep_watts, 3.0);
        assert_eq!(cfg.price_ct_per_kwh, 30.0);
        assert_eq!(cfg.value("POWER_AWAKE_WATTS").as_deref(), Some("75"));
        assert_eq!(cfg.value("PRICE_CT_PER_KWH").as_deref(), Some("30"));
    }

    #[test]
    fn power_and_price_take_decimals() {
        let mut cfg = Config::default();
        cfg.apply("PRICE_CT_PER_KWH", "32.5").unwrap();
        cfg.apply("POWER_AWAKE_WATTS", " 120 ").unwrap();
        cfg.apply("POWER_ASLEEP_WATTS", "0.8").unwrap();
        assert_eq!(cfg.price_ct_per_kwh, 32.5);
        assert_eq!(cfg.power_awake_watts, 120.0);
        assert_eq!(cfg.power_asleep_watts, 0.8);
        assert_eq!(cfg.value("PRICE_CT_PER_KWH").as_deref(), Some("32.5"));
    }

    #[test]
    fn power_and_price_must_be_positive_numbers() {
        let mut cfg = Config::default();
        for key in [
            "POWER_AWAKE_WATTS",
            "POWER_ASLEEP_WATTS",
            "PRICE_CT_PER_KWH",
        ] {
            // "drei\u{df}ig" is the German number word for 30.
            for raw in ["-3", "drei\u{df}ig", "", "NaN", "inf", "32,5"] {
                assert!(cfg.apply(key, raw).is_err(), "{key}={raw:?}");
            }
        }
        // Asleep the PC may draw nothing; awake and the price not.
        for key in ["POWER_AWAKE_WATTS", "PRICE_CT_PER_KWH"] {
            assert!(cfg.apply(key, "0").is_err(), "{key}=0");
        }
        cfg.apply("POWER_ASLEEP_WATTS", "0").unwrap();
        assert_eq!(cfg.power_asleep_watts, 0.0);
        cfg.apply("POWER_ASLEEP_WATTS", "3").unwrap();
        assert_eq!(cfg.power_awake_watts, 75.0);
        assert_eq!(cfg.power_asleep_watts, 3.0);
        assert_eq!(cfg.price_ct_per_kwh, 30.0);
    }
}
