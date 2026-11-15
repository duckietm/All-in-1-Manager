//! The data files the job writes one per step ([`DataExport`], serialized straight from the
//! catalogue) are byte for byte those of the reference exporters ([`output::data_files`]: the
//! built-in's code, through JSON trees and lists of rows), with the same stats, missing furni
//! and log lines, over a catalogue made to hit their corners: both catalogues, more rows than
//! one INSERT holds, disabled and repeated pages, repeated offers, unknown layouts, values cut
//! to their column, odd characters, products resolved by sprite id, by class name or not at all,
//! and every catalog_manager.sql mode.

mod support;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use catalog_rip_core::export::{ItemsBase, RippedCatalog, RippedMode};
use catalog_rip_core::furnidata::Furnidata;
use catalog_rip_core::manager::{ExistingCatalogue, ManagerMode};
use catalog_rip_core::missing;
use catalog_rip_core::output::{self, DataExport, ExportInput};
use catalog_rip_core::packets::{self, CatalogPage, FrontPageItem, IndexNode, Offer, Product};
use catalog_rip_core::productdata::Productdata;
use support::fake::extra_answer;
use support::*;

fn product(kind: &str, class_id: Option<i32>, extra: &str, count: i32) -> Product {
    Product {
        product_type: kind.into(),
        class_id,
        extra_param: extra.into(),
        count,
        limited: false,
        limited_series_size: 0,
        limited_items_left: 0,
    }
}

fn node(id: i32, name: &str, children: Vec<IndexNode>) -> IndexNode {
    IndexNode {
        visible: id % 7 != 0,
        icon: id.rem_euclid(50),
        page_id: id,
        parent_id: -1,
        name: name.into(),
        localization: if id % 5 == 0 {
            String::new()
        } else {
            format!("Page {id} «ü» 'q' \\")
        },
        offer_ids: vec![id * 10, id * 10 + 1],
        children,
    }
}

/// Page `id` of the generated part: three offers of many kinds.
fn generated_page(id: i32) -> CatalogPage {
    let layouts = [
        "default_3x3",
        "frontpage4",
        "some_new_layout",
        "pets",
        "club_buy",
        "",
    ];
    let long = "x".repeat(3000);
    let offers = (0..3)
        .map(|k| {
            let n = id * 3 + k;
            let products = match n % 9 {
                0 => vec![],
                1 => vec![
                    product("s", Some(n % 40), "", 2),
                    product("i", Some(500 + n % 10), "poster 7", 1),
                ],
                2 => vec![product("b", None, &format!("ACH_{}", n % 6), 1)],
                3 => vec![product("e", Some(n % 5), "", 1)],
                4 => vec![product("i", Some(500 + n % 10), "back\\", 1)],
                _ => {
                    let mut p = product("s", Some(n % 40), "", 1 + n % 3);
                    if n % 11 == 0 {
                        p.limited = true;
                        p.limited_series_size = 100;
                        p.limited_items_left = n % 100;
                    }
                    vec![p]
                }
            };
            Offer {
                // Every 50th page repeats the offers of an earlier one.
                offer_id: if id % 50 == 0 { 3000 + k } else { 3000 + n },
                localization_id: if n % 13 == 0 {
                    format!("{long}_{n}")
                } else {
                    format!("furni_{}_name'{}", n % 40, n % 3)
                },
                rent: false,
                price_credits: n % 30,
                price_activity_points: n % 4,
                activity_point_type: n % 2 * 5,
                extra_int: (n % 2 == 0).then_some(0),
                giftable: n % 3 != 0,
                products,
                club_level: n % 3,
                bundle_purchase_allowed: n % 2 == 0,
                is_pet: false,
                preview_image: format!("p{n}.png"),
                item_ids: (n % 4 == 0).then(String::new),
                have_offer: (n % 4 == 0).then_some(true),
            }
        })
        .collect();
    CatalogPage {
        page_id: id,
        mode: "NORMAL".into(),
        layout: layouts[id as usize % layouts.len()].into(),
        images: vec![format!("img_{id}"), String::new(), "a;b".into()],
        texts: if id % 17 == 0 {
            vec![long.clone(), "\"quoted\"\nnew line\r\n\0nul".into()]
        } else {
            vec![format!("Text {id}"), "Details \\".into(), "Teaser".into()]
        },
        offers,
        offer_id: -1,
        accept_season_currency_as_credits: (id % 3 == 0).then_some(id % 2 == 0),
        frontpage_items: if id % 100 == 1 {
            vec![FrontPageItem {
                position: 1,
                name: "Promo".into(),
                promo_image: "catalogue/promo.png".into(),
                item_type: 1,
                page_name: None,
                product_offer_id: Some(5),
                product_code: None,
                expiration_seconds: 0,
            }]
        } else {
            vec![]
        },
        packet_layout: "extended".into(),
    }
}

