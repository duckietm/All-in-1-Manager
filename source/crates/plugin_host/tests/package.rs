use aio_plugin_host::permissions::Permission;
use aio_plugin_host::{inspect, install, installed, remove, Manifest};
use std::io::Write;

const RESERVED: [&str; 2] = ["default-catalog", "cms"];

fn manifest(id: &str, api: &str, perms: &[&str]) -> String {
    serde_json::json!({
        "id": id, "name": "Hello", "version": "1.2.3", "author": "me", "description": "d",
        "api": api, "permissions": perms, "page_title": "Hello page",
        "tile": { "title": "Hello", "description": "tile", "color": "#aabbcc" }
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

fn good(id: &str) -> Vec<u8> {
    zip_of(&[("manifest.json", manifest(id, "1.0", &["ssh", "files"]).as_bytes()), ("plugin.wasm", b"\0asm")])
}

#[test]
fn valid_package_installs_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&pkg, dir.path()).unwrap();
    assert!(path.join("plugin.wasm").is_file() && path.join("data").is_dir());
    let list = installed(dir.path());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "hello");
    assert_eq!(list[0].permissions, vec![Permission::Ssh, Permission::Files]);
    remove("hello", dir.path()).unwrap();
    assert!(installed(dir.path()).is_empty());
}

#[test]
fn zip_with_parent_path_is_refused() {
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes()), ("plugin.wasm", b"x"), ("../evil.txt", b"x")]);
    assert!(inspect(&z, &RESERVED).is_err());
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes()), ("plugin.wasm", b"x"), ("i18n/../../a.json", b"x")]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn absolute_path_is_refused() {
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes()), ("plugin.wasm", b"x"), ("/etc/passwd", b"x")]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn unexpected_file_is_refused() {
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes()), ("plugin.wasm", b"x"), ("run.exe", b"x")]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn id_with_dots_is_refused() {
    for id in ["a.b", "..", "../x", "Hello", "a", "-ab"] {
        assert!(inspect(&good(id), &RESERVED).is_err(), "{id}");
    }
}

#[test]
fn builtin_id_is_refused() {
    assert!(inspect(&good("default-catalog"), &RESERVED).is_err());
}

#[test]
fn api_2_is_refused() {
    for api in ["2.0", "1.1", "0.9", "1", "1.0.0.0", "x"] {
        let z = zip_of(&[("manifest.json", manifest("hello", api, &[]).as_bytes()), ("plugin.wasm", b"x")]);
        assert!(inspect(&z, &RESERVED).is_err(), "{api}");
    }
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0.0", &[]).as_bytes()), ("plugin.wasm", b"x")]);
    assert!(inspect(&z, &RESERVED).is_ok());
}

#[test]
fn missing_wasm_is_refused() {
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes())]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn oversized_wasm_is_refused() {
    let big = vec![0u8; 25 * 1024 * 1024];
    let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[]).as_bytes()), ("plugin.wasm", &big)]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn unknown_permission_is_refused() {
    for p in ["root", "sql:read:", "sql:read:a;b", "tcp:localhost:0", "tcp:localhost:99999", "http:", "sql:read-private:users", "ssh "] {
        let z = zip_of(&[("manifest.json", manifest("hello", "1.0", &[p]).as_bytes()), ("plugin.wasm", b"x")]);
        assert!(inspect(&z, &RESERVED).is_err(), "{p}");
    }
}

#[test]
fn bad_version_is_refused() {
    let json = manifest("hello", "1.0", &[]).replace("1.2.3", "1.2");
    assert!(Manifest::parse(json.as_bytes()).is_err());
}

#[test]
fn failed_install_keeps_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    std::fs::write(path.join("data").join("keep.txt"), b"mine").unwrap();
    // A file where the .partial folder must go makes the second install fail.
    std::fs::write(dir.path().join(".hello.partial"), b"block").unwrap();
    let mut v2 = v1.clone();
    v2.wasm = b"new".to_vec();
    assert!(install(&v2, dir.path()).is_err());
    assert_eq!(std::fs::read(path.join("plugin.wasm")).unwrap(), b"\0asm");
    std::fs::remove_file(dir.path().join(".hello.partial")).unwrap();
    // A good upgrade replaces the code and keeps private data.
    install(&v2, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("plugin.wasm")).unwrap(), b"new");
    assert_eq!(std::fs::read(path.join("data").join("keep.txt")).unwrap(), b"mine");
}

#[test]
fn installed_skips_broken_and_leftover_folders() {
    let dir = tempfile::tempdir().unwrap();
    install(&inspect(&good("hello"), &RESERVED).unwrap(), dir.path()).unwrap();
    std::fs::create_dir_all(dir.path().join("broken")).unwrap();
    std::fs::write(dir.path().join("broken").join("manifest.json"), b"{nope").unwrap();
    std::fs::create_dir_all(dir.path().join(".hello.partial")).unwrap();
    std::fs::create_dir_all(dir.path().join(".hello.old")).unwrap();
    assert_eq!(installed(dir.path()).len(), 1);
}

#[test]
fn permission_strings_round_trip() {
    for s in [
        "ssh", "ssh:root", "files", "sql:read:users", "sql:read:users,rooms", "sql:write:catalog_items,catalog_pages",
        "sql:read-private:users.mail", "http:example.com", "http:api.example.com", "tcp:localhost:8080",
    ] {
        assert_eq!(Permission::parse(s).unwrap().to_string(), s);
    }
}
