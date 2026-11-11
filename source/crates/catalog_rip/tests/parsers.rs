//! The index and page parsers against packets built like the emulator's composers.

mod support;

use aio_catalog_rip::packets::{page_key, parse_index, parse_page};
use support::*;

#[test]
fn index_with_parent_ids() {
    let packet = index_packet(INDEX, "NORMAL", &normal_roots(), true, true);
    let index = parse_index(&packet.body).unwrap();
    assert!(index.has_parent_ids);
    assert!(index.new_additions_available);
    assert_eq!(index.mode, "NORMAL");
    assert_eq!(index.root.page_id, -1);
    assert_eq!(index.root.children.len(), 4);
    let furni = &index.root.children[1];
    assert_eq!(
        (
            furni.page_id,
            furni.parent_id,
            furni.name.as_str(),
            furni.localization.as_str()
        ),
        (2, -1, "furni", "Furni")
    );
    assert_eq!(furni.offer_ids, vec![300, 301]);
    assert_eq!(furni.children[0].parent_id, 2);
    let hidden = &furni.children[2];
    assert!(!hidden.visible);
    assert_eq!(hidden.page_id, -1);
    assert_eq!(hidden.children[0].page_id, 5);
    let ids: Vec<i32> = index.walk().iter().map(|(n, _, _)| n.page_id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, -1, 5, 6, 7]);
}

#[test]
fn index_classic_layout_derives_parents() {
    let packet = index_packet(INDEX, "NORMAL", &normal_roots(), false, false);
    let index = parse_index(&packet.body).unwrap();
    assert!(!index.has_parent_ids);
    let furni = &index.root.children[1];
    assert_eq!(furni.parent_id, -1);
    assert_eq!(furni.children[0].parent_id, 2);
    assert_eq!(
        furni.children[2].children[0].parent_id, -1,
        "child of a disabled (-1) page"
    );
    let walked: Vec<(i32, i32, usize)> = index
        .walk()
        .iter()
        .map(|(n, p, o)| (n.page_id, *p, *o))
        .collect();
    assert_eq!(walked[2], (3, 2, 1));
    assert_eq!(walked[4], (-1, 2, 3));
}

#[test]
fn index_rejects_garbage() {
    let mut packet = index_packet(INDEX, "NORMAL", &normal_roots(), false, false);
    packet.body.push(0);
    assert!(parse_index(&packet.body).is_err());
    assert!(parse_index(&packet.body[..20]).is_err());
}

fn check_shop_page(extended: bool) {
    let packet = page_answer(3, if extended { "NORMAL" } else { "BUILDERS_CLUB" }).unwrap();
    let page = parse_page(&packet.body).unwrap();
    assert_eq!(
        page.packet_layout,
        if extended { "extended" } else { "classic" }
    );
    assert_eq!(page.page_id, 3);
    assert_eq!(page.layout, "default_3x3");
    assert_eq!(
        page.images,
        vec![
            "catalog_header_roombuilder",
            "cr_infopic3",
            "catalog_special_txtbg1"
        ]
    );
    assert_eq!(page.texts[0], "It's a \"shop\"\nline 2");
    assert_eq!(page.offers.len(), 5);
    assert_eq!(page.offer_id, -1);
    assert_eq!(page.accept_season_currency_as_credits, Some(false));
    assert!(page.frontpage_items.is_empty());

    let limited = &page.offers[0];
    assert_eq!(
        (
            limited.offer_id,
            limited.localization_id.as_str(),
            limited.price_credits,
            limited.giftable
        ),
        (300, "rare_dragon_ltd", 50, false)
    );
    let p = &limited.products[0];
    assert_eq!(
        (
            p.product_type.as_str(),
            p.class_id,
            p.count,
            p.limited,
            p.limited_series_size,
            p.limited_items_left
        ),
        ("s", Some(4001), 1, true, 100, 63)
    );
    assert_eq!(limited.preview_image, "rare_dragon_ltd.png");

    let badge = &page.offers[1];
    assert_eq!(
        (badge.price_activity_points, badge.activity_point_type),
        (25, 5)
    );
    assert_eq!(badge.products[0].product_type, "b");
    assert_eq!(badge.products[0].class_id, None);
    assert_eq!(badge.products[0].extra_param, "ACH_Test1");

    let bundle = &page.offers[2];
    assert_eq!(bundle.products.len(), 2);
    assert_eq!(
        (bundle.club_level, bundle.bundle_purchase_allowed),
        (1, false)
    );
    assert_eq!(bundle.products[0].count, 2);
    assert_eq!(
        (
            bundle.products[1].product_type.as_str(),
            bundle.products[1].extra_param.as_str()
        ),
        ("i", "poster 7")
    );
    if extended {
        assert_eq!(bundle.item_ids.as_deref(), Some("12:2;13:1"));
        assert_eq!(bundle.have_offer, Some(false));
    } else {
        assert_eq!(bundle.item_ids, None);
    }
}

#[test]
fn page_extended_offers() {
    check_shop_page(true);
}

#[test]
fn page_classic_offers() {
    check_shop_page(false);
}

