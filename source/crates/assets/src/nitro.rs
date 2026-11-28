//! Nitro's .nitro bundle: u16 file count, then per file a u16 name length, the name, a u32 data length and the data
//! (all big-endian), each file a gzip stream (our converter) or a zlib stream (the original Nitro tools).
//! (All-in-1 Converter: NitroCompiler/NitroToHabConverter.ReadNitro, NitroBundler.cs)
//!
//! A furni's first file is `<name>.json` (gzip in Nitro's own furniture bundles): the same json a Habbo .hab holds
//! (name, logicType, visualizationType, logic.model.dimensions, assets, visualizations, spritesheet), so the furni
//! import reads both the same way. [`read_json`] unpacks only that file.

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Result};

/// The most a .nitro file may unpack to: a bundle that says more is refused instead of filling the memory.
pub const MAX_ENTRY: usize = 64 * 1024 * 1024;
/// The most all the files of one .nitro may unpack to together.
pub const MAX_TOTAL: usize = 2 * MAX_ENTRY;

fn take<'a>(data: &'a [u8], at: &mut usize, length: usize) -> Result<&'a [u8]> {
    if *at + length > data.len() {
        bail!("The .nitro bundle is cut short.");
    }
    let slice = &data[*at..*at + length];
    *at += length;
    Ok(slice)
}

fn is_zlib(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] & 0x0F == 8 && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0
}

/// A file some tools store as it is: an image (PNG, WebP, GIF, JPEG) or JSON (after a BOM and white space).
fn is_stored(data: &[u8]) -> bool {
    let text = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let text = &text[text.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(text.len())..];
    data.starts_with(&[0x89, b'P', b'N', b'G'])
        || data.starts_with(b"RIFF")
        || data.starts_with(b"GIF8")
        || data.starts_with(&[0xFF, 0xD8, 0xFF])
        || text.starts_with(b"{")
        || text.starts_with(b"[")
}

/// An entry unpacked, at most `limit` bytes (else an error); one that is not compressed is kept as it is.
fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let too_big = || anyhow!("A .nitro bundle unpacks to more than {} MiB.", limit / (1024 * 1024));
    if !data.starts_with(&[0x1F, 0x8B]) && !is_zlib(data) && is_stored(data) {
        return if data.len() > limit { Err(too_big()) } else { Ok(data.to_vec()) };
    }
    let take = limit as u64 + 1;
    let mut out = Vec::new();
    let result = if data.starts_with(&[0x1F, 0x8B]) {
        flate2::read::GzDecoder::new(data).take(take).read_to_end(&mut out)
    } else if is_zlib(data) {
        flate2::read::ZlibDecoder::new(data).take(take).read_to_end(&mut out)
    } else {
        flate2::read::DeflateDecoder::new(data).take(take).read_to_end(&mut out)
    };
    result.map_err(|e| anyhow!("A .nitro entry cannot be unpacked: {e}"))?;
    if out.len() > limit {
        return Err(too_big());
    }
    Ok(out)
}

/// The files of a .nitro as (name, stored bytes), not unpacked. Every count and length is checked against the data.
fn entries(data: &[u8]) -> Result<Vec<(String, &[u8])>> {
    let mut at = 0;
    let count = u16::from_be_bytes(take(data, &mut at, 2)?.try_into()?) as usize;
    if count == 0 {
        bail!("The .nitro bundle is empty.");
    }
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let length = u16::from_be_bytes(take(data, &mut at, 2)?.try_into()?) as usize;
        let name = String::from_utf8(take(data, &mut at, length)?.to_vec()).map_err(|_| anyhow!("A .nitro file name is not UTF-8."))?;
        let length = i32::from_be_bytes(take(data, &mut at, 4)?.try_into()?);
        if length < 0 {
            bail!("Invalid length for {name} in the .nitro bundle.");
        }
        files.push((name, take(data, &mut at, length as usize)?));
    }
    Ok(files)
}

/// Every file of a .nitro as (name, bytes). A damaged bundle is an error instead of a bundle with files missing.
/// Each file may unpack to [`MAX_ENTRY`], all of them together to [`MAX_TOTAL`].
pub fn read(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    read_within(data, MAX_ENTRY, MAX_TOTAL)
}

fn read_within(data: &[u8], per_entry: usize, total: usize) -> Result<Vec<(String, Vec<u8>)>> {
    let mut left = total;
    let mut files = Vec::new();
    for (name, stored) in entries(data)? {
        let bytes = decompress(stored, per_entry.min(left))?;
        left -= bytes.len();
        files.push((name, bytes));
    }
    Ok(files)
}

