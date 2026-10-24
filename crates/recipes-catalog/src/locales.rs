//! Translation sources (`en.json`) and published locale catalogs.
//!
//! Locale files use the i18next v4 JSON layout Weblate edits: nested objects
//! of strings, in the order of the YAML they were extracted from.

use serde_json::{Map, Value};

use crate::repo::Locales;

/// The translatable strings of a recipe, in the layout of its locale files:
///
/// ```json
/// { "name": "…", "description": "…",
///   "parameters": { "<key>": { "label": "…", "placeholder": "…", "options": { "<option>": "<option>" } } } }
/// ```
#[must_use]
pub fn recipe_source(doc: &Value) -> Value {
    let mut source = Map::new();
    if let Some(name) = doc.get("name") {
        source.insert("name".into(), name.clone());
    }
    if let Some(description) = doc.get("description").filter(|value| truthy(value)) {
        source.insert("description".into(), description.clone());
    }
    let mut parameters = Map::new();
    for parameter in doc
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut strings = Map::new();
        if let Some(label) = parameter.get("label") {
            strings.insert("label".into(), label.clone());
        }
        if let Some(placeholder) = parameter.get("placeholder").filter(|value| truthy(value)) {
            strings.insert("placeholder".into(), placeholder.clone());
        }
        if let Some(options) = parameter
            .get("options")
            .and_then(Value::as_array)
            .filter(|options| !options.is_empty())
        {
            let options = options
                .iter()
                .map(|option| (key_string(option), option.clone()))
                .collect();
            strings.insert("options".into(), Value::Object(options));
        }
        let key = parameter.get("key").map_or_else(String::new, key_string);
        parameters.insert(key, Value::Object(strings));
    }
    if !parameters.is_empty() {
        source.insert("parameters".into(), Value::Object(parameters));
    }
    Value::Object(source)
}

/// The translatable strings of `catalog.yaml`: `{ "categories": { "<id>": "<name>" } }`.
#[must_use]
pub fn catalog_source(catalog: &Value) -> Value {
    let categories = catalog
        .get("categories")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(id, category)| Some((id.clone(), category.get("name")?.clone())))
        .collect();
    let mut source = Map::new();
    source.insert("categories".into(), Value::Object(categories));
    Value::Object(source)
}

/// Flatten nested objects into `(dotted.path, leaf)` pairs, in order.
#[must_use]
pub fn flatten(node: &Value) -> Vec<(String, &Value)> {
    fn walk<'a>(node: &'a Map<String, Value>, prefix: &str, out: &mut Vec<(String, &'a Value)>) {
        for (key, value) in node {
            let path = format!("{prefix}{key}");
            match value {
                Value::Object(child) => walk(child, &format!("{path}."), out),
                leaf => out.push((path, leaf)),
            }
        }
    }
    let mut out = Vec::new();
    if let Value::Object(node) = node {
        walk(node, "", &mut out);
    }
    out
}

/// Locales as published: untranslated (blank) strings are dropped, then the
/// objects and locales left empty by that, so consumers fall back to English.
#[must_use]
pub fn prune_locales(locales: &Locales) -> Map<String, Value> {
    locales
        .iter()
        .filter_map(|(locale, catalog)| {
            let pruned = prune(catalog.as_object()?);
            (!pruned.is_empty()).then(|| (locale.clone(), Value::Object(pruned)))
        })
        .collect()
}

fn prune(node: &Map<String, Value>) -> Map<String, Value> {
    node.iter()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::String(text) if !is_blank(text) => value.clone(),
                Value::Object(child) => Some(prune(child))
                    .filter(|child| !child.is_empty())
                    .map(Value::Object)?,
                // Blank strings, and anything else (`check` rejects it).
                _ => return None,
            };
            Some((key.clone(), value))
        })
        .collect()
}

/// Blank as JavaScript's `String.prototype.trim` (which Weblate's tooling and
/// the previous scripts used) sees it: Rust's `char::is_whitespace` plus
/// U+FEFF, minus U+0085.
#[must_use]
pub fn is_blank(text: &str) -> bool {
    text.chars()
        .all(|c| c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}'))
}

/// JavaScript truthiness, which decided what the old extractor included.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A scalar as an object key (the schema only allows strings here anyway).
fn key_string(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_user_facing_strings() {
        let doc = json!({
            "name": "Demo",
            "description": "A demo",
            "parameters": [
                { "key": "port", "label": "Port", "type": "integer" },
                { "key": "proto", "label": "Protocol", "type": "choice", "options": ["https", "http"] },
                { "key": "dir", "label": "Directory", "type": "string", "placeholder": "Auto" },
                { "key": "empty", "label": "Empty", "type": "string", "placeholder": "" }
            ]
        });
        let expected = json!({
            "name": "Demo",
            "description": "A demo",
            "parameters": {
                "port": { "label": "Port" },
                "proto": { "label": "Protocol", "options": { "https": "https", "http": "http" } },
                "dir": { "label": "Directory", "placeholder": "Auto" },
                "empty": { "label": "Empty" }
            }
        });
        let source = recipe_source(&doc);
        assert_eq!(source, expected);
        // Order follows the YAML, not the alphabet.
        assert_eq!(
            crate::json::to_string(&source),
            crate::json::to_string(&expected)
        );
        assert_eq!(
            recipe_source(&json!({ "name": "X", "description": "" })),
            json!({ "name": "X" })
        );
    }

    #[test]
    fn extracts_categories() {
        let catalog = json!({ "name": "C", "categories": { "web": { "name": "Web" }, "apps": { "name": "Apps" } } });
        assert_eq!(
            crate::json::to_string(&catalog_source(&catalog)),
            "{\n\t\"categories\": {\n\t\t\"web\": \"Web\",\n\t\t\"apps\": \"Apps\"\n\t}\n}\n"
        );
    }

    #[test]
    fn flattens_nested_keys() {
        let value = json!({ "a": "1", "b": { "c": "2", "d": { "e": 3 } } });
        let flat: Vec<_> = flatten(&value)
            .into_iter()
            .map(|(key, value)| (key, value.clone()))
            .collect();
        assert_eq!(
            flat,
            [
                ("a".to_owned(), json!("1")),
                ("b.c".to_owned(), json!("2")),
                ("b.d.e".to_owned(), json!(3))
            ]
        );
    }

    #[test]
    fn prunes_untranslated_strings() {
        let locales = Locales::from([
            (
                "de".into(),
                json!({ "name": "Name", "description": "", "parameters": { "a": { "label": " \u{feff}" }, "b": { "label": "B" } } }),
            ),
            (
                "fr".into(),
                json!({ "name": "  ", "parameters": { "a": { "label": "" } } }),
            ),
            ("nl".into(), json!({ "name": 3, "description": ["x"] })),
        ]);
        let pruned = Value::Object(prune_locales(&locales));
        assert_eq!(
            pruned,
            json!({ "de": { "name": "Name", "parameters": { "b": { "label": "B" } } } })
        );
    }

    #[test]
    fn blank_matches_javascript_trim() {
        assert!(is_blank(" \t\n\u{a0}\u{feff}\u{2028}\u{3000}"));
        assert!(!is_blank("\u{85}"));
        assert!(!is_blank(" x "));
    }
}
