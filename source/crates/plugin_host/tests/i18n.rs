//! Plugin translations: the language file rules (at install and in developer folders), the lookup with its English
//! fallback (a running instance's language change is in `runtime.rs`).

use std::io::Write;

use aio_plugin_host::i18n::{self, Texts};
use aio_plugin_host::{inspect, install};

fn manifest() -> String {
    serde_json::json!({
        "id": "hello", "name": "Hello", "version": "1.0.0", "author": "me", "description": "d",
        "api": "1.1", "permissions": [], "page_title": "Hello page",
    })
    .to_string()
}

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in files {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// The error of inspecting a package with one more file, `name` holding `data`.
fn refusal(name: &str, data: &[u8]) -> String {
    let z = zip_of(&[("manifest.json", manifest().as_bytes()), ("plugin.wasm", b"\0asm"), (name, data)]);
    format!("{:#}", inspect(&z, &[]).expect_err(name))
}

#[test]
fn i18n_file_rules() {
    assert_eq!(refusal("i18n/it.json", b"[\"a\"]"), "i18n/it.json is not a JSON object of texts");
    assert_eq!(refusal("i18n/it.json", b"not json"), "i18n/it.json is not a JSON object of texts");
    assert_eq!(refusal("i18n/it.json", br#"{"Start": 3}"#), r#"i18n/it.json: the translation of "Start" is not a string"#);
    assert_eq!(refusal("i18n/it.json", br#"{"Start": {"a": "b"}}"#), r#"i18n/it.json: the translation of "Start" is not a string"#);

    let many: serde_json::Map<String, serde_json::Value> = (0..5_001).map(|i| (format!("k{i}"), "v".into())).collect();
    let many = serde_json::to_vec(&many).unwrap();
    assert_eq!(refusal("i18n/it.json", &many), "i18n/it.json has more than 5000 texts");
    let just_enough: serde_json::Map<String, serde_json::Value> = (0..5_000).map(|i| (format!("k{i}"), "v".into())).collect();
    assert!(Texts::parse("it", &serde_json::to_vec(&just_enough).unwrap()).is_ok());

    let long_key = serde_json::json!({ "x".repeat(4 * 1024 + 1): "v" }).to_string();
    assert_eq!(refusal("i18n/it.json", long_key.as_bytes()), "i18n/it.json: a text is longer than 4 KiB");
    let long_value = serde_json::json!({ "k": "x".repeat(4 * 1024 + 1) }).to_string();
    assert_eq!(refusal("i18n/it.json", long_value.as_bytes()), "i18n/it.json: a text is longer than 4 KiB");

    assert_eq!(refusal("i18n/xx.json", b"{}"), "Unknown language in the package: xx");
    assert_eq!(refusal("i18n/IT.json", b"{}"), "Unknown language in the package: IT");

    assert_eq!(
        refusal("i18n/de.json", r#"{"{count} files in {folder}": "{count} Dateien"}"#.as_bytes()),
        r#"i18n/de.json: "{count} files in {folder}" lost {folder}"#
    );
    // A spoofing key is shown display-safe in the refusal.
    let spoof = refusal("i18n/de.json", "{\"a\u{202e}b{x}\": \"y\"}".as_bytes());
    assert!(!spoof.contains('\u{202e}'), "{spoof}");

    // Deeply nested JSON is refused, not a crash.
    let deep = format!("{{\"a\": {}{}}}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(refusal("i18n/it.json", deep.as_bytes()), "i18n/it.json is not a JSON object of texts");

    // en.json is the English text itself: accepted and ignored (even when it is not valid).
    let z = zip_of(&[("manifest.json", manifest().as_bytes()), ("plugin.wasm", b"\0asm"), ("i18n/en.json", b"whatever")]);
    let pkg = inspect(&z, &[]).unwrap();
    assert!(pkg.i18n.is_empty());

    // A good file installs.
    let z = zip_of(&[("manifest.json", manifest().as_bytes()), ("plugin.wasm", b"\0asm"), ("i18n/it.json", r#"{"{n} files": "{n} file"}"#.as_bytes())]);
    let pkg = inspect(&z, &[]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = install(&pkg, dir.path()).unwrap();
    let (all, warnings) = i18n::load_dir(&path);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(all.get("it").and_then(|t| t.get("{n} files")), Some("{n} file"));
}

#[test]
fn developer_folder_files_become_warnings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("i18n")).unwrap();
    std::fs::write(dir.path().join("i18n/it.json"), r#"{"Start": "Avvia"}"#).unwrap();
    std::fs::write(dir.path().join("i18n/fr.json"), r#"{"Start": 1}"#).unwrap();
    std::fs::write(dir.path().join("i18n/xx.json"), "{}").unwrap();
    std::fs::write(dir.path().join("i18n/en.json"), "{}").unwrap();
    std::fs::write(dir.path().join("i18n/notes.txt"), "ignored").unwrap();
    let (all, warnings) = i18n::load_dir(dir.path());
    assert_eq!(all.keys().collect::<Vec<_>>(), ["it"]);
    assert_eq!(
        warnings,
        [r#"i18n/fr.json: the translation of "Start" is not a string; the file is ignored"#, "Unknown language: i18n/xx.json is ignored"]
    );
    // No folder, no texts and no warning.
    assert_eq!(i18n::load_dir(&dir.path().join("missing")), (Default::default(), Vec::new()));
}

#[test]
fn translate_falls_back_to_english() {
    let texts = Texts::parse("it", r#"{"Start": "Avvia", "Empty": "", "{n} files": "{n} file"}"#.as_bytes()).unwrap();
    assert_eq!(texts.language(), "it");
    assert_eq!(texts.get("Start"), Some("Avvia"));
    assert_eq!(texts.get("{n} files"), Some("{n} file"));
    // Missing or empty: no translation (the caller shows the English text, never a blank).
    assert_eq!(texts.get("Stop"), None);
    assert_eq!(texts.get("Empty"), None);
    assert_eq!(i18n::translate("Stop", Some(&texts)), "Stop");
    assert_eq!(i18n::translate("Empty", Some(&texts)), "Empty");
    assert_eq!(i18n::translate("Start", None), "Start");
    assert_eq!(i18n::translate("Start", Some(&texts)), "Avvia");
}
