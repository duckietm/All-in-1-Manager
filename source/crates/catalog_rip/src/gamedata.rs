//! Nitro gamedata: furni entries in the exact shape of a Nitro hotel's FurnitureData.json and
//! products in the shape of its ProductData.json, so the files the ripper writes can be appended
//! to the hotel's own without hand-editing.
//!
//! A floor entry has, in this order: id, classname, revision, category, defaultdir, xdim, ydim,
//! partcolors (`{"color":[..]}`, left out when the furni has no colours), name, description,
//! adurl, offerid, buyout, rentofferid, rentbuyout, bc, excludeddynamic, bcofferid,
//! customparams, specialtype, canstandon, cansiton, canlayon, canputstuffon, height (a float),
//! furniline, environment, rare, tradeable, recyclable. A wall entry: id, classname, revision,
//! category, name, description, adurl, specialtype, furniline, environment, rare, tradeable,
//! recyclable, offerid, buyout, rentofferid, rentbuyout, bc, excludeddynamic, bcofferid.
//!
//! A field the source lacks gets the value Nitro hotels use for "nothing": adurl and
//! customparams `null` (an empty string becomes `null` too), environment `null`, furniline `""`,
//! category `"other"`, offer ids `-1`, specialtype 1, flags `false` except tradeable and
//! recyclable, height 1.0, bcofferid the offer id for a Builders Club furni and -1 otherwise.
//! The text is compact JSON with floats written as Python writes them (`1.0`, `1e-06`), as the
//! hotels' converters do.

use serde_json::{json, Map, Value};

/// A furnidata entry (official / retro JSON, or the fields of a Flash XML entry) as a Nitro
/// FurnitureData.json entry; `kind` is `s` (floor) or `i` (wall). `product` (name, description
/// from the productdata) fills a name or description the entry lacks.
pub fn nitro_furni(kind: char, source: &Value, product: Option<(&str, &str)>) -> Value {
    let get = |key: &str| source.get(key);
    let int = |key: &str, default: i64| get(key).and_then(as_int).unwrap_or(default);
    let flag = |key: &str, default: bool| get(key).and_then(as_bool).unwrap_or(default);
    // A string, or null when the source says null; `default` when the source lacks it.
    let text = |key: &str, default: Value| match get(key) {
        None => default,
        Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(s),
        Some(Value::Number(n)) => json!(n.to_string()),
        Some(Value::Bool(b)) => json!(if *b { "1" } else { "0" }),
        Some(_) => default,
    };
    // adurl and customparams: Nitro hotels write null, never "".
    let optional = |key: &str| match text(key, Value::Null) {
        Value::String(s) if s.is_empty() => Value::Null,
        v => v,
    };
    let product_text = |i: usize| {
        product
            .map(|(n, d)| if i == 0 { n } else { d })
            .filter(|s| !s.is_empty())
            .map(|s| json!(s))
            .unwrap_or(Value::Null)
    };
    let name = match text("name", Value::Null) {
        Value::Null => product_text(0),
        v => v,
    };
    let description = match text("description", Value::Null) {
        Value::Null => product_text(1),
        v => v,
    };
    let offerid = int("offerid", -1);
    let bc = flag("bc", false);
    let bcofferid = int("bcofferid", if bc { offerid } else { -1 });

    let mut e = Map::new();
    e.insert("id".into(), json!(int("id", 0)));
    e.insert("classname".into(), text("classname", json!("")));
    e.insert("revision".into(), json!(int("revision", 0)));
    e.insert("category".into(), text("category", json!("other")));
    if kind == 's' {
        e.insert("defaultdir".into(), json!(int("defaultdir", 0)));
        e.insert("xdim".into(), json!(int("xdim", 1)));
        e.insert("ydim".into(), json!(int("ydim", 1)));
        let colors = part_colors(get("partcolors"));
        if !colors.is_empty() {
            e.insert("partcolors".into(), json!({ "color": colors }));
        }
        e.insert("name".into(), name);
        e.insert("description".into(), description);
        e.insert("adurl".into(), optional("adurl"));
        e.insert("offerid".into(), json!(offerid));
        e.insert("buyout".into(), json!(flag("buyout", false)));
        e.insert("rentofferid".into(), json!(int("rentofferid", -1)));
        e.insert("rentbuyout".into(), json!(flag("rentbuyout", false)));
        e.insert("bc".into(), json!(bc));
        e.insert("excludeddynamic".into(), json!(flag("excludeddynamic", false)));
        e.insert("bcofferid".into(), json!(bcofferid));
        e.insert("customparams".into(), optional("customparams"));
        e.insert("specialtype".into(), json!(int("specialtype", 1)));
        e.insert("canstandon".into(), json!(flag("canstandon", false)));
        e.insert("cansiton".into(), json!(flag("cansiton", false)));
        e.insert("canlayon".into(), json!(flag("canlayon", false)));
        e.insert("canputstuffon".into(), json!(flag("canputstuffon", false)));
        e.insert("height".into(), json!(get("height").and_then(as_float).unwrap_or(1.0)));
        e.insert("furniline".into(), text("furniline", json!("")));
        e.insert("environment".into(), text("environment", Value::Null));
        e.insert("rare".into(), json!(flag("rare", false)));
        e.insert("tradeable".into(), json!(flag("tradeable", true)));
        e.insert("recyclable".into(), json!(flag("recyclable", true)));
    } else {
        e.insert("name".into(), name);
        e.insert("description".into(), description);
        e.insert("adurl".into(), optional("adurl"));
        e.insert("specialtype".into(), json!(int("specialtype", 1)));
        e.insert("furniline".into(), text("furniline", json!("")));
        e.insert("environment".into(), text("environment", Value::Null));
        e.insert("rare".into(), json!(flag("rare", false)));
        e.insert("tradeable".into(), json!(flag("tradeable", true)));
        e.insert("recyclable".into(), json!(flag("recyclable", true)));
        e.insert("offerid".into(), json!(offerid));
        e.insert("buyout".into(), json!(flag("buyout", false)));
        e.insert("rentofferid".into(), json!(int("rentofferid", -1)));
        e.insert("rentbuyout".into(), json!(flag("rentbuyout", false)));
        e.insert("bc".into(), json!(bc));
        e.insert("excludeddynamic".into(), json!(flag("excludeddynamic", false)));
        e.insert("bcofferid".into(), json!(bcofferid));
    }
    Value::Object(e)
}

