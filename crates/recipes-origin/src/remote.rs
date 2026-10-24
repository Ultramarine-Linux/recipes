//! GitHub (sources) and Cloudflare (cache purging).

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, RequestBuilder, header};
use serde_json::{Value, json};

use crate::{
    config::{Cloudflare, Config},
    release::extract_tarball,
    webhook::is_sha,
};

/// Largest source tarball we download.
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!(
            "ultramarine-recipes-builder/",
            env!("CARGO_PKG_VERSION")
        ))
        .connect_timeout(Duration::from_secs(30))
        .build()?)
}

/// A GitHub request, authenticated when a token is configured.
fn github(config: &Config, request: RequestBuilder, accept: &str) -> RequestBuilder {
    let request = request.header(header::ACCEPT, accept);
    match &config.github_token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

/// The commit `branch` points at now.
pub async fn resolve_head(client: &Client, config: &Config) -> Result<String> {
    let url = format!(
        "https://api.github.com/repos/{}/commits/{}",
        config.repo,
        config.branch.replace('/', "%2F")
    );
    let response = github(config, client.get(&url), "application/vnd.github.sha")
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    ensure!(
        response.status().is_success(),
        "GET {url}: HTTP {}",
        response.status()
    );
    let sha = response.text().await?.trim().to_owned();
    ensure!(is_sha(&sha), "GET {url}: unexpected response");
    Ok(sha)
}

/// Download the source of `sha` and unpack it into `dest`. With a token, go
/// through the API (which also works for private repositories); otherwise
/// straight to codeload.
pub async fn fetch_source(
    client: &Client,
    config: &Config,
    sha: &str,
    dest: PathBuf,
) -> Result<()> {
    let url = if config.github_token.is_some() {
        format!("https://api.github.com/repos/{}/tarball/{sha}", config.repo)
    } else {
        format!("https://codeload.github.com/{}/tar.gz/{sha}", config.repo)
    };
    tracing::info!("fetching {}@{sha}", config.repo);
    let mut response = github(config, client.get(&url), "application/vnd.github+json")
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    ensure!(
        response.status().is_success(),
        "GET {url}: HTTP {}",
        response.status()
    );
    let mut archive = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("GET {url}"))?
    {
        archive.extend_from_slice(&chunk);
        if archive.len() > MAX_ARCHIVE_BYTES {
            bail!("GET {url}: archive is larger than {MAX_ARCHIVE_BYTES} bytes");
        }
    }
    tokio::task::spawn_blocking(move || extract_tarball(archive.as_slice(), &dest))
        .await?
        .context("failed to unpack the source archive")
}

/// Drop the cached `index.json` from Cloudflare. Bundles are
/// content-addressed, so they never need purging.
pub async fn purge_index(client: &Client, cloudflare: &Cloudflare) -> Result<()> {
    let url = format!(
        "https://api.cloudflare.com/client/v4/zones/{}/purge_cache",
        cloudflare.zone_id
    );
    let body = json!({ "files": [format!("{}/v1/index.json", cloudflare.public_url)] });
    let response = client
        .post(&url)
        .bearer_auth(&cloudflare.api_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .context("Cloudflare purge")?;
    let status = response.status();
    let body: Value = serde_json::from_slice(&response.bytes().await?).unwrap_or_default();
    if !status.is_success() || body.get("success") != Some(&Value::Bool(true)) {
        bail!(
            "Cloudflare purge: HTTP {status} {}",
            body.get("errors").unwrap_or(&Value::Null)
        );
    }
    Ok(())
}
