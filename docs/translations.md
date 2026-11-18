---
title: Translating recipes
description: "How recipe strings are extracted, translated in Weblate, and published."
---

`recipe.yaml` holds the English text. `extract` copies it into
`recipes/<tier>/<id>/locales/en.json` (and `catalog.yaml` into
`locales/en.json`), which Weblate uses as the monolingual base file in
i18next v4 JSON format, the same format the dashboard uses.

Translations are kept out of the bundle digest, so fixing a string never
changes what hosts verify, and a recipe doesn't need a version bump for it.

Weblate setup: one component for the catalog strings, plus the component
discovery add-on for the recipes. Both must be monolingual, so set the
English file as the *monolingual base language file*. If it only goes in
*template for new translations*, Weblate treats the files as bilingual and
every new language starts with no strings.

Catalog component:

- File mask: `locales/*.json`
- Monolingual base language file: `locales/en.json`
- File format: i18next JSON v4
- Source language: English

Component discovery add-on:

- Regex: `recipes/(?P<tier>official|community)/(?P<component>[^/]+)/locales/(?P<language>[^/.]+)\.json`
- Define the monolingual base filename: `recipes/{{ tier }}/{{ component }}/locales/en.json`
- Base file for new translations: leave empty
- File format: i18next JSON v4

Translation keys look like this:

```json
{
	"name": "Nextcloud",
	"description": "...",
	"parameters": {
		"admin_user": { "label": "Admin user" },
		"bundle_dir": { "label": "Bundle directory", "placeholder": "Managed by the agent" },
		"overwrite_protocol": { "label": "Public protocol", "options": { "https": "HTTPS" } }
	}
}
```
