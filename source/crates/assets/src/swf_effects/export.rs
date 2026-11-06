//! The C# converter works on an export folder: NativeSwfExtractor (or FFDEC) writes `symbolClass/symbols.csv`,
//! `binaryData/<id>_<symbol>.bin` and `images/<id>_<symbol>.png`, then FfdecExtractorEffects.RebuildImagesFromCsvAsync
//! and ImageRestorer rename the images by moving files between `images/` and `tmp/`, and the converter reads the
//! folders back by `Directory.GetFiles`. That folder dance decides which images make the sheet, under which names
//! and in which order, so it is replayed here in memory, with the folders behaving as on Windows (NTFS), where the
//! converter runs: names are case-insensitive, a move onto an existing name fails (or replaces it, with overwrite),
//! and a listing comes back in NTFS collation order (names compared upper-cased, UTF-16 unit by unit).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::swf::Swf;

/// NTFS's case folding of one name: every character upper-cased (when it upper-cases to one character).
pub fn upcase(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

/// One simulated folder of a Windows file system.
pub struct Folder<T> {
    files: Vec<(String, T)>,
}

impl<T> Folder<T> {
    pub fn new() -> Self {
        Folder { files: Vec::new() }
    }

    fn position(&self, name: &str) -> Option<usize> {
        let key = upcase(name);
        self.files.iter().position(|(n, _)| upcase(n) == key)
    }

    /// File.WriteAllBytes / SaveAsPng: a new file, or the existing one (of any case) overwritten, keeping its name.
    pub fn create(&mut self, name: String, data: T) {
        match self.position(&name) {
            Some(i) => self.files[i].1 = data,
            None => self.files.push((name, data)),
        }
    }

    /// File.Move without overwrite: fails (giving the data back) when the name exists in any case.
    pub fn add_new(&mut self, name: String, data: T) -> Result<(), T> {
        if self.position(&name).is_some() {
            return Err(data);
        }
        self.files.push((name, data));
        Ok(())
    }

    /// File.Move with overwrite: an existing file of that name (any case) is replaced, the new name is kept.
    pub fn add_replacing(&mut self, name: String, data: T) {
        if let Some(i) = self.position(&name) {
            self.files.remove(i);
        }
        self.files.push((name, data));
    }

    pub fn take(&mut self, name: &str) -> Option<(String, T)> {
        self.position(name).map(|i| self.files.remove(i))
    }

    /// Every file in `Directory.GetFiles` order on NTFS.
    pub fn listing(&self) -> Vec<(&str, &T)> {
        let mut files: Vec<(&str, &T)> = self.files.iter().map(|(n, d)| (n.as_str(), d)).collect();
        files.sort_by_cached_key(|(n, _)| upcase(n).encode_utf16().collect::<Vec<u16>>());
        files
    }

    pub fn drain(&mut self) -> Vec<(String, T)> {
        std::mem::take(&mut self.files)
    }
}

/// .NET `int.TryParse(s)` (NumberStyles.Integer): white space around, an optional sign, ASCII digits, in range.
pub fn parse_int(s: &str) -> Option<i32> {
    let s = s.trim_matches(|c: char| matches!(c, '\u{9}'..='\u{d}' | ' ')).trim_end_matches('\0');
    s.trim_end_matches(|c: char| matches!(c, '\u{9}'..='\u{d}' | ' ')).parse::<i32>().ok()
}

/// .NET `bool.TryParse(s)`: "true" or "false" in any case, white space (and NULs) around.
pub fn parse_bool(s: &str) -> Option<bool> {
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if s.eq_ignore_ascii_case("true") {
        Some(true)
    } else if s.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// .NET `ToLowerInvariant`, character by character.
pub fn lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

/// `File.ReadAllLines` of the symbols.csv NativeSwfExtractor writes (`<id>;"<name>"` per symbol, in file order):
/// lines end at "\r\n", "\n" or "\r".
pub fn symbols_csv_lines(swf: &Swf) -> Vec<String> {
    let mut text = String::new();
    for (id, name) in &swf.symbols {
        text.push_str(&format!("{id};\"{name}\"\n"));
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut line));
            }
            _ => line.push(c),
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The (tag, name) of every non-blank line with a `;`, as DebugXmlParser and EffectXMLParser read the CSV: both
/// trimmed, quotes removed from the name.
pub fn csv_entries(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let parts: Vec<&str> = l.split(';').collect();
            (parts.len() >= 2).then(|| (parts[0].trim().to_string(), parts[1].trim().replace('"', "")))
        })
        .collect()
}

