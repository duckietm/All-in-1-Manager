//! The Rust samples of the plugin guide (`docs/plugins/*.md`) still compile, so the guide cannot drift from the SDK.
//!
//! Every ```rust block is taken from the Markdown files as it is:
//! - a block with `export_plugin!` is a whole plugin: it becomes the `lib.rs` of its own crate, together with the
//!   `#[cfg(test)]` block that follows it in the same file;
//! - a block that only lists signatures (`pub trait ...`, `pub enum ...`) is skipped;
//! - any other block goes into a module of the last plugin crate (statements are wrapped in a function).
//!
//! Each crate is built for wasm32-wasip2 and its tests run natively. Like `out_of_tree.rs`, this needs the
//! `wasm32-wasip2` target and the guest workspace's dependencies in the local cargo cache (`--offline`); the output
//! goes to `source/plugins/target/guide-samples`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The ```rust blocks of `text`, in order.
fn rust_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match &mut current {
            None if line == "```rust" => current = Some(String::new()),
            Some(block) if line == "```" => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {}
        }
    }
    assert!(current.is_none(), "an unclosed ```rust block");
    blocks
}

struct Sample {
    /// Where the plugin comes from, for error messages.
    origin: String,
    lib: String,
}

/// The plugin crates to build: every `export_plugin!` block (with its test block), the other blocks as modules of
/// the last one.
fn samples(guide: &Path) -> Vec<Sample> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(guide).unwrap().map(|e| e.unwrap().path()).collect();
    files.retain(|p| p.extension().is_some_and(|e| e == "md"));
    files.sort();
    let mut plugins: Vec<Sample> = Vec::new();
    let mut snippets = String::from("#![allow(dead_code, unused_imports)]\n");
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let mut last_plugin_here: Option<usize> = None;
        for (i, block) in rust_blocks(&std::fs::read_to_string(file).unwrap()).into_iter().enumerate() {
            let first = block.trim_start();
            if block.contains("export_plugin!(") {
                plugins.push(Sample { origin: format!("{name} block {i}"), lib: block });
                last_plugin_here = Some(plugins.len() - 1);
            } else if first.starts_with("#[cfg(test)]") {
                let index = last_plugin_here.unwrap_or_else(|| panic!("{name} block {i}: a test block without a plugin before it"));
                plugins[index].lib.push('\n');
                plugins[index].lib.push_str(&block);
            } else if first.starts_with("pub trait ") || first.starts_with("pub enum ") {
                // A signature listing, not code.
            } else if block.lines().any(|l| l.starts_with("fn ")) {
                snippets.push_str(&format!("\n// {name} block {i}\nmod sample_{}_{i} {{\n{block}}}\n", module_name(&name)));
            } else {
                snippets.push_str(&format!("\n// {name} block {i}\nmod sample_{}_{i} {{\nfn sample() {{\n{block}}}\n}}\n", module_name(&name)));
            }
        }
    }
    let last = plugins.last_mut().expect("the guide has no full plugin sample");
    last.lib.push_str("\nmod snippets;\n");
    last.origin.push_str(" (+ the other samples in mod snippets)");
    // Kept apart from the plugin so its `use` lines do not clash.
    plugins.push(Sample { origin: "snippets".into(), lib: snippets });
    plugins
}

fn module_name(file: &str) -> String {
    file.trim_end_matches(".md").replace(['-', '.'], "_")
}

fn cargo(dir: &Path, target: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO"))
        .current_dir(dir)
        .args(args)
        .arg("--offline")
        .env("CARGO_TARGET_DIR", target)
        .env_remove("RUSTFLAGS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo {} failed in {}:\n{}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_guide_samples_compile_and_their_tests_pass() {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guest_workspace = sdk.parent().unwrap();
    let guide = sdk.join("../../../docs/plugins");
    let target = guest_workspace.join("target").join("guide-samples");

    let mut crates = samples(&guide);
    // The last entry holds the snippets: it becomes `src/snippets.rs` of the crate before it.
    let snippets = crates.pop().unwrap().lib;
    assert!(crates.len() >= 2, "expected the getting-started and the ui plugins, found {}", crates.len());

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("aio-plugin-guide-samples-{}-{nanos}", std::process::id()));
    let count = crates.len();
    for (n, sample) in crates.into_iter().enumerate() {
        let dir = root.join(format!("guide-sample-{n}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let manifest = format!(
            "[package]\nname = \"guide-sample-{n}\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n\
             [lib]\ncrate-type = [\"cdylib\"]\n\n\
             [dependencies]\naio-plugin-sdk = {{ path = '{}' }}\n\n[workspace]\n",
            sdk.to_str().unwrap()
        );
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(dir.join("src/lib.rs"), format!("// From docs/plugins/{}\n{}", sample.origin, sample.lib)).unwrap();
        if n == count - 1 {
            std::fs::write(dir.join("src/snippets.rs"), &snippets).unwrap();
        }
        std::fs::copy(guest_workspace.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();

        cargo(&dir, &target, &["build", "--target", "wasm32-wasip2"]);
        cargo(&dir, &target, &["test"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
