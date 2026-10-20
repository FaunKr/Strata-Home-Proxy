// No console window, neither for the background service nor for the shortcut.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Mutex};

use anyhow::{Context, Result, bail};
use strata_pc_agent::{Agent, Config, platform};

const USAGE: &str = "strata-pc-agent [--config FILE] [run | gaming on|off|toggle]";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gaming_cmd = args.iter().any(|a| a == "gaming");
    if let Err(e) = real_main(args).await {
        // Without a console, a message box is the only way to be seen.
        if gaming_cmd {
            platform::notify("Gaming mode", &format!("Error: {e:#}"));
        } else {
            platform::notify("Strata PC service", &format!("{e:#}"));
        }
        std::process::exit(1);
    }
}

async fn real_main(args: Vec<String>) -> Result<()> {
    let mut config_path: Option<PathBuf> = None;
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg == "--config" {
            config_path = Some(it.next().context("--config needs a file")?.into());
        } else {
            rest.push(arg);
        }
    }
    let cfg = Config::load(&config_path.unwrap_or_else(strata_pc_agent::config::default_path))?;
    match rest
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["run"] => {
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(cfg.state_dir().join("strata-pc-agent.log"))
                .context("Log file cannot be created")?;
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(Mutex::new(log))
                .init();
            strata_pc_agent::run(Agent::new(cfg, Box::new(platform::System))).await
        }
        ["gaming", mode] => {
            // The German words stay for desktop shortcuts of older installs.
            let want = match *mode {
                "on" | "an" => Some(true),
                "off" | "aus" => Some(false),
                "toggle" | "umschalten" => None,
                other => bail!("Unknown: {other:?}. {USAGE}"),
            };
            let status = strata_pc_agent::switch_gaming(&cfg, want)
                .await
                .context("PC service not reachable. Is it running?")?;
            let text = if status.gaming_mode {
                "Gaming mode is ON.\nStrata is stopped, the PC does not go to sleep on its own."
            } else {
                "Gaming mode is OFF.\nStrata starts, the PC may go to sleep on its own again."
            };
            platform::notify("Gaming mode", text);
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}
