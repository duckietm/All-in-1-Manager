//! Habbo's catalogue messages: the two requests the ripper sends and the two answers it reads.
//!
//! Structures follow what the server writes (Arcturus `CatalogPagesListComposer`,
//! `CatalogPageComposer`, `CatalogItem.serialize`, `CatalogFeaturedPage.serialize`), which is
//! what the Flash/AIR client reads. Two known variants are detected per packet:
//! * index nodes with or without a parent id after the page id (newer emulator forks and
//!   clients write it, Habbo's classic layout does not);
//! * offers with or without the trailing `item ids` string and `have offer` boolean after the
//!   preview image (an emulator fork extension), and pages with or without the
//!   `acceptSeasonCurrencyAsCredits` boolean.
//!
//! Retro servers (older Arcturus/R63 forks, Nitro) add a few more:
//! * an index without the trailing mode string, or without the new-additions flag as well;
//! * a page without the mode string after the page id (pre-2015 R63 servers).
//!
//! The variant that reads the packet to its exact end is used. A mode that the packet did not
//! carry is left empty; the ripper fills in the mode it asked for.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::gearth::PacketInfo;
use crate::profiles::HeaderMap;
use crate::wire::{Packet, Reader, Writer};

// ---------------------------------------------------------------- requests

/// GetCatalogIndex: `string mode` (NORMAL or BUILDERS_CLUB).
pub fn get_catalog_index(header: u16, mode: &str) -> Packet {
    Writer::new(header).string(mode).packet()
}

/// GetCatalogPage: `int page id, int offer id (-1), string mode`.
pub fn get_catalog_page(header: u16, page_id: i32, mode: &str) -> Packet {
    Writer::new(header)
        .i32(page_id)
        .i32(-1)
        .string(mode)
        .packet()
}

// ---------------------------------------------------------------- header ids

/// The header ids of the four messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Headers {
    pub get_index: u16,
    pub get_page: u16,
    pub index: u16,
    pub page: u16,
}

/// Names the packet info sources use (Sulek strips "MessageComposer"/"MessageEvent"; Harble
/// and older lists use the Request*/PagesList names).
pub const GET_INDEX_NAMES: &[&str] = &[
    "GetCatalogIndex",
    "GetCatalogIndexComposer",
    "GetCatalogIndexMessageComposer",
    "GetCatalogIndexMessageEvent",
    "RequestCatalogIndex",
    "CatalogIndexRequest",
];
pub const GET_PAGE_NAMES: &[&str] = &[
    "GetCatalogPage",
    "GetCatalogPageComposer",
    "GetCatalogPageMessageComposer",
    "GetCatalogPageMessageEvent",
    "RequestCatalogPage",
    "RequestCatalogPageEvent",
    "CatalogPageRequest",
];
pub const INDEX_NAMES: &[&str] = &[
    "CatalogIndex",
    "CatalogIndexMessageEvent",
    "CatalogIndexEvent",
    "CatalogIndexMessageComposer",
    "CatalogPagesList",
    "CatalogPagesListComposer",
    "CatalogIndexMessage",
];
pub const PAGE_NAMES: &[&str] = &[
    "CatalogPage",
    "CatalogPageMessageEvent",
    "CatalogPageEvent",
    "CatalogPageComposer",
    "CatalogPageMessageComposer",
    "CatalogPageMessage",
];

/// The four ids and where each came from ("override", "profile" or "G-Earth").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHeaders {
    pub headers: Headers,
    pub sources: [&'static str; 4],
}

/// Resolves the four header ids: `overrides` (the ripper settings) win, then the hotel
/// profile's ids, then G-Earth's packet info (names or hashes).
pub fn resolve_headers(
    packets: &[PacketInfo],
    profile: &HeaderMap,
    overrides: &HeaderMap,
    profile_name: &str,
) -> Result<ResolvedHeaders> {
    let (resolved, sources) = resolve_partial(packets, profile, overrides);
    if let [Some(get_index), Some(get_page), Some(index), Some(page)] = resolved {
        return Ok(ResolvedHeaders {
            headers: Headers {
                get_index,
                get_page,
                index,
                page,
            },
            sources,
        });
    }
    let labels = [
        "GetCatalogIndex (outgoing)",
        "GetCatalogPage (outgoing)",
        "CatalogIndex (incoming)",
        "CatalogPage (incoming)",
    ];
    let missing: Vec<&str> = labels
        .iter()
        .zip(resolved)
        .filter(|(_, r)| r.is_none())
        .map(|(l, _)| *l)
        .collect();
    let mut similar: Vec<String> = packets
        .iter()
        .filter_map(|p| {
            let name = p.name.as_deref()?;
            name.to_ascii_lowercase().contains("catalog").then(|| {
                format!(
                    "{name}={} ({})",
                    p.header_id,
                    if p.to_server { "out" } else { "in" }
                )
            })
        })
        .collect();
    similar.sort();
    similar.truncate(30);
    bail!(
        "No header id for: {}.
         The hotel profile \"{profile_name}\" does not set them and G-Earth's packet info ({} names for this client{}) has none.
         Fix: set the four ids in the hotel profile (GetCatalogIndex, GetCatalogPage, CatalogIndex, CatalogPage)          or in the ripper's header overrides, e.g. GetCatalogIndex=1195,GetCatalogPage=412,CatalogIndex=1032,CatalogPage=804          (Arcturus Morningstar / Nitro). For an official Habbo hotel, make sure G-Earth loaded its packet info (Extra tab).",
        missing.join(", "),
        packets.len(),
        if similar.is_empty() { String::new() } else { format!("; catalogue-related: {}", similar.join(", ")) }
    )
}

