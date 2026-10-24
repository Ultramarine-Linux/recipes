//! Validating a checkout before it can be published.
//!
//! Recipes go through tetra's own parser and policies (`tetra-recipe`), so a
//! recipe that passes here is one hosts accept:
//!
//! - `AppRecipe::load_str` (parse + `validate`), every parameter default
//!   through `Parameter::validate_json`, and a render of every recipe with
//!   sample values through `render_with_templates`
//! - `verify_images` (the pinned image policy) on every Quadlet template
//!
//! On top of that, the repository's own rules:
//!
//! - `recipe.yaml` and `catalog.yaml` match `schema/*.json`, which catches
//!   typos tetra would silently ignore (it skips unknown fields)
//! - recipes live in `recipes/<tier>/<recipe_id>/`, ids are unique across
//!   tiers, and categories are defined in `catalog.yaml`
//! - parameter fields only appear on the types that use them
//! - every referenced template exists (in the recipe's `templates/`, or in
//!   `shared/` for `shared/...` names), Quadlet templates are named after
//!   their unit type, every recipe template is used by its recipe, and every
//!   shared template by some recipe
//! - shared templates don't set an image: anything with an image pin belongs
//!   to one recipe, so bumping it never changes another recipe
//! - every `en.json` is up to date with the YAML, and translations only hold
//!   strings under keys from it
//! - optionally, a recipe whose bundle changed since the last publish must
//!   have bumped its `version`

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result};
use jsonschema::Validator;
use serde_json::Value;
use serde_norway::Value as YamlValue;
use tetra_recipe::{
    AppRecipe, ParameterKind, RenderedResource, ResourceKind,
    verification::{bundle_sha256, verify_images},
};

use crate::{
    json,
    locales::{catalog_source, flatten, is_blank, recipe_source},
    repo::{Locales, Recipe, Repo, SHARED_PREFIX, SOURCE_LOCALE, Tier},
};

/// The command that regenerates `en.json` files.
pub const EXTRACT_COMMAND: &str = "cargo run -p recipes -- extract";

#[derive(Debug, Default, Clone)]
pub struct CheckOptions {
    /// A previously published `index.json`. Recipes whose bundle digest
    /// changed since must have a different `version`.
    pub previous_index: Option<Value>,
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Problems that block publishing.
    pub errors: Vec<String>,
    /// `<recipe>/<locale>: <translated>/<total> strings` lines.
    pub translations: Vec<String>,
    /// `<id> <version> (<tier>): <bundle sha256>` lines.
    pub recipes: Vec<String>,
}

impl Report {
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }
}

/// Check `repo`. Only fails when the schemas themselves are unusable;
/// problems with the catalog end up in the [`Report`].
pub fn check(repo: &Repo, options: &CheckOptions) -> Result<Report> {
    let recipe_schema = schema(&repo.root, "recipe.schema.json")?;
    let catalog_schema = schema(&repo.root, "catalog.schema.json")?;
    let mut report = Report::default();

    schema_errors(&mut report, "catalog.yaml", &catalog_schema, &repo.catalog);
    let categories: BTreeSet<&str> = repo
        .catalog
        .get("categories")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, _)| id.as_str())
        .collect();
    let source = catalog_source(&repo.catalog);
    check_source_locale(
        &mut report,
        "locales",
        repo.catalog_source_locale.as_deref(),
        &source,
    );
    check_translations(&mut report, "locales", &repo.catalog_locales, &source);

    for dir in &repo.stray_dirs {
        let tiers: Vec<_> = Tier::ALL.iter().map(|tier| tier.as_str()).collect();
        report.error(format!(
            "recipes/{dir}: recipes go in recipes/{{{}}}/<recipe_id>/",
            tiers.join(",")
        ));
    }

    if repo.legacy_templates_dir {
        report.error(
            "templates/: templates now live in recipes/<tier>/<id>/templates/, or shared/ for image-free ones",
        );
    }

    // Keyed by display path, so a shared template is checked once.
    let mut referenced: BTreeMap<String, &String> = BTreeMap::new();
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let mut digests = BTreeMap::new();
    for recipe in &repo.recipes {
        for (name, body) in &recipe.templates {
            referenced.insert(recipe.template_display_path(name), body);
        }
        for name in &recipe.template_files {
            if !recipe.templates.contains_key(name) {
                report.error(format!(
                    "{}/templates/{name} is not used by the recipe",
                    recipe.path
                ));
            }
        }
        if recipe
            .template_files
            .iter()
            .any(|name| name.starts_with(SHARED_PREFIX))
        {
            report.error(format!(
                "{}/templates/shared/: `shared/...` names refer to the top-level shared/; rename the directory",
                recipe.path
            ));
        }
        if let Some(first) = seen.insert(&recipe.dir_name, &recipe.path) {
            report.error(format!(
                "{}: recipe id is already used by {first}",
                recipe.path
            ));
        }
        if let Some(digest) = check_recipe(&mut report, recipe, &recipe_schema, &categories) {
            digests.insert(recipe.dir_name.as_str(), digest);
        }
    }

    for (path, body) in &referenced {
        if path.starts_with(SHARED_PREFIX) {
            check_shared_has_no_image(&mut report, path, body);
        } else {
            check_template_images(&mut report, path, body);
        }
    }
    for name in &repo.shared_files {
        if !referenced.contains_key(&format!("{SHARED_PREFIX}{name}")) {
            report.error(format!("{SHARED_PREFIX}{name} is not used by any recipe"));
        }
    }

    if let Some(previous) = &options.previous_index {
        check_versions(&mut report, repo, &digests, previous);
    }
    Ok(report)
}

