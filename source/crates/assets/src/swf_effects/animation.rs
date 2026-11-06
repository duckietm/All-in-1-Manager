//! The effect's animation XML as Nitro's `animations` (Mapper/Animation/EffectAnimationMapper.cs). The structs keep
//! the C# property order; `None` is left out (the converter serializes with WhenWritingNull), an empty list is not.

use serde::Serialize;
use serde_json::{Map, Value};

use super::assets::{child, children, parse_xml};
use super::export::{parse_bool, parse_int};

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Animation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_on_toggle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directions: Option<Vec<Direction>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadows: Option<Vec<IdOnly>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adds: Option<Vec<Add>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removes: Option<Vec<IdOnly>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprites: Option<Vec<Sprite>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<Frame>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatars: Option<Vec<Avatar>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Vec<Override>>,
}

#[derive(Debug, Default, Serialize)]
pub struct Direction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
}

/// AssetAnimationShadow and AssetAnimationRemove.
#[derive(Debug, Default, Serialize)]
pub struct IdOnly {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Add {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sprite {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directions: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction_list: Option<Vec<SpriteDirection>>,
}

#[derive(Debug, Default, Serialize)]
pub struct SpriteDirection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dx: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dz: Option<i32>,
}

#[derive(Debug, Default, Serialize)]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeats: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directions: Option<Vec<Direction>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fxs: Option<Vec<FramePart>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bodyparts: Option<Vec<FramePart>>,
}

/// AssetAnimationFramePart. `base` and `action` carry [JsonPropertyOrder(1)] and (2), the others the default 0, and
/// System.Text.Json sorts properties by that order, stably (.NET 7+; .NET 6's sort of so few is stable too): so
/// `items` comes before `base` and `action`, unlike the declaration order.
#[derive(Debug, Default, Serialize)]
pub struct FramePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dx: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dz: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dd: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<FramePartItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct FramePartItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// Background (order 1), foreground (2), ink (3): the declaration order is reversed by the property orders.
#[derive(Debug, Default, Serialize)]
pub struct Avatar {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink: Option<i32>,
}

#[derive(Debug, Default, Serialize)]
pub struct Override {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<Frame>>,
}

fn attr(node: roxmltree::Node, name: &str) -> Option<String> {
    node.attribute(name).map(str::to_string)
}

fn int(node: roxmltree::Node, name: &str) -> Option<i32> {
    node.attribute(name).and_then(parse_int)
}

/// `element.Name.LocalName.ToLower()`.
fn local(node: roxmltree::Node) -> String {
    node.tag_name().name().to_lowercase()
}

/// ParseAnimationFileAsync: `{ desc: animation }` for the root element of the animation XML (nothing when it has no
/// desc, or the XML does not parse).
pub fn parse(text: &str) -> Value {
    let mut output = Map::new();
    let Some(doc) = parse_xml(text) else { return Value::Object(output) };
    let animation = map_animation(doc.root_element());
    if let Some(desc) = animation.desc.clone().filter(|d| !d.is_empty()) {
        output.insert(desc, serde_json::to_value(&animation).expect("plain data"));
    }
    Value::Object(output)
}

fn add(node: roxmltree::Node) -> Add {
    Add {
        id: attr(node, "id"),
        align: attr(node, "align"),
        blend: attr(node, "blend"),
        ink: int(node, "ink"),
        base: attr(node, "base"),
        action: attr(node, "action"),
    }
}

/// MapAnimationXML.
pub fn map_animation(xml: roxmltree::Node) -> Animation {
    let mut animation = Animation {
        name: attr(xml, "name"),
        desc: attr(xml, "desc"),
        reset_on_toggle: xml.attribute("resetOnToggle").and_then(parse_bool),
        ..Default::default()
    };
    for element in xml.children().filter(|c| c.is_element()) {
        match local(element).as_str() {
            "direction" => {
                let list = animation.directions.get_or_insert_with(Vec::new);
                if let Some(offset) = int(element, "offset") {
                    list.push(Direction { offset: Some(offset) });
                }
            }
            "shadow" => animation.shadows.get_or_insert_with(Vec::new).push(IdOnly { id: attr(element, "id") }),
            "adds" => {
                let list = animation.adds.get_or_insert_with(Vec::new);
                list.extend(children(element, "add").map(add));
            }
            "add" => animation.adds.get_or_insert_with(Vec::new).push(add(element)),
            "remove" => animation.removes.get_or_insert_with(Vec::new).push(IdOnly { id: attr(element, "id") }),
            "avatar" => animation.avatars.get_or_insert_with(Vec::new).push(Avatar {
                background: attr(element, "background"),
                foreground: attr(element, "foreground"),
                ink: int(element, "ink"),
            }),
            "sprite" => {
                let directions: Vec<SpriteDirection> = children(element, "direction")
                    .map(|d| SpriteDirection { id: int(d, "id"), dx: int(d, "dx"), dy: int(d, "dy"), dz: int(d, "dz") })
                    .collect();
                animation.sprites.get_or_insert_with(Vec::new).push(Sprite {
                    id: attr(element, "id"),
                    directions: int(element, "directions"),
                    member: attr(element, "member"),
                    ink: int(element, "ink"),
                    static_y: int(element, "staticY"),
                    direction_list: (!directions.is_empty()).then_some(directions),
                });
            }
            "frame" => animation.frames.get_or_insert_with(Vec::new).push(map_frame(element)),
            "override" => {
                let frames = match child(element, "frames") {
                    Some(frames) => Some(children(frames, "frame").map(map_frame).collect()),
                    None => {
                        let frames: Vec<Frame> = children(element, "frame").map(map_frame).collect();
                        (!frames.is_empty()).then_some(frames)
                    }
                };
                animation.overrides.get_or_insert_with(Vec::new).push(Override {
                    name: attr(element, "name"),
                    override_: attr(element, "override"),
                    frames,
                });
            }
            _ => {}
        }
    }
    animation
}

