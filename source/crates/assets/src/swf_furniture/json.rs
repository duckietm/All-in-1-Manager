//! The JSON the C# writes with System.Text.Json (`WriteIndented`, `WhenWritingNull`, the
//! `FloatToFixedDecimalConverter` for every float), written by hand so that every byte matches: serde_json
//! formats numbers ("1.0") and escapes strings differently from .NET.

use std::collections::HashMap;
use std::hash::Hash;

use anyhow::{bail, Result};

/// Utf8JsonWriter indents with Environment.NewLine. The converter is a Windows tool first (its file listing order,
/// modelled in `names::ntfs_order`, is Windows' too), so this is "\r\n"; on Linux the C# writes "\n".
pub const NEWLINE: &str = "\r\n";

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// A number (or other literal) as written.
    Raw(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn int(value: i32) -> Json {
        Json::Raw(value.to_string())
    }

    pub fn bool(value: bool) -> Json {
        Json::Raw(if value { "true" } else { "false" }.into())
    }

    pub fn str(value: &str) -> Json {
        Json::Str(value.to_string())
    }

    /// A float through FloatToFixedDecimalConverter.Write:
    /// `value % 1 == 0` -> `((int)value).ToString()`; `|value| < 0.0001f` -> `ToString("F6")`; else `ToString("G")`.
    /// It writes with WriteRawValue, which validates the JSON: NaN and the infinities ("NaN", "Infinity") throw,
    /// and the whole conversion fails, so they are an error here.
    pub fn float(value: f32) -> Result<Json> {
        if !value.is_finite() {
            bail!("The furniture has a float that is not a number ({value}), which the converter cannot write.");
        }
        let text = if value % 1.0 == 0.0 {
            // (int)value saturates in .NET 9 and later, as `as` does; -0 becomes "0".
            (value as i32).to_string()
        } else if value.abs() < 0.0001 {
            // .NET Core 3.0+ formats "F6" from the exact binary value, correctly rounded, as Rust does.
            format!("{:.6}", value)
        } else {
            // "G" without precision is the shortest round-trip string. In this range (not an integer, so below 2^23,
            // and at least 0.0001) .NET never switches to exponent notation, and neither does Rust's Display.
            format!("{}", value)
        };
        Ok(Json::Raw(text))
    }

    /// A double as Utf8JsonWriter writes it (shortest round-trip "G", invariant), with
    /// `AllowNamedFloatingPointLiterals`: NaN and the infinities become the strings "NaN", "Infinity", "-Infinity".
    pub fn double(value: f64) -> Json {
        if value.is_nan() {
            Json::str("NaN")
        } else if value.is_infinite() {
            Json::str(if value > 0.0 { "Infinity" } else { "-Infinity" })
        } else {
            Json::Raw(dotnet_general(value))
        }
    }
}

/// .NET's shortest round-trip "G" for a double: exponent notation ("1E+15", "1.5E-05") when the decimal exponent
/// is past the 15 digits of double precision (or the digit count, if longer) or below -4; else positional.
pub fn dotnet_general(value: f64) -> String {
    let sci = format!("{:e}", value); // shortest digits, e.g. "-1.25e-7"
    let (negative, sci) = match sci.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, sci.as_str()),
    };
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = if digits.chars().all(|c| c == '0') { "0".to_string() } else { digits };
    // .NET's scale: value = 0.d1d2... * 10^scale.
    let scale = exponent + 1;
    let max_digits = (digits.len() as i32).max(15);
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if digits != "0" && (scale > max_digits || scale < -3) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = scale - 1;
        out.push('E');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", e.abs()));
    } else if digits == "0" {
        out.push('0');
    } else if scale <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-scale) as usize));
        out.push_str(&digits);
    } else if scale as usize >= digits.len() {
        out.push_str(&digits);
        out.push_str(&"0".repeat(scale as usize - digits.len()));
    } else {
        out.push_str(&digits[..scale as usize]);
        out.push('.');
        out.push_str(&digits[scale as usize..]);
    }
    out
}

/// An object under construction, in property order.
#[derive(Default)]
pub struct Obj(Vec<(String, Json)>);

impl Obj {
    pub fn new() -> Obj {
        Obj(Vec::new())
    }

    pub fn put(&mut self, key: &str, value: Json) -> &mut Obj {
        self.0.push((key.to_string(), value));
        self
    }

    /// A property that is left out when null (`WhenWritingNull` / `WhenWritingDefault` on a nullable).
    pub fn opt(&mut self, key: &str, value: Option<Json>) -> &mut Obj {
        if let Some(value) = value {
            self.0.push((key.to_string(), value));
        }
        self
    }

