//! Builds the example plugin `hello` (source/plugins/hello) into an `.aiop` for the tests.
//!
//! The package is produced by `source/tools/pack_plugin.py` and cached per feature in
//! `target/test-plugins/`, so repeated tests (and test binaries) do not rebuild it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn build(feature: &str) -> Vec<u8> {
    let root = repo_root();
    let cache = root.join("target/test-plugins");
    std::fs::create_dir_all(&cache).unwrap();
    let out = cache.join(if feature.is_empty() { "hello.aiop".to_string() } else { format!("hello-{feature}.aiop") });
    // Several tests (in parallel threads) may ask at once: build to a private file, then rename.
    if out.is_file() {
        return std::fs::read(&out).unwrap();
    }
    let tmp = cache.join(format!("{}.{}.tmp", out.file_name().unwrap().to_string_lossy(), std::process::id()));
    let script = root.join("source/tools/pack_plugin.py");
    let plugin = root.join("source/plugins/hello");
    let mut last_err = String::new();
    for python in ["python", "python3"] {
        let mut cmd = Command::new(python);
        cmd.arg(&script).arg(&plugin).arg("--out").arg(&tmp);
        if !feature.is_empty() {
            cmd.args(["--features", feature]);
        }
        match cmd.output() {
            Ok(o) if o.status.success() => {
                let _ = std::fs::rename(&tmp, &out);
                return std::fs::read(if out.is_file() { &out } else { &tmp }).unwrap();
            }
            Ok(o) => {
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if text.contains("rustup target add wasm32-wasip2") {
                    panic!("install the target: rustup target add wasm32-wasip2");
                }
                panic!("packing the hello plugin failed:\n{text}");
            }
            Err(e) => last_err = format!("{python}: {e}"),
        }
    }
    panic!("python is needed to build the test plugin ({last_err})");
}
