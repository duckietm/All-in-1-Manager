use aio_plugin_host::permissions::Permission;
use aio_plugin_host::{
    inspect, install, install_failing_after_move_aside, install_failing_with_data_restore_failure, install_with_fault, installed, remove, Fault,
    Manifest, Step,
};
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
    for api in ["2.0", "1.2", "0.9", "1", "1.0.0.0", "x"] {
        let z = zip_of(&[("manifest.json", manifest("hello", api, &[]).as_bytes()), ("plugin.wasm", b"x")]);
        assert!(inspect(&z, &RESERVED).is_err(), "{api}");
    }
    for api in ["1.0.0", "1.0", "1.1", "1.1.0"] {
        let z = zip_of(&[("manifest.json", manifest("hello", api, &[]).as_bytes()), ("plugin.wasm", b"x")]);
        assert!(inspect(&z, &RESERVED).is_ok(), "{api}");
    }
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

#[test]
fn windows_reserved_id_is_refused() {
    for id in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
        assert!(inspect(&good(id), &RESERVED).is_err(), "{id}");
    }
    for id in ["con-x", "com0", "com10", "lpt"] {
        assert!(inspect(&good(id), &RESERVED).is_ok(), "{id}");
    }
}

#[test]
fn swap_failure_after_move_aside_restores_old_version_and_data() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    std::fs::write(path.join("data").join("keep.txt"), b"mine").unwrap();
    let mut v2 = v1.clone();
    v2.wasm = b"new".to_vec();
    assert!(install_failing_after_move_aside(&v2, dir.path()).is_err());
    assert_eq!(std::fs::read(path.join("plugin.wasm")).unwrap(), b"\0asm");
    assert_eq!(std::fs::read(path.join("data").join("keep.txt")).unwrap(), b"mine");
    assert!(!dir.path().join(".hello.partial").exists() && !dir.path().join(".hello.old").exists());
}

#[test]
fn stray_old_folder_from_a_crash_keeps_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    std::fs::write(path.join("data").join("keep.txt"), b"mine").unwrap();
    // Simulate a crash after the old version was moved aside.
    std::fs::rename(&path, dir.path().join(".hello.old")).unwrap();
    install(&v1, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("data").join("keep.txt")).unwrap(), b"mine");
    assert!(!dir.path().join(".hello.old").exists());
}

#[test]
fn data_left_in_partial_by_a_crash_survives_next_install() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    // Crash after data was moved into .partial, before the swap.
    std::fs::create_dir_all(dir.path().join(".hello.partial").join("data")).unwrap();
    std::fs::write(dir.path().join(".hello.partial").join("data").join("x"), b"mine").unwrap();
    std::fs::remove_dir_all(path.join("data")).ok();
    assert!(!path.join("data").exists());
    install(&v1, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("data").join("x")).unwrap(), b"mine");
    assert!(!dir.path().join(".hello.partial").exists());
}

#[test]
fn failed_data_restore_keeps_partial_and_next_install_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    std::fs::write(path.join("data").join("keep.txt"), b"mine").unwrap();
    let err = install_failing_with_data_restore_failure(&v1, dir.path()).unwrap_err();
    assert!(format!("{err:#}").contains(".hello.partial"), "{err:#}");
    assert_eq!(std::fs::read(dir.path().join(".hello.partial/data/keep.txt")).unwrap(), b"mine");
    install(&v1, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("data/keep.txt")).unwrap(), b"mine");
}

#[test]
fn crash_with_data_split_between_old_and_partial_loses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    std::fs::write(path.join("data/keep.txt"), b"mine").unwrap();
    // Crash after move-aside: data in .partial, the rest of the old version in .old, no live folder.
    let partial = dir.path().join(".hello.partial");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::rename(path.join("data"), partial.join("data")).unwrap();
    std::fs::rename(&path, dir.path().join(".hello.old")).unwrap();
    install(&v1, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("data/keep.txt")).unwrap(), b"mine");
}

#[test]
fn leftover_old_data_next_to_a_live_folder_without_data_survives_the_upgrade() {
    // Finding from re-review 3: live folder without data/, `.old` with data/, install reported success and lost it.
    let dir = tempfile::tempdir().unwrap();
    let v1 = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&v1, dir.path()).unwrap();
    let old = dir.path().join(".hello.old");
    std::fs::create_dir_all(old.join("data")).unwrap();
    std::fs::write(old.join("data/keep.txt"), b"mine").unwrap();
    std::fs::remove_dir_all(path.join("data")).unwrap();
    let mut v2 = v1.clone();
    v2.wasm = b"new".to_vec();
    install(&v2, dir.path()).unwrap();
    assert_eq!(std::fs::read(path.join("plugin.wasm")).unwrap(), b"new");
    assert_eq!(std::fs::read(path.join("data/keep.txt")).unwrap(), b"mine");
    assert!(!old.exists());
}

