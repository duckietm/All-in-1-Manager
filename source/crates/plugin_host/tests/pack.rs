//! `source/tools/pack_plugin.py`: deterministic packages and `--locked` / `--offline`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Runs pack_plugin.py with the first Python that works (`python` may be a Windows Store stub).
fn pack(plugin: &Path, out: &Path, extra: &[&str]) -> Output {
    let script = repo_root().join("source/tools/pack_plugin.py");
    let mut last = String::new();
    for python in ["python", "python3", "py"] {
        match Command::new(python).arg(&script).arg(plugin).arg("--out").arg(out).args(extra).output() {
            Ok(o) if o.status.success() || String::from_utf8_lossy(&o.stderr).contains("error:") => return o,
            Ok(o) => last = format!("{python}: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => last = format!("{python}: {e}"),
        }
    }
    panic!("python is needed to run pack_plugin.py ({last})");
}

fn packed(plugin: &Path, out: &Path, extra: &[&str]) -> Vec<u8> {
    let o = pack(plugin, out, extra);
    assert!(o.status.success(), "pack_plugin.py {extra:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    std::fs::read(out).unwrap()
}

#[test]
fn packing_hello_twice_gives_identical_bytes() {
    let hello = repo_root().join("source/plugins/hello");
    let dir = tempfile::tempdir().unwrap();
    let first = packed(&hello, &dir.path().join("first.aiop"), &[]);
    // A newer modification time of a packed file changes nothing.
    let manifest = std::fs::File::options().append(true).open(hello.join("manifest.json")).unwrap();
    manifest.set_modified(std::time::SystemTime::now()).unwrap();
    drop(manifest);
    let second = packed(&hello, &dir.path().join("second.aiop"), &["--locked"]);
    assert!(first == second, "the two packages differ");

    // Fixed order, time, mode and compression.
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(first)).unwrap();
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    assert_eq!(names, ["manifest.json", "plugin.wasm"]);
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).unwrap();
        let time = file.last_modified().unwrap();
        assert_eq!((time.year(), time.month(), time.day(), time.hour(), time.minute(), time.second()), (1980, 1, 1, 0, 0, 0));
        assert_eq!(file.unix_mode(), Some(0o100644));
        assert_eq!(file.compression(), zip::CompressionMethod::Deflated);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
    }
}

#[test]
fn locked_needs_a_cargo_lock() {
    // A plugin crate of its own workspace, outside the repository, with no Cargo.lock.
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("nolock");
    std::fs::create_dir_all(plugin.join("src")).unwrap();
    std::fs::write(plugin.join("Cargo.toml"), "[package]\nname = \"nolock\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n").unwrap();
    std::fs::write(plugin.join("src/lib.rs"), "").unwrap();
    std::fs::write(plugin.join("manifest.json"), "{\"id\": \"nolock\"}").unwrap();
    let out = dir.path().join("nolock.aiop");
    let o = pack(&plugin, &out, &["--locked", "--offline"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(stderr.contains("--locked needs a Cargo.lock"), "{stderr}");
    assert!(!plugin.join("Cargo.lock").exists(), "nothing was built (cargo would have written one)");
    assert!(!out.exists());
}
