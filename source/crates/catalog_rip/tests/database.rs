//! item_ids from an items_base, and catalog_manager.sql read back by the Manager's own import
//! parser (`aio_remote::catalog_transfer`).

mod support;

use std::collections::{BTreeMap, HashMap};

use aio_catalog_rip::export::{arcturus_sql, item_ids_sql, ItemsBase, RippedCatalog, RippedMode};
use aio_catalog_rip::manager::manager_sql;
use aio_catalog_rip::packets::{parse_index, parse_page};
use aio_remote::catalog_journal::Entity;
use aio_remote::catalog_transfer;
use support::*;

fn catalog() -> RippedCatalog {
    let index =
        parse_index(&index_packet(INDEX, "NORMAL", &normal_roots(), true, false).body).unwrap();
    let mut pages = HashMap::new();
    for id in [1, 2, 3, 4, 5, 6] {
        pages.insert(
            id,
            parse_page(&page_answer(id, "NORMAL").unwrap().body).unwrap(),
        );
    }
    let bc_index =
        parse_index(&index_packet(INDEX, "BUILDERS_CLUB", &builders_roots(), false, false).body)
            .unwrap();
    let mut bc_pages = HashMap::new();
    for id in [100, 4] {
        bc_pages.insert(
            id,
            parse_page(&page_answer(id, "BUILDERS_CLUB").unwrap().body).unwrap(),
        );
    }
    RippedCatalog {
        hotel: "habbo.nl".into(),
        host: "game-nl.habbo.com".into(),
        hotel_version: "WIN63-test".into(),
        client_type: "FLASH".into(),
        ripped_at: "2026-01-01 00:00:00".into(),
        complete: true,
        modes: vec![
            RippedMode {
                mode: "NORMAL".into(),
                index,
                pages,
                failed: vec![7],
            },
            RippedMode {
                mode: "BUILDERS_CLUB".into(),
                index: bc_index,
                pages: bc_pages,
                failed: vec![],
            },
        ],
    }
}

fn items() -> ItemsBase {
    let mut items = ItemsBase::default();
    items.add(20, 4001, "s", "rare_dragon", 0);
    items.add(10, 4001, "s", "rare_dragon_copy", 0); // lowest id wins
    items.add(11, 4002, "s", "table", 0);
    items.add(12, 5001, "i", "poster", 0);
    items.add(13, 0, "b", "ACH_Test1", 0);
    items
}

#[test]
fn literal_item_ids_from_items_base() {
    let items = items();
    let catalog = catalog();
    let page = &catalog.modes[0].pages[&3];
    assert_eq!(item_ids_sql(&page.offers[0], Some(&items)).sql, "'10'");
    assert_eq!(item_ids_sql(&page.offers[1], Some(&items)).sql, "'13'");
    assert_eq!(
        item_ids_sql(&page.offers[2], Some(&items)).sql,
        "'11:2;12:1'"
    );
    // 3001 (wallpaper) is unknown: a subquery, the rest literal.
    let ids = item_ids_sql(&page.offers[3], Some(&items));
    assert_eq!((ids.resolved, ids.unresolved), (0, 1));
    let sql = arcturus_sql(&catalog, Some(&items));
    assert!(sql.items.contains("(300, '10', 3,"), "{}", sql.items);
}

