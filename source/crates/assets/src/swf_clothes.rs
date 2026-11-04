//! A Flash clothing library (`hh_human_shirt.swf` and the like) converted to the Nitro clothing bundle: the JSON
//! (`assets` with their offsets and flipped sources, the library `name`, the `spritesheet` frames) and the PNG sheet.
//! (All-in-1 Converter: SWF_Clothes_Compiler/SWF_Clothes_To_Nitro.cs, Assets/ClothesAssetsMapper.cs,
//! Mapper/ClothesDebugXmlParser.cs, Spritesheet/SpritesheetClothesMapper.cs, FFDEC/FfdecExtractorClothes.cs,
//! and the shared SWF_Furni_Compiler/Mapper/Assests/DebugXmlParser.cs, Mapper/Spritesheet/ImageRestorer.cs,
//! Mapper/Spritesheet/DataStructureLogic.cs)
//!
//! The C# works on FFDEC's export on disk (`symbolClass/symbols.csv`, `binaryData/<id>_<symbol>.bin`,
//! `images/<id>_<symbol>.png`), moves the images through a `tmp` folder and lists folders. Here the same steps run in
//! memory, with the folder behaviour of Windows (the converter's main platform) reproduced where it shows in the
//! output: file names compared case-insensitively, and folder listings in NTFS order (by upper-cased UTF-16 units).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{bail, Context, Result};
use image::{Rgba, RgbaImage};

use crate::swf::Swf;

/// SpritesheetClothesMapper: the gap between frames and around the sheet, and the 1-pixel edge copied around each.
const FRAME_PADDING: u32 = 2;
const MAX_SHEET_SIZE: u32 = 14000;
const NUM_ROWS: usize = 10;

/// System.Text.Json's indented writer ends lines with Environment.NewLine: "\r\n" on Windows, where the converter
/// is mostly run (on Linux it would be "\n").
const NEWLINE: &str = "\r\n";

/// One manifest asset: its offset and, for a flipped/aliased asset, the asset whose image it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asset {
    x: i32,
    y: i32,
    source: Option<String>,
}

/// Converts a clothing library .swf (already read with images) into the bundle files, in the order the C# packs
/// them: `<name>.json`, then the sheet `<name>.png`. `name` is the .swf's file name without the extension.
pub fn convert(name: &str, swf: &Swf) -> Result<Vec<(String, Vec<u8>)>> {
    // ProcessSwfFileAsync skips the effect library.
    if eq_ignore_case(name, "hh_human_fx") {
        bail!("hh_human_fx is the effects library, not clothing.");
    }
    let csv = Csv::of(swf);

    // ClothesDebugXmlParser.GetClothesImageMapping: lower-cased object name -> lower-cased source name.
    let clothes_mapping = clothes_mapping(&csv);
    if clothes_mapping.is_empty() {
        bail!("{name}: the .swf has no symbols.");
    }

    let (library_name, assets) = asset_data(swf, &csv);
    let images: Vec<(String, &RgbaImage)> =
        images(swf, &csv, &clothes_mapping)?.into_iter().map(|(key, i)| (key, &swf.bitmaps[i].image)).collect();
    if images.is_empty() {
        bail!("{name}: the .swf has no named images.");
    }
    let (sheet, spritesheet) = sprite_sheet(&images, name)?;

    let json = J::Obj(vec![
        ("assets".into(), J::Obj(assets.into_iter().map(|(key, asset)| (key, asset.to_json())).collect())),
        ("name".into(), J::Str(library_name)),
        ("spritesheet".into(), spritesheet),
    ]);
    let mut text = String::new();
    json.write(0, &mut text);

    let mut png = Vec::new();
    sheet.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).context("The sprite sheet cannot be written as PNG.")?;
    Ok(vec![(format!("{name}.json"), text.into_bytes()), (format!("{name}.png"), png)])
}

impl Asset {
    fn to_json(&self) -> J {
        let mut fields = vec![("x".into(), J::Int(i64::from(self.x))), ("y".into(), J::Int(i64::from(self.y)))];
        // [JsonIgnore(Condition = WhenWritingNull)]
        if let Some(source) = &self.source {
            fields.push(("source".into(), J::Str(source.clone())));
        }
        J::Obj(fields)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// symbols.csv
// ---------------------------------------------------------------------------------------------------------------

/// The symbols.csv the C# reads, as NativeSwfExtractor writes it (`<id>;"<name>"` per symbol, in file order) and as
/// File.ReadAllLines reads it back, so names with `;`, `"`, line breaks or spaces round-trip as they do there.
struct Csv {
    lines: Vec<String>,
}

impl Csv {
    fn of(swf: &Swf) -> Csv {
        let text: String = swf.symbols.iter().map(|(id, name)| format!("{id};\"{name}\"{NEWLINE}")).collect();
        Csv { lines: read_all_lines(&text) }
    }

    /// DebugXmlParser / ClothesDebugXmlParser: the non-blank lines with at least two `;` parts, as (tag, name) with
    /// the tag trimmed and the name trimmed and stripped of quotes.
    fn entries(&self) -> Vec<(String, String)> {
        self.lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.split(';').collect();
                (parts.len() >= 2).then(|| (parts[0].trim().to_string(), parts[1].trim().replace('"', "")))
            })
            .collect()
    }

