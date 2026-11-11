//! The assets of the furni (Mapper/Assests: AssetsMapper, AssetNameMapper, DebugXmlParser): every asset of the
//! assets XML that the manifest lists as a PNG or that has a `source`, the sources the symbol aliases give, and the
//! image mapping (`LatestImageMapping`) the image restorer names the left-over bitmaps with.

use std::collections::HashMap;

use anyhow::Result;

use super::json::{Dict, Json, Obj};
use super::names::{self, csv_entries, ends_with_ci, force_cf_upper, is_null_or_empty, parse_int};
use super::xml;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Asset {
    pub source: Option<String>,
    pub x: i32,
    pub y: i32,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Asset {
    /// source (left out when null), x, y, flipH and flipV (left out when false: WhenWritingDefault).
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.opt("source", self.source.as_deref().map(Json::str));
        o.put("x", Json::int(self.x)).put("y", Json::int(self.y));
        o.opt("flipH", self.flip_h.then(|| Json::bool(true)));
        o.opt("flipV", self.flip_v.then(|| Json::bool(true)));
        o.build()
    }
}

pub type Assets = Dict<String, Asset>;

/// DebugXmlParser.ParseDebugXml: for every CSV tag with at least two names, the second name (lower-cased) maps to
/// the first (lower-cased). The result is an OrdinalIgnoreCase dictionary in insertion order: (key, value) pairs.
pub fn debug_mapping(lines: &[String]) -> Vec<(String, String)> {
    // GroupBy keeps the order in which each tag first appears.
    let mut groups: Dict<String, Vec<String>> = Dict::new();
    for (tag, name) in csv_entries(lines) {
        match groups.get_mut(&tag) {
            Some(list) => list.push(name),
            None => groups.insert(tag, vec![name]),
        }
    }
    let mut mapping: Dict<String, (String, String)> = Dict::new();
    for (_, list) in &groups.items {
        if list.len() >= 2 {
            let source = names::to_lower(&list[0]);
            let object = names::to_lower(&list[1]);
            let key = names::fold(&object);
            // `mapping[obj] = source` keeps the first key on a case-insensitive hit.
            let kept = mapping.get(&key).map(|(k, _)| k.clone()).unwrap_or(object);
            mapping.insert(key, (kept, source));
        }
    }
    mapping.items.into_iter().map(|(_, pair)| pair).collect()
}

/// DebugXmlParser.ExtractSymbolClassTags: every CSV tag (as text, "0" included) with its names, in order.
fn symbol_class_tags(lines: &[String]) -> Dict<String, Vec<String>> {
    let mut tags: Dict<String, Vec<String>> = Dict::new();
    for (tag, name) in csv_entries(lines) {
        match tags.get_mut(&tag) {
            Some(list) => list.push(name),
            None => tags.insert(tag, vec![name]),
        }
    }
    tags
}

/// AssetNameMapper.BuildCanonicalMapping: cleaned short name -> full lower-cased symbol name, for every symbol
/// (id not 0) whose name mentions none of the library's XML files. Keyed case-insensitively (the folded key maps
/// to the full name).
pub fn canonical_mapping(lines: &[String]) -> HashMap<String, String> {
    let mut mapping = HashMap::new();
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() < 2 {
            continue;
        }
        match parse_int(Some(parts[0].trim())) {
            Some(id) if id != 0 => {}
            _ => continue,
        }
        let name = parts[1].trim().replace('"', "");
        let lower = names::to_lower(&name);
        if ["manifest", "assets", "logic", "visualization", "index"].iter().any(|w| lower.contains(w)) {
            continue;
        }
        let short = names::clean_asset_name(&lower);
        mapping.entry(names::fold(&short)).or_insert(lower);
    }
    mapping
}

/// AssetsMapper.MapAssetsXML.
fn map_assets_xml(root: roxmltree::Node, manifest: roxmltree::Node, lines: &[String]) -> Assets {
    let manifest_names: std::collections::HashSet<String> = manifest
        .descendants()
        .filter(|n| n != &manifest && xml::is(n, "asset") && n.attribute("mimeType") == Some("image/png"))
        .map(|n| force_cf_upper(n.attribute("name").unwrap_or("")))
        .collect();
    let mut output = Assets::new();
    for element in xml::elements(root, "asset") {
        let name = force_cf_upper(element.attribute("name").unwrap_or(""));
        if !manifest_names.contains(&name) && element.attribute("source").is_none() {
            continue;
        }
        output.insert(
            name,
            Asset {
                x: parse_int(element.attribute("x")).unwrap_or(0),
                y: parse_int(element.attribute("y")).unwrap_or(0),
                flip_h: element.attribute("flipH") == Some("1"),
                flip_v: element.attribute("flipV") == Some("1"),
                source: element.attribute("source").map(force_cf_upper),
            },
        );
    }
    for (key, value) in debug_mapping(lines) {
        let (key, value) = (force_cf_upper(&key), force_cf_upper(&value));
        if let Some(asset) = output.get_mut(&key) {
            if is_null_or_empty(&asset.source) {
                asset.source = Some(value);
            }
        }
    }
    output
}