    pub fn build(&mut self) -> Json {
        Json::Obj(std::mem::take(&mut self.0))
    }
}

/// A .NET Dictionary that is only added to: enumerated in insertion order, and `dict[key] = value` on an existing
/// key replaces the value where it is (and keeps the first key).
#[derive(Debug, Clone)]
pub struct Dict<K: Eq + Hash + Clone, V> {
    index: HashMap<K, usize>,
    pub items: Vec<(K, V)>,
}

impl<K: Eq + Hash + Clone, V> Default for Dict<K, V> {
    fn default() -> Self {
        Dict { index: HashMap::new(), items: Vec::new() }
    }
}

impl<K: Eq + Hash + Clone, V> Dict<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: K, value: V) {
        match self.index.get(&key) {
            Some(&i) => self.items[i].1 = value,
            None => {
                self.index.insert(key.clone(), self.items.len());
                self.items.push((key, value));
            }
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.index.get(key).map(|&i| &self.items[i].1)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.index.get(key).map(|&i| &mut self.items[i].1)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// System.Text.Json's default encoder (JavaScriptEncoder.Default): only printable ASCII outside the HTML-sensitive
/// characters is written as is; `"` & ' + < > ` and DEL become \u00XX, every non-ASCII character \uXXXX (UTF-16,
/// upper-case hex), the usual control characters their short escapes.
fn escape(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => out.push_str(&format!("\\u{:04X}", c as u32)),
            ' '..='~' => out.push(c),
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

fn indent(out: &mut String, depth: usize) {
    out.push_str(NEWLINE);
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(value: &Json, depth: usize, out: &mut String) {
    match value {
        Json::Raw(text) => out.push_str(text),
        Json::Str(text) => escape(text, out),
        Json::Arr(items) => {
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
                write_value(item, depth + 1, out);
            }
            indent(out, depth);
            out.push(']');
        }
        Json::Obj(properties) => {
            if properties.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in properties.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                escape(key, out);
                out.push_str(": ");
                write_value(item, depth + 1, out);
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// The text `JsonSerializer.Serialize(value, new JsonSerializerOptions { WriteIndented = true })` gives.
pub fn to_string(value: &Json) -> String {
    let mut out = String::new();
    write_value(value, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_as_the_converter_writes_them() {
        let f = |v: f32| match Json::float(v).unwrap() {
            Json::Raw(t) => t,
            _ => unreachable!(),
        };
        assert_eq!(f(1.0), "1");
        assert_eq!(f(-0.0), "0");
        assert_eq!(f(0.5), "0.5");
        assert_eq!(f(0.1), "0.1");
        assert_eq!(f(-1.25), "-1.25");
        assert_eq!(f(0.000001), "0.000001");
        assert_eq!(f(-0.00005), "-0.000050");
        assert_eq!(f(0.0001), "0.0001");
        assert_eq!(f(3.3333333), "3.3333333");
        assert!(Json::float(f32::NAN).is_err());
    }

    #[test]
    fn doubles_as_utf8jsonwriter_writes_them() {
        assert_eq!(dotnet_general(1.0), "1");
        assert_eq!(dotnet_general(-0.0), "-0");
        assert_eq!(dotnet_general(0.5), "0.5");
        assert_eq!(dotnet_general(1000.0), "1000");
        assert_eq!(dotnet_general(1e15), "1E+15");
        assert_eq!(dotnet_general(123456789012345.0), "123456789012345");
        assert_eq!(dotnet_general(0.0001), "0.0001");
        assert_eq!(dotnet_general(0.00001), "1E-05");
        assert_eq!(dotnet_general(-1.5e-7), "-1.5E-07");
        assert_eq!(dotnet_general(1.2345678901234568e20), "1.2345678901234568E+20");
        assert_eq!(Json::double(f64::NAN), Json::str("NaN"));
    }

    #[test]
    fn indented_and_escaped() {
        let json = Obj::new()
            .put("a", Json::int(1))
            .put("b<+>", Json::Arr(vec![Json::int(0), Json::str("é\"\\\n")]))
            .put("c", Json::Obj(vec![]))
            .put("d", Json::Arr(vec![]))
            .build();
        let expected = "{\n  \"a\": 1,\n  \"b\\u003C\\u002B\\u003E\": [\n    0,\n    \"\\u00E9\\u0022\\\\\\n\"\n  ],\n  \"c\": {},\n  \"d\": []\n}";
        assert_eq!(to_string(&json), expected.replace('\n', NEWLINE));
    }
}