    /// DebugXmlParser.ExtractSymbolClassTags (and LINQ GroupBy): the names per tag, tags in first-seen order.
    fn tags(&self) -> Vec<(String, Vec<String>)> {
        group(self.entries())
    }
}

/// File.ReadAllLines: lines end at "\r\n", "\r" or "\n"; a final terminator makes no empty last line.
fn read_all_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut pending = false;
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
                pending = false;
            }
            _ => {
                current.push(c);
                pending = true;
            }
        }
    }
    if pending {
        lines.push(current);
    }
    lines
}

/// LINQ GroupBy: groups in the order their key is first seen, items in their order.
fn group<K: Eq + std::hash::Hash + Clone, V>(items: impl IntoIterator<Item = (K, V)>) -> Vec<(K, Vec<V>)> {
    let mut groups: Vec<(K, Vec<V>)> = Vec::new();
    let mut index: HashMap<K, usize> = HashMap::new();
    for (key, value) in items {
        let at = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[at].1.push(value);
    }
    groups
}

/// A Dictionary<string, string> with StringComparer.OrdinalIgnoreCase.
#[derive(Default)]
struct IgnoreCaseMap {
    values: HashMap<Vec<u16>, String>,
}

impl IgnoreCaseMap {
    fn insert(&mut self, key: &str, value: String) {
        self.values.insert(fold(key), value);
    }

    fn get(&self, key: &str) -> Option<&String> {
        self.values.get(&fold(key))
    }

    fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// ClothesDebugXmlParser.ParseSymbolsCsv: per tag, the second name maps to the first (both lower-cased); a tag with a
/// single name maps that name to itself.
fn clothes_mapping(csv: &Csv) -> IgnoreCaseMap {
    let mut mapping = IgnoreCaseMap::default();
    for (_, names) in csv.tags() {
        if names.len() >= 2 {
            mapping.insert(&lower_invariant(&names[1]), lower_invariant(&names[0]));
        } else if names.len() == 1 {
            let only = lower_invariant(&names[0]);
            mapping.insert(&only, only.clone());
        }
    }
    mapping
}

// ---------------------------------------------------------------------------------------------------------------
// The manifest: assets and their sources (ClothesAssetsMapper)
// ---------------------------------------------------------------------------------------------------------------

/// SWF_Clothes_To_Nitro.GetAssetDataAsync: the library name and the assets (in manifest order) of the first
/// `binaryData/*_manifest.*` file; ("", no assets) when there is none or it is not XML (the C# goes on with that).
fn asset_data(swf: &Swf, csv: &Csv) -> (String, Vec<(String, Asset)>) {
    // Directory.GetFiles(binaryData, "*_manifest.*"): the exported "<export name>.bin" files, in NTFS order; the
    // Windows pattern match is case-insensitive.
    let mut manifests: Vec<(String, &crate::swf::Binary)> = swf
        .binaries
        .iter()
        .map(|b| (format!("{}.bin", b.export_name()), b))
        .filter(|(file, _)| lower_invariant(file).contains("_manifest."))
        .collect();
    manifests.sort_by(|a, b| ntfs_order(&a.0, &b.0));
    let Some((_, manifest)) = manifests.first() else { return (String::new(), Vec::new()) };

    let text = manifest.text();
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let Ok(document) = roxmltree::Document::parse_with_options(&text, options) else { return (String::new(), Vec::new()) };
    let root = document.root_element();

    let mut assets = OrderedAssets::default();
    map_assets_from_manifest(root, &mut assets);
    let library_name = child(root, "library").and_then(|l| l.attribute("name")).unwrap_or("").to_string();
    build_mappings(&mut assets, csv);
    (library_name, assets.items)
}

/// A Dictionary<string, Asset>: insertion order, an existing key keeps its place when set again.
#[derive(Default)]
struct OrderedAssets {
    items: Vec<(String, Asset)>,
    index: HashMap<String, usize>,
}

impl OrderedAssets {
    fn set(&mut self, key: &str, asset: Asset) {
        match self.index.get(key) {
            Some(&at) => self.items[at].1 = asset,
            None => {
                self.index.insert(key.to_string(), self.items.len());
                self.items.push((key.to_string(), asset));
            }
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Asset> {
        self.index.get(key).map(|&at| &mut self.items[at].1)
    }
}

/// XElement.Element(name): the first child element with that local name and no namespace.
fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace().is_none())
}

fn children<'a, 'i: 'a>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.children().filter(move |c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace().is_none())
}

/// ClothesAssetsMapper.MapAssetsFromManifest: every `library/assets/asset` of type image/png with a name and a valid
/// "x,y" in its first `offset` param (the small-scale `sh_` assets included).
fn map_assets_from_manifest(root: roxmltree::Node, output: &mut OrderedAssets) {
    let Some(library) = child(root, "library") else { return };
    let Some(assets) = child(library, "assets") else { return };
    for asset in children(assets, "asset") {
        if asset.attribute("mimeType") != Some("image/png") {
            continue;
        }
        let Some(name) = asset.attribute("name").filter(|n| !n.is_empty()) else { continue };
        let Some(param) = children(asset, "param").find(|p| p.attribute("key") == Some("offset")) else { continue };
        let Some(value) = param.attribute("value").filter(|v| !v.is_empty()) else { continue };
        let parts: Vec<&str> = value.split(',').collect();
        if parts.len() != 2 {
            continue;
        }
        // int.TryParse (NumberStyles.Integer, after Trim): an optional sign and decimal digits within Int32.
        let (Ok(x), Ok(y)) = (parts[0].trim().parse::<i32>(), parts[1].trim().parse::<i32>()) else { continue };
        output.set(name, Asset { x, y, source: None });
    }
}

