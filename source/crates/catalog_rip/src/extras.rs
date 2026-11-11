//! The catalogue data besides the pages, read the same way (one request, one answer):
//! * Habbo Club offers (the prices of HC/VIP months);
//! * the bundle discount ruleset (how many items make a discounted bundle);
//! * the gift wrapping configuration (wrapping price, boxes, ribbons, gift furni).
//!
//! Header ids and layouts, checked against the sources (November 2026):
//! * Arcturus Morningstar `Incoming.java`: `GetClubDataEvent = 3285` (reads an int window id),
//!   `RequestDiscountEvent = 223`, `RequestGiftConfigurationEvent = 418`; `Outgoing.java`:
//!   `ClubDataComposer = 2405`, `DiscountComposer = 2347`, `GiftConfigurationComposer = 2234`;
//!   field orders from ClubOffer.serialize, DiscountComposer and GiftConfigurationComposer.
//! * nitro-renderer: `GET_CLUB_OFFERS = 3285`, `CLUB_OFFERS = 2405`,
//!   `GET_BUNDLE_DISCOUNT_RULESET = 223`, `BUNDLE_DISCOUNT_RULESET = 2347`,
//!   `GET_GIFT_WRAPPING_CONFIG = 418`, `GIFT_WRAPPER_CONFIG = 2234`; ClubOfferData,
//!   BundleDiscountRuleset and GiftWrappingConfigurationParser read the same fields.
//!
//! Official Habbo takes the ids from G-Earth's packet info (the names below). The parsers accept
//! the trailing window id of the club offers being there or not.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::gearth::PacketInfo;
use crate::wire::{Packet, Reader, Writer};

/// One extra message pair: what to send and what answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extra {
    /// The key in catalog_extras.json.
    pub key: &'static str,
    pub request: &'static str,
    pub answer: &'static str,
    request_names: &'static [&'static str],
    answer_names: &'static [&'static str],
}

pub const EXTRAS: &[Extra] = &[
    Extra {
        key: "club_offers",
        request: "GetClubOffers",
        answer: "ClubOffers",
        request_names: &["GetClubOffers", "GetClubOffersMessageComposer", "GetClubDataEvent", "RequestClubDataEvent"],
        answer_names: &["HabboClubOffers", "ClubOffers", "HabboClubOffersMessageEvent", "ClubDataComposer"],
    },
    Extra {
        key: "bundle_discount",
        request: "GetBundleDiscountRuleset",
        answer: "BundleDiscountRuleset",
        request_names: &["GetBundleDiscountRuleset", "GetBundleDiscountRulesetComposer", "RequestDiscountEvent"],
        answer_names: &["BundleDiscountRuleset", "BundleDiscountRulesetMessageEvent", "DiscountComposer"],
    },
    Extra {
        key: "gift_wrapping",
        request: "GetGiftWrappingConfiguration",
        answer: "GiftWrappingConfiguration",
        request_names: &["GetGiftWrappingConfiguration", "GetGiftWrappingConfigurationComposer", "RequestGiftConfigurationEvent"],
        answer_names: &["GiftWrappingConfiguration", "GiftWrappingConfigurationEvent", "GiftConfigurationComposer"],
    },
];

