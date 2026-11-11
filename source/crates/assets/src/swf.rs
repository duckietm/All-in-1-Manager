//! A Flash .swf read far enough for Habbo's asset libraries: the symbol names (SymbolClass), the embedded XML and
//! other binary data (DefineBinaryData) and the bitmaps (DefineBitsLossless 1/2, DefineBitsJPEG2/3) as RGBA images.
//! Every SWF converter (furniture, clothes, pets, effects) starts from this. (All-in-1 Converter:
//! Tools/NativeSwfExtractor.cs, which writes the same names FFDEC's export does: "<id>_<symbol>")

use std::collections::HashMap;
use std::io::Read;

use anyhow::{anyhow, bail, Result};
use image::RgbaImage;

/// One embedded binary (an XML of the library, usually).
#[derive(Debug, Clone)]
pub struct Binary {
    pub id: u16,
    /// The first symbol name of the id, if it has one.
    pub name: Option<String>,
    pub data: Vec<u8>,
}

impl Binary {
    /// The export name: "<id>_<symbol>", or "<id>".
    pub fn export_name(&self) -> String {
        match &self.name {
            Some(name) => format!("{}_{name}", self.id),
            None => self.id.to_string(),
        }
    }

    pub fn text(&self) -> String {
        let data = self.data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&self.data);
        String::from_utf8_lossy(data).into_owned()
    }
}

/// One bitmap with every symbol name that points at it.
#[derive(Debug, Clone)]
pub struct Bitmap {
    pub id: u16,
    pub names: Vec<String>,
    pub image: RgbaImage,
}

