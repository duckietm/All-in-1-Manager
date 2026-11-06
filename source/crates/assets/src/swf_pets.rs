//! A pet's .swf turned into the files of its .hab: `<name>.json` (Nitro's pet data: index, assets, palettes, logic,
//! visualizations and the sprite sheet's frames) and the sheet `<name>.png`. (All-in-1 Converter:
//! SWF_Pets_Compiler/SWF_Pets_To_Nitro.cs and its Mapper/, with what it borrows from SWF_Furni_Compiler: the FFDEC
//! export's image renaming (FfdecExtractor.RebuildImagesFromCsvAsync, ImageRestorer), AssetNameMapper, DebugXmlParser,
//! LogicMapper, FloatToFixedDecimalConverter and SpriteSheetMapper.)
//!
//! The C# reads an FFDEC-style export from disk; here the same names come from [`crate::swf::Swf`] in memory:
//! `symbols.csv` is `symbols`, `binaryData/<export name>.bin` the binaries, `images/<export name>.png` the bitmaps.
//! Where the C# output depends on .NET or Windows behaviour (directory listing order, System.Text.Json's indented
//! writer and escaping, float formatting, the `Dictionary` insertion order), it is reproduced on purpose and said so.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use image::{Rgba, RgbaImage};
use roxmltree::{Document, Node};

use crate::swf::Swf;

/// The bundle's files, as the C# hands them to AssetBundleWriter before its WebP step: the JSON, then the PNG sheet.
pub fn convert(name: &str, swf: &Swf) -> Result<Vec<(String, Vec<u8>)>> {
    let csv = Csv::from(swf);

    // GetIndexPetsDataAsync: the first "*_index.bin"; no index (or a broken one) and the pet is not converted.
    let index_xml = first_bin(swf, "_index").ok_or_else(|| anyhow!("{name}: the .swf has no *_index binary."))?;
    let index = Index::parse(&index_xml).with_context(|| format!("{name}: the index XML"))?;

    // GetAssetPetsDataAsync: both the assets and the manifest are needed (without them the C# fails further on).
    let assets_xml = first_bin(swf, "_assets").ok_or_else(|| anyhow!("{name}: the .swf has no *_assets binary."))?;
    let manifest_xml = first_bin(swf, "_manifest").ok_or_else(|| anyhow!("{name}: the .swf has no *_manifest binary."))?;
    let image_sources = csv.debug_mapping();
    let (mut assets, source_map) = Assets::parse(&assets_xml, &manifest_xml, &image_sources, &csv);

    // GetLogicDataAsync: no logic leaves logicData null, which the C# then dereferences: the conversion fails.
    let logic_xml = first_bin(swf, "_logic").ok_or_else(|| anyhow!("{name}: the .swf has no *_logic binary."))?;
    let logic = logic_json(&parse_xml(&logic_xml).with_context(|| format!("{name}: the logic XML"))?.root_element());

    let mut visualizations = match first_bin(swf, "_visualization") {
        Some(xml) => Some(visualizations(&parse_xml(&xml).with_context(|| format!("{name}: the visualization XML"))?.root_element())?),
        None => None,
    };
    let palettes = palettes(swf, &assets_xml).with_context(|| format!("{name}: the palettes"))?;

    let keep_size_32 = should_keep_size_32(visualizations.as_deref(), &assets);
    if !keep_size_32 {
        if let Some(list) = visualizations.as_mut() {
            list.retain(|v| v.size != 32);
        }
        assets.list.retain(|(key, _)| !key.contains("_32_"));
    }

    // LoadImages: the images/ folder after the renaming, minus sh_ images and (unless kept) the size-32 frames.
    let images: Vec<(String, &RgbaImage)> = image_files(swf, &csv, &source_map)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("sh_") && (keep_size_32 || !name.contains("_32_")))
        .collect();
    if images.is_empty() {
        bail!("{name}: the .swf has no images to put in a sprite sheet.");
    }
    let canonical = csv.canonical_mapping();
    let (sheet, spritesheet) = sprite_sheet(&images, name, &canonical)?;

    let visualizations = visualizations.map(|list| list.into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>()).filter(|list| !list.is_empty());
    let mut root = Obj::new();
    root.opt("type", index.logic.clone().map(J::Str));
    root.opt("name", index.kind.map(J::Str));
    root.opt("logicType", index.logic.map(J::Str));
    root.opt("visualizationType", index.visualization.map(J::Str));
    root.push("assets", assets.json());
    root.push("palettes", J::Obj(palettes.into_iter().map(|(id, p)| (id.to_string(), p.json())).collect()));
    root.push("logic", logic);
    root.opt("visualizations", visualizations.map(|list| J::Arr(list.into_iter().map(Visualization::json).collect())));
    root.push("spritesheet", spritesheet);

    // ProcessSwfFileAsync's clean-up of System.Text.Json's indentation, on the "\n" new lines the writer uses on
    // Linux. (On Windows its Environment.NewLine is "\r\n", which these replacements only partly match: the JSON is
    // the same data, with other white space.)
    let json = J::Obj(root.0).to_indented().replace("[\n  ", "[").replace("\n  ]", "]").replace("\n    ", " ");

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(sheet).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).context("encoding the sprite sheet")?;
    Ok(vec![(format!("{name}.json"), json.into_bytes()), (format!("{name}.png"), png)])
}

// ---------------------------------------------------------------------------------------------------------------
// JSON as System.Text.Json writes it (WriteIndented, WhenWritingNull, AllowNamedFloatingPointLiterals)
// ---------------------------------------------------------------------------------------------------------------

/// One JSON value. `Raw` is written as it is (numbers, and the converters' WriteRawValue); `RawArr` is an array of raw
/// values (CompactRgbConverter), which Utf8JsonWriter does not indent.
#[derive(Debug, Clone, PartialEq)]
enum J {
    Raw(String),
    Str(String),
    Bool(bool),
    Arr(Vec<J>),
    RawArr(Vec<String>),
    Obj(Vec<(String, J)>),
}

fn int(value: i32) -> J {
    J::Raw(value.to_string())
}

/// An object under construction; `opt` leaves out a None like WhenWritingNull does.
#[derive(Default)]
struct Obj(Vec<(String, J)>);

impl Obj {
    fn new() -> Self {
        Self::default()
    }
    fn push(&mut self, key: &str, value: J) {
        self.0.push((key.to_string(), value));
    }
    fn opt(&mut self, key: &str, value: Option<J>) {
        if let Some(value) = value {
            self.push(key, value);
        }
    }
    fn json(self) -> J {
        J::Obj(self.0)
    }
}

/// `dictionary[key] = value` on a .NET Dictionary: a new key goes last, an existing one keeps its place.
fn put<K: PartialEq, V>(list: &mut Vec<(K, V)>, key: K, value: V) {
    match list.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = value,
        None => list.push((key, value)),
    }
}

impl J {
    /// Utf8JsonWriter with Indented: two spaces, `"key": value`, empty containers as `{}` / `[]`.
    fn to_indented(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        fn newline(out: &mut String, depth: usize) {
            out.push('\n');
            out.extend(std::iter::repeat_n("  ", depth));
        }
        match self {
            J::Raw(raw) => out.push_str(raw),
            J::Str(text) => escape(out, text),
            J::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            J::Arr(items) if items.is_empty() => out.push_str("[]"),
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, depth + 1);
                    item.write(out, depth + 1);
                }
                newline(out, depth);
                out.push(']');
            }
            // WriteRawValue writes the list separator but no new line or indentation; the closing bracket then gets
            // its new line (the writer takes a raw value for a finished token).
            J::RawArr(items) if items.is_empty() => out.push_str("[]"),
            J::RawArr(items) => {
                out.push('[');
                out.push_str(&items.join(","));
                newline(out, depth);
                out.push(']');
            }
            J::Obj(entries) if entries.is_empty() => out.push_str("{}"),
            J::Obj(entries) => {
                out.push('{');
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, depth + 1);
                    escape(out, key);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                newline(out, depth);
                out.push('}');
            }
        }
    }
}

/// A JSON string as System.Text.Json's default encoder writes it: printable ASCII as it is, except the HTML-sensitive
/// `" & ' + < > \``; \b \t \n \f \r \\ short; everything else (control characters, DEL, non-ASCII) as uppercase
/// \uXXXX per UTF-16 unit.
fn escape(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => out.push_str(&format!("\\u{:04X}", c as u32)),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04X}"));
                }
            }
        }
    }
    out.push('"');
}

/// A float as System.Text.Json writes it: .NET's shortest round-trip "G" (plain from 1E-04 up to 9 integer digits,
/// otherwise "1.5E+20" / "1E-05"), and NaN / ±Infinity as strings (AllowNamedFloatingPointLiterals).
fn float(value: f32) -> J {
    if value.is_nan() {
        return J::Str("NaN".into());
    }
    if value.is_infinite() {
        return J::Str(if value > 0.0 { "Infinity" } else { "-Infinity" }.into());
    }
    J::Raw(dotnet_g(value))
}

/// .NET Core 3.0+ `float.ToString("G", InvariantCulture)`, measured with .NET 10: shortest round-trip digits, in
/// scientific notation when the decimal exponent is below -4 or above 8.
fn dotnet_g(value: f32) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} has an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} exponent");
    if (-4..=8).contains(&exponent) {
        format!("{value}")
    } else {
        format!("{mantissa}E{}{:02}", if exponent < 0 { '-' } else { '+' }, exponent.abs())
    }
}

