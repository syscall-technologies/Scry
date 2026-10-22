//! Phase 0 step d: ask the AVD ARM gateway for a connection and request the RDSAAD
//! proof-of-possession token for the session host.

use crate::{aad::PopKey, auth, feed, gateway, rdpfile};
use anyhow::{Context, Result};

pub fn probe(index: usize) -> Result<()> {
    let path = feed::feed_dir()?.join(format!("{index}.rdp"));
    let rdp = rdpfile::parse(&rdpfile::decode(
        &std::fs::read(&path).context("run `scry list` first")?,
    ));

    println!("1. AVD access token...");
    let token = auth::avd_access_token()?;

    println!("2. ARM gateway connection request...");
    let arm = gateway::arm_connect(&rdp, &token)?;
    println!("   OK, response fields: {}", arm.fields.join(", "));
    let ws_host = url::Url::parse(&arm.ws_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from));
    println!("   gateway websocket host: {}", ws_host.as_deref().unwrap_or("?"));
    let host_short = arm.server_name.split('.').next().unwrap_or(&arm.server_name);
    println!("   session host: {host_short}");

    println!("3. RDSAAD proof-of-possession token for {host_short}...");
    let key = PopKey::generate()?;
    let at = auth::rdsaad_token(host_short, &key.kid)?;
    let claims = auth::jwt_claims(&at);
    println!(
        "   OK, aud: {}, has cnf: {}",
        claims["aud"],
        claims.get("cnf").is_some()
    );

    println!("\nGO: sign-in, gateway and session-host SSO token all succeeded.");
    Ok(())
}