/// The official test hotel's catalogues, the NORMAL one with 600 generated pages more (some
/// disabled, one twice in the tree), and a page that did not answer.
fn catalogue(generator: &str) -> RippedCatalog {
    let mut modes = Vec::new();
    for (mode, roots, with_parent, ids, failed) in [
        (
            "NORMAL",
            normal_roots(),
            true,
            vec![1, 2, 3, 4, 5, 6],
            vec![7, 1599],
        ),
        ("BUILDERS_CLUB", builders_roots(), false, vec![100, 4], vec![]),
    ] {
        let mut index =
            packets::parse_index(&index_packet(INDEX, mode, &roots, with_parent, false).body)
                .unwrap();
        let mut pages: HashMap<i32, CatalogPage> = ids
            .into_iter()
            .map(|id| {
                let body = page_answer(id, mode).unwrap().body;
                (id, packets::parse_page(&body).unwrap())
            })
            .collect();
        if mode == "NORMAL" {
            let mut children = Vec::new();
            for id in 1000..1600 {
                // Every 97th node is a disabled page (id -1) with one child.
                if id % 97 == 0 {
                    children.push(node(-1, "disabled", vec![node(id, "under_disabled", vec![])]));
                } else {
                    children.push(node(id, &format!("page_{id}"), vec![]));
                }
                if id != 1599 {
                    pages.insert(id, generated_page(id));
                }
            }
            // Page 1001 is in the tree twice.
            children.push(node(1001, "again", vec![]));
            index.root.children.push(node(999, "generated", children));
        }
        modes.push(RippedMode {
            mode: mode.into(),
            index,
            pages,
            failed,
        });
    }
    RippedCatalog {
        hotel: "habbo.nl".into(),
        host: "game-nl.habbo.com".into(),
        hotel_version: "WIN63-202501011200-123".into(),
        client_type: "FLASH".into(),
        ripped_at: "2025-11-15 12:00:00".into(),
        complete: false,
        modes,
        generator: generator.into(),
    }
}

const FURNIDATA: &str = r##"{"roomitemtypes":{"furnitype":[
    {"id":1,"classname":"chair_a","name":"Chair A","partcolors":{"color":["#FFF"]}},
    {"id":2,"classname":"Table_B*3","name":"Table \"B\"","xdim":2},
    {"id":3,"classname":"lamp_c","rare":true},
    {"id":4001,"classname":"rare_dragon","name":"Dragon"},
    {"id":4002,"classname":"table"},
    {"id":5,"classname":"nobody_has_this","name":"Missing 5","offerid":55}
  ]},
  "wallitemtypes":{"furnitype":[
    {"id":501,"classname":"poster","name":"Poster"},
    {"id":502,"classname":"wall_missing","name":"Wall missing"},
    {"id":5001,"classname":"poster"}
  ]}}"##;

const PRODUCTDATA: &str = r#"{"productdata":{"product":[
    {"code":"chair_a","name":"Chair A","description":"Sit"},
    {"code":"table_b*3","name":"Table","description":""},
    {"code":"furni_3_name'0","name":"Offer 3","description":"By localization"},
    {"code":"nobody_has_this","name":"Missing five","description":"x"},
    {"code":"poster","name":"Poster","description":"On the wall"}
  ]}}"#;

fn items_base() -> ItemsBase {
    let mut items = ItemsBase::default();
    // By sprite id (no furnidata) and by name (with it).
    for sprite in (0..40).step_by(2) {
        items.add(10 + sprite as u32, sprite, "s", &format!("sprite_{sprite}"), 0);
    }
    items.add(100, 999, "s", "chair_a", 0);
    items.add(101, 998, "s", "table_b*3", 0);
    items.add(102, 4001, "s", "rare_dragon", 0);
    items.add(103, 501, "i", "poster", 0);
    items.add(104, 0, "b", "ACH_1", 0);
    items.add(105, 3, "e", "fx_3", 3);
    items
}

fn existing() -> ExistingCatalogue {
    ExistingCatalogue {
        pages: BTreeMap::from([
            ("NORMAL".to_string(), BTreeSet::from([1, 5, 40_000])),
            ("BUILDER".to_string(), BTreeSet::from([2])),
        ]),
        offers: BTreeMap::from([("NORMAL".to_string(), BTreeSet::from([7, 90_000]))]),
    }
}

