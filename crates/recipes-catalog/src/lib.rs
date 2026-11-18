//! The Ultramarine Server recipe catalog: loading a checkout, checking it,
//! extracting translation sources, and building the published catalog.
//!
//! Shared by the `recipes` CLI and the `recipes-origin` builder, which runs
//! the same check and build in-process for every push.

pub mod build;
pub mod check;
pub mod extract;
pub mod history;
pub mod images;
pub mod json;
pub mod locales;
pub mod repo;

use std::path::Path;

use anyhow::{Result, bail};

pub use build::{Catalog, FORMAT, build};
pub use check::{CheckOptions, Report, check};
pub use extract::extract;
pub use history::Published;
pub use repo::{Recipe, Repo, Tier};

/// Load, check and build the checkout at `root`, refusing to build one that
/// fails the check. What the origin builder runs for every push.
///
/// The origin passes the live catalog as `previous`, so older versions stay
/// listed and the version checks run against it.
pub fn check_and_build(root: &Path, previous: Option<&Published>) -> Result<(Report, Catalog)> {
    let repo = Repo::load(root)?;
    let options = CheckOptions {
        previous_index: previous.map(|published| published.index.clone()),
    };
    let report = check(&repo, &options)?;
    if !report.is_ok() {
        bail!(
            "check found {} problem(s):\n{}",
            report.errors.len(),
            report.errors.join("\n")
        );
    }
    let catalog = build(&repo, previous)?;
    Ok((report, catalog))
}