fn frame_part(node: roxmltree::Node, bodypart: bool) -> FramePart {
    let items: Vec<FramePartItem> = if bodypart {
        children(node, "item").map(|i| FramePartItem { id: attr(i, "id"), base: attr(i, "base") }).collect()
    } else {
        Vec::new()
    };
    FramePart {
        id: attr(node, "id"),
        frame: int(node, "frame"),
        dx: int(node, "dx"),
        dy: int(node, "dy"),
        dz: int(node, "dz"),
        dd: int(node, "dd"),
        items: (!items.is_empty()).then_some(items),
        // Only a bodypart reads `base`.
        base: if bodypart { attr(node, "base") } else { None },
        action: attr(node, "action"),
    }
}

/// MapFrame: the repeats, the per-frame direction offsets, the fx and bodypart parts.
fn map_frame(node: roxmltree::Node) -> Frame {
    let mut frame = Frame { repeats: int(node, "repeats"), ..Default::default() };
    for c in node.children().filter(|c| c.is_element()) {
        match local(c).as_str() {
            "direction" => {
                if let Some(offset) = int(c, "offset") {
                    frame.directions.get_or_insert_with(Vec::new).push(Direction { offset: Some(offset) });
                }
            }
            "fx" => frame.fxs.get_or_insert_with(Vec::new).push(frame_part(c, false)),
            "bodypart" => frame.bodyparts.get_or_insert_with(Vec::new).push(frame_part(c, true)),
            _ => {}
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_element_like_the_converter() {
        let xml = r#"<animation name="Dance" desc="dance.1" resetOnToggle="True">
            <direction offset="2"/><direction offset="x"/>
            <shadow id="sd"/>
            <adds><add id="fx1" align="top" blend="ADD" ink="33" base="b"/><other/></adds>
            <ADD id="fx2" ink="no"/>
            <remove id="rh"/>
            <avatar ink="5" foreground="fg"/>
            <sprite id="s1" member="fx1" ink="33" staticY="1" directions="4"><direction id="0" dx="1" dy="-2" dz="3"/></sprite>
            <sprite id="s2"/>
            <frame repeats="2"><direction offset="1"/><fx id="fx1" frame="3" dx="1" dd="2" action="Wave" base="ignored"/>
              <bodypart id="head" action="std" base="h" frame="0"><item id="it" base="ib"/></bodypart></frame>
            <frame/>
            <override name="o1" override="rh"><frames><frame/></frames></override>
            <override name="o2"><frame repeats="1"/></override>
            <override name="o3"/>
        </animation>"#;
        let value = parse(xml);
        let expected = serde_json::json!({ "dance.1": {
            "name": "Dance", "desc": "dance.1", "resetOnToggle": true,
            "directions": [{ "offset": 2 }],
            "shadows": [{ "id": "sd" }],
            "adds": [{ "id": "fx1", "align": "top", "blend": "ADD", "ink": 33, "base": "b" }, { "id": "fx2" }],
            "removes": [{ "id": "rh" }],
            "sprites": [
                { "id": "s1", "directions": 4, "member": "fx1", "ink": 33, "staticY": 1, "directionList": [{ "id": 0, "dx": 1, "dy": -2, "dz": 3 }] },
                { "id": "s2" }
            ],
            "frames": [
                { "repeats": 2, "directions": [{ "offset": 1 }],
                  "fxs": [{ "id": "fx1", "frame": 3, "dx": 1, "dd": 2, "action": "Wave" }],
                  "bodyparts": [{ "id": "head", "frame": 0, "items": [{ "id": "it", "base": "ib" }], "base": "h", "action": "std" }] },
                {}
            ],
            "avatars": [{ "foreground": "fg", "ink": 5 }],
            "overrides": [{ "name": "o1", "override": "rh", "frames": [{}] }, { "name": "o2", "frames": [{ "repeats": 1 }] }, { "name": "o3" }]
        }});
        assert_eq!(value, expected);
        // Key order too (serde_json's preserve_order compares maps without order).
        assert_eq!(super::super::json::to_string(&value), super::super::json::to_string(&expected));
        assert_eq!(parse("<animation name=\"x\"/>"), serde_json::json!({}));
        assert_eq!(parse("not xml"), serde_json::json!({}));
    }
}
