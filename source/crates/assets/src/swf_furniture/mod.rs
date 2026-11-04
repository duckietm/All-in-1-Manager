//! A furniture .swf converted to the Nitro furniture bundle: `<name>.json` and the sprite sheet `<name>.png`, the
//! files the C# puts in the .hab (before its WebP step, which we leave out: the sheet stays PNG).
//! (All-in-1 Converter: SWF_Furni_Compiler/SWF_Furni_To_Nitro.cs and its Mapper folder, with the parts of
//! FFDEC/FfdecExtractor.cs and Tools/NativeSwfExtractor.cs that decide which image files exist under which name.)
//!
//! The C# works on an exported folder (binaryData/, images/, symbolClass/symbols.csv), moving images through a
//! tmp/ folder and back. That file juggling decides which images reach the sheet and under which name, so it is
//! replayed here on in-memory folders with Windows' rules: case-insensitive names, a move onto an existing name
//! failing (or replacing, when the C# asks to overwrite), listings in NTFS order.

mod assets;
mod json;
mod logic;
mod names;
mod sheet;
mod visualization;
mod xml;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, bail, Result};
use image::RgbaImage;

use crate::swf::Swf;
use json::{Json, Obj};
use names::{fold, ntfs_order};

/// A folder of files (name with extension -> payload), with Windows' case-insensitive names.
struct Folder<T> {
    files: Vec<(String, T)>,
}

impl<T: Clone> Folder<T> {
    fn new() -> Self {
        Folder { files: Vec::new() }
    }

    fn position(&self, name: &str) -> Option<usize> {
        let key = fold(name);
        self.files.iter().position(|(n, _)| fold(n) == key)
    }

    /// File.WriteAllBytes / File.Move(overwrite: true): an existing file of that name (in any case) is replaced.
    fn write(&mut self, name: String, value: T) {
        match self.position(&name) {
            Some(i) => self.files[i] = (name, value),
            None => self.files.push((name, value)),
        }
    }

    /// File.Move without overwrite: false when the name is taken (or not a valid file name), as the C# then skips.
    fn create(&mut self, name: String, value: T) -> bool {
        if names::invalid_file_name(&name) || self.position(&name).is_some() {
            return false;
        }
        self.files.push((name, value));
        true
    }

    /// Directory.GetFiles: the names in NTFS order.
    fn listed(&self) -> Vec<(String, T)> {
        let mut files = self.files.clone();
        files.sort_by(|a, b| ntfs_order(&a.0, &b.0));
        files
    }
}

/// `Path.GetFileNameWithoutExtension` of a "<stem>.png" / "<stem>.bin" name.
fn stem(name: &str) -> &str {
    name.rfind('.').map_or(name, |i| &name[..i])
}

/// The images/ folder the C# loads the sheet from: (file stem, bitmap index) in listing order, "sh_" images left
/// out (SWF_Furni_To_Nitro.LoadImages), after the native export, FfdecExtractor.RebuildImagesFromCsvAsync and
/// ImageRestorer.RestoreImagesFromTmpAsync.
fn image_files(swf: &Swf, lines: &[String], image_mapping: &HashMap<String, String>) -> Result<Vec<(String, usize)>> {
    // NativeSwfExtractor: "<id>_<name>.png" for a bitmap with exactly one symbol name, else "<id>.png".
    let mut exported = Folder::new();
    for (i, bitmap) in swf.bitmaps.iter().enumerate() {
        exported.write(format!("{}.png", bitmap.export_name()), i);
    }
    // RebuildImagesFromCsvAsync: everything moves to tmp/, looked up by stem (quotes removed; ToDictionary throws on
    // a repeated key, which fails the conversion).
    let mut tmp: Vec<Option<(String, usize)>> = exported.files.into_iter().map(Some).collect();
    let mut lookup: HashMap<String, usize> = HashMap::new();
    for (at, file) in tmp.iter().enumerate() {
        let (name, _) = file.as_ref().expect("just filled");
        if lookup.insert(stem(name).replace('"', ""), at).is_some() {
            bail!("Two images of the furniture have the same name ({name}).");
        }
    }
    let mut images: Folder<usize> = Folder::new();
    // ParseCsv: id not 0, the name cut at " <=" (FFDEC's comments), mappings whose comment names an XML skipped;
    // grouped by id in order of appearance.
    let mut groups: json::Dict<i32, Vec<(String, bool)>> = json::Dict::new();
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() < 2 {
            continue;
        }
        let Some(id) = names::parse_int(Some(parts[0].trim())).filter(|id| *id != 0) else { continue };
        let name_part = parts[1].trim().replace('"', "");
        let (name, comment) = match name_part.find(" <=") {
            Some(i) => (name_part[..i].trim().to_string(), name_part[i + 3..].trim().to_string()),
            None => (name_part.clone(), String::new()),
        };
        let comment = names::to_lower(&comment);
        if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|w| comment.contains(w)) {
            continue;
        }
        let is_source = comment.contains("source");
        match groups.get_mut(&id) {
            Some(list) => list.push((name, is_source)),
            None => groups.insert(id, vec![(name, is_source)]),
        }
    }
    for (id, mappings) in &groups.items {
        let key = if mappings.len() > 1 { id.to_string() } else { format!("{id}_{}", mappings[0].0) };
        let Some(&at) = lookup.get(&key) else { continue };
        let Some((_, bitmap)) = tmp[at].clone() else { continue };
        let source = mappings.iter().find(|m| m.1).unwrap_or(&mappings[0]);
        if images.create(format!("{}.png", source.0), bitmap) {
            tmp[at] = None;
        }
    }
    // ImageRestorer: with a non-empty mapping, every image left in tmp/ (in listing order) whose id (the stem up to
    // the first '_') is mapped moves to "<mapped name>.png", replacing what is there.
    if !image_mapping.is_empty() {
        let mut left = Folder::new();
        for (name, bitmap) in tmp.into_iter().flatten() {
            left.write(name, bitmap);
        }
        for (name, bitmap) in left.listed() {
            let id = stem(&name).split('_').next().unwrap_or("");
            if let Some(correct) = image_mapping.get(id) {
                let target = format!("{correct}.png");
                if !names::invalid_file_name(&target) {
                    images.write(target, bitmap);
                }
            }
        }
    }
    Ok(images.listed().into_iter().map(|(name, i)| (stem(&name).to_string(), i)).filter(|(s, _)| !s.starts_with("sh_")).collect())
}

