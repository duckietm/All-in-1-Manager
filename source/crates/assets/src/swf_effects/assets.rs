//! The assets and aliases of the manifest, with each asset's `source` taken from the symbol table
//! (Mapper/Assets/EffectAssetsMapper.cs).

use serde::Serialize;
use serde_json::{Map, Value};

use super::export::{lower, parse_bool, parse_int, symbol_class_tags};

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Asset {
    pub x: i32,
    pub y: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Alias {
    pub link: String,
    #[serde(rename = "flipH")]
    pub flip_h: bool,
    #[serde(rename = "flipV")]
    pub flip_v: bool,
}

/// A .NET `Dictionary<string, T>` filled by `dict[key] = value`: insertion order, a repeated key keeps its place.
pub type Dict<T> = Vec<(String, T)>;

pub fn set<T>(dict: &mut Dict<T>, key: &str, value: T) {
    match dict.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value,
        None => dict.push((key.to_string(), value)),
    }
}

pub fn to_object<T: Serialize>(dict: &Dict<T>) -> Value {
    let mut map = Map::new();
    for (key, value) in dict {
        map.insert(key.clone(), serde_json::to_value(value).expect("plain data"));
    }
    Value::Object(map)
}

#[derive(Debug, Default)]
pub struct AssetData {
    pub assets: Dict<Asset>,
    pub aliases: Dict<Alias>,
}

/// `XElement.Element(name)` / `Elements(name)`: children of that name in no namespace.
pub fn children<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(move |c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace().is_none())
}

pub fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'static str) -> Option<roxmltree::Node<'a, 'i>> {
    children(node, name).next()
}

/// The XML options closest to `XElement.Parse`: no DTD (XmlReader prohibits it by default).
pub fn parse_xml(text: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse(text).ok()
}

/// ParseAssetsFileAsync: an unreadable manifest gives no assets and no aliases.
pub fn parse(manifest: &str, csv_lines: &[String]) -> AssetData {
    let Some(doc) = parse_xml(manifest) else { return AssetData::default() };
    let root = doc.root_element();
    let mut assets = map_assets(root);
    build_sources(&mut assets, csv_lines);
    AssetData { assets, aliases: map_aliases(root) }
}

/// MapAssetsFromManifest: every image/png asset with an "x,y" offset param (the first `offset` param counts).
fn map_assets(root: roxmltree::Node) -> Dict<Asset> {
    let mut output = Dict::new();
    let Some(assets) = child(root, "library").and_then(|l| child(l, "assets")) else { return output };
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
        let (Some(x), Some(y)) = (parse_int(parts[0].trim()), parse_int(parts[1].trim())) else { continue };
        set(&mut output, name, Asset { x, y, source: None });
    }
    output
}

/// MapAliasesFromManifest: every alias with a name and a link; fliph/flipv as a bool, or an integer (not 0 = true).
fn map_aliases(root: roxmltree::Node) -> Dict<Alias> {
    let mut output = Dict::new();
    let Some(aliases) = child(root, "library").and_then(|l| child(l, "aliases")) else { return output };
    let flag = |value: Option<&str>| match value.and_then(parse_bool) {
        Some(b) => b,
        None => value.and_then(parse_int).is_some_and(|i| i != 0),
    };
    for alias in children(aliases, "alias") {
        let Some(name) = alias.attribute("name").filter(|n| !n.is_empty()) else { continue };
        let Some(link) = alias.attribute("link").filter(|l| !l.is_empty()) else { continue };
        set(&mut output, name, Alias { link: link.to_string(), flip_h: flag(alias.attribute("fliph")), flip_v: flag(alias.attribute("flipv")) });
    }
    output
}

/// RemoveSwfPrefix: `^<prefix>_` removed, ignoring case (Regex IgnoreCase compares lower-cased characters).
pub fn remove_swf_prefix(name: &str, prefix: &str) -> String {
    if name.is_empty() || prefix.is_empty() {
        return name.to_string();
    }
    let pattern: Vec<char> = lower(&format!("{prefix}_")).chars().collect();
    let mut chars = name.char_indices();
    for p in &pattern {
        match chars.next() {
            Some((_, c)) if lower(&c.to_string()).chars().eq(std::iter::once(*p)) => {}
            _ => return name.to_string(),
        }
    }
    match chars.next() {
        Some((i, _)) => name[i..].to_string(),
        None => String::new(),
    }
}