/// ClothesAssetsMapper.BuildMappingsInMemoryAsync + UpdateAssetsWithSourceFromCsvLinesAsync: per symbol id (not 0),
/// the names without the "<document class>_" prefix; the first is the source of every later one that is an asset.
fn build_mappings(assets: &mut OrderedAssets, csv: &Csv) {
    let tags = csv.tags();
    let prefix = tags.iter().find(|(tag, _)| tag == "0").and_then(|(_, names)| names.first().cloned()).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    for (tag, names) in &tags {
        if tag == "0" {
            continue;
        }
        for name in names {
            let cleaned = remove_swf_prefix(name, &prefix);
            if ["visualization", "logic", "index", "assets", "manifest"].iter().any(|s| ends_with_ignore_case(&cleaned, s)) {
                continue;
            }
            lines.push(format!("{tag},{cleaned}"));
        }
    }
    let mut source_map: HashMap<String, String> = HashMap::new();
    for line in lines {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 2 {
            continue;
        }
        let (id, name) = (parts[0], parts[1]);
        match source_map.get(id) {
            Some(source) => {
                if let Some(asset) = assets.get_mut(name) {
                    asset.source = Some(source.clone());
                }
            }
            None => {
                source_map.insert(id.to_string(), name.to_string());
            }
        }
    }
}

/// ClothesAssetsMapper.RemoveSwfPrefix: `^<prefix>_` removed, ignoring case.
fn remove_swf_prefix(name: &str, prefix: &str) -> String {
    if name.is_empty() || prefix.is_empty() {
        return name.to_string();
    }
    let pattern: Vec<char> = prefix.chars().chain(['_']).collect();
    let mut rest = name.chars();
    for p in &pattern {
        match rest.next() {
            Some(c) if fold_char(c) == fold_char(*p) || lower_char(c) == lower_char(*p) => {}
            _ => return name.to_string(),
        }
    }
    rest.as_str().to_string()
}

// ---------------------------------------------------------------------------------------------------------------
// The images, through the export's folders (FfdecExtractorClothes.RebuildImagesFromCsvAsync, ImageRestorer,
// SWF_Clothes_To_Nitro.LoadImages)
// ---------------------------------------------------------------------------------------------------------------

/// A folder of PNGs: (file name without ".png", index of the bitmap), with Windows' case-insensitive names.
#[derive(Default)]
struct Folder {
    files: Vec<(String, usize)>,
}

impl Folder {
    fn find(&self, stem: &str) -> Option<usize> {
        let key = fold(stem);
        self.files.iter().position(|(s, _)| fold(s) == key)
    }

    /// SaveAsPng over an existing file: the file keeps its name, the content is replaced.
    fn save(&mut self, stem: String, bitmap: usize) {
        match self.find(&stem) {
            Some(at) => self.files[at].1 = bitmap,
            None => self.files.push((stem, bitmap)),
        }
    }

    /// The files in Directory.GetFiles order on NTFS.
    fn listed(&self) -> Vec<(String, usize)> {
        let mut files = self.files.clone();
        files.sort_by(|a, b| ntfs_order(&format!("{}.png", a.0), &format!("{}.png", b.0)));
        files
    }
}

/// The images the sprite sheet is made of, in LoadImages order: (name, bitmap index into `swf.bitmaps`).
fn images(swf: &Swf, csv: &Csv, clothes_mapping: &IgnoreCaseMap) -> Result<Vec<(String, usize)>> {
    // The export: images/<id>_<symbol>.png, or images/<id>.png for a bitmap with no or several names.
    let mut exported = Folder::default();
    for (i, bitmap) in swf.bitmaps.iter().enumerate() {
        exported.save(bitmap.export_name(), i);
    }

    // RebuildImagesFromCsvAsync: every PNG goes to tmp/, then each symbol id's PNG comes back to images/ named
    // after the id's first symbol (a "Source" comment wins, but the native export writes no comments).
    let mut tmp = exported;
    // fileLookup: the file names without quotes; Enumerable.ToDictionary throws on a duplicate, failing the file.
    let mut lookup: HashMap<String, String> = HashMap::new();
    for (stem, _) in &tmp.files {
        if lookup.insert(stem.replace('"', ""), stem.clone()).is_some() {
            bail!("Two exported images are both named {stem}.");
        }
    }
    let mut images = Folder::default();
    for (id, mappings) in group(rebuild_mappings(csv)) {
        let lookup_key = if mappings.len() > 1 { id.to_string() } else { format!("{id}_{}", mappings[0].0) };
        let Some(file) = lookup.get(&lookup_key) else { continue };
        let Some(at) = tmp.files.iter().position(|(s, _)| s == file) else { continue };
        let source = if mappings.len() > 1 { mappings.iter().find(|m| m.1).unwrap_or(&mappings[0]) } else { &mappings[0] };
        // File.Move without overwrite: a name already taken in images/ fails, and the PNG stays in tmp/.
        if images.find(&source.0).is_some() {
            continue;
        }
        let (_, bitmap) = tmp.files.remove(at);
        images.files.push((source.0.clone(), bitmap));
    }

    // ImageRestorer.RestoreImagesFromTmpAsync (with the clothes mapping): what is left in tmp/ comes back when the
    // part of its name before the first "_" is a key of the mapping (overwriting).
    for (stem, bitmap) in tmp.listed() {
        let id = stem.split('_').next().unwrap_or("");
        let Some(correct) = clothes_mapping.get(id) else { continue };
        match images.find(correct) {
            Some(at) => images.files[at] = (correct.clone(), bitmap),
            None => images.files.push((correct.clone(), bitmap)),
        }
    }
    Ok(images.listed())
}

