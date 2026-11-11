//! Synthetic furniture SWFs, with the output derived by hand from the C# mappers.

use std::io::Write;

use super::json::NEWLINE;
use super::*;

fn tag(code: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&((code << 6) | 0x3F).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[derive(Default)]
struct Builder {
    tags: Vec<u8>,
    symbols: Vec<(u16, String)>,
}

impl Builder {
    fn binary(&mut self, id: u16, symbol: &str, text: &str) -> &mut Self {
        let mut payload = id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(text.as_bytes());
        self.tags.extend(tag(87, &payload));
        self.symbols.push((id, symbol.into()));
        self
    }

    /// A DefineBitsLossless2 ARGB bitmap, opaque, every pixel `rgb` (the first pixel `first`).
    fn bitmap(&mut self, id: u16, symbols: &[&str], w: u16, h: u16, rgb: [u8; 3], first: [u8; 4]) -> &mut Self {
        let mut payload = id.to_le_bytes().to_vec();
        payload.push(5);
        payload.extend_from_slice(&w.to_le_bytes());
        payload.extend_from_slice(&h.to_le_bytes());
        let mut pixels = Vec::new();
        for i in 0..(w as usize * h as usize) {
            if i == 0 {
                pixels.extend_from_slice(&first);
            } else {
                pixels.extend_from_slice(&[255, rgb[0], rgb[1], rgb[2]]);
            }
        }
        payload.extend(zlib(&pixels));
        self.tags.extend(tag(36, &payload));
        for s in symbols {
            self.symbols.push((id, s.to_string()));
        }
        self
    }

    fn symbol(&mut self, id: u16, symbol: &str) -> &mut Self {
        self.symbols.push((id, symbol.into()));
        self
    }

    fn swf(&self) -> Swf {
        let mut body = vec![0x00, 0, 24, 1, 0];
        body.extend_from_slice(&self.tags);
        let mut symbols = (self.symbols.len() as u16).to_le_bytes().to_vec();
        for (id, name) in &self.symbols {
            symbols.extend_from_slice(&id.to_le_bytes());
            symbols.extend_from_slice(name.as_bytes());
            symbols.push(0);
        }
        body.extend(tag(76, &symbols));
        body.extend_from_slice(&[0, 0]);
        let mut out = b"CWS\x0a".to_vec();
        out.extend_from_slice(&((body.len() + 8) as u32).to_le_bytes());
        out.extend(zlib(&body));
        crate::swf::read(&out, true).unwrap()
    }
}

const INDEX: &str = r#"<?xml version="1.0" encoding="UTF-8"?><object type="chair" visualization="furniture_static" logic="furniture_basic"/>"#;

const ASSETS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<assets>
  <asset name="chair_64_a_0_0" x="-10" y="20"/>
  <asset name="chair_64_a_2_0" x="5" y="6"/>
  <asset name="chair_64_a_4_0" source="chair_64_a_2_0" x="7" y="6" flipH="1"/>
  <asset name="chair_64_b_0_0" x="1" y="2"/>
  <asset name="chair_64_b_2_0" x="3" y="4.5"/>
  <asset name="chair_64_c_0_0" x="0" y="0"/>
</assets>"#;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest><library name="chair" version="0.0.0"><assets>
  <asset name="chair_64_a_0_0" mimeType="image/png"/>
  <asset name="chair_64_a_2_0" mimeType="image/png"/>
  <asset name="chair_64_b_0_0" mimeType="image/png"/>
  <asset name="chair_64_b_2_0" mimeType="image/png"/>
  <asset name="chair_logic" mimeType="text/xml"/>
</assets></library></manifest>"#;

const LOGIC: &str = r#"<objectData type="chair">
  <model>
    <dimensions x="1" y="1" z="0.5" centerZ="0.25"/>
    <directions><direction id="0"/><direction id="2"/></directions>
  </model>
  <action link="http://x" startState="1"/>
  <sound><sample id="12" nopitch="true"/></sound>
  <credits value="me"/>
  <customvars><variable name="a"/><variable name=""/></customvars>
</objectData>"#;

const VISUALIZATION: &str = r#"<visualizationData type="chair"><graphics>
  <visualization size="64" layerCount="1" angle="45">
    <layers><layer id="0" z="0.5" alpha="128" ink="ADD" ignoreMouse="1"/></layers>
    <directions><direction id="0"/><direction id="2"><layer id="0" z="-1"/></direction></directions>
  </visualization>
  <visualization size="1" layerCount="1" angle="45"/>
</graphics></visualizationData>"#;

fn chair() -> Builder {
    chair_with(VISUALIZATION)
}

fn chair_with(visualization: &str) -> Builder {
    let mut b = Builder::default();
    b.symbol(0, "chair");
    b.binary(1, "chair_chair_index", INDEX)
        .binary(2, "chair_chair_assets", ASSETS)
        .binary(3, "chair_chair_logic", LOGIC)
        .binary(4, "chair_chair_visualization", visualization)
        .binary(5, "chair_manifest", MANIFEST)
        .bitmap(6, &["chair_chair_64_a_0_0"], 2, 1, [10, 20, 30], [255, 1, 2, 3])
        .bitmap(7, &["chair_chair_64_a_2_0"], 1, 2, [40, 50, 60], [0, 9, 9, 9])
        .bitmap(8, &["chair_chair_64_b_0_0", "chair_chair_64_b_2_0"], 3, 3, [70, 80, 90], [255, 70, 80, 90])
        .bitmap(10, &["sh_chair"], 1, 1, [1, 1, 1], [255, 1, 1, 1]);
    b
}

fn convert_json(name: &str, swf: &Swf) -> (String, RgbaImage) {
    let files = convert(name, swf).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, format!("{name}.json"));
    assert_eq!(files[1].0, format!("{name}.png"));
    let sheet = image::load_from_memory(&files[1].1).unwrap().to_rgba8();
    (String::from_utf8(files[0].1.clone()).unwrap(), sheet)
}

