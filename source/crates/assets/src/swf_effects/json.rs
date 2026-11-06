//! The JSON text System.Text.Json writes with `WriteIndented = true` and its default encoder, so the effect JSON is
//! the same text the C# converter writes, byte for byte.

use serde_json::Value;

/// System.Text.Json ends its indented lines with `Environment.NewLine`: "\r\n" when the converter runs on Windows,
/// "\n" elsewhere. We write "\n" (the converter as run on Linux/macOS); the JSON is the same either way.
const NEW_LINE: &str = "\n";

/// `value` indented by two spaces, `"key": value`, empty objects and arrays as `{}` and `[]`, no trailing newline.
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write(&mut out, value, 0);
    out
}

fn indent(out: &mut String, depth: usize) {
    out.push_str(NEW_LINE);
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // Integers as they are; the only floats are the sheet's 0.5 pivots, which both write as "0.5" (and a float
        // 1.0 is put in as the integer 1, as System.Text.Json writes it).
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write(out, item, depth + 1);
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                string(out, key);
                out.push_str(": ");
                write(out, item, depth + 1);
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// System.Text.Json's default escaping (JavaScriptEncoder.Default): only printable ASCII other than the
/// HTML-sensitive `" & ' + < > \` and backtick goes out as it is. `\n \r \t \b \f \\` get their short escapes; the
/// quote and everything else become `\uXXXX` with upper-case hex, characters past the BMP as two escaped surrogates.
fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\\' => out.push_str("\\\\"),
            ' '..='~' if !matches!(c, '"' | '&' | '\'' | '+' | '<' | '>' | '`') => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04X}", unit));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_like_system_text_json() {
        let value = serde_json::json!({ "a": { "x": 1, "y": -2 }, "e": {}, "l": [], "p": 0.5, "s": "it's <\"é\">\n😀", "t": true });
        assert_eq!(
            to_string(&value),
            "{\n  \"a\": {\n    \"x\": 1,\n    \"y\": -2\n  },\n  \"e\": {},\n  \"l\": [],\n  \"p\": 0.5,\n  \
             \"s\": \"it\\u0027s \\u003C\\u0022\\u00E9\\u0022\\u003E\\n\\uD83D\\uDE00\",\n  \"t\": true\n}"
        );
    }
}