/// What one of the three folders looks like before an install.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Absent,
    NoData,
    EmptyData,
    WithData,
}

const SLOTS: [Slot; 4] = [Slot::Absent, Slot::NoData, Slot::EmptyData, Slot::WithData];
const NAMES: [&str; 3] = ["live", "old", "partial"];

fn folder(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    match name {
        "live" => root.join("hello"),
        _ => root.join(format!(".hello.{name}")),
    }
}

/// Lays out live/.old/.partial. A folder with data holds `data/marker-<name>`.
fn lay_out(root: &std::path::Path, slots: [Slot; 3]) {
    for (name, slot) in NAMES.iter().zip(slots) {
        let dir = folder(root, name);
        if slot == Slot::Absent {
            continue;
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.wasm"), name.as_bytes()).unwrap();
        if slot != Slot::NoData {
            std::fs::create_dir_all(dir.join("data")).unwrap();
        }
        if slot == Slot::WithData {
            std::fs::write(dir.join("data").join(format!("marker-{name}")), name.as_bytes()).unwrap();
        }
    }
}

/// Every place each marker file is found under `root`.
fn markers(root: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let (path, name) = (entry.path(), entry.file_name().to_string_lossy().into_owned());
            if path.is_dir() {
                todo.push(path);
            } else if let Some(marker) = name.strip_prefix("marker-") {
                out.push((marker.to_string(), path));
            }
        }
    }
    out
}

/// Checks the invariant after an install: each marker exists exactly once; on success the most recent one
/// (partial, then live, then old) is in the live folder's data/.
fn check(root: &std::path::Path, slots: [Slot; 3], ok: bool, what: &str) {
    let found = markers(root);
    let placed: Vec<&str> = NAMES.iter().zip(slots).filter(|(_, s)| *s == Slot::WithData).map(|(n, _)| *n).collect();
    for name in &placed {
        let copies = found.iter().filter(|(m, _)| m == name).count();
        assert_eq!(copies, 1, "{what}: marker-{name} found {copies} times: {found:?}");
    }
    assert_eq!(found.len(), placed.len(), "{what}: {found:?}");
    if ok {
        let live_data = folder(root, "live").join("data");
        assert!(live_data.is_dir(), "{what}: success without live data/");
        if let Some(winner) = ["partial", "live", "old"].into_iter().find(|n| placed.contains(n)) {
            assert!(live_data.join(format!("marker-{winner}")).is_file(), "{what}: success but marker-{winner} not in live: {found:?}");
        }
    }
}

fn combinations() -> Vec<[Slot; 3]> {
    let mut out = Vec::new();
    for live in SLOTS {
        for old in SLOTS {
            for partial in SLOTS {
                out.push([live, old, partial]);
            }
        }
    }
    out
}

#[test]
fn every_leftover_state_installs_with_the_data_kept_exactly_once() {
    let pkg = inspect(&good("hello"), &RESERVED).unwrap();
    for slots in combinations() {
        let dir = tempfile::tempdir().unwrap();
        lay_out(dir.path(), slots);
        let what = format!("{slots:?}");
        let path = install(&pkg, dir.path()).unwrap_or_else(|e| panic!("{what}: {e:#}"));
        check(dir.path(), slots, true, &what);
        assert_eq!(std::fs::read(path.join("plugin.wasm")).unwrap(), b"\0asm", "{what}");
        assert!(!folder(dir.path(), "old").exists() && !folder(dir.path(), "partial").exists(), "{what}");
        assert_eq!(installed(dir.path()).len(), 1, "{what}");
    }
}

