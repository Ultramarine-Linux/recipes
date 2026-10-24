# Ultramarine Server recipes

The app recipe catalog for Ultramarine Server. Recipes are YAML documents that
[tetra](https://github.com/Ultramarine-Linux/tetra) renders into Podman
Quadlet units. Dashboard fetches the published catalog at runtime and sends the
chosen recipe to the host inline when an app is installed.

This repository is the source of truth for recipes, their templates, and their
translations. A central origin builds it on every push to `main` and serves
the result behind Cloudflare.

## Quick start

```sh
cargo run -p recipes -- extract   # after changing any user-facing string
cargo run -p recipes -- check     # everything CI and the origin enforce
cargo run -p recipes -- build     # check, then write dist/v1/
```

Recipes live in `recipes/official/<id>/` or `recipes/community/<id>/`, with
shared Tera templates in `templates/`.

## Documentation

- [Writing recipes](docs/authoring.md): layout, tooling, what `check`
  enforces, versioning.
- [Official and community recipes](docs/tiers.md)
- [Translating recipes](docs/translations.md): extraction and Weblate.
- [Recipe catalog format](docs/catalog-format.md): the published format,
  for dashboards and anyone hosting a custom catalog.
- [Recipe origin builder](docs/origin.md): `recipes-origin`, its config,
  webhook, and image.

These pages are meant to move to the Ultramarine wiki eventually; they use
its frontmatter already.

## tetra-recipe

The checks and the bundle digest come from tetra's
[`tetra-recipe`](https://github.com/Ultramarine-Linux/tetra/tree/main/crates/tetra-recipe)
crate, a git dependency pinned by `Cargo.lock`. `cargo update -p
tetra-recipe` moves it to tetra's latest `main`. To try changes against a
local tetra checkout (this rewrites `Cargo.lock` to point at the path, so
don't commit that):

```sh
cargo --config 'patch."https://github.com/Ultramarine-Linux/tetra".tetra-recipe.path="../tetra/crates/tetra-recipe"' run -p recipes -- check
```
