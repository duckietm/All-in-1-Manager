//! The source hotel's furnidata: product class id -> furni class name.
//!
//! A catalogue product carries the class id of the hotel it was ripped from (its furnidata id,
//! items_base.sprite_id there). Another hotel numbers its furni differently, so matching on
//! sprite_id puts the wrong furni (or none) in item_ids. With the source's furnidata the class id
//! becomes a class name (`chair_norja*4`), matched against items_base.item_name of the target
//! hotel, which is the same in every emulator.
//!
//! Both formats are read: furnidata JSON (`roomitemtypes.furnitype[]` / `wallitemtypes.
//! furnitype[]`, each with `id` and `classname`) and furnidata XML (`<roomitemtypes><furnitype
//! id=".." classname="..">`). The whole entries are kept, so the furni a hotel lacks can be
//! handed to a furni import as a furnidata of their own ([`Furnidata::subset`]).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::packets::Product;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Furnidata {
    /// Floor furni (product type `s`): id -> class name.
    pub floor: HashMap<i32, String>,
    /// Wall furni (product type `i`): id -> class name.
    pub wall: HashMap<i32, String>,
    /// The whole entry of every furni, by (`s` / `i`, id), as furnidata JSON (an XML entry
    /// becomes the same JSON fields).
    pub entries: BTreeMap<(char, i32), Value>,
}

impl Furnidata {
    /// Reads furnidata JSON or XML (detected from the first character).
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim_start_matches('\u{feff}').trim_start();
        let data = if text.starts_with('{') {
            Self::parse_json(text)?
        } else if text.starts_with('<') {
            Self::parse_xml(text)
        } else {
            bail!("not furnidata (neither JSON nor XML)");
        };
        if data.floor.is_empty() && data.wall.is_empty() {
            bail!("the furnidata lists no furni");
        }
        Ok(data)
    }

    fn insert(&mut self, kind: char, id: i32, name: String, entry: Value) {
        let map = if kind == 's' {
            &mut self.floor
        } else {
            &mut self.wall
        };
        map.entry(id).or_insert(name);
        self.entries.entry((kind, id)).or_insert(entry);
    }

    fn parse_json(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).context("furnidata JSON")?;
        let mut data = Self::default();
        for (section, kind) in [("roomitemtypes", 's'), ("wallitemtypes", 'i')] {
            let list = value
                .get(section)
                .and_then(|s| s.get("furnitype"))
                .and_then(Value::as_array);
            for f in list.into_iter().flatten() {
                let id = f.get("id").and_then(|id| {
                    id.as_i64()
                        .or_else(|| id.as_str().and_then(|s| s.trim().parse().ok()))
                });
                let name = f.get("classname").and_then(Value::as_str).map(str::trim);
                if let (Some(id), Some(name)) = (id, name) {
                    if !name.is_empty() {
                        data.insert(kind, id as i32, name.to_string(), f.clone());
                    }
                }
            }
        }
        Ok(data)
    }

    fn parse_xml(text: &str) -> Self {
        let mut data = Self::default();
        for (section, kind) in [("roomitemtypes", 's'), ("wallitemtypes", 'i')] {
            let open = format!("<{section}");
            let close = format!("</{section}>");
            let Some(start) = text.find(&open) else {
                continue;
            };
            let end = text[start..]
                .find(&close)
                .map(|e| start + e)
                .unwrap_or(text.len());
            for item in text[start..end].split("<furnitype").skip(1) {
                let tag = &item[..item.find('>').unwrap_or(item.len())];
                let id = xml_attribute(tag, "id").and_then(|i| i.trim().parse::<i32>().ok());
                let name = xml_attribute(tag, "classname").map(|n| n.trim().to_string());
                let (Some(id), Some(name)) = (id, name) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                // The simple child elements (<name>, <description>, <offerid>, ...) as fields.
                let mut entry = Map::new();
                entry.insert("id".into(), json!(id));
                entry.insert("classname".into(), json!(name));
                let body = if tag.ends_with('/') {
                    ""
                } else {
                    &item[(tag.len() + 1).min(item.len())..]
                };
                let body = &body[..body.find("</furnitype>").unwrap_or(body.len())];
                for child in body.split('<').skip(1) {
                    let Some((head, value)) = child.split_once('>') else {
                        continue;
                    };
                    let key = head.trim();
                    if key.is_empty()
                        || key.starts_with('/')
                        || key.contains(char::is_whitespace)
                        || key.ends_with('/')
                    {
                        continue;
                    }
                    let value = xml_unescape(value.trim());
                    let value = match value.parse::<i64>() {
                        Ok(n) if key != "name" && key != "description" => json!(n),
                        _ => json!(value),
                    };
                    entry.entry(key.to_string()).or_insert(value);
                }
                data.insert(kind, id, name, Value::Object(entry));
            }
        }
        data
    }

    /// A furnidata JSON with only these furni (`s` / `i`, id), in the format furnidata imports
    /// read (`roomitemtypes.furnitype[]`, `wallitemtypes.furnitype[]`).
    pub fn subset(&self, wanted: &[(char, i32)]) -> Value {
        let mut floor = Vec::new();
        let mut wall = Vec::new();
        for key in wanted {
            if let Some(entry) = self.entries.get(key) {
                if key.0 == 's' {
                    floor.push(entry.clone());
                } else {
                    wall.push(entry.clone());
                }
            }
        }
        json!({
            "roomitemtypes": { "furnitype": floor },
            "wallitemtypes": { "furnitype": wall },
        })
    }

    /// The class name of a floor (`s`) or wall (`i`) product.
    pub fn classname(&self, product: &Product) -> Option<&str> {
        let id = product.class_id?;
        match product.product_type.as_str() {
            "s" => self.floor.get(&id),
            "i" => self.wall.get(&id),
            _ => None,
        }
        .map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.floor.len() + self.wall.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn xml_attribute(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let before_ok = at == 0 || rest.as_bytes()[at - 1].is_ascii_whitespace();
        let after = rest[at + name.len()..].trim_start();
        if before_ok {
            if let Some(after) = after.strip_prefix('=') {
                let after = after.trim_start();
                let quote = after.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let value = &after[1..];
                    let end = value.find(quote)?;
                    return Some(xml_unescape(&value[..end]));
                }
            }
        }
        rest = &rest[at + name.len()..];
    }
}

