//! Building and publishing releases. Layout under `DATA_DIR`:
//!
//! ```text
//! current -> <DATA_DIR>/releases/<sha>   served by Caddy; swapped atomically
//! releases/<sha>/v1/...                  the built catalog of a commit
//! releases/.tmp-*                        a release being written
//! work/<sha>/                            source tree while building
//! ```
//!
//! `current` only ever moves to a fully written release, so a failed or
//! interrupted build leaves the live catalog untouched.

use std::{
    fs,
    future::Future,
    io::Read,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use tar::EntryType;

use crate::webhook::is_sha;

/// Newest releases kept on disk (the live one is always kept too).
pub const KEEP_RELEASES: usize = 3;
/// Limits for an extracted source tree.
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub sha: String,
    /// When `current` was last pointed somewhere.
    pub built_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    AlreadyLive,
    Published,
}

/// Create the directories, and clear what an interrupted run left behind.
pub fn prepare(data_dir: &Path) -> Result<()> {
    let work = data_dir.join("work");
    if work.exists() {
        fs::remove_dir_all(&work).with_context(|| format!("failed to clear {}", work.display()))?;
    }
    fs::create_dir_all(&work)?;
    let releases = data_dir.join("releases");
    fs::create_dir_all(&releases)?;
    for entry in fs::read_dir(&releases)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            fs::remove_dir_all(&path)?;
        }
    }
    for entry in fs::read_dir(data_dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".current-"))
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The live release, or `None` when `current` is missing or dangling.
#[must_use]
pub fn current(data_dir: &Path) -> Option<Live> {
    let link = data_dir.join("current");
    let target = fs::read_link(&link).ok()?;
    if !link.join("v1/index.json").is_file() {
        return None;
    }
    Some(Live {
        sha: target.file_name()?.to_str()?.to_owned(),
        built_at: fs::symlink_metadata(&link).ok()?.modified().ok()?,
    })
}

/// Point `current` at `releases/<sha>` (absolute): write a fresh symlink
/// and rename it over the old one, so readers see either release, never
/// neither.
pub fn activate(data_dir: &Path, sha: &str) -> Result<()> {
    let target = data_dir.join("releases").join(sha);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = data_dir.join(format!(".current-{}-{nanos}", std::process::id()));
    std::os::unix::fs::symlink(&target, &temp)
        .with_context(|| format!("failed to create {}", temp.display()))?;
    if let Err(error) = fs::rename(&temp, data_dir.join("current")) {
        let _removed = fs::remove_file(&temp);
        return Err(error).context("failed to swap `current`");
    }
    Ok(())
}

/// Delete all but the newest `keep` releases, never the live one. Returns
/// the removed shas.
pub fn prune(data_dir: &Path, keep: usize) -> Result<Vec<String>> {
    let live = current(data_dir).map(|live| live.sha);
    let mut releases = Vec::new();
    for entry in fs::read_dir(data_dir.join("releases"))? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        releases.push((entry.metadata()?.modified()?, name));
    }
    // Newest first; the name breaks ties so pruning is deterministic.
    releases.sort_by(|a, b| b.cmp(a));
    let mut removed = Vec::new();
    for (_, name) in releases.into_iter().skip(keep) {
        if Some(&name) == live.as_ref() {
            continue;
        }
        fs::remove_dir_all(data_dir.join("releases").join(&name))?;
        removed.push(name);
    }
    Ok(removed)
}

/// Unpack a GitHub source tarball into `dest`, dropping the top-level
/// `<repo>-<sha>/` directory. Rejects absolute paths, `..`, links that could
/// point outside `dest`, and special files.
pub fn extract_tarball(gzipped: impl Read, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    let mut archive = tar::Archive::new(GzDecoder::new(gzipped));
    let mut total = 0_u64;
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < MAX_SOURCE_ENTRIES, "archive has too many entries");
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        // GitHub stores the commit id in a global pax header.
        if matches!(kind, EntryType::XGlobalHeader | EntryType::XHeader) {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Some(relative) = strip_top_level(&path) else {
            bail!("unsafe path in archive: {}", path.display());
        };
        total = total.saturating_add(entry.size());
        ensure!(total <= MAX_SOURCE_BYTES, "archive is too large");
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(&relative);
        match kind {
            EntryType::Directory => fs::create_dir_all(&target)?,
            EntryType::Regular | EntryType::Continuous => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                entry.set_preserve_permissions(false);
                entry.set_preserve_mtime(false);
                entry.set_unpack_xattrs(false);
                entry
                    .unpack(&target)
                    .with_context(|| format!("failed to unpack {}", relative.display()))?;
            }
            EntryType::Symlink => {
                let link = entry
                    .link_name()?
                    .with_context(|| format!("symlink without a target: {}", path.display()))?;
                // Only downward links: with no `..` or root anywhere, no chain
                // of links can leave `dest`.
                ensure!(
                    !link.as_os_str().is_empty()
                        && link
                            .components()
                            .all(|part| matches!(part, Component::Normal(_))),
                    "symlink {} -> {} could point outside the source tree",
                    path.display(),
                    link.display()
                );
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                std::os::unix::fs::symlink(&*link, &target)?;
            }
            other => bail!("unsupported {other:?} entry in archive: {}", path.display()),
        }
    }
    Ok(())
}