/// The ids each source gives, in the order of [`HeaderMap::as_array`], with where each came
/// from (empty when no source has it).
pub fn resolve_partial(
    packets: &[PacketInfo],
    profile: &HeaderMap,
    overrides: &HeaderMap,
) -> ([Option<u16>; 4], [&'static str; 4]) {
    let find = |names: &[&str], to_server: bool| -> Option<u16> {
        for wanted in names {
            for p in packets.iter().filter(|p| p.to_server == to_server) {
                let matches = p
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
                    || p.hash
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(wanted));
                if matches && (0..=u16::MAX as i32).contains(&p.header_id) {
                    return Some(p.header_id as u16);
                }
            }
        }
        None
    };
    let from_gearth = [
        find(GET_INDEX_NAMES, true),
        find(GET_PAGE_NAMES, true),
        find(INDEX_NAMES, false),
        find(PAGE_NAMES, false),
    ];
    let overrides = overrides.as_array();
    let profile_ids = profile.as_array();
    let mut sources = [""; 4];
    let mut resolved = [None; 4];
    for i in 0..4 {
        (resolved[i], sources[i]) = if let Some(id) = overrides[i] {
            (Some(id), "override")
        } else if let Some(id) = profile_ids[i] {
            (Some(id), "profile")
        } else if let Some(id) = from_gearth[i] {
            (Some(id), "G-Earth")
        } else {
            (None, "")
        };
    }
    (resolved, sources)
}

// ---------------------------------------------------------------- index

/// One node of the catalogue index tree (the root is a node too, page id -1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexNode {
    pub visible: bool,
    pub icon: i32,
    pub page_id: i32,
    /// Sent only by the newer layout; otherwise taken from the tree.
    pub parent_id: i32,
    pub name: String,
    pub localization: String,
    pub offer_ids: Vec<i32>,
    pub children: Vec<IndexNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogIndex {
    pub root: IndexNode,
    pub new_additions_available: bool,
    /// Empty when the packet did not carry it (older retros).
    pub mode: String,
    /// Whether the nodes carried a parent id field.
    pub has_parent_ids: bool,
    /// What followed the tree: "flag+mode", "flag" or "nothing".
    pub tail: String,
}

impl CatalogIndex {
    /// Every node below the root, depth first, with its position among its siblings (1-based).
    pub fn walk(&self) -> Vec<(&IndexNode, i32, usize)> {
        fn rec<'a>(node: &'a IndexNode, parent: i32, out: &mut Vec<(&'a IndexNode, i32, usize)>) {
            for (i, child) in node.children.iter().enumerate() {
                out.push((child, parent, i + 1));
                rec(child, child.page_id, out);
            }
        }
        let mut out = Vec::new();
        rec(&self.root, -1, &mut out);
        out
    }
}

const MAX_DEPTH: usize = 64;

fn read_node(r: &mut Reader, with_parent: bool, parent: i32, depth: usize) -> Result<IndexNode> {
    if depth > MAX_DEPTH {
        bail!("index tree deeper than {MAX_DEPTH}");
    }
    let visible = r.bool("node visible")?;
    let icon = r.i32("node icon")?;
    let page_id = r.i32("node page id")?;
    let parent_id = if with_parent {
        r.i32("node parent id")?
    } else {
        parent
    };
    let name = r.string("node name")?;
    let localization = r.string("node localization")?;
    let offers = r.count(4, "node offer count")?;
    let mut offer_ids = Vec::with_capacity(offers);
    for _ in 0..offers {
        offer_ids.push(r.i32("node offer id")?);
    }
    let children_count = r.count(15, "node child count")?;
    let mut children = Vec::with_capacity(children_count);
    for _ in 0..children_count {
        // A child's parent is this node; the root (-1 id) has no page so its children say -1.
        children.push(read_node(r, with_parent, page_id, depth + 1)?);
    }
    Ok(IndexNode {
        visible,
        icon,
        page_id,
        parent_id,
        name,
        localization,
        offer_ids,
        children,
    })
}

