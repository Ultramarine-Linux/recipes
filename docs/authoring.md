---
title: Writing recipes
description: "Lay out, check, and build Ultramarine Server app recipes."
---

Recipes are YAML documents that [tetra](https://github.com/Ultramarine-Linux/tetra)
renders into Podman Quadlet units. This page covers the repository layout,
the tooling, and the rules every recipe has to pass. For which tier a recipe
belongs in, see [Official and community recipes](./tiers.md); for its
strings, see [Translating recipes](./translations.md).

## Layout

```
catalog.yaml                                 catalog name and categories
locales/<locale>.json                        category translations
recipes/official/<id>/recipe.yaml            recipes vetted or maintained by Fyra Labs
recipes/community/<id>/recipe.yaml           recipes contributed by others
recipes/<tier>/<id>/locales/<locale>.json    translations of a recipe
templates/                                   Tera templates, shared between recipes
schema/                                      JSON schemas for recipe.yaml and catalog.yaml
crates/recipes-catalog/                      library: load, check, extract, build
crates/recipes/                              the `recipes` CLI
crates/recipes-origin/                       the origin builder (webhook server)
origin/                                      builder image and a local Caddyfile
```

The YAML files point editors at `schema/` through
`# yaml-language-server: $schema=...` comments.

## Tooling

You need a Rust toolchain. Cargo fetches tetra's `tetra-recipe` crate from
git.

```sh
cargo run -p recipes -- extract   # after changing any name, description, label, placeholder or option
cargo run -p recipes -- check     # everything CI and the origin enforce
cargo run -p recipes -- build     # check, then write dist/v1/ (--out <dir> to change)
```

## What `check` enforces

`check` runs every recipe through tetra's own code
([`tetra-recipe`](https://github.com/Ultramarine-Linux/tetra/tree/main/crates/tetra-recipe)),
so what passes here is what hosts accept:

- `AppRecipe::load_str` (parsing and `validate`): required fields, unique
  and unreserved parameter keys, choice parameters with options.
- Every parameter `default` through tetra's value validation (type, range,
  choice, single line).
- A render of every recipe through `render_with_templates` with sample
  values, once with all booleans off and once on, which catches Tera errors
  and undefined variables.
- The pinned image policy (`verify_images`) on every Quadlet template: each
  `Image=` must be `name@sha256:<64 hex>`, and raw `PodmanArgs=`, image
  mounts, `Exec*=` in `[Service]`, `.pod`, and `.build` units are refused.
  Tetra enforces this for official and community recipes alike.

## Repository rules

On top of tetra's rules:

- `recipe.yaml` and `catalog.yaml` match `schema/*.json`. Tetra ignores
  unknown fields, so the schema is what catches typos.
- A recipe lives in `recipes/{official,community}/<recipe_id>/`, its id is
  unique across both, and its `category` is defined in `catalog.yaml`.
- Parameters only use the fields of their type: `options` for `choice`,
  `min`/`max` (with `min <= max`) for `integer`, and `generate` for `string`
  and `secret` (and not together with a `default`).
- Every `resources[].template` is a plain relative path that exists under
  `templates/`, Quadlet templates are named after their unit type
  (`*.container.tera`, `*.volume.tera`, ...), and every template is used.
- Every `locales/en.json` matches the YAML (run `extract`), and translations
  only contain strings, under keys from `en.json`. `check` also prints how
  complete each translation is.
- With `--previous <index.json>` (CI passes the published index), a recipe
  whose recipe or templates changed must have a new `version`.

## Versioning

Bump `version` whenever `recipe.yaml` or any template it uses changes: hosts
record the version and digest they installed, and dashboards offer updates by
version. Translation changes don't need a bump.