/// Check one recipe; returns `(version, digest)` if tetra accepts it.
fn check_recipe(
    report: &mut Report,
    recipe: &Recipe,
    schema: &Validator,
    categories: &BTreeSet<&str>,
) -> Option<(String, String)> {
    let at = &recipe.path;
    let before = report.errors.len();
    schema_errors(report, &format!("{at}/recipe.yaml"), schema, &recipe.doc);

    let id = recipe.doc.get("recipe_id").and_then(Value::as_str);
    if id.is_some_and(|id| id != recipe.dir_name) {
        report.error(format!(
            "{at}: recipe_id `{}` must match the directory name",
            id.unwrap_or_default()
        ));
    }
    if let Some(category) = recipe.doc.get("category").and_then(Value::as_str)
        && !categories.contains(category)
    {
        report.error(format!(
            "{at}: category `{category}` is not defined in catalog.yaml"
        ));
    }

    for name in &recipe.invalid_templates {
        report.error(format!(
            "{at}: template `{name}` must be a relative path in the recipe's templates/ or shared/ (no `.`, `..` or empty segments)"
        ));
    }
    for name in &recipe.missing_templates {
        report.error(format!(
            "{at}: template `{name}` does not exist ({})",
            recipe.template_display_path(name)
        ));
    }

    let source = recipe_source(&recipe.doc);
    let locales = format!("{at}/locales");
    check_source_locale(report, &locales, recipe.source_locale.as_deref(), &source);
    check_translations(report, &locales, &recipe.locales, &source);

    let parsed = match AppRecipe::load_str(&recipe.yaml) {
        Ok(parsed) => parsed,
        Err(error) => {
            report.error(format!("{at}/recipe.yaml: tetra rejects it: {error:#}"));
            return None;
        }
    };
    check_parameters(report, at, &parsed);
    check_resources(report, at, &parsed);
    // Rendering needs every template; their absence is reported above.
    if recipe.missing_templates.is_empty() && recipe.invalid_templates.is_empty() {
        check_render(report, at, &parsed, &recipe.templates);
    }

    let digest = bundle_sha256(&recipe.yaml, &recipe.templates);
    report.recipes.push(format!(
        "{} {} ({}): {digest}",
        parsed.recipe_id,
        parsed.version,
        recipe.tier.as_str()
    ));
    (report.errors.len() == before).then_some((parsed.version, digest))
}

/// Field combinations tetra accepts but ignores, and defaults it would only
/// reject at install time.
fn check_parameters(report: &mut Report, at: &str, recipe: &AppRecipe) {
    for parameter in &recipe.parameters {
        let at = format!("{at}: parameter `{}`", parameter.key);
        let kind = &parameter.kind;
        if !parameter.options.is_empty() && *kind != ParameterKind::Choice {
            report.error(format!("{at}: only choice parameters take options"));
        }
        if (parameter.min.is_some() || parameter.max.is_some()) && *kind != ParameterKind::Integer {
            report.error(format!("{at}: only integer parameters take min/max"));
        }
        if let (Some(min), Some(max)) = (parameter.min, parameter.max)
            && min > max
        {
            report.error(format!("{at}: min is above max"));
        }
        if parameter.generate.is_some()
            && !matches!(kind, ParameterKind::String | ParameterKind::Secret)
        {
            report.error(format!(
                "{at}: only string and secret parameters can be generated"
            ));
        }
        if parameter.generate.is_some() && parameter.default.is_some() {
            report.error(format!(
                "{at}: has both a default and a generator; the generator never runs"
            ));
        }
        if let Some(default) = &parameter.default {
            let valid = serde_json::to_value(default)
                .map_err(anyhow::Error::from)
                .and_then(|default| parameter.validate_json(&default));
            if let Err(error) = valid {
                report.error(format!("{at}: invalid default: {error:#}"));
            }
        }
    }
}

/// Quadlet templates must be named `*.<type>.tera`, so the image policy below
/// knows which unit type a template is.
fn check_resources(report: &mut Report, at: &str, recipe: &AppRecipe) {
    for resource in &recipe.resources {
        let ext = resource.kind.ext();
        if resource.kind != ResourceKind::File
            && !resource
                .template
                .strip_suffix(".tera")
                .is_some_and(|name| name.ends_with(ext))
        {
            report.error(format!(
                "{at}: template `{}` of a {} resource must be named `*{ext}.tera`",
                resource.template,
                ext.trim_start_matches('.')
            ));
        }
    }
}

