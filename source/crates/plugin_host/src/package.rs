//! Reading, installing and removing `.aiop` packages. `inspect` writes nothing.

use crate::manifest::{valid_id, Manifest};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_PACKAGE: usize = 32 * 1024 * 1024;
pub const MAX_WASM: u64 = 24 * 1024 * 1024;
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_ICON: u64 = 2 * 1024 * 1024;
const MAX_I18N: u64 = 512 * 1024;
const MAX_ENTRIES: usize = 64;

/// A validated package, held in memory.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub wasm: Vec<u8>,
    pub icon: Option<Vec<u8>>,
    /// language code to file content
    pub i18n: BTreeMap<String, Vec<u8>>,
}

fn read_limited(entry: &mut impl Read, limit: u64, name: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // Read one byte past the limit: the zip header may lie about the size.
    entry.take(limit + 1).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        bail!("{name} is too large");
    }
    Ok(out)
}

fn lang_of(name: &str) -> Option<&str> {
    let lang = name.strip_prefix("i18n/")?.strip_suffix(".json")?;
    let ok = (1..=16).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(lang)
}

/// Reads and validates a package. Writes nothing. Any unsafe or unexpected entry fails the whole package.
pub fn inspect(aiop: &[u8], reserved_ids: &[&str]) -> Result<Package> {
    if aiop.len() > MAX_PACKAGE {
        bail!("The package is larger than 32 MiB");
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(aiop)).context("The package is not a zip")?;
    if archive.len() > MAX_ENTRIES {
        bail!("The package has too many files");
    }
    let (mut manifest, mut wasm, mut icon) = (None, None, None);
    let mut i18n = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() || name.starts_with('/') || name.contains('\\') || name.split('/').any(|p| p == "..") {
            bail!("Unsafe path in the package: {name}");
        }
        if entry.is_dir() || entry.is_symlink() {
            bail!("Unexpected entry in the package: {name}");
        }
        let (limit, slot) = match name.as_str() {
            "manifest.json" => (MAX_MANIFEST, 0),
            "plugin.wasm" => (MAX_WASM, 1),
            "icon.png" => (MAX_ICON, 2),
            n if lang_of(n).is_some() => (MAX_I18N, 3),
            _ => bail!("Unexpected file in the package: {name}"),
        };
        if entry.size() > limit {
            bail!("{name} is too large");
        }
        let data = read_limited(&mut entry, limit, &name)?;
        let duplicate = match slot {
            0 => manifest.replace(data).is_some(),
            1 => wasm.replace(data).is_some(),
            2 => icon.replace(data).is_some(),
            _ => i18n.insert(lang_of(&name).unwrap_or_default().to_string(), data).is_some(),
        };
        if duplicate {
            bail!("Duplicate file in the package: {name}");
        }
    }
    let manifest = Manifest::parse(&manifest.context("manifest.json is missing")?)?;
    let wasm = wasm.context("plugin.wasm is missing")?;
    if wasm.is_empty() {
        bail!("plugin.wasm is empty");
    }
    if reserved_ids.contains(&manifest.id.as_str()) {
        bail!("The id {} belongs to a built-in plugin", manifest.id);
    }
    Ok(Package { manifest, wasm, icon, i18n })
}

fn manifest_json(m: &Manifest) -> Result<String> {
    let mut value = serde_json::json!({
        "id": m.id, "name": m.name, "version": m.version, "author": m.author,
        "description": m.description, "api": m.api,
        "permissions": m.permissions.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        "page_title": m.page_title,
    });
    if let Some(t) = &m.tile {
        value["tile"] = serde_json::json!({ "title": t.title, "description": t.description, "color": t.color });
    }
    Ok(serde_json::to_string_pretty(&value)?)
}

