//! An avatar effect .swf converted to the files of its Nitro bundle: `<name>.json` (assets, aliases, animations,
//! name, spritesheet) and the sprite sheet `<name>.png`, exactly as the All-in-1 Converter writes them before its
//! WebP step. `crate::hab::write(name, &files)` makes the .hab. (All-in-1 Converter: SWF_Effects_Compiler/
//! SWF_Effects_To_Nitro.cs, Mapper/Assets, Mapper/Animation, Spritesheet/EffectsSpritesheetMapper.cs)
//!
//! The C# works on the folders FFDEC or its NativeSwfExtractor exports; `export` replays that in memory.

mod animation;
mod assets;
mod export;
mod json;
mod sheet;

use anyhow::Result;
use image::ImageEncoder;
use serde_json::{Map, Value};

use crate::swf::Swf;

/// The bundle's files (name, bytes) for the effect library `name` (the .swf's file name without extension): the
/// JSON, then the PNG sheet when there are images (and they fit in 14000 x 14000 px).
pub fn convert(name: &str, swf: &Swf) -> Result<Vec<(String, Vec<u8>)>> {
    let export = export::export(swf)?;

    // GetAssetDataAsync: the first binaryData/*_manifest.* file.
    let asset_data = match export::first_binary(&export, |f| f.contains("_manifest.")) {
        Some(data) => assets::parse(&export::text(data), &export.csv_lines),
        None => assets::AssetData::default(),
    };
    let mut asset_list = asset_data.assets;

    // ParseAnimationFileAsync: the first binaryData/*_animation.bin file.
    let animations = match export::first_binary(&export, |f| f.ends_with("_animation.bin")) {
        Some(data) => animation::parse(&export::text(data)),
        None => Value::Object(Map::new()),
    };

    // A sheet that cannot be made (too large) is left out, as the C# catches the error.
    let sheet = if export.images.is_empty() { None } else { sheet::generate(&export.images, name).ok() };

    // No assets in the manifest: one { x: 0, y: 0 } per sheet frame, named without the "<name>_" prefix.
    if asset_list.is_empty() {
        if let Some(sheet) = &sheet {
            let prefix = format!("{name}_");
            for frame in &sheet.frames {
                let lookup = frame.strip_prefix(&prefix).unwrap_or(frame);
                if !asset_list.iter().any(|(k, _)| k == lookup) {
                    asset_list.push((lookup.to_string(), assets::Asset::default()));
                }
            }
        }
    }

    // The anonymous object { assets, aliases, animations, name, spritesheet }, nulls left out.
    let mut root = Map::new();
    if !asset_list.is_empty() {
        root.insert("assets".into(), assets::to_object(&asset_list));
    }
    root.insert("aliases".into(), assets::to_object(&asset_data.aliases));
    root.insert("animations".into(), animations);
    root.insert("name".into(), Value::String(name.to_string()));
    if let Some(sheet) = &sheet {
        root.insert("spritesheet".into(), sheet.data.clone());
    }

    let mut files = vec![(format!("{name}.json"), json::to_string(&Value::Object(root)).into_bytes())];
    if let Some(sheet) = sheet {
        let mut png = Vec::new();
        let (w, h) = sheet.image.dimensions();
        image::codecs::png::PngEncoder::new(&mut png).write_image(sheet.image.as_raw(), w, h, image::ExtendedColorType::Rgba8)?;
        files.push((format!("{name}.png"), png));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swf::{Binary, Bitmap};
    use image::{Rgba, RgbaImage};

    fn binary(id: u16, name: &str, text: &str) -> Binary {
        Binary { id, name: Some(name.to_string()), data: text.as_bytes().to_vec() }
    }

    fn bitmap(id: u16, names: &[&str], w: u32, h: u32) -> Bitmap {
        Bitmap { id, names: names.iter().map(|n| n.to_string()).collect(), image: RgbaImage::from_pixel(w, h, Rgba([9, 8, 7, 255])) }
    }

    fn swf(symbols: &[(u16, &str)], binaries: Vec<Binary>, bitmaps: Vec<Bitmap>) -> Swf {
        Swf { symbols: symbols.iter().map(|(i, n)| (*i, n.to_string())).collect(), binaries, bitmaps }
    }

    #[test]
    fn a_small_effect_end_to_end() {
        let manifest = r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest><library name="fx" version="0.1"><assets>
<asset name="h_std_fx_a_0_0" mimeType="image/png"><param key="offset" value="-1,2"/></asset>
<asset name="h_std_fx_b_0_0" mimeType="image/png"><param key="offset" value="3,4"/></asset>
</assets><aliases><alias name="h_std_fx_a_4_0" link="h_std_fx_a_0_0" fliph="1"/></aliases></library></manifest>"#;
        let animation = r#"<animation name="fx" desc="fx.1"><sprite id="fx" member="h_std_fx"/></animation>"#;
        let symbols = [(0, "fx"), (1, "fx_manifest"), (2, "fx_animation"), (3, "fx_h_std_fx_a_0_0"), (3, "fx_h_std_fx_b_0_0"), (4, "fx_Z")];
        let swf = swf(
            &symbols,
            vec![binary(1, "fx_manifest", manifest), binary(2, "fx_animation", animation)],
            // Bitmap 3 has two names (exported as "3.png", restored as its first name); 4 one; 5 none (left out).
            vec![bitmap(3, &["fx_h_std_fx_a_0_0", "fx_h_std_fx_b_0_0"], 2, 3), bitmap(4, &["fx_Z"], 1, 1), bitmap(5, &[], 1, 1)],
        );
        let files = convert("fx", &swf).unwrap();
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["fx.json", "fx.png"]);
        let json = String::from_utf8(files[0].1.clone()).unwrap();
        // NTFS order: "FX_H_STD_FX_A_0_0.PNG" before "FX_Z.PNG" ('H' < 'Z'); one image per row (2 images, 10 rows).
        let expected = r#"{
  "assets": {
    "h_std_fx_a_0_0": {
      "x": -1,
      "y": 2
    },
    "h_std_fx_b_0_0": {
      "x": 3,
      "y": 4,
      "source": "h_std_fx_a_0_0"
    }
  },
  "aliases": {
    "h_std_fx_a_4_0": {
      "link": "h_std_fx_a_0_0",
      "flipH": true,
      "flipV": false
    }
  },
  "animations": {
    "fx.1": {
      "name": "fx",
      "desc": "fx.1",
      "sprites": [
        {
          "id": "fx",
          "member": "h_std_fx"
        }
      ]
    }
  },
  "name": "fx",
  "spritesheet": {
    "frames": {
      "fx_h_std_fx_a_0_0": {
        "frame": {
          "x": 2,
          "y": 2,
          "w": 2,
          "h": 3
        },
        "rotated": false,
        "trimmed": false,
        "spriteSourceSize": {
          "x": 0,
          "y": 0,
          "w": 2,
          "h": 3
        },
        "sourceSize": {
          "w": 2,
          "h": 3
        },
        "pivot": {
          "x": 0.5,
          "y": 0.5
        }
      },
      "fx_Z": {
        "frame": {
          "x": 2,
          "y": 7,
          "w": 1,
          "h": 1
        },
        "rotated": false,
        "trimmed": false,
        "spriteSourceSize": {
          "x": 0,
          "y": 0,
          "w": 1,
          "h": 1
        },
        "sourceSize": {
          "w": 1,
          "h": 1
        },
        "pivot": {
          "x": 0.5,
          "y": 0.5
        }
      }
    },
    "meta": {
      "image": "fx.png",
      "format": "RGBA8888",
      "size": {
        "w": 6,
        "h": 10
      },
      "scale": 1,
      "converter": "All-in-1-dowload-tool"
    }
  }
}"#;
        assert_eq!(json, expected);
        let png = image::load_from_memory(&files[1].1).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), (6, 10));
        assert_eq!(png.get_pixel(2, 2).0, [9, 8, 7, 255]);
    }

    #[test]
    fn without_a_manifest_the_assets_come_from_the_frames() {
        let swf = swf(&[(0, "fx"), (3, "fx_a"), (4, "b")], vec![], vec![bitmap(3, &["fx_a"], 1, 1), bitmap(4, &["b"], 1, 1)]);
        let files = convert("fx", &swf).unwrap();
        let json: Value = serde_json::from_slice(&files[0].1).unwrap();
        assert_eq!(json["assets"], serde_json::json!({ "a": { "x": 0, "y": 0 }, "b": { "x": 0, "y": 0 } }));
        assert_eq!(json["animations"], serde_json::json!({}));
        assert_eq!(json["aliases"], serde_json::json!({}));
        // Nothing at all: only the JSON, with no assets and no spritesheet.
        let files = convert("empty", &Swf::default()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(String::from_utf8(files[0].1.clone()).unwrap(), "{\n  \"aliases\": {},\n  \"animations\": {},\n  \"name\": \"empty\"\n}");
    }

    #[test]
    fn image_names_collide_as_on_windows() {
        // Two bitmaps whose names differ only in case: the second move fails and it stays out of the sheet.
        let swf = swf(&[(3, "fx_A"), (4, "fx_a")], vec![], vec![bitmap(3, &["fx_A"], 1, 1), bitmap(4, &["fx_a"], 2, 2)]);
        let files = convert("fx", &swf).unwrap();
        let json: Value = serde_json::from_slice(&files[0].1).unwrap();
        let frames: Vec<&String> = json["spritesheet"]["frames"].as_object().unwrap().keys().collect();
        assert_eq!(frames, ["fx_A"]);
    }

    /// Every custom effect of the converter's repository, when it is next to this one.
    #[test]
    fn real_effect_swfs_when_present() {
        // The converter's repository next to this one (its samples are not part of this repository), as in swf.rs;
        // looked for further up too, for a checkout in a worktree.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(folder) = manifest_dir
            .ancestors()
            .map(|a| a.join("duckietm/all-in-1-converter/Addons/Custom Effects/SWF"))
            .find(|f| f.is_dir())
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(folder) else { return };
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "swf")).collect();
        files.sort();
        let (mut converted, mut with_sheet, mut frames_total, mut assets_total) = (0, 0, 0, 0);
        let mut failures = Vec::new();
        let mut unresolved = Vec::new();
        for path in &files {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let swf = match crate::swf::read(&std::fs::read(path).unwrap(), true) {
                Ok(swf) => swf,
                Err(e) => {
                    failures.push(format!("{name}: read: {e}"));
                    continue;
                }
            };
            let bundle = match convert(&name, &swf) {
                Ok(b) => b,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            assert_eq!(bundle[0].0, format!("{name}.json"));
            let json: Value = serde_json::from_slice(&bundle[0].1).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(json["name"], Value::String(name.clone()));
            let frames = json["spritesheet"]["frames"].as_object();
            if let Some(frames) = frames {
                with_sheet += 1;
                frames_total += frames.len();
                assert_eq!(bundle.len(), 2, "{name}");
                assert_eq!(bundle[1].0, format!("{name}.png"));
                let png = image::load_from_memory(&bundle[1].1).unwrap();
                let size = &json["spritesheet"]["meta"]["size"];
                assert_eq!((u64::from(png.width()), u64::from(png.height())), (size["w"].as_u64().unwrap(), size["h"].as_u64().unwrap()), "{name}");
                for (key, f) in frames {
                    let r = &f["frame"];
                    let (x, y, w, h) = (r["x"].as_u64().unwrap(), r["y"].as_u64().unwrap(), r["w"].as_u64().unwrap(), r["h"].as_u64().unwrap());
                    assert!(x >= 1 && y >= 1 && x + w < u64::from(png.width()) && y + h < u64::from(png.height()), "{name}: frame {key} outside the sheet");
                }
            } else {
                assert_eq!(bundle.len(), 1, "{name}");
            }
            // Each asset should point at a sheet frame "<name>_<asset>" (or its source's); count the ones that do not.
            if let Some(assets) = json["assets"].as_object() {
                assets_total += assets.len();
                // The library prefix may differ in case from the file name (Habvilleodk618 / habvilleodk618_...).
                let keys: std::collections::HashSet<String> = frames.map(|f| f.keys().map(|k| k.to_lowercase()).collect()).unwrap_or_default();
                let has = |a: &str| keys.contains(&format!("{name}_{a}").to_lowercase()) || keys.contains(&a.to_lowercase());
                for (asset, data) in assets {
                    let ok = has(asset) || data["source"].as_str().is_some_and(has);
                    if !ok {
                        // Only when the .swf has no bitmap of that name at all (an asset the library lacks), never
                        // because an image got lost on the way.
                        let suffix = format!("_{}", asset.to_lowercase());
                        let bitmap = swf.bitmaps.iter().any(|b| b.names.iter().any(|n| n.to_lowercase().ends_with(&suffix)));
                        assert!(!bitmap, "{name}: asset {asset} has a bitmap but no frame");
                        unresolved.push(format!("{name}: {asset}"));
                    }
                }
            }
            let hab = crate::hab::write(&name, &bundle);
            assert_eq!(crate::hab::read(&hab).unwrap(), bundle, "{name}");
            converted += 1;
        }
        println!(
            "effects: {converted}/{} converted, {with_sheet} with a sheet, {frames_total} frames, {assets_total} assets, {} assets without a frame",
            files.len(),
            unresolved.len()
        );
        for f in &failures {
            println!("  failed: {f}");
        }
        for u in unresolved.iter().take(20) {
            println!("  no frame: {u}");
        }
        assert!(failures.is_empty(), "{failures:?}");
        assert!(files.len() > 400 && converted == files.len());
    }
}
