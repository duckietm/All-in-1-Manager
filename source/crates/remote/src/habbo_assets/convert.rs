//! What Habbo serves as XML or text, as the JSON Octane loads, with the field names and order of the All-in-1
//! Converter (Download Classes/Clothes.cs, Effects.cs, Productdata.cs, Badge.cs, Quests.cs, Reception.cs), and the
//! lists the downloads take from Habbo's texts and variables.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::server::{Result, ServerError};

fn parse<'x>(xml: &'x str, what: &str) -> Result<roxmltree::Document<'x>> {
    roxmltree::Document::parse(xml).map_err(|e| ServerError(format!("{what} is not valid XML: {e}")).into())
}

fn int(node: roxmltree::Node, attribute: &str) -> i64 {
    node.attribute(attribute).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

fn flag(node: roxmltree::Node, attribute: &str) -> bool {
    node.attribute(attribute) == Some("1")
}

fn text(node: roxmltree::Node) -> String {
    node.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect::<String>().trim().to_string()
}

fn children<'a, 'input>(node: roxmltree::Node<'a, 'input>, tag: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'input>> + 'a {
    node.children().filter(move |n| n.has_tag_name(tag))
}

fn child<'a, 'input>(node: roxmltree::Node<'a, 'input>, tag: &str) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|n| n.has_tag_name(tag))
}

