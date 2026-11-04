//! The sprite sheet (Mapper/Spritesheet/SpriteSheetMapper.GenerateSpriteSheet): the images in rows of
//! ceil(count / 10), 2 px apart, each with its edge pixels extruded 1 px into the gap, and the frame JSON.

use std::collections::HashMap;

use anyhow::{bail, Result};
use image::{Rgba, RgbaImage};

use super::json::{Json, Obj};
use super::names::{clean_asset_name, fold, force_cf_upper};

const FRAME_PADDING: u32 = 2;
const NUM_ROWS: usize = 10;
const MAX_SIZE: u32 = 14000;

fn extrude(sheet: &mut RgbaImage, sprite: &RgbaImage, x0: u32, y0: u32) {
    let (w, h) = sprite.dimensions();
    for x in 0..w {
        sheet.put_pixel(x0 + x, y0 - 1, *sprite.get_pixel(x, 0));
        sheet.put_pixel(x0 + x, y0 + h, *sprite.get_pixel(x, h - 1));
    }
    for y in 0..h {
        sheet.put_pixel(x0 - 1, y0 + y, *sprite.get_pixel(0, y));
        sheet.put_pixel(x0 + w, y0 + y, *sprite.get_pixel(w - 1, y));
    }
    sheet.put_pixel(x0 - 1, y0 - 1, *sprite.get_pixel(0, 0));
    sheet.put_pixel(x0 + w, y0 - 1, *sprite.get_pixel(w - 1, 0));
    sheet.put_pixel(x0 - 1, y0 + h, *sprite.get_pixel(0, h - 1));
    sheet.put_pixel(x0 + w, y0 + h, *sprite.get_pixel(w - 1, h - 1));
}

fn rect(x: u32, y: u32, w: u32, h: u32) -> Json {
    Obj::new().put("x", Json::int(x as i32)).put("y", Json::int(y as i32)).put("w", Json::int(w as i32)).put("h", Json::int(h as i32)).build()
}

fn size(w: u32, h: u32) -> Json {
    Obj::new().put("w", Json::int(w as i32)).put("h", Json::int(h as i32)).build()
}

/// The sheet and its `spritesheet` JSON ({ frames, meta }). `images` are (file name, image) in the order the C#
/// dictionary holds them; `canonical` maps folded short names to full names (AssetNameMapper).
pub fn generate(images: &[(String, &RgbaImage)], name: &str, canonical: &HashMap<String, String>) -> Result<(RgbaImage, Json)> {
    if images.is_empty() {
        bail!("The furniture has no images.");
    }
    let per_row = images.len().div_ceil(NUM_ROWS);
    let rows: Vec<&[(String, &RgbaImage)]> = images.chunks(per_row).collect();
    let mut width = 0u64;
    let mut stacked = 0u64;
    for row in &rows {
        let row_width = u64::from(FRAME_PADDING) + row.iter().map(|(_, i)| u64::from(i.width() + FRAME_PADDING)).sum::<u64>();
        let row_height = row.iter().map(|(_, i)| u64::from(i.height())).max().unwrap_or(0);
        width = width.max(row_width);
        stacked += row_height + u64::from(FRAME_PADDING);
    }
    let height = u64::from(FRAME_PADDING) + stacked;
    if width > u64::from(MAX_SIZE) || height > u64::from(MAX_SIZE) {
        bail!("Sprite sheet dimensions ({width}x{height}) exceed maximum allowed ({MAX_SIZE}x{MAX_SIZE}).");
    }
    let (width, height) = (width as u32, height as u32);
    let mut sheet = RgbaImage::new(width, height);
    // A .NET Dictionary: a repeated key replaces the frame where the first one was.
    let mut frames = super::json::Dict::<String, Json>::new();
    let mut y = FRAME_PADDING;
    for row in &rows {
        let mut x = FRAME_PADDING;
        let mut row_height = 0;
        for (key, image) in row.iter() {
            let short = clean_asset_name(key);
            let full = canonical.get(&fold(&short)).cloned().unwrap_or(short);
            let final_key = force_cf_upper(&full);
            // DrawImage (source over a transparent sheet): the sprite as it is, except that a fully transparent
            // pixel comes out as (0, 0, 0, 0). The extruded edges are copied raw.
            for (px, py, p) in image.enumerate_pixels() {
                let out = if p.0[3] == 0 { Rgba([0, 0, 0, 0]) } else { *p };
                sheet.put_pixel(x + px, y + py, out);
            }
            extrude(&mut sheet, image, x, y);
            let (w, h) = image.dimensions();
            let frame = Obj::new()
                .put("frame", rect(x, y, w, h))
                .put("rotated", Json::bool(false))
                .put("trimmed", Json::bool(false))
                .put("spriteSourceSize", rect(0, 0, w, h))
                .put("sourceSize", size(w, h))
                .put("pivot", Obj::new().put("x", Json::float(0.5)?).put("y", Json::float(0.5)?).build())
                .build();
            frames.insert(final_key, frame);
            x += w + FRAME_PADDING;
            row_height = row_height.max(h);
        }
        y += row_height + FRAME_PADDING;
    }
    let meta = Obj::new()
        .put("image", Json::Str(format!("{name}.png")))
        .put("format", Json::str("RGBA8888"))
        .put("size", size(width, height))
        .put("scale", Json::float(1.0)?)
        .put("converter", Json::str("All-in-1-download-tool"))
        .build();
    let json = Obj::new().put("frames", Json::Obj(frames.items)).put("meta", meta).build();
    Ok((sheet, json))
}