/// The json of a furni (or any) .nitro: its first `.json` file, the only one unpacked. The whole bundle is still
/// checked, so a damaged file is an error. Gives the same json as the .hab of the furni.
pub fn read_json(data: &[u8]) -> Result<serde_json::Value> {
    let files = entries(data)?;
    let (name, stored) = files.iter().find(|(name, _)| name.to_ascii_lowercase().ends_with(".json")).ok_or_else(|| anyhow!("The .nitro bundle has no .json file."))?;
    let bytes = decompress(stored, MAX_ENTRY)?;
    let mut json: serde_json::Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).map_err(|e| anyhow!("{name} in the .nitro bundle is not JSON: {e}"))?;
    lift_model(&mut json);
    Ok(json)
}

/// Older converters write `dimensions` and `directions` at the top of the json instead of in logic.model (about one
/// furni in twenty of a big retro): they are moved there, as a .hab has them. Into an existing logic.model only
/// what it lacks; a `logic` or `model` that is not an object is left alone, and so are the top-level values.
fn lift_model(json: &mut serde_json::Value) {
    use serde_json::{Map, Value};
    let Some(object) = json.as_object_mut() else { return };
    let lifted: Vec<&str> = ["dimensions", "directions"].into_iter().filter(|key| object.contains_key(*key)).collect();
    if !object.contains_key("dimensions") {
        return;
    }
    let logic = object.entry("logic").or_insert_with(|| Value::Object(Map::new()));
    let Some(logic) = logic.as_object_mut() else { return };
    let model = logic.entry("model").or_insert_with(|| Value::Object(Map::new()));
    let Some(model) = model.as_object_mut() else { return };
    let missing: Vec<&str> = lifted.into_iter().filter(|key| !model.contains_key(*key)).collect();
    for key in missing {
        if let Some(value) = object.remove(key) {
            // `object` and `model` are both borrowed from json: look the model up again.
            if let Some(model) = object.get_mut("logic").and_then(|l| l.get_mut("model")).and_then(Value::as_object_mut) {
                model.insert(key.to_string(), value);
            }
        }
    }
}