/// FfdecExtractorClothes.ParseCsv: (id, (name, is a "source")) per symbol, ids 0 and the library's XML left out.
fn rebuild_mappings(csv: &Csv) -> Vec<(i32, (String, bool))> {
    let mut result = Vec::new();
    for line in &csv.lines {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() < 2 {
            continue;
        }
        let Ok(id) = parts[0].trim().parse::<i32>() else { continue };
        if id == 0 {
            continue;
        }
        let name_part = parts[1].trim().replace('"', "");
        let (name, comment) = match name_part.find(" <=") {
            Some(at) => (name_part[..at].trim().to_string(), name_part[at + 3..].trim().to_string()),
            None => (name_part.clone(), String::new()),
        };
        let lower = lower_invariant(&comment);
        if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|s| lower.contains(s)) {
            continue;
        }
        result.push((id, (name, lower.contains("source"))));
    }
    result
}

// ---------------------------------------------------------------------------------------------------------------
// The sprite sheet (SpritesheetClothesMapper.GenerateSpriteSheet)
// ---------------------------------------------------------------------------------------------------------------

fn sprite_sheet(images: &[(String, &RgbaImage)], name: &str) -> Result<(RgbaImage, J)> {
    // Consecutive runs of ceil(count / 10) images make the rows (LINQ GroupBy(index / imagesPerRow)).
    let per_row = images.len().div_ceil(NUM_ROWS);
    let rows: Vec<&[(String, &RgbaImage)]> = images.chunks(per_row).collect();
    let mut width = 0u64;
    let mut stacked = 0u64;
    for row in &rows {
        let row_width = u64::from(FRAME_PADDING) + row.iter().map(|(_, i)| u64::from(i.width()) + u64::from(FRAME_PADDING)).sum::<u64>();
        let row_height = row.iter().map(|(_, i)| i.height()).max().unwrap_or(0);
        width = width.max(row_width);
        stacked += u64::from(row_height) + u64::from(FRAME_PADDING);
    }
    // Each row is as tall as its own tallest sprite.
    let height = u64::from(FRAME_PADDING) + stacked;
    if width > u64::from(MAX_SHEET_SIZE) || height > u64::from(MAX_SHEET_SIZE) {
        bail!(
            "Sprite sheet dimensions ({width}x{height}) exceed maximum allowed ({MAX_SHEET_SIZE}x{MAX_SHEET_SIZE}). Reduce the number of images or adjust the maximum dimensions."
        );
    }
    let (width, height) = (width as u32, height as u32);
    let mut sheet = RgbaImage::new(width, height);
    let mut frames = Vec::new();
    let mut y = FRAME_PADDING;
    for row in rows {
        let mut x = FRAME_PADDING;
        let mut row_height = 0;
        for (key, image) in row {
            draw_image(&mut sheet, image, x, y);
            extrude_edges(&mut sheet, image, x, y);
            let (w, h) = (i64::from(image.width()), i64::from(image.height()));
            // The frame key goes through ClothesAssetsMapper.LatestImageMapping, which by then maps every asset name
            // to itself (ParseAssetsFileAsync sets it last): the key is the image's name either way.
            frames.push((
                key.clone(),
                J::Obj(vec![
                    ("frame".into(), rect(i64::from(x), i64::from(y), w, h)),
                    ("rotated".into(), J::Bool(false)),
                    ("trimmed".into(), J::Bool(false)),
                    ("spriteSourceSize".into(), rect(0, 0, w, h)),
                    ("sourceSize".into(), size(w, h)),
                    ("pivot".into(), J::Obj(vec![("x".into(), J::Num("0.5")), ("y".into(), J::Num("0.5"))])),
                ]),
            ));
            x += image.width() + FRAME_PADDING;
            row_height = row_height.max(image.height());
        }
        y += row_height + FRAME_PADDING;
    }
    // MetaData: image, format, size, scale (1.0f, which .NET writes "1"), converter. The sheet stays PNG (no WebP).
    let meta = J::Obj(vec![
        ("image".into(), J::Str(format!("{name}.png"))),
        ("format".into(), J::Str("RGBA8888".into())),
        ("size".into(), size(i64::from(width), i64::from(height))),
        ("scale".into(), J::Num("1")),
        ("converter".into(), J::Str("All-in-1-dowload-tool".into())),
    ]);
    Ok((sheet, J::Obj(vec![("frames".into(), J::Obj(frames)), ("meta".into(), meta)])))
}

