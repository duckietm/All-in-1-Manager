//! Habbo's .hab bundle and the Electron .asar archive of the Habbo Classic app, read far enough to take the effect
//! icons out of habbo-inventory-com.hab, and the .hab writer every conversion ends with (All-in-1 Converter:
//! NitroCompiler/HabBundle.cs, Download Classes/EffectIcons.cs).
//!
//! A .hab is "HAB\0", u16 version, u16 flags, u32 index length, u32 raw index length, u32 data length (little-endian),
//! a zlib JSON index { format: "hab", entries: [{ name, offset, storedLength, originalLength, compression }] } and the
//! data section the offsets count from.

use std::collections::BTreeMap;
use std::io::Read;

use serde::Deserialize;

use anyhow::Result;

const HEADER: usize = 20;
pub const INVENTORY_BUNDLE: &str = "habbo-inventory-com.hab";

pub fn is_hab(data: &[u8]) -> bool {
    data.len() >= HEADER && data.starts_with(b"HAB\0")
}

pub fn is_png(data: &[u8]) -> bool {
    data.len() > 8 && data.starts_with(&[0x89, b'P', b'N', b'G'])
}

fn bad(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn u32_at(data: &[u8], at: usize) -> usize {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize
}

/// Habbo stores zlib streams; a raw deflate stream is accepted too.
fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).is_ok() {
        return Ok(out);
    }
    out.clear();
    flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).map_err(|e| bad(format!("A .hab entry cannot be unpacked: {e}")))?;
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format: String,
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    name: String,
    offset: i64,
    stored_length: i64,
    original_length: Option<i64>,
    compression: Option<String>,
}

/// Every entry of a .hab as (name, bytes). All lengths are checked: a damaged file is an error, not half a bundle.
pub fn read(buffer: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if !is_hab(buffer) {
        return Err(bad("Not a .hab bundle (no HAB header)."));
    }
    let index_length = u32_at(buffer, 8);
    let raw_index_length = u32_at(buffer, 12);
    let data_length = u32_at(buffer, 16);
    let data_start = HEADER + index_length;
    if data_start + data_length > buffer.len() {
        return Err(bad("The .hab bundle is shorter than its header says."));
    }
    let raw_index = inflate(&buffer[HEADER..data_start])?;
    if raw_index_length != 0 && raw_index.len() != raw_index_length {
        return Err(bad("The .hab index has the wrong length."));
    }
    let index: Index = serde_json::from_slice(&raw_index).map_err(|e| bad(format!("The .hab index is not JSON: {e}")))?;
    if index.format != "hab" {
        return Err(bad("The .hab index is not a \"hab\" index."));
    }
    let mut files = Vec::with_capacity(index.entries.len());
    for entry in index.entries {
        if entry.name.is_empty() || entry.offset < 0 || entry.stored_length < 0 || (entry.offset + entry.stored_length) as usize > data_length {
            return Err(bad(format!("The .hab entry \"{}\" lies outside the data section.", entry.name)));
        }
        let start = data_start + entry.offset as usize;
        let stored = &buffer[start..start + entry.stored_length as usize];
        let bytes = if entry.compression.as_deref() == Some("deflate") { inflate(stored)? } else { stored.to_vec() };
        if entry.original_length.is_some_and(|length| length as usize != bytes.len()) {
            return Err(bad(format!("The .hab entry \"{}\" has the wrong length.", entry.name)));
        }
        files.push((entry.name, bytes));
    }
    Ok(files)
}

/// The mime type Habbo's index gives a file (HabBundle.MimeTypeOf).
pub fn mime_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "json" => "application/json",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "xml" => "application/xml",
        _ => "text/plain",
    }
}

fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).expect("writing to memory");
    encoder.finish().expect("writing to memory")
}

/// Packs the files (name, bytes) into one .hab named `name`. Like Habbo's own bundles, an entry is zlib-compressed
/// only when that makes it smaller; an already compressed image is stored as it is ("none"). (HabBundle.Write)
pub fn write(name: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut entries = Vec::with_capacity(files.len());
    for (file, bytes) in files {
        let deflated = zlib(bytes);
        let compress = deflated.len() < bytes.len();
        let stored = if compress { deflated } else { bytes.clone() };
        entries.push(serde_json::json!({
            "name": file,
            "mimeType": mime_type(file),
            "offset": data.len(),
            "storedLength": stored.len(),
            "originalLength": bytes.len(),
            "compression": if compress { "deflate" } else { "none" },
        }));
        data.extend_from_slice(&stored);
    }
    let raw = serde_json::to_vec(&serde_json::json!({ "format": "hab", "version": 1, "name": name, "entries": entries })).expect("json");
    let index = zlib(&raw);
    let mut out = Vec::with_capacity(HEADER + index.len() + data.len());
    out.extend_from_slice(b"HAB\0");
    out.extend_from_slice(&1u16.to_le_bytes());
    // Habbo's own furni bundles carry flags 1.
    out.extend_from_slice(&1u16.to_le_bytes());
    for length in [index.len(), raw.len(), data.len()] {
        out.extend_from_slice(&(length as u32).to_le_bytes());
    }
    out.extend(index);
    out.extend(data);
    out
}