/// What an index packet carries after the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexTail {
    FlagAndMode,
    Flag,
    Nothing,
}

fn parse_index_variant(body: &[u8], with_parent: bool, tail: IndexTail) -> Result<CatalogIndex> {
    let mut r = Reader::new(body);
    let mut root = read_node(&mut r, with_parent, -1, 0)?;
    let new_additions_available = match tail {
        IndexTail::Nothing => false,
        _ => r.bool("new additions flag")?,
    };
    let mode = match tail {
        IndexTail::FlagAndMode => r.string("catalog mode")?,
        _ => String::new(),
    };
    if !r.is_empty() {
        bail!("{} bytes left after the index", r.remaining());
    }
    // Top-level pages are stored with parent -1 (as Arcturus does), whatever the root says.
    for child in &mut root.children {
        child.parent_id = -1;
    }
    Ok(CatalogIndex {
        root,
        new_additions_available,
        mode,
        has_parent_ids: with_parent,
        tail: match tail {
            IndexTail::FlagAndMode => "flag+mode",
            IndexTail::Flag => "flag",
            IndexTail::Nothing => "nothing",
        }
        .into(),
    })
}

/// Parses a CatalogIndex body, detecting whether nodes carry a parent id and what follows the
/// tree (the first variant that reads the packet to its exact end wins).
pub fn parse_index(body: &[u8]) -> Result<CatalogIndex> {
    let mut errors = Vec::new();
    for tail in [IndexTail::FlagAndMode, IndexTail::Flag, IndexTail::Nothing] {
        for with_parent in [false, true] {
            match parse_index_variant(body, with_parent, tail) {
                Ok(index) => return Ok(index),
                Err(e) => errors.push(format!(
                    "{tail:?}{}: {e}",
                    if with_parent { " with parent ids" } else { "" }
                )),
            }
        }
    }
    Err(anyhow!("unreadable catalogue index ({})", errors.join("; ")))
}

// ---------------------------------------------------------------- page

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Product {
    /// s (floor), i (wall), e (effect), b (badge), r (bot), h (club), ... as sent, lower case.
    pub product_type: String,
    /// The furni class id (items_base sprite_id), effect id, ...; none for badges.
    pub class_id: Option<i32>,
    /// Badge code, poster number, wallpaper pattern, bot figure, effect duration, ...
    pub extra_param: String,
    pub count: i32,
    pub limited: bool,
    pub limited_series_size: i32,
    pub limited_items_left: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub offer_id: i32,
    pub localization_id: String,
    pub rent: bool,
    pub price_credits: i32,
    pub price_activity_points: i32,
    pub activity_point_type: i32,
    /// The int the current client sends after the points type (not in the Arcturus layout).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_int: Option<i32>,
    pub giftable: bool,
    pub products: Vec<Product>,
    /// 0 none, 1 HC, 2 VIP.
    pub club_level: i32,
    pub bundle_purchase_allowed: bool,
    pub is_pet: bool,
    pub preview_image: String,
    /// Only in the extended offer layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_ids: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have_offer: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontPageItem {
    pub position: i32,
    pub name: String,
    pub promo_image: String,
    /// 0 catalogue page, 1 product offer, 2 in-app purchase.
    pub item_type: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_offer_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_code: Option<String>,
    /// Seconds until it expires, 0 for none.
    pub expiration_seconds: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogPage {
    pub page_id: i32,
    /// Empty when the packet did not carry it (pre-2015 servers).
    pub mode: String,
    pub layout: String,
    pub images: Vec<String>,
    pub texts: Vec<String>,
    pub offers: Vec<Offer>,
    /// The offer the page was opened for (-1).
    pub offer_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_season_currency_as_credits: Option<bool>,
    pub frontpage_items: Vec<FrontPageItem>,
    /// Which packet layout matched ("classic" or "extended").
    pub packet_layout: String,
}

/// Variations of the offer/product layout seen across clients: the current Habbo client
/// (2026) adds an int after the points type in an offer and a second string after a product's
/// extra parameter, compared with the Arcturus-era layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    with_mode: bool,
    extended: bool,
    season_flag: bool,
    offer_extra_int: bool,
    product_extra_string: bool,
}