/// LINQ `GroupBy`: the groups in order of their first element, the elements in order.
pub fn group_by<K: Eq + std::hash::Hash + Clone, V>(items: impl IntoIterator<Item = (K, V)>) -> Vec<(K, Vec<V>)> {
    let mut groups: Vec<(K, Vec<V>)> = Vec::new();
    let mut at: HashMap<K, usize> = HashMap::new();
    for (k, v) in items {
        match at.get(&k) {
            Some(&i) => groups[i].1.push(v),
            None => {
                at.insert(k.clone(), groups.len());
                groups.push((k, vec![v]));
            }
        }
    }
    groups
}

/// EffectXMLParser.GetEffectsImageMapping (ParseSymbolsCsv): per tag, its second name (lower-cased) maps to its
/// first; a tag with one name maps it to itself. An OrdinalIgnoreCase dictionary: keyed here by `upcase`.
pub fn effects_image_mapping(lines: &[String]) -> HashMap<String, String> {
    let mut mapping = HashMap::new();
    for (_, names) in group_by(csv_entries(lines)) {
        if names.len() >= 2 {
            mapping.insert(upcase(&lower(&names[1])), lower(&names[0]));
        } else {
            mapping.insert(upcase(&lower(&names[0])), lower(&names[0]));
        }
    }
    mapping
}

/// DebugXmlParser.ExtractSymbolClassTags: every tag with its names, in order of first appearance.
pub fn symbol_class_tags(lines: &[String]) -> Vec<(String, Vec<String>)> {
    group_by(csv_entries(lines))
}

/// One line of FfdecExtractorEffects.ParseCsv: id (not 0), name, and whether its FFDEC comment calls it the source.
struct CsvMapping {
    id: i32,
    name: String,
    source: bool,
}

fn ffdec_csv(lines: &[String]) -> Vec<CsvMapping> {
    let mut result = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() < 2 {
            continue;
        }
        let Some(id) = parse_int(parts[0].trim()) else { continue };
        if id == 0 {
            continue;
        }
        let name_part = parts[1].trim().replace('"', "");
        let (name, comment) = match name_part.find(" <=") {
            Some(i) => (name_part[..i].trim().to_string(), name_part[i + 3..].trim().to_string()),
            None => (name_part.clone(), String::new()),
        };
        // ToLower (current culture): ASCII is what matters for these words.
        let comment = lower(&comment);
        if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|w| comment.contains(w)) {
            continue;
        }
        result.push(CsvMapping { id, name, source: comment.contains("source") });
    }
    result
}

/// `Path.GetFileNameWithoutExtension`.
pub fn stem(file: &str) -> &str {
    match file.rfind('.') {
        Some(i) => &file[..i],
        None => file,
    }
}

/// The export as the converter reads it back.
pub struct Export<'a> {
    pub csv_lines: Vec<String>,
    /// `binaryData/`: file name, content.
    pub binaries: Folder<&'a [u8]>,
    /// LoadImages: (name without .png, image) in `images/` listing order.
    pub images: Vec<(String, &'a image::RgbaImage)>,
}

