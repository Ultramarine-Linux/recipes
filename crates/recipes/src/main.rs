//! `recipes`: maintain the recipe catalog in this repository.
//!
//! ```sh
//! cargo run -p recipes -- extract   # after changing a user-facing string
//! cargo run -p recipes -- check     # everything CI and the origin enforce
//! cargo run -p recipes -- build     # writes dist/v1/
//! ```

use std::{fs, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use recipes_catalog::{CheckOptions, Repo, build, check, extract};

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
