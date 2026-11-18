//! `recipes`: maintain the recipe catalog in this repository.
//!
//! ```sh
//! cargo run -p recipes -- extract   # after changing a user-facing string
//! cargo run -p recipes -- check     # everything CI and the origin enforce
//! cargo run -p recipes -- build     # writes dist/v1/ (--previous <v1 dir> keeps old versions)
//! cargo run -p recipes -- render <id> [--values values.yaml] [--output-dir dir]
//! cargo run -p recipes -- update-images [<id>...] [--previous index.json] [--dry-run]
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as Process, ExitCode},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use recipes_catalog::{
    CheckOptions, Published, Repo, build, check, extract, images, repo::is_plain_relative_path,
};
use sha2::{Digest, Sha256};
use tetra_recipe::{AppRecipe, load_values};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Root of the recipes checkout.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate recipes, templates and translations.
    Check {
        /// A previously published index.json: recipes whose bundle changed
        /// since must bump their version.
        #[arg(long)]
        previous: Option<PathBuf>,
        /// Don't print translation completeness and bundle digests.
        #[arg(long, short)]
        quiet: bool,
    },
    /// Regenerate the `en.json` translation sources from the YAML.
    Extract,
    /// Check, then write the published catalog to `<out>/v1/`.
    Build {
        #[arg(long, default_value = "dist")]
        out: PathBuf,
        /// A previously published `v1/` directory: its versions of each
        /// recipe stay listed (and their bundles are copied over), and the
        /// version checks run against it.
        #[arg(long)]
        previous: Option<PathBuf>,
    },
    /// Move pinned images to newer tags in their release line (or refresh
    /// their digests) and bump the versions of recipes that changed. Needs
    /// `skopeo`. Major versions are only reported.
    UpdateImages {
        /// Only these recipes (all when omitted).
        ids: Vec<String>,
        /// The published index.json: recipes already ahead of it aren't
        /// bumped again.
        #[arg(long)]
        previous: Option<PathBuf>,
        /// Report without changing any file.
        #[arg(long)]
        dry_run: bool,
        /// Also write the Markdown summary here (for a pull request body).
        #[arg(long)]
        summary: Option<PathBuf>,
    },
    /// Render a recipe through tetra with the templates its bundle would
    /// carry (its own and `shared/`), like a host would.
    Render {
        /// The recipe id (its directory name).
        id: String,
        /// Parameter values, as a YAML map. Defaults fill in the rest.
        #[arg(long)]
        values: Option<PathBuf>,
        /// Write the rendered files here instead of printing them.
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let repo = Repo::load(&cli.root)?;
    match cli.command {
        Command::Check { previous, quiet } => {
            let previous_index = previous.as_deref().map(read_index).transpose()?;
            let report = check(&repo, &CheckOptions { previous_index })?;
            if !quiet {
                for line in report.recipes.iter().chain(&report.translations) {
                    println!("{line}");
                }
            }
            Ok(finish(&report))
        }
        Command::Extract => {
            let written = extract(&repo)?;
            for path in &written {
                let path = path.strip_prefix(&cli.root).unwrap_or(path);
                println!("updated {}", path.display());
            }
            println!("{} translation source(s) updated.", written.len());
            Ok(ExitCode::SUCCESS)
        }
        Command::Build { out, previous } => {
            let previous = previous.as_deref().map(Published::load).transpose()?;
            let options = CheckOptions {
                previous_index: previous.as_ref().map(|published| published.index.clone()),
            };
            let report = check(&repo, &options)?;
            if !report.is_ok() {
                return Ok(finish(&report));
            }
            let catalog = build(&repo, previous.as_ref())?;
            for warning in &catalog.warnings {
                eprintln!("warning: {warning}");
            }
            let base = catalog.write(&out)?;
            println!(
                "Built {} recipe(s) into {}.",
                catalog.recipe_count(),
                base.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::UpdateImages {
            ids,
            previous,
            dry_run,
            summary,
        } => {
            let previous = previous.as_deref().map(read_index).transpose()?;
            let updates = images::plan(&repo, &mut Skopeo, &ids, previous.as_ref())?;
            if !dry_run {
                images::apply(&repo, &updates)?;
            }
            let text = images::summary(&updates);
            print!("{text}");
            if let Some(path) = summary {
                fs::write(&path, &text)
                    .with_context(|| format!("failed to write {}", path.display()))?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Render {
            id,
            values,
            output_dir,
        } => {
            render(&repo, &id, values.as_deref(), output_dir.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn render(repo: &Repo, id: &str, values: Option<&Path>, output_dir: Option<&Path>) -> Result<()> {
    let Some(recipe) = repo.recipes.iter().find(|recipe| recipe.dir_name == id) else {
        bail!("no recipe `{id}` under recipes/");
    };
    if let Some(name) = recipe
        .missing_templates
        .iter()
        .chain(&recipe.invalid_templates)
        .next()
    {
        bail!(
            "{}: template `{name}` can't be loaded; run `check`",
            recipe.path
        );
    }
    let parsed = AppRecipe::load_str(&recipe.yaml)
        .with_context(|| format!("{}/recipe.yaml", recipe.path))?;
    let values = load_values(values)?;
    let resources = parsed.render_with_templates(&values, &recipe.templates)?;
    for resource in resources {
        if let Some(dir) = output_dir {
            if !is_plain_relative_path(&resource.filename) {
                bail!(
                    "rendered file name `{}` isn't a plain relative path",
                    resource.filename
                );
            }
            let path = dir.join(&resource.filename);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&path, &resource.contents)
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("wrote {}", path.display());
        } else {
            println!("--- {}\n{}", resource.filename, resource.contents);
        }
    }
    Ok(())
}

fn read_index(path: &Path) -> Result<serde_json::Value> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// Registry lookups through `skopeo`, which handles registry auth, Docker
/// Hub's quirks and tag pagination.
struct Skopeo;

impl Skopeo {
    fn run(args: &[&str]) -> Result<Vec<u8>> {
        let mut last = None;
        // Registries flake; retry once.
        for _ in 0..2 {
            let output = Process::new("skopeo")
                // Listing every tag of a busy repository (thousands of PR builds)
                // can take minutes.
                .args(["--command-timeout", "5m"])
                .args(args)
                .output()
                .context("failed to run skopeo (is it installed?)")?;
            if output.status.success() {
                return Ok(output.stdout);
            }
            last = Some(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        bail!("skopeo {}: {}", args.join(" "), last.unwrap_or_default())
    }
}

impl images::Registry for Skopeo {
    fn tags(&mut self, name: &str) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct Tags {
            #[serde(rename = "Tags")]
            tags: Vec<String>,
        }
        let output = Self::run(&["list-tags", &format!("docker://{name}")])?;
        Ok(serde_json::from_slice::<Tags>(&output)
            .with_context(|| format!("unexpected `skopeo list-tags` output for {name}"))?
            .tags)
    }

    /// The digest of the manifest (or index) exactly as the registry serves
    /// it, which is what `Image=…@sha256:` pins.
    fn digest(&mut self, name: &str, tag: &str) -> Result<String> {
        let manifest = Self::run(&["inspect", "--raw", &format!("docker://{name}:{tag}")])?;
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(&manifest))))
    }
}

fn finish(report: &recipes_catalog::Report) -> ExitCode {
    for error in &report.errors {
        eprintln!("error: {error}");
    }
    if report.is_ok() {
        println!("Catalog OK.");
        ExitCode::SUCCESS
    } else {
        eprintln!("\n{} problem(s) found.", report.errors.len());
        ExitCode::FAILURE
    }
}
