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

Weblate setup: one component for `locales/*.json`, plus the component
discovery add-on for the recipes:

- Regex: `recipes/(?P<tier>official|community)/(?P<component>[^/]+)/locales/(?P<language>[^/.]+)\.json`
- Base file: `recipes/{{ tier }}/{{ component }}/locales/en.json`
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