impl Bitmap {
    /// The export name FFDEC gives the image file: "<id>_<symbol>" when exactly one symbol names it, else "<id>".
    pub fn export_name(&self) -> String {
        if self.names.len() == 1 {
            format!("{}_{}", self.id, self.names[0])
        } else {
            self.id.to_string()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Swf {
    /// (character id, symbol name) in file order, like FFDEC's symbols.csv.
    pub symbols: Vec<(u16, String)>,
    pub binaries: Vec<Binary>,
    pub bitmaps: Vec<Bitmap>,
}

impl Swf {
    /// The symbol names per character id.
    pub fn names_by_id(&self) -> HashMap<u16, Vec<String>> {
        let mut names: HashMap<u16, Vec<String>> = HashMap::new();
        for (id, name) in &self.symbols {
            names.entry(*id).or_default().push(name.clone());
        }
        names
    }

    /// The binary whose symbol name ends with `suffix` (as the converters look up "..._index", "..._assets" ...).
    pub fn binary_ending(&self, suffix: &str) -> Option<&Binary> {
        self.binaries.iter().find(|b| b.name.as_deref().is_some_and(|n| n.ends_with(suffix)))
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn need(&self, length: usize) -> Result<()> {
        if self.at + length > self.data.len() {
            bail!("The .swf is cut short.");
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        self.at += 1;
        Ok(self.data[self.at - 1])
    }

    fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.data[self.at], self.data[self.at + 1]]);
        self.at += 2;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.data[self.at..self.at + 4].try_into()?);
        self.at += 4;
        Ok(v)
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8]> {
        self.need(length)?;
        self.at += length;
        Ok(&self.data[self.at - length..self.at])
    }

    fn string(&mut self) -> Result<String> {
        let start = self.at;
        while self.at < self.data.len() && self.data[self.at] != 0 {
            self.at += 1;
        }
        let text = String::from_utf8_lossy(&self.data[start..self.at]).into_owned();
        self.at = (self.at + 1).min(self.data.len());
        Ok(text)
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).map_err(|e| anyhow!("A zlib stream in the .swf is damaged: {e}"))?;
    Ok(out)
}

/// The SWF body after the 8-byte header: as is (FWS), zlib-unpacked (CWS) or LZMA-unpacked (ZWS: u32 packed length,
/// the 5 LZMA properties, then the raw stream; the header's length counts the 8 header bytes too).
fn body(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 9 {
        bail!("Not a .swf file.");
    }
    match &data[..3] {
        b"FWS" => Ok(data[8..].to_vec()),
        b"CWS" => inflate(&data[8..]),
        b"ZWS" => {
            if data.len() < 17 {
                bail!("The LZMA .swf is cut short.");
            }
            let unpacked = u32::from_le_bytes(data[4..8].try_into()?).saturating_sub(8) as u64;
            // As a classic .lzma stream: the properties, the unpacked size, the data.
            let mut stream = data[12..17].to_vec();
            stream.extend_from_slice(&unpacked.to_le_bytes());
            stream.extend_from_slice(&data[17..]);
            let mut out = Vec::with_capacity(unpacked as usize);
            lzma_rs::lzma_decompress(&mut std::io::Cursor::new(stream), &mut out).map_err(|e| anyhow!("The LZMA .swf is damaged: {e:?}"))?;
            Ok(out)
        }
        _ => bail!("Not a .swf file."),
    }
}

fn premultiplied(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    match a {
        0 => [0, 0, 0, 0],
        255 => [r, g, b, 255],
        _ => {
            let un = |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            [un(r), un(g), un(b), a]
        }
    }
}

/// DefineBitsLossless (1) and DefineBitsLossless2 (alpha, premultiplied): colour-mapped (3), RGB15 (4) or ARGB (5).
fn lossless(payload: &[u8], alpha: bool) -> Result<RgbaImage> {
    let mut r = Reader { data: payload, at: 2 };
    let format = r.u8()?;
    let width = r.u16()? as usize;
    let height = r.u16()? as usize;
    if width == 0 || height == 0 {
        bail!("A bitmap in the .swf has no size.");
    }
    let palette = if format == 3 { r.u8()? as usize + 1 } else { 0 };
    // A bitmap whose data ends early (some old effects) keeps what is there; the rest is transparent, as in FFDEC.
    let mut data = Vec::new();
    let _ = flate2::read::ZlibDecoder::new(&payload[r.at..]).read_to_end(&mut data);
    let mut pixels = vec![0u8; width * height * 4];
    match format {
        3 => {
            let entry = if alpha { 4 } else { 3 };
            let palette_bytes = palette * entry;
            let stride = (width + 3) & !3;
            if data.len() < palette_bytes {
                bail!("A colour-mapped bitmap in the .swf has no palette.");
            }
            data.resize(data.len().max(palette_bytes + stride * height), 0);
            for y in 0..height {
                for x in 0..width {
                    let mut index = data[palette_bytes + y * stride + x] as usize;
                    if index >= palette {
                        index = 0;
                    }
                    let e = index * entry;
                    let a = if alpha { data[e + 3] } else { 255 };
                    let px = if alpha { premultiplied(data[e], data[e + 1], data[e + 2], a) } else { [data[e], data[e + 1], data[e + 2], 255] };
                    pixels[(y * width + x) * 4..][..4].copy_from_slice(&px);
                }
            }
        }
        5 => {
            let stride = width * 4;
            if data.is_empty() {
                bail!("An ARGB bitmap in the .swf has no data.");
            }
            data.resize(data.len().max(stride * height), 0);
            for y in 0..height {
                for x in 0..width {
                    let s = y * stride + x * 4;
                    let a = if alpha { data[s] } else { 255 };
                    let px = if alpha { premultiplied(data[s + 1], data[s + 2], data[s + 3], a) } else { [data[s + 1], data[s + 2], data[s + 3], 255] };
                    pixels[(y * width + x) * 4..][..4].copy_from_slice(&px);
                }
            }
        }
        4 if !alpha => {
            let stride = (width * 2 + 3) & !3;
            if data.is_empty() {
                bail!("An RGB15 bitmap in the .swf has no data.");
            }
            data.resize(data.len().max(stride * height), 0);
            let expand = |v: u16| ((v << 3) | (v >> 2)) as u8;
            for y in 0..height {
                for x in 0..width {
                    let s = y * stride + x * 2;
                    let pix = (u16::from(data[s]) << 8) | u16::from(data[s + 1]);
                    let px = [expand((pix >> 10) & 0x1F), expand((pix >> 5) & 0x1F), expand(pix & 0x1F), 255];
                    pixels[(y * width + x) * 4..][..4].copy_from_slice(&px);
                }
            }
        }
        _ => bail!("Bitmap format {format} in the .swf is not supported."),
    }
    RgbaImage::from_raw(width as u32, height as u32, pixels).ok_or_else(|| anyhow!("A bitmap in the .swf has the wrong size."))
}

/// Flash-era JPEG data may start with a spurious EOI/SOI pair: the data from the first real SOI.
fn jpeg_start(data: &[u8]) -> &[u8] {
    (0..data.len().saturating_sub(2)).find(|&i| data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF).map_or(data, |i| &data[i..])
}

/// The image of a DefineBitsJPEG2/3 tag: a JPEG, or (Flash 8 and later) a PNG or GIF.
fn jpeg(data: &[u8]) -> Result<RgbaImage> {
    let (data, format) = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        (data, image::ImageFormat::Png)
    } else if data.starts_with(b"GIF8") {
        (data, image::ImageFormat::Gif)
    } else {
        (jpeg_start(data), image::ImageFormat::Jpeg)
    };
    let image = image::load_from_memory_with_format(data, format).map_err(|e| anyhow!("An image in the .swf cannot be read: {e}"))?;
    Ok(image.to_rgba8())
}