/// The inventory bundle from an Electron .asar: u32 4, u32 header size, the header pickle (u32 payload size, u32 json
/// length, json) and the file data after the header. It is looked up by name (client/generated/ in the app).
pub fn from_asar(asar: &[u8]) -> Result<Option<Vec<u8>>> {
    if asar.len() < 16 {
        return Err(bad("Not an .asar archive."));
    }
    let header_size = u32_at(asar, 4);
    let json_length = u32_at(asar, 12);
    if 16 + json_length > asar.len() || 8 + header_size > asar.len() {
        return Err(bad("The .asar header is damaged."));
    }
    let header: serde_json::Value =
        serde_json::from_slice(&asar[16..16 + json_length]).map_err(|e| bad(format!("The .asar header is not JSON: {e}")))?;
    fn find<'a>(directory: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        for (name, child) in directory.get("files")?.as_object()? {
            if child.get("files").is_some() {
                if let Some(found) = find(child) {
                    return Some(found);
                }
            } else if name.eq_ignore_ascii_case(INVENTORY_BUNDLE) {
                return Some(child);
            }
        }
        None
    }
    let Some(node) = find(&header) else { return Ok(None) };
    if node.get("unpacked").and_then(|u| u.as_bool()) == Some(true) {
        return Ok(None);
    }
    let offset: usize = node.get("offset").and_then(|o| o.as_str()).and_then(|o| o.parse().ok()).unwrap_or(0);
    let size = node.get("size").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
    let start = 8 + header_size + offset;
    if start + size > asar.len() {
        return Err(bad("The .asar entry lies outside the archive."));
    }
    Ok(Some(asar[start..start + size].to_vec()))
}

/// The inventory bundle from HabboClassicWin.zip: resources/app.asar.unpacked/.../habbo-inventory-com.hab, or the
/// bundle inside resources/app.asar.
pub fn from_zip(zip: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| bad(format!("Not a zip file: {e}")))?;
    let read = |archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str| -> Result<Vec<u8>> {
        let mut entry = archive.by_name(name).map_err(|e| bad(format!("{name}: {e}")))?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).map_err(|e| bad(format!("{name}: {e}")))?;
        Ok(bytes)
    };
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();
    if let Some(unpacked) = names.iter().find(|n| {
        n.to_ascii_lowercase().starts_with("resources/app.asar.unpacked/") && n.rsplit('/').next().is_some_and(|f| f.eq_ignore_ascii_case(INVENTORY_BUNDLE))
    }) {
        return read(&mut archive, unpacked).map(Some);
    }
    match names.iter().find(|n| n.eq_ignore_ascii_case("resources/app.asar")) {
        Some(asar) => from_asar(&read(&mut archive, asar)?),
        None => Ok(None),
    }
}

/// The type of an effect icon's name: fx_icon_12_png in the bundle, fx_icon_12_png.png or fx_icon_12.png on disk.
pub fn icon_type(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("fx_icon_")?;
    let rest = rest.strip_suffix(".png").unwrap_or(rest);
    let rest = rest.strip_suffix("_png").unwrap_or(rest);
    rest.parse().ok().filter(|t| *t > 0)
}

