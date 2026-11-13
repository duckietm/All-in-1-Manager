//! Runs the real `aio-index` binary.

#[path = "../../plugin_host/tests/support/mod.rs"]
mod support;

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

const PREFIX: &str = "https://example.test/download/";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aio-index")).args(args).env_remove("AIO_INDEX_KEY").output().unwrap()
}

fn ok(args: &[&str]) -> String {
    let o = run(args);
    assert!(o.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn line<'a>(out: &'a str, key: &str) -> &'a str {
    out.lines().find_map(|l| l.strip_prefix(key)).unwrap().trim()
}

fn entry_json(dir: &Path, file: &str, id: &str) {
    let e = serde_json::json!({
        "id": id, "name": "N", "version": "1.0.0", "author": "A", "description": "D", "api": "1.0",
        "permissions": [], "size": 10,
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "url": format!("{PREFIX}{id}-1.0.0/{id}.aiop"),
        "source": { "repo": "https://example.test/r", "commit": "abc" }
    });
    std::fs::write(dir.join(file), serde_json::to_vec(&e).unwrap()).unwrap();
}

fn build_index(dir: &Path, previous: &str, out: &Path) -> Output {
    run(&[
        "build",
        "--entries",
        s(&dir.join("entries")),
        "--revoked",
        s(&dir.join("revoked.json")),
        "--previous",
        previous,
        "--days",
        "30",
        "--prefix",
        PREFIX,
        "--out",
        s(out),
    ])
}

fn setup_entries(dir: &Path) {
    std::fs::create_dir_all(dir.join("entries")).unwrap();
    entry_json(&dir.join("entries"), "zeta.json", "zeta");
    entry_json(&dir.join("entries"), "alpha.json", "alpha");
    std::fs::write(dir.join("revoked.json"), "[]").unwrap();
}

#[test]
fn keygen_sign_verify_cli() {
    let t = tempfile::tempdir().unwrap();
    setup_entries(t.path());
    let out = ok(&["keygen", "--out", s(t.path())]);
    let public = line(&out, "public:").to_string();
    assert_eq!(line(&out, "key-id:").len(), 16);
    let secret = std::fs::read_to_string(t.path().join("secret.key")).unwrap();
    assert!(!out.contains(secret.trim()));
    let index = t.path().join("index.json");
    let b = build_index(t.path(), "none", &index);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    ok(&["sign", "--key", s(&t.path().join("secret.key")), "--prefix", PREFIX, s(&index)]);
    let sig = t.path().join("index.json.sig");
    assert!(sig.exists());
    let v = ok(&["verify", "--pubkey", &public, "--prefix", PREFIX, s(&index), s(&sig)]);
    assert_eq!(v.trim(), "ok sequence=1 plugins=2");
    // A changed index no longer verifies.
    let mut bytes = std::fs::read(&index).unwrap();
    bytes.push(b' ');
    std::fs::write(&index, bytes).unwrap();
    assert!(!run(&["verify", "--pubkey", &public, "--prefix", PREFIX, s(&index), s(&sig)]).status.success());
}

#[test]
fn entry_from_hello_package() {
    let t = tempfile::tempdir().unwrap();
    let aiop = t.path().join("hello.aiop");
    let bytes = support::hello_aiop();
    std::fs::write(&aiop, &bytes).unwrap();
    let out = ok(&["entry", "--aiop", s(&aiop), "--prefix", PREFIX, "--repo", "https://example.test/hello", "--commit", "abc123"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], "hello");
    let version = v["version"].as_str().unwrap();
    assert_eq!(v["url"], format!("{PREFIX}hello-{version}/hello.aiop"));
    assert_eq!(v["size"], bytes.len());
    assert_eq!(v["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(v["source"]["commit"], "abc123");
    assert!(v["permissions"].is_array());
}

#[test]
fn build_increments_sequence() {
    let t = tempfile::tempdir().unwrap();
    setup_entries(t.path());
    let first = t.path().join("first.json");
    assert!(build_index(t.path(), "none", &first).status.success());
    let v: Value = serde_json::from_slice(&std::fs::read(&first).unwrap()).unwrap();
    assert_eq!(v["sequence"], 1);
    assert_eq!(v["plugins"][0]["id"], "alpha");
    assert_eq!(v["plugins"][1]["id"], "zeta");
    let second = t.path().join("second.json");
    assert!(build_index(t.path(), s(&first), &second).status.success());
    let v: Value = serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
    assert_eq!(v["sequence"], 2);
}

#[test]
fn sign_refuses_invalid_index() {
    let t = tempfile::tempdir().unwrap();
    ok(&["keygen", "--out", s(t.path())]);
    let index = t.path().join("index.json");
    std::fs::write(&index, "{\"schema\": 2}").unwrap();
    let o = run(&["sign", "--key", s(&t.path().join("secret.key")), "--prefix", PREFIX, s(&index)]);
    assert!(!o.status.success());
    assert!(!t.path().join("index.json.sig").exists());
}

#[test]
fn keygen_refuses_overwrite() {
    let t = tempfile::tempdir().unwrap();
    ok(&["keygen", "--out", s(t.path())]);
    let before = std::fs::read(t.path().join("secret.key")).unwrap();
    assert!(!run(&["keygen", "--out", s(t.path())]).status.success());
    assert_eq!(std::fs::read(t.path().join("secret.key")).unwrap(), before);
}