fn read_product(r: &mut Reader, layout: Layout) -> Result<Product> {
    let product_type = r.string("product type")?.to_lowercase();
    if product_type == "b" {
        let extra_param = r.string("badge code")?;
        return Ok(Product {
            product_type,
            class_id: None,
            extra_param,
            count: 1,
            limited: false,
            limited_series_size: 0,
            limited_items_left: 0,
        });
    }
    let class_id = r.i32("product class id")?;
    let extra_param = r.string("product extra")?;
    if layout.product_extra_string {
        let _ = r.string("product second string")?;
    }
    let count = r.i32("product count")?;
    let limited = r.bool("product limited")?;
    let (limited_series_size, limited_items_left) = if limited {
        (r.i32("limited series size")?, r.i32("limited items left")?)
    } else {
        (0, 0)
    };
    Ok(Product {
        product_type,
        class_id: Some(class_id),
        extra_param,
        count,
        limited,
        limited_series_size,
        limited_items_left,
    })
}

fn read_offer(r: &mut Reader, layout: Layout) -> Result<Offer> {
    let extended = layout.extended;
    let offer_id = r.i32("offer id")?;
    let localization_id = r.string("offer localization")?;
    let rent = r.bool("offer rent")?;
    let price_credits = r.i32("offer credits")?;
    let price_activity_points = r.i32("offer points")?;
    let activity_point_type = r.i32("offer points type")?;
    let extra_int = if layout.offer_extra_int { Some(r.i32("offer extra int")?) } else { None };
    let giftable = r.bool("offer giftable")?;
    let count = r.count(4, "offer product count")?;
    let mut products = Vec::with_capacity(count);
    for _ in 0..count {
        products.push(read_product(r, layout)?);
    }
    let club_level = r.i32("offer club level")?;
    let bundle_purchase_allowed = r.bool("offer bundle purchase")?;
    let is_pet = r.bool("offer is pet")?;
    let preview_image = r.string("offer preview image")?;
    let (item_ids, have_offer) = if extended {
        (
            Some(r.string("offer item ids")?),
            Some(r.bool("offer have offer")?),
        )
    } else {
        (None, None)
    };
    Ok(Offer {
        offer_id,
        localization_id,
        rent,
        price_credits,
        price_activity_points,
        activity_point_type,
        extra_int,
        giftable,
        products,
        club_level,
        bundle_purchase_allowed,
        is_pet,
        preview_image,
        item_ids,
        have_offer,
    })
}

fn read_frontpage_item(r: &mut Reader) -> Result<FrontPageItem> {
    let position = r.i32("frontpage position")?;
    let name = r.string("frontpage name")?;
    let promo_image = r.string("frontpage image")?;
    let item_type = r.i32("frontpage type")?;
    let (mut page_name, mut product_offer_id, mut product_code) = (None, None, None);
    match item_type {
        0 => page_name = Some(r.string("frontpage page name")?),
        1 => product_offer_id = Some(r.i32("frontpage offer id")?),
        2 => product_code = Some(r.string("frontpage product code")?),
        other => bail!("unknown frontpage item type {other}"),
    }
    let expiration_seconds = r.i32("frontpage expiration")?;
    Ok(FrontPageItem {
        position,
        name,
        promo_image,
        item_type,
        page_name,
        product_offer_id,
        product_code,
        expiration_seconds,
    })
}

fn parse_page_variant(body: &[u8], variant: Layout) -> Result<CatalogPage> {
    let season_flag = variant.season_flag;
    let mut r = Reader::new(body);
    let page_id = r.i32("page id")?;
    let mode = if variant.with_mode {
        r.string("page mode")?
    } else {
        String::new()
    };
    let layout = r.string("page layout")?;
    let image_count = r.count(2, "image count")?;
    let mut images = Vec::with_capacity(image_count);
    for _ in 0..image_count {
        images.push(r.string("page image")?);
    }
    let text_count = r.count(2, "text count")?;
    let mut texts = Vec::with_capacity(text_count);
    for _ in 0..text_count {
        texts.push(r.string("page text")?);
    }
    let offer_count = r.count(30, "offer count")?;
    let mut offers = Vec::with_capacity(offer_count);
    for _ in 0..offer_count {
        offers.push(read_offer(&mut r, variant)?);
    }
    let offer_id = r.i32("page offer id")?;
    let accept_season_currency_as_credits = if season_flag {
        Some(r.bool("accept season currency")?)
    } else {
        None
    };
    let mut frontpage_items = Vec::new();
    if !r.is_empty() {
        let count = r.count(16, "frontpage item count")?;
        for _ in 0..count {
            frontpage_items.push(read_frontpage_item(&mut r)?);
        }
    }
    if !r.is_empty() {
        bail!("{} bytes left after the page", r.remaining());
    }
    let packet_layout = layout_name(variant);
    Ok(CatalogPage {
        page_id,
        mode,
        layout,
        images,
        texts,
        offers,
        offer_id,
        accept_season_currency_as_credits,
        frontpage_items,
        packet_layout,
    })
}

