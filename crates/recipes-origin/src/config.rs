//! Configuration from the environment (documented in docs/origin.md).

use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// Absolute, so `current` can point at an absolute release path.
    pub data_dir: PathBuf,
    /// `owner/name` on GitHub.
    pub repo: String,
    pub branch: String,
    /// `None` only with `ALLOW_UNSIGNED_WEBHOOKS=1`.
    pub webhook_secret: Option<String>,
    pub github_token: Option<String>,
    pub cloudflare: Option<Cloudflare>,
}

/// Purges `<public_url>/v1/index.json` after each deploy.
#[derive(Debug, Clone)]
pub struct Cloudflare {
    pub api_token: String,
    pub zone_id: String,
    /// Without a trailing slash.
    pub public_url: String,
}

pub const DEFAULT_PORT: u16 = 8080;

/// `PORT`, for the `healthcheck` command (which ignores everything else).
pub fn port(get: impl Fn(&str) -> Option<String>) -> Result<u16> {
    get("PORT")
        .filter(|value| !value.is_empty())
        .map_or(Ok(DEFAULT_PORT), |port| {
            port.parse()
                .with_context(|| format!("PORT is not a valid port: {port}"))
        })
}

impl Config {
    /// Read the configuration; `get` looks up a variable (empty counts as
    /// unset).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |name: &str| get(name).filter(|value| !value.is_empty());
        let port = port(get)?;
        let data_dir = std::path::absolute(get("DATA_DIR").unwrap_or_else(|| "/data".into()))
            .context("DATA_DIR")?;
        let repo = get("RECIPES_REPO").unwrap_or_else(|| "Ultramarine-Server/recipes".into());
        let branch = get("RECIPES_BRANCH").unwrap_or_else(|| "main".into());
        ensure!(
            repo.split_once('/').is_some_and(|(owner, name)| {
                <[&str; 2]>::from((owner, name)).iter().all(|part| {
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                })
            }),
            "RECIPES_REPO must be owner/name"
        );
        ensure!(
            !branch.is_empty()
                && !branch.starts_with(['/', '-'])
                && !branch.contains("..")
                && branch
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')),
            "RECIPES_BRANCH is not a valid branch name"
        );

        let webhook_secret = get("GITHUB_WEBHOOK_SECRET");
        if webhook_secret.is_none() && get("ALLOW_UNSIGNED_WEBHOOKS").as_deref() != Some("1") {
            bail!(
                "GITHUB_WEBHOOK_SECRET is required (ALLOW_UNSIGNED_WEBHOOKS=1 for local development)"
            );
        }

        let cloudflare = match (
            get("CLOUDFLARE_API_TOKEN"),
            get("CLOUDFLARE_ZONE_ID"),
            get("PUBLIC_URL"),
        ) {
            (Some(api_token), Some(zone_id), Some(public_url)) => Some(Cloudflare {
                api_token,
                zone_id,
                public_url: public_url.trim_end_matches('/').to_owned(),
            }),
            // PUBLIC_URL alone is fine: it's only used for purging.
            (None, None, _) => None,
            _ => bail!(
                "set both CLOUDFLARE_API_TOKEN and CLOUDFLARE_ZONE_ID (and PUBLIC_URL), or neither"
            ),
        };

        Ok(Self {
            port,
            data_dir,
            repo,
            branch,
            webhook_secret,
            github_token: get("GITHUB_TOKEN"),
            cloudflare,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        Config::from_env(|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults() {
        let config = config(&[("GITHUB_WEBHOOK_SECRET", "s")]).unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.repo, "Ultramarine-Server/recipes");
        assert_eq!(config.branch, "main");
        assert!(config.cloudflare.is_none());
    }

    #[test]
    fn requires_a_webhook_secret() {
        assert!(config(&[]).is_err());
        assert!(config(&[("GITHUB_WEBHOOK_SECRET", "")]).is_err());
        let config = config(&[("ALLOW_UNSIGNED_WEBHOOKS", "1")]).unwrap();
        assert!(config.webhook_secret.is_none());
    }

    #[test]
    fn cloudflare_needs_all_settings() {
        let secret = ("GITHUB_WEBHOOK_SECRET", "s");
        assert!(config(&[secret, ("CLOUDFLARE_API_TOKEN", "t")]).is_err());
        assert!(
            config(&[
                secret,
                ("CLOUDFLARE_API_TOKEN", "t"),
                ("CLOUDFLARE_ZONE_ID", "z")
            ])
            .is_err()
        );
        let cloudflare = config(&[
            secret,
            ("CLOUDFLARE_API_TOKEN", "t"),
            ("CLOUDFLARE_ZONE_ID", "z"),
            ("PUBLIC_URL", "https://recipes.example/"),
        ])
        .unwrap()
        .cloudflare
        .unwrap();
        assert_eq!(cloudflare.public_url, "https://recipes.example");
        assert!(config(&[secret, ("PUBLIC_URL", "https://recipes.example")]).is_ok());
    }

    #[test]
    fn rejects_bad_values() {
        let secret = ("GITHUB_WEBHOOK_SECRET", "s");
        assert!(config(&[secret, ("PORT", "http")]).is_err());
        assert!(config(&[secret, ("RECIPES_REPO", "nope")]).is_err());
        assert!(config(&[secret, ("RECIPES_REPO", "a/b/c")]).is_err());
        assert!(config(&[secret, ("RECIPES_BRANCH", "a..b")]).is_err());
        assert!(config(&[secret, ("RECIPES_BRANCH", "release/1.0")]).is_ok());
    }
}
