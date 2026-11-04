//! The sprite sheet of an effect (Spritesheet/EffectsSpritesheetMapper.cs): the images in up to 10 rows, 2 px apart,
//! each with its edge pixels repeated 1 px around it.

use anyhow::{bail, Result};
use image::{Rgba, RgbaImage};
use serde_json::{json, Map, Value};

const FRAME_PADDING: u32 = 2;
const NUM_ROWS: usize = 10;
const MAX_SIZE: u32 = 14000;

pub struct Sheet {
    pub image: RgbaImage,
    /// The `spritesheet` JSON: { frames, meta }.
    pub data: Value,
    /// The frame names, in order.
    pub frames: Vec<String>,
}

/// ExtrudeEdges: the sprite's raw edge pixels (as stored, not blended) copied 1 px outside it, the corners last.
fn extrude(sheet: &mut RgbaImage, sprite: &RgbaImage, x: u32, y: u32) {
    let (w, h) = sprite.dimensions();
    for i in 0..w {
        sheet.put_pixel(x + i, y - 1, *sprite.get_pixel(i, 0));
        sheet.put_pixel(x + i, y + h, *sprite.get_pixel(i, h - 1));
    }
    for j in 0..h {
        sheet.put_pixel(x - 1, y + j, *sprite.get_pixel(0, j));
        sheet.put_pixel(x + w, y + j, *sprite.get_pixel(w - 1, j));
    }
    sheet.put_pixel(x - 1, y - 1, *sprite.get_pixel(0, 0));
    sheet.put_pixel(x + w, y - 1, *sprite.get_pixel(w - 1, 0));
    sheet.put_pixel(x - 1, y + h, *sprite.get_pixel(0, h - 1));
    sheet.put_pixel(x + w, y + h, *sprite.get_pixel(w - 1, h - 1));
}

/// GenerateSpriteSheet: ceil(n / 10) images per row; a row is as tall as its tallest image; an error past 14000 px
/// (the converter then writes no sheet).
pub fn generate(images: &[(String, &RgbaImage)], name: &str) -> Result<Sheet> {
    if images.is_empty() {
        bail!("No images for the sprite sheet.");
    }
    let per_row = images.len().div_ceil(NUM_ROWS);
    let rows: Vec<&[(String, &RgbaImage)]> = images.chunks(per_row).collect();
    let mut width = 0u64;
    let mut stacked = 0u64;
    for row in &rows {
        let row_width = u64::from(FRAME_PADDING) + row.iter().map(|(_, i)| u64::from(i.width() + FRAME_PADDING)).sum::<u64>();
        width = width.max(row_width);
        stacked += u64::from(row.iter().map(|(_, i)| i.height()).max().unwrap_or(0) + FRAME_PADDING);
    }
    let height = u64::from(FRAME_PADDING) + stacked;
    if width > u64::from(MAX_SIZE) || height > u64::from(MAX_SIZE) {
        bail!("Sprite sheet dimensions ({width}x{height}) exceed maximum allowed ({MAX_SIZE}x{MAX_SIZE}).");
    }
    let (width, height) = (width as u32, height as u32);
    let mut sheet = RgbaImage::new(width, height);
    let mut frames = Map::new();
    let mut names = Vec::new();
    let mut y = FRAME_PADDING;
    for row in &rows {
        let mut x = FRAME_PADDING;
        let mut row_height = 0;
        for (key, image) in row.iter() {
            let (w, h) = image.dimensions();
            // DrawImage (normal blending, source over) onto the transparent sheet: the sprite's pixels, but a fully
            // transparent one comes out as (0, 0, 0, 0) (ImageSharp divides the colour by the alpha it blends to).
            for (i, j, p) in image.enumerate_pixels() {
                let p = if p.0[3] == 0 { Rgba([0, 0, 0, 0]) } else { *p };
                sheet.put_pixel(x + i, y + j, p);
            }
            extrude(&mut sheet, image, x, y);
            let size = json!({ "w": w, "h": h });
            // `frames[name] = ...`: a repeated name keeps its first place (names are unique here: they are files).
            frames.insert(
                key.clone(),
                json!({
                    "frame": { "x": x, "y": y, "w": w, "h": h },
                    "rotated": false,
                    "trimmed": false,
                    "spriteSourceSize": { "x": 0, "y": 0, "w": w, "h": h },
                    "sourceSize": size,
                    "pivot": { "x": 0.5, "y": 0.5 },
                }),
            );
            names.push(key.clone());
            x += w + FRAME_PADDING;
            row_height = row_height.max(h);
        }
        y += row_height + FRAME_PADDING;
    }
    // MetaData: image, format, size, scale (the float 1.0, which System.Text.Json writes as "1"), converter. The
    // image is "<name>.png": the sheet is kept as PNG (ConverterSettings.UseWebp off).
    let data = json!({
        "frames": frames,
        "meta": {
            "image": format!("{name}.png"),
            "format": "RGBA8888",
            "size": { "w": width, "h": height },
            "scale": 1,
            "converter": "All-in-1-dowload-tool",
        },
    });
    Ok(Sheet { image: sheet, data, frames: names })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_padding_and_extrusion() {
        // 11 images: 2 per row, 6 rows.
        let images: Vec<RgbaImage> = (0..11u32).map(|i| RgbaImage::from_pixel(i + 1, 2, Rgba([i as u8, 1, 2, 255]))).collect();
        let named: Vec<(String, &RgbaImage)> = images.iter().enumerate().map(|(i, im)| (format!("f{i}"), im)).collect();
        let sheet = generate(&named, "fx").unwrap();
        // Widest row: images 8 and 9 (9 and 10 px): 2 + 11 + 12 = 25; height 2 + 6 * (2 + 2) = 26.
        assert_eq!(sheet.image.dimensions(), (25, 26));
        let f3 = &sheet.data["frames"]["f3"]["frame"];
        assert_eq!((f3["x"].as_u64(), f3["y"].as_u64()), (Some(2 + 3 + 2), Some(2 + 4)));
        assert_eq!(sheet.data["meta"]["size"], json!({ "w": 25, "h": 26 }));
        assert_eq!(sheet.frames.len(), 11);
        // Extruded edges of f0 (1x2 at 2,2): the pixel above, below, left and right.
        for (x, y) in [(2, 1), (2, 4), (1, 2), (3, 3), (1, 1), (3, 4)] {
            assert_eq!(sheet.image.get_pixel(x, y).0, [0, 1, 2, 255], "{x},{y}");
        }
        assert_eq!(sheet.image.get_pixel(0, 0).0, [0, 0, 0, 0]);
        let big = RgbaImage::new(14000, 1);
        assert!(generate(&[("big".to_string(), &big)], "fx").is_err());
    }
}
