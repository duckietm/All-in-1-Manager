//! `new`: a plugin project that builds, packs and loads as it is.

use aio_plugin_host::manifest::valid_id;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// The SDK as a git dependency, pinned to its release tag.
pub const SDK_GIT: &str = r#"aio-plugin-sdk = { git = "https://github.com/duckietm/All-in-1-Manager", tag = "sdk-v1.0.0" }"#;

const CARGO_TOML: &str = include_str!("../templates/Cargo.toml.txt");
const MANIFEST: &str = include_str!("../templates/manifest.json.txt");
const LIB_RS: &str = include_str!("../templates/lib.rs.txt");
const GITIGNORE: &str = include_str!("../templates/gitignore.txt");
const README: &str = include_str!("../templates/README.md.txt");

/// The Cargo package name for `id`: the id itself, unless Cargo refuses it as a package name.
fn crate_name(id: &str) -> String {
    let refused = id.starts_with(|c: char| c.is_ascii_digit()) || matches!(id, "std" | "core" | "alloc" | "test" | "proc-macro");
    if refused { format!("plugin-{id}") } else { id.to_string() }
}

/// "my-plugin" -> "My Plugin".
fn display_name(id: &str) -> String {
    id.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The dependency lines: the git one, or with `sdk_path` a path one (the git one stays, commented out).
fn sdk_dependency(sdk_path: Option<&Path>) -> Result<String> {
    Ok(match sdk_path {
        None => format!("{SDK_GIT}\n# For local SDK development, use a path instead:\n# aio-plugin-sdk = {{ path = \"../All-in-1-Manager/source/plugins/sdk\" }}"),
        Some(path) => {
            if !path.join("Cargo.toml").is_file() {
                bail!("--sdk-path {} has no Cargo.toml (it should be source/plugins/sdk)", path.display());
            }
            let absolute = std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))?;
            // Forward slashes work on every system and need no escaping in TOML.
            let text = absolute.to_str().context("--sdk-path is not valid UTF-8")?.replace('\\', "/");
            if text.contains('"') {
                bail!("--sdk-path must not contain a double quote");
            }
            format!("aio-plugin-sdk = {{ path = \"{text}\" }}\n# The released SDK:\n# {SDK_GIT}")
        }
    })
}

/// Writes a new plugin project into `dir`, which must not exist or be empty.
pub fn create(dir: &Path, id: &str, sdk_path: Option<&Path>) -> Result<()> {
    if !valid_id(id) {
        bail!("invalid plugin id {id:?}: 2 to 40 characters, lowercase letters, digits and dashes, starting with a letter or digit");
    }
    if dir.exists() {
        let mut entries = std::fs::read_dir(dir).with_context(|| format!("{} exists and is not a folder", dir.display()))?;
        if entries.next().is_some() {
            bail!("{} is not empty; choose a new folder", dir.display());
        }
    }
    let krate = crate_name(id);
    let name = display_name(id);
    let fill = |template: &str| -> Result<String> {
        Ok(template
            .replace("{{sdk}}", &sdk_dependency(sdk_path)?)
            .replace("{{crate}}", &krate)
            .replace("{{wasm}}", &krate.replace('-', "_"))
            .replace("{{id}}", id)
            .replace("{{name}}", &name))
    };
    std::fs::create_dir_all(dir.join("src")).with_context(|| format!("cannot create {}", dir.display()))?;
    for (file, template) in [("Cargo.toml", CARGO_TOML), ("manifest.json", MANIFEST), ("src/lib.rs", LIB_RS), (".gitignore", GITIGNORE), ("README.md", README)] {
        let path = dir.join(file);
        std::fs::write(&path, fill(template)?).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_refuses_bad_id_and_non_empty_dir() {
        let root = tempfile::tempdir().unwrap();
        assert!(create(&root.path().join("a"), "Bad_Id", None).is_err());
        std::fs::write(root.path().join("file"), "x").unwrap();
        let error = create(root.path(), "good-id", None).unwrap_err();
        assert!(error.to_string().contains("not empty"), "{error}");
    }

    #[test]
    fn new_writes_the_git_dependency_and_a_valid_manifest() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("my-plugin");
        create(&dir, "my-plugin", None).unwrap();
        let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains(r#"aio-plugin-sdk = { git = "https://github.com/duckietm/All-in-1-Manager", tag = "sdk-v1.0.0" }"#), "{cargo}");
        assert!(cargo.contains("# aio-plugin-sdk = { path = "), "{cargo}");
        assert!(cargo.contains(r#"crate-type = ["cdylib"]"#));
        let manifest = aio_plugin_host::Manifest::parse(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!((manifest.id.as_str(), manifest.name.as_str(), manifest.api.as_str()), ("my-plugin", "My Plugin", "1.0"));
        assert_eq!(manifest.permissions, [aio_plugin_host::Permission::Files]);
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(json["dev_wasm"], "target/wasm32-wasip2/release/my_plugin.wasm");
        for file in ["src/lib.rs", ".gitignore", "README.md"] {
            assert!(dir.join(file).is_file(), "{file}");
        }
        assert_eq!(crate_name("9lives"), "plugin-9lives");
    }
}
