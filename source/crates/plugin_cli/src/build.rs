//! `build` (cargo for wasm32-wasip2) and `dev`. Mirrors source/tools/pack_plugin.py: the same cargo command, the
//! same target dir for a feature build and the same place to find the built wasm.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const TARGET: &str = "wasm32-wasip2";

/// The build flags shared by `build` and `pack`.
#[derive(Default)]
pub struct Options {
    pub locked: bool,
    pub offline: bool,
    pub features: Option<String>,
}

impl From<&crate::Args> for Options {
    fn from(args: &crate::Args) -> Options {
        Options { locked: args.switch("locked"), offline: args.switch("offline"), features: args.flag("features").map(str::to_string) }
    }
}

/// The error for a missing target, given the output of `rustup target list --installed`; `None` when it is there.
pub fn missing_target(installed: &str) -> Option<String> {
    (!installed.split_whitespace().any(|t| t == TARGET))
        .then(|| format!("the {TARGET} target is not installed. Install it with:\n\n    rustup target add {TARGET}\n"))
}

/// Checks with rustup that the target is installed. Without rustup, cargo reports what is missing.
fn check_target() -> Result<()> {
    let Ok(out) = Command::new("rustup").args(["target", "list", "--installed"]).output() else { return Ok(()) };
    if out.status.success() {
        if let Some(message) = missing_target(&String::from_utf8_lossy(&out.stdout)) {
            bail!(message);
        }
    }
    Ok(())
}

/// A plugin folder: a Cargo.toml and a manifest.json.
pub fn require_plugin_dir(dir: &Path) -> Result<()> {
    if !dir.join("Cargo.toml").is_file() || !dir.join("manifest.json").is_file() {
        bail!("{} needs a Cargo.toml and a manifest.json (is it a plugin folder?)", dir.display());
    }
    Ok(())
}

fn cargo(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("cargo").args(args).current_dir(dir).output().context("cannot run cargo (is Rust installed?)")?;
    if !out.status.success() {
        bail!("cargo {} failed:\n{}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The package name from Cargo.toml, dashes as underscores (the wasm file is named after it).
fn crate_name(dir: &Path) -> Result<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).context("cannot read Cargo.toml")?;
    // Like pack_plugin.py: the first `name = "..."` line.
    let name = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("name")?.trim_start().strip_prefix('=')?.trim().strip_prefix('"'))
        .find_map(|rest| rest.split_once('"').map(|(name, _)| name.to_string()))
        .filter(|name| !name.is_empty())
        .context("no package name in Cargo.toml")?;
    Ok(name.replace('-', "_"))
}

/// A feature build gets its own target dir, so the variants do not overwrite each other.
fn feature_target_dir(dir: &Path, features: &str) -> PathBuf {
    let safe: String = features.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-') { c } else { '_' }).collect();
    dir.join("target").join(format!("features-{safe}"))
}

/// Where the release build puts the wasm (without building).
pub fn wasm_path(dir: &Path, options: &Options) -> Result<PathBuf> {
    let target_dir = match &options.features {
        Some(features) => feature_target_dir(dir, features),
        None => {
            // The plugin may be a member of a workspace, which owns the target dir.
            let mut args = vec!["metadata", "--format-version", "1", "--no-deps"];
            args.extend(options.locked.then_some("--locked"));
            args.extend(options.offline.then_some("--offline"));
            let meta: serde_json::Value = serde_json::from_str(&cargo(dir, &args)?).context("cargo metadata gave no JSON")?;
            PathBuf::from(meta["target_directory"].as_str().context("cargo metadata has no target_directory")?)
        }
    };
    Ok(target_dir.join(TARGET).join("release").join(format!("{}.wasm", crate_name(dir)?)))
}

/// Builds the plugin in `dir` and returns the built wasm.
pub fn build(dir: &Path, options: &Options) -> Result<PathBuf> {
    require_plugin_dir(dir)?;
    check_target()?;
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--target", TARGET]).current_dir(dir);
    if options.locked {
        let found = cargo(dir, &["locate-project", "--workspace", "--message-format", "plain"])?;
        let lock = Path::new(found.trim()).parent().unwrap_or(dir).join("Cargo.lock");
        if !lock.is_file() {
            bail!("--locked needs a Cargo.lock next to the workspace's Cargo.toml: {} is missing", lock.display());
        }
        cmd.arg("--locked");
    }
    if options.offline {
        cmd.arg("--offline");
    }
    if let Some(features) = &options.features {
        cmd.args(["--features", features]).arg("--target-dir").arg(feature_target_dir(dir, features));
    }
    let status = cmd.status().context("cannot run cargo (is Rust installed?)")?;
    if !status.success() {
        bail!("the cargo build failed");
    }
    let wasm = wasm_path(dir, options)?;
    if !wasm.is_file() {
        bail!("the build did not produce {} (is crate-type cdylib?)", wasm.display());
    }
    Ok(wasm)
}

/// `dev`: the folder to load in developer mode, and how.
pub fn dev_text(dir: &Path) -> Result<String> {
    require_plugin_dir(dir)?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?).context("manifest.json is not valid JSON")?;
    let wasm = manifest["dev_wasm"].as_str().unwrap_or("plugin.wasm");
    let mut text = format!(
        "Developer folder: {}\n\n\
         1. In the Manager, open Settings and turn on Developer mode.\n\
         2. On the Plugins page, under \"Developer plugins\", click \"Load a plugin folder\" and pick the folder above.\n\
         3. After each `aio-plugin build`, reload the plugin from its card.\n\n\
         The Manager loads {wasm} (the manifest's dev_wasm) from that folder.\n",
        dir.display()
    );
    if !dir.join(wasm).is_file() {
        text.push_str("It is not built yet: run `aio-plugin build` first.\n");
    }
    // Where the build really puts the wasm: if that is not dev_wasm, the Manager will not find it.
    if let Ok(built) = wasm_path(dir, &Options::default()) {
        let wanted = dir.join(wasm);
        let same = |a: &Path, b: &Path| a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
        let parent_same = match (built.parent().and_then(|p| p.canonicalize().ok()), wanted.parent().and_then(|p| p.canonicalize().ok())) {
            (Some(a), Some(b)) => a == b && built.file_name() == wanted.file_name(),
            _ => false,
        };
        if !same(&built, &wanted) && !parent_same {
            text.push_str(&format!(
                "\nWarning: `aio-plugin build` puts the wasm at {}, but dev_wasm names {wasm}. Add a .cargo/config.toml with\n  [build]\n  target-dir = \"target\"\nnext to Cargo.toml (or change dev_wasm), or the Manager will not find the plugin.\n",
                built.display()
            ));
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_without_target_has_clear_error() {
        // rustup lists other targets only: the error says the exact command to run.
        let message = missing_target("x86_64-pc-windows-gnu\nwasm32-wasip1\n").unwrap();
        assert!(message.contains("\n    rustup target add wasm32-wasip2\n"), "{message}");
        assert!(message.contains("wasm32-wasip2 target is not installed"), "{message}");
        assert_eq!(missing_target("x86_64-pc-windows-gnu\nwasm32-wasip2\n"), None);
    }

    #[test]
    fn crate_name_and_feature_dir_match_pack_plugin_py() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n  name = \"my-plugin\"\nversion = \"1.0.0\"\n").unwrap();
        assert_eq!(crate_name(dir.path()).unwrap(), "my_plugin");
        assert!(feature_target_dir(dir.path(), "a,b c").ends_with("target/features-a_b_c"));
    }
}