fn xml_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The furnidata of an official Habbo hotel from its game host (game-nl.habbo.com ->
/// https://www.habbo.nl/gamedata/furnidata_json/1); `None` for other hosts.
pub fn official_url(host: &str) -> Option<String> {
    let name = crate::rip::hotel_name(host);
    name.starts_with("habbo.")
        .then(|| format!("https://www.{name}/gamedata/furnidata_json/1"))
}

/// Loads furnidata from an http(s) URL or a local file.
pub async fn load(source: &str) -> Result<Furnidata> {
    let source = source.trim();
    let text = if source.starts_with("http://") || source.starts_with("https://") {
        let bytes = crate::http::try_get_bytes(source)
            .await?
            .with_context(|| format!("{source} is not there"))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    Furnidata::parse(&text).with_context(|| format!("reading the furnidata at {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(kind: &str, id: i32) -> Product {
        Product {
            product_type: kind.into(),
            class_id: Some(id),
            extra_param: String::new(),
            count: 1,
            limited: false,
            limited_series_size: 0,
            limited_items_left: 0,
        }
    }

    #[test]
    fn json_and_xml() {
        let json = r#"{"roomitemtypes":{"furnitype":[{"id":13,"classname":"shelves_norja","xdim":1},{"id":"14","classname":"chair*4"}]},
                       "wallitemtypes":{"furnitype":[{"id":4001,"classname":"poster"}]}}"#;
        let f = Furnidata::parse(json).unwrap();
        assert_eq!(f.classname(&product("s", 14)), Some("chair*4"));
        assert_eq!(f.classname(&product("i", 4001)), Some("poster"));
        assert_eq!(f.classname(&product("i", 13)), None);
        assert_eq!(f.classname(&product("e", 13)), None);
        assert_eq!(f.entries[&('s', 13)]["xdim"], 1, "the whole entry is kept");

        let xml = "\u{feff}<?xml version=\"1.0\"?><furnidata><roomitemtypes>\
                   <furnitype id=\"13\" classname=\"shelves_norja\"><name>Shelf &amp; co</name><offerid>55</offerid></furnitype>\
                   <furnitype classname='rare_a&amp;b' id='15'/>\
                   </roomitemtypes><wallitemtypes><furnitype id=\"13\" classname=\"wallpaper\"></furnitype></wallitemtypes></furnidata>";
        let f = Furnidata::parse(xml).unwrap();
        assert_eq!(f.classname(&product("s", 13)), Some("shelves_norja"));
        assert_eq!(f.classname(&product("s", 15)), Some("rare_a&b"));
        assert_eq!(f.classname(&product("i", 13)), Some("wallpaper"));
        assert_eq!(f.entries[&('s', 13)]["name"], "Shelf & co");
        assert_eq!(f.entries[&('s', 13)]["offerid"], 55);
        let subset = f.subset(&[('s', 15), ('i', 13), ('s', 999)]);
        assert_eq!(subset["roomitemtypes"]["furnitype"][0]["classname"], "rare_a&b");
        assert_eq!(subset["wallitemtypes"]["furnitype"][0]["id"], 13);
        assert!(Furnidata::parse("{}").is_err());
        assert!(Furnidata::parse("nope").is_err());
    }

    #[test]
    fn official_urls() {
        assert_eq!(
            official_url("game-nl.habbo.com").as_deref(),
            Some("https://www.habbo.nl/gamedata/furnidata_json/1")
        );
        assert_eq!(
            official_url("game-us.habbo.com").as_deref(),
            Some("https://www.habbo.com/gamedata/furnidata_json/1")
        );
        assert_eq!(official_url("game.retro.net"), None);
    }

    #[tokio::test]
    async fn loads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("furnidata.xml");
        std::fs::write(&file, "<furnidata><roomitemtypes><furnitype id=\"1\" classname=\"a\"/></roomitemtypes></furnidata>").unwrap();
        let f = load(file.to_str().unwrap()).await.unwrap();
        assert_eq!(f.floor[&1], "a");
    }
}
