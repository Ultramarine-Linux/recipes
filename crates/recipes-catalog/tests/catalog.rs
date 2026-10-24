//! End-to-end tests on throwaway checkouts.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use recipes_catalog::{CheckOptions, Repo, build, check, extract, json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tetra_recipe::verification::bundle_sha256;

const IMAGE: &str = "docker.io/library/nginx:alpine@sha256:a3554c0d0b48177b1413c26e01f695334428020d9df01243fbae3e11cfd17417";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn recipe_yaml(id: &str) -> String {
    format!(
        "recipe_id: {id}
name: {id} app
description: The {id} app.
category: apps
version: 1.0.0
parameters:
  - key: app_id
    label: App ID
    type: string
    default: {id}
  - key: proto
    label: Protocol
    type: choice
    default: https
    options: [https, http]
resources:
  - type: container
    filename: \"{{{{ app_id }}}}.container\"
    template: app.container.tera
  - type: file
    filename: index.html
    template: shared/index.html.tera
"
    )
}

/// A valid checkout: `alpha` (community) and `zeta` (official), with
/// `en.json` files already extracted.
fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for schema in ["recipe.schema.json", "catalog.schema.json"] {
        fs::create_dir_all(root.join("schema")).unwrap();
        fs::copy(
            repo_root().join("schema").join(schema),
            root.join("schema").join(schema),
        )
        .unwrap();
    }
    write(
        root,
        "catalog.yaml",
        "name: Test catalog\ncategories:\n  apps:\n    name: Apps\n",
    );
    for id in ["official/zeta", "community/alpha"] {
        write(
            root,
            &format!("recipes/{id}/templates/app.container.tera"),
            &format!("[Container]\nContainerName={{{{ app_id }}}}\nImage={IMAGE}\n"),
        );
    }
    write(root, "shared/index.html.tera", "<h1>{{ name }}</h1>\n");
    write(
        root,
        "recipes/official/zeta/recipe.yaml",
        &recipe_yaml("zeta"),
    );
    write(
        root,
        "recipes/community/alpha/recipe.yaml",
        &recipe_yaml("alpha"),
    );
    extract(&load(root)).unwrap();
    dir
}

fn load(root: &Path) -> Repo {
    Repo::load(root).unwrap()
}

fn errors(root: &Path) -> Vec<String> {
    check(&load(root), &CheckOptions::default()).unwrap().errors
}

/// Assert that exactly one error mentions `needle`.
#[track_caller]
fn assert_error(root: &Path, needle: &str) {
    let errors = errors(root);
    assert!(
        errors.iter().filter(|error| error.contains(needle)).count() == 1,
        "expected one error containing {needle:?}, got {errors:#?}"
    );
}

#[test]
fn bundle_digest_is_tetras() {
    assert_eq!(
        bundle_sha256("", &BTreeMap::new()),
        "daa263c4011e48aef28bad521f6a2cf5120dcd148fd57de9a5ebff1af83d4e50"
    );
}

#[test]
fn this_repository_passes() {
    let repo = load(&repo_root());
    let report = check(&repo, &CheckOptions::default()).unwrap();
    assert!(report.is_ok(), "{:#?}", report.errors);
    // Nothing to extract either.
    for (path, contents) in recipes_catalog::extract::sources(&repo) {
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            contents,
            "{}",
            path.display()
        );
    }
}

#[test]
fn fixture_passes() {
    let dir = fixture();
    assert_eq!(errors(dir.path()), Vec::<String>::new());
}