fn frame(x: u32, y: u32, w: u32, h: u32) -> String {
    format!(
        r#"{{
        "frame": {{
          "x": {x},
          "y": {y},
          "w": {w},
          "h": {h}
        }},
        "rotated": false,
        "trimmed": false,
        "spriteSourceSize": {{
          "x": 0,
          "y": 0,
          "w": {w},
          "h": {h}
        }},
        "sourceSize": {{
          "w": {w},
          "h": {h}
        }},
        "pivot": {{
          "x": 0.5,
          "y": 0.5
        }}
      }}"#
    )
}

#[test]
fn plain_furni_whole_json_and_sheet() {
    let (json, sheet) = convert_json("chair", &chair().swf());
    let expected = r#"{
  "name": "chair",
  "logicType": "furniture_basic",
  "visualizationType": "furniture_static",
  "assets": {
    "chair_64_a_0_0": {
      "x": -10,
      "y": 20
    },
    "chair_64_a_2_0": {
      "x": 5,
      "y": 6
    },
    "chair_64_a_4_0": {
      "source": "chair_64_a_2_0",
      "x": 7,
      "y": 6,
      "flipH": true
    },
    "chair_64_b_0_0": {
      "x": 1,
      "y": 2
    },
    "chair_64_b_2_0": {
      "source": "chair_64_b_0_0",
      "x": 3,
      "y": 0
    }
  },
  "logic": {
    "model": {
      "dimensions": {
        "x": 1,
        "y": 1,
        "z": 0.5,
        "centerZ": 0.25
      },
      "directions": [
        0,
        2
      ]
    },
    "action": {
      "link": "http://x",
      "startState": 1
    },
    "credits": "me",
    "soundSample": {
      "id": 12,
      "noPitch": true
    },
    "customVars": {
      "variables": [
        "a"
      ]
    }
  },
  "visualizations": [
    {
      "angle": 45,
      "layerCount": 1,
      "size": 64,
      "layers": {
        "0": {
          "ink": "ADD",
          "alpha": 128,
          "z": 0.5,
          "ignoreMouse": true
        }
      },
      "directions": {
        "0": {
          "layers": {}
        },
        "2": {
          "layers": {
            "0": {
              "z": -1
            }
          }
        }
      },
      "colors": {}
    },
    {
      "angle": 45,
      "layerCount": 1,
      "size": 1,
      "layers": {},
      "directions": {},
      "colors": {}
    }
  ],
  "spritesheet": {
    "frames": {
      "chair_chair_64_a_0_0": FRAME_A0,
      "chair_chair_64_a_2_0": FRAME_A2,
      "chair_chair_64_b_0_0": FRAME_B0
    },
    "meta": {
      "image": "chair.png",
      "format": "RGBA8888",
      "size": {
        "w": 7,
        "h": 14
      },
      "scale": 1,
      "converter": "All-in-1-download-tool"
    }
  }
}"#
    .replace("FRAME_A0", &frame(2, 2, 2, 1))
    .replace("FRAME_A2", &frame(2, 5, 1, 2))
    .replace("FRAME_B0", &frame(2, 9, 3, 3))
    .replace('\n', NEWLINE);
    assert_eq!(json, expected);

    // One image per row (3 images: ceil(3 / 10) = 1), 2 px apart, the edges extruded 1 px.
    assert_eq!(sheet.dimensions(), (7, 14));
    assert_eq!(sheet.get_pixel(2, 2).0, [1, 2, 3, 255]);
    assert_eq!(sheet.get_pixel(3, 2).0, [10, 20, 30, 255]);
    assert_eq!(sheet.get_pixel(1, 1).0, [1, 2, 3, 255], "corner extruded");
    assert_eq!(sheet.get_pixel(4, 2).0, [10, 20, 30, 255], "right edge extruded");
    assert_eq!(sheet.get_pixel(5, 2).0, [0, 0, 0, 0]);
    assert_eq!(sheet.get_pixel(2, 5).0, [0, 0, 0, 0], "a transparent pixel is cleared");
    assert_eq!(sheet.get_pixel(2, 6).0, [40, 50, 60, 255]);
    assert_eq!(sheet.get_pixel(4, 12).0, [70, 80, 90, 255], "bottom edge extruded");
    assert_eq!(sheet.get_pixel(4, 13).0, [0, 0, 0, 0]);
    assert_eq!(sheet.get_pixel(0, 0).0, [0, 0, 0, 0]);

    let files = convert("chair", &chair().swf()).unwrap();
    let hab = crate::hab::write("chair", &files);
    assert_eq!(crate::hab::read(&hab).unwrap(), files);
}

