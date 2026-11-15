//! The `aio-plugin` binary end to end. These tests build for wasm32-wasip2 (the target must be installed); they
//! need no network when the cargo cache already holds the crates of source/plugins.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn aio_plugin(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aio-plugin")).args(args).current_dir(dir).output().unwrap()
}

fn ok(output: Output, what: &str) -> String {
    let (stdout, stderr) = (String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{what} failed:\n{stdout}\n{stderr}");
    stdout.into_owned()
}

/// A Python 3 whose zlib is stock zlib (on Windows `python3` may be the store's placeholder, and python.org 3.14+
/// is built on zlib-ng, whose deflate gives other bytes).
fn python() -> Option<&'static str> {
    let stock = |p: &&str| {
        let probe = Command::new(p).args(["-c", "import zlib; print(hasattr(zlib, 'ZLIBNG_VERSION') or 'ng' in zlib.ZLIB_RUNTIME_VERSION)"]).output();
        probe.is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "False")
    };
    ["python3", "python", "py"].into_iter().find(stock)
}

#[test]
fn pack_matches_pack_plugin_py() {
    // pack.rs checks the deflate bytes against a stock-zlib golden vector without Python; this compares with the
    // script itself, which needs a Python on stock zlib (CI pins Python 3.13).
    let Some(python) = python() else {
        eprintln!("SKIPPED pack_matches_pack_plugin_py: no Python 3 on stock zlib (python3, python or py) on PATH; a zlib-ng Python packs other bytes");
        return;
    };
    let hello = repo().join("source/plugins/hello");
    let out = tempfile::tempdir().unwrap();
    let (py, rs) = (out.path().join("py.aiop"), out.path().join("rs.aiop"));
    let script = repo().join("source/tools/pack_plugin.py");
    let packed = Command::new(python).arg(&script).arg(&hello).args(["--locked", "--out"]).arg(&py).output().unwrap();
    ok(packed, "pack_plugin.py");
    ok(aio_plugin(&hello, &["pack", "--locked", "--out", rs.to_str().unwrap()]), "aio-plugin pack");
    let (py, rs) = (std::fs::read(&py).unwrap(), std::fs::read(&rs).unwrap());
    assert!(py == rs, "aio-plugin pack differs from pack_plugin.py ({} vs {} bytes)", rs.len(), py.len());

    // `check` on the package prints its real size and sha256.
    let text = ok(aio_plugin(out.path(), &["check", "rs.aiop"]), "aio-plugin check");
    assert!(text.contains(&format!("sha256:      {:x}", Sha256::digest(&rs))), "{text}");
    assert!(text.contains("  - files: Keep its own files in a private folder (always allowed)"), "{text}");

    // A broken package is a non-zero exit.
    std::fs::write(out.path().join("bad.aiop"), b"not a zip").unwrap();
    let bad = aio_plugin(out.path(), &["check", "bad.aiop"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not a valid plugin package"));
}

#[test]
fn new_project_builds_and_checks() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("demo-counter");
    let sdk = repo().join("source/plugins/sdk");
    ok(aio_plugin(root.path(), &["new", "demo-counter", "--id", "demo-counter", "--sdk-path", sdk.to_str().unwrap()]), "new");
    let cargo = std::fs::read_to_string(project.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("aio-plugin-sdk = { path = \""), "{cargo}");
    assert!(cargo.contains(r#"# aio-plugin-sdk = { git = "https://github.com/duckietm/All-in-1-Manager", tag = "sdk-v1.0.0" }"#), "{cargo}");

    // A second `new` into the same folder is refused.
    assert!(!aio_plugin(root.path(), &["new", "demo-counter", "--id", "demo-counter"]).status.success());

    // Offline when the cargo cache is warm; a cold cache (a fresh CI runner) needs the network once.
    let offline = aio_plugin(&project, &["pack", "--offline"]);
    let packed = if offline.status.success() { ok(offline, "pack --offline") } else { ok(aio_plugin(&project, &["pack"]), "pack") };
    let aiop = PathBuf::from(packed.trim());
    assert!(aiop.ends_with("dist/demo-counter.aiop"), "{packed}");
    assert!(project.join("target/wasm32-wasip2/release/demo_counter.wasm").is_file(), "dev_wasm is where the manifest says");

    let text = ok(aio_plugin(&project, &["check", aiop.to_str().unwrap()]), "check package");
    assert!(text.contains("id:          demo-counter") && text.contains("name:        Demo Counter"), "{text}");
    let sha = format!("sha256:      {:x}", Sha256::digest(std::fs::read(&aiop).unwrap()));
    assert!(text.contains(&sha), "{text}");
    // The folder, packed in memory, gives the same package.
    let folder = ok(aio_plugin(root.path(), &["check", "demo-counter"]), "check folder");
    assert!(folder.contains(&sha), "{folder}");

    let dev = ok(aio_plugin(&project, &["dev"]), "dev");
    assert!(dev.contains("turn on Developer mode") && dev.contains("demo-counter"), "{dev}");
    assert!(!dev.contains("not built yet"), "{dev}");
}