#[test]
fn frontpage_items() {
    let packet = page_answer(1, "NORMAL").unwrap();
    let page = parse_page(&packet.body).unwrap();
    assert_eq!(page.layout, "frontpage4");
    assert_eq!(page.frontpage_items.len(), 3);
    let f = &page.frontpage_items;
    assert_eq!(
        (f[0].item_type, f[0].page_name.as_deref()),
        (0, Some("shop_3x3"))
    );
    assert_eq!(
        (
            f[1].item_type,
            f[1].product_offer_id,
            f[1].expiration_seconds
        ),
        (1, Some(300), 3600)
    );
    assert_eq!(
        (f[2].item_type, f[2].product_code.as_deref()),
        (2, Some("credits_pack_1"))
    );
    assert_eq!(f[0].promo_image, "catalogue/feature_cata_vert_new.png");
}

#[test]
fn page_without_season_flag() {
    // An older server: no acceptSeasonCurrencyAsCredits after the offer id.
    let mut packet = page_answer(6, "NORMAL").unwrap();
    packet.body.pop();
    let page = parse_page(&packet.body).unwrap();
    assert_eq!(page.accept_season_currency_as_credits, None);
    assert_eq!(page.texts.len(), 4);
}

#[test]
fn unreadable_page_still_has_a_key() {
    let packet = page_answer(3, "NORMAL").unwrap();
    let cut = &packet.body[..packet.body.len() - 7];
    assert!(parse_page(cut).is_err());
    assert_eq!(page_key(cut), Some((3, "NORMAL".to_string())));
}

// ------------------------------------------------------------------ retro variants

#[test]
fn index_without_mode_or_flag() {
    let full = index_packet(INDEX, "NORMAL", &normal_roots(), false, true);
    let index = parse_index(&without_index_tail(full.clone(), "NORMAL", true).body).unwrap();
    assert_eq!((index.mode.as_str(), index.tail.as_str()), ("", "flag"));
    assert!(index.new_additions_available);
    assert_eq!(index.walk().len(), 8);
    let index = parse_index(&without_index_tail(full, "NORMAL", false).body).unwrap();
    assert_eq!((index.mode.as_str(), index.tail.as_str()), ("", "nothing"));
    assert!(!index.new_additions_available);
    assert_eq!(index.root.children[1].children[0].page_id, 3);
    // With parent ids and no tail too.
    let parents = index_packet(INDEX, "NORMAL", &normal_roots(), true, false);
    let index = parse_index(&without_index_tail(parents, "NORMAL", false).body).unwrap();
    assert!(index.has_parent_ids);
    assert_eq!(index.walk().len(), 8);
}

#[test]
fn page_without_mode() {
    for (id, offers) in [(3, 5), (1, 0), (6, 0), (100, 1)] {
        // The classic offer layout (as retros send it).
        let packet = without_page_mode(page_answer(id, "BUILDERS_CLUB").unwrap());
        let page = parse_page(&packet.body).unwrap();
        assert_eq!(page.page_id, id);
        assert_eq!(page.mode, "", "{}", page.packet_layout);
        assert!(page.packet_layout.ends_with("without mode"), "{}", page.packet_layout);
        assert_eq!(page.offers.len(), offers);
        assert_eq!(page_key(&packet.body), Some((id, String::new())));
    }
    let packet = without_page_mode(page_answer(3, "BUILDERS_CLUB").unwrap());
    let page = parse_page(&packet.body).unwrap();
    assert_eq!(page.layout, "default_3x3");
    assert_eq!(page.offers[2].products[1].extra_param, "poster 7");
    let packet = without_page_mode(page_answer(1, "BUILDERS_CLUB").unwrap());
    assert_eq!(parse_page(&packet.body).unwrap().frontpage_items.len(), 3);
}

/// A CatalogPage as Arcturus Morningstar (PRODUCTION-201611291003-338511768) writes it, byte by
/// byte from CatalogPageComposer / Default_3x3Layout / CatalogItem.serialize: classic offers,
/// the acceptSeasonCurrencyAsCredits flag and no frontpage items.
#[test]
fn hand_made_arcturus_page() {
    let body: Vec<u8> = [
        &[0, 0, 0, 9][..],                         // page id 9
        &[0, 6], b"NORMAL",                         // mode
        &[0, 11], b"default_3x3",                   // layout
        &[0, 0, 0, 1], &[0, 4], b"head",            // 1 image
        &[0, 0, 0, 1], &[0, 2], b"hi",              // 1 text
        &[0, 0, 0, 1],                              // 1 offer
        &[0, 0, 0, 42], &[0, 5], b"chair",          // offer id, name
        &[0],                                       // rent
        &[0, 0, 0, 3], &[0, 0, 0, 0], &[0, 0, 0, 0], // credits, points, points type
        &[1],                                       // giftable
        &[0, 0, 0, 1], &[0, 1], b"s", &[0, 0, 0x0F, 0xA1], &[0, 0], &[0, 0, 0, 1], &[0], // product
        &[0, 0, 0, 0], &[1], &[0], &[0, 9], b"chair.png", // club, bundle, is pet, preview
        &[0xFF, 0xFF, 0xFF, 0xFF],                  // offer id -1
        &[0],                                       // acceptSeasonCurrencyAsCredits
    ]
    .concat();
    let page = parse_page(&body).unwrap();
    assert_eq!(page.packet_layout, "classic");
    assert_eq!(page.accept_season_currency_as_credits, Some(false));
    let offer = &page.offers[0];
    assert_eq!((offer.offer_id, offer.price_credits, offer.preview_image.as_str()), (42, 3, "chair.png"));
    assert_eq!(offer.products[0].class_id, Some(4001));
}