/// The Arcturus Morningstar / Nitro ids (see the module docs).
pub fn arcturus_extra_headers() -> BTreeMap<String, u16> {
    [
        ("GetClubOffers", 3285),
        ("ClubOffers", 2405),
        ("GetBundleDiscountRuleset", 223),
        ("BundleDiscountRuleset", 2347),
        ("GetGiftWrappingConfiguration", 418),
        ("GiftWrappingConfiguration", 2234),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The (request, answer) ids of an extra: the profile's, else G-Earth's packet info.
pub fn resolve(extra: &Extra, profile: &BTreeMap<String, u16>, packets: &[PacketInfo]) -> Option<(u16, u16)> {
    let from_profile = |name: &str| {
        profile
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    };
    let from_gearth = |names: &[&str], to_server: bool| {
        names.iter().find_map(|wanted| {
            packets
                .iter()
                .filter(|p| p.to_server == to_server)
                .find(|p| p.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(wanted)))
                .and_then(|p| u16::try_from(p.header_id).ok())
        })
    };
    let request = from_profile(extra.request).or_else(|| from_gearth(extra.request_names, true))?;
    let answer = from_profile(extra.answer).or_else(|| from_gearth(extra.answer_names, false))?;
    Some((request, answer))
}

/// The request packet of an extra.
pub fn request(extra: &Extra, header: u16) -> Packet {
    match extra.key {
        // The client sends the window it opens the offers in; the server echoes it.
        "club_offers" => Writer::new(header).i32(1).packet(),
        _ => Writer::new(header).packet(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClubOffer {
    pub offer_id: i32,
    pub product_code: String,
    pub price_credits: i32,
    pub price_activity_points: i32,
    pub activity_point_type: i32,
    pub vip: bool,
    pub months: i32,
    pub extra_days: i32,
    pub giftable: bool,
    pub days_left_after_purchase: i32,
    pub year: i32,
    pub month: i32,
    pub day: i32,
}

pub fn parse_club_offers(body: &[u8]) -> Result<Vec<ClubOffer>> {
    let mut r = Reader::new(body);
    let count = r.count(30, "club offer count")?;
    let mut offers = Vec::with_capacity(count);
    for _ in 0..count {
        let offer_id = r.i32("offer id")?;
        let product_code = r.string("product code")?;
        let _unused = r.bool("unused")?;
        offers.push(ClubOffer {
            offer_id,
            product_code,
            price_credits: r.i32("credits")?,
            price_activity_points: r.i32("points")?,
            activity_point_type: r.i32("points type")?,
            vip: r.bool("vip")?,
            months: r.i32("months")?,
            extra_days: r.i32("extra days")?,
            giftable: r.bool("giftable")?,
            days_left_after_purchase: r.i32("days left")?,
            year: r.i32("year")?,
            month: r.i32("month")?,
            day: r.i32("day")?,
        });
    }
    // Arcturus echoes the window id; other servers may not.
    if r.remaining() == 4 {
        r.i32("window id")?;
    }
    if !r.is_empty() {
        bail!("{} bytes left after the club offers", r.remaining());
    }
    Ok(offers)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundleDiscount {
    pub max_purchase_size: i32,
    pub bundle_size: i32,
    pub bundle_discount_size: i32,
    pub bonus_threshold: i32,
    pub additional_bonus_discount_threshold_quantities: Vec<i32>,
}

pub fn parse_bundle_discount(body: &[u8]) -> Result<BundleDiscount> {
    let mut r = Reader::new(body);
    let mut d = BundleDiscount {
        max_purchase_size: r.i32("max purchase size")?,
        bundle_size: r.i32("bundle size")?,
        bundle_discount_size: r.i32("bundle discount size")?,
        bonus_threshold: r.i32("bonus threshold")?,
        additional_bonus_discount_threshold_quantities: Vec::new(),
    };
    let count = r.count(4, "threshold count")?;
    for _ in 0..count {
        d.additional_bonus_discount_threshold_quantities
            .push(r.i32("threshold")?);
    }
    if !r.is_empty() {
        bail!("{} bytes left after the bundle discount ruleset", r.remaining());
    }
    Ok(d)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GiftWrapping {
    pub enabled: bool,
    pub price: i32,
    pub gift_wrappers: Vec<i32>,
    pub box_types: Vec<i32>,
    pub ribbon_types: Vec<i32>,
    pub gift_furnis: Vec<i32>,
}

pub fn parse_gift_wrapping(body: &[u8]) -> Result<GiftWrapping> {
    let mut r = Reader::new(body);
    let enabled = r.bool("enabled")?;
    let price = r.i32("price")?;
    let mut list = |what: &str| -> Result<Vec<i32>> {
        let n = r.count(4, what)?;
        (0..n).map(|_| r.i32(what)).collect()
    };
    let g = GiftWrapping {
        enabled,
        price,
        gift_wrappers: list("gift wrappers")?,
        box_types: list("box types")?,
        ribbon_types: list("ribbon types")?,
        gift_furnis: list("gift furni")?,
    };
    if !r.is_empty() {
        bail!("{} bytes left after the gift wrapping configuration", r.remaining());
    }
    Ok(g)
}

/// catalog_extras.json from the answers (by [`Extra::key`]): what could be read, and the raw
/// bytes (hex) of what could not.
pub fn extras_json(answers: &BTreeMap<String, Vec<u8>>) -> Value {
    let mut out = serde_json::Map::new();
    for (key, body) in answers {
        let parsed = match key.as_str() {
            "club_offers" => parse_club_offers(body).map(|v| json!(v)),
            "bundle_discount" => parse_bundle_discount(body).map(|v| json!(v)),
            "gift_wrapping" => parse_gift_wrapping(body).map(|v| json!(v)),
            _ => continue,
        };
        let value = match parsed {
            Ok(v) => v,
            Err(e) => json!({
                "error": format!("{e:#}"),
                "raw_hex": body.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            }),
        };
        out.insert(key.clone(), value);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn club_offers_body(window: bool) -> Vec<u8> {
        let mut w = Writer::new(0)
            .i32(1)
            .i32(7)
            .string("HABBO_CLUB_1_MONTH")
            .bool(false)
            .i32(25)
            .i32(0)
            .i32(0)
            .bool(false)
            .i32(1)
            .i32(31)
            .bool(false)
            .i32(31)
            .i32(2026)
            .i32(12)
            .i32(10);
        if window {
            w = w.i32(1);
        }
        w.packet().body
    }

    #[test]
    fn parses_the_arcturus_layouts() {
        for window in [true, false] {
            let offers = parse_club_offers(&club_offers_body(window)).unwrap();
            assert_eq!(offers[0].product_code, "HABBO_CLUB_1_MONTH");
            assert_eq!((offers[0].price_credits, offers[0].months, offers[0].month), (25, 1, 12));
        }
        let discount = Writer::new(0).i32(100).i32(6).i32(1).i32(1).i32(2).i32(40).i32(99).packet();
        let d = parse_bundle_discount(&discount.body).unwrap();
        assert_eq!(d.additional_bonus_discount_threshold_quantities, vec![40, 99]);
        let gift = Writer::new(0)
            .bool(true).i32(2)
            .i32(2).i32(3372).i32(3373)
            .i32(3).i32(0).i32(1).i32(2)
            .i32(1).i32(0)
            .i32(0)
            .packet();
        let g = parse_gift_wrapping(&gift.body).unwrap();
        assert_eq!((g.price, g.gift_wrappers.len(), g.box_types.len()), (2, 2, 3));

        let mut answers = BTreeMap::new();
        answers.insert("bundle_discount".to_string(), discount.body.clone());
        answers.insert("gift_wrapping".to_string(), vec![1, 2, 3]);
        let json = extras_json(&answers);
        assert_eq!(json["bundle_discount"]["bundle_size"], 6);
        assert_eq!(json["gift_wrapping"]["raw_hex"], "010203");
    }

    #[test]
    fn resolves_ids() {
        let extra = &EXTRAS[0];
        assert_eq!(resolve(extra, &arcturus_extra_headers(), &[]), Some((3285, 2405)));
        let info = |id, name: &str, out| PacketInfo {
            header_id: id,
            hash: None,
            name: Some(name.into()),
            structure: None,
            to_server: out,
            source: "Sulek".into(),
        };
        let gearth = vec![info(10, "GetClubOffers", true), info(11, "HabboClubOffers", false)];
        assert_eq!(resolve(extra, &BTreeMap::new(), &gearth), Some((10, 11)));
        assert_eq!(resolve(&EXTRAS[1], &BTreeMap::new(), &gearth), None);
        assert_eq!(request(extra, 5).body, vec![0, 0, 0, 1]);
    }
}
