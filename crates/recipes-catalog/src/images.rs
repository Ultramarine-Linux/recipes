//! Keeping pinned images current: `recipes update-images`.
//!
//! Every `Image=name:tag@sha256:…` line in a recipe's templates is checked
//! against its registry. A newer tag in the same release line (same prefix,
//! same suffix, same number of version components) replaces the tag and
//! digest; otherwise the tag's digest is refreshed, which picks up rebuilds of
//! floating tags like `18` or `alpine`. Major versions are held back and
//! reported for a person to look at, since they tend to break data (Postgres
//! data directories, app migrations).
//!
//! A comment directly above an `Image=` line changes how far it may move:
//!
//! ```text
//! # image-updates: patch   only x.y.Z
//! # image-updates: minor   x.Y.z and x.y.Z (the default)
//! # image-updates: major   anything newer in the line
//! # image-updates: off     leave this image alone
//! ```
//!
//! Calendar versions (a first component of 1000 or more, like `2026.10.1`)
//! have no majors, so `minor` lets them move freely.
//!
//! A recipe whose templates change gets a patch version bump, unless its
//! version is already newer than the published one.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map::Entry},
    fmt::Write as _,
    fs,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    history::{compare_versions, published_versions},
    repo::{Recipe, Repo},
};

/// Where image tags and digests come from.
pub trait Registry {
    /// Every tag of the repository `name` (no tag or digest).
    fn tags(&mut self, name: &str) -> Result<Vec<String>>;
    /// The `sha256:<hex>` digest `name:tag` points at (for a multi-arch
    /// image, the digest of its index).
    fn digest(&mut self, name: &str, tag: &str) -> Result<String>;
}

/// An `Image=` value: `name[:tag]@sha256:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub name: String,
    pub tag: Option<String>,
    pub digest: String,
}

impl ImageRef {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (name_tag, digest) = value.split_once('@')?;
        // A `:` after the last `/` is the tag; one before it is a port.
        let slash = name_tag.rfind('/').map_or(0, |index| index + 1);
        let (name, tag) = name_tag[slash..]
            .rfind(':')
            .map_or((name_tag, None), |colon| {
                (
                    &name_tag[..slash + colon],
                    Some(name_tag[slash + colon + 1..].to_owned()),
                )
            });
        Some(Self {
            name: name.to_owned(),
            tag,
            digest: digest.to_owned(),
        })
    }
}

impl std::fmt::Display for ImageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.tag {
            Some(tag) => write!(f, "{}:{tag}@{}", self.name, self.digest),
            None => write!(f, "{}@{}", self.name, self.digest),
        }
    }
}

/// How far an image may move on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Policy {
    Off,
    Patch,
    Minor,
    Major,
}

impl Policy {
    fn parse(comment: &str) -> Option<Result<Self>> {
        let value = comment
            .trim_start_matches(['#', ';'])
            .trim()
            .strip_prefix("image-updates:")?
            .trim();
        Some(match value {
            "off" => Ok(Self::Off),
            "patch" => Ok(Self::Patch),
            "minor" => Ok(Self::Minor),
            "major" => Ok(Self::Major),
            other => Err(anyhow::anyhow!(
                "unknown `image-updates: {other}` (use off, patch, minor or major)"
            )),
        })
    }
}

/// A tag read as a version: `<prefix><n>(.<n>)*` then `-`-separated
/// segments, where all-digit segments (image revisions like `-5`) count as
/// further components and hash-like ones (`-cb74e7f81`) match any hash.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TagVersion {
    prefix: String,
    numbers: Vec<u64>,
    /// The suffix with numbers and hashes replaced, for matching.
    shape: String,
}

