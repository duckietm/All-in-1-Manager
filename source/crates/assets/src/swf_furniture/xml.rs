//! The XElement calls the mappers make, over roxmltree.

use anyhow::{anyhow, Result};

/// File.ReadAllTextAsync: the byte order mark picks UTF-8 / UTF-16 / UTF-32 (and is dropped), else UTF-8 with
/// invalid bytes replaced.
pub fn decode(data: &[u8]) -> String {
    if let Some(rest) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf32 = |rest: &[u8], le: bool| -> String {
        rest.chunks_exact(4)
            .map(|c| {
                let v = if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
                char::from_u32(v).unwrap_or('\u{FFFD}')
            })
            .collect()
    };
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE, 0, 0]) {
        return utf32(rest, true);
    }
    if let Some(rest) = data.strip_prefix(&[0, 0, 0xFE, 0xFF]) {
        return utf32(rest, false);
    }
    let utf16 = |rest: &[u8], le: bool| -> String {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    String::from_utf8_lossy(data).into_owned()
}

/// XElement.Parse (which accepts a DTD).
pub fn parse(text: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
        .map_err(|e| anyhow!("An XML of the furniture cannot be read: {e}"))
}

/// An element named `name` with no namespace, as XName "name" matches.
pub fn is(node: &roxmltree::Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace().is_none()
}

/// `element.Elements(name)`.
pub fn elements<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.children().filter(move |n| is(n, name))
}

/// `element.Element(name)`.
pub fn element<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|n| is(n, name))
}
