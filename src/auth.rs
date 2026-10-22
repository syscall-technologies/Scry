//! Entra ID sign-in (auth code + PKCE) and token refresh.
//! Endpoints, client id and scopes mirror FreeRDP (libfreerdp/core/settings.c, aad.c).

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::BufRead, path::PathBuf};
use url::Url;

/// Microsoft Remote Desktop public client, as used by FreeRDP. Override with SCRY_CLIENT_ID.
const DEFAULT_CLIENT_ID: &str = "a85cf173-4192-42f8-81fa-777a763e6e2c";
pub const REDIRECT: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
const LOGIN: &str = "https://login.microsoftonline.com/organizations/oauth2/v2.0";
const AVD_SCOPE: &str = "https://www.wvd.microsoft.com/.default openid profile offline_access";
/// The feed service rejects requests without an approved X-MS-User-Agent (web client value).
pub const MS_USER_AGENT: &str = "com.microsoft.rdc.html/2.0.79.2 rdhtml-sdk/2.0.4";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    refresh_token: String,
}

fn client_id() -> String {
    std::env::var("SCRY_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.into())
}

pub fn data_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("no data dir")?;
    let dir = base.join("scry");
    // One-time migration from the former "winapp" name, so a rename doesn't force a re-login.
    let legacy = base.join("winapp");
    if !dir.exists() && legacy.is_dir() {
        let _ = fs::rename(&legacy, &dir);
    }
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn store_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("tokens.json"))
}

fn save_refresh(token: &str) -> Result<()> {
    let path = store_path()?;
    fs::write(
        &path,
        serde_json::to_vec(&Stored {
            refresh_token: token.into(),
        })?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_refresh() -> Result<String> {
    let bytes = fs::read(store_path()?).context("not signed in, run `scry login`")?;
    Ok(serde_json::from_slice::<Stored>(&bytes)?.refresh_token)
}

/// True when a sign-in is stored (so a connect can start without interaction).
pub fn is_signed_in() -> bool {
    store_path().map(|p| p.exists()).unwrap_or(false)
}

/// Forgets the stored sign-in, so the next sign-in starts fresh (lets you switch account).
pub fn logout() -> Result<()> {
    let path = store_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

fn random_b64(len: usize) -> String {
    let mut buf = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut buf);
    B64.encode(buf)
}

/// POST to the token endpoint; surfaces AADSTS errors verbatim since they tell us which policy blocked us.
fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = reqwest::blocking::Client::new()
        .post(format!("{LOGIN}/token"))
        .form(form)
        .send()?;
    let status = resp.status();
    let body = resp.text()?;
    if !status.is_success() {
        let err: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        bail!(
            "token request failed ({status}): {} {}",
            err["error"].as_str().unwrap_or(""),
            err["error_description"].as_str().unwrap_or(&body)
        );
    }
    Ok(serde_json::from_str(&body)?)
}

/// Decodes a JWT's claims without verifying it (diagnostics only).
pub fn jwt_claims(jwt: &str) -> serde_json::Value {
    jwt.split('.')
        .nth(1)
        .and_then(|p| B64.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn login() -> Result<()> {
    let tok = interactive(AVD_SCOPE, &[])?;
    let id = tok.id_token.as_deref().map(jwt_claims).unwrap_or_default();
    println!(
        "Signed in as {} (tenant {})",
        id["preferred_username"].as_str().unwrap_or("?"),
        id["tid"].as_str().unwrap_or("?")
    );
    Ok(())
}

/// Embedded web view on macOS/Windows unless SCRY_LOGIN=browser; Linux uses the browser for now.
fn use_webview() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows")) && std::env::var("SCRY_LOGIN").as_deref() != Ok("browser")
}

/// Opens the default browser and reads the redirect URL back from the user.
fn browser_sign_in(url: &Url) -> Result<String> {
    println!("Opening the browser for sign-in:\n\n{url}\n");
    let _ = webbrowser::open(url.as_str());
    println!("After sign-in you land on a blank page. Copy its full URL, then press Enter here:");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    // macOS terminals cap an input line at 1024 bytes and the redirect URL is longer,
    // so read it from the clipboard unless something was typed.
    if line.trim().is_empty() {
        line = arboard::Clipboard::new()?.get_text().context("clipboard has no text")?;
    }
    Ok(line)
}

/// Interactive sign-in (auth code + PKCE) for `scope`; `extra` is added to the code exchange.
fn interactive(scope: &str, extra: &[(&str, &str)]) -> Result<TokenResponse> {
    let client_id = client_id();
    let verifier = random_b64(32);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);
    let url = Url::parse_with_params(
        &format!("{LOGIN}/authorize"),
        &[
            ("client_id", client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("scope", scope),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("prompt", "select_account"),
        ],
    )?;

    let line = if use_webview() {
        crate::weblogin::sign_in(url.as_str())?
    } else {
        browser_sign_in(&url)?
    };

    let redirected = Url::parse(line.trim()).context("not a URL")?;
    let params: std::collections::HashMap<_, _> = redirected.query_pairs().into_owned().collect();
    if let Some(err) = params.get("error") {
        bail!(
            "sign-in failed: {err} {}",
            params.get("error_description").map_or("", |s| s)
        );
    }
    if params.get("state") != Some(&state) {
        bail!("state mismatch, paste the URL from this sign-in attempt");
    }
    let code = params.get("code").context("no `code` in URL")?;

    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id.as_str()),
        ("code", code.as_str()),
        ("redirect_uri", REDIRECT),
        ("scope", scope),
        ("code_verifier", verifier.as_str()),
    ];
    form.extend_from_slice(extra);
    let tok = token_request(&form)?;
    // The session-host scope is requested without offline_access, so no refresh token comes back.
    if let Some(rt) = &tok.refresh_token {
        save_refresh(rt)?;
    }
    Ok(tok)
}