/// A ProductData.json entry.
pub fn nitro_product(code: &str, name: &str, description: &str) -> Value {
    json!({ "code": code, "name": name, "description": description })
}

/// `{"roomitemtypes":{"furnitype":[..]},"wallitemtypes":{"furnitype":[..]}}`.
pub fn furniture_data(floor: Vec<Value>, wall: Vec<Value>) -> Value {
    json!({
        "roomitemtypes": { "furnitype": floor },
        "wallitemtypes": { "furnitype": wall },
    })
}

/// `{"productdata":{"product":[..]}}`.
pub fn product_data(products: Vec<Value>) -> Value {
    json!({ "productdata": { "product": products } })
}

/// Compact JSON as the hotels' gamedata files have it (one line, no spaces, UTF-8 text,
/// floats as Python writes them).
pub fn to_text(value: &Value) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonFloats);
    serde::Serialize::serialize(value, &mut serializer).expect("JSON values always serialize");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

struct PythonFloats;

impl serde_json::ser::Formatter for PythonFloats {
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, value: f64) -> std::io::Result<()> {
        writer.write_all(python_float(value).as_bytes())
    }
}

/// A float as Python's `repr` writes it: the shortest digits that read back the same, with an
/// exponent below 1e-4 and from 1e16 (`1.0`, `0.0001`, `1e-05`, `1.5e+16`).
pub fn python_float(value: f64) -> String {
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..16).contains(&exponent) {
        let plain = format!("{value}");
        if plain.contains('.') {
            plain
        } else {
            format!("{plain}.0")
        }
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn as_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
}