/// figuredata.xml as FigureData.json. `not_sellable` makes every set non-sellable, so every clothing item can be worn
/// (the converter's "disable all sellable" question).
pub fn figuredata(xml: &str, not_sellable: bool) -> Result<Value> {
    let document = parse(xml, "figuredata")?;
    let root = document.root_element();
    if !root.has_tag_name("figuredata") {
        return Err(ServerError("figuredata has no <figuredata> root element.".into()).into());
    }
    let palettes: Vec<Value> = child(root, "colors")
        .map(|colors| {
            children(colors, "palette")
                .map(|p| {
                    let colors: Vec<Value> = children(p, "color")
                        .map(|c| {
                            json!({
                                "id": int(c, "id"),
                                "index": int(c, "index"),
                                "club": int(c, "club"),
                                "selectable": flag(c, "selectable"),
                                "hexCode": text(c),
                            })
                        })
                        .collect();
                    json!({ "id": int(p, "id"), "colors": colors })
                })
                .collect()
        })
        .unwrap_or_default();
    let set_types: Vec<Value> = child(root, "sets")
        .map(|sets| {
            children(sets, "settype")
                .map(|st| {
                    let sets: Vec<Value> = children(st, "set")
                        .map(|s| {
                            let parts: Vec<Value> = children(s, "part")
                                .map(|p| {
                                    json!({
                                        "id": int(p, "id"),
                                        "type": p.attribute("type").unwrap_or(""),
                                        "colorable": flag(p, "colorable"),
                                        "index": int(p, "index"),
                                        "colorindex": int(p, "colorindex"),
                                    })
                                })
                                .collect();
                            let mut set = Map::new();
                            set.insert("id".into(), int(s, "id").into());
                            set.insert("gender".into(), s.attribute("gender").unwrap_or("").into());
                            set.insert("club".into(), int(s, "club").into());
                            set.insert("colorable".into(), flag(s, "colorable").into());
                            set.insert("selectable".into(), flag(s, "selectable").into());
                            set.insert("preselectable".into(), flag(s, "preselectable").into());
                            set.insert("sellable".into(), (!not_sellable && flag(s, "sellable")).into());
                            set.insert("parts".into(), parts.into());
                            let hidden: Vec<Value> = children(s, "hiddenlayers")
                                .flat_map(|h| children(h, "layer"))
                                .map(|l| json!({ "partType": l.attribute("parttype").unwrap_or("") }))
                                .collect();
                            if !hidden.is_empty() {
                                set.insert("hiddenLayers".into(), hidden.into());
                            }
                            Value::Object(set)
                        })
                        .collect();
                    json!({
                        "type": st.attribute("type").unwrap_or(""),
                        "paletteId": int(st, "paletteid"),
                        "mandatory_f_0": flag(st, "mand_f_0"),
                        "mandatory_f_1": flag(st, "mand_f_1"),
                        "mandatory_m_0": flag(st, "mand_m_0"),
                        "mandatory_m_1": flag(st, "mand_m_1"),
                        "sets": sets,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({ "palettes": palettes, "setTypes": set_types }))
}

/// figuremap.xml as FigureMap.json, and the clothes libraries it lists (each once, in order).
pub fn figuremap(xml: &str) -> Result<(Value, Vec<String>)> {
    let document = parse(xml, "figuremap")?;
    let root = document.root_element();
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let libraries: Vec<Value> = children(root, "lib")
        .map(|lib| {
            let id = lib.attribute("id").map(str::to_string);
            if let Some(id) = id.as_ref().filter(|id| !id.is_empty()) {
                if seen.insert(id.to_lowercase()) {
                    names.push(id.clone());
                }
            }
            let parts: Vec<Value> = children(lib, "part")
                .map(|p| {
                    let id = p.attribute("id").and_then(|v| v.trim().parse::<i64>().ok());
                    json!({ "id": id, "type": p.attribute("type") })
                })
                .collect();
            json!({ "id": id, "revision": int(lib, "revision"), "parts": parts })
        })
        .collect();
    Ok((json!({ "libraries": libraries }), names))
}

/// effectmap.xml as EffectMap.json, and the effect libraries it lists (each once, in order).
pub fn effectmap(xml: &str) -> Result<(Value, Vec<String>)> {
    let document = parse(xml, "effectmap")?;
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let effects: Vec<Value> = document
        .descendants()
        .filter(|n| n.has_tag_name("effect"))
        .map(|e| {
            if let Some(lib) = e.attribute("lib").filter(|l| !l.is_empty()) {
                if seen.insert(lib.to_string()) {
                    names.push(lib.to_string());
                }
            }
            json!({ "id": e.attribute("id"), "lib": e.attribute("lib"), "type": e.attribute("type"), "revision": int(e, "revision") })
        })
        .collect();
    Ok((json!({ "effects": effects }), names))
}

fn true_or_none(node: roxmltree::Node, attribute: &str) -> Option<bool> {
    node.attribute(attribute).filter(|v| *v == "1" || v.eq_ignore_ascii_case("true")).map(|_| true)
}

fn list(node: roxmltree::Node, attribute: &str) -> Option<Vec<Value>> {
    let items: Vec<Value> = node
        .attribute(attribute)?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .collect();
    (!items.is_empty()).then_some(items)
}

fn put<T: Into<Value>>(map: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        map.insert(key.into(), value.into());
    }
}

/// A whole number as an integer (-17, as .NET writes the double -17), anything else as a decimal.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

/// habbo-avatar-render-lib's action_offset_lay.xml and action_offset_swim.xml (Effects.cs DefaultActionOffsets).
const DEFAULT_OFFSETS: [(&str, &[(&str, i64, f64, f64, f64)]); 2] = [
    ("lay", &[("h", 4, -17.0, 17.0, -0.9), ("h", 2, 22.0, 17.0, -0.9), ("sh", 4, -5.0, 16.0, -0.9), ("sh", 2, 9.0, 16.0, -0.9)]),
    (
        "swim",
        &[
            ("sh", 0, -28.0, 0.0, 0.0),
            ("sh", 1, -28.0, 0.0, 0.0),
            ("sh", 2, -28.0, 0.0, 0.0),
            ("sh", 3, -28.0, 0.0, 0.0),
            ("sh", 4, 0.0, 0.0, 0.0),
            ("sh", 5, 0.0, 0.0, 0.0),
            ("sh", 6, 0.0, 0.0, 0.0),
            ("sh", 7, -28.0, 0.0, 0.0),
        ],
    ),
];

/// HabboAvatarActions.xml as HabboAvatarActions.json: missing values are left out, and the lay and swim offsets of
/// the avatar render library are added as actionOffsets.
pub fn avatar_actions(xml: &str) -> Result<Value> {
    let document = parse(xml, "HabboAvatarActions")?;
    let mut actions = Vec::new();
    let mut states: Vec<String> = Vec::new();
    for x in document.descendants().filter(|n| n.has_tag_name("action")) {
        let mut action = Map::new();
        put(&mut action, "id", x.attribute("id"));
        put(&mut action, "state", x.attribute("state"));
        if let Some(state) = x.attribute("state").filter(|s| !s.is_empty()) {
            if !states.iter().any(|s| s == state) {
                states.push(state.to_string());
            }
        }
        put(&mut action, "precedence", x.attribute("precedence").and_then(|v| v.trim().parse::<i64>().ok()));
        put(&mut action, "animation", true_or_none(x, "animation"));
        put(&mut action, "main", true_or_none(x, "main"));
        put(&mut action, "geometryType", x.attribute("geometrytype"));
        put(&mut action, "activePartSet", x.attribute("activepartset"));
        put(&mut action, "assetPartDefinition", x.attribute("assetpartdefinition"));
        put(&mut action, "startFromFrameZero", true_or_none(x, "startfromframezero"));
        put(&mut action, "preventHeadTurn", true_or_none(x, "preventheadturn"));
        put(&mut action, "prevents", list(x, "prevents"));
        let mut params: Vec<roxmltree::Node> = child(x, "params").map(|p| children(p, "param").collect()).unwrap_or_default();
        if params.is_empty() {
            params = children(x, "param").collect();
        }
        if !params.is_empty() {
            let params: Vec<Value> = params
                .iter()
                .map(|p| {
                    let mut param = Map::new();
                    put(&mut param, "id", p.attribute("id"));
                    put(&mut param, "value", p.attribute("value"));
                    Value::Object(param)
                })
                .collect();
            action.insert("params".into(), params.into());
        }
        let types: Vec<Value> = children(x, "type")
            .map(|y| {
                let mut kind = Map::new();
                put(&mut kind, "id", y.attribute("id"));
                let animated = y.attribute("animated").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
                kind.insert("animated".into(), animated.into());
                put(&mut kind, "preventHeadTurn", true_or_none(y, "preventheadturn"));
                put(&mut kind, "prevents", list(y, "prevents"));
                Value::Object(kind)
            })
            .collect();
        if !types.is_empty() {
            action.insert("types".into(), types.into());
        }
        actions.push(Value::Object(action));
    }
    let offsets: Vec<Value> = states
        .iter()
        .filter_map(|state| {
            let (_, defaults) = DEFAULT_OFFSETS.iter().find(|(name, _)| name == state)?;
            let offsets: Vec<Value> = defaults
                .iter()
                .map(|(size, direction, x, y, z)| json!({ "size": size, "direction": direction, "x": number(*x), "y": number(*y), "z": number(*z) }))
                .collect();
            Some(json!({ "action": state, "offsets": offsets }))
        })
        .collect();
    let mut root = Map::new();
    root.insert("actions".into(), actions.into());
    if !offsets.is_empty() {
        root.insert("actionOffsets".into(), offsets.into());
    }
    Ok(Value::Object(root))
}

/// HTML entities (HttpUtility.HtmlDecode) for the ones productdata uses.
fn html_decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Backslash escapes (Regex.Unescape): \uXXXX, \xXX, \n, \t, \r, \" and the like.
fn unescape(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let next = chars[i + 1];
        let hex = |from: usize, len: usize| -> Option<char> {
            let digits: String = chars.get(from..from + len)?.iter().collect();
            u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32)
        };
        match next {
            'u' => match hex(i + 2, 4) {
                Some(c) => {
                    out.push(c);
                    i += 6;
                    continue;
                }
                None => out.push('u'),
            },
            'x' => match hex(i + 2, 2) {
                Some(c) => {
                    out.push(c);
                    i += 4;
                    continue;
                }
                None => out.push('x'),
            },
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'e' => out.push('\u{1b}'),
            other => out.push(other),
        }
        i += 2;
    }
    out
}

/// productdata as ProductData.json: Habbo's XML (<product code="..."><name/><description/></product>, served since
/// 2026) or the older ["code","name","description"] triples.
pub fn productdata(text: &str) -> Value {
    let xml = text.trim_start_matches('\u{feff}').trim_start();
    if xml.starts_with('<') {
        let products: Vec<Value> = roxmltree::Document::parse(xml)
            .map(|doc| {
                children(doc.root_element(), "product")
                    .filter_map(|p| {
                        let code = p.attribute("code")?.trim();
                        let field = |tag: &str| child(p, tag).map(self::text).unwrap_or_default();
                        (!code.is_empty()).then(|| json!({ "code": code, "name": field("name"), "description": field("description") }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return json!({ "productdata": { "product": products } });
    }
    let triple = regex::Regex::new(r#"\[\s*"(.*?)"\s*,\s*"(.*?)"\s*,\s*"(.*?)"\s*\]"#).expect("productdata pattern");
    let products: Vec<Value> = triple
        .captures_iter(text)
        .map(|m| {
            let decode = |i: usize| unescape(&html_decode(m[i].trim()));
            json!({ "code": m[1].trim(), "name": decode(2), "description": decode(3) })
        })
        .collect();
    json!({ "productdata": { "product": products } })
}

/// key=value lines (external_variables, external_flash_texts) as one JSON object; # lines are comments.
pub fn key_values(text: &str) -> Value {
    let mut map = Map::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some(at) = line.find('=').filter(|&at| at > 0) else { continue };
        map.insert(line[..at].trim().to_string(), Value::String(line[at + 1..].trim().to_string()));
    }
    Value::Object(map)
}

/// The badge codes of "badge_name_CODE=..." lines; the _HHCA and _HHUK copies are left out unless fb_ or al_.
pub fn badge_names(texts: &str) -> Vec<String> {
    texts
        .lines()
        .filter_map(|line| line.strip_prefix("badge_name_"))
        .filter_map(|rest| rest.split_once('=').map(|(code, _)| code))
        .filter(|code| code.starts_with("fb_") || code.starts_with("al_") || (!code.contains("_HHCA") && !code.contains("_HHUK")))
        .filter(|code| safe_name(code))
        .map(str::to_string)
        .collect()
}

/// A name that is one plain file name everywhere (no folders, nothing Windows refuses).
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

/// The quest images "quests.<quest>.<image>..." lines name: <quest>.png and <quest>_<image>.png, lowercase, once each.
pub fn quest_images(texts: &str) -> Vec<String> {
    let mut images = BTreeSet::new();
    for line in texts.lines().filter(|l| l.starts_with("quests.")) {
        let parts: Vec<&str> = line.split('.').filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            continue;
        }
        let quest = parts[1].to_lowercase();
        let image = parts[2].to_lowercase();
        if !image.contains('=') {
            images.insert(format!("{quest}_{image}.png"));
        }
        images.insert(format!("{quest}.png"));
    }
    images.into_iter().filter(|name| safe_name(name)).collect()
}

/// The images under ${image.library.url} the variables name (hotel view, promos), as paths below c_images/.
pub fn library_images(variables: &str) -> Vec<String> {
    const MARK: &str = "${image.library.url}";
    let mut images = BTreeSet::new();
    for line in variables.lines() {
        for part in line.split(MARK).skip(1) {
            let path = part.split([',', ';']).next().unwrap_or("").trim().trim_end_matches(['}', '"']).trim();
            let lower = path.to_lowercase();
            if !(lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".gif")) {
                continue;
            }
            if path.starts_with('/') || path.split('/').any(|segment| !safe_name(segment)) {
                continue;
            }
            images.insert(path.to_string());
        }
    }
    images.into_iter().collect()
}

/// The release folder ("flash-assets-PRODUCTION-...") of flash.client.url in external_variables.
pub fn release(variables: &str) -> Option<String> {
    let line = variables.lines().find(|l| l.contains("flash.client.url="))?;
    let url = line.trim().trim_end_matches('/');
    let release = match url.find("/gordon/") {
        Some(at) => url[at + "/gordon/".len()..].split('/').next().unwrap_or(""),
        None => url.split('/').nth(4).unwrap_or(""),
    };
    (safe_name(release)).then(|| release.to_string())
}

/// The furniture in furnidata XML: (classname, revision) of every floor and wall item.
pub fn furnidata_entries(xml: &str) -> Result<Vec<(String, i64)>> {
    let document = parse(xml, "furnidata")?;
    let root = document.root_element();
    if !root.has_tag_name("furnidata") {
        return Err(ServerError("furnidata has no <furnidata> root element.".into()).into());
    }
    let mut entries = Vec::new();
    for section in ["roomitemtypes", "wallitemtypes"] {
        for item in child(root, section).into_iter().flat_map(|s| children(s, "furnitype")) {
            let classname = item.attribute("classname").unwrap_or("").to_string();
            if classname.is_empty() {
                continue;
            }
            let revision = child(item, "revision").and_then(|r| text(r).parse().ok()).unwrap_or(0);
            entries.push((classname, revision));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figuredata_like_the_converter() {
        let xml = r##"<figuredata><colors><palette id="1"><color id="2" index="3" club="0" selectable="1"> FFCC99 </color></palette></colors>
            <sets><settype type="hd" paletteid="1" mand_f_0="1" mand_m_1="1">
              <set id="180" gender="M" club="0" colorable="1" selectable="1" preselectable="1" sellable="1">
                <part id="1" type="bd" colorable="1" index="0" colorindex="1"/>
                <hiddenlayers><layer parttype="ha"/></hiddenlayers>
              </set>
              <set id="181" gender="F" sellable="0"/>
            </settype></sets></figuredata>"##;
        let json = figuredata(xml, false).unwrap();
        assert_eq!(json["palettes"][0]["colors"][0], json!({"id":2,"index":3,"club":0,"selectable":true,"hexCode":"FFCC99"}));
        let set_type = &json["setTypes"][0];
        assert_eq!(set_type["paletteId"], 1);
        assert_eq!((set_type["mandatory_f_0"].clone(), set_type["mandatory_m_0"].clone()), (json!(true), json!(false)));
        let set = &set_type["sets"][0];
        let keys: Vec<&String> = set.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["id", "gender", "club", "colorable", "selectable", "preselectable", "sellable", "parts", "hiddenLayers"]);
        assert_eq!(set["parts"][0], json!({"id":1,"type":"bd","colorable":true,"index":0,"colorindex":1}));
        assert_eq!(set["hiddenLayers"], json!([{"partType":"ha"}]));
        assert!(set_type["sets"][1].get("hiddenLayers").is_none());
        assert_eq!(figuredata(xml, true).unwrap()["setTypes"][0]["sets"][0]["sellable"], false);
    }

    #[test]
    fn figuremap_and_effectmap_list_their_libraries() {
        let (json, libs) = figuremap(r#"<map><lib id="hh_human_body" revision="12"><part id="1" type="bd"/><part id="x" type="hd"/></lib><lib id="HH_human_body"/></map>"#).unwrap();
        assert_eq!(libs, ["hh_human_body"]);
        assert_eq!(json["libraries"][0], json!({"id":"hh_human_body","revision":12,"parts":[{"id":1,"type":"bd"},{"id":null,"type":"hd"}]}));
        let (json, libs) = effectmap(r#"<map><effect id="1" lib="Dance1" type="dance" revision="3"/><effect id="2" lib="Dance1" type="dance" revision="3"/></map>"#).unwrap();
        assert_eq!(libs, ["Dance1"]);
        assert_eq!(json["effects"][0], json!({"id":"1","lib":"Dance1","type":"dance","revision":3}));
    }

    #[test]
    fn avatar_actions_leave_out_what_is_missing() {
        let xml = r#"<actions><action id="Lay" state="lay" precedence="10" main="1" geometrytype="horizontal" prevents="Dance, Wave">
            <params><param id="default" value="1"/></params><type id="1" animated="1" prevents="Sit"/></action>
            <action id="Talk" state="talk" animation="0"/></actions>"#;
        let json = avatar_actions(xml).unwrap();
        let lay = json["actions"][0].as_object().unwrap();
        let keys: Vec<&String> = lay.keys().collect();
        assert_eq!(keys, ["id", "state", "precedence", "main", "geometryType", "prevents", "params", "types"]);
        assert_eq!(lay["prevents"], json!(["Dance", "Wave"]));
        assert_eq!(lay["types"], json!([{"id":"1","animated":true,"prevents":["Sit"]}]));
        assert_eq!(json["actions"][1], json!({"id":"Talk","state":"talk"}));
        assert_eq!(json["actionOffsets"][0]["action"], "lay");
        assert_eq!(json["actionOffsets"][0]["offsets"][0], json!({"size":"h","direction":4,"x":-17,"y":17,"z":-0.9}));
        assert_eq!(json["actionOffsets"].as_array().unwrap().len(), 1, "talk has no offsets");
    }

    #[test]
    fn productdata_decodes_entities_and_escapes() {
        let json = productdata(r#"[["rare_dragon"," Dragon &amp; Lamp","Café &#39;hot&#39;\n"],["x","y","z"]]"#);
        assert_eq!(json["productdata"]["product"][0], json!({"code":"rare_dragon","name":"Dragon & Lamp","description":"Café 'hot'\n"}));
        assert_eq!(json["productdata"]["product"].as_array().unwrap().len(), 2);
        // Habbo's XML (since 2026): entities decoded, the code trimmed, empty names kept.
        let json = productdata(
            r#"<?xml version="1.0" encoding="UTF-8"?><productdata><product code=" starter_med"><name>Medium Starter Pack</name><description></description></product><product code="04Xduck"><name>Christmas &amp; Duck</name><description><![CDATA[A little <b>quack</b>]]></description></product><product code="wf_promo1"><name></name><description/></product></productdata>"#,
        );
        assert_eq!(
            json["productdata"]["product"],
            json!([
                {"code": "starter_med", "name": "Medium Starter Pack", "description": ""},
                {"code": "04Xduck", "name": "Christmas & Duck", "description": "A little <b>quack</b>"},
                {"code": "wf_promo1", "name": "", "description": ""}
            ])
        );
        // Neither format: no products.
        assert_eq!(productdata("<html>nope")["productdata"]["product"], json!([]));
    }

    #[test]
    fn lists_from_texts_and_variables() {
        let texts = "badge_name_ADM=Admin\nbadge_name_ACH_X_HHUK=No\nbadge_name_fb_HHUK=Yes\nbadge_name_bad/name=x\nquests.welcome.name=Hi\nquests.welcome.Step1.title=x\n";
        assert_eq!(badge_names(texts), ["ADM", "fb_HHUK"]);
        assert_eq!(quest_images(texts), ["welcome.png", "welcome_step1.png"]);
        let vars = "landing.view.background=${image.library.url}reception/bg.png,${image.library.url}web_promo_small/promo.jpg;x\nother=${image.library.url}../evil.png\nflash.client.url=//images.habbo.com/gordon/flash-assets-PRODUCTION-2024/\n";
        assert_eq!(library_images(vars), ["reception/bg.png", "web_promo_small/promo.jpg"]);
        assert_eq!(release(vars).as_deref(), Some("flash-assets-PRODUCTION-2024"));
        assert_eq!(key_values("# c\na=1\n=x\nb = two = 2\n"), json!({"a":"1","b":"two = 2"}));
    }

    #[test]
    fn furnidata_entries_of_both_sections() {
        let xml = r#"<furnidata><roomitemtypes><furnitype id="1" classname="chair*2"><revision>45</revision></furnitype></roomitemtypes>
            <wallitemtypes><furnitype id="2" classname="poster"><revision>7</revision></furnitype></wallitemtypes></furnidata>"#;
        assert_eq!(furnidata_entries(xml).unwrap(), [("chair*2".to_string(), 45), ("poster".to_string(), 7)]);
    }
}