/// The first binaryData file (in listing order) whose name ends with `suffix` (Directory.GetFiles(dir, "*" +
/// suffix), case-insensitive on Windows), decoded as File.ReadAllText does.
fn binary_file(binaries: &[(String, usize)], swf: &Swf, suffix: &str) -> Option<String> {
    binaries.iter().find(|(name, _)| names::ends_with_ci(name, suffix)).map(|(_, i)| xml::decode(&swf.binaries[*i].data))
}

/// Converts a furniture .swf (read with `crate::swf::read(bytes, true)`) named `name` into the bundle's files:
/// `<name>.json` and `<name>.png`, in that order (for `crate::hab::write`). An error where the C# gives up on the
/// file (no index, an XML that does not parse, no images, a sheet past 14000 px ...).
pub fn convert(name: &str, swf: &Swf) -> Result<Vec<(String, Vec<u8>)>> {
    let lines = names::csv_lines(&swf.symbols);
    let mut binary_folder = Folder::new();
    for (i, binary) in swf.binaries.iter().enumerate() {
        binary_folder.write(format!("{}.bin", binary.export_name()), i);
    }
    let binaries = binary_folder.listed();

    // IndexMapper: an index that is missing or does not parse fails the file.
    let index_text = binary_file(&binaries, swf, "_index.bin").ok_or_else(|| anyhow!("The furniture .swf has no index."))?;
    let index_doc = xml::parse(&index_text)?;
    let index = index_doc.root_element();

    let assets = assets::parse(
        binary_file(&binaries, swf, "_assets.bin").as_deref(),
        binary_file(&binaries, swf, "_manifest.bin").as_deref(),
        &lines,
    );

    let logic_text = binary_file(&binaries, swf, "_logic.bin");
    let logic_doc = logic_text.as_deref().map(xml::parse).transpose()?;
    let logic = logic_doc.as_ref().map(|d| logic::map(d.root_element()));

    let visualization_text = binary_file(&binaries, swf, "_visualization.bin");
    let visualization_doc = visualization_text.as_deref().map(xml::parse).transpose()?;
    let visualizations = visualization_doc.as_ref().map(|d| visualization::map(d.root_element())).transpose()?;

    let files = image_files(swf, &lines, &assets.image_mapping)?;
    if files.is_empty() {
        bail!("The furniture .swf has no images.");
    }
    let images: Vec<(String, &RgbaImage)> = files.iter().map(|(n, i)| (n.clone(), &swf.bitmaps[*i].image)).collect();
    let canonical = assets::canonical_mapping(&lines);
    let (sheet, spritesheet) = sheet::generate(&images, name, &canonical)?;

    let json = Obj::new()
        .opt("name", index.attribute("type").map(Json::str))
        .opt("logicType", index.attribute("logic").map(Json::str))
        .opt("visualizationType", index.attribute("visualization").map(Json::str))
        .opt("assets", assets.assets.as_ref().map(assets::to_json))
        .put("logic", logic::to_json(logic.as_ref())?)
        .opt("visualizations", visualizations)
        .put("spritesheet", spritesheet)
        .build();

    let mut png = Vec::new();
    sheet.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(vec![(format!("{name}.json"), json::to_string(&json).into_bytes()), (format!("{name}.png"), png)])
}
