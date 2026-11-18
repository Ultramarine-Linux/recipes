//! The previously published catalog, so a build can keep listing (and
//! serving) older versions of each recipe.
//!
//! Bundles are content-addressed and immutable, so carrying one forward is a
//! copy: it is only kept when its bytes still hash to its file name.

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::repo::is_plain_relative_path;

/// Versions of each recipe listed in the index, the current one included.
pub const MAX_VERSIONS: usize = 10;

/// One published version of a recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    pub version: String,
    pub sha256: String,
    pub bundle: String,
}

/// The versions `index` lists for each recipe, newest first. Indexes from
/// before `versions` existed list only the entry itself.
#[must_use]
pub fn published_versions(index: &Value) -> BTreeMap<String, Vec<Version>> {
    let mut out = BTreeMap::new();
    for entry in index
        .get("recipes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let listed = entry.get("versions").and_then(Value::as_array);
        let mut versions: Vec<Version> = listed
            .map_or_else(|| vec![entry], |list| list.iter().collect())
            .into_iter()
            .filter_map(parse_version)
            .collect();
        versions.sort_by(|a, b| compare_versions(&b.version, &a.version));
        versions.dedup_by(|a, b| a.version == b.version);
        out.insert(id.to_owned(), versions);
    }
    out
}

fn parse_version(value: &Value) -> Option<Version> {
    let field = |name| value.get(name).and_then(Value::as_str).map(str::to_owned);
    Some(Version {
        version: field("version")?,
        sha256: field("sha256")?,
        bundle: field("bundle")?,
    })
}

/// A previously published `v1/` directory.
#[derive(Debug, Clone)]
pub struct Published {
    pub index: Value,
    dir: PathBuf,
}

impl Published {
    /// Load `<dir>/index.json`; bundles are read on demand.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("index.json");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let index = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Self {
            index,
            dir: dir.to_owned(),
        })
    }

    /// The bundle file at `path` (relative to `v1/`), if it exists under
    /// `recipes/` and its bytes match the hash in its name.
    pub fn bundle(&self, path: &str) -> Result<String> {
        anyhow::ensure!(
            is_plain_relative_path(path) && path.starts_with("recipes/"),
            "`{path}` isn't a bundle path"
        );
        let file = self.dir.join(path);
        let text = fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()))?;
        let hash = hex::encode(Sha256::digest(text.as_bytes()));
        let name = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        anyhow::ensure!(
            !name.is_empty() && hash.starts_with(name),
            "{path} doesn't match its content hash"
        );
        Ok(text)
    }
}

/// Compare `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` versions: numerically,
/// a release after its prereleases, build metadata ignored. Anything that
/// doesn't parse sorts first.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (version_key(a), version_key(b)) {
        (Some(a), Some(b)) => a.0.cmp(&b.0).then_with(|| match (a.1, b.1) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => compare_prerelease(a, b),
        }),
        (a, b) => a.is_some().cmp(&b.is_some()),
    }
}

fn version_key(version: &str) -> Option<([u64; 3], Option<&str>)> {
    let version = version.split_once('+').map_or(version, |(core, _)| core);
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut parts = core.split('.').map(str::parse::<u64>);
    let key = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    parts.next().is_none().then_some((key, pre))
}

/// Semver precedence: dot-separated identifiers, numeric ones compared as
/// numbers and before alphanumeric ones, fewer identifiers first.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn orders_versions_like_semver() {
        let mut versions = [
            "1.0.0",
            "0.10.0",
            "0.9.1",
            "1.0.0-rc.1",
            "1.0.0-beta",
            "1.0.0-rc.10",
            "1.0.0-rc.2",
            "2.0.0+build",
            "junk",
        ];
        versions.sort_by(|a, b| compare_versions(a, b));
        assert_eq!(
            versions,
            [
                "junk",
                "0.9.1",
                "0.10.0",
                "1.0.0-beta",
                "1.0.0-rc.1",
                "1.0.0-rc.2",
                "1.0.0-rc.10",
                "1.0.0",
                "2.0.0+build"
            ]
        );
        assert_eq!(compare_versions("1.2.3", "1.2.3+x"), Ordering::Equal);
    }

    #[test]
    fn reads_old_and_new_indexes() {
        let index = json!({ "recipes": [
            { "id": "old", "version": "1.0.0", "sha256": "a", "bundle": "recipes/old/1.json" },
            { "id": "new", "version": "1.1.0", "sha256": "c", "bundle": "recipes/new/3.json",
              "versions": [
                { "version": "1.0.0", "sha256": "b", "bundle": "recipes/new/2.json" },
                { "version": "1.1.0", "sha256": "c", "bundle": "recipes/new/3.json" },
              ] },
        ] });
        let versions = published_versions(&index);
        assert_eq!(versions["old"].len(), 1);
        let new: Vec<_> = versions["new"].iter().map(|v| v.version.as_str()).collect();
        assert_eq!(new, ["1.1.0", "1.0.0"]);
    }
}