/// The streamed files, as the job makes them (one per call, the furnidata keeping only the
/// entries of missing furni), against the reference.
fn check(
    catalog: &RippedCatalog,
    items: Option<&ItemsBase>,
    with_furnidata: bool,
    with_products: bool,
    manager_mode: ManagerMode,
    existing: Option<&ExistingCatalogue>,
    extra_answers: &BTreeMap<String, Vec<u8>>,
) -> output::DataFiles {
    let case = format!(
        "items {} furnidata {with_furnidata} products {with_products} {manager_mode:?} existing {} extras {}",
        items.is_some(),
        existing.is_some(),
        extra_answers.len()
    );
    let whole = with_furnidata.then(|| Furnidata::parse(FURNIDATA).unwrap());
    let kept = with_furnidata.then(|| {
        Furnidata::parse_keeping(FURNIDATA, |names| {
            Some(missing::missing_furni_keys(catalog, items, names))
        })
        .unwrap()
    });
    let products = with_products.then(|| Productdata::parse(PRODUCTDATA).unwrap());
    let input = ExportInput {
        catalog,
        items,
        furnidata: whole.as_ref(),
        products: products.as_ref(),
        manager_mode,
        existing,
        extra_answers,
    };
    let reference = output::data_files(&input);
    let input = ExportInput {
        furnidata: kept.as_ref(),
        ..input
    };
    let mut export = DataExport::new();
    let mut files = BTreeMap::new();
    let mut log = Vec::new();
    while let Some(made) = export.next_file(&input) {
        assert!(
            files.insert(made.file.name().to_string(), made.bytes).is_none(),
            "{case}: {} twice",
            made.file.name()
        );
        log.extend(made.log);
    }
    let names: Vec<String> = reference.files.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(export.files(), names, "{case}: the files, in order");
    for (name, want) in &reference.files {
        let got = &files[name];
        if got != want {
            let at = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(0);
            let from = at.saturating_sub(200);
            panic!(
                "{case}: {name} differs at byte {at} ({} vs {} bytes)\n--- got:\n{}\n--- want:\n{}",
                got.len(),
                want.len(),
                String::from_utf8_lossy(&got[from..(at + 200).min(got.len())]),
                String::from_utf8_lossy(&want[from..(at + 200).min(want.len())]),
            );
        }
    }
    assert_eq!(export.sql, reference.sql, "{case}: SQL stats");
    assert_eq!(export.manager, reference.manager, "{case}: manager stats");
    assert_eq!(export.missing_furni, reference.missing_furni, "{case}");
    assert_eq!(log, reference.log, "{case}: log lines");
    assert!(!export.needs_items_base() && !export.needs_furnidata_entries());
    reference
}

#[test]
fn streamed_files_are_the_reference_files() {
    let catalog = catalogue("Catalogue ripper plugin 9.9.9");
    let items = items_base();
    let existing = existing();
    let extras: BTreeMap<String, Vec<u8>> =
        [("club_offers".to_string(), extra_answer(3285).unwrap().body)].into();
    let none = BTreeMap::new();
    // The corners are there.
    let full = check(&catalog, Some(&items), true, true, ManagerMode::Replace, None, &extras);
    let text = |name: &str| {
        let (_, bytes) = full.files.iter().find(|(n, _)| n == name).unwrap();
        String::from_utf8_lossy(bytes).into_owned()
    };
    assert!(text("catalog_items.sql").matches("INSERT INTO `catalog_items`").count() >= 3);
    assert!(text("catalog_pages.sql").matches("INSERT INTO `catalog_pages`").count() >= 2);
    assert!(text("catalog.json").contains("\"product_names\": {"));
    assert!(text("missing_furnidata.json").contains("\"offerid\": 55"));
    assert_eq!(full.files.len(), 9, "every file");
    let sql = &full.sql;
    assert!(sql.synthetic_page_ids > 0 && sql.duplicate_offers > 0 && sql.truncated_values > 0);
    assert!(!sql.unknown_layouts.is_empty() && sql.offers_without_products > 0);
    assert!(sql.products_resolved > 0 && sql.products_unresolved > 0 && sql.bc_items > 0);
    let manager = &full.manager;
    assert!(manager.adjusted_values > 0 && manager.renumbered_offers > 0);
    assert!(manager.offers > 0 && manager.offers_unresolved > 0);
    assert!(!full.missing_furni.is_empty() && full.log.len() == 1);

    for items in [None, Some(&items)] {
        for (furnidata, products) in [(false, false), (true, false), (true, true), (false, true)] {
            check(&catalog, items, furnidata, products, ManagerMode::Replace, None, &extras);
        }
        for (mode, existing) in [
            (ManagerMode::ReplaceRipped, Some(&existing)),
            (ManagerMode::ReplaceRipped, None),
            (ManagerMode::Add, Some(&existing)),
            (ManagerMode::Add, None),
        ] {
            check(&catalog, items, true, true, mode, existing, &none);
        }
    }
    // Only the normal catalogue: no _bc files.
    let mut normal = catalogue("x");
    normal.modes.truncate(1);
    check(&normal, Some(&items), true, true, ManagerMode::Replace, None, &none);
    // Only the Builders Club.
    let mut builders = catalogue("x");
    builders.modes.remove(0);
    check(&builders, Some(&items), false, false, ManagerMode::ReplaceRipped, None, &none);
}

#[test]
fn missing_keys_without_the_list() {
    let catalog = catalogue("x");
    let items = items_base();
    let furnidata = Furnidata::parse(FURNIDATA).unwrap();
    let list = missing::missing_furni(&catalog, Some(&items), Some(&furnidata), None);
    let want: std::collections::HashSet<_> = missing::missing_keys(&list).into_iter().collect();
    assert!(!want.is_empty());
    assert_eq!(missing::missing_furni_keys(&catalog, Some(&items), &furnidata), want);
    assert!(missing::missing_furni_keys(&catalog, None, &furnidata).is_empty());
}