fn rect(x: i64, y: i64, w: i64, h: i64) -> J {
    J::Obj(vec![("x".into(), J::Int(x)), ("y".into(), J::Int(y)), ("w".into(), J::Int(w)), ("h".into(), J::Int(h))])
}

fn size(w: i64, h: i64) -> J {
    J::Obj(vec![("w".into(), J::Int(w)), ("h".into(), J::Int(h))])
}

/// ImageSharp's DrawImage (source-over, opacity 1) onto the sheet's transparent canvas: each pixel as it is, except a
/// fully transparent one, which the blend turns into (0, 0, 0, 0).
fn draw_image(sheet: &mut RgbaImage, sprite: &RgbaImage, x: u32, y: u32) {
    for (sx, sy, pixel) in sprite.enumerate_pixels() {
        let value = if pixel[3] == 0 { Rgba([0, 0, 0, 0]) } else { *pixel };
        sheet.put_pixel(x + sx, y + sy, value);
    }
}

/// SpritesheetClothesMapper.ExtrudeEdges: the sprite's edge pixels copied (as they are, no blending) one pixel
/// outside it, corners included.
fn extrude_edges(sheet: &mut RgbaImage, sprite: &RgbaImage, x: u32, y: u32) {
    let (w, h) = sprite.dimensions();
    for sx in 0..w {
        sheet.put_pixel(x + sx, y - 1, *sprite.get_pixel(sx, 0));
        sheet.put_pixel(x + sx, y + h, *sprite.get_pixel(sx, h - 1));
    }
    for sy in 0..h {
        sheet.put_pixel(x - 1, y + sy, *sprite.get_pixel(0, sy));
        sheet.put_pixel(x + w, y + sy, *sprite.get_pixel(w - 1, sy));
    }
    sheet.put_pixel(x - 1, y - 1, *sprite.get_pixel(0, 0));
    sheet.put_pixel(x + w, y - 1, *sprite.get_pixel(w - 1, 0));
    sheet.put_pixel(x - 1, y + h, *sprite.get_pixel(0, h - 1));
    sheet.put_pixel(x + w, y + h, *sprite.get_pixel(w - 1, h - 1));
}

// ---------------------------------------------------------------------------------------------------------------
// .NET string helpers
// ---------------------------------------------------------------------------------------------------------------

/// The simple (one-to-one) upper case of a UTF-16 unit, as OrdinalIgnoreCase and NTFS's upcase table use it.
fn upcase_unit(unit: u16) -> u16 {
    match char::from_u32(u32::from(unit)) {
        Some(c) => {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) if (u as u32) < 0x10000 => u as u16,
                _ => unit,
            }
        }
        None => unit, // a surrogate
    }
}

/// A string folded for OrdinalIgnoreCase comparison (and Windows file names).
fn fold(s: &str) -> Vec<u16> {
    s.encode_utf16().map(upcase_unit).collect()
}

fn fold_char(c: char) -> Vec<u16> {
    fold(c.encode_utf8(&mut [0; 4]))
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    fold(a) == fold(b)
}

fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    fold(s).ends_with(&fold(suffix))
}

/// Directory.GetFiles order on NTFS: file names compared by their upper-cased UTF-16 units.
fn ntfs_order(a: &str, b: &str) -> Ordering {
    fold(a).cmp(&fold(b))
}

/// The simple lower case of a char (ToLowerInvariant maps one UTF-16 unit to one).
fn lower_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn lower_invariant(s: &str) -> String {
    s.chars().map(lower_char).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// JSON as System.Text.Json writes it (WriteIndented, the default encoder)
// ---------------------------------------------------------------------------------------------------------------

enum J {
    Obj(Vec<(String, J)>),
    Str(String),
    Int(i64),
    Bool(bool),
    /// A number already formatted (the floats 0.5 and 1, which .NET writes as "0.5" and "1").
    Num(&'static str),
}

impl J {
    fn write(&self, indent: usize, out: &mut String) {
        match self {
            J::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            J::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(NEWLINE);
                    out.push_str(&" ".repeat(indent + 2));
                    write_string(key, out);
                    out.push_str(": ");
                    value.write(indent + 2, out);
                }
                out.push_str(NEWLINE);
                out.push_str(&" ".repeat(indent));
                out.push('}');
            }
            J::Str(s) => write_string(s, out),
            J::Int(n) => out.push_str(&n.to_string()),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(n),
        }
    }
}

