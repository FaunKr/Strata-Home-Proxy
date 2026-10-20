use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cfg = strata_nas_proxy::Config::from_env()?;
    match std::env::args().nth(1).as_deref() {
        // Sends one magic packet and exits, for testing Wake on LAN by hand.
        Some("wake") => {
            let cfg = strata_nas_proxy::Settings::new(cfg, &[]).current();
            let mac = cfg
                .pc_mac
                .ok_or_else(|| anyhow::anyhow!("No MAC address known (PC_MAC)"))?;
            strata_nas_proxy::wol::send(mac, cfg.wol_addr).await?;
            println!("Wake packet sent to {}", cfg.wol_addr);
            Ok(())
        }
        None => strata_nas_proxy::run(cfg).await,
        Some(other) => anyhow::bail!("Unknown command {other:?}, only \"wake\" is allowed"),
    }
}
