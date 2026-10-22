//! AVD ARM gateway: request a connection for a feed resource (FreeRDP gateway/arm.c).

use crate::{auth, feed, rdpfile::RdpFile};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub struct ArmConnection {
    /// WebSocket URL of the gateway host that relays RDP to the session host.
    pub ws_url: String,
    /// Session host name (a short machine label).
    pub server_name: String,
    /// Response field names, for diagnostics (values carry secrets).
    pub fields: Vec<String>,
}

pub fn arm_connect(rdp: &RdpFile, token: &str) -> Result<ArmConnection> {
    let gateway = rdp.get("gatewayhostname").context(".rdp has no gatewayhostname")?;
    let gateway = gateway.split(':').next().unwrap_or(gateway);
    let url = format!("https://{gateway}/api/arm/v2/connections");
    feed::ensure_trusted(&url)?;

    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .bearer_auth(token)
        .header("Accept", "*/*")
        .header("X-MS-User-Agent", auth::MS_USER_AGENT)
        .json(&json!({
            "application": rdp.get("remoteapplicationprogram"),
            "loadBalanceInfo": rdp.get("loadbalanceinfo"),
            "LogonToken": null,
            "gatewayLoadBalancerToken": null,
        }))
        .send()?;
    let status = resp.status();
    let body: Value = serde_json::from_str(&resp.text()?).unwrap_or_default();
    if !status.is_success() {
        bail!("gateway refused ({status}): {} {}", body["Code"], body["Message"]);
    }

    let ws_url = body["gatewayLocationPreWebSocket"]
        .as_str()
        .or(body["gatewayLocation"].as_str())
        .context("no gatewayLocation in response")?;
    let server_name = body["redirectedServerName"]
        .as_str()
        .context("no redirectedServerName in response")?;
    Ok(ArmConnection {
        ws_url: ws_url.into(),
        server_name: server_name.into(),
        fields: body
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
    })
}
