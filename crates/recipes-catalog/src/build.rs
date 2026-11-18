//! Rendering the published catalog (format 1, documented in docs/catalog-format.md):
//!
//! ```text
//! v1/index.json                    catalog index; short cache
//! v1/recipes/<id>/<hash>.json      one bundle per recipe; immutable
//! ```
//!
//! Bundle filenames hash the bundle file itself (translations included), so
//! they can be cached forever and only the index needs revalidating. `sha256`
//! is tetra's bundle digest over the recipe and templates only, which is what
//! hosts verify on install. The output is deterministic: an unchanged tree
//! produces the same bytes.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tetra_recipe::{AppRecipe, verification::bundle_sha256};

use crate::{
    history::{MAX_VERSIONS, Published, Version, compare_versions, published_versions},
    json,
    locales::prune_locales,
    repo::Repo,
};

/// Published format version, also the `/v<FORMAT>/` path prefix.
pub const FORMAT: u32 = 1;

#[derive(Serialize)]
struct Index<'a> {
    format: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    categories: Option<&'a Value>,
    locales: Map<String, Value>,
    recipes: Vec<IndexEntry<'a>>,
}

#[derive(Serialize)]
struct IndexEntry<'a> {
    id: &'a str,
    name: &'a str,
    description: &'a str,
    category: Option<&'a str>,
    icon: Option<&'a str>,
    homepage: Option<&'a str>,
    documentation: Option<&'a str>,
    version: &'a str,
    catalog: &'static str,
    sha256: String,
    bundle: String,
    /// Every version still served, newest first; the current one included.
    versions: &'a [Version],
}

#[derive(Serialize)]
struct Bundle<'a> {
    format: u32,
    id: &'a str,
    version: &'a str,
    sha256: &'a str,
    recipe: &'a str,
    templates: &'a BTreeMap<String, String>,
    locales: Map<String, Value>,
}

/// The published files, keyed by path relative to `v1/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub files: BTreeMap<String, String>,
    /// Recipes in the index.
    pub recipes: usize,
    /// Older versions that couldn't be carried forward (their bundle was
    /// missing or corrupt), so they're no longer listed.
    pub warnings: Vec<String>,
}

/// Render the catalog. Run [`crate::check`] first: this only fails on
/// recipes tetra can't parse.
///
/// With `previous`, the previously published catalog, each recipe also lists
/// (and ships the bundles of) up to [`MAX_VERSIONS`] versions in total, the
/// newest first, so dashboards can install an older one.
pub fn build(repo: &Repo, previous: Option<&Published>) -> Result<Catalog> {
    let mut files = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut published = previous
        .map(|previous| published_versions(&previous.index))
        .unwrap_or_default();
    let mut entries = Vec::new();
    for recipe in &repo.recipes {
        let parsed = AppRecipe::load_str(&recipe.yaml)
            .with_context(|| format!("{}/recipe.yaml", recipe.path))?;
        let sha256 = bundle_sha256(&recipe.yaml, &recipe.templates);
        let bundle = json::to_string(&Bundle {
            format: FORMAT,
            id: &parsed.recipe_id,
            version: &parsed.version,
            sha256: &sha256,
            recipe: &recipe.yaml,
            templates: &recipe.templates,
            locales: prune_locales(&recipe.locales),
        });
        let file_hash = hex::encode(Sha256::digest(bundle.as_bytes()));
        let path = format!("recipes/{}/{}.json", parsed.recipe_id, &file_hash[..16]);
        files.insert(path.clone(), bundle);

        let mut versions = vec![Version {
            version: parsed.version.clone(),
            sha256: sha256.clone(),
            bundle: path.clone(),
        }];
        let older = published.remove(&parsed.recipe_id).unwrap_or_default();
        // `check` makes sure a published version never changes its bundle,
        // so the current version's own entry is simply replaced.
        for old in older
            .into_iter()
            .filter(|old| old.version != parsed.version)
        {
            if versions.len() >= MAX_VERSIONS {
                break;
            }
            let Some(previous) = previous else { break };
            match previous.bundle(&old.bundle) {
                Ok(text)
                    if old
                        .bundle
                        .starts_with(&format!("recipes/{}/", parsed.recipe_id)) =>
                {
                    files.insert(old.bundle.clone(), text);
                    versions.push(old);
                }
                Ok(_) => warnings.push(format!(
                    "{} {}: bundle {} belongs to another recipe; dropped",
                    parsed.recipe_id, old.version, old.bundle
                )),
                Err(error) => warnings.push(format!(
                    "{} {}: {error:#}; dropped",
                    parsed.recipe_id, old.version
                )),
            }
        }
        versions.sort_by(|a, b| compare_versions(&b.version, &a.version));
        entries.push((recipe, parsed, sha256, path, versions));
    }

    let index = Index {
        format: FORMAT,
        name: repo.catalog.get("name"),
        categories: repo.catalog.get("categories"),
        locales: prune_locales(&repo.catalog_locales),
        recipes: entries
            .iter()
            .map(|(recipe, parsed, sha256, path, versions)| IndexEntry {
                id: &parsed.recipe_id,
                name: &parsed.name,
                description: parsed.description.as_deref().unwrap_or_default(),
                category: parsed.category.as_deref(),
                icon: parsed.icon.as_deref(),
                homepage: parsed.homepage.as_deref(),
                documentation: parsed.documentation.as_deref(),
                version: &parsed.version,
                catalog: recipe.tier.as_str(),
                sha256: sha256.clone(),
                bundle: path.clone(),
                versions,
            })
            .collect(),
    };
    files.insert("index.json".into(), json::to_string(&index));
    Ok(Catalog {
        files,
        recipes: entries.len(),
        warnings,
    })
}

impl Catalog {
    /// Write the files under `<out>/v1/`, replacing whatever was there (and
    /// nothing else in `out`).
    pub fn write(&self, out: &Path) -> Result<PathBuf> {
        let base = out.join(format!("v{FORMAT}"));
        if base.exists() {
            fs::remove_dir_all(&base)
                .with_context(|| format!("failed to remove {}", base.display()))?;
        }
        for (path, contents) in &self.files {
            let file = base.join(path);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&file, contents)
                .with_context(|| format!("failed to write {}", file.display()))?;
        }
        Ok(base)
    }

    /// Number of recipes in the catalog.
    #[must_use]
    pub const fn recipe_count(&self) -> usize {
        self.recipes
    }
}