#[test]
fn manager_file_reads_back_in_the_manager_import() {
    let items = items();
    let catalog = catalog();
    let file = manager_sql(&catalog, Some(&items));
    let text = &file.text;
    assert!(text.starts_with("-- Catalog Studio restricted SQL export."));
    // 3 offers lack items_base rows: 303 (3001), 304 (4003), 500 (4004).
    assert_eq!(file.stats.offers_unresolved, 3, "{text}");
    assert_eq!(file.stats.offers, 4); // 300, 301, 302 and the BC table 1000
    assert!(text.contains("-- skipped: NORMAL offer 303"), "{text}");

    let target = catalog_transfer::target(&BTreeMap::new(), text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let pages: Vec<&Entity> = target.keys().filter(|e| e.page).collect();
    // NORMAL: 1-7 + the disabled page (renumbered 8); BUILDER: 100 and 4.
    assert_eq!(pages.len(), 10, "{pages:?}");
    assert_eq!(file.stats.pages, 10);
    let normal = |id| target[&Entity { builders: false, page: true, id }].clone();
    let builder = |id| target[&Entity { builders: true, page: true, id }].clone();
    let shop = normal(3);
    assert_eq!(shop["captionSave"], "shop_3x3");
    assert_eq!(shop["parentId"], 2);
    assert_eq!(shop["pageText1"], "It's a \"shop\"\nline 2");
    assert_eq!(shop["pageTextDetails"], "Details \\ backslash");
    assert_eq!(shop["visible"], true);
    let hidden = normal(8);
    assert_eq!((hidden["visible"].clone(), hidden["enabled"].clone()), (false.into(), false.into()));
    assert_eq!(normal(5)["parentId"], 8);
    assert_eq!(normal(5)["pageLayout"], "recolorable_furniture");
    assert_eq!(normal(1)["pageLayout"], "frontpage");
    assert_eq!(builder(4)["pageText1"], "Rent one||Rent two");
    assert_eq!(builder(100)["catalogMode"], "BUILDER");

    let offer = |builders, id| target[&Entity { builders, page: false, id }].clone();
    let dragon = offer(false, 300);
    assert_eq!(dragon["itemIds"], "10");
    assert_eq!(dragon["limitedStack"], 100);
    assert_eq!(dragon["costCredits"], 50);
    assert_eq!(dragon["catalogType"], "NORMAL");
    let bundle = offer(false, 302);
    assert_eq!(bundle["itemIds"], "11:2;12:1");
    assert_eq!(bundle["catalogName"], "bundle_o'brien");
    assert_eq!((bundle["haveOffer"].clone(), bundle["clubOnly"].clone()), (false.into(), true.into()));
    assert_eq!(offer(false, 301)["extradata"], "ACH_Test1");
    let bc = offer(true, 1000);
    assert_eq!((bc["itemIds"].clone(), bc["pageId"].clone()), ("11".into(), 100.into()));
}

#[test]
fn manager_file_without_items_base_keeps_pages_only() {
    let file = manager_sql(&catalog(), None);
    assert_eq!(file.stats.offers, 0);
    assert_eq!(file.stats.offers_unresolved, 7);
    let target = catalog_transfer::target(&BTreeMap::new(), &file.text).unwrap();
    assert_eq!(target.len(), 10);
}

#[test]
fn manager_file_survives_hostile_texts() {
    let mut catalog = catalog();
    let page = catalog.modes[0].pages.get_mut(&3).unwrap();
    page.texts = vec![
        "ends with a backslash \\".into(),
        "x\\'); DELETE FROM users; --".into(),
        "/* not a comment */ SELECT 1".into(),
    ];
    let file = manager_sql(&catalog, Some(&items()));
    assert_eq!(file.stats.adjusted_values, 2);
    let target = catalog_transfer::target(&BTreeMap::new(), &file.text)
        .unwrap_or_else(|e| panic!("{e}\n{}", file.text));
    let shop = &target[&Entity { builders: false, page: true, id: 3 }];
    assert_eq!(shop["pageText1"], "ends with a backslash \\\\");
    assert_eq!(shop["pageTextDetails"], "x\\\\'); DELETE FROM users; --");
    assert_eq!(shop["pageTextTeaser"], "/* not a comment */ SELECT 1");
}

#[test]
fn changes_file_brings_an_imported_catalogue_up_to_date() {
    use aio_catalog_rip::diff::diff;
    use aio_catalog_rip::manager::ManagerExport;
    let items = items();
    let old = catalog();
    let mut new = catalog();
    {
        let page = new.modes[0].pages.get_mut(&3).unwrap();
        page.offers[0].price_credits = 99; // dragon 300
        page.offers.retain(|o| o.offer_id != 302); // bundle gone
        page.texts[0] = "New text".into();
    }
    new.ripped_at = "2026-02-01 00:00:00".into();
    let export = ManagerExport { items: Some(&items), ..ManagerExport::default() };
    let (report, text) = diff(&old, &new, &export, true);
    assert!(report.added.is_empty(), "{report:?}");
    let changed: Vec<(&str, i64, Vec<&str>)> = report
        .changed
        .iter()
        .map(|e| (e.what, e.id, e.changed.keys().copied().collect()))
        .collect();
    assert!(changed.contains(&("offer", 300, vec!["cost_credits"])), "{changed:?}");
    assert!(changed.contains(&("page", 3, vec!["page_text1"])), "{changed:?}");
    assert_eq!(changed.len(), 2, "{changed:?}");
    assert_eq!(report.removed.iter().map(|e| (e.what, e.id)).collect::<Vec<_>>(), vec![("offer", 302)]);
    assert!(report.summary().contains("1 offer removed"), "{}", report.summary());

    // The hotel holds the first rip (imported in full); the changes file applies on top of it.
    let imported = catalog_transfer::target(&BTreeMap::new(), &manager_sql_with_items(&old, &items)).unwrap();
    let after = catalog_transfer::target(&imported, &text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let offer = |id| &after[&Entity { builders: false, page: false, id }];
    assert_eq!(offer(300)["costCredits"], 99);
    assert!(!after.contains_key(&Entity { builders: false, page: false, id: 302 }));
    assert_eq!(after[&Entity { builders: false, page: true, id: 3 }]["pageText1"], "New text");

    // Without deleting: the removed offer stays.
    let (_, keep) = diff(&old, &new, &export, false);
    assert!(!keep.contains("DELETE"), "{keep}");
}

fn manager_sql_with_items(catalog: &RippedCatalog, items: &ItemsBase) -> String {
    manager_sql(catalog, Some(items)).text
}


/// Found ripping habbo.it: the same page id in both catalogues, with other offers in each.
/// Merged into one row, the Builders Club offers were lost; they go to the _bc tables now.
#[test]
fn builders_club_pages_with_normal_ids_keep_their_offers() {
    let mut catalog = catalog();
    let shop_offers = catalog.modes[0].pages[&3].offers.clone();
    // BC page 4 (also a normal page id) sells the shop's offers there.
    catalog.modes[1].pages.get_mut(&4).unwrap().offers = shop_offers.clone();
    let sql = arcturus_sql(&catalog, None);
    assert_eq!(sql.stats.bc_items, 1 + shop_offers.len(), "bc table + the shop's offers");
    assert!(sql.items_bc.contains("(300, CONCAT_WS"), "{}", sql.items_bc);
    assert!(sql.items_bc.contains(", 4, 'rare_dragon_ltd', 1, '')"), "{}", sql.items_bc);
    // The normal catalogue is untouched: page 4 has no offers there.
    assert!(!sql.items.contains(", 4, 'rare_dragon_ltd',"), "{}", sql.items);
    assert!(sql.items.contains(", 3, 'rare_dragon_ltd',"), "{}", sql.items);
}
