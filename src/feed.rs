//! AVD feed discovery: feeddiscovery -> tenant feeds -> resources -> .rdp files.

use crate::{auth, rdpfile};
use anyhow::{Context, Result, bail};
use std::{fs, path::PathBuf};
use url::Url;

const DISCOVERY: &str = "https://rdweb.wvd.microsoft.com/api/arm/feeddiscovery";
const ACCEPT_DISCOVERY: &str = "application/x-msts-radc-discovery+xml,text/xml";
const ACCEPT_FEED: &str = "application/x-msts-radc+xml;radc_schema_version=2.0,text/xml";

struct Resource {
    title: String,
    kind: String,
    rdp_url: Option<String>,
}

pub fn feed_dir() -> Result<PathBuf> {
    let dir = auth::data_dir()?.join("feed");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The bearer token is only ever sent to Microsoft hosts, since feed XML supplies the URLs.
pub fn ensure_trusted(url: &str) -> Result<()> {
    let u = Url::parse(url)?;
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let ok = u.scheme() == "https"
        && ["microsoft.com", "microsoftonline.com"]
            .iter()
            .any(|s| host == *s || host.ends_with(&format!(".{s}")));
    if !ok {
        bail!("refusing to send token to untrusted URL {url}");
    }
    Ok(())
}

fn get(url: &str, token: &str, accept: &str) -> Result<Vec<u8>> {
    ensure_trusted(url)?;
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .bearer_auth(token)
        .header("Accept", accept)
        .header("X-MS-User-Agent", auth::MS_USER_AGENT)
        .send()?;
    let status = resp.status();
    let body = resp.bytes()?.to_vec();
    if !status.is_success() {
        bail!("GET {url} failed ({status}): {}", String::from_utf8_lossy(&body));
    }
    Ok(body)
}

fn enumerate(token: &str) -> Result<Vec<Resource>> {
    let body = String::from_utf8(get(DISCOVERY, token, ACCEPT_DISCOVERY)?)?;
    let disc = roxmltree::Document::parse(&body)?;
    let mut resources = Vec::new();
    for feed in disc.descendants().filter(|n| n.has_tag_name("TenantFeedURL")) {
        let Some(url) = feed.attribute("FeedURL") else { continue };
        let tenant = feed.attribute("TenantDisplayName").unwrap_or("").to_string();
        let body = match get(url, token, ACCEPT_FEED) {
            Ok(b) => String::from_utf8(b)?,
            Err(e) => {
                eprintln!("  ! skipping tenant feed {tenant}: {e}");
                continue;
            }
        };
        let doc = roxmltree::Document::parse(&body)?;
        for res in doc.descendants().filter(|n| n.has_tag_name("Resource")) {
            resources.push(Resource {
                title: res.attribute("Title").unwrap_or("?").into(),
                kind: res.attribute("Type").unwrap_or("?").into(),
                rdp_url: res
                    .descendants()
                    .find(|n| n.has_tag_name("ResourceFile"))
                    .and_then(|n| n.attribute("URL"))
                    .map(Into::into),
            });
        }
    }
    Ok(resources)
}

/// A Cloud PC the account is entitled to; `index` is its `connect`/`session` argument.
pub struct CloudPc {
    pub index: usize,
    pub title: String,
    pub kind: String,
}

/// Signs in, enumerates the entitled resources and saves each one's `.rdp` under the feed dir.
pub fn collect() -> Result<Vec<CloudPc>> {
    let token = auth::avd_access_token()?;
    let resources = enumerate(&token)?;
    if resources.is_empty() {
        bail!("feed returned no resources (no Cloud PC / AVD assignment for this account?)");
    }
    let dir = feed_dir()?;
    let mut list = Vec::new();
    for (index, r) in resources.into_iter().enumerate() {
        let url = r.rdp_url.as_deref().context("resource has no .rdp URL")?;
        let bytes = get(url, &token, "*/*")?;
        fs::write(dir.join(format!("{index}.rdp")), &bytes)?;
        list.push(CloudPc {
            index,
            title: r.title,
            kind: r.kind,
        });
    }
    Ok(list)
}

pub fn list() -> Result<()> {
    let dir = feed_dir()?;
    for pc in collect()? {
        println!("[{}] {}  ({})", pc.index, pc.title, pc.kind);
        let rdp = rdpfile::parse(&rdpfile::decode(&fs::read(dir.join(format!("{}.rdp", pc.index)))?));
        for key in [
            "full address",
            "gatewayhostname",
            "remoteapplicationprogram",
            "enablerdsaadauth",
        ] {
            println!("      {key}: {}", rdp.get(key).map_or("-", |s| s));
        }
        println!(
            "      loadbalanceinfo: {}",
            if rdp.contains_key("loadbalanceinfo") {
                "present"
            } else {
                "MISSING"
            }
        );
    }
    Ok(())
}
