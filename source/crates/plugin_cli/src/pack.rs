//! `pack`: the deterministic `.aiop` zip, byte for byte what source/tools/pack_plugin.py writes with Python's
//! `zipfile` (the same files always give the same bytes).
//!
//! The zip is written here by hand, field by field as `zipfile` writes it, rather than with the `zip` crate, whose
//! headers differ: entries in a fixed order (manifest.json, plugin.wasm, icon.png, then i18n/*.json by name),
//! time 1980-01-01 00:00:00, made on Unix (3) with mode 0o100644, version 2.0, deflate with zlib level 9 (raw,
//! window 15, memory level 8), no extra fields, no data descriptors, no comments.

use crate::build::{self, Options};
use anyhow::{bail, Context, Result};
use flate2::write::DeflateEncoder;
use flate2::{Compression, Crc};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// zip "version made by" / "needed to extract" 2.0 (deflate), as `zipfile`'s DEFAULT_VERSION.
const VERSION: u8 = 20;
/// "made by" Unix, so the mode below means the same everywhere.
const UNIX: u8 = 3;
/// Regular file, mode 0644, in the high 16 bits.
const EXTERNAL_ATTR: u32 = 0o100644 << 16;
const DEFLATED: u16 = 8;
/// MS-DOS date of 1980-01-01; the time 00:00:00 is 0.
const DOS_DATE: u16 = 1 << 5 | 1;
const DOS_TIME: u16 = 0;
/// General purpose flag: the name is UTF-8 (set only for a non-ASCII name, as `zipfile` does).
const UTF8_NAME: u16 = 0x800;

/// manifest.json, plugin.wasm, icon.png, then the others (i18n/*.json) by name.
fn entry_order(name: &str) -> (u8, &str) {
    let rank = match name {
        "manifest.json" => 0,
        "plugin.wasm" => 1,
        "icon.png" => 2,
        _ => 3,
    };
    (rank, name)
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(9));
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn u16_of(n: usize, what: &str) -> Result<u16> {
    u16::try_from(n).ok().with_context(|| format!("{what} is too large for a zip"))
}

fn u32_of(n: usize, what: &str) -> Result<u32> {
    // `zipfile` switches to zip64 past 2 GiB; a package is far smaller (the Manager takes 32 MiB at most).
    u32::try_from(n).ok().filter(|n| *n < 0x7FFF_FFFF).with_context(|| format!("{what} is too large for a package"))
}