/// FloatToFixedDecimalConverter (dimensions' z): whole numbers as an int, tiny ones with six decimals, the rest "G".
fn fixed_decimal(value: f32) -> J {
    if value % 1.0 == 0.0 {
        // (int)value: a saturating cast in .NET Core 3.0+ as in Rust
        J::Raw((value as i32).to_string())
    } else if value.abs() < 0.0001 {
        // "F6": .NET Core 3.0+ formats the exact binary value, as Rust does
        J::Raw(format!("{value:.6}"))
    } else {
        J::Raw(dotnet_g(value))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// .NET parsing rules
// ---------------------------------------------------------------------------------------------------------------

/// The white space .NET's number parsing trims (NumberStyles.AllowLeading/TrailingWhite).
fn trim_white(text: &str) -> &str {
    text.trim_matches(|c: char| c == ' ' || ('\t'..='\r').contains(&c))
}

/// `int.TryParse` (NumberStyles.Integer): white space around, an optional sign, ASCII digits.
fn parse_int(text: Option<&str>) -> Option<i32> {
    let text = trim_white(text?);
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `float.TryParse(NumberStyles.Float, InvariantCulture)`: Rust's grammar plus white space, but only .NET's names
/// for the specials ("NaN", "Infinity", case-insensitive; Rust would also take "inf").
fn parse_float(text: Option<&str>) -> Option<f32> {
    let text = trim_white(text?);
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned.eq_ignore_ascii_case("inf") {
        return None;
    }
    text.parse().ok()
}

/// `bool.TryParse`: "true"/"false" in any case, white space around.
fn parse_bool(text: Option<&str>) -> bool {
    text.is_some_and(|t| t.trim().eq_ignore_ascii_case("true"))
}

/// `int.Parse` (palettes): a failure fails the conversion, as the C#'s exception does.
fn parse_int_strict(text: &str, what: &str) -> Result<i32> {
    parse_int(Some(text)).ok_or_else(|| anyhow!("{what} \"{text}\" is not a number."))
}

/// `Convert.ToInt32(hex, 16)`: an optional 0x, up to 8 hex digits taken as the bits of an int; anything else throws.
fn parse_hex(text: &str) -> Result<i32> {
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("The colour \"{text}\" is not hexadecimal.");
    }
    Ok(u32::from_str_radix(digits, 16)? as i32)
}

// ---------------------------------------------------------------------------------------------------------------
// XML (XElement)
// ---------------------------------------------------------------------------------------------------------------

fn parse_xml(text: &str) -> Result<Document<'_>> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    Ok(Document::parse_with_options(text, options)?)
}

/// `element.Elements(name)`.
fn children<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |n| n.is_element() && n.tag_name().name() == name && n.tag_name().namespace().is_none())
}

/// `element.Element(name)`.
fn child<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> Option<Node<'a, 'i>> {
    children(node, name).next()
}

fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute(name)
}

fn attr_int(node: Node, name: &str) -> Option<i32> {
    parse_int(attr(node, name))
}

fn attr_float(node: Node, name: &str) -> Option<f32> {
    parse_float(attr(node, name))
}

fn attr_str(node: Node, name: &str) -> Option<J> {
    attr(node, name).map(|v| J::Str(v.to_string()))
}

// ---------------------------------------------------------------------------------------------------------------
// The export's file names
// ---------------------------------------------------------------------------------------------------------------

/// The order Windows lists a folder in (NTFS keeps names sorted by their upper-case UTF-16 units), which is the
/// order Directory.GetFiles hands the C# its "first" binary and its images. (On Linux .NET lists in the file system's
/// own order, which is not reproducible.)
fn ntfs_key(file: &str) -> Vec<u16> {
    let upper: String = file
        .chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect();
    upper.encode_utf16().collect()
}

/// The binaries as `binaryData/*.bin` files: (file name, binary), in Windows' listing order.
fn bin_files(swf: &Swf) -> Vec<(String, &crate::swf::Binary)> {
    let mut files: Vec<_> = swf.binaries.iter().map(|b| (format!("{}.bin", b.export_name()), b)).collect();
    files.sort_by_cached_key(|(file, _)| ntfs_key(file));
    files
}

/// The text of the first `*<suffix>.bin` (Windows matches the pattern case-insensitively).
fn first_bin(swf: &Swf, suffix: &str) -> Option<String> {
    let pattern = format!("{suffix}.bin").to_lowercase();
    bin_files(swf).into_iter().find(|(file, _)| file.to_lowercase().ends_with(&pattern)).map(|(_, b)| b.text())
}

/// `symbols.csv` as the C# reads it back: the export writes `id;"name"` lines; the readers split on ';', trim and drop
/// the quotes.
struct Csv {
    /// (tag, name) per non-empty line.
    entries: Vec<(String, String)>,
}

impl Csv {
    fn from(swf: &Swf) -> Self {
        let mut entries = Vec::new();
        for (id, name) in &swf.symbols {
            let line = format!("{id};\"{name}\"");
            // File.ReadAllLines splits a name with a line break into lines of their own.
            for line in line.split(['\n', '\r']).filter(|l| !l.trim().is_empty()) {
                let parts: Vec<&str> = line.split(';').collect();
                if parts.len() < 2 {
                    continue;
                }
                entries.push((parts[0].trim().to_string(), parts[1].trim().replace('"', "")));
            }
        }
        Csv { entries }
    }

    /// The tags in order of first appearance with their names (GroupBy / ExtractSymbolClassTags).
    fn groups(&self) -> Vec<(&str, Vec<&str>)> {
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for (tag, name) in &self.entries {
            match groups.iter_mut().find(|(t, _)| t == tag) {
                Some((_, names)) => names.push(name),
                None => groups.push((tag, vec![name])),
            }
        }
        groups
    }

    /// DebugXmlParser.ParseDebugXml: for a tag with two names or more, the second name (lower case) → the first. A
    /// case-insensitive Dictionary: (lower-case key, value) in insertion order.
    fn debug_mapping(&self) -> CiMap {
        let mut mapping = CiMap::default();
        for (_, names) in self.groups() {
            if names.len() >= 2 {
                mapping.set(names[1].to_lowercase(), names[0].to_lowercase());
            }
        }
        mapping
    }

    /// AssetNameMapper.BuildCanonicalMapping: cleaned name → the first full name (lower case) that cleans to it,
    /// skipping tag 0 and the library's own binaries. Case-insensitive.
    fn canonical_mapping(&self) -> CiMap {
        let mut mapping = CiMap::default();
        for (tag, name) in &self.entries {
            if parse_int(Some(tag)).is_none_or(|id| id == 0) {
                continue;
            }
            let lower = name.to_lowercase();
            if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|w| lower.contains(w)) {
                continue;
            }
            let short = clean_asset_name(&lower);
            if mapping.get(&short).is_none() {
                mapping.set(short, lower);
            }
        }
        mapping
    }
}

/// A `Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)`, compared through lower case.
#[derive(Default)]
struct CiMap {
    entries: Vec<(String, String)>,
    index: HashMap<String, usize>,
}

