//! Nitro's .nitro bundle: u16 file count, then per file a u16 name length, the name, a u32 data length and the data
//! (all big-endian), each file a gzip stream (our converter) or a zlib stream (the original Nitro tools).
//! (All-in-1 Converter: NitroCompiler/NitroToHabConverter.ReadNitro, NitroBundler.cs)

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Result};

fn take<'a>(data: &'a [u8], at: &mut usize, length: usize) -> Result<&'a [u8]> {
    if *at + length > data.len() {
        bail!("The .nitro bundle is cut short.");
    }
    let slice = &data[*at..*at + length];
    *at += length;
    Ok(slice)
}

/// An entry unpacked; one that is not compressed (some tools store images as they are) is kept as it is.
fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) || data.starts_with(b"RIFF") || data.starts_with(b"{") || data.starts_with(b"[") {
        return Ok(data.to_vec());
    }
    let mut out = Vec::new();
    let result = if data.starts_with(&[0x1F, 0x8B]) {
        flate2::read::GzDecoder::new(data).read_to_end(&mut out)
    } else if data.first() == Some(&0x78) {
        flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)
    } else {
        flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)
    };
    result.map_err(|e| anyhow!("A .nitro entry cannot be unpacked: {e}"))?;
    Ok(out)
}

/// Every file of a .nitro as (name, bytes). A damaged bundle is an error instead of a bundle with files missing.
pub fn read(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut at = 0;
    let count = u16::from_be_bytes(take(data, &mut at, 2)?.try_into()?) as usize;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let length = u16::from_be_bytes(take(data, &mut at, 2)?.try_into()?) as usize;
        let name = String::from_utf8(take(data, &mut at, length)?.to_vec()).map_err(|_| anyhow!("A .nitro file name is not UTF-8."))?;
        let length = i32::from_be_bytes(take(data, &mut at, 4)?.try_into()?);
        if length < 0 {
            bail!("Invalid length for {name} in the .nitro bundle.");
        }
        files.push((name, decompress(take(data, &mut at, length as usize)?)?));
    }
    if files.is_empty() {
        bail!("The .nitro bundle is empty.");
    }
    Ok(files)
}

/// Packs files into a .nitro, each as a gzip stream like the converter writes them.
pub fn write(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = (files.len() as u16).to_be_bytes().to_vec();
    for (name, data) in files {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gzip.write_all(data).expect("writing to memory");
        let compressed = gzip.finish().expect("writing to memory");
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(compressed.len() as i32).to_be_bytes());
        out.extend(compressed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read_back() {
        let files = vec![("chair.json".to_string(), b"{}".to_vec()), ("chair.png".to_string(), vec![1, 2, 3])];
        assert_eq!(read(&write(&files)).unwrap(), files);
        assert!(read(&write(&files)[..10]).is_err(), "cut short");
        assert!(read(&[0, 0]).is_err(), "empty");
    }

    #[test]
    fn uncompressed_entries_are_kept() {
        let png = [0x89, b'P', b'N', b'G', 13, 10, 26, 10, 1];
        let mut bundle = 2u16.to_be_bytes().to_vec();
        for (name, data) in [("a.png", &png[..]), ("a.json", &b"{\"x\":1}"[..])] {
            bundle.extend_from_slice(&(name.len() as u16).to_be_bytes());
            bundle.extend_from_slice(name.as_bytes());
            bundle.extend_from_slice(&(data.len() as i32).to_be_bytes());
            bundle.extend_from_slice(data);
        }
        let files = read(&bundle).unwrap();
        assert_eq!(files[0].1, png);
        assert_eq!(files[1].1, b"{\"x\":1}");
    }

    #[test]
    fn zlib_entries_of_the_original_tools() {
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(b"hello").unwrap();
        let entry = zlib.finish().unwrap();
        let mut bundle = 1u16.to_be_bytes().to_vec();
        bundle.extend_from_slice(&5u16.to_be_bytes());
        bundle.extend_from_slice(b"a.txt");
        bundle.extend_from_slice(&(entry.len() as i32).to_be_bytes());
        bundle.extend(entry);
        assert_eq!(read(&bundle).unwrap(), vec![("a.txt".to_string(), b"hello".to_vec())]);
    }
}
