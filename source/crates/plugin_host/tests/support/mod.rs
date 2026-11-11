//! Builds the example plugin `hello` (source/plugins/hello) into an `.aiop` for the tests.
//!
//! The package is produced by `source/tools/pack_plugin.py` once per test process and feature
//! (the bytes are kept in memory), so a changed plugin source is always picked up.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The default `hello` package.
pub fn hello_aiop() -> Vec<u8> {
    build("")
}

/// `hello` built with one Cargo feature (`runaway` or `hog`).
pub fn hello_aiop_with(feature: &str) -> Vec<u8> {
    build(feature)
}

/// Bytes of every variant already built by this test process (key: the feature, "" = default).
/// The lock is held during a build, so parallel tests never build (or write) the same variant twice.
fn build(feature: &str) -> Vec<u8> {
    static BUILT: Mutex<BTreeMap<String, Vec<u8>>> = Mutex::new(BTreeMap::new());
    let mut built = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(bytes) = built.get(feature) {
        return bytes.clone();
    }
    // Always run the script once per process: cargo is incremental, so this is quick when nothing
    // changed, and a changed plugin source can never be served from a stale file.
    let bytes = pack(feature);
    built.insert(feature.to_string(), bytes.clone());
    bytes
}

fn pack(feature: &str) -> Vec<u8> {
    let root = repo_root();
    let cache = root.join("target/test-plugins");
    std::fs::create_dir_all(&cache).unwrap();
    let name = if feature.is_empty() { "hello".to_string() } else { format!("hello-{feature}") };
    let out = cache.join(format!("{name}.{}.aiop", std::process::id()));
    let script = root.join("source/tools/pack_plugin.py");
    let plugin = root.join("source/plugins/hello");
    let mut last_err = String::new();
    // `python` may be missing or a Windows Store stub: try the next interpreter then.
    for python in ["python", "python3", "py"] {
        let mut cmd = Command::new(python);
        cmd.arg(&script).arg(&plugin).arg("--out").arg(&out);
        if !feature.is_empty() {
            cmd.args(["--features", feature]);
        }
        match cmd.output() {
            Ok(o) if o.status.success() => {
                let bytes = std::fs::read(&out).unwrap();
                let _ = std::fs::remove_file(&out);
                return bytes;
            }
            Ok(o) => {
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if text.contains("rustup target add wasm32-wasip2") {
                    panic!("install the target: rustup target add wasm32-wasip2");
                }
                if text.contains("pack_plugin") || text.contains("error:") {
                    panic!("packing the hello plugin failed:
{text}");
                }
                last_err = format!("{python}: {text}");
            }
            Err(e) => last_err = format!("{python}: {e}"),
        }
    }
    panic!("python is needed to build the test plugin ({last_err})");
}
