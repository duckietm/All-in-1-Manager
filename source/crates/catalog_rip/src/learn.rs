//! Learning the four header ids from the user's own client, for a hotel that no profile and no
//! G-Earth packet info covers: the user opens the catalogue and clicks a page, and the packets
//! are recognised by their shape.
//! * GetCatalogIndex (to the server): only a catalogue mode string (`NORMAL` / `BUILDERS_CLUB`);
//! * GetCatalogPage (to the server): page id, offer id, mode string, nothing else;
//! * CatalogIndex (to the client), after a GetCatalogIndex: a whole catalogue tree with a root
//!   (-1) and at least one page;
//! * CatalogPage (to the client): a catalogue page with the id the client just asked for.

use crate::packets::{self, Headers};
use crate::profiles::HeaderMap;
use crate::wire::{Packet, Reader};

fn is_mode(text: &str) -> bool {
    ["NORMAL", "BUILDERS_CLUB"]
        .iter()
        .any(|m| m.eq_ignore_ascii_case(text))
}

/// Watches game packets and keeps what they reveal.
#[derive(Debug, Clone)]
pub struct Learner {
    /// GetCatalogIndex, GetCatalogPage, CatalogIndex, CatalogPage.
    ids: [Option<u16>; 4],
    /// Which of them were learned here (not given).
    learned: [bool; 4],
    index_asked: bool,
    page_asked: Option<i32>,
}

impl Learner {
    /// `known`: the ids the other sources gave (kept as they are).
    pub fn new(known: [Option<u16>; 4]) -> Self {
        Self {
            ids: known,
            learned: [false; 4],
            index_asked: false,
            page_asked: None,
        }
    }

    fn set(&mut self, slot: usize, header: u16) -> Option<&'static str> {
        if self.ids[slot].is_some() || self.ids.contains(&Some(header)) {
            return None;
        }
        self.ids[slot] = Some(header);
        self.learned[slot] = true;
        Some(["GetCatalogIndex", "GetCatalogPage", "CatalogIndex", "CatalogPage"][slot])
    }

    /// Looks at one game packet; returns the name of the id it revealed, if any.
    pub fn observe(&mut self, to_server: bool, packet: &Packet) -> Option<&'static str> {
        let header = packet.header;
        if to_server {
            if self.ids[0] == Some(header) {
                self.index_asked = true;
                return None;
            }
            if self.ids[1] == Some(header) {
                self.page_asked = Reader::new(&packet.body).i32("page id").ok();
                return None;
            }
            let mut r = packet.reader();
            if let Ok(mode) = r.string("mode") {
                if r.is_empty() && is_mode(&mode) {
                    self.index_asked = true;
                    return self.set(0, header);
                }
            }
            let mut r = packet.reader();
            if let (Ok(page), Ok(_offer), Ok(mode)) =
                (r.i32("page id"), r.i32("offer id"), r.string("mode"))
            {
                if r.is_empty() && is_mode(&mode) && page > 0 {
                    self.page_asked = Some(page);
                    return self.set(1, header);
                }
            }
            return None;
        }
        if self.ids[2].is_none() && self.index_asked && packet.body.len() >= 20 {
            if let Ok(index) = packets::parse_index(&packet.body) {
                if index.root.page_id == -1 && !index.root.children.is_empty() {
                    return self.set(2, header);
                }
            }
        }
        if self.ids[3].is_none() && self.ids[2] != Some(header) {
            if let Some(asked) = self.page_asked {
                if let Ok(page) = packets::parse_page(&packet.body) {
                    if page.page_id == asked {
                        return self.set(3, header);
                    }
                }
            }
        }
        None
    }

    /// All four, once known.
    pub fn headers(&self) -> Option<Headers> {
        match self.ids {
            [Some(get_index), Some(get_page), Some(index), Some(page)] => Some(Headers {
                get_index,
                get_page,
                index,
                page,
            }),
            _ => None,
        }
    }

    /// Every id known so far.
    pub fn known(&self) -> HeaderMap {
        let [a, b, c, d] = self.ids;
        HeaderMap {
            get_catalog_index: a,
            get_catalog_page: b,
            catalog_index: c,
            catalog_page: d,
        }
    }

    /// The names still missing.
    pub fn missing(&self) -> Vec<&'static str> {
        ["GetCatalogIndex", "GetCatalogPage", "CatalogIndex", "CatalogPage"]
            .iter()
            .zip(self.ids)
            .filter(|(_, id)| id.is_none())
            .map(|(n, _)| *n)
            .collect()
    }

    pub fn learned_any(&self) -> bool {
        self.learned.iter().any(|l| *l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Writer;

    fn index(header: u16) -> Packet {
        Writer::new(header)
            .bool(true).i32(0).i32(-1).string("root").string("").i32(0).i32(1)
            .bool(true).i32(1).i32(5).string("shop").string("Shop").i32(0).i32(0)
            .bool(false).string("NORMAL")
            .packet()
    }

    fn page(header: u16, id: i32) -> Packet {
        Writer::new(header)
            .i32(id).string("NORMAL").string("default_3x3").i32(0).i32(0).i32(0).i32(-1).bool(false)
            .packet()
    }

    #[test]
    fn learns_from_the_client_opening_the_catalogue() {
        let mut l = Learner::new([None; 4]);
        // Noise first: an incoming tree before any request is not taken.
        assert_eq!(l.observe(false, &index(77)), None);
        assert_eq!(l.observe(true, &Writer::new(5).string("hello").packet()), None);
        assert_eq!(l.observe(true, &Writer::new(10).string("NORMAL").packet()), Some("GetCatalogIndex"));
        assert_eq!(l.observe(false, &Writer::new(11).i32(0).packet()), None);
        assert_eq!(l.observe(false, &index(12)), Some("CatalogIndex"));
        assert_eq!(l.observe(true, &Writer::new(13).i32(5).i32(-1).string("NORMAL").packet()), Some("GetCatalogPage"));
        assert_eq!(l.observe(false, &page(14, 9)), None, "not the page asked for");
        assert_eq!(l.observe(false, &page(14, 5)), Some("CatalogPage"));
        let h = l.headers().unwrap();
        assert_eq!((h.get_index, h.index, h.get_page, h.page), (10, 12, 13, 14));
        assert!(l.learned_any());
    }

    #[test]
    fn keeps_known_ids() {
        let mut l = Learner::new([Some(10), None, Some(12), None]);
        assert_eq!(l.missing(), vec!["GetCatalogPage", "CatalogPage"]);
        assert_eq!(l.observe(true, &Writer::new(99).string("NORMAL").packet()), None);
        assert_eq!(l.observe(true, &Writer::new(13).i32(5).i32(-1).string("NORMAL").packet()), Some("GetCatalogPage"));
        assert_eq!(l.observe(false, &page(12, 5)), None, "a known id is not taken again");
        assert_eq!(l.observe(false, &page(14, 5)), Some("CatalogPage"));
        assert_eq!(l.known().to_text(), "GetCatalogIndex=10,GetCatalogPage=13,CatalogIndex=12,CatalogPage=14");
    }
}