#[test]
fn every_leftover_state_survives_a_failure_or_crash_at_every_step() {
    let pkg = inspect(&good("hello"), &RESERVED).unwrap();
    let mut faults = Vec::new();
    for step in Step::ALL {
        faults.push(Fault { crash_after: Some(step), ..Default::default() });
        if step != Step::Recover {
            faults.push(Fault { fail: Some(step), ..Default::default() });
            faults.push(Fault { fail: Some(step), fail_data_restore: true, ..Default::default() });
        }
    }
    for slots in combinations() {
        for fault in faults.iter().copied() {
            let dir = tempfile::tempdir().unwrap();
            lay_out(dir.path(), slots);
            let what = format!("{slots:?} {fault:?}");
            let result = install_with_fault(&pkg, dir.path(), fault);
            check(dir.path(), slots, result.is_ok(), &what);
            // Whatever was left, the next install recovers it and puts the data in live.
            install(&pkg, dir.path()).unwrap_or_else(|e| panic!("{what}, then install: {e:#}"));
            check(dir.path(), slots, true, &format!("{what}, then install"));
            assert!(!folder(dir.path(), "old").exists() && !folder(dir.path(), "partial").exists(), "{what}");
        }
    }
}

#[test]
fn display_safe_strips_spoofing_characters() {
    use aio_plugin_host::display_safe;
    assert_eq!(display_safe("Good\u{202E}evil", 80), "Good\u{FFFD}evil");
    assert_eq!(display_safe("a\u{200B}b\u{2066}c\u{FEFF}d\u{E0041}e\u{0007}f", 80), "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f");
    assert_eq!(display_safe("  one \n\t two   three  ", 80), "one two three");
    assert_eq!(display_safe("abcdefghij", 5), "abcd\u{2026}");
    assert_eq!(display_safe("abcde", 5), "abcde");
}

#[test]
fn manifest_with_bidi_override_is_refused() {
    for (field, bad) in [("name", "Hel\u{202E}lo"), ("author", "me\u{200B}"), ("description", "d\u{0007}"), ("page_title", "P\u{2066}")] {
        let mut m: serde_json::Value = serde_json::from_str(&manifest("hello", "1.0", &[])).unwrap();
        m[field] = bad.into();
        let z = zip_of(&[("manifest.json", m.to_string().as_bytes()), ("plugin.wasm", b"\0asm")]);
        assert!(inspect(&z, &RESERVED).is_err(), "{field}");
    }
    let mut m: serde_json::Value = serde_json::from_str(&manifest("hello", "1.0", &[])).unwrap();
    m["tile"]["title"] = "T\u{202E}".into();
    let z = zip_of(&[("manifest.json", m.to_string().as_bytes()), ("plugin.wasm", b"\0asm")]);
    assert!(inspect(&z, &RESERVED).is_err());
}

#[test]
fn permission_lists_are_capped_and_without_repeats() {
    use aio_plugin_host::manifest::MAX_PERMISSIONS;
    let parse = |perms: &[&str]| Manifest::parse(manifest("hello", "1.0", perms).as_bytes());
    let ports: Vec<String> = (1..=MAX_PERMISSIONS + 1).map(|p| format!("tcp:localhost:{p}")).collect();
    let ports: Vec<&str> = ports.iter().map(String::as_str).collect();
    assert_eq!(parse(&ports[..MAX_PERMISSIONS]).unwrap().permissions.len(), MAX_PERMISSIONS);
    assert!(parse(&ports).unwrap_err().to_string().contains("at most 64"));
    for repeated in [
        &["ssh", "ssh"][..],
        &["sql:read:rooms,rooms"],
        &["sql:read:rooms", "sql:read:items,rooms"],
        &["sql:write:rooms", "sql:write:rooms"],
        &["http:example.com", "http:EXAMPLE.com"],
        &["sql:read-private:users.mail", "sql:read-private:users.mail"],
    ] {
        assert!(parse(repeated).unwrap_err().to_string().contains("listed twice"), "{repeated:?}");
    }
    // The same table for reading and for writing is two different permissions.
    assert_eq!(parse(&["sql:read:rooms", "sql:write:rooms", "ssh", "ssh:root"]).unwrap().permissions.len(), 4);
}

/// Names in `dir`, sorted.
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn remove_leaves_nothing_behind_and_a_reinstall_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = inspect(&good("hello"), &RESERVED).unwrap();
    let path = install(&pkg, dir.path()).unwrap();
    std::fs::write(path.join("data/secret.txt"), b"mine").unwrap();
    // What a crash can leave, and data copies kept aside by a recovery.
    for leftover in [".hello.partial/data", ".hello.old/data", ".hello.data-from-old", ".hello.data-from-live-2"] {
        std::fs::create_dir_all(dir.path().join(leftover)).unwrap();
        std::fs::write(dir.path().join(leftover).join("x"), b"stranded").unwrap();
    }
    std::fs::write(dir.path().join(".hello.data-from-partial"), b"a file in that place").unwrap();
    // Another plugin, whose id only starts the same, keeps its things.
    let other = install(&inspect(&good("hello-two"), &RESERVED).unwrap(), dir.path()).unwrap();
    std::fs::create_dir_all(dir.path().join(".hello-two.data-from-old")).unwrap();

    remove("hello", dir.path()).unwrap();
    assert_eq!(names(dir.path()), vec![".hello-two.data-from-old".to_string(), "hello-two".to_string()]);
    assert!(other.join("plugin.wasm").is_file());

    // Nothing of the old plugin comes back with a new install of the same id.
    let path = install(&pkg, dir.path()).unwrap();
    assert!(names(&path.join("data")).is_empty());
    // Removing what is not installed is fine.
    remove("hello", dir.path()).unwrap();
    remove("hello", dir.path()).unwrap();
}