/// JavaScriptEncoder.Default: printable ASCII as is, except the HTML-sensitive `< > & ' + \``; `"` and `\` and the
/// usual control characters with their short escapes; everything else (controls, DEL, non-ASCII) as \uXXXX of its
/// UTF-16 units, upper-case hex.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\'' | '+' | '`' => out.push_str(&format!("\\u{:04X}", c as u32)),
            ' '..='~' => out.push(c),
            _ => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{:04X}", unit));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tag(code: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((code << 6) | 0x3F).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// The colour of pixel (x, y) of bitmap `id` in the test SWFs (opaque).
    fn colour(id: u16, x: u32, y: u32) -> [u8; 4] {
        [(id as u8).wrapping_mul(20), (100 + x % 100) as u8, (150 + y % 100) as u8, 255]
    }

    /// A CWS with the binaries, the opaque ARGB bitmaps (id, width, height) and the SymbolClass entries.
    fn swf(binaries: &[(u16, &str)], bitmaps: &[(u16, u16, u16)], symbols: &[(u16, &str)]) -> Vec<u8> {
        let mut body = vec![0x00, 0, 24, 1, 0];
        for (id, text) in binaries {
            let mut binary = id.to_le_bytes().to_vec();
            binary.extend_from_slice(&[0, 0, 0, 0]);
            binary.extend_from_slice(text.as_bytes());
            body.extend(tag(87, &binary));
        }
        for &(id, w, h) in bitmaps {
            let mut bitmap = id.to_le_bytes().to_vec();
            bitmap.push(5);
            bitmap.extend_from_slice(&w.to_le_bytes());
            bitmap.extend_from_slice(&h.to_le_bytes());
            let mut argb = Vec::new();
            for y in 0..u32::from(h) {
                for x in 0..u32::from(w) {
                    let [r, g, b, a] = colour(id, x, y);
                    argb.extend_from_slice(&[a, r, g, b]);
                }
            }
            bitmap.extend(zlib(&argb));
            body.extend(tag(36, &bitmap));
        }
        let mut symbol_class = (symbols.len() as u16).to_le_bytes().to_vec();
        for (id, name) in symbols {
            symbol_class.extend_from_slice(&id.to_le_bytes());
            symbol_class.extend_from_slice(name.as_bytes());
            symbol_class.push(0);
        }
        body.extend(tag(76, &symbol_class));
        body.extend_from_slice(&[0, 0]);
        let mut out = b"CWS\x0a".to_vec();
        out.extend_from_slice(&((body.len() + 8) as u32).to_le_bytes());
        out.extend(zlib(&body));
        out
    }

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="ISO-8859-1" ?>
<manifest>
  <library name="hh_human_shirt" version="0.1">
    <assets>
      <asset name="h_std_ch_10_0_0" mimeType="image/png"><param key="offset" value="-23,-38"/></asset>
      <asset name="h_std_ch_10_2_0" mimeType="image/png"><param key="offset" value=" 1 , 2 "/></asset>
      <asset name="h_std_ch_10_4_0" mimeType="image/png"><param key="other" value="9,9"/><param key="offset" value="+31,-5"/></asset>
      <asset name="sh_std_ch_10_0_0" mimeType="image/png"><param key="offset" value="-10,-20"/></asset>
      <asset name="h_std_ch_10_6_0" mimeType="image/png"><param key="offset" value="bad"/></asset>
      <asset name="h_std_ch_10_7_0" mimeType="image/png"><param key="offset" value="1,2,3"/></asset>
      <asset name="h_std_ch_10_1_0" mimeType="image/png"><param key="offset" value=""/><param key="offset" value="4,4"/></asset>
      <asset name="h_std_ch_10_3_0" mimeType="image/png"/>
      <asset name="" mimeType="image/png"><param key="offset" value="1,1"/></asset>
      <asset name="h_std_ch_10_5_0" mimeType="application/x-shockwave-flash"><param key="offset" value="1,1"/></asset>
      <asset name="h_std_ch_10_0_0" mimeType="image/png"><param key="offset" value="-24,-39"/></asset>
    </assets>
  </library>