fn value(json: &str) -> serde_json::Value {
    serde_json::from_str(json).unwrap()
}

/// Compared as compact text, so that the property order counts.
fn same(actual: &serde_json::Value, expected: serde_json::Value) {
    assert_eq!(serde_json::to_string(actual).unwrap(), serde_json::to_string(&expected).unwrap());
}

#[test]
fn colours_and_animations() {
    let visualization = r#"<visualizationData><graphics><visualization size="64" layerCount="2" angle="45">
      <layers><layer id="0" z="0"/><layer id="1" x="0" y="-3" tag="glow" alpha="0"/></layers>
      <colors>
        <color id="2"><colorLayer id="0" color="0xFF0000"/><colorLayer id="1" color=""/></color>
        <color id="1"><colorLayer id="1" color="ffffff"/><colorLayer id="0" color="00FF00"/></color>
      </colors>
      <animations>
        <animation id="1" transitionTo="2" randomStart="1">
          <animationLayer id="1" frameRepeat="2" loopCount="0">
            <frameSequence loopCount="3"><frame id="0"/><frame id="1" x="2" randomY="4"/></frameSequence>
            <frameSequence random="1"/>
          </animationLayer>
          <animationLayer id="2"/>
        </animation>
        <animation id="0" immediateChangeFrom="3,4"/>
        <animation id="x"/>
      </animations>
    </visualization></graphics>
    <visualization size="32" layerCount="1" angle="45"><animations/></visualization></visualizationData>"#;
    let (json, _) = convert_json("chair", &chair_with(visualization).swf());
    let v = value(&json);
    same(
        &v["visualizations"],
        serde_json::json!([
            {
                "angle": 45, "layerCount": 2, "size": 64,
                "layers": { "0": { "z": 0 }, "1": { "alpha": 0, "x": 0, "y": -3, "tag": "glow" } },
                "directions": {},
                "animations": {
                    "1": {
                        "transitionTo": 2, "randomStart": true,
                        "layers": {
                            "1": {
                                "frameRepeat": 2, "loopCount": 0,
                                "frameSequences": {
                                    "0": { "loopCount": 3, "frames": { "0": { "id": 0 }, "1": { "id": 1, "x": 2, "randomY": 4 } } },
                                    "1": { "random": 1, "frames": {} }
                                }
                            },
                            "2": {}
                        }
                    },
                    "0": { "immediateChangeFrom": "3,4", "layers": {} }
                },
                "colors": {
                    "1": { "layers": { "1": { "color": 16777215 }, "0": { "color": 65280 } } },
                    "2": { "layers": { "0": { "color": 16711680 }, "1": { "color": 0 } } }
                }
            },
            { "angle": 45, "layerCount": 1, "size": 32, "layers": {}, "directions": {}, "colors": {} }
        ]),
    );
    // The layer z is a double: "0", not "0.0".
    assert!(json.contains("\"z\": 0\r\n") || json.contains("\"z\": 0\n"));
}

