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
recipes/<tier>/<id>/templates/               a recipe's own Tera templates
recipes/<tier>/<id>/locales/<locale>.json    translations of a recipe
shared/                                      image-free templates any recipe can use
schema/                                      JSON schemas for recipe.yaml and catalog.yaml
crates/recipes-catalog/                      library: load, check, extract, build
crates/recipes/                              the `recipes` CLI
crates/recipes-origin/                       the origin builder (webhook server)
origin/                                      builder image and a local Caddyfile
```

The YAML files point editors at `schema/` through
`# yaml-language-server: $schema=...` comments.

## Templates

A recipe keeps its templates next to it, in `recipes/<tier>/<id>/templates/`,
so one directory holds everything a recipe ships:

```
recipes/official/forgejo/
  recipe.yaml
  locales/en.json
  templates/
    forgejo.container.tera
    postgres.container.tera
```

In `recipe.yaml`, `template: forgejo.container.tera` names a file in the
recipe's own `templates/`, and `template: shared/named.volume.tera` names one
in the top-level `shared/`. That name is also the template's key in the
published bundle.

`shared/` is only for templates without an image, like `named.volume.tera`
and `bridge.network.tera`. Anything that pins an image, including databases
and caches, gets its own copy in each recipe that uses it. That way bumping
Postgres for one app (a major version bump breaks existing data directories)
never changes another app's install.

Name the app's main container `<id>.container.tera`, and other templates
after what they run (`postgres.container.tera`, `index.html.tera`).

## Tooling

You need a Rust toolchain. Cargo fetches tetra's `tetra-recipe` crate from
git.

```sh
cargo run -p recipes -- extract   # after changing any name, description, label, placeholder or option
cargo run -p recipes -- check     # everything CI and the origin enforce
cargo run -p recipes -- build     # check, then write dist/v1/ (--out <dir> to change)
cargo run -p recipes -- render <id> [--values values.yaml] [--output-dir dir]
```

`render` renders a recipe through tetra with exactly the templates its bundle
carries, the way a host would, and prints the files (or writes them with
`--output-dir`). Values default to the recipe's defaults and generators.

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
- Every `resources[].template` is a plain relative path that exists in the
  recipe's `templates/` (or in `shared/`, for `shared/...` names), and
  Quadlet templates are named after their unit type (`*.container.tera`,
  `*.volume.tera`, ...).
- Every file in a recipe's `templates/` is used by that recipe, and every
  file in `shared/` by some recipe. Shared templates can't set `Image=`.
- Every `locales/en.json` matches the YAML (run `extract`), and translations
  only contain strings, under keys from `en.json`. `check` also prints how
  complete each translation is.
- With `--previous <index.json>` (CI passes the published index), a recipe
  whose recipe or templates changed must have a new `version`.

## Versioning

Bump `version` whenever `recipe.yaml` or any template it uses changes: hosts
record the version and digest they installed, and dashboards offer updates by
version. Translation changes don't need a bump.