fn write_all(pkg: &Package, dir: &Path) -> Result<()> {
    std::fs::write(dir.join("manifest.json"), manifest_json(&pkg.manifest)?)?;
    std::fs::write(dir.join("plugin.wasm"), &pkg.wasm)?;
    if let Some(icon) = &pkg.icon {
        std::fs::write(dir.join("icon.png"), icon)?;
    }
    if !pkg.i18n.is_empty() {
        std::fs::create_dir_all(dir.join("i18n"))?;
        for (lang, data) in &pkg.i18n {
            std::fs::write(dir.join("i18n").join(format!("{lang}.json")), data)?;
        }
    }
    std::fs::create_dir_all(dir.join("data"))?;
    Ok(())
}

/// Installs into `root/<id>`. Atomic: written to a `.partial` folder (private `data/` moved in, so the new version is
/// complete before anything is swapped), then renamed; the previous version is moved aside and put back, data
/// included, if anything fails. A `.old` left by a crash is recovered, not deleted.
pub fn install(pkg: &Package, root: &Path) -> Result<PathBuf> {
    install_inner(pkg, root, false)
}

/// Test hook: behaves like `install` but fails the final rename, after the old version was moved aside.
#[doc(hidden)]
pub fn install_failing_after_move_aside(pkg: &Package, root: &Path) -> Result<PathBuf> {
    install_inner(pkg, root, true)
}

fn install_inner(pkg: &Package, root: &Path, fail_final_rename: bool) -> Result<PathBuf> {
    let id = &pkg.manifest.id;
    if !valid_id(id) {
        bail!("Invalid plugin id: {id}");
    }
    std::fs::create_dir_all(root)?;
    let into = root.join(id);
    let partial = root.join(format!(".{id}.partial"));
    let old = root.join(format!(".{id}.old"));
    // A crash between "move aside" and "put in place" leaves only `.old`: bring it back, data included.
    if old.exists() && !into.exists() {
        std::fs::rename(&old, &into).context("Could not recover the previous version")?;
    }
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir(&partial).with_context(|| format!("Could not create {}", partial.display()))?;
    if let Err(error) = write_all(pkg, &partial) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(error);
    }
    let had_old = into.exists();
    let data = into.join("data");
    let mut data_moved = false;
    if had_old && data.is_dir() {
        let step = std::fs::remove_dir_all(partial.join("data")).and_then(|_| std::fs::rename(&data, partial.join("data")));
        if let Err(error) = step {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(error).context("Could not carry the plugin data over");
        }
        data_moved = true;
    }
    // Undo: put the data back where the live version expects it and drop the half-built one.
    let give_back = |data_moved: bool| {
        if data_moved {
            let _ = std::fs::rename(partial.join("data"), &data);
        }
        let _ = std::fs::remove_dir_all(&partial);
    };
    let _ = std::fs::remove_dir_all(&old);
    if had_old {
        if let Err(error) = std::fs::rename(&into, &old) {
            give_back(data_moved);
            return Err(error).with_context(|| format!("Could not move {} aside", into.display()));
        }
    }
    let put = if fail_final_rename {
        Err(std::io::Error::other("injected failure"))
    } else {
        std::fs::rename(&partial, &into)
    };
    if let Err(error) = put {
        if had_old {
            if std::fs::rename(&old, &into).is_ok() {
                give_back(data_moved);
            }
            // If even the restore failed, `.old` and `.partial` stay on disk and the next install recovers.
        } else {
            give_back(data_moved);
        }
        return Err(error).with_context(|| format!("Could not put the plugin in {}", into.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(into)
}

/// Removes the plugin folder, its private data included.
pub fn remove(id: &str, root: &Path) -> Result<()> {
    if !valid_id(id) {
        bail!("Invalid plugin id: {id}");
    }
    let dir = root.join(id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("Could not remove {}", dir.display()))?;
    }
    Ok(())
}

/// Manifests of the installed plugins; folders with a broken manifest and `.partial`/`.old` leftovers are skipped.
pub fn installed(root: &Path) -> Vec<Manifest> {
    let Ok(read) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Manifest> = read
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir())
        .filter_map(|e| {
            let manifest = Manifest::parse(&std::fs::read(e.path().join("manifest.json")).ok()?).ok()?;
            (e.file_name().to_string_lossy() == manifest.id.as_str()).then_some(manifest)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}