/// Packs files into a .nitro, each as a gzip stream like the converter writes them.
pub fn write(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = (files.len() as u16).to_be_bytes().to_vec();
    for (name, data) in files {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gzip.write_all(data).expect("writing to memory");
        let compressed = gzip.finish().expect("writing to memory");
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(compressed.len() as i32).to_be_bytes());
        out.extend(compressed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read_back() {
        let files = vec![("chair.json".to_string(), b"{}".to_vec()), ("chair.png".to_string(), vec![1, 2, 3])];
        assert_eq!(read(&write(&files)).unwrap(), files);
        assert!(read(&write(&files)[..10]).is_err(), "cut short");
        assert!(read(&[0, 0]).is_err(), "empty");
    }

    #[test]
    fn uncompressed_entries_are_kept() {
        let png = [0x89, b'P', b'N', b'G', 13, 10, 26, 10, 1];
        let mut bundle = 2u16.to_be_bytes().to_vec();
        for (name, data) in [("a.png", &png[..]), ("a.json", &b"{\"x\":1}"[..])] {
            bundle.extend_from_slice(&(name.len() as u16).to_be_bytes());
            bundle.extend_from_slice(name.as_bytes());
            bundle.extend_from_slice(&(data.len() as i32).to_be_bytes());
            bundle.extend_from_slice(data);
        }
        let files = read(&bundle).unwrap();
        assert_eq!(files[0].1, png);
        assert_eq!(files[1].1, b"{\"x\":1}");
    }

    /// Black_sapphire_dice.nitro of a retro's bundled/furniture: gzip json, webp spritesheet.
    const DICE: &[u8] = include_bytes!("../tests/fixtures/black_sapphire_dice.nitro");

    #[test]
    fn a_real_furni_bundle() {
        let files = read(DICE).unwrap();
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["Black_sapphire_dice.json", "Black_sapphire_dice.webp"]);
        assert!(files[1].1.starts_with(b"RIFF"), "a webp spritesheet, stored as it is");
        let json = read_json(DICE).unwrap();
        assert_eq!(json["name"], "Black_sapphire_dice");
        assert_eq!(json["logicType"], "furniture_dice");
        assert_eq!(json["logic"]["model"]["dimensions"], serde_json::json!({"x": 1, "y": 1, "z": 1}));
        assert!(json["visualizations"].as_array().unwrap().iter().any(|v| v["size"] == 64 && v["animations"].get("6").is_some()));
        // Damaged: cut anywhere, a count larger than the files, no .json.
        for cut in [1, 3, 10, DICE.len() / 2, DICE.len() - 1] {
            assert!(read_json(&DICE[..cut]).is_err(), "cut at {cut}");
        }
        let mut more = DICE.to_vec();
        more[1] += 1;
        assert!(read_json(&more).is_err(), "one file more than there are");
        assert!(read_json(&write(&[("a.png".to_string(), vec![1])])).is_err(), "no json");
    }

    #[test]
    fn the_model_of_older_converters_is_moved_into_logic() {
        let old = br#"{"name":"x","logicType":"furniture_multistate","dimensions":{"x":2,"y":1,"z":0.5},"directions":[0]}"#;
        let json = read_json(&write(&[("x.json".to_string(), old.to_vec())])).unwrap();
        assert_eq!(json["logic"]["model"]["dimensions"], serde_json::json!({"x": 2, "y": 1, "z": 0.5}));
        assert_eq!(json["logic"]["model"]["directions"], serde_json::json!([0]));
        assert!(json.get("dimensions").is_none());
        // One that has logic.model keeps it.
        let new = br#"{"logic":{"model":{"dimensions":{"x":3}}},"dimensions":{"x":9}}"#;
        let json = read_json(&write(&[("y.json".to_string(), new.to_vec())])).unwrap();
        assert_eq!(json["logic"]["model"]["dimensions"]["x"], 3);
        // A logic.model without dimensions gets them, and keeps what it has.
        let partial = br#"{"logic":{"model":{"directions":[90]}},"dimensions":{"x":4},"directions":[0]}"#;
        let json = read_json(&write(&[("z.json".to_string(), partial.to_vec())])).unwrap();
        assert_eq!(json["logic"]["model"], serde_json::json!({"directions": [90], "dimensions": {"x": 4}}));
        assert!(json.get("dimensions").is_none() && json["directions"] == serde_json::json!([0]));
        // A logic that is not an object: nothing is dropped.
        let odd = br#"{"logic":"x","dimensions":{"x":5}}"#;
        let json = read_json(&write(&[("w.json".to_string(), odd.to_vec())])).unwrap();
        assert_eq!((&json["logic"], &json["dimensions"]["x"]), (&serde_json::json!("x"), &serde_json::json!(5)));
    }

    #[test]
    fn all_entries_together_are_capped() {
        let files: Vec<(String, Vec<u8>)> = (0..3).map(|i| (format!("{i}.bin"), vec![b' '; 1000])).collect();
        let bundle = write(&files);
        assert!(read_within(&bundle, 1000, 3000).is_ok());
        assert!(read_within(&bundle, 999, 3000).is_err(), "one entry too big");
        assert!(read_within(&bundle, 1000, 2999).is_err(), "together too big");
    }

    #[test]
    fn stored_files_are_kept_as_they_are() {
        let mut bundle = 4u16.to_be_bytes().to_vec();
        let stored: [(&str, &[u8]); 4] =
            [("a.gif", b"GIF89a..."), ("a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 1]), ("a.json", b"\xEF\xBB\xBF \n {\"x\":1}"), ("b.json", b"\r\n[1]")];
        for (name, data) in stored {
            bundle.extend_from_slice(&(name.len() as u16).to_be_bytes());
            bundle.extend_from_slice(name.as_bytes());
            bundle.extend_from_slice(&(data.len() as i32).to_be_bytes());
            bundle.extend_from_slice(data);
        }
        let files = read(&bundle).unwrap();
        for ((_, data), (_, read)) in stored.iter().zip(&files) {
            assert_eq!(read, data);
        }
        assert_eq!(read_json(&bundle).unwrap(), serde_json::json!({"x": 1}));
    }

    #[test]
    fn an_entry_unpacking_past_the_limit_is_refused() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let zeros = vec![b' '; 1024 * 1024];
        for _ in 0..=MAX_ENTRY / zeros.len() {
            gzip.write_all(&zeros).unwrap();
        }
        let entry = gzip.finish().unwrap();
        let mut bundle = 1u16.to_be_bytes().to_vec();
        bundle.extend_from_slice(&6u16.to_be_bytes());
        bundle.extend_from_slice(b"a.json");
        bundle.extend_from_slice(&(entry.len() as i32).to_be_bytes());
        bundle.extend(entry);
        let error = read_json(&bundle).unwrap_err().to_string();
        assert!(error.contains("more than 64 MiB"), "{error}");
    }

    #[test]
    fn zlib_entries_of_the_original_tools() {
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(b"hello").unwrap();
        let entry = zlib.finish().unwrap();
        let mut bundle = 1u16.to_be_bytes().to_vec();
        bundle.extend_from_slice(&5u16.to_be_bytes());
        bundle.extend_from_slice(b"a.txt");
        bundle.extend_from_slice(&(entry.len() as i32).to_be_bytes());
        bundle.extend(entry);
        assert_eq!(read(&bundle).unwrap(), vec![("a.txt".to_string(), b"hello".to_vec())]);
    }
}