</manifest>"#;

    fn shirt() -> Swf {
        let data = swf(
            &[(1, MANIFEST), (7, "<index/>")],
            &[(2, 2, 3), (3, 1, 1), (4, 1, 1), (5, 1, 1), (6, 1, 2)],
            &[
                (0, "hh_human_shirt"),
                (1, "hh_human_shirt_manifest"),
                (7, "hh_human_shirt_index"),
                (2, "hh_human_shirt_h_std_ch_10_0_0"),
                (3, "hh_human_shirt_h_std_ch_10_2_0"),
                (3, "hh_human_shirt_h_std_ch_10_4_0"),
                (4, "hh_human_shirt_sh_std_ch_10_0_0"),
                (6, "hh_human_shirt_h_std_chl_5_0_0"),
            ],
        );
        crate::swf::read(&data, true).unwrap()
    }

    fn crlf(text: &str) -> String {
        text.replace('\n', "\r\n")
    }

    fn frame(x: u32, y: u32, w: u32, h: u32) -> String {
        format!(
            r#"{{
        "frame": {{
          "x": {x},
          "y": {y},
          "w": {w},
          "h": {h}
        }},
        "rotated": false,
        "trimmed": false,
        "spriteSourceSize": {{
          "x": 0,
          "y": 0,
          "w": {w},
          "h": {h}
        }},
        "sourceSize": {{
          "w": {w},
          "h": {h}
        }},
        "pivot": {{
          "x": 0.5,
          "y": 0.5
        }}
      }}"#
        )
    }

    #[test]
    fn a_clothing_library() {
        let swf = shirt();
        let files = convert("hh_human_shirt", &swf).unwrap();
        assert_eq!(files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["hh_human_shirt.json", "hh_human_shirt.png"]);

        // Assets in manifest order (a repeated name keeps its place, takes the later offset); h_std_ch_10_4_0 shares
        // the bitmap of h_std_ch_10_2_0, its source. Frames in NTFS order: "chl" before "ch_" ('L' < '_'), the
        // bitmap without a symbol (5) dropped; one image per row (4 images: ceil(4 / 10) = 1 per row).
        let expected = format!(
            r#"{{
  "assets": {{
    "h_std_ch_10_0_0": {{
      "x": -24,
      "y": -39
    }},
    "h_std_ch_10_2_0": {{
      "x": 1,
      "y": 2
    }},
    "h_std_ch_10_4_0": {{
      "x": 31,
      "y": -5,
      "source": "h_std_ch_10_2_0"
    }},
    "sh_std_ch_10_0_0": {{
      "x": -10,
      "y": -20
    }}
  }},
  "name": "hh_human_shirt",
  "spritesheet": {{
    "frames": {{
      "hh_human_shirt_h_std_chl_5_0_0": {},
      "hh_human_shirt_h_std_ch_10_0_0": {},
      "hh_human_shirt_h_std_ch_10_2_0": {},
      "hh_human_shirt_sh_std_ch_10_0_0": {}
    }},
    "meta": {{
      "image": "hh_human_shirt.png",
      "format": "RGBA8888",
      "size": {{
        "w": 6,
        "h": 17
      }},
      "scale": 1,
      "converter": "All-in-1-dowload-tool"
    }}
  }}
}}"#,
            frame(2, 2, 1, 2),
            frame(2, 6, 2, 3),
            frame(2, 11, 1, 1),
            frame(2, 14, 1, 1)
        );
        assert_eq!(String::from_utf8(files[0].1.clone()).unwrap(), crlf(&expected));

        let sheet = image::load_from_memory(&files[1].1).unwrap().to_rgba8();
        assert_eq!(sheet.dimensions(), (6, 17));
        // Bitmap 2 (2x3) at (2, 6): its pixels, then its edges one pixel out, corners included.
        assert_eq!(sheet.get_pixel(2, 6).0, colour(2, 0, 0));
        assert_eq!(sheet.get_pixel(3, 8).0, colour(2, 1, 2));
        assert_eq!(sheet.get_pixel(1, 5).0, colour(2, 0, 0));
        assert_eq!(sheet.get_pixel(2, 5).0, colour(2, 0, 0));
        assert_eq!(sheet.get_pixel(3, 5).0, colour(2, 1, 0));
        assert_eq!(sheet.get_pixel(4, 7).0, colour(2, 1, 1));
        assert_eq!(sheet.get_pixel(1, 7).0, colour(2, 0, 1));
        assert_eq!(sheet.get_pixel(4, 9).0, colour(2, 1, 2));
        assert_eq!(sheet.get_pixel(5, 7).0, [0, 0, 0, 0]);
        assert_eq!(sheet.get_pixel(0, 7).0, [0, 0, 0, 0]);
        // The shared bitmap 3 under the first name of its symbol; bitmap 6 on top.
        assert_eq!(sheet.get_pixel(2, 11).0, colour(3, 0, 0));
        assert_eq!(sheet.get_pixel(2, 3).0, colour(6, 0, 1));
        assert_eq!(sheet.get_pixel(2, 14).0, colour(4, 0, 0));
        assert_eq!(sheet.get_pixel(0, 0).0, [0, 0, 0, 0]);

        let hab = crate::hab::write("hh_human_shirt", &files);
        assert_eq!(crate::hab::read(&hab).unwrap(), files);
    }

    /// 11 images: ceil(11 / 10) = 2 per row, 6 rows; no manifest and no document class (id 0): no assets, name "".
    #[test]
    fn rows_and_no_manifest() {
        let bitmaps: Vec<(u16, u16, u16)> = (1..=11).map(|i| (i, i, 12 - i)).collect();
        let names: Vec<String> = (1..=11).map(|i| format!("lib_a_{i:02}")).collect();
        let symbols: Vec<(u16, &str)> = names.iter().enumerate().map(|(i, n)| (i as u16 + 1, n.as_str())).collect();
        let swf = crate::swf::read(&swf(&[], &bitmaps, &symbols), true).unwrap();
        let files = convert("lib", &swf).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
        assert_eq!(json["assets"], serde_json::json!({}));
        assert_eq!(json["name"], "");
        let text = String::from_utf8(files[0].1.clone()).unwrap();
        assert!(text.starts_with("{\r\n  \"assets\": {},\r\n  \"name\": \"\",\r\n"));
        let frames = json["spritesheet"]["frames"].as_object().unwrap();
        assert_eq!(frames.keys().cloned().collect::<Vec<_>>(), names);
        let at = |key: &str| {
            let f = &frames[key]["frame"];
            (f["x"].as_i64().unwrap(), f["y"].as_i64().unwrap(), f["w"].as_i64().unwrap(), f["h"].as_i64().unwrap())
        };
        // Row 0: 01 (1x11), 02 (2x10); row 1 starts at y = 2 + 11 + 2.
        assert_eq!(at("lib_a_01"), (2, 2, 1, 11));
        assert_eq!(at("lib_a_02"), (5, 2, 2, 10));
        assert_eq!(at("lib_a_03"), (2, 15, 3, 9));
        assert_eq!(at("lib_a_04"), (7, 15, 4, 8));
        assert_eq!(at("lib_a_11"), (2, 15 + 11 + 9 + 7 + 5, 11, 1));
        // Width: the widest row, 9 + 10 + 3 * 2 = 25 (row 4: 09, 10); the last row 2 + 11 + 2 = 15.
        // Height: 2 + (11 + 2) + (9 + 2) + (7 + 2) + (5 + 2) + (3 + 2) + (1 + 2) = 50.
        assert_eq!(json["spritesheet"]["meta"]["size"], serde_json::json!({"w": 25, "h": 50}));
        assert_eq!(json["spritesheet"]["meta"]["image"], "lib.png");
    }

    #[test]
    fn refusals() {
        assert!(convert("HH_HUMAN_FX", &shirt()).is_err());
        assert!(convert("empty", &Swf::default()).is_err(), "no symbols");
        let only_xml = crate::swf::read(&swf(&[(1, MANIFEST)], &[], &[(1, "x_manifest")]), true).unwrap();
        assert!(convert("x", &only_xml).is_err(), "no images");
        let huge = crate::swf::read(&swf(&[], &[(1, 14000, 1)], &[(1, "x_a")]), true).unwrap();
        assert!(convert("x", &huge).unwrap_err().to_string().contains("14004x5"));
    }

    #[test]
    fn transparent_pixels_drawn_as_zero_but_extruded_as_they_are() {
        let mut sprite = RgbaImage::new(1, 1);
        sprite.put_pixel(0, 0, Rgba([9, 8, 7, 0]));
        let mut sheet = RgbaImage::new(3, 3);
        draw_image(&mut sheet, &sprite, 1, 1);
        extrude_edges(&mut sheet, &sprite, 1, 1);
        assert_eq!(sheet.get_pixel(1, 1).0, [0, 0, 0, 0]);
        assert_eq!(sheet.get_pixel(0, 0).0, [9, 8, 7, 0]);
        assert_eq!(sheet.get_pixel(2, 1).0, [9, 8, 7, 0]);
    }

    #[test]
    fn dotnet_details() {
        let mut out = String::new();
        write_string("a+b<c>&'`\"\\\n\u{1}\u{7f}é😀", &mut out);
        let u = |hex: &str| format!("{}u{hex}", '\\');
        let expected = format!(
            "\"a{}b{}c{}{}{}{}\\\"\\\\\\n{}{}{}{}{}\"",
            u("002B"),
            u("003C"),
            u("003E"),
            u("0026"),
            u("0027"),
            u("0060"),
            u("0001"),
            u("007F"),
            u("00E9"),
            u("D83D"),
            u("DE00")
        );
        assert_eq!(out, expected);
        assert_eq!(remove_swf_prefix("HH_Human_Shirt_h_std", "hh_human_shirt"), "h_std");
        assert_eq!(remove_swf_prefix("hh_human_shirth_std", "hh_human_shirt"), "hh_human_shirth_std");
        assert_eq!(read_all_lines("a\r\nb\rc\n\nd"), ["a", "b", "c", "", "d"]);
        assert_eq!(read_all_lines("a\r\n"), ["a"]);
        let mut names = vec!["a_b.png", "ab.png", "A.png", "a!.png"];
        names.sort_by(|a, b| ntfs_order(a, b));
        assert_eq!(names, ["a!.png", "A.png", "ab.png", "a_b.png"]);
    }

    /// Symbols sharing a name but for case: on Windows the second image cannot be moved over the first, and is lost.
    #[test]
    fn image_names_are_case_insensitive() {
        let swf = crate::swf::read(&swf(&[], &[(1, 1, 1), (2, 2, 2)], &[(1, "lib_A"), (2, "lib_a")]), true).unwrap();
        let files = convert("lib", &swf).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
        assert_eq!(json["spritesheet"]["frames"].as_object().unwrap().keys().collect::<Vec<_>>(), ["lib_A"]);
        assert_eq!(json["spritesheet"]["frames"]["lib_A"]["frame"]["w"], 1);
    }

    /// The custom effects are avatar libraries too, when the converter's repository is next to this one. Most have a
    /// manifest of image offsets and convert fully; some have no bitmaps (refused, as in the C#) or no offsets (no
    /// assets, as in the C#).
    #[test]
    fn real_effect_swfs_when_present() {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../duckietm/all-in-1-converter/Addons/Custom Effects/SWF");
        let Ok(entries) = std::fs::read_dir(folder) else { return };
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "swf")).collect();
        files.sort();
        let mut complete = 0;
        for path in &files {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let swf = crate::swf::read(&std::fs::read(path).unwrap(), true).unwrap();
            let Ok(files) = convert(&name, &swf) else {
                assert!(swf.bitmaps.is_empty(), "{name}");
                continue;
            };
            let json: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
            assert!(!json["spritesheet"]["frames"].as_object().unwrap().is_empty(), "{name}");
            let sheet = image::load_from_memory(&files[1].1).unwrap();
            assert_eq!(json["spritesheet"]["meta"]["size"]["w"], sheet.width(), "{name}");
            if !json["assets"].as_object().unwrap().is_empty() {
                complete += 1;
            }
        }
        assert!(complete > 350, "{complete}");
    }
}
