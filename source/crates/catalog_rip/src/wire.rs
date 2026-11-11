//! The wire format shared by Habbo (EVA wire) and the G-Earth extension protocol: a packet is
//! `int32 length` (big endian, counts the header and body), `uint16 header`, then the body.
//! Integers are big-endian int32, booleans one byte, strings a uint16 byte length followed by
//! the bytes (UTF-8 for Habbo, ISO-8859-1 for G-Earth's own texts).

use anyhow::{bail, Result};

/// One packet: header id plus body (without the length prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub header: u16,
    pub body: Vec<u8>,
}

impl Packet {
    /// The full packet with its length prefix.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() + 6);
        out.extend_from_slice(&((self.body.len() + 2) as i32).to_be_bytes());
        out.extend_from_slice(&self.header.to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// Parses a full packet (length prefix, header, body). The length must match the data.
    pub fn from_bytes(data: &[u8]) -> Result<Packet> {
        if data.len() < 6 {
            bail!("packet too short ({} bytes)", data.len());
        }
        let length = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if length < 2 || length as usize != data.len() - 4 {
            bail!(
                "packet length {length} does not match its {} bytes",
                data.len() - 4
            );
        }
        Ok(Packet {
            header: u16::from_be_bytes([data[4], data[5]]),
            body: data[6..].to_vec(),
        })
    }

    pub fn reader(&self) -> Reader<'_> {
        Reader::new(&self.body)
    }
}

/// Reads a packet body field by field.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn bytes(&mut self, count: usize, what: &str) -> Result<&'a [u8]> {
        if self.remaining() < count {
            bail!(
                "packet truncated at byte {} while reading {what} ({count} bytes needed, {} left)",
                self.pos,
                self.remaining()
            );
        }
        let slice = &self.data[self.pos..self.pos + count];
        self.pos += count;
        Ok(slice)
    }

    pub fn byte(&mut self, what: &str) -> Result<u8> {
        Ok(self.bytes(1, what)?[0])
    }

    pub fn bool(&mut self, what: &str) -> Result<bool> {
        Ok(self.byte(what)? != 0)
    }

    pub fn u16(&mut self, what: &str) -> Result<u16> {
        let b = self.bytes(2, what)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i32(&mut self, what: &str) -> Result<i32> {
        let b = self.bytes(4, what)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A count that must be non-negative and plausible for the bytes left (`min_item_bytes` each).
    pub fn count(&mut self, min_item_bytes: usize, what: &str) -> Result<usize> {
        let value = self.i32(what)?;
        if value < 0 {
            bail!("negative {what} ({value})");
        }
        let value = value as usize;
        if value.saturating_mul(min_item_bytes) > self.remaining() {
            bail!(
                "{what} {value} does not fit in the {} bytes left",
                self.remaining()
            );
        }
        Ok(value)
    }

    /// The raw bytes of a uint16-prefixed string.
    pub fn string_bytes(&mut self, what: &str) -> Result<&'a [u8]> {
        let length = self.u16(what)? as usize;
        self.bytes(length, what)
    }

    /// A Habbo string (UTF-8; invalid sequences are replaced).
    pub fn string(&mut self, what: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(self.string_bytes(what)?).into_owned())
    }

    /// A G-Earth string (ISO-8859-1, every byte one char).
    pub fn string_latin1(&mut self, what: &str) -> Result<String> {
        Ok(self
            .string_bytes(what)?
            .iter()
            .map(|&b| b as char)
            .collect())
    }

    /// The raw bytes of an int32-prefixed "long string".
    pub fn long_bytes(&mut self, what: &str) -> Result<&'a [u8]> {
        let length = self.i32(what)?;
        if length < 0 {
            bail!("negative length for {what}");
        }
        self.bytes(length as usize, what)
    }
}

/// Builds a packet body.
#[derive(Debug, Clone)]
pub struct Writer {
    header: u16,
    body: Vec<u8>,
}

impl Writer {
    pub fn new(header: u16) -> Self {
        Self {
            header,
            body: Vec::new(),
        }
    }

    pub fn i32(mut self, value: i32) -> Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn u16(mut self, value: u16) -> Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn byte(mut self, value: u8) -> Self {
        self.body.push(value);
        self
    }

    pub fn bool(self, value: bool) -> Self {
        self.byte(value as u8)
    }

    pub fn raw(mut self, bytes: &[u8]) -> Self {
        self.body.extend_from_slice(bytes);
        self
    }

    /// uint16 length + bytes. Longer data is cut at 65,535 bytes.
    pub fn string_bytes(mut self, bytes: &[u8]) -> Self {
        let bytes = &bytes[..bytes.len().min(u16::MAX as usize)];
        self.body
            .extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        self.body.extend_from_slice(bytes);
        self
    }

    /// A Habbo string (UTF-8).
    pub fn string(self, value: &str) -> Self {
        self.string_bytes(value.as_bytes())
    }

    /// A G-Earth string (ISO-8859-1; characters past U+00FF become '?').
    pub fn string_latin1(self, value: &str) -> Self {
        let bytes: Vec<u8> = value
            .chars()
            .map(|c| {
                if (c as u32) < 256 {
                    c as u32 as u8
                } else {
                    b'?'
                }
            })
            .collect();
        self.string_bytes(&bytes)
    }

    /// int32 length + bytes.
    pub fn long_bytes(mut self, bytes: &[u8]) -> Self {
        self.body
            .extend_from_slice(&(bytes.len() as i32).to_be_bytes());
        self.body.extend_from_slice(bytes);
        self
    }

    pub fn packet(self) -> Packet {
        Packet {
            header: self.header,
            body: self.body,
        }
    }

    pub fn to_bytes(self) -> Vec<u8> {
        self.packet().to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let packet = Writer::new(1234)
            .i32(-5)
            .bool(true)
            .string("héllo")
            .long_bytes(b"ab\tc")
            .packet();
        let bytes = packet.to_bytes();
        assert_eq!(&bytes[..4], &((bytes.len() - 4) as i32).to_be_bytes());
        let parsed = Packet::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, packet);
        let mut r = parsed.reader();
        assert_eq!(r.i32("a").unwrap(), -5);
        assert!(r.bool("b").unwrap());
        assert_eq!(r.string("c").unwrap(), "héllo");
        assert_eq!(r.long_bytes("d").unwrap(), b"ab\tc");
        assert!(r.is_empty());
        assert!(r.i32("past the end").is_err());
    }

    #[test]
    fn latin1_strings() {
        let packet = Writer::new(1).string_latin1("é€").packet();
        assert_eq!(packet.body, vec![0, 2, 0xE9, b'?']);
        assert_eq!(packet.reader().string_latin1("s").unwrap(), "é?");
    }

    #[test]
    fn bad_lengths() {
        assert!(Packet::from_bytes(&[0, 0, 0, 9, 0, 1]).is_err());
        assert!(Packet::from_bytes(&[0, 0, 0]).is_err());
        let mut r = Reader::new(&[0, 0, 0, 200]);
        assert!(r.count(4, "items").is_err());
    }
}