/// The effect icons (type -> PNG) of one file: HabboClassicWin.zip, its app.asar, habbo-inventory-com.hab, or a PNG.
pub fn icons_from(name: &str, data: &[u8]) -> Result<BTreeMap<u32, Vec<u8>>> {
    let mut icons = BTreeMap::new();
    if is_png(data) {
        if let Some(t) = icon_type(name) {
            icons.insert(t, data.to_vec());
        }
        return Ok(icons);
    }
    let bundle = if data.starts_with(b"PK\x03\x04") {
        from_zip(data)?
    } else if is_hab(data) {
        Some(data.to_vec())
    } else if data.len() >= 4 && u32_at(data, 0) == 4 {
        from_asar(data)?
    } else {
        return Err(bad(format!("{name} is not a zip, app.asar, .hab or PNG file.")));
    };
    let Some(bundle) = bundle else { return Err(bad(format!("{INVENTORY_BUNDLE} is not in {name}."))) };
    for (entry, bytes) in read(&bundle)? {
        if let Some(t) = icon_type(&entry).filter(|_| is_png(&bytes)) {
            icons.entry(t).or_insert(bytes);
        }
    }
    Ok(icons)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0];

    /// A .hab like Habbo's (the test's own copy of the format, to check [`write`] against).
    pub fn hab(files: &[(&str, &[u8])]) -> Vec<u8> {
        let zlib = |bytes: &[u8]| {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(bytes).unwrap();
            e.finish().unwrap()
        };
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (name, bytes) in files {
            let deflated = zlib(bytes);
            let (stored, compression) = if deflated.len() < bytes.len() { (deflated, "deflate") } else { (bytes.to_vec(), "none") };
            entries.push(serde_json::json!({"name": name, "offset": data.len(), "storedLength": stored.len(), "originalLength": bytes.len(), "compression": compression}));
            data.extend_from_slice(&stored);
        }
        let raw = serde_json::to_vec(&serde_json::json!({"format": "hab", "version": 1, "name": "x", "entries": entries})).unwrap();
        let index = zlib(&raw);
        let mut out = b"HAB\0".to_vec();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        for n in [index.len(), raw.len(), data.len()] {
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        out.extend(index);
        out.extend(data);
        out
    }

    fn asar(name: &str, content: &[u8]) -> Vec<u8> {
        let json = serde_json::to_vec(&serde_json::json!({"files": {"client": {"files": {"generated": {"files": {name: {"offset": "0", "size": content.len()}}}}}}})).unwrap();
        let mut pickle = Vec::new();
        pickle.extend_from_slice(&((json.len() + 4) as u32).to_le_bytes());
        pickle.extend_from_slice(&(json.len() as u32).to_le_bytes());
        pickle.extend(json);
        let mut out = 4u32.to_le_bytes().to_vec();
        out.extend_from_slice(&(pickle.len() as u32).to_le_bytes());
        out.extend(pickle);
        out.extend_from_slice(content);
        out
    }

    #[test]
    fn icons_from_the_bundle_the_asar_and_the_zip() {
        let bundle = hab(&[("fx_icon_12_png", PNG), ("fx_icon_3_png", PNG), ("other.json", b"{}"), ("fx_icon_x_png", PNG)]);
        assert_eq!(read(&bundle).unwrap().len(), 4);
        let from_bundle = icons_from(INVENTORY_BUNDLE, &bundle).unwrap();
        assert_eq!(from_bundle.keys().copied().collect::<Vec<_>>(), [3, 12]);

        let archive = asar(INVENTORY_BUNDLE, &bundle);
        assert_eq!(icons_from("app.asar", &archive).unwrap().len(), 2);

        let mut zip_bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            zip.start_file("resources/app.asar", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(&archive).unwrap();
            zip.finish().unwrap();
        }
        assert_eq!(icons_from("HabboClassicWin.zip", &zip_bytes).unwrap().len(), 2);

        assert_eq!(icons_from("fx_icon_7.png", PNG).unwrap().keys().copied().collect::<Vec<_>>(), [7]);
        assert!(icons_from("page.html", b"<html>").is_err());
        assert!(icons_from("app.asar", &asar("other.hab", &bundle)).is_err(), "no inventory bundle in it");
    }

    #[test]
    fn written_bundles_read_back_like_habbos() {
        let files = vec![("chair.json".to_string(), b"{\"name\":\"chair\"}".repeat(20)), ("chair.png".to_string(), PNG.to_vec())];
        let written = write("chair", &files);
        let expected = hab(&[("chair.json", &files[0].1), ("chair.png", PNG)]);
        assert_eq!(read(&written).unwrap(), read(&expected).unwrap());
        assert_eq!(&written[..4], b"HAB\0");
        assert_eq!(mime_type("x.WEBP"), "image/webp");
    }

    #[test]
    fn a_damaged_bundle_is_refused() {
        let mut bundle = hab(&[("fx_icon_1_png", PNG)]);
        bundle.truncate(bundle.len() - 3);
        assert!(read(&bundle).is_err());
        assert_eq!(icon_type("FX_ICON_44_png.png"), Some(44));
        assert_eq!(icon_type("fx_icon_0.png"), None);
    }
}