#[test]
fn extracts_translation_sources() {
    let dir = fixture();
    let en = fs::read_to_string(dir.path().join("recipes/official/zeta/locales/en.json")).unwrap();
    assert_eq!(
        en,
        "{\n\t\"name\": \"zeta app\",\n\t\"description\": \"The zeta app.\",\n\t\"parameters\": {\n\t\t\"app_id\": {\n\t\t\t\"label\": \"App ID\"\n\t\t},\n\t\t\"proto\": {\n\t\t\t\"label\": \"Protocol\",\n\t\t\t\"options\": {\n\t\t\t\t\"https\": \"https\",\n\t\t\t\t\"http\": \"http\"\n\t\t\t}\n\t\t}\n\t}\n}\n"
    );
    let catalog = fs::read_to_string(dir.path().join("locales/en.json")).unwrap();
    assert_eq!(
        catalog,
        "{\n\t\"categories\": {\n\t\t\"apps\": \"Apps\"\n\t}\n}\n"
    );
    // Extracting again changes nothing.
    assert!(extract(&load(dir.path())).unwrap().is_empty());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one walk through the whole published format"
)]
fn builds_the_documented_format() {
    let dir = fixture();
    let root = dir.path();
    write(
        root,
        "recipes/official/zeta/locales/de.json",
        r#"{ "name": "Zeta-App", "description": "", "parameters": { "proto": { "label": "", "options": { "https": "HTTPS", "http": "" } } } }"#,
    );
    write(
        root,
        "recipes/official/zeta/locales/fr.json",
        r#"{ "name": " " }"#,
    );
    write(
        root,
        "locales/de.json",
        r#"{ "categories": { "apps": "Anwendungen" } }"#,
    );
    write(
        root,
        "locales/fr.json",
        r#"{ "categories": { "apps": "" } }"#,
    );
    let repo = load(root);
    assert!(check(&repo, &CheckOptions::default()).unwrap().is_ok());
    let catalog = build(&repo).unwrap();
    assert_eq!(build(&load(root)).unwrap(), catalog, "deterministic");

    let index: Value = serde_json::from_str(&catalog.files["index.json"]).unwrap();
    let keys = |value: &Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&index),
        ["format", "name", "categories", "locales", "recipes"]
    );
    assert_eq!(index["format"], 1);
    assert_eq!(index["name"], "Test catalog");
    assert_eq!(index["categories"], json!({ "apps": { "name": "Apps" } }));
    assert_eq!(
        index["locales"],
        json!({ "de": { "categories": { "apps": "Anwendungen" } } })
    );

    let recipes = index["recipes"].as_array().unwrap();
    let ids: Vec<_> = recipes
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["alpha", "zeta"]);
    let zeta = &recipes[1];
    assert_eq!(
        keys(zeta),
        [
            "id",
            "name",
            "description",
            "category",
            "icon",
            "version",
            "catalog",
            "sha256",
            "bundle"
        ]
    );
    assert_eq!(recipes[0]["catalog"], "community");
    assert_eq!(zeta["catalog"], "official");
    assert_eq!(zeta["icon"], Value::Null);

    let path = zeta["bundle"].as_str().unwrap();
    let text = &catalog.files[path];
    let hash = hex::encode(Sha256::digest(text.as_bytes()));
    assert_eq!(path, format!("recipes/zeta/{}.json", &hash[..16]));
    assert_eq!(
        text,
        &json::to_string(&serde_json::from_str::<Value>(text).unwrap())
    );

    let bundle: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        keys(&bundle),
        [
            "format",
            "id",
            "version",
            "sha256",
            "recipe",
            "templates",
            "locales"
        ]
    );
    assert_eq!(bundle["recipe"], recipe_yaml("zeta"));
    assert_eq!(
        keys(&bundle["templates"]),
        ["app.container.tera", "shared/index.html.tera"]
    );
    let templates: BTreeMap<String, String> =
        serde_json::from_value(bundle["templates"].clone()).unwrap();
    assert_eq!(
        bundle["sha256"],
        bundle_sha256(&recipe_yaml("zeta"), &templates)
    );
    assert_eq!(bundle["sha256"], zeta["sha256"]);
    assert_eq!(
        bundle["locales"],
        json!({ "de": { "name": "Zeta-App", "parameters": { "proto": { "options": { "https": "HTTPS" } } } } })
    );
}

#[test]
fn translations_change_the_bundle_not_the_digest() {
    let dir = fixture();
    let root = dir.path();
    let entry = |catalog: &recipes_catalog::Catalog| {
        let index: Value = serde_json::from_str(&catalog.files["index.json"]).unwrap();
        index["recipes"][1].clone()
    };
    let before = entry(&build(&load(root)).unwrap());
    write(
        root,
        "recipes/official/zeta/locales/de.json",
        r#"{ "name": "Zeta" }"#,
    );
    let after = entry(&build(&load(root)).unwrap());
    assert_eq!(after["sha256"], before["sha256"]);
    assert_ne!(after["bundle"], before["bundle"]);
}

#[test]
fn writes_identical_bytes_on_rebuild() {
    let dir = fixture();
    let out = tempfile::tempdir().unwrap();
    let catalog = build(&load(dir.path())).unwrap();
    catalog.write(out.path()).unwrap();
    let first = read_tree(&out.path().join("v1"));
    fs::write(out.path().join("keep.txt"), "not ours").unwrap();
    build(&load(dir.path())).unwrap().write(out.path()).unwrap();
    assert_eq!(read_tree(&out.path().join("v1")), first);
    assert!(out.path().join("keep.txt").exists());
}

fn read_tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(read_tree(&path));
        } else {
            files.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    files
}

#[test]
fn stale_source_locale() {
    let dir = fixture();
    let path = "recipes/official/zeta/recipe.yaml";
    write(
        dir.path(),
        path,
        &recipe_yaml("zeta").replace("label: App ID", "label: Application ID"),
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta/locales: en.json is stale",
    );
    extract(&load(dir.path())).unwrap();
    fs::remove_file(dir.path().join("locales/en.json")).unwrap();
    assert_error(dir.path(), "locales: en.json is stale");
}

