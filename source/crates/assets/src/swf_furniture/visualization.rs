//! The visualizations of the furni (Mapper/Visualizations): layers, directions, animations and colours, mapped and
//! written straight to their JSON (property order and null handling as their C# classes declare them).

use anyhow::Result;

use super::json::{Dict, Json, Obj};
use super::names::{convert_hex, parse_double, parse_int};
use super::xml::{element, elements};

fn int(value: Option<i32>) -> Option<Json> {
    value.map(Json::int)
}

/// Dictionary<int, T> written as an object (keys as text) in insertion order.
fn dict(d: Dict<i32, Json>) -> Json {
    Json::Obj(d.items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Layer: ink, alpha, z (a double), x, y, tag, ignoreMouse; each left out when null.
fn layer(xml: roxmltree::Node) -> Json {
    Obj::new()
        .opt("ink", xml.attribute("ink").map(Json::str))
        .opt("alpha", int(parse_int(xml.attribute("alpha"))))
        .opt("z", parse_double(xml.attribute("z")).map(Json::double))
        .opt("x", int(parse_int(xml.attribute("x"))))
        .opt("y", int(parse_int(xml.attribute("y"))))
        .opt("tag", xml.attribute("tag").map(Json::str))
        .opt("ignoreMouse", (xml.attribute("ignoreMouse") == Some("1")).then(|| Json::bool(true)))
        .build()
}

/// The <layer> children with an integer id (a later duplicate id replaces the value in place).
fn layers(parent: Option<roxmltree::Node>) -> Json {
    let mut d = Dict::new();
    if let Some(parent) = parent {
        for l in elements(parent, "layer") {
            if let Some(id) = parse_int(l.attribute("id")) {
                d.insert(id, layer(l));
            }
        }
    }
    dict(d)
}

/// Frame (FrameConverter): id, then x, y, randomX, randomY when present.
fn frame(xml: roxmltree::Node) -> Json {
    Obj::new()
        .put("id", Json::int(parse_int(xml.attribute("id")).unwrap_or(0)))
        .opt("x", int(parse_int(xml.attribute("x"))))
        .opt("y", int(parse_int(xml.attribute("y"))))
        .opt("randomX", int(parse_int(xml.attribute("randomX"))))
        .opt("randomY", int(parse_int(xml.attribute("randomY"))))
        .build()
}

/// FrameSequence: loopCount, random, frames (indexed 0.., always written).
fn frame_sequence(xml: roxmltree::Node) -> Json {
    let frames = elements(xml, "frame").enumerate().map(|(i, f)| (i.to_string(), frame(f))).collect();
    Obj::new()
        .opt("loopCount", int(parse_int(xml.attribute("loopCount"))))
        .opt("random", int(parse_int(xml.attribute("random"))))
        .put("frames", Json::Obj(frames))
        .build()
}

/// AnimationLayer: frameRepeat, loopCount, random, frameSequences (indexed 0..; left out when there are none).
fn animation_layer(xml: roxmltree::Node) -> Json {
    let sequences: Vec<(String, Json)> = elements(xml, "frameSequence").enumerate().map(|(i, s)| (i.to_string(), frame_sequence(s))).collect();
    Obj::new()
        .opt("frameRepeat", int(parse_int(xml.attribute("frameRepeat"))))
        .opt("loopCount", int(parse_int(xml.attribute("loopCount"))))
        .opt("random", int(parse_int(xml.attribute("random"))))
        .opt("frameSequences", (!sequences.is_empty()).then_some(Json::Obj(sequences)))
        .build()
}

/// Animation: transitionTo, transitionFrom, immediateChangeFrom, randomStart (true or left out), layers (always).
fn animation(xml: roxmltree::Node) -> Json {
    let mut layers = Dict::new();
    for l in elements(xml, "animationLayer") {
        if let Some(id) = parse_int(l.attribute("id")) {
            layers.insert(id, animation_layer(l));
        }
    }
    Obj::new()
        .opt("transitionTo", int(parse_int(xml.attribute("transitionTo"))))
        .opt("transitionFrom", int(parse_int(xml.attribute("transitionFrom"))))
        .opt("immediateChangeFrom", xml.attribute("immediateChangeFrom").map(Json::str))
        .opt("randomStart", (xml.attribute("randomStart") == Some("1")).then(|| Json::bool(true)))
        .put("layers", dict(layers))
        .build()
}

/// Color: layers { id: { color } }, the colour hex parsed by Convert.ToInt32(hex, 16) (0 when missing or empty; a
/// bad one throws in the C#, failing the conversion).
fn color(xml: roxmltree::Node) -> Result<Json> {
    let mut layers = Dict::new();
    for l in elements(xml, "colorLayer") {
        if let Some(id) = parse_int(l.attribute("id")) {
            let value = match l.attribute("color") {
                Some(hex) if !hex.is_empty() => convert_hex(hex)?,
                _ => 0,
            };
            layers.insert(id, Obj::new().put("color", Json::int(value)).build());
        }
    }
    Ok(Obj::new().put("layers", dict(layers)).build())
}

/// One <visualization>: angle, layerCount, size, layers (always), directions (always, each { layers }),
/// animations (left out when there are none), colors (a SortedDictionary: by id, always).
fn visualization(xml: roxmltree::Node) -> Result<Json> {
    let mut directions = Dict::new();
    if let Some(d) = element(xml, "directions") {
        for direction in elements(d, "direction") {
            if let Some(id) = parse_int(direction.attribute("id")) {
                directions.insert(id, Obj::new().put("layers", layers(Some(direction))).build());
            }
        }
    }
    let mut animations = Dict::new();
    if let Some(a) = element(xml, "animations") {
        for anim in elements(a, "animation") {
            if let Some(id) = parse_int(anim.attribute("id")) {
                animations.insert(id, animation(anim));
            }
        }
    }
    let mut colors = std::collections::BTreeMap::new();
    if let Some(c) = element(xml, "colors") {
        for col in elements(c, "color") {
            if let Some(id) = parse_int(col.attribute("id")) {
                colors.insert(id, color(col)?);
            }
        }
    }
    Ok(Obj::new()
        .put("angle", Json::int(parse_int(xml.attribute("angle")).unwrap_or(0)))
        .put("layerCount", Json::int(parse_int(xml.attribute("layerCount")).unwrap_or(0)))
        .put("size", Json::int(parse_int(xml.attribute("size")).unwrap_or(0)))
        .put("layers", layers(element(xml, "layers")))
        .put("directions", dict(directions))
        .opt("animations", (!animations.is_empty()).then(|| dict(animations)))
        .put("colors", Json::Obj(colors.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))
        .build())
}

/// VisualizationsMapper.MapVisualizationsXml: the <visualization>s of every <graphics>, then the ones directly
/// under the root; no size is excluded.
pub fn map(root: roxmltree::Node) -> Result<Json> {
    let list = elements(root, "graphics").flat_map(|g| elements(g, "visualization")).chain(elements(root, "visualization"));
    Ok(Json::Arr(list.map(visualization).collect::<Result<_>>()?))
}
