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
    build_with(dir, &["--previous", previous], out)
}

fn build_with(dir: &Path, extra: &[&str], out: &Path) -> Output {
    let entries = dir.join("entries");
    let revoked = dir.join("revoked.json");
    let mut args = vec!["build", "--entries", s(&entries), "--revoked", s(&revoked), "--days", "30", "--prefix", PREFIX, "--out", s(out)];
    args.extend_from_slice(extra);
    run(&args)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A key made with `keygen` in `dir`: (secret key file, public key base64).
fn keypair(dir: &Path) -> (std::path::PathBuf, String) {
    std::fs::create_dir_all(dir).unwrap();
    let out = ok(&["keygen", "--out", s(dir)]);
    (dir.join("secret.key"), line(&out, "public:").to_string())
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
    let (secret, public) = keypair(&t.path().join("key"));
    let first = t.path().join("first.json");
    assert!(build_index(t.path(), "none", &first).status.success());
    let v: Value = serde_json::from_slice(&std::fs::read(&first).unwrap()).unwrap();
    assert_eq!(v["sequence"], 1);
    assert_eq!(v["plugins"][0]["id"], "alpha");
    assert_eq!(v["plugins"][1]["id"], "zeta");
    ok(&["sign", "--key", s(&secret), "--prefix", PREFIX, s(&first)]);
    let second = t.path().join("second.json");
    let o = build_with(t.path(), &["--previous", s(&first), "--pubkey", &public], &second);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: Value = serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
    assert_eq!(v["sequence"], 2);
}

#[test]
fn build_min_sequence() {
    let t = tempfile::tempdir().unwrap();
    setup_entries(t.path());
    let (secret, public) = keypair(&t.path().join("key"));
    let seq = |p: &Path| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap()["sequence"].as_u64().unwrap();
    // Recovery after a lost index.json: jump past what Managers stored.
    let first = t.path().join("first.json");
    let o = build_with(t.path(), &["--previous", "none", "--min-sequence", "500"], &first);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(seq(&first), 500);
    // Below previous + 1 it changes nothing.
    ok(&["sign", "--key", s(&secret), "--prefix", PREFIX, s(&first)]);
    let second = t.path().join("second.json");
    let o = build_with(t.path(), &["--previous", s(&first), "--pubkey", &public, "--min-sequence", "3"], &second);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(seq(&second), 501);
    for bad in ["0", "-1", "x", ""] {
        let o = build_with(t.path(), &["--previous", "none", "--min-sequence", bad], &t.path().join("bad.json"));
        assert!(!o.status.success() && stderr(&o).contains("--min-sequence"), "{bad}: {}", stderr(&o));
    }
}

#[test]
fn build_verifies_previous_index() {
    let t = tempfile::tempdir().unwrap();
    setup_entries(t.path());
    let (old_secret, old_public) = keypair(&t.path().join("old"));
    let (_, new_public) = keypair(&t.path().join("new"));
    let previous = t.path().join("previous.json");
    assert!(build_index(t.path(), "none", &previous).status.success());
    let out = t.path().join("out.json");
    let fails = |extra: &[&str], why: &str| {
        let o = build_with(t.path(), extra, &out);
        assert!(!o.status.success(), "{extra:?} should fail");
        assert!(stderr(&o).contains(why), "{extra:?}: {}", stderr(&o));
        assert!(!out.exists(), "nothing is written");
    };
    let p = s(&previous);
    // No signature next to it, or no key to check it with.
    fails(&["--previous", p, "--pubkey", &old_public], "previous.json.sig");
    ok(&["sign", "--key", s(&old_secret), "--prefix", PREFIX, p]);
    fails(&["--previous", p], "--pubkey");
    // Signed by another key.
    fails(&["--previous", p, "--pubkey", &new_public], "does not verify");
    fails(&["--previous", p, "--pubkey", "not base64!"], "not valid base64");
    // Changed after signing: its sequence is not trusted.
    let original = std::fs::read(&previous).unwrap();
    let mut v: Value = serde_json::from_slice(&original).unwrap();
    v["sequence"] = serde_json::json!(1000);
    std::fs::write(&previous, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    fails(&["--previous", p, "--pubkey", &old_public], "does not verify");
    std::fs::write(&previous, &original).unwrap();
    // During a key rotation both keys are given: the old index verifies with the old one, the sequence goes on.
    let both = format!("{new_public} {old_public}");
    let o = build_with(t.path(), &["--previous", p, "--pubkey", &both], &out);
    assert!(o.status.success(), "{}", stderr(&o));
    let built: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(built["sequence"], 2);
    // --previous none (the first index) needs no key.
    assert!(build_index(t.path(), "none", &out).status.success());
}

#[test]
fn build_fails_on_bad_entry_file() {
    let fails_naming = |setup: &dyn Fn(&Path), file: &str| {
        let t = tempfile::tempdir().unwrap();
        setup_entries(t.path());
        setup(&t.path().join("entries"));
        let out = t.path().join("index.json");
        let o = build_index(t.path(), "none", &out);
        assert!(!o.status.success(), "{file} should fail the build");
        assert!(stderr(&o).contains(file), "the error names {file}: {}", stderr(&o));
        assert!(!out.exists());
    };
    // Not json.
    fails_naming(&|d| std::fs::write(d.join("broken.json"), "{ not json").unwrap(), "broken.json");
    // Json, but not an entry (an unknown field).
    fails_naming(
        &|d| {
            entry_json(d, "extra.json", "extra");
            let mut v: Value = serde_json::from_slice(&std::fs::read(d.join("extra.json")).unwrap()).unwrap();
            v["unknown"] = serde_json::json!(1);
            std::fs::write(d.join("extra.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        },
        "extra.json",
    );
    // An entry the Manager would refuse (its url is not under the prefix).
    fails_naming(
        &|d| {
            entry_json(d, "elsewhere.json", "elsewhere");
            let mut v: Value = serde_json::from_slice(&std::fs::read(d.join("elsewhere.json")).unwrap()).unwrap();
            v["url"] = serde_json::json!("https://evil.test/elsewhere.aiop");
            std::fs::write(d.join("elsewhere.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        },
        "elsewhere.json",
    );
    // Unreadable: a folder where an entry file should be.
    fails_naming(&|d| std::fs::create_dir(d.join("folder.json")).unwrap(), "folder.json");
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