#[test]
fn translation_keys_and_types() {
    let dir = fixture();
    write(
        dir.path(),
        "recipes/official/zeta/locales/de.json",
        r#"{ "name": "Zeta", "parameters": { "nope": { "label": "x" }, "app_id": { "label": 3 } } }"#,
    );
    assert_error(dir.path(), "de.json: unknown key \"parameters.nope.label\"");
    assert_error(
        dir.path(),
        "de.json: \"parameters.app_id.label\" must be a string",
    );
    let report = check(&load(dir.path()), &CheckOptions::default()).unwrap();
    assert!(
        report
            .translations
            .contains(&"recipes/official/zeta/locales/de.json: 1/6 strings".to_owned()),
        "{:?}",
        report.translations
    );
}

#[test]
fn unpinned_image() {
    let dir = fixture();
    write(
        dir.path(),
        "recipes/official/zeta/templates/app.container.tera",
        "[Container]\nImage=docker.io/library/nginx:latest\n",
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta/templates/app.container.tera: image `docker.io/library/nginx:latest` must use @sha256:<64 hex>",
    );
}

#[test]
fn image_policy_covers_service_commands() {
    let dir = fixture();
    write(
        dir.path(),
        "recipes/official/zeta/templates/app.container.tera",
        &format!("[Container]\nImage={IMAGE}\n\n[Service]\nExecStartPre=podman pull evil\n"),
    );
    assert_error(dir.path(), "`ExecStartPre=` commands are unsupported");
}

#[test]
fn missing_and_unused_templates() {
    let dir = fixture();
    fs::rename(
        dir.path().join("shared/index.html.tera"),
        dir.path().join("shared/other.html.tera"),
    )
    .unwrap();
    write(
        dir.path(),
        "recipes/official/zeta/templates/extra.container.tera",
        &format!("[Container]\nImage={IMAGE}\n"),
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta: template `shared/index.html.tera` does not exist",
    );
    assert_error(
        dir.path(),
        "recipes/community/alpha: template `shared/index.html.tera` does not exist",
    );
    assert_error(
        dir.path(),
        "shared/other.html.tera is not used by any recipe",
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta/templates/extra.container.tera is not used by the recipe",
    );
}

#[test]
fn recipe_templates_are_its_own() {
    let dir = fixture();
    // alpha can't reach zeta's templates: names resolve in its own directory.
    fs::remove_file(
        dir.path()
            .join("recipes/community/alpha/templates/app.container.tera"),
    )
    .unwrap();
    assert_error(
        dir.path(),
        "recipes/community/alpha: template `app.container.tera` does not exist (recipes/community/alpha/templates/app.container.tera)",
    );
}

#[test]
fn shared_templates_cant_set_images() {
    let dir = fixture();
    write(
        dir.path(),
        "shared/index.html.tera",
        &format!("Image={IMAGE}\n"),
    );
    assert_error(
        dir.path(),
        "shared/index.html.tera: shared templates can't set `Image=`",
    );
}

#[test]
fn old_templates_dir_is_rejected() {
    let dir = fixture();
    write(dir.path(), "templates/containers/app.container.tera", "");
    assert_error(dir.path(), "templates/: templates now live in");
}

#[test]
fn template_paths_stay_in_templates() {
    let dir = fixture();
    write(dir.path(), "secret.tera", "x");
    write(
        dir.path(),
        "recipes/official/zeta/recipe.yaml",
        &recipe_yaml("zeta").replace("shared/index.html.tera", "shared/../../secret.tera"),
    );
    let errors = errors(dir.path());
    assert!(
        errors
            .iter()
            .any(|error| error.contains("must be a relative path in the recipe's templates/")),
        "{errors:#?}"
    );
    // The schema rejects it too.
    assert!(
        errors
            .iter()
            .any(|error| error.contains("/resources/1/template")),
        "{errors:#?}"
    );
}

#[test]
fn quadlet_templates_are_named_after_their_type() {
    let dir = fixture();
    for id in ["official/zeta", "community/alpha"] {
        fs::rename(
            dir.path()
                .join(format!("recipes/{id}/templates/app.container.tera")),
            dir.path().join(format!("recipes/{id}/templates/app.tera")),
        )
        .unwrap();
        let path = format!("recipes/{id}/recipe.yaml");
        let yaml = fs::read_to_string(dir.path().join(&path)).unwrap();
        write(
            dir.path(),
            &path,
            &yaml.replace("app.container.tera", "app.tera"),
        );
    }
    assert_error(
        dir.path(),
        "recipes/official/zeta: template `app.tera` of a container resource must be named `*.container.tera`",
    );
}

