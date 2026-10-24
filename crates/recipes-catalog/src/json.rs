//! The JSON style of every generated file.
//!
//! Tab-indented, keys in insertion order, trailing newline. Byte-identical to JavaScript's
//! `JSON.stringify(value, null, '\t') + '\n'` (which produced the files before).

use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};

#[must_use]
pub fn to_string(value: &impl Serialize) -> String {
    let mut out = Vec::new();
    let mut serializer = Serializer::with_formatter(&mut out, PrettyFormatter::with_indent(b"\t"));
    // Only fails for maps with non-string keys, which none of our types have.
    value
        .serialize(&mut serializer)
        .expect("generated JSON is serializable");
    out.push(b'\n');
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn matches_json_stringify_with_tabs() {
        let value = json!({ "b": [1, {}], "a": { "x": "é\u{1}\n\"<" }, "c": [] });
        assert_eq!(
            super::to_string(&value),
            "{\n\t\"b\": [\n\t\t1,\n\t\t{}\n\t],\n\t\"a\": {\n\t\t\"x\": \"é\\u0001\\n\\\"<\"\n\t},\n\t\"c\": []\n}\n"
        );
    }
}