/// Replays the export and the renames (FfdecExtractorEffects.ExtractSWFAsync, ImageRestorer.RestoreImagesFromTmpAsync
/// with GetEffectsImageMapping, LoadImages).
pub fn export(swf: &Swf) -> Result<Export<'_>> {
    let csv_lines = symbols_csv_lines(swf);

    let mut binaries = Folder::new();
    for binary in &swf.binaries {
        binaries.create(format!("{}.bin", binary.export_name()), binary.data.as_slice());
    }

    let mut images: Folder<&image::RgbaImage> = Folder::new();
    for bitmap in &swf.bitmaps {
        images.create(format!("{}.png", bitmap.export_name()), &bitmap.image);
    }

    // RebuildImagesFromCsvAsync: every PNG moves to tmp/images/, then each symbol id with an image moves back as
    // images/<its first name>.png (a "source" one when FFDEC's comments name one). A name already taken (in any case)
    // makes the move fail: that image stays in tmp/.
    let mut tmp = Folder::new();
    for (name, image) in images.drain() {
        tmp.create(name, image);
    }
    let mut lookup: HashMap<String, String> = HashMap::new();
    for (file, _) in tmp.listing() {
        // ToDictionary: a duplicate key throws, and the converter gives up on the file.
        if lookup.insert(stem(file).replace('"', ""), file.to_string()).is_some() {
            bail!("Two images of the .swf have the same export name ({}).", stem(file));
        }
    }
    for (id, mappings) in group_by(ffdec_csv(&csv_lines).into_iter().map(|m| (m.id, m))) {
        let key = if mappings.len() > 1 { id.to_string() } else { format!("{id}_{}", mappings[0].name) };
        let Some(file) = lookup.get(&key) else { continue };
        let source = if mappings.len() > 1 { mappings.iter().find(|m| m.source).unwrap_or(&mappings[0]) } else { &mappings[0] };
        let Some((file_name, image)) = tmp.take(file) else { continue };
        let extension = file_name.rfind('.').map_or("", |i| &file_name[i..]);
        if let Err(image) = images.add_new(format!("{}{extension}", source.name), image) {
            tmp.create(file_name, image);
        }
    }

    // ImageRestorer: an image left in tmp/ whose id (the name up to the first "_") is a symbol name of the mapping
    // moves back under the name it maps to, replacing any file of that name.
    let mapping = effects_image_mapping(&csv_lines);
    if !mapping.is_empty() {
        let order: Vec<String> = tmp.listing().into_iter().map(|(n, _)| n.to_string()).collect();
        for file in order {
            let id = stem(&file).split('_').next().unwrap_or("");
            let Some(correct) = mapping.get(&upcase(id)) else { continue };
            let extension = file.rfind('.').map_or("", |i| &file[i..]).to_string();
            let correct = format!("{correct}{extension}");
            if let Some((_, image)) = tmp.take(&file) {
                images.add_replacing(correct, image);
            }
        }
    }

    // LoadImages: images/*.png in listing order, by name without extension (the first of a name wins).
    let mut loaded: Vec<(String, &image::RgbaImage)> = Vec::new();
    for (file, image) in images.listing() {
        if !file.to_ascii_lowercase().ends_with(".png") {
            continue;
        }
        let name = stem(file).to_string();
        if !loaded.iter().any(|(n, _)| *n == name) {
            loaded.push((name, image));
        }
    }

    Ok(Export { csv_lines, binaries, images: loaded })
}

/// .NET's text decoding of `File.ReadAllText`: a UTF-8, UTF-16 or UTF-32 byte-order mark picks the encoding (and is
/// dropped), else UTF-8; invalid bytes become U+FFFD.
pub fn text(data: &[u8]) -> String {
    let utf16 = |data: &[u8], le: bool| {
        let units: Vec<u16> = data.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    let utf32 = |data: &[u8], le: bool| {
        data.chunks_exact(4)
            .map(|c| {
                let v = if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
                char::from_u32(v).unwrap_or('\u{FFFD}')
            })
            .collect()
    };
    if let Some(rest) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE, 0, 0]) {
        utf32(rest, true)
    } else if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, true)
    } else if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, false)
    } else if let Some(rest) = data.strip_prefix(&[0, 0, 0xFE, 0xFF]) {
        utf32(rest, false)
    } else {
        String::from_utf8_lossy(data).into_owned()
    }
}

/// The first binary whose file name matches a `Directory.GetFiles` pattern (case-insensitive on Windows), in listing
/// order.
pub fn first_binary<'a>(export: &Export<'a>, matches: impl Fn(&str) -> bool) -> Option<&'a [u8]> {
    export.binaries.listing().into_iter().find(|(name, _)| matches(&name.to_ascii_lowercase())).map(|(_, data)| *data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_parsing() {
        assert_eq!(parse_int(" -12 "), Some(-12));
        assert_eq!(parse_int("+3"), Some(3));
        assert_eq!(parse_int("1.5"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("99999999999"), None);
        assert_eq!(parse_bool(" TRUE "), Some(true));
        assert_eq!(parse_bool("1"), None);
    }

    #[test]
    fn folders_list_and_collide_like_ntfs() {
        let mut f = Folder::new();
        f.create("b.png".to_string(), 1);
        f.create("A_b.png".to_string(), 2);
        f.create("a.png".to_string(), 3);
        f.create("B.PNG".to_string(), 4);
        assert_eq!(f.listing(), [("a.png", &3), ("A_b.png", &2), ("b.png", &4)]);
        assert_eq!(f.add_new("A.png".to_string(), 5), Err(5));
        f.add_replacing("A.png".to_string(), 6);
        assert_eq!(f.listing()[0], ("A.png", &6));
    }
}