#[test]
fn id_must_match_directory() {
    let dir = fixture();
    fs::rename(
        dir.path().join("recipes/official/zeta"),
        dir.path().join("recipes/official/omega"),
    )
    .unwrap();
    assert_error(
        dir.path(),
        "recipes/official/omega: recipe_id `zeta` must match the directory name",
    );
}

#[test]
fn ids_are_unique_across_tiers() {
    let dir = fixture();
    write(
        dir.path(),
        "recipes/community/zeta/recipe.yaml",
        &recipe_yaml("zeta"),
    );
    extract(&load(dir.path())).unwrap();
    assert_error(
        dir.path(),
        "recipes/community/zeta: recipe id is already used by recipes/official/zeta",
    );
}

#[test]
fn recipes_live_in_a_tier() {
    let dir = fixture();
    write(dir.path(), "recipes/zeta/recipe.yaml", &recipe_yaml("zeta"));
    assert_error(
        dir.path(),
        "recipes/zeta: recipes go in recipes/{official,community}/<recipe_id>/",
    );
}

#[test]
fn schema_catches_unknown_fields_and_categories() {
    let dir = fixture();
    write(
        dir.path(),
        "recipes/official/zeta/recipe.yaml",
        &recipe_yaml("zeta").replace("category: apps", "category: games\ndescriptoin: typo"),
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta/recipe.yaml: / Additional properties are not allowed ('descriptoin'",
    );
    assert_error(
        dir.path(),
        "category `games` is not defined in catalog.yaml",
    );
    write(
        dir.path(),
        "catalog.yaml",
        "name: Test\ncategories:\n  apps:\n    name: Apps\n    icon: x\n",
    );
    assert_error(
        dir.path(),
        "catalog.yaml: /categories/apps Additional properties",
    );
}

#[test]
fn parameter_sanity() {
    let dir = fixture();
    let yaml = recipe_yaml("zeta").replace(
        "resources:",
        "  - key: port
    label: Port
    type: integer
    default: 70000
    min: 1
    max: 65535
  - key: flag
    label: Flag
    type: boolean
    min: 1
  - key: secret
    label: Secret
    type: secret
    default: fixed
    generate: random_32
resources:",
    );
    write(dir.path(), "recipes/official/zeta/recipe.yaml", &yaml);
    extract(&load(dir.path())).unwrap();
    assert_error(
        dir.path(),
        "parameter `port`: invalid default: parameter `port` is above its maximum",
    );
    assert_error(
        dir.path(),
        "parameter `flag`: only integer parameters take min/max",
    );
    assert_error(
        dir.path(),
        "parameter `secret`: has both a default and a generator",
    );

    let bad_choice = recipe_yaml("zeta").replace("default: https", "default: ftp");
    write(dir.path(), "recipes/official/zeta/recipe.yaml", &bad_choice);
    assert_error(
        dir.path(),
        "parameter `proto`: invalid default: parameter `proto` has an unsupported choice",
    );
}

#[test]
fn tetra_validation_runs() {
    let dir = fixture();
    let yaml = recipe_yaml("zeta").replace("key: proto", "key: app_id");
    write(dir.path(), "recipes/official/zeta/recipe.yaml", &yaml);
    extract(&load(dir.path())).unwrap();
    assert_error(dir.path(), "tetra rejects it: duplicate parameter `app_id`");
}

#[test]
fn renders_with_sample_values() {
    let dir = fixture();
    write(
        dir.path(),
        "shared/index.html.tera",
        "{{ undefined_thing }}\n",
    );
    assert_error(
        dir.path(),
        "recipes/official/zeta: fails to render with sample values",
    );
}

#[test]
fn changed_bundles_need_a_version_bump() {
    let dir = fixture();
    let published = build(&load(dir.path())).unwrap();
    let previous: Value = serde_json::from_str(&published.files["index.json"]).unwrap();
    let options = CheckOptions {
        previous_index: Some(previous),
    };
    assert!(check(&load(dir.path()), &options).unwrap().is_ok());

    write(
        dir.path(),
        "shared/index.html.tera",
        "<h2>{{ name }}</h2>\n",
    );
    let errors = check(&load(dir.path()), &options).unwrap().errors;
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors[0].contains(
        "recipes/community/alpha: the recipe or its templates changed since 1.0.0 was published"
    ));

    for id in ["official/zeta", "community/alpha"] {
        let path = format!("recipes/{id}/recipe.yaml");
        let yaml = fs::read_to_string(dir.path().join(&path)).unwrap();
        write(
            dir.path(),
            &path,
            &yaml.replace("version: 1.0.0", "version: 1.0.1"),
        );
    }
    assert!(check(&load(dir.path()), &options).unwrap().is_ok());
}