impl TagVersion {
    fn parse(tag: &str) -> Option<Self> {
        let start = tag.find(|c: char| c.is_ascii_digit())?;
        let prefix = &tag[..start];
        if !prefix
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_')
        {
            return None;
        }
        let rest = &tag[start..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let (core, suffix) = rest.split_at(end);
        if core.ends_with('.') || core.contains("..") {
            return None;
        }
        let mut numbers = core
            .split('.')
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        let mut shape = String::new();
        if !suffix.is_empty() {
            let suffix = suffix.strip_prefix('-')?;
            for segment in suffix.split('-') {
                shape.push('-');
                if !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()) {
                    numbers.push(segment.parse().ok()?);
                    shape.push('#');
                } else if is_hash(segment) {
                    shape.push_str("<hash>");
                } else {
                    shape.push_str(segment);
                }
            }
        }
        Some(Self {
            prefix: prefix.to_owned(),
            numbers,
            shape,
        })
    }

    fn same_line(&self, other: &Self) -> bool {
        self.prefix == other.prefix
            && self.shape == other.shape
            && self.numbers.len() == other.numbers.len()
    }

    /// The smallest policy that allows moving from `self` to `newer`.
    fn needs(&self, newer: &Self) -> Policy {
        let calendar = self.numbers[0] >= 1000;
        if self.numbers[0] != newer.numbers[0] && !calendar {
            Policy::Major
        } else if self.numbers.get(1) != newer.numbers.get(1) {
            Policy::Minor
        } else {
            Policy::Patch
        }
    }
}

fn is_hash(segment: &str) -> bool {
    segment.len() >= 7
        && segment
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && segment.chars().any(|c| c.is_ascii_alphabetic())
}

/// One `Image=` line that changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The template's name in the recipe (`postgres.container.tera`).
    pub template: String,
    pub from: ImageRef,
    pub to: ImageRef,
}

/// A newer tag that its policy keeps out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub template: String,
    pub image: ImageRef,
    pub available: String,
}

/// What `update-images` found for one recipe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeUpdate {
    pub id: String,
    /// `recipes/<tier>/<id>`.
    pub path: String,
    pub changes: Vec<Change>,
    pub held: Vec<Held>,
    /// Images that couldn't be checked. Any error leaves the whole recipe
    /// unchanged.
    pub errors: Vec<String>,
    /// `(from, to)` when the recipe version is bumped.
    pub version: Option<(String, String)>,
}

/// Look up every pinned image of the recipes in `only` (all when empty).
/// `published` is the published `index.json`, used to decide whether a
/// recipe's version still needs a bump.
pub fn plan(
    repo: &Repo,
    registry: &mut dyn Registry,
    only: &[String],
    published: Option<&Value>,
) -> Result<Vec<RecipeUpdate>> {
    for id in only {
        if !repo.recipes.iter().any(|recipe| &recipe.dir_name == id) {
            bail!("no recipe `{id}` under recipes/");
        }
    }
    let published = published.map(published_versions).unwrap_or_default();
    let mut cache = Cache::default();
    let mut updates = Vec::new();
    for recipe in &repo.recipes {
        if !only.is_empty() && !only.contains(&recipe.dir_name) {
            continue;
        }
        let mut update = RecipeUpdate {
            id: recipe.dir_name.clone(),
            path: recipe.path.clone(),
            ..RecipeUpdate::default()
        };
        for (template, body) in &recipe.templates {
            if template.starts_with(crate::repo::SHARED_PREFIX) {
                continue;
            }
            for (image, policy) in
                images(body).with_context(|| format!("{}/templates/{template}", recipe.path))?
            {
                if let Err(error) =
                    check_image(&mut update, template, image, policy, registry, &mut cache)
                {
                    update.errors.push(format!("{template}: {error:#}"));
                }
            }
        }
        // All or nothing: images of one app often have to move together
        // (Immich's server and machine learning), so a recipe with an image
        // that couldn't be checked keeps all of them.
        if !update.errors.is_empty() {
            update.changes.clear();
        }
        if !update.changes.is_empty() {
            update.version = version_bump(recipe, published.get(&recipe.dir_name))?;
        }
        if !update.changes.is_empty() || !update.held.is_empty() || !update.errors.is_empty() {
            updates.push(update);
        }
    }
    Ok(updates)
}

