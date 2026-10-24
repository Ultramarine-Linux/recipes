//! `recipes`: maintain the recipe catalog in this repository.
//!
//! ```sh
//! cargo run -p recipes -- extract   # after changing a user-facing string
//! cargo run -p recipes -- check     # everything CI and the origin enforce
//! cargo run -p recipes -- build     # writes dist/v1/
//! cargo run -p recipes -- render <id> [--values values.yaml] [--output-dir dir]
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use recipes_catalog::{CheckOptions, Repo, build, check, extract, repo::is_plain_relative_path};
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
            let previous_index = previous
                .map(|path| -> Result<_> {
                    let text = fs::read_to_string(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?;
                    serde_json::from_str(&text)
                        .with_context(|| format!("failed to parse {}", path.display()))
                })
                .transpose()?;
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
        Command::Build { out } => {
            let report = check(&repo, &CheckOptions::default())?;
            if !report.is_ok() {
                return Ok(finish(&report));
            }
            let catalog = build(&repo)?;
            let base = catalog.write(&out)?;
            println!(
                "Built {} recipe(s) into {}.",
                catalog.recipe_count(),
                base.display()
            );
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
