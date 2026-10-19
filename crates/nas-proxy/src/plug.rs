//! Die Messsteckdose: the Shelly plug the PC is plugged into, asked for its
//! current Leistung. Only for display; it has no say in waking or sleeping.

use reqwest::Client;
use serde_json::Value;

/// The current Leistung in watts; `None` when the plug does not answer usefully.
/// Works with both Shelly generations without being told which one it is.
pub async fn power(client: &Client, base: &str) -> Option<f64> {
    // Gen2+ plugs: relay channel 0 is the only one a plug has.
    let gen2 = client
        .get(format!("{base}/rpc/Switch.GetStatus?id=0"))
        .send()
        .await
        .ok()?;
    if gen2.status().is_success()
        && let Some(watts) = gen2
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["apower"].as_f64())
    {
        return Some(watts);
    }
    // Gen1 plugs answer the RPC with an error. A plug that did not answer at
    // all is not asked twice, so a hanging one costs one timeout only; hence
    // no shared helper with `get` for the first call.
    get(client, &format!("{base}/status")).await?["meters"][0]["power"].as_f64()
}

async fn get(client: &Client, url: &str) -> Option<Value> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}