/// `top/rest` → `rest` (empty for `top` itself), or `None` unless every
/// component is a plain name.
fn strip_top_level(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) {
        return None;
    }
    let rest = components.as_path();
    rest.components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then(|| rest.to_path_buf())
}

/// Build `sha` and make it live. `fetch` writes the source tree into the
/// directory it's given. Failures leave `current` untouched.
pub async fn build_release<F, Fut>(data_dir: &Path, sha: &str, fetch: F) -> Result<Outcome>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    ensure!(is_sha(sha), "not a commit sha: {sha}");
    if current(data_dir).is_some_and(|live| live.sha == sha) {
        tracing::info!("{sha} is already live");
        return Ok(Outcome::AlreadyLive);
    }

    let work = data_dir.join("work").join(sha);
    let result = async {
        remove_dir_if_exists(&work)?;
        fetch(work.clone()).await?;
        let data_dir = data_dir.to_owned();
        let (work, sha) = (work.clone(), sha.to_owned());
        tokio::task::spawn_blocking(move || write_release(&data_dir, &work, &sha)).await?
    }
    .await;
    if let Err(error) = remove_dir_if_exists(&work) {
        tracing::warn!("failed to clean {}: {error:#}", work.display());
    }
    result?;

    activate(data_dir, sha)?;
    tracing::info!("{sha} is live");
    match prune(data_dir, KEEP_RELEASES) {
        Ok(removed) if !removed.is_empty() => tracing::info!("pruned {}", removed.join(", ")),
        Ok(_) => {}
        Err(error) => tracing::warn!("failed to prune releases: {error:#}"),
    }
    Ok(Outcome::Published)
}

