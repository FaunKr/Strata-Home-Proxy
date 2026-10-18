//! Configuration, read from environment variables.

use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct Config {
    /// Address the API proxy listens on (`LISTEN_ADDR`).
    pub listen_addr: SocketAddr,
    /// Base URL of Strata on the PC, e.g. `http://192.168.1.50:8080` (`STRATA_URL`).
    pub strata_url: String,
    /// MAC address of the PC's network card (`PC_MAC`).
    pub pc_mac: [u8; 6],
    /// Where the magic packet is sent (`WOL_ADDR`), usually the LAN broadcast on port 9.
    pub wol_addr: SocketAddr,
    /// Base URL of the PC service (`PC_AGENT_URL`). Without it the proxy cannot
    /// detect gaming mode and never puts the PC to sleep.
    pub pc_agent_url: Option<String>,
    /// Bearer token for the PC service (`PC_AGENT_TOKEN`).
    pub pc_agent_token: Option<String>,
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
    /// Largest accepted request body in bytes (`MAX_BODY_BYTES`).
    pub max_body_bytes: usize,
    /// Directory for the event history (`DATA_DIR`); in memory only if unset.
    pub data_dir: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let strata_url = required("STRATA_URL")?.trim_end_matches('/').to_string();
        let pc_mac = parse_mac(&required("PC_MAC")?)?;
        Ok(Self {
            listen_addr: parse_or("LISTEN_ADDR", "0.0.0.0:8080")?,
            strata_url,
            pc_mac,
            wol_addr: parse_or("WOL_ADDR", "255.255.255.255:9")?,
            pc_agent_url: optional("PC_AGENT_URL").map(|u| u.trim_end_matches('/').to_string()),
            pc_agent_token: optional("PC_AGENT_TOKEN"),
            idle_timeout: secs("IDLE_TIMEOUT_SECS", 900)?,
            wake_timeout: secs("WAKE_TIMEOUT_SECS", 240)?,
            wol_resend: secs("WOL_RESEND_SECS", 10)?,
            health_poll: Duration::from_millis(parse_or("HEALTH_POLL_MS", "1000")?),
            ping_interval: secs("PING_INTERVAL_SECS", 15)?,
            idle_check: secs("IDLE_CHECK_SECS", 15)?,
            sleep_retry: secs("SLEEP_RETRY_SECS", 60)?,
            sleep_settle: secs("SLEEP_SETTLE_SECS", 30)?,
            probe_timeout: Duration::from_millis(parse_or("PROBE_TIMEOUT_MS", "2000")?),
            max_body_bytes: parse_or("MAX_BODY_BYTES", "67108864")?,
            data_dir: optional("DATA_DIR").map(PathBuf::from),
        })
    }
}

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn required(key: &str) -> Result<String> {
    optional(key).with_context(|| format!("{key} ist nicht gesetzt"))
}

fn parse_or<T>(key: &str, default: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = optional(key).unwrap_or_else(|| default.to_string());
    raw.trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("{key}={raw:?} ist ungültig: {e}"))
}

fn secs(key: &str, default: u64) -> Result<Duration> {
    Ok(Duration::from_secs(parse_or(key, &default.to_string())?))
}

/// Parses `AA:BB:CC:DD:EE:FF` or `AA-BB-CC-DD-EE-FF` (the format Windows prints).
pub fn parse_mac(raw: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = raw.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("MAC-Adresse {raw:?} hat nicht sechs Teile");
    }
    let mut mac = [0u8; 6];
    for (byte, part) in mac.iter_mut().zip(parts) {
        *byte = u8::from_str_radix(part, 16)
            .with_context(|| format!("MAC-Adresse {raw:?} enthält {part:?}"))?;
    }
    Ok(mac)
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
    }
}