fn layout_name(layout: Layout) -> String {
    let mut name = if layout.extended { "extended" } else { "classic" }.to_string();
    if layout.offer_extra_int {
        name.push_str("+offer int");
    }
    if layout.product_extra_string {
        name.push_str("+product string");
    }
    if !layout.season_flag {
        name.push_str(" without season flag");
    }
    if !layout.with_mode {
        name.push_str(" without mode");
    }
    name
}

/// Parses a CatalogPage body, trying every known offer layout; the first that reads the packet
/// to its exact end wins.
pub fn parse_page(body: &[u8]) -> Result<CatalogPage> {
    let mut errors = Vec::new();
    for with_mode in [true, false] {
        for offer_extra_int in [false, true] {
            for product_extra_string in [false, true] {
                for (extended, season_flag) in
                    [(false, true), (true, true), (false, false), (true, false)]
                {
                    let layout = Layout {
                        with_mode,
                        extended,
                        season_flag,
                        offer_extra_int,
                        product_extra_string,
                    };
                    match parse_page_variant(body, layout) {
                        Ok(page) => return Ok(page),
                        Err(e) => errors.push(format!("{}: {e}", layout_name(layout))),
                    }
                }
            }
        }
    }
    bail!("unreadable catalogue page ({})", errors.join("; "))
}

/// The page id and mode at the start of a CatalogPage body, even when the rest cannot be read.
/// The mode is empty when the string after the id is not a catalogue mode (a server that does
/// not send it).
pub fn page_key(body: &[u8]) -> Option<(i32, String)> {
    let mut r = Reader::new(body);
    let id = r.i32("page id").ok()?;
    let mode = r.string("mode").unwrap_or_default();
    let known = ["NORMAL", "BUILDERS_CLUB"]
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&mode));
    Some((id, if known { mode } else { String::new() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: i32, name: &str, to_server: bool) -> PacketInfo {
        PacketInfo {
            header_id: id,
            hash: None,
            name: Some(name.into()),
            structure: None,
            to_server,
            source: "Sulek".into(),
        }
    }

    #[test]
    fn resolves_sulek_names() {
        let list = vec![
            info(1, "GetCatalogIndex", true),
            info(2, "GetCatalogPage", true),
            info(3, "CatalogIndex", false),
            info(4, "CatalogPage", false),
            info(5, "CatalogPage", true), // wrong direction must not match
        ];
        let none = HeaderMap::default();
        assert_eq!(
            resolve_headers(&list, &none, &none, "test").unwrap().headers,
            Headers {
                get_index: 1,
                get_page: 2,
                index: 3,
                page: 4
            }
        );
    }

    #[test]
    fn resolves_harble_names_and_overrides() {
        let list = vec![
            info(10, "RequestCatalogIndex", true),
            info(11, "RequestCatalogPage", true),
            info(12, "CatalogPagesList", false),
        ];
        let none = HeaderMap::default();
        let err = resolve_headers(&list, &none, &none, "Retro X")
            .unwrap_err()
            .to_string();
        assert!(err.contains("CatalogPage (incoming)"), "{err}");
        assert!(err.contains("\"Retro X\""), "{err}");
        let overrides = HeaderMap::parse("CatalogPage=99, GetCatalogIndex=7").unwrap();
        let profile = HeaderMap::parse("CatalogPage=50,GetCatalogPage=51").unwrap();
        let h = resolve_headers(&list, &profile, &overrides, "p").unwrap();
        assert_eq!(h.sources, ["override", "profile", "G-Earth", "override"]);
        assert_eq!(
            h.headers,
            Headers {
                get_index: 7,
                get_page: 51,
                index: 12,
                page: 99
            }
        );
        assert!(HeaderMap::parse("Nope=1").is_err());
    }

    #[test]
    fn requests() {
        let p = get_catalog_page(55, 12, "NORMAL");
        let mut r = p.reader();
        assert_eq!(
            (
                r.i32("").unwrap(),
                r.i32("").unwrap(),
                r.string("").unwrap()
            ),
            (12, -1, "NORMAL".to_string())
        );
        assert_eq!(
            get_catalog_index(9, "BUILDERS_CLUB")
                .reader()
                .string("")
                .unwrap(),
            "BUILDERS_CLUB"
        );
    }
}