/// Check and build the source in `work` into `releases/<sha>`.
fn write_release(data_dir: &Path, work: &Path, sha: &str) -> Result<()> {
    let (_, catalog) = recipes_catalog::check_and_build(work)?;
    let releases = data_dir.join("releases");
    let temp = releases.join(format!(".tmp-{sha}"));
    remove_dir_if_exists(&temp)?;
    catalog.write(&temp)?;
    let release = releases.join(sha);
    // A rebuild of an older commit (a revert): replace it, it isn't live.
    remove_dir_if_exists(&release)?;
    fs::rename(&temp, &release).with_context(|| format!("failed to move {}", release.display()))?;
    tracing::info!("built {} recipe(s) for {sha}", catalog.recipe_count());
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("failed to remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use flate2::{Compression, write::GzEncoder};
    use tar::Header;

    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    enum Item<'a> {
        File(&'a str, &'a str),
        Link(&'a str, &'a str),
        Hardlink(&'a str, &'a str),
    }

    /// A gzipped tarball. Names are written raw, so they can be malicious.
    fn tarball(items: &[Item<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for item in items {
            let mut header = Header::new_gnu();
            let (name, kind, size, link) = match item {
                Item::File(name, body) => (name, EntryType::Regular, body.len(), None),
                Item::Link(name, target) => (name, EntryType::Symlink, 0, Some(target)),
                Item::Hardlink(name, target) => (name, EntryType::Link, 0, Some(target)),
            };
            let gnu = header.as_gnu_mut().unwrap();
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(link) = link {
                gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
            header.set_entry_type(kind);
            header.set_size(size as u64);
            header.set_mode(0o644);
            header.set_cksum();
            let body = match item {
                Item::File(_, body) => body.as_bytes(),
                _ => &[],
            };
            builder.append(&header, body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_and_strips_the_top_level_directory() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tarball(&[
            Item::File("recipes-abc/catalog.yaml", "name: x\n"),
            Item::File("recipes-abc/templates/a/b.tera", "body"),
            Item::Link("recipes-abc/link", "templates/a"),
        ]);
        extract_tarball(archive.as_slice(), dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("catalog.yaml")).unwrap(),
            "name: x\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("link/b.tera")).unwrap(),
            "body"
        );
    }

    #[test]
    fn rejects_escaping_paths() {
        let outside = tempfile::tempdir().unwrap();
        let canary = outside.path().join("canary");
        for items in [
            vec![Item::File("top/../../canary", "x")],
            vec![Item::File("/canary", "x")],
            vec![Item::File("../canary", "x")],
            vec![Item::Link("top/l", "../..")],
            vec![Item::Link("top/l", "/etc")],
            vec![Item::Link("top/l", "a/../../x")],
            vec![Item::Hardlink("top/l", "/etc/passwd")],
        ] {
            let dest = outside.path().join("dest");
            remove_dir_if_exists(&dest).unwrap();
            let archive = tarball(&items);
            assert!(extract_tarball(archive.as_slice(), &dest).is_err());
            assert!(!canary.exists());
        }
    }

    fn release(data: &Path, sha: &str) {
        fs::create_dir_all(data.join("releases").join(sha).join("v1")).unwrap();
        fs::write(data.join("releases").join(sha).join("v1/index.json"), sha).unwrap();
    }

    #[test]
    fn swaps_current_atomically() {
        let data = tempfile::tempdir().unwrap();
        prepare(data.path()).unwrap();
        assert_eq!(current(data.path()), None);
        release(data.path(), SHA_A);
        activate(data.path(), SHA_A).unwrap();
        assert_eq!(current(data.path()).unwrap().sha, SHA_A);
        let link = fs::read_link(data.path().join("current")).unwrap();
        assert!(link.is_absolute());
        assert_eq!(link, data.path().join("releases").join(SHA_A));

        release(data.path(), SHA_B);
        activate(data.path(), SHA_B).unwrap();
        assert_eq!(
            fs::read_to_string(data.path().join("current/v1/index.json")).unwrap(),
            SHA_B
        );
        // No temporary links left behind.
        let names: Vec<_> = fs::read_dir(data.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|name| !name.starts_with(".current")),
            "{names:?}"
        );
    }

    #[test]
    fn prunes_old_releases_but_not_the_live_one() {
        let data = tempfile::tempdir().unwrap();
        prepare(data.path()).unwrap();
        let shas: Vec<String> = (0..5).map(|i| format!("{i}").repeat(40)).collect();
        for sha in &shas {
            release(data.path(), sha);
            // Distinct mtimes, oldest first.
            std::thread::sleep(Duration::from_millis(15));
        }
        activate(data.path(), &shas[0]).unwrap();
        let mut removed = prune(data.path(), 3).unwrap();
        removed.sort();
        assert_eq!(removed, [shas[1].clone()]);
        assert!(data.path().join("releases").join(&shas[0]).exists());
    }

    /// A minimal recipes checkout, as a fetch would write it.
    fn write_source(dir: &Path, version: &str) -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for file in [
            "catalog.yaml",
            "locales/en.json",
            "schema/recipe.schema.json",
            "schema/catalog.schema.json",
        ] {
            let target = dir.join(file);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::copy(root.join(file), target)?;
        }
        let recipe = dir.join("recipes/official/demo");
        fs::create_dir_all(recipe.join("locales"))?;
        fs::write(
            recipe.join("recipe.yaml"),
            format!(
                "recipe_id: demo\nname: Demo\nversion: {version}\ncategory: web\nresources:\n  - type: file\n    filename: a.txt\n    template: files/a.txt.tera\n"
            ),
        )?;
        fs::write(
            recipe.join("locales/en.json"),
            "{\n\t\"name\": \"Demo\"\n}\n",
        )?;
        fs::create_dir_all(dir.join("templates/files"))?;
        fs::write(dir.join("templates/files/a.txt.tera"), "hello\n")?;
        Ok(())
    }

    #[tokio::test]
    async fn builds_in_process_and_publishes() {
        let data = tempfile::tempdir().unwrap();
        prepare(data.path()).unwrap();

        let outcome = build_release(data.path(), SHA_A, |dir| async move {
            write_source(&dir, "1.0.0")
        })
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Published);
        assert_eq!(current(data.path()).unwrap().sha, SHA_A);
        let index = fs::read_to_string(data.path().join("current/v1/index.json")).unwrap();
        assert!(index.contains("\"id\": \"demo\""), "{index}");
        assert!(
            fs::read_dir(data.path().join("work"))
                .unwrap()
                .next()
                .is_none()
        );

        let again = build_release(data.path(), SHA_A, |_| async { bail!("not fetched") }).await;
        assert_eq!(again.unwrap(), Outcome::AlreadyLive);

        // A failing check leaves the live release alone.
        let failed = build_release(data.path(), SHA_B, |dir| async move {
            write_source(&dir, "1.0.0")?;
            fs::write(dir.join("templates/unused.tera"), "")?;
            Ok(())
        })
        .await;
        let error = format!("{:#}", failed.unwrap_err());
        assert!(
            error.contains("templates/unused.tera is not used"),
            "{error}"
        );
        assert_eq!(current(data.path()).unwrap().sha, SHA_A);
        assert!(!data.path().join("releases").join(SHA_B).exists());
        assert!(
            fs::read_dir(data.path().join("work"))
                .unwrap()
                .next()
                .is_none()
        );

        let fetch_failed = build_release(data.path(), SHA_B, |_| async { bail!("offline") }).await;
        assert!(fetch_failed.is_err());
        assert_eq!(current(data.path()).unwrap().sha, SHA_A);
    }
}