/// The colours of `partcolors`: `{"color":[..]}`, a bare list, or one colour.
fn part_colors(value: Option<&Value>) -> Vec<Value> {
    let list = match value {
        Some(Value::Object(o)) => o.get("color"),
        other => other,
    };
    let one = |v: &Value| match v {
        Value::String(s) => Some(json!(s)),
        Value::Number(n) => Some(json!(n.to_string())),
        _ => None,
    };
    match list {
        Some(Value::Array(colors)) => colors.iter().filter_map(one).collect(),
        Some(v) => one(v).into_iter().collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entries copied unchanged from a Nitro hotel's FurnitureData.json: floor with and without
    /// colours, null name, Python floats, customparams, a wall entry with an adurl.
    const FLOOR: &[&str] = &[
        r##"{"id":13,"classname":"shelves_norja","revision":61856,"category":"shelf","defaultdir":0,"xdim":1,"ydim":1,"partcolors":{"color":["#ffffff","#F7EBBC"]},"name":"Libreria Iced","description":"Per i tuoi libri e oggetti art deco","adurl":null,"offerid":5,"buyout":true,"rentofferid":-1,"rentbuyout":false,"bc":true,"excludeddynamic":false,"bcofferid":5,"customparams":null,"specialtype":1,"canstandon":false,"cansiton":false,"canlayon":false,"canputstuffon":false,"height":1.0,"furniline":"iced","environment":"","rare":false,"tradeable":true,"recyclable":true}"##,
    ];

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/nitro_furnituredata.json")).unwrap()
    }

    #[test]
    fn reference_entries_come_out_byte_identical() {
        let reference = include_str!("../tests/fixtures/nitro_furnituredata.json").trim_end();
        let data = fixture();
        let mut floor = Vec::new();
        let mut wall = Vec::new();
        for (section, kind, out) in [("roomitemtypes", 's', &mut floor), ("wallitemtypes", 'i', &mut wall)] {
            for entry in data[section]["furnitype"].as_array().unwrap() {
                let nitro = nitro_furni(kind, entry, None);
                assert_eq!(to_text(&nitro), to_text(entry), "key order and values");
                out.push(nitro);
            }
        }
        assert!(floor.len() >= 5 && wall.len() >= 2);
        assert_eq!(to_text(&furniture_data(floor, wall)), reference, "the whole file");
        let first: Value = serde_json::from_str(FLOOR[0]).unwrap();
        assert_eq!(to_text(&nitro_furni('s', &first, None)), FLOOR[0]);
    }

    #[test]
    fn floats_as_python_writes_them() {
        for (value, text) in [
            (1.0, "1.0"),
            (0.0, "0.0"),
            (1.2, "1.2"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.000001, "1e-06"),
            (2.5e-7, "2.5e-07"),
            (-0.5, "-0.5"),
            (1e16, "1e+16"),
            (123456.75, "123456.75"),
        ] {
            assert_eq!(python_float(value), text);
        }
    }

    #[test]
    fn official_json_gets_every_field() {
        // An official furnidata_json entry: no bcofferid / canputstuffon / height / tradeable /
        // recyclable, "" for adurl and customparams, empty partcolors.
        let official: Value = serde_json::from_str(
            r#"{"id":4100,"classname":"rare_dragonlamp*4","revision":1,"category":"lighting","defaultdir":0,"xdim":1,"ydim":1,
                "partcolors":{"color":[]},"name":"Dragon","description":"Fire","adurl":"","offerid":77,"buyout":true,
                "rentofferid":-1,"rentbuyout":false,"bc":true,"excludeddynamic":false,"customparams":"","specialtype":1,
                "canstandon":false,"cansiton":false,"canlayon":false,"furniline":"rare","environment":"","rare":true,"extra":1}"#,
        )
        .unwrap();
        assert_eq!(
            to_text(&nitro_furni('s', &official, None)),
            r#"{"id":4100,"classname":"rare_dragonlamp*4","revision":1,"category":"lighting","defaultdir":0,"xdim":1,"ydim":1,"name":"Dragon","description":"Fire","adurl":null,"offerid":77,"buyout":true,"rentofferid":-1,"rentbuyout":false,"bc":true,"excludeddynamic":false,"bcofferid":77,"customparams":null,"specialtype":1,"canstandon":false,"cansiton":false,"canlayon":false,"canputstuffon":false,"height":1.0,"furniline":"rare","environment":"","rare":true,"tradeable":true,"recyclable":true}"#
        );
        let bare: Value = serde_json::from_str(r#"{"id":"9","classname":"poster"}"#).unwrap();
        assert_eq!(
            to_text(&nitro_furni('i', &bare, Some(("Poster", "On the wall")))),
            r#"{"id":9,"classname":"poster","revision":0,"category":"other","name":"Poster","description":"On the wall","adurl":null,"specialtype":1,"furniline":"","environment":null,"rare":false,"tradeable":true,"recyclable":true,"offerid":-1,"buyout":false,"rentofferid":-1,"rentbuyout":false,"bc":false,"excludeddynamic":false,"bcofferid":-1}"#
        );
    }

    #[test]
    fn products() {
        assert_eq!(
            to_text(&product_data(vec![nitro_product("chair", "Chair", ""), nitro_product("a\"b", "Ä", "x")])),
            r#"{"productdata":{"product":[{"code":"chair","name":"Chair","description":""},{"code":"a\"b","name":"Ä","description":"x"}]}}"#
        );
    }
}
