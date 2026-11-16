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
//! id=".." classname="..">`). Whole entries are kept as JSON text, so the furni a hotel lacks can
//! be handed to a furni import as a furnidata of their own ([`Furnidata::subset`]); a rip keeps
//! only the entries of those furni ([`Furnidata::parse_keeping`]): an official furnidata is
//! 10 MB and more, too much to keep whole in a plugin's memory.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use serde::de::IgnoredAny;
use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};

use crate::packets::Product;

/// A furni: floor (`s`) or wall (`i`), and its id.
pub type FurniKey = (char, i32);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Furnidata {
    /// Floor furni (product type `s`): id -> class name.
    pub floor: HashMap<i32, String>,
    /// Wall furni (product type `i`): id -> class name.
    pub wall: HashMap<i32, String>,
    /// The whole entry of the furni kept, by (`s` / `i`, id), as furnidata JSON text (an XML
    /// entry becomes the same JSON fields): every furni after [`Furnidata::parse`], the wanted
    /// ones after [`Furnidata::parse_keeping`].
    pub entries: BTreeMap<FurniKey, Box<str>>,
}

/// The fields of a JSON furnitype entry the names come from (the rest is skipped unread).
#[derive(Deserialize)]
struct JsonHead {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    classname: Option<Value>,
}

#[derive(Deserialize)]
struct JsonDoc<'a> {
    #[serde(borrow, default)]
    roomitemtypes: Option<&'a RawValue>,
    #[serde(borrow, default)]
    wallitemtypes: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct JsonSection<'a> {
    #[serde(borrow, default)]
    furnitype: Option<&'a RawValue>,
}

/// An XML furnitype: its kind, id, class name, and its text (the tag's attributes and the rest).
struct XmlItem<'a> {
    kind: char,
    id: i32,
    name: String,
    tag: &'a str,
    item: &'a str,
}

impl Furnidata {
    /// Reads furnidata JSON or XML (detected from the first character), keeping every entry.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_keeping(text, |_| None)
    }

    /// Reads furnidata JSON or XML, keeping the whole entries only of the furni `wanted` names
    /// (`None`: every furni). `wanted` is asked once the class names are read (it gets the
    /// furnidata without entries).
    pub fn parse_keeping(
        text: &str,
        wanted: impl FnOnce(&Furnidata) -> Option<HashSet<FurniKey>>,
    ) -> Result<Self> {
        let text = text.trim_start_matches('\u{feff}').trim_start();
        let mut data = Self::default();
        if text.starts_with('{') {
            let mut heads = Vec::new();
            for (kind, raw) in json_entries(text)? {
                if let Some((id, name)) = json_head(raw) {
                    data.insert(kind, id, name);
                    heads.push((kind, id, raw));
                }
            }
            data.check()?;
            let wanted = wanted(&data);
            for (kind, id, raw) in heads {
                if wanted.as_ref().is_none_or(|w| w.contains(&(kind, id))) {
                    data.entries
                        .entry((kind, id))
                        .or_insert_with(|| raw.get().into());
                }
            }
        } else if text.starts_with('<') {
            let items = xml_items(text);
            for item in &items {
                data.insert(item.kind, item.id, item.name.clone());
            }
            data.check()?;
            let wanted = wanted(&data);
            for item in &items {
                let key = (item.kind, item.id);
                if wanted.as_ref().is_none_or(|w| w.contains(&key))
                    && !data.entries.contains_key(&key)
                {
                    let entry = xml_entry(item).to_string();
                    data.entries.insert(key, entry.into());
                }
            }
        } else {
            bail!("not furnidata (neither JSON nor XML)");
        }
        Ok(data)
    }

    fn check(&self) -> Result<()> {
        if self.floor.is_empty() && self.wall.is_empty() {
            bail!("the furnidata lists no furni");
        }
        Ok(())
    }

    fn insert(&mut self, kind: char, id: i32, name: String) {
        let map = if kind == 's' {
            &mut self.floor
        } else {
            &mut self.wall
        };
        map.entry(id).or_insert(name);
    }

    /// The whole entry of a furni, if it was kept.
    pub fn entry(&self, key: FurniKey) -> Option<Value> {
        self.entries
            .get(&key)
            .and_then(|text| serde_json::from_str(text).ok())
    }

    /// A furnidata JSON with only these furni (`s` / `i`, id), in the format furnidata imports
    /// read (`roomitemtypes.furnitype[]`, `wallitemtypes.furnitype[]`). Furni whose entry was
    /// not kept are left out.
    pub fn subset(&self, wanted: &[FurniKey]) -> Value {
        let (floor, wall) = self.subset_entries(wanted);
        let values = |list: Vec<&str>| -> Vec<Value> {
            list.into_iter()
                .filter_map(|text| serde_json::from_str(text).ok())
                .collect()
        };
        json!({
            "roomitemtypes": { "furnitype": values(floor) },
            "wallitemtypes": { "furnitype": values(wall) },
        })
    }

    /// [`Furnidata::subset`] to serialize straight from the kept entries (one entry's JSON
    /// value at a time): the same JSON, without the whole tree in memory.
    pub fn subset_json(&self, wanted: &[FurniKey]) -> SubsetJson<'_> {
        let (floor, wall) = self.subset_entries(wanted);
        SubsetJson { floor, wall }
    }

    fn subset_entries(&self, wanted: &[FurniKey]) -> (Vec<&str>, Vec<&str>) {
        let mut floor = Vec::new();
        let mut wall = Vec::new();
        for key in wanted {
            if let Some(entry) = self.entries.get(key) {
                if key.0 == 's' {
                    floor.push(&**entry);
                } else {
                    wall.push(&**entry);
                }
            }
        }
        (floor, wall)
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

/// What [`Furnidata::subset_json`] serializes.
pub struct SubsetJson<'a> {
    floor: Vec<&'a str>,
    wall: Vec<&'a str>,
}