/// Silent token via the stored refresh token, falling back to a browser sign-in when Entra
/// demands interaction (sign-in frequency, device consent, MFA, expired or missing token).
fn refresh(scope: &str, extra: &[(&str, &str)]) -> Result<String> {
    match silent(scope, extra) {
        Err(e) if needs_interaction(&e) => {
            println!("   {}\n   Signing in again in the browser...", e.root_cause());
            Ok(interactive(scope, extra)?.access_token)
        }
        other => other,
    }
}

fn needs_interaction(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("invalid_grant") || msg.contains("interaction_required") || msg.contains("not signed in")
}

fn silent(scope: &str, extra: &[(&str, &str)]) -> Result<String> {
    let client_id = client_id();
    let rt = load_refresh()?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id.as_str()),
        ("refresh_token", rt.as_str()),
        ("scope", scope),
    ];
    form.extend_from_slice(extra);
    let tok = token_request(&form)?;
    if let Some(rt) = &tok.refresh_token {
        save_refresh(rt)?;
    }
    Ok(tok.access_token)
}

/// Server nonce that goes into the RDSAAD assertion's `client_claims`.
pub fn srv_nonce() -> Result<String> {
    let resp = reqwest::blocking::Client::new()
        .post("https://login.microsoftonline.com/common/oauth2/v2.0/token")
        .form(&[("grant_type", "srv_challenge")])
        .send()?;
    let body: serde_json::Value = resp.json()?;
    Ok(body["Nonce"]
        .as_str()
        .context("no Nonce in srv_challenge reply")?
        .to_string())
}

/// Token for the AVD feed and gateway.
pub fn avd_access_token() -> Result<String> {
    refresh(AVD_SCOPE, &[])
}

/// Proof-of-possession token for Entra SSO into the session host (RDSAAD), bound to `req_cnf`.
pub fn rdsaad_token(host_short: &str, req_cnf: &str) -> Result<String> {
    let scope = format!("ms-device-service://termsrv.wvd.microsoft.com/name/{host_short}/user_impersonation");
    refresh(&scope, &[("req_cnf", req_cnf)])
}