/// Tetra's pinned image policy, on the template text: image lines are
/// literal in templates, so this covers every resource whatever the
/// parameters. The unit type comes from the file name (`x.container.tera`).
fn check_template_images(report: &mut Report, path: &str, body: &str) {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let resource = RenderedResource {
        kind: ResourceKind::File,
        filename: filename
            .strip_suffix(".tera")
            .unwrap_or(filename)
            .to_owned(),
        contents: body.to_owned(),
    };
    if let Err(error) = verify_images(&[resource], false, None) {
        report.error(format!("{path}: {error:#}"));
    }
}

/// Shared templates stay image-free: an image pin belongs to one recipe.
fn check_shared_has_no_image(report: &mut Report, path: &str, body: &str) {
    let sets_image = body.lines().any(|line| {
        line.trim_start()
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "Image")
    });
    if sets_image {
        report.error(format!(
            "{path}: shared templates can't set `Image=`; give each recipe its own copy"
        ));
    }
}

/// Render through tetra with sample values: once with every boolean false
/// and once with every boolean true, so conditional resources render too.
fn check_render(
    report: &mut Report,
    at: &str,
    recipe: &AppRecipe,
    templates: &BTreeMap<String, String>,
) {
    let mut errors = BTreeSet::new();
    for flag in [false, true] {
        let values = sample_values(recipe, flag);
        if let Err(error) = recipe.render_with_templates(&values, templates) {
            errors.insert(format!("{error:#}"));
        }
    }
    for error in errors {
        report.error(format!("{at}: fails to render with sample values: {error}"));
    }
}

fn sample_values(recipe: &AppRecipe, flag: bool) -> BTreeMap<String, YamlValue> {
    let mut values = BTreeMap::new();
    for parameter in &recipe.parameters {
        let value = match parameter.kind {
            ParameterKind::Boolean => YamlValue::Bool(flag),
            _ if parameter.default.is_some() || parameter.generate.is_some() => continue,
            ParameterKind::String | ParameterKind::Secret => YamlValue::String("example".into()),
            ParameterKind::Integer => {
                YamlValue::Number(parameter.min.or(parameter.max).unwrap_or(1).into())
            }
            ParameterKind::Choice => match parameter.options.first() {
                Some(option) => YamlValue::String(option.clone()),
                None => continue,
            },
        };
        values.insert(parameter.key.clone(), value);
    }
    values
}

fn check_source_locale(report: &mut Report, at: &str, actual: Option<&str>, source: &Value) {
    if actual != Some(json::to_string(source).as_str()) {
        report.error(format!(
            "{at}: {SOURCE_LOCALE}.json is stale; run `{EXTRACT_COMMAND}`"
        ));
    }
}

fn check_translations(report: &mut Report, at: &str, locales: &Locales, source: &Value) {
    let keys: BTreeSet<String> = flatten(source).into_iter().map(|(key, _)| key).collect();
    for (locale, catalog) in locales {
        let at = format!("{at}/{locale}.json");
        if !catalog.is_object() {
            report.error(format!("{at}: must be a JSON object"));
            continue;
        }
        let mut translated = 0_usize;
        for (key, value) in flatten(catalog) {
            if !keys.contains(&key) {
                report.error(format!("{at}: unknown key \"{key}\""));
            } else if let Some(text) = value.as_str() {
                translated += usize::from(!is_blank(text));
            } else {
                report.error(format!("{at}: \"{key}\" must be a string"));
            }
        }
        report
            .translations
            .push(format!("{at}: {translated}/{} strings", keys.len()));
    }
}

/// A recipe whose bundle changed must ship under a new version: hosts record
/// the version they installed, and the dashboard offers updates by version.
fn check_versions(
    report: &mut Report,
    repo: &Repo,
    digests: &BTreeMap<&str, (String, String)>,
    previous: &Value,
) {
    let published = previous
        .get("recipes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for entry in published {
        let field = |name| entry.get(name).and_then(Value::as_str);
        let (Some(id), Some(version), Some(sha256)) =
            (field("id"), field("version"), field("sha256"))
        else {
            continue;
        };
        let Some((current_version, digest)) = digests.get(id) else {
            continue;
        };
        if current_version == version && digest != sha256 {
            let path = repo
                .recipes
                .iter()
                .find(|recipe| recipe.dir_name == id)
                .map_or(id, |recipe| recipe.path.as_str());
            report.error(format!(
                "{path}: the recipe or its templates changed since {version} was published; bump `version`"
            ));
        }
    }
}

fn schema(root: &Path, name: &str) -> Result<Validator> {
    let path = root.join("schema").join(name);
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let schema: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    jsonschema::validator_for(&schema)
        .map_err(|error| anyhow::anyhow!("schema/{name} is invalid: {error}"))
}

fn schema_errors(report: &mut Report, at: &str, schema: &Validator, instance: &Value) {
    for error in schema.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let path = if path.is_empty() {
            "/".to_owned()
        } else {
            path
        };
        report.error(format!("{at}: {path} {error}"));
    }
}