impl CiMap {
    fn set(&mut self, key: String, value: String) {
        match self.index.get(&key.to_lowercase()) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.to_lowercase(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.index.get(&key.to_lowercase()).map(|&i| self.entries[i].1.as_str())
    }
}

/// SpriteSheetMapper.CleanAssetName: `^([^_]+)_\1_` → `$1_` (a doubled library prefix made single).
fn clean_asset_name(name: &str) -> String {
    if let Some((first, rest)) = name.split_once('_') {
        if !first.is_empty() {
            if let Some(after) = rest.strip_prefix(first).and_then(|r| r.strip_prefix('_')) {
                return format!("{first}_{after}");
            }
        }
    }
    name.to_string()
}

/// SpriteSheetMapper.ForceCFUpper: "cf_" / "cfc_" at the start or after an underscore, in upper case.
fn force_cf_upper(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    let bytes = name.as_bytes();
    while i < name.len() {
        let at_start = i == 0 || bytes[i - 1] == b'_';
        if at_start {
            let rest = &name[i..];
            // The alternation tries "cfc_" before "cf_".
            let len = if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case("cfc_") {
                4
            } else if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case("cf_") {
                3
            } else {
                0
            };
            if len > 0 {
                out.push_str(&rest[..len].to_ascii_uppercase());
                i += len;
                continue;
            }
        }
        let c = name[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The `images/` folder the C# loads its sheet from: FfdecExtractor.RebuildImagesFromCsvAsync moves every exported
/// PNG to tmp/ and back as `<first csv name>.png` (a lone name: from `<id>_<name>.png`; several names: from
/// `<id>.png`; File.Move without overwrite, so a name taken already (case-insensitively, on Windows) stays in tmp/);
/// ImageRestorer then moves what is left in tmp/ to `<source map name>.png` by its id, overwriting. Returned sorted
/// in Windows' listing order (LoadImages fills its dictionary, and so the sheet, in that order).
fn image_files<'a>(swf: &'a Swf, csv: &Csv, source_map: &[(String, String)]) -> Vec<(String, &'a RgbaImage)> {
    // tmp/: (file name without .png, image)
    let mut tmp: Vec<(String, &RgbaImage)> = swf.bitmaps.iter().map(|b| (b.export_name().replace('"', ""), &b.image)).collect();
    let mut images: Vec<(String, &RgbaImage)> = Vec::new();
    let taken = |images: &Vec<(String, &RgbaImage)>, name: &str| images.iter().position(|(n, _)| n.to_lowercase() == name.to_lowercase());

    // FfdecExtractor.ParseCsv: id > 0, the name before an optional " <=" comment, skipping commented binaries.
    let mut groups: Vec<(i32, Vec<(String, bool)>)> = Vec::new();
    for (tag, value) in &csv.entries {
        let Some(id) = parse_int(Some(tag)).filter(|&id| id != 0) else { continue };
        let (name, comment) = match value.find(" <=") {
            Some(at) => (value[..at].trim().to_string(), value[at + 3..].trim().to_lowercase()),
            None => (value.clone(), String::new()),
        };
        if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|w| comment.contains(w)) {
            continue;
        }
        let is_source = comment.contains("source");
        match groups.iter_mut().find(|(i, _)| *i == id) {
            Some((_, list)) => list.push((name, is_source)),
            None => groups.push((id, vec![(name, is_source)])),
        }
    }
    for (id, mappings) in &groups {
        let lookup = if mappings.len() > 1 { id.to_string() } else { format!("{id}_{}", mappings[0].0) };
        let Some(at) = tmp.iter().position(|(file, _)| *file == lookup) else { continue };
        let target = if mappings.len() > 1 { mappings.iter().find(|m| m.1).unwrap_or(&mappings[0]) } else { &mappings[0] };
        if taken(&images, &target.0).is_some() {
            continue;
        }
        let (_, image) = tmp.remove(at);
        images.push((target.0.clone(), image));
    }

    if !source_map.is_empty() {
        tmp.sort_by_cached_key(|(file, _)| ntfs_key(&format!("{file}.png")));
        for (file, image) in tmp {
            let id = file.split('_').next().unwrap_or("");
            if let Some((_, name)) = source_map.iter().find(|(tag, _)| tag == id) {
                if let Some(at) = taken(&images, name) {
                    images.remove(at);
                }
                images.push((name.clone(), image));
            }
        }
    }
    images.sort_by_cached_key(|(name, _)| ntfs_key(&format!("{name}.png")));
    images
}

// ---------------------------------------------------------------------------------------------------------------
// Index (IndexPetsMapper)
// ---------------------------------------------------------------------------------------------------------------

struct Index {
    logic: Option<String>,
    kind: Option<String>,
    visualization: Option<String>,
}

impl Index {
    fn parse(text: &str) -> Result<Index> {
        let doc = parse_xml(text)?;
        let root = doc.root_element();
        let get = |name| attr(root, name).map(str::to_string);
        // "type" is the logic attribute and "name" the type attribute, as the C# maps them.
        Ok(Index { logic: get("logic"), kind: get("type"), visualization: get("visualization") })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Assets (AssetsPetsMapper)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
struct Asset {
    source: Option<String>,
    x: i32,
    y: i32,
    flip_h: bool,
    flip_v: bool,
    uses_palette: bool,
}

/// The assets by lower-case name, in Dictionary order.
#[derive(Debug, Default)]
struct Assets {
    list: Vec<(String, Asset)>,
}

impl Assets {
    fn position(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|(n, _)| n == name)
    }

    /// ParseAssetsFileAsync. Also returns BuildMappingsInMemoryAsync's source map (tag → name), which ImageRestorer
    /// uses. A broken XML gives no assets and an empty map, as the C#'s catch does.
    fn parse(assets_xml: &str, manifest_xml: &str, image_sources: &CiMap, csv: &Csv) -> (Assets, Vec<(String, String)>) {
        let (Ok(assets_doc), Ok(manifest_doc)) = (parse_xml(assets_xml), parse_xml(manifest_xml)) else {
            return (Assets::default(), Vec::new());
        };
        let mut assets = Assets::map_xml(assets_doc.root_element(), manifest_doc.root_element(), image_sources);
        let source_map = assets.build_mappings(csv, image_sources);
        for i in 0..assets.list.len() {
            // In Dictionary order, each resolution seeing the ones before it.
            if let Some(source) = assets.list[i].1.source.clone().filter(|s| !s.is_empty()) {
                assets.list[i].1.source = Some(assets.resolve_source(source));
            }
        }
        (assets, source_map)
    }

    /// MapAssetsXML: the assets that are PNGs in the manifest or have a source; then a source from the csv aliases.
    fn map_xml(root: Node, manifest: Node, image_sources: &CiMap) -> Assets {
        let manifest_names: HashSet<String> = manifest
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "asset" && n.tag_name().namespace().is_none())
            .filter(|a| attr(*a, "mimeType") == Some("image/png"))
            .map(|a| attr(a, "name").unwrap_or("").to_lowercase())
            .collect();
        let mut assets = Assets::default();
        for element in children(root, "asset") {
            let name = attr(element, "name").unwrap_or("").to_lowercase();
            if !manifest_names.contains(&name) && attr(element, "source").is_none() {
                continue;
            }
            let asset = Asset {
                source: attr(element, "source").map(str::to_lowercase),
                x: attr_int(element, "x").unwrap_or(0),
                y: attr_int(element, "y").unwrap_or(0),
                flip_h: attr(element, "flipH") == Some("1"),
                flip_v: attr(element, "flipV") == Some("1"),
                uses_palette: attr(element, "usesPalette") == Some("1"),
            };
            put(&mut assets.list, name, asset);
        }
        for (key, value) in &image_sources.entries {
            if let Some(at) = assets.position(key) {
                if assets.list[at].1.source.as_deref().is_none_or(str::is_empty) {
                    assets.list[at].1.source = Some(value.clone());
                }
            }
        }
        assets
    }

    /// BuildMappingsInMemoryAsync + UpdateAssetsWithSourceFromCsvLinesAsync: every tag's first name (the library
    /// prefix taken off, the library's binaries skipped) becomes the source of the assets named by that tag, the
    /// first one included (an asset becomes its own source). The first csv line is skipped (`.Skip(1)`).
    fn build_mappings(&mut self, csv: &Csv, image_sources: &CiMap) -> Vec<(String, String)> {
        let groups = csv.groups();
        let prefix = groups.iter().find(|(tag, _)| *tag == "0").and_then(|(_, names)| names.first().copied()).unwrap_or("");
        let mut lines: Vec<(&str, String)> = Vec::new();
        let mut source_map: Vec<(String, String)> = Vec::new();
        for (tag, names) in &groups {
            for name in names {
                let cleaned = remove_swf_prefix(name, prefix);
                // Regex "visualization|logic|index|assets|manifest$": only "manifest" is anchored.
                if ["visualization", "logic", "index", "assets"].iter().any(|w| cleaned.contains(w)) || cleaned.ends_with("manifest") {
                    continue;
                }
                if !source_map.iter().any(|(t, _)| t == tag) {
                    source_map.push((tag.to_string(), cleaned.clone()));
                }
                lines.push((tag, cleaned));
            }
        }
        for (tag, cleaned) in lines.into_iter().skip(1) {
            // line.Split(',') must give exactly two parts
            if cleaned.contains(',') {
                continue;
            }
            let name = cleaned.to_lowercase();
            let Some(at) = self.position(&name) else { continue };
            if let Some((_, source)) = source_map.iter().find(|(t, _)| t == tag) {
                let value = if self.position(source).is_some() { source.clone() } else { image_sources.get(&name).unwrap_or(source).to_string() };
                self.list[at].1.source = Some(value);
            }
        }
        source_map
    }

    /// ResolveSource: follows sources through other assets, stopping at a loop.
    fn resolve_source(&self, mut source: String) -> String {
        let mut visited = HashSet::new();
        while !source.is_empty() {
            let Some(next) = self.position(&source).and_then(|at| self.list[at].1.source.clone()).filter(|s| !s.is_empty()) else { break };
            if !visited.insert(source.clone()) || visited.contains(&next) {
                return source;
            }
            source = next;
        }
        source
    }

    fn json(&self) -> J {
        J::Obj(
            self.list
                .iter()
                .map(|(name, a)| {
                    let mut o = Obj::new();
                    o.opt("source", a.source.clone().map(J::Str));
                    o.push("x", int(a.x));
                    o.push("y", int(a.y));
                    // WhenWritingDefault: false is left out
                    o.opt("flipH", a.flip_h.then_some(J::Bool(true)));
                    o.opt("flipV", a.flip_v.then_some(J::Bool(true)));
                    o.opt("usesPalette", a.uses_palette.then_some(J::Bool(true)));
                    (name.clone(), o.json())
                })
                .collect(),
        )
    }
}

/// RemoveSwfPrefix: `^<prefix>_` taken off, ignoring case.
fn remove_swf_prefix(name: &str, prefix: &str) -> String {
    if name.is_empty() || prefix.is_empty() {
        return name.to_string();
    }
    let wanted = format!("{prefix}_");
    let mut rest = name.chars();
    for w in wanted.chars() {
        match rest.next() {
            Some(c) if c == w || c.to_lowercase().eq(w.to_lowercase()) => {}
            _ => return name.to_string(),
        }
    }
    rest.as_str().to_string()
}

// ---------------------------------------------------------------------------------------------------------------
// Palettes (PaletteExtractor, CompactRgbConverter)
// ---------------------------------------------------------------------------------------------------------------

struct Palette {
    id: i32,
    source: String,
    master: bool,
    tags: Vec<String>,
    breed: Option<i32>,
    color_tag: Option<i32>,
    color1: Option<String>,
    color2: Option<String>,
    rgb: Vec<[u8; 3]>,
}

impl Palette {
    fn json(self) -> J {
        let mut o = Obj::new();
        o.push("id", int(self.id));
        o.push("source", J::Str(self.source));
        o.push("master", J::Bool(self.master));
        o.push("tags", J::Arr(self.tags.into_iter().map(J::Str).collect()));
        o.opt("breed", self.breed.map(int));
        o.opt("colorTag", self.color_tag.map(int));
        o.opt("color1", self.color1.map(J::Str));
        o.opt("color2", self.color2.map(J::Str));
        o.push("rgb", J::RawArr(self.rgb.iter().map(|[r, g, b]| format!("[{r},{g},{b}]")).collect()));
        o.json()
    }
}

const DEFAULT_COLOR_HEX: &str = "999999";

/// PaletteExtractor.ExtractPalettes: each `<palette>` of the assets XML with its colour table (the binary named
/// `*_<source>`, RGB triplets); a palette whose binary is missing borrows the master's (or first) table; no palette
/// at all gives a grey one. In Dictionary order (a repeated id keeps its first place, with the last palette).
fn palettes(swf: &Swf, assets_xml: &str) -> Result<Vec<(i32, Palette)>> {
    let doc = parse_xml(assets_xml)?;
    let files = bin_files(swf);
    let mut all: Vec<Palette> = Vec::new();
    let mut by_id: Vec<(i32, usize)> = Vec::new();
    let mut deferred: Vec<usize> = Vec::new();
    let mut reference: Option<(Vec<[u8; 3]>, Option<String>)> = None;
    for element in children(doc.root_element(), "palette") {
        let id = parse_int_strict(attr(element, "id").unwrap_or("-1"), "The palette id")?;
        if id == -1 {
            continue;
        }
        let source = attr(element, "source").unwrap_or("unknown").to_string();
        let master = parse_bool(attr(element, "master"));
        let breed = attr(element, "breed").map(|v| parse_int_strict(v, "The palette breed")).transpose()?;
        let color_tag = attr(element, "colortag").map(|v| parse_int_strict(v, "The palette colortag")).transpose()?;
        // Directory.GetFiles("*_*.bin").FirstOrDefault(f => f.EndsWith("_<source>.bin")): ordinal, case-sensitive
        let ending = format!("_{source}.bin");
        let binary = files.iter().find(|(file, _)| file.contains('_') && file.ends_with(&ending)).map(|(_, b)| b);
        let mut palette = Palette {
            id,
            source,
            master,
            tags: attr(element, "tags").unwrap_or("").split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
            breed,
            color_tag,
            color1: attr(element, "color1").map(str::to_string),
            color2: attr(element, "color2").map(str::to_string),
            rgb: Vec::new(),
        };
        match binary {
            None => deferred.push(all.len()),
            Some(binary) => {
                palette.rgb = binary.data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                if reference.is_none() || master {
                    reference = Some((palette.rgb.clone(), palette.color1.clone()));
                }
            }
        }
        put(&mut by_id, id, all.len());
        all.push(palette);
    }
    for at in deferred {
        let palette = &mut all[at];
        let (rgb, color1) = match &reference {
            Some((rgb, color1)) => (rgb.clone(), color1.clone()),
            None => (vec![[0x99; 3]; 256], None),
        };
        palette.rgb = rgb;
        if palette.color1.as_deref().is_none_or(str::is_empty) {
            palette.color1 = Some(color1.unwrap_or_else(|| DEFAULT_COLOR_HEX.to_string()));
        }
    }
    let mut slots: Vec<Option<Palette>> = all.into_iter().map(Some).collect();
    let mut palettes: Vec<(i32, Palette)> = by_id.into_iter().map(|(id, at)| (id, slots[at].take().expect("one slot per id"))).collect();
    if palettes.is_empty() {
        palettes.push((
            0,
            Palette {
                id: 0,
                source: "default".into(),
                master: true,
                tags: Vec::new(),
                breed: None,
                color_tag: None,
                color1: Some(DEFAULT_COLOR_HEX.into()),
                color2: None,
                rgb: vec![[0x99; 3]; 256],
            },
        ));
    }
    Ok(palettes)
}

// ---------------------------------------------------------------------------------------------------------------
// Logic (LogicMapper, MapPlanetSystem, MapParticleSystem; the anonymous logic object of SWF_Pets_To_Nitro)
// ---------------------------------------------------------------------------------------------------------------

fn logic_json(root: &Node) -> J {
    let root = *root;
    let mut model = Obj::new();
    if let Some(element) = child(root, "model") {
        model.opt(
            "dimensions",
            child(element, "dimensions").map(|d| {
                // ParseFloatOrNull: NaN counts as missing
                let get = |name| attr_float(d, name).filter(|v| !v.is_nan());
                let mut o = Obj::new();
                o.push("x", float(get("x").unwrap_or(0.0)));
                o.push("y", float(get("y").unwrap_or(0.0)));
                o.push("z", fixed_decimal(get("z").unwrap_or(0.000001)));
                o.opt("centerZ", get("centerZ").map(float));
                o.json()
            }),
        );
        // MapDirections: no element → empty (still written); an element without valid ids → [0]
        let directions = match child(element, "directions") {
            None => Vec::new(),
            Some(d) => {
                let mut ids: Vec<i32> = children(d, "direction").filter_map(|e| attr_int(e, "id")).collect();
                if ids.is_empty() {
                    ids.push(0);
                }
                ids
            }
        };
        model.push("directions", J::Arr(directions.into_iter().map(int).collect()));
    }

    let mut logic = Obj::new();
    logic.push("model", model.json());
    logic.opt(
        "action",
        child(root, "action").map(|a| {
            let mut o = Obj::new();
            o.opt("link", attr(a, "link").filter(|l| !l.is_empty()).map(|l| J::Str(l.to_string())));
            o.opt("startState", attr_int(a, "startState").map(int));
            o.json()
        }),
    );
    logic.opt("maskType", child(root, "mask").and_then(|m| attr_str(m, "type")));
    logic.opt("credits", child(root, "credits").and_then(|c| attr_str(c, "value")));
    logic.opt(
        "soundSample",
        child(root, "sound").and_then(|s| child(s, "sample")).map(|s| {
            let mut o = Obj::new();
            o.push("id", int(attr_int(s, "id").unwrap_or(0)));
            o.push("noPitch", J::Bool(attr(s, "nopitch") == Some("true")));
            o.json()
        }),
    );
    let planets: Vec<J> = child(root, "planetsystem")
        .map(|p| {
            children(p, "object")
                .map(|o| {
                    let mut j = Obj::new();
                    j.opt("id", attr_int(o, "id").map(int));
                    j.opt("name", attr_str(o, "name"));
                    j.opt("parent", attr_str(o, "parent"));
                    j.opt("radius", attr_float(o, "radius").map(float));
                    j.opt("arcSpeed", attr_float(o, "arcspeed").map(float));
                    j.opt("arcOffset", attr_float(o, "arcoffset").map(float));
                    j.opt("blend", attr_int(o, "blend").map(int));
                    j.opt("height", attr_float(o, "height").map(float));
                    j.json()
                })
                .collect()
        })
        .unwrap_or_default();
    logic.opt("planetSystems", (!planets.is_empty()).then_some(J::Arr(planets)));
    let particles: Vec<J> = child(root, "particlesystems").map(|p| children(p, "particlesystem").filter_map(particle_system).collect()).unwrap_or_default();
    logic.opt("particleSystems", (!particles.is_empty()).then_some(J::Arr(particles)));
    let variables: Vec<J> = child(root, "customvars")
        .map(|c| children(c, "variable").filter_map(|v| attr(v, "name").filter(|n| !n.is_empty())).map(|n| J::Str(n.to_string())).collect())
        .unwrap_or_default();
    logic.opt("customVars", (!variables.is_empty()).then(|| J::Obj(vec![("variables".into(), J::Arr(variables))])));
    logic.json()
}

/// One particle system, or None when it has neither emitters nor a size (the C#'s Where).
fn particle_system(p: Node) -> Option<J> {
    let size = attr_int(p, "size");
    let emitters: Vec<J> = children(p, "emitter")
        .map(|e| {
            let mut o = Obj::new();
            o.opt("id", attr_int(e, "id").map(int));
            o.opt("name", attr_str(e, "name"));
            o.opt("spriteId", attr_int(e, "sprite_id").map(int));
            o.opt("maxNumParticles", attr_int(e, "max_num_particles").map(int));
            o.opt("particlesPerFrame", attr_int(e, "particles_per_frame").map(int));
            o.push("burstPulse", int(attr_int(e, "burst_pulse").unwrap_or(1)));
            o.opt("fuseTime", attr_int(e, "fuse_time").map(int));
            o.opt(
                "simulation",
                child(e, "simulation").map(|s| {
                    let mut j = Obj::new();
                    j.opt("force", attr_float(s, "force").map(float));
                    j.opt("direction", attr_float(s, "direction").map(float));
                    j.opt("gravity", attr_float(s, "gravity").map(float));
                    j.opt("airFriction", attr_float(s, "airfriction").map(float));
                    j.opt("shape", attr_str(s, "shape"));
                    j.opt("energy", attr_float(s, "energy").map(float));
                    j.json()
                }),
            );
            o.opt(
                "particles",
                child(e, "particles").map(|ps| {
                    J::Arr(
                        children(ps, "particle")
                            .map(|q| {
                                let mut j = Obj::new();
                                j.push("isEmitter", J::Bool(attr(q, "is_emitter") == Some("true")));
                                j.opt("lifeTime", attr_int(q, "lifetime").map(int));
                                j.push("fade", J::Bool(attr(q, "fade") == Some("true")));
                                let frames = children(q, "frame").filter_map(|f| attr(f, "name").filter(|n| !n.is_empty())).map(|n| J::Str(n.to_string())).collect();
                                j.push("frames", J::Arr(frames));
                                j.json()
                            })
                            .collect(),
                    )
                }),
            );
            o.json()
        })
        .collect();
    if emitters.is_empty() && size.is_none() {
        return None;
    }
    let mut o = Obj::new();
    o.opt("size", size.map(int));
    o.opt("canvasId", attr_int(p, "canvas_id").map(int));
    o.opt("offsetY", attr_float(p, "offset_y").map(float));
    o.opt("blend", attr_float(p, "blend").map(float));
    o.opt("bgColor", attr_str(p, "bg_color"));
    o.opt("emitters", (!emitters.is_empty()).then_some(J::Arr(emitters)));
    Some(o.json())
}

// ---------------------------------------------------------------------------------------------------------------
// Visualizations (VisualizationsMapper and its data structures)
// ---------------------------------------------------------------------------------------------------------------

struct Visualization {
    angle: i32,
    layer_count: i32,
    size: i32,
    layers: Vec<(i32, J)>,
    /// SortedDictionary: by id, each with its layers sorted by id.
    directions: BTreeMap<i32, BTreeMap<i32, J>>,
    /// None when there are none (the C# sets an empty dictionary to null).
    animations: Option<Vec<(i32, J)>>,
    colors: Vec<(i32, J)>,
    postures: Option<(Option<String>, Vec<J>)>,
    gestures: Option<Vec<J>>,
}

/// Every `<graphics>/<visualization>` (no size is excluded any more).
fn visualizations(root: &Node) -> Result<Vec<Visualization>> {
    children(*root, "graphics").flat_map(|g| children(g, "visualization")).map(Visualization::parse).collect()
}

impl Visualization {
    fn parse(xml: Node) -> Result<Visualization> {
        let mut layers = Vec::new();
        if let Some(element) = child(xml, "layers") {
            for layer in children(element, "layer") {
                if let Some(id) = attr_int(layer, "id") {
                    put(&mut layers, id, layer_json(layer));
                }
            }
        }
        let mut directions = BTreeMap::new();
        if let Some(element) = child(xml, "directions") {
            for direction in children(element, "direction") {
                if let Some(id) = attr_int(direction, "id") {
                    let mut layers = BTreeMap::new();
                    for layer in children(direction, "layer") {
                        if let Some(layer_id) = attr_int(layer, "id") {
                            layers.insert(layer_id, layer_json(layer));
                        }
                    }
                    directions.insert(id, layers);
                }
            }
        }
        let mut animations = Vec::new();
        if let Some(element) = child(xml, "animations") {
            for animation in children(element, "animation") {
                if let Some(id) = attr_int(animation, "id") {
                    put(&mut animations, id, animation_json(animation));
                }
            }
        }
        let mut colors = Vec::new();
        if let Some(element) = child(xml, "colors") {
            for color in children(element, "color") {
                if let Some(id) = attr_int(color, "id") {
                    let mut layers = Vec::new();
                    for layer in children(color, "colorLayer") {
                        if let Some(layer_id) = attr_int(layer, "id") {
                            // An invalid colour throws in the C#, which fails the whole conversion.
                            let value = match attr(layer, "color").filter(|c| !c.is_empty()) {
                                Some(hex) => parse_hex(hex)?,
                                None => 0,
                            };
                            put(&mut layers, layer_id, J::Obj(vec![("color".into(), int(value))]));
                        }
                    }
                    let layers = J::Obj(layers.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
                    put(&mut colors, id, J::Obj(vec![("layers".into(), layers)]));
                }
            }
        }
        let postures = child(xml, "postures").map(|p| (attr(p, "defaultPosture").map(str::to_string), children(p, "posture").map(id_and_animation).collect()));
        let gestures = child(xml, "gestures").map(|g| children(g, "gesture").map(id_and_animation).collect());
        Ok(Visualization {
            angle: attr_int(xml, "angle").unwrap_or(0),
            layer_count: attr_int(xml, "layerCount").unwrap_or(0),
            size: attr_int(xml, "size").unwrap_or(0),
            layers,
            directions,
            animations: (!animations.is_empty()).then_some(animations),
            colors,
            postures,
            gestures,
        })
    }

    fn animation_count(&self) -> usize {
        self.animations.as_ref().map_or(0, Vec::len)
    }

    /// IsVisualizationEmpty (through the getters that give null for empty collections).
    fn is_empty(&self) -> bool {
        self.layers.is_empty()
            && self.directions.is_empty()
            && self.animations.is_none()
            && self.colors.is_empty()
            && self.postures.as_ref().is_none_or(|p| p.1.is_empty())
            && self.gestures.as_ref().is_none_or(Vec::is_empty)
    }

    fn json(self) -> J {
        fn by_id(list: Vec<(i32, J)>) -> J {
            J::Obj(list.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        }
        let mut o = Obj::new();
        o.push("angle", int(self.angle));
        o.push("layerCount", int(self.layer_count));
        o.push("size", int(self.size));
        o.opt("layers", (!self.layers.is_empty()).then(|| by_id(self.layers)));
        if !self.directions.is_empty() {
            let directions = self
                .directions
                .into_iter()
                .map(|(id, layers)| (id.to_string(), J::Obj(vec![("layers".into(), J::Obj(layers.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))])))
                .collect();
            o.push("directions", J::Obj(directions));
        }
        o.opt("animations", self.animations.map(by_id));
        o.opt("colors", (!self.colors.is_empty()).then(|| by_id(self.colors)));
        if let Some((default, postures)) = self.postures.filter(|p| !p.1.is_empty()) {
            let mut p = Obj::new();
            p.opt("defaultPosture", default.map(J::Str));
            p.push("postures", J::Arr(postures));
            o.push("postures", p.json());
        }
        o.opt("gestures", self.gestures.filter(|g| !g.is_empty()).map(J::Arr));
        o.json()
    }
}

/// Layer: every attribute optional; "x"/"y" are int? with WhenWritingDefault, whose default is null, so 0 is kept.
fn layer_json(xml: Node) -> J {
    let mut o = Obj::new();
    o.opt("ink", attr_str(xml, "ink"));
    o.opt("alpha", attr_int(xml, "alpha").map(int));
    o.opt("z", attr_int(xml, "z").map(int));
    o.opt("x", attr_int(xml, "x").map(int));
    o.opt("y", attr_int(xml, "y").map(int));
    o.opt("tag", attr_str(xml, "tag"));
    o.opt("ignoreMouse", (attr(xml, "ignoreMouse") == Some("1")).then_some(J::Bool(true)));
    o.json()
}

/// Posture / Gesture.
fn id_and_animation(xml: Node) -> J {
    let mut o = Obj::new();
    o.opt("id", attr_str(xml, "id"));
    o.push("animationId", int(attr_int(xml, "animationId").unwrap_or(0)));
    o.json()
}

fn animation_json(xml: Node) -> J {
    let mut layers = Vec::new();
    for layer in children(xml, "animationLayer") {
        if let Some(id) = attr_int(layer, "id") {
            put(&mut layers, id, animation_layer_json(layer));
        }
    }
    let mut o = Obj::new();
    o.opt("transitionTo", attr_int(xml, "transitionTo").map(int));
    o.opt("transitionFrom", attr_int(xml, "transitionFrom").map(int));
    o.opt("immediateChangeFrom", attr_str(xml, "immediateChangeFrom"));
    o.opt("randomStart", (attr(xml, "randomStart") == Some("1")).then_some(J::Bool(true)));
    o.push("layers", J::Obj(layers.into_iter().map(|(k, v)| (k.to_string(), v)).collect()));
    o.json()
}

fn animation_layer_json(xml: Node) -> J {
    let sequences: Vec<(String, J)> = children(xml, "frameSequence")
        .enumerate()
        .map(|(i, sequence)| {
            let frames = children(sequence, "frame").enumerate().map(|(j, frame)| (j.to_string(), frame_json(frame))).collect();
            (i.to_string(), J::Obj(vec![("frames".into(), J::Obj(frames))]))
        })
        .collect();
    let mut o = Obj::new();
    o.opt("frameRepeat", attr_int(xml, "frameRepeat").map(int));
    o.opt("loopCount", attr_int(xml, "loopCount").map(int));
    o.opt("random", attr_int(xml, "random").map(int));
    o.opt("frameSequences", (!sequences.is_empty()).then_some(J::Obj(sequences)));
    o.json()
}

/// FrameConverter: id, then randomX, randomY, x, y and the offsets by direction when present.
fn frame_json(xml: Node) -> J {
    let mut o = Obj::new();
    o.push("id", int(attr_int(xml, "id").unwrap_or(0)));
    o.opt("randomX", attr_int(xml, "randomX").map(int));
    o.opt("randomY", attr_int(xml, "randomY").map(int));
    o.opt("x", attr_int(xml, "x").map(int));
    o.opt("y", attr_int(xml, "y").map(int));
    let mut offsets = Vec::new();
    if let Some(element) = child(xml, "offsets") {
        for offset in children(element, "offset") {
            if let Some(direction) = attr_int(offset, "direction") {
                let mut j = Obj::new();
                j.push("direction", int(direction));
                j.opt("x", attr_int(offset, "x").map(int));
                j.opt("y", attr_int(offset, "y").map(int));
                put(&mut offsets, direction, j.json());
            }
        }
    }
    if !offsets.is_empty() {
        o.push("offsets", J::Obj(offsets.into_iter().map(|(k, v)| (k.to_string(), v)).collect()));
    }
    o.json()
}

/// ShouldKeepSize32: a size-32 visualization is kept only when it has as many animations as the size-64 one and at
/// least 90 % as many assets.
fn should_keep_size_32(visualizations: Option<&[Visualization]>, assets: &Assets) -> bool {
    let Some(visualizations) = visualizations else { return false };
    let (Some(size32), Some(size64)) = (visualizations.iter().find(|v| v.size == 32), visualizations.iter().find(|v| v.size == 64)) else {
        return false;
    };
    if size32.animation_count() < size64.animation_count() {
        return false;
    }
    let assets32 = assets.list.iter().filter(|(k, _)| k.contains("_32_")).count();
    let assets64 = assets.list.iter().filter(|(k, _)| k.contains("_64_")).count();
    // (int)(assets64 * 0.9): a double, truncated
    !(assets64 > 0 && (assets32 as f64) < ((assets64 as f64 * 0.9) as i64) as f64)
}

// ---------------------------------------------------------------------------------------------------------------
// The sprite sheet (SpriteSheetMapper.GenerateSpriteSheet)
// ---------------------------------------------------------------------------------------------------------------

const FRAME_PADDING: u32 = 2;
const MAX_SHEET_SIZE: u32 = 14000;
const SHEET_ROWS: usize = 10;

/// The images in rows (ceil(count / 10) per row, each row as tall as its tallest image), 2 px apart with the edge
/// pixels repeated 1 px around each image; the frames keyed by their canonical names.
fn sprite_sheet(images: &[(String, &RgbaImage)], name: &str, canonical: &CiMap) -> Result<(RgbaImage, J)> {
    // An image of zero width or height cannot exist in ImageSharp: the C# never sees one.
    let images: Vec<&(String, &RgbaImage)> = images.iter().filter(|(_, i)| i.width() > 0 && i.height() > 0).collect();
    if images.is_empty() {
        bail!("{name}: the .swf has no images to put in a sprite sheet.");
    }
    let per_row = images.len().div_ceil(SHEET_ROWS);
    let rows: Vec<&[&(String, &RgbaImage)]> = images.chunks(per_row).collect();
    let mut width = 0u64;
    let mut stacked = 0u64;
    for row in &rows {
        let row_width = FRAME_PADDING as u64 + row.iter().map(|(_, i)| i.width() as u64 + FRAME_PADDING as u64).sum::<u64>();
        width = width.max(row_width);
        stacked += row.iter().map(|(_, i)| i.height() as u64).max().unwrap_or(0) + FRAME_PADDING as u64;
    }
    let height = FRAME_PADDING as u64 + stacked;
    if width > MAX_SHEET_SIZE as u64 || height > MAX_SHEET_SIZE as u64 {
        bail!("{name}: the sprite sheet would be {width}x{height}, more than {MAX_SHEET_SIZE}x{MAX_SHEET_SIZE}.");
    }
    let (width, height) = (width as u32, height as u32);
    let mut sheet = RgbaImage::new(width, height);
    let mut frames = Vec::new();
    let mut y = FRAME_PADDING;
    for row in rows {
        let mut x = FRAME_PADDING;
        let mut row_height = 0;
        for (key, image) in row {
            let short = clean_asset_name(key);
            let key = force_cf_upper(canonical.get(&short).unwrap_or(&short));
            draw(&mut sheet, image, x, y);
            let (w, h) = (image.width() as i32, image.height() as i32);
            let rect = |x: i32, y: i32| J::Obj(vec![("x".into(), int(x)), ("y".into(), int(y)), ("w".into(), int(w)), ("h".into(), int(h))]);
            let size = J::Obj(vec![("w".into(), int(w)), ("h".into(), int(h))]);
            let frame = J::Obj(vec![
                ("frame".into(), rect(x as i32, y as i32)),
                ("rotated".into(), J::Bool(false)),
                ("trimmed".into(), J::Bool(false)),
                ("spriteSourceSize".into(), rect(0, 0)),
                ("sourceSize".into(), size),
                ("pivot".into(), J::Obj(vec![("x".into(), float(0.5)), ("y".into(), float(0.5))])),
            ]);
            put(&mut frames, key, frame);
            x += image.width() + FRAME_PADDING;
            row_height = row_height.max(image.height());
        }
        y += row_height + FRAME_PADDING;
    }
    let meta = J::Obj(vec![
        ("image".into(), J::Str(format!("{name}.png"))),
        ("format".into(), J::Str("RGBA8888".into())),
        ("size".into(), J::Obj(vec![("w".into(), int(width as i32)), ("h".into(), int(height as i32))])),
        ("scale".into(), float(1.0)),
        ("converter".into(), J::Str("All-in-1-download-tool".into())),
    ]);
    Ok((sheet, J::Obj(vec![("frames".into(), J::Obj(frames)), ("meta".into(), meta)])))
}

/// DrawImage onto the transparent sheet (nothing overlaps, so source-over gives the source pixel; a fully transparent
/// one comes out as 0,0,0,0), then ExtrudeEdges, which copies the raw edge pixels one pixel out.
fn draw(sheet: &mut RgbaImage, image: &RgbaImage, x: u32, y: u32) {
    for (px, py, pixel) in image.enumerate_pixels() {
        let value = if pixel[3] == 0 { Rgba([0, 0, 0, 0]) } else { *pixel };
        sheet.put_pixel(x + px, y + py, value);
    }
    let (w, h) = (image.width(), image.height());
    for px in 0..w {
        sheet.put_pixel(x + px, y - 1, *image.get_pixel(px, 0));
        sheet.put_pixel(x + px, y + h, *image.get_pixel(px, h - 1));
    }
    for py in 0..h {
        sheet.put_pixel(x - 1, y + py, *image.get_pixel(0, py));
        sheet.put_pixel(x + w, y + py, *image.get_pixel(w - 1, py));
    }
    sheet.put_pixel(x - 1, y - 1, *image.get_pixel(0, 0));
    sheet.put_pixel(x + w, y - 1, *image.get_pixel(w - 1, 0));
    sheet.put_pixel(x - 1, y + h, *image.get_pixel(0, h - 1));
    sheet.put_pixel(x + w, y + h, *image.get_pixel(w - 1, h - 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swf::{Binary, Bitmap};

    fn bin(id: u16, name: &str, text: &str) -> Binary {
        Binary { id, name: Some(name.into()), data: text.as_bytes().to_vec() }
    }

    fn xml(text: &str) -> Document<'_> {
        parse_xml(text).unwrap()
    }

    fn compact(j: J) -> String {
        j.to_indented().replace(['\n', ' '], "")
    }

    #[test]
    fn json_writer_matches_system_text_json() {
        let j = J::Obj(vec![
            ("a".into(), J::Arr(vec![int(1), int(2)])),
            ("b".into(), J::Obj(vec![("c".into(), J::Obj(vec![])), ("d".into(), J::Arr(vec![]))])),
            ("rgb".into(), J::RawArr(vec!["[1,2,3]".into(), "[4,5,6]".into()])),
            ("s".into(), J::Str("a\"b<>&'+`\\\n\u{e9}\u{1}\u{7f}/".into())),
        ]);
        assert_eq!(
            j.to_indented(),
            "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": {},\n    \"d\": []\n  },\n  \"rgb\": [[1,2,3],[4,5,6]\n  ],\n  \"s\": \"a\\u0022b\\u003C\\u003E\\u0026\\u0027\\u002B\\u0060\\\\\\n\\u00E9\\u0001\\u007F/\"\n}"
        );
    }

    /// Measured with .NET 10: float.ToString("G") / System.Text.Json, and FloatToFixedDecimalConverter.
    #[test]
    fn floats_like_dotnet() {
        let g = |v: f32| match float(v) {
            J::Raw(r) | J::Str(r) => r,
            _ => unreachable!(),
        };
        for (v, s) in [
            (0.5, "0.5"),
            (1.0, "1"),
            (1e8, "100000000"),
            (123456789.0, "123456790"),
            (1e9, "1E+09"),
            (1.5e20, "1.5E+20"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (1e-5, "1E-05"),
            (0.000012345, "1.2345E-05"),
            (-2.75, "-2.75"),
            (-0.0, "-0"),
            (12345.678, "12345.678"),
            (3.4028235e38, "3.4028235E+38"),
            (f32::NAN, "NaN"),
        ] {
            assert_eq!(g(v), s, "{v}");
        }
        assert_eq!(fixed_decimal(1.0), J::Raw("1".into()));
        assert_eq!(fixed_decimal(0.000001), J::Raw("0.000001".into()));
        assert_eq!(fixed_decimal(0.00005), J::Raw("0.000050".into()));
        assert_eq!(fixed_decimal(0.25), J::Raw("0.25".into()));
    }

    #[test]
    fn dotnet_parsing() {
        assert_eq!(parse_int(Some(" 12")), Some(12));
        assert_eq!(parse_int(Some("+5")), Some(5));
        assert_eq!(parse_int(Some("0x1")), None);
        assert_eq!(parse_float(Some("1.")), Some(1.0));
        assert_eq!(parse_float(Some(" .5 ")), Some(0.5));
        assert_eq!(parse_float(Some("inf")), None);
        assert!(parse_float(Some("Infinity")).unwrap().is_infinite());
        assert_eq!(parse_float(Some("1,5")), None);
        assert!(parse_bool(Some(" TRUE ")) && !parse_bool(Some("1")));
        assert_eq!(parse_hex("0xFF").unwrap(), 255);
        assert_eq!(parse_hex("FFFFFFFF").unwrap(), -1);
        assert!(parse_hex(" ff").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(clean_asset_name("dog_dog_64_a_0_0"), "dog_64_a_0_0");
        assert_eq!(clean_asset_name("dog_64_a_0_0"), "dog_64_a_0_0");
        assert_eq!(clean_asset_name("dog_dogs_1"), "dog_dogs_1");
        assert_eq!(force_cf_upper("cf_gold_cfc_x_acf_y"), "CF_gold_CFC_x_acf_y");
        assert_eq!(remove_swf_prefix("Dog_dog_64", "dog"), "dog_64");
        assert_eq!(remove_swf_prefix("dogs_64", "dog"), "dogs_64");
        let mut files = vec!["1_x.bin", "10_x.bin", "a.png", "a_b.png", "B.png"];
        files.sort_by_key(|f| ntfs_key(f));
        assert_eq!(files, ["10_x.bin", "1_x.bin", "a.png", "a_b.png", "B.png"]);
    }

    fn swf(symbols: &[(u16, &str)], binaries: Vec<Binary>, bitmaps: &[(u16, u32)]) -> Swf {
        let names = |id: u16| symbols.iter().filter(|(i, _)| *i == id).map(|(_, n)| n.to_string()).collect();
        Swf {
            symbols: symbols.iter().map(|(i, n)| (*i, n.to_string())).collect(),
            binaries,
            bitmaps: bitmaps.iter().map(|&(id, w)| Bitmap { id, names: names(id), image: RgbaImage::from_pixel(w, 2, Rgba([id as u8, 0, 0, 255])) }).collect(),
        }
    }

    /// The asset sources the C# derives from the csv: a tag's first name is the source of every asset the tag names,
    /// itself included (lines after the first); a source attribute is followed through other assets.
    #[test]
    fn asset_sources() {
        let csv = Csv::from(&swf(&[(0, "dog"), (1, "dog_dog_64_a_0_0"), (2, "dog_dog_64_b_0_0"), (2, "dog_dog_64_b_2_0"), (3, "dog_dog_index")], vec![], &[]));
        let assets_xml = r#"<assets><asset name="dog_64_a_0_0" x="-3" y="4"/><asset name="dog_64_b_0_0" usesPalette="1"/><asset name="dog_64_b_2_0" flipH="1"/><asset name="dog_64_c" source="dog_64_b_2_0"/><asset name="dog_64_skip"/></assets>"#;
        let manifest_xml = r#"<manifest><library><assets><asset name="dog_64_a_0_0" mimeType="image/png"/><asset name="dog_64_b_0_0" mimeType="image/png"/><asset name="dog_64_b_2_0" mimeType="image/png"/></assets></library></manifest>"#;
        let (assets, source_map) = Assets::parse(assets_xml, manifest_xml, &csv.debug_mapping(), &csv);
        let names: Vec<_> = assets.list.iter().map(|(n, a)| (n.as_str(), a.source.as_deref())).collect();
        // The skipped first line is "0,dog" (the prefix is not taken off itself), so dog_64_a_0_0 still gets its name.
        assert_eq!(
            names,
            [("dog_64_a_0_0", Some("dog_64_a_0_0")), ("dog_64_b_0_0", Some("dog_64_b_0_0")), ("dog_64_b_2_0", Some("dog_64_b_0_0")), ("dog_64_c", Some("dog_64_b_0_0"))]
        );
        assert_eq!(source_map, [("0".to_string(), "dog".to_string()), ("1".into(), "dog_64_a_0_0".into()), ("2".into(), "dog_64_b_0_0".into())]);
        assert_eq!(
            compact(assets.json()),
            r#"{"dog_64_a_0_0":{"source":"dog_64_a_0_0","x":-3,"y":4},"dog_64_b_0_0":{"source":"dog_64_b_0_0","x":0,"y":0,"usesPalette":true},"dog_64_b_2_0":{"source":"dog_64_b_0_0","x":0,"y":0,"flipH":true},"dog_64_c":{"source":"dog_64_b_0_0","x":0,"y":0}}"#
        );
    }

    #[test]
    fn palettes_borrow_and_default() {
        let s = swf(&[(5, "dog_palette_1")], vec![bin(5, "dog_palette_1", "\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}")], &[]);
        let xml = r#"<assets><palette id="1" source="palette_1" color1="aa" tags="a,,b" breed="3"/><palette id="2" source="palette_2" master="True"/></assets>"#;
        let p = palettes(&s, xml).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].1.rgb, [[1, 2, 3], [4, 5, 6]]);
        assert_eq!(p[1].1.rgb, p[0].1.rgb);
        assert_eq!(p[1].1.color1.as_deref(), Some("aa"));
        let (_, p0) = p.into_iter().next().unwrap();
        assert_eq!(
            p0.json().to_indented(),
            "{\n  \"id\": 1,\n  \"source\": \"palette_1\",\n  \"master\": false,\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ],\n  \"breed\": 3,\n  \"color1\": \"aa\",\n  \"rgb\": [[1,2,3],[4,5,6]\n  ]\n}"
        );
        let none = palettes(&s, "<assets/>").unwrap();
        assert_eq!((none[0].0, none[0].1.source.as_str(), none[0].1.rgb.len()), (0, "default", 256));
        assert!(palettes(&s, r#"<assets><palette id="x"/></assets>"#).is_err());
    }

    #[test]
    fn logic_object() {
        let doc = xml(
            r#"<objectData><model><dimensions x="1" y="1"/><directions/></model><action link="l"/><sound><sample id="7" nopitch="true"/></sound><particlesystems><particlesystem canvas_id="1"/></particlesystems><customvars><variable name="v"/></customvars></objectData>"#,
        );
        assert_eq!(
            compact(logic_json(&doc.root_element())),
            r#"{"model":{"dimensions":{"x":1,"y":1,"z":0.000001},"directions":[0]},"action":{"link":"l"},"soundSample":{"id":7,"noPitch":true},"customVars":{"variables":["v"]}}"#
        );
        let doc = xml("<objectData><model/></objectData>");
        assert_eq!(compact(logic_json(&doc.root_element())), r#"{"model":{"directions":[]}}"#);
    }

    #[test]
    fn visualization_mapping() {
        let doc = xml(
            r#"<visualizationData><graphics><visualization size="64" layerCount="2" angle="45">
            <layers><layer id="1" tag="head" x="0"/><layer id="0" ignoreMouse="1"/></layers>
            <directions><direction id="4"><layer id="1" z="2"/><layer id="0"/></direction><direction id="2"/></directions>
            <colors><color id="1"><colorLayer id="0" color="FF00FF"/></color></colors>
            <animations><animation id="3" randomStart="1"><animationLayer id="0" frameRepeat="2"><frameSequence><frame id="1" x="2"><offsets><offset direction="2" y="-1"/></offsets></frame></frameSequence></animationLayer><animationLayer id="1"/></animation></animations>
            <postures defaultPosture="std"><posture id="std" animationId="0"/></postures><gestures/>
            </visualization><visualization size="32"/></graphics></visualizationData>"#,
        );
        let list = visualizations(&doc.root_element()).unwrap();
        assert!(list[1].is_empty());
        assert_eq!(
            compact(list.into_iter().next().unwrap().json()),
            r#"{"angle":45,"layerCount":2,"size":64,"layers":{"1":{"x":0,"tag":"head"},"0":{"ignoreMouse":true}},"directions":{"2":{"layers":{}},"4":{"layers":{"0":{},"1":{"z":2}}}},"animations":{"3":{"randomStart":true,"layers":{"0":{"frameRepeat":2,"frameSequences":{"0":{"frames":{"0":{"id":1,"x":2,"offsets":{"2":{"direction":2,"y":-1}}}}}}},"1":{}}}},"colors":{"1":{"layers":{"0":{"color":16711935}}}},"postures":{"defaultPosture":"std","postures":[{"id":"std","animationId":0}]}}"#
        );
        let bad = xml(r#"<v><graphics><visualization><colors><color id="1"><colorLayer id="0" color="zz"/></color></colors></visualization></graphics></v>"#);
        assert!(visualizations(&bad.root_element()).is_err());
    }

    /// 21 images give 3 per row; the canonical name restores the doubled prefix; edges are extruded.
    #[test]
    fn sheet_layout() {
        let images: Vec<RgbaImage> = (0..21).map(|i| RgbaImage::from_pixel(3 + i % 2, 2, Rgba([i as u8, 1, 2, 255]))).collect();
        let named: Vec<(String, &RgbaImage)> = images.iter().enumerate().map(|(i, im)| (format!("dog_dog_64_{i:02}"), im)).collect();
        let mut canonical = CiMap::default();
        canonical.set("dog_64_00".into(), "dog_dog_64_00".into());
        let (sheet, data) = sprite_sheet(&named, "dog", &canonical).unwrap();
        // rows of 3: the widest is 2 + (4+2) + (3+2) + (4+2) = 19; 7 rows of 2 + 2, plus 2
        assert_eq!((sheet.width(), sheet.height()), (19, 30));
        let J::Obj(top) = data else { panic!() };
        let J::Obj(frames) = &top[0].1 else { panic!() };
        assert_eq!(frames[0].0, "dog_dog_64_00");
        assert_eq!(frames[1].0, "dog_64_01");
        assert_eq!(sheet.get_pixel(1, 1).0, [0, 1, 2, 255]);
        assert_eq!(sheet.get_pixel(5, 4).0, [0, 1, 2, 255]);
        assert_eq!(sheet.get_pixel(6, 0).0, [0, 0, 0, 0]);
        assert_eq!(sheet.get_pixel(6, 4).0, [1, 1, 2, 255], "the next image's extruded corner");
        assert_eq!(sheet.get_pixel(7, 2).0, [1, 1, 2, 255]);
    }

    /// A whole (tiny) pet: images named by their first csv name, size 32 dropped without a size-32 visualization.
    #[test]
    fn small_pet() {
        let s = swf(
            &[
                (0, "dog"),
                (1, "dog_dog_64_a_0_0"),
                (2, "dog_dog_32_a_0_0"),
                (3, "dog_dog_64_b_0_0"),
                (3, "dog_dog_64_b_2_0"),
                (4, "sh_x"),
                (10, "dog_index"),
                (11, "dog_dog_assets"),
                (12, "dog_manifest"),
                (13, "dog_dog_logic"),
                (14, "dog_dog_visualization"),
            ],
            vec![
                bin(10, "dog_index", r#"<object type="dog" logic="pet" visualization="pet_animated"/>"#),
                bin(11, "dog_dog_assets", r#"<assets><asset name="dog_64_a_0_0"/><asset name="dog_32_a_0_0"/><asset name="dog_64_b_0_0"/><asset name="dog_64_b_2_0" flipH="1"/></assets>"#),
                bin(
                    12,
                    "dog_manifest",
                    r#"<manifest><library><assets><asset name="dog_64_a_0_0" mimeType="image/png"/><asset name="dog_32_a_0_0" mimeType="image/png"/><asset name="dog_64_b_0_0" mimeType="image/png"/><asset name="dog_64_b_2_0" mimeType="image/png"/></assets></library></manifest>"#,
                ),
                bin(13, "dog_dog_logic", r#"<objectData><model><dimensions x="1" y="1" z="1"/><directions><direction id="2"/></directions></model></objectData>"#),
                bin(14, "dog_dog_visualization", r#"<visualizationData><graphics><visualization size="64" layerCount="1" angle="45"><layers><layer id="0" tag="body"/></layers></visualization></graphics></visualizationData>"#),
            ],
            &[(1, 3), (2, 2), (3, 4), (4, 1), (5, 1)],
        );
        let files = convert("dog", &s).unwrap();
        assert_eq!(files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["dog.json", "dog.png"]);
        let json: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
        assert_eq!(json["type"], "pet");
        assert_eq!(json["name"], "dog");
        assert!(json["assets"].get("dog_32_a_0_0").is_none());
        assert_eq!(json["assets"]["dog_64_b_2_0"]["source"], "dog_64_b_0_0");
        let frames: Vec<&String> = json["spritesheet"]["frames"].as_object().unwrap().keys().collect();
        // images/: dog_dog_64_a_0_0.png and dog_dog_64_b_0_0.png (sh_x skipped, size 32 dropped, id 5 has no name)
        assert_eq!(frames, ["dog_dog_64_a_0_0", "dog_dog_64_b_0_0"]);
        assert_eq!(json["palettes"]["0"]["source"], "default");
        assert_eq!(json["logic"]["model"]["dimensions"]["z"], 1);
        let text = String::from_utf8(files[0].1.clone()).unwrap();
        assert!(
            text.starts_with("{\n  \"type\": \"pet\",\n  \"name\": \"dog\",\n  \"logicType\": \"pet\",\n  \"visualizationType\": \"pet_animated\",\n  \"assets\": { \"dog_64_a_0_0\": {   \"source\": \"dog_64_a_0_0\","),
            "{text}"
        );
        let sheet = image::load_from_memory(&files[1].1).unwrap();
        // two images: one per row (ceil(2 / 10))
        assert_eq!((sheet.width(), sheet.height()), (2 + 4 + 2, 2 + 4 + 4));
    }

    /// The converter's repository next to this one (its samples are not part of this repository), or None.
    fn converter_folder(folder: &str) -> Option<std::path::PathBuf> {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().map(|a| a.join("duckietm/all-in-1-converter").join(folder)).find(|p| p.is_dir())
    }

    /// Every sample pet converts to a sane bundle. With AIO_PETS_REFERENCE set to a folder of the C#'s own output
    /// (`<pet>/<pet>.json` and `<pet>/<pet>.png`), the files must also be the same, the JSON byte for byte and the
    /// sheet pixel for pixel.
    #[test]
    fn real_pet_swfs_when_present() {
        let Some(folder) = converter_folder("Compiled/SWFCompiler/import/pets") else { return };
        let mut paths: Vec<_> = std::fs::read_dir(&folder).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "swf")).collect();
        paths.sort();
        let reference = std::env::var_os("AIO_PETS_REFERENCE").map(std::path::PathBuf::from);
        let (mut total_swf, mut total_json, mut total_png, mut total_hab, mut total_frames) = (0, 0, 0, 0, 0);
        for path in &paths {
            let name = path.file_stem().unwrap().to_str().unwrap();
            let bytes = std::fs::read(path).unwrap();
            let swf = crate::swf::read(&bytes, true).unwrap();
            let files = convert(name, &swf).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(files.len(), 2);
            let json: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap_or_else(|e| panic!("{name}: {e}"));
            let sheet = image::load_from_memory(&files[1].1).unwrap();
            let meta = &json["spritesheet"]["meta"];
            assert_eq!(meta["image"], format!("{name}.png"));
            assert_eq!((meta["size"]["w"].as_u64().unwrap(), meta["size"]["h"].as_u64().unwrap()), (sheet.width() as u64, sheet.height() as u64), "{name}");
            let frames = json["spritesheet"]["frames"].as_object().unwrap();
            assert!(!frames.is_empty());
            for (key, frame) in frames {
                let f = &frame["frame"];
                let (x, y, w, h) = (f["x"].as_u64().unwrap(), f["y"].as_u64().unwrap(), f["w"].as_u64().unwrap(), f["h"].as_u64().unwrap());
                assert!(x >= 1 && y >= 1 && x + w < sheet.width() as u64 && y + h < sheet.height() as u64, "{name}: {key}");
            }
            // Nitro looks an asset's image up as "<library>_<source or asset name>"; a source may also be an asset.
            let library = json["name"].as_str().unwrap();
            let assets = json["assets"].as_object().unwrap();
            for (key, asset) in assets {
                let image = asset["source"].as_str().unwrap_or(key);
                let found = frames.contains_key(image) || frames.contains_key(&format!("{library}_{image}")) || (image != key && assets.contains_key(image));
                assert!(found, "{name}: asset {key} has no image ({image})");
            }
            let hab = crate::hab::write(name, &files);
            assert_eq!(crate::hab::read(&hab).unwrap(), files);
            if let Some(reference) = &reference {
                let dir = reference.join(name);
                let theirs = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
                assert!(String::from_utf8_lossy(&files[0].1) == theirs, "{name}.json differs");
                let theirs = image::open(dir.join(format!("{name}.png"))).unwrap().to_rgba8();
                assert!(theirs == sheet.to_rgba8(), "{name}.png differs");
            }
            total_swf += bytes.len();
            total_json += files[0].1.len();
            total_png += files[1].1.len();
            total_hab += hab.len();
            total_frames += frames.len();
        }
        println!(
            "{} pets ({} KB of .swf): {} frames, {} KB json, {} KB png, {} KB of .hab{}",
            paths.len(),
            total_swf / 1024,
            total_frames,
            total_json / 1024,
            total_png / 1024,
            total_hab / 1024,
            if reference.is_some() { ", identical to the C# output" } else { "" }
        );
        assert!(!paths.is_empty());
    }
}