/// DefineBitsJPEG3: the JPEG, then a zlib alpha plane.
fn jpeg3(payload: &[u8]) -> Result<RgbaImage> {
    let mut r = Reader { data: payload, at: 2 };
    let alpha_offset = r.u32()? as usize;
    let jpeg_data = r.bytes(alpha_offset)?;
    let mut image = jpeg(jpeg_data)?;
    // A PNG or GIF brings its own alpha; only a JPEG has the alpha plane after it.
    if !jpeg_data.starts_with(&[0xFF]) && !jpeg_start(jpeg_data).starts_with(&[0xFF, 0xD8]) || r.at >= payload.len() {
        return Ok(image);
    }
    let alpha = inflate(&payload[r.at..])?;
    let (w, h) = image.dimensions();
    if alpha.len() < (w * h) as usize {
        bail!("The alpha of a JPEG in the .swf is cut short.");
    }
    for (i, pixel) in image.pixels_mut().enumerate() {
        pixel.0[3] = alpha[i];
    }
    Ok(image)
}

/// Reads a .swf. `images` false skips the bitmaps (for callers that only need the XML).
pub fn read(data: &[u8], images: bool) -> Result<Swf> {
    let body = body(data)?;
    let mut r = Reader { data: &body, at: 0 };
    // The frame size (a RECT of 5 + 4 * n bits), then the frame rate and count.
    let bits = (*body.first().ok_or_else(|| anyhow!("The .swf is empty."))? >> 3) as usize;
    r.at = (5 + 4 * bits).div_ceil(8) + 4;
    let mut swf = Swf::default();
    let mut binaries = Vec::new();
    let mut bitmaps = Vec::new();
    while r.at + 2 <= body.len() {
        let code_and_length = r.u16()?;
        let code = code_and_length >> 6;
        let mut length = (code_and_length & 0x3F) as usize;
        if length == 0x3F {
            length = r.u32()? as usize;
        }
        let tag = r.bytes(length)?;
        match code {
            0 => break,
            76 => {
                let mut t = Reader { data: tag, at: 0 };
                let count = t.u16()?;
                for _ in 0..count {
                    let id = t.u16()?;
                    swf.symbols.push((id, t.string()?));
                }
            }
            87 => {
                let mut t = Reader { data: tag, at: 0 };
                let id = t.u16()?;
                t.u32()?;
                binaries.push((id, tag[t.at..].to_vec()));
            }
            20 | 21 | 35 | 36 if images => bitmaps.push((code, tag.to_vec())),
            6 | 90 if images => bail!("This .swf uses JPEG tables or DefineBitsJPEG4, which are not supported."),
            _ => {}
        }
    }
    let names = swf.names_by_id();
    swf.binaries = binaries
        .into_iter()
        .map(|(id, data)| Binary { id, name: names.get(&id).and_then(|n| n.first().cloned()), data })
        .collect();
    for (code, payload) in bitmaps {
        let id = u16::from_le_bytes([payload[0], *payload.get(1).unwrap_or(&0)]);
        let image = match code {
            20 => lossless(&payload, false)?,
            36 => lossless(&payload, true)?,
            21 => jpeg(&payload[2..])?,
            _ => jpeg3(&payload)?,
        };
        swf.bitmaps.push(Bitmap { id, names: names.get(&id).cloned().unwrap_or_default(), image });
    }
    Ok(swf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

    /// A CWS with one XML binary, one 2x1 ARGB bitmap (premultiplied, half transparent) and their symbols.
    pub fn sample() -> Vec<u8> {
        let mut body = vec![0x00]; // RECT with 0-bit fields: one byte
        body.extend_from_slice(&[0, 24, 1, 0]); // frame rate, frame count
        let mut binary = 1u16.to_le_bytes().to_vec();
        binary.extend_from_slice(&[0, 0, 0, 0]);
        binary.extend_from_slice(b"<index type=\"chair\"/>");
        body.extend(tag(87, &binary));
        let mut bitmap = 2u16.to_le_bytes().to_vec();
        bitmap.push(5);
        bitmap.extend_from_slice(&2u16.to_le_bytes());
        bitmap.extend_from_slice(&1u16.to_le_bytes());
        bitmap.extend(zlib(&[255, 10, 20, 30, 128, 64, 0, 0]));
        body.extend(tag(36, &bitmap));
        let mut symbols = 2u16.to_le_bytes().to_vec();
        symbols.extend_from_slice(&1u16.to_le_bytes());
        symbols.extend_from_slice(b"chair_chair_index\0");
        symbols.extend_from_slice(&2u16.to_le_bytes());
        symbols.extend_from_slice(b"chair_64_a_0_0\0");
        body.extend(tag(76, &symbols));
        body.extend_from_slice(&[0, 0]);
        let mut out = b"CWS\x0a".to_vec();
        out.extend_from_slice(&((body.len() + 8) as u32).to_le_bytes());
        out.extend(zlib(&body));
        out
    }

    #[test]
    fn symbols_binaries_and_bitmaps() {
        let swf = read(&sample(), true).unwrap();
        assert_eq!(swf.symbols, [(1, "chair_chair_index".to_string()), (2, "chair_64_a_0_0".to_string())]);
        let index = swf.binary_ending("_index").unwrap();
        assert_eq!(index.export_name(), "1_chair_chair_index");
        assert_eq!(index.text(), "<index type=\"chair\"/>");
        let bitmap = &swf.bitmaps[0];
        assert_eq!(bitmap.export_name(), "2_chair_64_a_0_0");
        assert_eq!(bitmap.image.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(bitmap.image.get_pixel(1, 0).0, [128, 0, 0, 128], "un-premultiplied");
        assert!(read(&sample(), false).unwrap().bitmaps.is_empty());
        assert!(read(b"ZWS\x0a00000000", true).is_err());
    }

    /// The converter's repository next to this one (its sample SWFs are not part of this repository), or None.
    pub fn converter_samples(folder: &str) -> Option<Vec<std::path::PathBuf>> {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../duckietm/all-in-1-converter").join(folder);
        let mut files: Vec<_> = std::fs::read_dir(folder).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "swf")).collect();
        files.sort();
        Some(files)
    }

    #[test]
    fn real_pet_swfs_when_present() {
        let Some(files) = converter_samples("Compiled/SWFCompiler/import/pets") else { return };
        for path in &files {
            let swf = read(&std::fs::read(path).unwrap(), true).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(swf.binary_ending("_index").is_some(), "{}", path.display());
            assert!(!swf.bitmaps.is_empty(), "{}", path.display());
        }
        assert!(!files.is_empty());
    }

    /// 433 custom effects, among them LZMA (ZWS) files, PNGs in JPEG tags and bitmaps whose data ends early.
    #[test]
    fn real_effect_swfs_when_present() {
        let Some(files) = converter_samples("Addons/Custom Effects/SWF") else { return };
        for path in &files {
            read(&std::fs::read(path).unwrap(), true).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        assert!(files.len() > 400);
    }
}


