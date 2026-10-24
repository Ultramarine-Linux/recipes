//! Regenerating the English translation sources from the YAML.
//!
//! Weblate reads `en.json` as the monolingual base file, so this runs after
//! any change to a user-facing string; `check` fails while one is stale.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};

use crate::{
    json,
    locales::{catalog_source, recipe_source},
    repo::{Repo, SOURCE_LOCALE},
};

/// The `en.json` files the YAML produces: `(path, contents)`.
#[must_use]
pub fn sources(repo: &Repo) -> Vec<(PathBuf, String)> {
    let mut files = vec![(
        repo.root
            .join("locales")
            .join(format!("{SOURCE_LOCALE}.json")),
        json::to_string(&catalog_source(&repo.catalog)),
    )];
    for recipe in &repo.recipes {
        files.push((
            recipe
                .locales_dir(&repo.root)
                .join(format!("{SOURCE_LOCALE}.json")),
            json::to_string(&recipe_source(&recipe.doc)),
        ));
    }
    files
}

/// Write every source locale file that changed; returns the ones written.
pub fn extract(repo: &Repo) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (path, contents) in sources(repo) {
        if fs::read_to_string(&path).is_ok_and(|current| current == contents) {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}
