//! The string helpers of the C# (its regexes, .NET's case rules, number parsing and file listing order), and the
//! symbols.csv every mapper reads, rebuilt in memory exactly as Tools/NativeSwfExtractor.cs writes it.

use std::cmp::Ordering;

use anyhow::{bail, Result};

/// One character through .NET's invariant simple case mapping (a character whose full mapping is several
/// characters keeps itself, as ToUpperInvariant/ToLowerInvariant do).
fn simple(c: char, upper: bool) -> char {
    let mapped: Vec<char> = if upper { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
    match (mapped.first().copied(), mapped.get(1)) {
        (Some(one), None) => one,
        _ => c,
    }
}

pub fn to_lower(text: &str) -> String {
    text.chars().map(|c| simple(c, false)).collect()
}

pub fn to_upper(text: &str) -> String {
    text.chars().map(|c| simple(c, true)).collect()
}

/// The key of a `StringComparer.OrdinalIgnoreCase` dictionary (ordinal on upper-cased characters).
pub fn fold(text: &str) -> String {
    to_upper(text)
}

/// `name.EndsWith(suffix, StringComparison.OrdinalIgnoreCase)`.
pub fn ends_with_ci(text: &str, suffix: &str) -> bool {
    fold(text).ends_with(&fold(suffix))
}

/// `Regex.Replace(name, @"(?<=^|_)(cfc_|cf_)", m => m.Value.ToUpperInvariant(), RegexOptions.IgnoreCase)`:
/// left to right, non-overlapping, the look-behind seeing the original text.
pub fn force_cf_upper(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let starts = |at: usize, pattern: &str| {
        let p: Vec<char> = pattern.chars().collect();
        at + p.len() <= chars.len() && chars[at..at + p.len()].iter().zip(&p).all(|(a, b)| a.to_ascii_lowercase() == *b)
    };
    let mut i = 0;
    while i < chars.len() {
        if i == 0 || chars[i - 1] == '_' {
            let length = if starts(i, "cfc_") {
                4
            } else if starts(i, "cf_") {
                3
            } else {
                0
            };
            if length > 0 {
                out.extend(chars[i..i + length].iter().map(|c| c.to_ascii_uppercase()));
                i += length;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// SpriteSheetMapper.CleanAssetName: `Regex.Replace(name, @"^([^_]+)_\1_", "$1_")`. `[^_]+` followed by `_` can
/// only be the whole first segment, so: "a_a_rest" -> "a_rest" (the back-reference is case-sensitive).
pub fn clean_asset_name(name: &str) -> String {
    if let Some((first, rest)) = name.split_once('_') {
        if !first.is_empty() {
            if let Some(after) = rest.strip_prefix(first).and_then(|r| r.strip_prefix('_')) {
                return format!("{first}_{after}");
            }
        }
    }
    name.to_string()
}

/// AssetsMapper.RemoveSwfPrefix: `Regex.Replace(name, "^" + Regex.Escape(prefix) + "_", "", IgnoreCase)`.
pub fn remove_swf_prefix(name: &str, prefix: &str) -> String {
    if name.is_empty() || prefix.is_empty() {
        return name.to_string();
    }
    let name_chars: Vec<char> = name.chars().collect();
    let prefix_chars: Vec<char> = prefix.chars().collect();
    let n = prefix_chars.len();
    let matches = name_chars.len() > n
        && name_chars[..n].iter().zip(&prefix_chars).all(|(a, b)| simple(*a, true) == simple(*b, true) || simple(*a, false) == simple(*b, false))
        && name_chars[n] == '_';
    if matches {
        name_chars[n + 1..].iter().collect()
    } else {
        name.to_string()
    }
}

/// The order Directory.GetFiles lists a folder in on Windows: NTFS keeps a directory sorted by the names
/// upper-cased through its $UpCase table, compared as UTF-16 code units. (On Linux the order is whatever the file
/// system gives, so the Windows order is the reference here.)
pub fn ntfs_order(a: &str, b: &str) -> Ordering {
    let key = |s: &str| -> Vec<u16> { to_upper(s).encode_utf16().collect() };
    key(a).cmp(&key(b))
}

/// The whitespace .NET's number parsing skips around a number (NumberStyles.AllowLeading/TrailingWhite).
fn number_trim(text: &str) -> &str {
    text.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' '))
}

/// `int.TryParse(value, out var v) ? v : null` (NumberStyles.Integer): optional white space, an optional sign,
/// decimal digits only, in Int32's range.
pub fn parse_int(value: Option<&str>) -> Option<i32> {
    let text = number_trim(value?);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for b in digits.bytes() {
        v = v * 10 + i64::from(b - b'0');
        if v > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    let v = if negative { -v } else { v };
    i32::try_from(v).ok()
}

/// The text of a float under NumberStyles.Float with the invariant culture, made ready for Rust's parser: optional
/// white space and sign, digits with an optional point (at least one digit), an optional exponent; or "NaN",
/// "Infinity" (.NET Core 3.0+ matches these case-insensitively). Rust alone would also take "inf" and "nan" forms.
fn float_text(value: Option<&str>) -> Option<String> {
    let text = number_trim(value?);
    let (sign, body) = match text.as_bytes().first() {
        Some(b'-') => ("-", &text[1..]),
        Some(b'+') => ("", &text[1..]),
        _ => ("", text),
    };
    if body.eq_ignore_ascii_case("nan") {
        return Some("NaN".into());
    }
    if body.eq_ignore_ascii_case("infinity") {
        return Some(format!("{sign}inf"));
    }
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    Some(format!("{sign}{body}"))
}

/// `float.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out var v) ? v : null`
/// (correctly rounded; an overflow is an infinity, as in .NET Core 3.0+).
pub fn parse_float(value: Option<&str>) -> Option<f32> {
    float_text(value)?.parse().ok()
}

/// The double variant of `parse_float`.
pub fn parse_double(value: Option<&str>) -> Option<f64> {
    float_text(value)?.parse().ok()
}

/// `Convert.ToInt32(hex, 16)`: an optional '+', an optional "0x"/"0X", hex digits up to 32 bits (taken as the
/// bits of an Int32: "FFFFFFFF" is -1), nothing else; anything else throws in .NET, which fails the conversion.
pub fn convert_hex(text: &str) -> Result<i32> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    if chars.first() == Some(&'-') {
        bail!("The colour \"{text}\" is negative.");
    }
    if chars.first() == Some(&'+') {
        i += 1;
    }
    if i + 1 < chars.len() && chars[i] == '0' && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
        i += 2;
    }
    let start = i;
    let mut value: u64 = 0;
    while i < chars.len() {
        let Some(digit) = chars[i].to_digit(16) else { break };
        value = value * 16 + u64::from(digit);
        if value > u64::from(u32::MAX) {
            bail!("The colour \"{text}\" is too large.");
        }
        i += 1;
    }
    if i == start || i < chars.len() {
        bail!("The colour \"{text}\" is not a hexadecimal number.");
    }
    Ok(value as u32 as i32)
}

/// File.ReadAllLines: lines split at "\r\n", "\r" or "\n"; a final line break does not make an empty line.
pub fn read_all_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut pending = false;
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
                pending = false;
            }
            _ => {
                current.push(c);
                pending = true;
            }
        }
    }
    if pending {
        lines.push(current);
    }
    lines
}

