//! Keeps `i18n/it.json` in step with the texts the plugin passes to `tr` / `tr_with`.
//!
//! The scanner reads `src/*.rs` as text. Its limits: it finds `tr("...")` and `tr_with("..."` whose first argument
//! is a plain string literal (with `\"`, `\\`, `\n`, `\t`, `\u{..}` and a `\` at the end of a line understood), and it
//! fails on a call whose first argument is anything else, so a text can not slip past it by being built elsewhere.
//! It does not parse Rust: a `tr(` inside a comment or a string would count, and raw strings are not read.
//!
//! The file's keys must be exactly these texts plus the manifest's (the Manager translates those itself, by their
//! English text), so the file can neither lack a text nor keep one the plugin no longer uses.

use std::collections::{BTreeMap, BTreeSet};

/// The plugin's sources, as `(file, text)`.
fn sources() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            found.push((path.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&path).unwrap()));
        }
    }
    found
}

/// The string literal that starts at `chars[at]` (the opening quote): its text and the index after it.
fn literal(chars: &[char], at: usize) -> (String, usize) {
    assert_eq!(chars[at], '"');
    let mut text = String::new();
    let mut i = at + 1;
    while i < chars.len() {
        match chars[i] {
            '"' => return (text, i + 1),
            '\\' => {
                i += 1;
                match chars[i] {
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    '"' | '\\' | '\'' => text.push(chars[i]),
                    // A backslash at the end of a line: the line break and the next line's indent are dropped.
                    '\n' | '\r' => {
                        while chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                            i += 1;
                        }
                    }
                    'u' => {
                        let end = chars[i..].iter().position(|c| *c == '}').unwrap() + i;
                        let hex: String = chars[i + 2..end].iter().collect();
                        text.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                        i = end;
                    }
                    other => panic!("an escape the scanner does not know: \\{other}"),
                }
                i += 1;
            }
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    panic!("a string literal never ends");
}

/// Every text passed to `tr` / `tr_with` in `text`.
fn used_texts(file: &str, text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = BTreeSet::new();
    for name in ["tr", "tr_with"] {
        let call: Vec<char> = name.chars().chain(['(']).collect();
        let mut i = 0;
        while i + call.len() <= chars.len() {
            let boundary = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            if boundary && chars[i..i + call.len()] == call[..] {
                let mut at = i + call.len();
                while chars[at].is_whitespace() {
                    at += 1;
                }
                assert_eq!(chars[at], '"', "{file}: a call of {name} whose first argument is not a string literal");
                let (english, end) = literal(&chars, at);
                found.insert(english);
                i = end;
            } else {
                i += 1;
            }
        }
    }
    found
}

/// The `{name}` placeholders of a text, as the Manager reads them (from a `{` to the next `}`).
fn placeholders(text: &str) -> BTreeSet<String> {
    text.split('{').skip(1).filter_map(|part| part.split_once('}')).map(|(name, _)| name.to_string()).collect()
}

fn read_json(path: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap()
}

/// The manifest's texts the Manager translates itself.
fn manifest_texts() -> BTreeSet<String> {
    let manifest = read_json("manifest.json");
    let text = |value: &serde_json::Value| value.as_str().unwrap().to_string();
    [&manifest["name"], &manifest["description"], &manifest["page_title"], &manifest["tile"]["title"], &manifest["tile"]["description"]]
        .into_iter()
        .map(text)
        .collect()
}

#[test]
fn the_scanner_reads_literals() {
    let code = "ui.text(tr(\"A \\\"quoted\\\" \\u{2013} text\")); tr_with(\n    \"Two \\\n     lines {n}\",\n    &[]); fn str(x) { str(\"no\") }";
    let used = used_texts("sample", code);
    assert_eq!(used.into_iter().collect::<Vec<_>>(), ["A \"quoted\" \u{2013} text", "Two lines {n}"]);
}

#[test]
fn italian_covers_every_text_and_only_those() {
    let json = read_json("i18n/it.json");
    let italian: BTreeMap<String, String> = json
        .as_object()
        .expect("it.json is an object")
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().expect("every value is a string").to_string()))
        .collect();

    let mut expected = manifest_texts();
    for (file, text) in sources() {
        expected.extend(used_texts(&file, &text));
    }
    assert!(expected.len() > 40, "the scanner found only {} texts", expected.len());

    let missing: Vec<_> = expected.iter().filter(|text| !italian.contains_key(*text)).collect();
    assert!(missing.is_empty(), "texts without a translation in it.json: {missing:#?}");
    let unused: Vec<_> = italian.keys().filter(|key| !expected.contains(*key)).collect();
    assert!(unused.is_empty(), "it.json keys the plugin never uses: {unused:#?}");

    for (english, translation) in &italian {
        assert!(!translation.trim().is_empty(), "empty translation of {english:?}");
        let lost: Vec<_> = placeholders(english).difference(&placeholders(translation)).cloned().collect();
        assert!(lost.is_empty(), "the translation of {english:?} lost {lost:?}");
    }
}