#[test]
fn a_bad_colour_fails_the_conversion() {
    let mut b = Builder::default();
    b.binary(1, "x_x_index", INDEX)
        .binary(2, "x_x_visualization", r##"<v><visualization size="64"><colors><color id="1"><colorLayer id="0" color="#FF"/></color></colors></visualization></v>"##)
        .bitmap(3, &["x_x_64_a_0_0"], 1, 1, [0, 0, 0], [255, 0, 0, 0]);
    assert!(convert("x", &b.swf()).is_err());
}

#[test]
fn flipped_assets_aliases_and_the_image_restorer() {
    let assets = r#"<assets>
      <asset name="cf_5_64_a_0_0" x="1" y="1"/>
      <asset name="cf_5_64_a_2_0" source="cf_5_64_a_0_0" flipH="1" x="2" y="2"/>
      <asset name="cf_5_64_a_4_0" source="cf_5_64_a_2_0" flipH="1" flipV="1" x="3" y="3"/>
      <asset name="cf_5_64_a_6_0" x="4" y="4"/>
      <asset name="cf_5_64_b_0_0" source="" x="5" y="5"/>
    </assets>"#;
    let manifest = r#"<manifest><library><assets>
      <asset name="CF_5_64_a_0_0" mimeType="image/png"/><asset name="cf_5_64_a_6_0" mimeType="image/png"/>
    </assets></library></manifest>"#;
    let mut b = Builder::default();
    b.symbol(0, "cf_5");
    b.binary(1, "cf_5_cf_5_index", r#"<object type="cf_5" logic="furniture_basic"/>"#)
        .binary(2, "cf_5_cf_5_assets", assets)
        .binary(3, "cf_5_manifest", manifest)
        // An alias: cf_5_64_a_6_0 draws the image of cf_5_64_a_0_0 (its source, from the symbols).
        .bitmap(4, &["cf_5_cf_5_64_a_0_0", "cf_5_cf_5_64_a_6_0"], 1, 1, [1, 1, 1], [255, 1, 1, 1])
        // Same name in another case: the move back from tmp/ fails, then the restorer replaces the first.
        .bitmap(5, &["cf_5_cf_5_64_x"], 1, 1, [2, 2, 2], [255, 2, 2, 2])
        .bitmap(6, &["CF_5_cf_5_64_X"], 2, 1, [3, 3, 3], [255, 3, 3, 3])
        // No symbol: never reaches the sheet.
        .bitmap(7, &[], 1, 1, [4, 4, 4], [255, 4, 4, 4]);
    let (json, sheet) = convert_json("cf_5", &b.swf());
    let v = value(&json);
    // Asset names from the XML get "cf_" upper-cased; the second symbol of bitmap 4 makes a_6_0 an alias of a_0_0;
    // sources
    // resolve to the end of their chain (a_4_0 -> a_2_0 -> a_0_0).
    same(
        &v["assets"],
        serde_json::json!({
            "CF_5_64_a_0_0": { "x": 1, "y": 1 },
            "CF_5_64_a_2_0": { "source": "CF_5_64_a_0_0", "x": 2, "y": 2, "flipH": true },
            "CF_5_64_a_4_0": { "source": "CF_5_64_a_0_0", "x": 3, "y": 3, "flipH": true, "flipV": true },
            "CF_5_64_a_6_0": { "source": "CF_5_64_a_0_0", "x": 4, "y": 4 },
            "CF_5_64_b_0_0": { "source": "", "x": 5, "y": 5 }
        }),
    );
    assert!(v.get("visualizations").is_none());
    assert!(v.get("visualizationType").is_none());
    same(&v["logic"], serde_json::json!({ "model": {} }));
    // Frames: "cf_5_cf_5_64_a_0_0" (bitmap 4, under its first name), then the restored "CF_5_CF_5_64_X" (bitmap 6)
    // in place of "cf_5_cf_5_64_x". The keys: cleaned, looked up in the canonical mapping (lower-cased full names),
    // then "cf_" upper-cased.
    let frames: Vec<&String> = v["spritesheet"]["frames"].as_object().unwrap().keys().collect();
    assert_eq!(frames, ["CF_5_CF_5_64_a_0_0", "CF_5_CF_5_64_x"]);
    assert_eq!(v["spritesheet"]["frames"]["CF_5_CF_5_64_x"]["frame"]["w"], 2);
    assert_eq!(sheet.get_pixel(2, 5).0, [3, 3, 3, 255]);
}

#[test]
fn the_alias_mapping_needs_the_assets_xml() {
    // Without a manifest there are no assets (left out) and no image mapping, so the colliding image stays in tmp/.
    let mut b = Builder::default();
    b.symbol(0, "x");
    b.binary(1, "x_x_index", INDEX)
        .bitmap(5, &["x_a"], 1, 1, [2, 2, 2], [255, 2, 2, 2])
        .bitmap(6, &["X_A"], 2, 1, [3, 3, 3], [255, 3, 3, 3]);
    let (json, _) = convert_json("x", &b.swf());
    let v = value(&json);
    assert!(v.get("assets").is_none());
    let frames: Vec<&String> = v["spritesheet"]["frames"].as_object().unwrap().keys().collect();
    assert_eq!(frames, ["x_a"]);
    assert_eq!(v["spritesheet"]["frames"]["x_a"]["frame"]["w"], 1);
}

#[test]
fn particle_and_planet_systems() {
    let logic = r#"<objectData type="fx">
      <model><dimensions x="1" y="2"/><directions/></model>
      <mask type="disc"/>
      <planetsystem>
        <object id="1" name="sun" parent="" radius="0" arcspeed="0.1" arcoffset="-1.5" blend="1" height="0.00005"/>
        <object name="moon"/>
      </planetsystem>
      <particlesystems>
        <particlesystem size="64" canvas_id="1" offset_y="10" blend="0.6" bg_color="000000">
          <emitter id="0" name="sparks" sprite_id="3" max_num_particles="50" particles_per_frame="5" fuse_time="20">
            <simulation force="2.5" direction="0" gravity="0.1" airfriction="0.05" shape="cone" energy="1"/>
            <particles>
              <particle lifetime="10" is_emitter="false" fade="true"><frame name="spark_1"/><frame name=""/><frame name="spark_2"/></particle>
            </particles>
          </emitter>
          <emitter id="1"/>
        </particlesystem>
        <particlesystem size="32"/>
        <particlesystem canvas_id="2"/>
      </particlesystems>
    </objectData>"#;
    let mut b = Builder::default();
    b.binary(1, "fx_fx_index", r#"<object type="fx" visualization="furniture_animated" logic="furniture_fx"/>"#)
        .binary(2, "fx_fx_logic", logic)
        .bitmap(3, &["fx_fx_64_a_0_0"], 1, 1, [0, 0, 0], [255, 0, 0, 0]);
    let (json, _) = convert_json("fx", &b.swf());
    let v = value(&json);
    same(
        &v["logic"],
        serde_json::json!({
            "model": { "dimensions": { "x": 1, "y": 2, "z": 0.000001 }, "directions": [0] },
            "maskType": "disc",
            "planetSystems": [
                { "id": 1, "name": "sun", "parent": "", "radius": 0, "arcSpeed": 0.1, "arcOffset": -1.5, "blend": 1, "height": 0.00005 },
                { "name": "moon" }
            ],
            "particleSystems": [
                {
                    "size": 64, "canvasId": 1, "offsetY": 10, "blend": 0.6, "bgColor": "000000",
                    "emitters": [
                        {
                            "id": 0, "name": "sparks", "spriteId": 3, "maxNumParticles": 50, "particlesPerFrame": 5,
                            "burstPulse": 1, "fuseTime": 20,
                            "simulation": { "force": 2.5, "direction": 0, "gravity": 0.1, "airFriction": 0.05, "shape": "cone", "energy": 1 },
                            "particles": [ { "isEmitter": false, "lifeTime": 10, "fade": true, "frames": ["spark_1", "spark_2"] } ]
                        },
                        { "id": 1, "burstPulse": 1 }
                    ]
                },
                { "size": 32 }
            ]
        }),
    );
    // The converter's own number text: z as F6, a tiny float as F6 too, the others shortest.
    assert!(json.contains("\"z\": 0.000001"));
    assert!(json.contains("\"height\": 0.000050"));
    assert!(json.contains("\"blend\": 0.6,"));
    // The sheet with no assets XML: one frame, its key as the symbol name (nothing to clean).
    assert!(v["spritesheet"]["frames"].get("fx_fx_64_a_0_0").is_some());
}

#[test]
fn no_index_or_no_images_fails() {
    let mut b = Builder::default();
    b.bitmap(3, &["x_64_a_0_0"], 1, 1, [0, 0, 0], [255, 0, 0, 0]);
    assert!(convert("x", &b.swf()).is_err());
    let mut b = Builder::default();
    b.binary(1, "x_x_index", INDEX).bitmap(3, &["sh_x"], 1, 1, [0, 0, 0], [255, 0, 0, 0]);
    assert!(convert("x", &b.swf()).is_err());
}

#[test]
fn many_images_fill_ten_rows() {
    let mut b = Builder::default();
    b.binary(1, "x_x_index", INDEX);
    for i in 0..23u16 {
        b.bitmap(10 + i, &[&format!("x_x_64_a_{i:02}")], 1 + i % 3, 2, [0, 0, 0], [255, 0, 0, 0]);
    }
    let (json, sheet) = convert_json("x", &b.swf());
    let v = value(&json);
    let frames = v["spritesheet"]["frames"].as_object().unwrap();
    assert_eq!(frames.len(), 23);
    // ceil(23 / 10) = 3 per row: 8 rows, each 2 high + 2 apart.
    assert_eq!(frames["x_x_64_a_03"]["frame"]["y"], 6);
    assert_eq!(frames["x_x_64_a_03"]["frame"]["x"], 2);
    assert_eq!(frames["x_x_64_a_05"]["frame"]["x"], 2 + (1 + 2) + (2 + 2));
    assert_eq!(sheet.height(), 2 + 8 * 4);
    // Widest row: images 1..3 wide in each row of 3 (1+2+3, or a permutation): 2 + 6 + 3 * 2.
    assert_eq!(sheet.width(), 14);
}