/// symbols.csv as NativeSwfExtractor writes it (`{id};"{name}"` per symbol, in file order), read back as
/// File.ReadAllLines gives it.
pub fn csv_lines(symbols: &[(u16, String)]) -> Vec<String> {
    let text: String = symbols.iter().map(|(id, name)| format!("{id};\"{name}\"\r\n")).collect();
    read_all_lines(&text)
}

/// The (tag, name) of every non-blank CSV line with at least two fields: `parts[0].Trim()`,
/// `parts[1].Trim().Replace("\"", "")` (a name with ';' is cut there, as the C# does).
pub fn csv_entries(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.split(';');
            let tag = parts.next()?;
            let name = parts.next()?;
            Some((tag.trim().to_string(), name.trim().replace('"', "")))
        })
        .collect()
}

/// `string.IsNullOrEmpty`. (.NET's `string.Trim()` white space and Rust's `trim()` agree: Unicode White_Space.)
pub fn is_null_or_empty(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(str::is_empty)
}

/// A file name Windows refuses (File.Move then throws, and the C# skips that file).
pub fn invalid_file_name(name: &str) -> bool {
    name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regexes() {
        assert_eq!(force_cf_upper("cf_5_cfc_10_cf_x"), "CF_5_CFC_10_CF_x");
        assert_eq!(force_cf_upper("cf_cf_x"), "CF_CF_x");
        assert_eq!(force_cf_upper("xcf_5"), "xcf_5");
        assert_eq!(force_cf_upper("Cf_5"), "CF_5");
        assert_eq!(clean_asset_name("chair_chair_64_a_0_0"), "chair_64_a_0_0");
        assert_eq!(clean_asset_name("chair_64_a_0_0"), "chair_64_a_0_0");
        assert_eq!(clean_asset_name("Chair_chair_64"), "Chair_chair_64");
        assert_eq!(clean_asset_name("_x_x_"), "_x_x_");
        assert_eq!(remove_swf_prefix("Chair_chair_64", "chair"), "chair_64");
        assert_eq!(remove_swf_prefix("chair", "chair"), "chair");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_int(Some(" -12 ")), Some(-12));
        assert_eq!(parse_int(Some("+3")), Some(3));
        assert_eq!(parse_int(Some("1.5")), None);
        assert_eq!(parse_int(Some("2147483648")), None);
        assert_eq!(parse_int(Some("-2147483648")), Some(i32::MIN));
        assert_eq!(parse_int(None), None);
        assert_eq!(parse_float(Some("0.5")), Some(0.5));
        assert_eq!(parse_float(Some(".5")), Some(0.5));
        assert_eq!(parse_float(Some("1e2")), Some(100.0));
        assert_eq!(parse_float(Some("inf")), None);
        assert_eq!(parse_float(Some("1,5")), None);
        assert!(parse_float(Some("NaN")).unwrap().is_nan());
        assert_eq!(convert_hex("FF0000").unwrap(), 0xFF0000);
        assert_eq!(convert_hex("0xffffffff").unwrap(), -1);
        assert!(convert_hex("#FF0000").is_err());
        assert!(convert_hex("100000000").is_err());
    }

    #[test]
    fn listing_order() {
        let mut names = vec!["b.png", "a_b.png", "A.png", "10.png", "9.png"];
        names.sort_by(|a, b| ntfs_order(a, b));
        assert_eq!(names, ["10.png", "9.png", "A.png", "a_b.png", "b.png"]);
    }
}
