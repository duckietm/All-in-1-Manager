//! `check`: reads a package (or packs a built plugin folder in memory) the way the Manager does, and prints what the
//! Manager will show: id, name, version, author, api, every permission in plain words, size and sha256.

use crate::build::{self, Options};
use crate::pack;
use aio_plugin_host::package::{inspect, Package};
use aio_plugin_host::display_safe;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;

/// The first bytes of a WebAssembly component (a core module from wasm32-wasip1 has version 1 instead).
const COMPONENT_HEADER: [u8; 8] = *b"\0asm\x0d\x00\x01\x00";

/// The package bytes of `path`: the file itself, or for a plugin folder its built wasm packed in memory.
fn package_bytes(path: &Path) -> Result<Vec<u8>> {
    if path.is_dir() {
        build::require_plugin_dir(path)?;
        let wasm = build::wasm_path(path, &Options::default())?;
        if !wasm.is_file() {
            bail!("{} is not built yet: run `aio-plugin build` in that folder first", wasm.display());
        }
        pack::write_package(&pack::package_files(path, &wasm)?)
    } else {
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))
    }
}

/// What `check` prints for a valid package.
pub fn report(bytes: &[u8], pkg: &Package) -> String {
    let m = &pkg.manifest;
    let safe = |s: &str| display_safe(s, 200);
    let mut out = String::new();
    let _ = writeln!(out, "id:          {}", m.id);
    let _ = writeln!(out, "name:        {}", safe(&m.name));
    let _ = writeln!(out, "version:     {}", m.version);
    let _ = writeln!(out, "author:      {}", safe(&m.author));
    let _ = writeln!(out, "api:         {}", m.api);
    if m.permissions.is_empty() {
        let _ = writeln!(out, "permissions: none");
    } else {
        let _ = writeln!(out, "permissions:");
        for p in &m.permissions {
            let _ = writeln!(out, "  - {p}: {}", p.plain_words().english_text());
        }
    }
    let _ = writeln!(out, "size:        {} bytes", bytes.len());
    let _ = writeln!(out, "sha256:      {:x}", Sha256::digest(bytes));
    out
}

/// Checks `bytes` as the Manager does on install, plus that the wasm is a component.
pub fn check_bytes(bytes: &[u8]) -> Result<String> {
    let pkg = inspect(bytes, &[])?;
    if !pkg.wasm.starts_with(&COMPONENT_HEADER) {
        bail!("plugin.wasm is not a WebAssembly component (build it for {}, crate-type cdylib)", build::TARGET);
    }
    Ok(report(bytes, &pkg))
}

pub fn check(path: &Path) -> Result<()> {
    let bytes = package_bytes(path)?;
    let text = check_bytes(&bytes).with_context(|| format!("{} is not a valid plugin package", path.display()))?;
    print!("{text}");
    println!("OK: the package is valid.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn package(permissions: &[&str], wasm: &[u8]) -> Vec<u8> {
        let manifest = serde_json::json!({
            "id": "demo", "name": "Demo", "version": "1.2.3", "author": "Someone", "description": "A demo",
            "api": "1.0", "permissions": permissions, "page_title": "Demo",
        });
        let files = BTreeMap::from([("manifest.json".to_string(), manifest.to_string().into_bytes()), ("plugin.wasm".to_string(), wasm.to_vec())]);
        pack::write_package(&files).unwrap()
    }

    #[test]
    fn check_reports_permissions_and_hash() {
        let component = [COMPONENT_HEADER.as_slice(), &[0; 16]].concat();
        let bytes = package(&["sql:read:rooms,items", "http:example.com", "sql:read-private:users.mail", "files"], &component);
        let text = check_bytes(&bytes).unwrap();
        for line in [
            "id:          demo",
            "name:        Demo",
            "version:     1.2.3",
            "author:      Someone",
            "api:         1.0",
            "  - sql:read:rooms,items: Read the tables rooms, items of the hotel database",
            "  - http:example.com: Fetch pages from example.com on the internet",
            "  - sql:read-private:users.mail: Read the private column mail of the table users (it can hold passwords, addresses or tokens)",
            "  - files: Keep its own files in a private folder (always allowed)",
        ] {
            assert!(text.lines().any(|l| l == line), "missing {line:?} in\n{text}");
        }
        assert!(text.contains(&format!("size:        {} bytes", bytes.len())), "{text}");
        assert!(text.contains(&format!("sha256:      {:x}", Sha256::digest(&bytes))), "{text}");

        // Problems are errors: a core module instead of a component, a bad permission, not a zip.
        assert!(check_bytes(&package(&["files"], b"\0asm\x01\0\0\0")).unwrap_err().to_string().contains("not a WebAssembly component"));
        assert!(check_bytes(&package(&["root"], &component)).is_err());
        assert!(check_bytes(b"not a zip").is_err());
    }
}