/// BuildMappingsInMemoryAsync + UpdateAssetsWithSourceFromCsvLinesAsync: the symbol names of each id (but id 0, the
/// document class, whose first name is the prefix taken off the others), less the XML ones; the first name of an id
/// is the source of the asset named by each further name. Names with a comma are dropped (the C# joins and splits
/// "id,name" lines).
fn build_sources(assets: &mut Dict<Asset>, csv_lines: &[String]) {
    let tags = symbol_class_tags(csv_lines);
    let prefix = tags.iter().find(|(t, _)| t == "0").and_then(|(_, n)| n.first().cloned()).unwrap_or_default();
    let mut sources: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (tag, names) in &tags {
        if tag == "0" {
            continue;
        }
        for name in names {
            let cleaned = remove_swf_prefix(name, &prefix);
            let l = cleaned.to_ascii_lowercase();
            if ["visualization", "logic", "index", "assets", "manifest"].iter().any(|s| l.ends_with(s)) {
                continue;
            }
            // "{tag},{cleaned}".Split(',') must give exactly two parts.
            if tag.contains(',') || cleaned.contains(',') {
                continue;
            }
            match sources.get(tag) {
                Some(source) => {
                    if let Some(entry) = assets.iter_mut().find(|(k, _)| *k == cleaned) {
                        entry.1.source = Some(source.clone());
                    }
                }
                None => {
                    sources.insert(tag.clone(), cleaned);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(symbols: &[(u16, &str)]) -> Vec<String> {
        symbols.iter().map(|(i, n)| format!("{i};\"{n}\"")).collect()
    }

    #[test]
    fn assets_sources_and_aliases() {
        let manifest = r#"<manifest><library name="fx" version="0.1"><assets>
            <asset name="h_std_fx5_a_0_0" mimeType="image/png"><param key="offset" value="-20, 33"/></asset>
            <asset name="h_std_fx5_b_0_0" mimeType="image/png"><param key="offset" value="1,2"/></asset>
            <asset name="h_std_fx5_c_0_0" mimeType="image/png"><param key="offset" value="1,2,3"/></asset>
            <asset name="noOffset" mimeType="image/png"><param key="x" value="1,2"/></asset>
            <asset name="xml" mimeType="text/xml"><param key="offset" value="1,2"/></asset>
            <asset name="h_std_fx5_a_0_0" mimeType="image/png"><param key="offset" value="5,6"/></asset>
            </assets><aliases>
            <alias name="h_std_fx5_a_2_0" link="h_std_fx5_a_0_0" fliph="1" flipv="False"/>
            <alias name="nolink" link=""/>
            <alias name="h_std_fx5_a_4_0" link="h_std_fx5_a_0_0" fliph="yes" flipv="true"/>
            </aliases></library></manifest>"#;
        let csv = lines(&[(0, "fx"), (1, "fx_manifest"), (5, "fx_h_std_fx5_a_0_0"), (5, "FX_h_std_fx5_b_0_0"), (6, "fx_h_std_fx5_c_0_0")]);
        let data = parse(manifest, &csv);
        let assets: Vec<_> = data.assets.iter().map(|(k, a)| (k.as_str(), a.x, a.y, a.source.as_deref())).collect();
        assert_eq!(assets, [("h_std_fx5_a_0_0", 5, 6, None), ("h_std_fx5_b_0_0", 1, 2, Some("h_std_fx5_a_0_0"))]);
        assert_eq!(
            data.aliases,
            [
                ("h_std_fx5_a_2_0".to_string(), Alias { link: "h_std_fx5_a_0_0".into(), flip_h: true, flip_v: false }),
                ("h_std_fx5_a_4_0".to_string(), Alias { link: "h_std_fx5_a_0_0".into(), flip_h: false, flip_v: true }),
            ]
        );
        assert!(parse("<manifest", &csv).assets.is_empty());
        assert_eq!(remove_swf_prefix("FX_a", "fx"), "a");
        assert_eq!(remove_swf_prefix("fxa", "fx"), "fxa");
    }
}