/// AssetsMapper.BuildMappingsInMemoryAsync: the sources the symbol aliases give (the second and later names of a
/// character point at its first name, the SWF prefix taken off) and the image mapping: character id -> its first
/// name that is not one of the XML files (with "cf_"/"cfc_" upper-cased).
fn build_mappings(assets: &mut Assets, lines: &[String]) -> HashMap<String, String> {
    let tags = symbol_class_tags(lines);
    let prefix = tags.get(&"0".to_string()).and_then(|n| n.first().cloned()).unwrap_or_default();
    let mut mapping_lines = Vec::new();
    let mut image_mapping = HashMap::new();
    for (tag, tag_names) in &tags.items {
        if tag == "0" {
            continue;
        }
        for original in tag_names {
            let cleaned = force_cf_upper(&names::remove_swf_prefix(original, &prefix));
            if ["visualization", "logic", "index", "assets", "manifest"].iter().any(|s| ends_with_ci(&cleaned, s)) {
                continue;
            }
            mapping_lines.push(format!("{tag},{cleaned}"));
            image_mapping.entry(tag.clone()).or_insert_with(|| force_cf_upper(original));
        }
    }
    // UpdateAssetsWithSourceFromCsvLinesAsync: "id,name" lines (a name with ',' does not split in two: skipped).
    let mut source_map: HashMap<String, String> = HashMap::new();
    for line in &mapping_lines {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 2 {
            continue;
        }
        let name = force_cf_upper(parts[1]);
        match source_map.get(parts[0]) {
            Some(source) => {
                if let Some(asset) = assets.get_mut(&name) {
                    asset.source = Some(source.clone());
                }
            }
            None => {
                source_map.insert(parts[0].to_string(), name);
            }
        }
    }
    image_mapping
}

/// AssetsMapper.ResolveSource: follows a chain of sources to its end (stopping on a loop).
fn resolve_source(assets: &Assets, mut source: String) -> String {
    let mut visited = std::collections::HashSet::new();
    while !source.is_empty() {
        let Some(next) = assets.get(&source).and_then(|a| a.source.clone()).filter(|s| !s.is_empty()) else { break };
        if !visited.insert(source.clone()) {
            break;
        }
        source = next;
    }
    source
}

/// What the assets step leaves: the assets for the JSON (None without an assets or a manifest XML: the property is
/// then left out) and the image mapping (empty unless the step got as far as building it).
pub struct AssetsResult {
    pub assets: Option<Assets>,
    pub image_mapping: HashMap<String, String>,
}

/// AssetsMapper.ParseAssetsFileAsync (from GetAssetDataAsync, which needs both files). An XML that does not parse
/// is caught there: no assets ({}), and no image mapping.
pub fn parse(assets_xml: Option<&str>, manifest_xml: Option<&str>, lines: &[String]) -> AssetsResult {
    let (Some(assets_xml), Some(manifest_xml)) = (assets_xml, manifest_xml) else {
        return AssetsResult { assets: None, image_mapping: HashMap::new() };
    };
    let parsed: Result<(roxmltree::Document, roxmltree::Document)> = (|| Ok((xml::parse(assets_xml)?, xml::parse(manifest_xml)?)))();
    let Ok((root, manifest)) = parsed else {
        return AssetsResult { assets: Some(Assets::new()), image_mapping: HashMap::new() };
    };
    let mut assets = map_assets_xml(root.root_element(), manifest.root_element(), lines);
    let image_mapping = build_mappings(&mut assets, lines);
    // In order, each seeing the sources already resolved before it.
    for i in 0..assets.items.len() {
        if let Some(source) = assets.items[i].1.source.clone().filter(|s| !s.is_empty()) {
            let resolved = resolve_source(&assets, source);
            assets.items[i].1.source = Some(resolved);
        }
    }
    AssetsResult { assets: Some(assets), image_mapping }
}

pub fn to_json(assets: &Assets) -> Json {
    Json::Obj(assets.items.iter().map(|(k, a)| (k.clone(), a.to_json())).collect())
}