struct EntriesJson<'a>(&'a [&'a str]);

impl Serialize for EntriesJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for text in self.0 {
            let value: Value = serde_json::from_str(text).map_err(S::Error::custom)?;
            seq.serialize_element(&value)?;
        }
        seq.end()
    }
}

struct SectionJson<'a>(&'a [&'a str]);

impl Serialize for SectionJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("furnitype", &EntriesJson(self.0))?;
        map.end()
    }
}

impl Serialize for SubsetJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("roomitemtypes", &SectionJson(&self.floor))?;
        map.serialize_entry("wallitemtypes", &SectionJson(&self.wall))?;
        map.end()
    }
}

/// The furnitype entries of furnidata JSON, floor then wall, in document order (as raw JSON,
/// borrowed from `text`). A section that is not there, or not an object with a `furnitype`
/// list, has none.
fn json_entries(text: &str) -> Result<Vec<(char, &RawValue)>> {
    let doc: JsonDoc = match serde_json::from_str(text) {
        Ok(doc) => doc,
        Err(_) => {
            // Not JSON at all (the error says where), or JSON of another shape (no furni).
            serde_json::from_str::<IgnoredAny>(text).context("furnidata JSON")?;
            JsonDoc {
                roomitemtypes: None,
                wallitemtypes: None,
            }
        }
    };
    let mut out = Vec::new();
    for (section, kind) in [(doc.roomitemtypes, 's'), (doc.wallitemtypes, 'i')] {
        let list = section
            .and_then(|s| serde_json::from_str::<JsonSection>(s.get()).ok())
            .and_then(|s| s.furnitype)
            .and_then(|list| serde_json::from_str::<Vec<&RawValue>>(list.get()).ok());
        out.extend(list.into_iter().flatten().map(|raw| (kind, raw)));
    }
    Ok(out)
}

/// The id and class name of a JSON furnitype entry (`None`: it has no usable pair).
fn json_head(raw: &RawValue) -> Option<(i32, String)> {
    let head: JsonHead = serde_json::from_str(raw.get()).ok()?;
    let id = head.id.as_ref().and_then(|id| {
        id.as_i64()
            .or_else(|| id.as_str().and_then(|s| s.trim().parse().ok()))
    })?;
    let name = head.classname.as_ref().and_then(Value::as_str).map(str::trim)?;
    (!name.is_empty()).then(|| (id as i32, name.to_string()))
}

/// The furnitypes of furnidata XML with an id and a class name, floor then wall.
fn xml_items(text: &str) -> Vec<XmlItem<'_>> {
    let mut out = Vec::new();
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
            out.push(XmlItem {
                kind,
                id,
                name,
                tag,
                item,
            });
        }
    }
    out
}

