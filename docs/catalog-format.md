---
title: Recipe catalog format
description: "The published recipe catalog format, for dashboards and custom catalogs."
---

This is a public contract: dashboards depend on it, and anyone can publish a
custom catalog in the same format. Dashboards install recipes from
user-added catalogs as tetra's `custom` catalog, whatever their `catalog`
field says. Breaking changes go under a new `/v2/` path.

A catalog is a base URL (ending in `/v1`) serving:

| Path                           | Contents                                       | Suggested cache |
| ------------------------------ | ---------------------------------------------- | --------------- |
| `/v1/index.json`               | categories, category translations, recipe list | minutes         |
| `/v1/recipes/<id>/<hash>.json` | recipe YAML, templates, translations           | immutable       |

Serve both with `Access-Control-Allow-Origin: *` so dashboards can fetch them.

## `index.json`

```json
{
	"format": 1,
	"name": "Ultramarine Server recipes",
	"categories": { "apps": { "name": "Apps" } },
	"locales": { "de": { "categories": { "apps": "Anwendungen" } } },
	"recipes": [
		{
			"id": "nextcloud",
			"name": "Nextcloud",
			"description": "...",
			"category": "apps",
			"icon": "nextcloud",
			"version": "0.1.1",
			"catalog": "official",
			"sha256": "<tetra bundle digest>",
			"bundle": "recipes/nextcloud/<hash>.json"
		}
	]
}
```

- `format`: always `1` under `/v1/`.
- `name`: display name of the catalog.
- `categories`: category id → `{ "name": <English name> }`.
- `locales`: locale → `{ "categories": { <id>: <translated name> } }`.
- `recipes`: sorted by `id`. `description` is `""` when the recipe has none,
  `icon` is `null`. `catalog` is `official` or `community` here; custom
  catalogs may use anything (it's ignored for them). `bundle` is relative to
  the `/v1/` base.

## Bundles

```json
{
	"format": 1,
	"id": "nextcloud",
	"version": "0.1.1",
	"sha256": "<tetra bundle digest>",
	"recipe": "<exact recipe.yaml text>",
	"templates": { "mariadb.container.tera": "...", "shared/named.volume.tera": "..." },
	"locales": { "de": { "name": "Nextcloud", "parameters": { "admin_user": { "label": "Admin-Benutzer" } } } }
}
```

- `recipe` is the exact text of `recipe.yaml`. `templates` holds exactly the
  templates its `resources` reference, keyed by the `template` value.
- `sha256` is tetra's `bundle_sha256` over `recipe` and `templates` only:
  SHA-256 of `tetra-recipe-bundle-v1\0`, then the recipe's UTF-8 length as a
  big-endian u64 followed by its bytes, then the template count as a u64,
  then each template's name and body (length-prefixed the same way) in byte
  order of the names. Dashboards pass it to tetra as the install's
  `source.sha256`, and tetra recomputes it.
- `locales` holds the translations of the recipe's strings (same keys as the
  example above), English excluded. Untranslated (empty) strings and the
  objects left empty by removing them are left out, so consumers fall back
  to the English text in `recipe`.
- `<hash>` is the first 16 hex digits of the SHA-256 of the bundle file
  itself, translations included, so a bundle URL never changes content and
  can be cached forever. Only `index.json` needs revalidating.

## Encoding

All files are UTF-8 JSON, indented with tabs, with a trailing newline. Keys
appear in the order shown. Category, parameter, and option keys follow the
source YAML, keys inside a translation follow its file, and template names
and locale names are in byte order.
Builds are deterministic: an unchanged tree produces identical bytes, so
mirrors and sync jobs only see changes when the catalog changed.

## Consumers

- **Dashboards** fetch `index.json` at runtime, then the bundles of the
  recipes they show, and send `recipe`, `templates`, and `sha256` to tetra on
  install.
- **Tetra** consumes the bundle inline (`recipes.render_inline`); its tests
  keep their own fixture copies of some templates.