#[test]
fn install_without_data_sets_the_old_data_aside() {
    use aio_plugin_host::package::install_without_data;
    let dir = tempfile::tempdir().unwrap();
    let pkg = inspect(&good("hello"), &RESERVED).unwrap();
    // A fresh install: nothing to set aside.
    let (path, aside) = install_without_data(&pkg, dir.path()).unwrap();
    assert!(aside.is_none());
    std::fs::write(path.join("data/secret.txt"), b"mine").unwrap();
    let (path, aside) = install_without_data(&pkg, dir.path()).unwrap();
    assert_eq!(aside.as_deref(), Some(dir.path().join(".hello.data-from-previous-author").as_path()));
    assert!(names(&path.join("data")).is_empty(), "the new plugin starts empty");
    assert_eq!(std::fs::read(dir.path().join(".hello.data-from-previous-author/secret.txt")).unwrap(), b"mine");
    // Data a crash left in `.partial` is found too: it is the old plugin's as well.
    std::fs::create_dir_all(dir.path().join(".hello.partial/data")).unwrap();
    std::fs::write(dir.path().join(".hello.partial/data/late.txt"), b"late").unwrap();
    let (path, aside) = install_without_data(&pkg, dir.path()).unwrap();
    assert_eq!(aside.as_deref(), Some(dir.path().join(".hello.data-from-previous-author-2").as_path()));
    assert!(names(&path.join("data")).is_empty());
    assert_eq!(std::fs::read(dir.path().join(".hello.data-from-previous-author-2/late.txt")).unwrap(), b"late");
}

#[test]
fn port_range_parses_and_limits() {
    assert_eq!(Permission::parse("tcp:localhost:9092-9101").unwrap(), Permission::TcpLocalRange(9092, 9101));
    assert_eq!(Permission::parse("tcp:localhost:9092-9101").unwrap().to_string(), "tcp:localhost:9092-9101");
    // At most 16 ports: 9092-9107 is 16 of them.
    assert_eq!(Permission::parse("tcp:localhost:9092-9107").unwrap(), Permission::TcpLocalRange(9092, 9107));
    for bad in [
        "tcp:localhost:9092-9200",
        "tcp:localhost:9092-9108",
        "tcp:localhost:9101-9092",
        "tcp:localhost:9092-9092",
        "tcp:localhost:0-10",
        "tcp:localhost:65530-65536",
        "tcp:localhost:9092-",
        "tcp:localhost:-9092",
        "tcp:localhost:9092-+9093",
        "tcp:localhost:9092-9093-9094",
    ] {
        assert!(Permission::parse(bad).is_err(), "{bad}");
    }
    // Single ports are unchanged.
    assert_eq!(Permission::parse("tcp:localhost:9092").unwrap(), Permission::TcpLocal(9092));
    let words = Permission::parse("tcp:localhost:9092-9101").unwrap().plain_words();
    assert_eq!(words.english_text(), "Connect to programs on this computer, ports 9092\u{2013}9101 (for example G-Earth)");
    // A 1.1 permission needs a 1.1 manifest.
    let parse = |api: &str, perms: &[&str]| Manifest::parse(manifest("hello", api, perms).as_bytes());
    assert!(parse("1.1", &["tcp:localhost:9092-9101", "http:ask"]).is_ok());
    for p in ["tcp:localhost:9092-9101", "http:ask"] {
        let e = parse("1.0", &[p]).unwrap_err().to_string();
        assert!(e.contains("needs API 1.1"), "{e}");
    }
    assert_eq!(Permission::parse("http:ask").unwrap(), Permission::HttpAsk);
    assert_eq!(Permission::HttpAsk.to_string(), "http:ask");
    assert_eq!(Permission::HttpAsk.plain_words().english_text(), "Ask you before fetching from any other internet host");
}