/// The `.aiop` bytes: `files` maps each entry name to its content.
pub fn write_package(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut names: Vec<&String> = files.keys().collect();
    names.sort_by(|a, b| entry_order(a).cmp(&entry_order(b)));
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for name in &names {
        let data = &files[*name];
        let compressed = deflate(data)?;
        let mut crc = Crc::new();
        crc.update(data);
        let flags = if name.is_ascii() { 0 } else { UTF8_NAME };
        let (name_len, size, csize) = (u16_of(name.len(), name)?, u32_of(data.len(), name)?, u32_of(compressed.len(), name)?);
        let offset = u32_of(out.len(), "the package")?;
        // Fields shared by the local header and the central directory: from the flags to the name length.
        let mut common = Vec::new();
        common.extend(flags.to_le_bytes());
        common.extend(DEFLATED.to_le_bytes());
        common.extend(DOS_TIME.to_le_bytes());
        common.extend(DOS_DATE.to_le_bytes());
        common.extend(crc.sum().to_le_bytes());
        common.extend(csize.to_le_bytes());
        common.extend(size.to_le_bytes());
        common.extend(name_len.to_le_bytes());
        common.extend(0u16.to_le_bytes()); // extra field length

        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend([VERSION, 0]); // needed to extract; `zipfile`'s "reserved" byte
        out.extend(&common);
        out.extend(name.as_bytes());
        out.extend(&compressed);

        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend([VERSION, UNIX, VERSION, 0]); // made by (version, system), needed to extract, reserved
        central.extend(&common);
        central.extend(0u16.to_le_bytes()); // comment length
        central.extend(0u16.to_le_bytes()); // disk number
        central.extend(0u16.to_le_bytes()); // internal attributes
        central.extend(EXTERNAL_ATTR.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let count = u16_of(names.len(), "the number of files")?;
    let (central_offset, central_size) = (u32_of(out.len(), "the package")?, u32_of(central.len(), "the package")?);
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // this disk
    out.extend(0u16.to_le_bytes()); // disk of the central directory
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(central_size.to_le_bytes());
    out.extend(central_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // comment length
    Ok(out)
}

/// The entries of the package of the plugin in `dir`, with `wasm` as plugin.wasm: manifest.json, and icon.png and
/// i18n/*.json when present (as pack_plugin.py).
pub fn package_files(dir: &Path, wasm: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let read = |path: &Path| std::fs::read(path).with_context(|| format!("cannot read {}", path.display()));
    let mut files = BTreeMap::new();
    files.insert("manifest.json".to_string(), read(&dir.join("manifest.json"))?);
    files.insert("plugin.wasm".to_string(), read(wasm)?);
    let icon = dir.join("icon.png");
    if icon.is_file() {
        files.insert("icon.png".to_string(), read(&icon)?);
    }
    let i18n = dir.join("i18n");
    if i18n.is_dir() {
        for entry in std::fs::read_dir(&i18n).with_context(|| format!("cannot read {}", i18n.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { bail!("{} has a file whose name is not UTF-8", i18n.display()) };
            if name.ends_with(".json") && entry.path().is_file() {
                files.insert(format!("i18n/{name}"), read(&entry.path())?);
            }
        }
    }
    Ok(files)
}

/// The plugin's id from manifest.json (the default package name).
fn plugin_id(dir: &Path) -> Result<String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?).context("manifest.json is not valid JSON")?;
    match manifest["id"].as_str() {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => bail!("manifest.json has no id"),
    }
}

/// `pack`: builds the plugin in `dir`, then writes `out` (default `dist/<id>.aiop` in the current folder).
/// Returns the absolute path of the package.
pub fn pack(dir: &Path, options: &Options, out: Option<PathBuf>) -> Result<PathBuf> {
    build::require_plugin_dir(dir)?;
    let id = plugin_id(dir)?;
    let wasm = build::build(dir, options)?;
    let bytes = write_package(&package_files(dir, &wasm)?)?;
    let out = out.unwrap_or_else(|| Path::new("dist").join(format!("{id}.aiop")));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&out, bytes).with_context(|| format!("cannot write {}", out.display()))?;
    Ok(std::path::absolute(&out).unwrap_or(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_in_fixed_order_and_readable() {
        let mut files = BTreeMap::new();
        for name in ["i18n/it.json", "icon.png", "plugin.wasm", "i18n/de.json", "manifest.json"] {
            files.insert(name.to_string(), name.repeat(50).into_bytes());
        }
        let bytes = write_package(&files).unwrap();
        assert_eq!(bytes, write_package(&files).unwrap(), "same files, same bytes");
        let mut zip = Vec::new();
        let mut i = 0;
        // Walk the local headers: the names come in the fixed order.
        while bytes[i..].starts_with(&0x0403_4b50u32.to_le_bytes()) {
            let field = |at: usize| u16::from_le_bytes([bytes[i + at], bytes[i + at + 1]]) as usize;
            let csize = u32::from_le_bytes(bytes[i + 18..i + 22].try_into().unwrap()) as usize;
            let name_len = field(26);
            zip.push(String::from_utf8(bytes[i + 30..i + 30 + name_len].to_vec()).unwrap());
            i += 30 + name_len + csize;
        }
        assert_eq!(zip, ["manifest.json", "plugin.wasm", "icon.png", "i18n/de.json", "i18n/it.json"]);
    }
}