/// The images of a template with the policy of each.
fn images(body: &str) -> Result<Vec<(ImageRef, Policy)>> {
    let mut out = Vec::new();
    let mut policy = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with(['#', ';']) {
            if let Some(parsed) = Policy::parse(line) {
                policy = Some(parsed?);
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("Image=")
            && let Some(image) = ImageRef::parse(value.trim())
        {
            out.push((image, policy.unwrap_or(Policy::Minor)));
        }
        policy = None;
    }
    Ok(out)
}

#[derive(Default)]
struct Cache {
    tags: BTreeMap<String, Vec<String>>,
    digests: BTreeMap<(String, String), String>,
}

impl Cache {
    fn tags(&mut self, registry: &mut dyn Registry, name: &str) -> Result<&[String]> {
        match self.tags.entry(name.to_owned()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => Ok(entry.insert(registry.tags(name)?)),
        }
    }

    fn digest(&mut self, registry: &mut dyn Registry, name: &str, tag: &str) -> Result<String> {
        let key = (name.to_owned(), tag.to_owned());
        if let Some(digest) = self.digests.get(&key) {
            return Ok(digest.clone());
        }
        let digest = registry.digest(name, tag)?;
        if !digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        {
            bail!("{name}:{tag}: registry returned `{digest}`, not a sha256 digest");
        }
        self.digests.insert(key, digest.clone());
        Ok(digest)
    }
}

fn check_image(
    update: &mut RecipeUpdate,
    template: &str,
    image: ImageRef,
    policy: Policy,
    registry: &mut dyn Registry,
    cache: &mut Cache,
) -> Result<()> {
    if policy == Policy::Off {
        return Ok(());
    }
    // Without a tag there's nothing to follow.
    let Some(tag) = image.tag.clone() else {
        return Ok(());
    };
    let mut target = tag.clone();
    if let Some(current) = TagVersion::parse(&tag) {
        let mut newest_allowed: Option<(TagVersion, &String)> = None;
        let mut newest: Option<(TagVersion, &String)> = None;
        for candidate in cache.tags(registry, &image.name)? {
            let Some(version) = TagVersion::parse(candidate) else {
                continue;
            };
            if !current.same_line(&version) || version.numbers <= current.numbers {
                continue;
            }
            let newer = |best: &Option<(TagVersion, &String)>| {
                best.as_ref().is_none_or(|(best, best_tag)| {
                    match version.numbers.cmp(&best.numbers) {
                        Ordering::Equal => candidate > *best_tag,
                        order => order == Ordering::Greater,
                    }
                })
            };
            if current.needs(&version) <= policy && newer(&newest_allowed) {
                newest_allowed = Some((version.clone(), candidate));
            }
            if newer(&newest) {
                newest = Some((version, candidate));
            }
        }
        if let Some((_, candidate)) = &newest_allowed {
            target.clone_from(*candidate);
        }
        if let Some((_, candidate)) = newest
            && Some(candidate) != newest_allowed.as_ref().map(|(_, tag)| *tag)
        {
            update.held.push(Held {
                template: template.to_owned(),
                image: image.clone(),
                available: candidate.clone(),
            });
        }
    }
    let digest = cache.digest(registry, &image.name, &target)?;
    if target != tag || digest != image.digest {
        update.changes.push(Change {
            template: template.to_owned(),
            to: ImageRef {
                name: image.name.clone(),
                tag: Some(target),
                digest,
            },
            from: image,
        });
    }
    Ok(())
}

/// `(from, to)` if the recipe needs a new version for its changed templates:
/// always without a published index, otherwise only while it's still at the
/// newest published version.
fn version_bump(
    recipe: &Recipe,
    published: Option<&Vec<crate::history::Version>>,
) -> Result<Option<(String, String)>> {
    let current = recipe
        .doc
        .get("version")
        .and_then(Value::as_str)
        .with_context(|| format!("{}/recipe.yaml has no version", recipe.path))?;
    if let Some(newest) = published.and_then(|versions| versions.first())
        && compare_versions(current, &newest.version) == Ordering::Greater
    {
        return Ok(None);
    }
    let core = current.split(['-', '+']).next().unwrap_or(current);
    let parts: Vec<u64> = core
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .with_context(|| format!("{}: can't bump version `{current}`", recipe.path))?;
    let [major, minor, patch] = parts[..] else {
        bail!("{}: can't bump version `{current}`", recipe.path);
    };
    Ok(Some((
        current.to_owned(),
        format!("{major}.{minor}.{}", patch + 1),
    )))
}

/// Write `updates` into the checkout: the changed `Image=` lines and the
/// bumped versions.
pub fn apply(repo: &Repo, updates: &[RecipeUpdate]) -> Result<()> {
    for update in updates {
        let Some(recipe) = repo
            .recipes
            .iter()
            .find(|recipe| recipe.dir_name == update.id)
        else {
            bail!("no recipe `{}`", update.id);
        };
        let dir = repo.root.join(&recipe.path);
        let mut by_template: BTreeMap<&str, Vec<&Change>> = BTreeMap::new();
        for change in &update.changes {
            by_template
                .entry(&change.template)
                .or_default()
                .push(change);
        }
        for (template, changes) in by_template {
            let path = dir.join("templates").join(template);
            let mut text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            for change in changes {
                let from = format!("Image={}", change.from);
                anyhow::ensure!(text.contains(&from), "{}: `{from}` is gone", path.display());
                text = text.replace(&from, &format!("Image={}", change.to));
            }
            fs::write(&path, text)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        if let Some((from, to)) = &update.version {
            let path = dir.join("recipe.yaml");
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let line = format!("version: {from}");
            let mut replaced = false;
            let text: String = text
                .split_inclusive('\n')
                .map(|current| {
                    if !replaced && current.trim_end() == line {
                        replaced = true;
                        current.replacen(from.as_str(), to, 1)
                    } else {
                        current.to_owned()
                    }
                })
                .collect();
            anyhow::ensure!(replaced, "{}: no `{line}` line", path.display());
            fs::write(&path, text)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
    }
    Ok(())
}

/// A Markdown summary of `updates`, for the pull request body.
#[must_use]
pub fn summary(updates: &[RecipeUpdate]) -> String {
    let mut out = String::new();
    let changed: Vec<_> = updates.iter().filter(|u| !u.changes.is_empty()).collect();
    if changed.is_empty() {
        out.push_str("No image updates.\n");
    } else {
        out.push_str("## Image updates\n\n| Recipe | Image | From | To |\n|---|---|---|---|\n");
        for update in &changed {
            let version = update
                .version
                .as_ref()
                .map_or_else(String::new, |(from, to)| format!(" ({from} → {to})"));
            for change in &update.changes {
                writeln!(
                    out,
                    "| `{}`{version} | `{}` | {} | {} |",
                    update.id,
                    change.from.name,
                    describe(&change.from),
                    describe(&change.to),
                )
                .expect("writing to a String can't fail");
            }
        }
    }
    let held: Vec<_> = updates
        .iter()
        .flat_map(|u| u.held.iter().map(move |held| (u, held)))
        .collect();
    if !held.is_empty() {
        out.push_str(
            "\n## Held back\n\nNewer than the image's `image-updates` policy allows (majors by default). These need a person: check the upstream upgrade notes, then bump the tag by hand.\n\n| Recipe | Image | Current | Available |\n|---|---|---|---|\n",
        );
        for (update, held) in held {
            writeln!(
                out,
                "| `{}` | `{}` | `{}` | `{}` |",
                update.id,
                held.image.name,
                held.image.tag.as_deref().unwrap_or_default(),
                held.available
            )
            .expect("writing to a String can't fail");
        }
    }
    let errors: Vec<_> = updates
        .iter()
        .flat_map(|u| u.errors.iter().map(move |error| (u, error)))
        .collect();
    if !errors.is_empty() {
        out.push_str("\n## Couldn't check\n\nThese recipes were left alone this time.\n\n");
        for (update, error) in errors {
            writeln!(out, "- `{}`: {error}", update.id).expect("writing to a String can't fail");
        }
    }
    out
}

fn describe(image: &ImageRef) -> String {
    let short = image.digest.trim_start_matches("sha256:");
    let short = &short[..short.len().min(12)];
    format!(
        "`{}` (`{short}`)",
        image.tag.as_deref().unwrap_or("untagged")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_refs() {
        let image = ImageRef::parse("registry.example:5000/a/b:1.2@sha256:ab").unwrap();
        assert_eq!(image.name, "registry.example:5000/a/b");
        assert_eq!(image.tag.as_deref(), Some("1.2"));
        assert_eq!(image.to_string(), "registry.example:5000/a/b:1.2@sha256:ab");
        let untagged = ImageRef::parse("registry.example:5000/a/b@sha256:ab").unwrap();
        assert_eq!(untagged.tag, None);
        assert_eq!(ImageRef::parse("docker.io/library/nginx:alpine"), None);
    }

    fn line(a: &str, b: &str) -> bool {
        TagVersion::parse(a)
            .unwrap()
            .same_line(&TagVersion::parse(b).unwrap())
    }

    #[test]
    fn reads_tags_as_versions() {
        assert!(line("6.4.4", "6.5.0"));
        assert!(line("v0.11.1", "v0.12.0"));
        assert!(line("2026.10.1-java25", "2026.11.0-java25"));
        assert!(line("2026.10.27-cb74e7f81", "2026.10.30-0a1b2c3d4"));
        assert!(line("v1.6.870-5", "v1.6.871-1"));
        assert!(!line("6.4.4", "6.4.4.10685-ls290"));
        assert!(!line("v0.11.1", "0.12.0"));
        assert!(!line("4.5.2", "4.6.0-pre.1"));
        assert!(!line("2026.10.1-java25", "2026.10.1-java21"));
        assert!(!line("v26.9.1", "v26.9.1-maxperf"));
        assert_eq!(TagVersion::parse("alpine"), None);
        assert_eq!(TagVersion::parse("latest"), None);
        assert_eq!(
            TagVersion::parse("v1.6.870-5").unwrap().numbers,
            [1, 6, 870, 5]
        );
    }

    #[test]
    fn classifies_moves() {
        let needs = |a: &str, b: &str| {
            TagVersion::parse(a)
                .unwrap()
                .needs(&TagVersion::parse(b).unwrap())
        };
        assert_eq!(needs("1.2.3", "1.2.4"), Policy::Patch);
        assert_eq!(needs("1.2.3", "1.3.0"), Policy::Minor);
        assert_eq!(needs("1.2.3", "2.0.0"), Policy::Major);
        assert_eq!(needs("18", "19"), Policy::Major);
        assert_eq!(needs("0.64.2", "0.65.0"), Policy::Minor);
        assert_eq!(needs("2026.10.1-java25", "2027.1.0-java25"), Policy::Minor);
    }

    #[test]
    fn reads_policies() {
        let body = "Image=a/b:1.0.0@sha256:1\n# image-updates: major\nImage=c/d:2.0.0@sha256:2\n# image-updates: off\n\nImage=e/f:3@sha256:3\n";
        let policies: Vec<_> = images(body).unwrap().into_iter().map(|(_, p)| p).collect();
        // A blank line ends a directive.
        assert_eq!(policies, [Policy::Minor, Policy::Major, Policy::Minor]);
        assert!(images("# image-updates: sometimes\nImage=a:1@sha256:1\n").is_err());
    }
}