/// An XML furnitype as furnidata JSON fields: id, classname, then its simple child elements
/// (`<name>`, `<description>`, `<offerid>`, ...).
fn xml_entry(x: &XmlItem) -> Value {
    let mut entry = Map::new();
    entry.insert("id".into(), json!(x.id));
    entry.insert("classname".into(), json!(x.name));
    let body = if x.tag.ends_with('/') {
        ""
    } else {
        &x.item[(x.tag.len() + 1).min(x.item.len())..]
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
    Value::Object(entry)
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
    let name = crate::names::hotel_name(host);
    name.starts_with("habbo.")
        .then(|| format!("https://www.{name}/gamedata/furnidata_json/1"))
}

/// Reads furnidata from the bytes of an http(s) response or a local file (the caller fetches or
/// reads them). `source` is only named in the error.
pub fn load(source: &str, bytes: &[u8]) -> Result<Furnidata> {
    load_keeping(source, bytes, |_| None)
}

/// [`load`] keeping only the entries `wanted` names (see [`Furnidata::parse_keeping`]).
pub fn load_keeping(
    source: &str,
    bytes: &[u8],
    wanted: impl FnOnce(&Furnidata) -> Option<HashSet<FurniKey>>,
) -> Result<Furnidata> {
    let text = String::from_utf8_lossy(bytes);
    Furnidata::parse_keeping(&text, wanted)
        .with_context(|| format!("reading the furnidata at {}", source.trim()))
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
        assert_eq!(f.entry(('s', 13)).unwrap()["xdim"], 1, "the whole entry is kept");

        let xml = "\u{feff}<?xml version=\"1.0\"?><furnidata><roomitemtypes>\
                   <furnitype id=\"13\" classname=\"shelves_norja\"><name>Shelf &amp; co</name><offerid>55</offerid></furnitype>\
                   <furnitype classname='rare_a&amp;b' id='15'/>\
                   </roomitemtypes><wallitemtypes><furnitype id=\"13\" classname=\"wallpaper\"></furnitype></wallitemtypes></furnidata>";
        let f = Furnidata::parse(xml).unwrap();
        assert_eq!(f.classname(&product("s", 13)), Some("shelves_norja"));
        assert_eq!(f.classname(&product("s", 15)), Some("rare_a&b"));
        assert_eq!(f.classname(&product("i", 13)), Some("wallpaper"));
        assert_eq!(f.entry(('s', 13)).unwrap()["name"], "Shelf & co");
        assert_eq!(f.entry(('s', 13)).unwrap()["offerid"], 55);
        let subset = f.subset(&[('s', 15), ('i', 13), ('s', 999)]);
        assert_eq!(subset["roomitemtypes"]["furnitype"][0]["classname"], "rare_a&b");
        assert_eq!(subset["wallitemtypes"]["furnitype"][0]["id"], 13);
        assert!(Furnidata::parse("{}").is_err());
        assert!(Furnidata::parse("nope").is_err());
        assert!(Furnidata::parse("{\"roomitemtypes\": [1, 2").is_err());
        assert!(Furnidata::parse("{\"roomitemtypes\": 5}").is_err(), "no furni");
    }

    /// The entries come back as the JSON values the whole document has for them, whatever the
    /// spacing, and the first usable entry of an id wins (as for the class names).
    #[test]
    fn entries_are_the_documents_values() {
        let json = r##"{ "roomitemtypes" : { "furnitype" : [
            {"id": 1, "classname": "", "name": "no class name"},
            {"id": 1, "classname": " a ", "name": "A \"quoted\" é", "partcolors": {"color": ["#FFF", "#000"]}, "x": 1.5},
            {"id": 1, "classname": "b"},
            7,
            {"id": "2", "classname": "c", "rare": true, "n": null}
        ]}, "wallitemtypes": {"furnitype": {"not": "a list"}}, "other": [1] }"##;
        let whole: Value = serde_json::from_str(json).unwrap();
        let list = &whole["roomitemtypes"]["furnitype"];
        let f = Furnidata::parse(json).unwrap();
        assert_eq!(f.classname(&product("s", 1)), Some("a"));
        assert_eq!(f.entry(('s', 1)).unwrap(), list[1]);
        assert_eq!(f.entry(('s', 2)).unwrap(), list[4]);
        assert_eq!(f.len(), 2);
        assert_eq!(
            serde_json::to_string_pretty(&f.subset_json(&[('s', 2), ('s', 1), ('i', 9)])).unwrap(),
            serde_json::to_string_pretty(&f.subset(&[('s', 2), ('s', 1), ('i', 9)])).unwrap()
        );
    }

    #[test]
    fn keeps_only_the_wanted_entries() {
        let json = r#"{"roomitemtypes":{"furnitype":[{"id":1,"classname":"a","name":"A"},{"id":2,"classname":"b"}]},
                       "wallitemtypes":{"furnitype":[{"id":1,"classname":"w"}]}}"#;
        let mut asked = 0;
        let f = Furnidata::parse_keeping(json, |names| {
            asked += 1;
            assert_eq!(names.len(), 3, "the names are read first");
            assert!(names.entries.is_empty());
            Some(HashSet::from([('s', 1), ('i', 1), ('s', 99)]))
        })
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(f.len(), 3, "every class name");
        assert_eq!(f.entries.keys().copied().collect::<Vec<_>>(), [('i', 1), ('s', 1)]);
        assert_eq!(f.entry(('s', 1)).unwrap()["name"], "A");

        let xml = "<furnidata><roomitemtypes><furnitype id=\"1\" classname=\"a\"><name>A</name></furnitype>\
                   <furnitype id=\"2\" classname=\"b\"/></roomitemtypes></furnidata>";
        let f = Furnidata::parse_keeping(xml, |_| Some(HashSet::from([('s', 2)]))).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.entry(('s', 2)).unwrap(), json!({"id": 2, "classname": "b"}));
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

    #[test]
    fn loads_bytes() {
        let xml = "<furnidata><roomitemtypes><furnitype id=\"1\" classname=\"a\"/></roomitemtypes></furnidata>";
        let f = load("furnidata.xml", xml.as_bytes()).unwrap();
        assert_eq!(f.floor[&1], "a");
        assert!(load("furnidata.xml", b"nope").is_err());
    }
}
