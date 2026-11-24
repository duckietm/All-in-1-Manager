//! The products of a rip that the target hotel's items_base lacks: what keeps offers out of the
//! Manager import. Written as `missing_furni.json` (a list), and — with the source furnidata —
//! `missing_furnidata.json` and `missing_productdata.json`: only those furni, in the Nitro
//! FurnitureData.json / ProductData.json format ([`crate::gamedata`]), to append to the hotel's
//! own. missing_furnidata.json is also the input the Manager's furni import
//! (`aio_remote::furni_import::plan`) reads, so the furni can be added to items_base (with their
//! client files) before the catalogue is imported.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Value};

use crate::export::{ItemsBase, RippedCatalog};
use crate::furnidata::Furnidata;
use crate::gamedata;
use crate::productdata::Productdata;

/// One product the hotel lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingFurni {
    /// s (floor), i (wall), e (effect), b (badge), ...
    pub product_type: String,
    /// The source hotel's class id (none for badges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_id: Option<i32>,
    /// From the source furnidata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classname: Option<String>,
    /// Badge code, poster number, ... for products without a class name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub extra_param: String,
    /// From the source productdata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The offers that sell it.
    pub offers: Vec<i32>,
    /// NORMAL / BUILDERS_CLUB.
    pub catalogs: BTreeSet<String>,
}

/// Every product of the rip that `items` cannot resolve (empty without items_base: then every
/// product would be "missing", which says nothing).
pub fn missing_furni(
    catalog: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
    productdata: Option<&Productdata>,
) -> Vec<MissingFurni> {
    let Some(items) = items else {
        return Vec::new();
    };
    let mut found: BTreeMap<(String, Option<i32>, String), MissingFurni> = BTreeMap::new();
    for mode in &catalog.modes {
        for page in mode.pages.values() {
            for offer in &page.offers {
                for product in &offer.products {
                    if items.lookup_with(product, furnidata).is_some() {
                        continue;
                    }
                    let classname = furnidata.and_then(|f| f.classname(product)).map(str::to_string);
                    let extra = if product.class_id.is_none() {
                        product.extra_param.clone()
                    } else {
                        String::new()
                    };
                    let key = (product.product_type.clone(), product.class_id, extra.clone());
                    let entry = found.entry(key).or_insert_with(|| MissingFurni {
                        product_type: product.product_type.clone(),
                        class_id: product.class_id,
                        name: productdata
                            .and_then(|p| {
                                classname
                                    .as_deref()
                                    .and_then(|c| p.get(c))
                                    .or_else(|| p.get(&offer.localization_id))
                            })
                            .map(|(n, _)| n.to_string())
                            .filter(|n| !n.is_empty()),
                        classname,
                        extra_param: extra,
                        offers: Vec::new(),
                        catalogs: BTreeSet::new(),
                    });
                    if !entry.offers.contains(&offer.offer_id) {
                        entry.offers.push(offer.offer_id);
                    }
                    entry.catalogs.insert(mode.mode.clone());
                }
            }
        }
    }
    let mut list: Vec<MissingFurni> = found.into_values().collect();
    for furni in &mut list {
        furni.offers.sort_unstable();
    }
    list
}

/// A furni whose id the hotel's FurnitureData.json already gives another furni.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Renumbered {
    /// s (floor) or i (wall).
    #[serde(rename = "type")]
    pub product_type: String,
    pub classname: String,
    pub old_id: i64,
    pub new_id: i64,
}

/// The missing floor and wall furni as Nitro gamedata (see [`missing_gamedata`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissingGamedata {
    /// FurnitureData.json of the missing furni (missing_furnidata.json).
    pub furnidata: Value,
    /// ProductData.json of the same furni (missing_productdata.json).
    pub productdata: Value,
    /// Furni the hotel's own FurnitureData.json already lists (though items_base lacks them):
    /// (s / i, class name). Left out of the two files.
    pub in_hotel_furnidata: Vec<(String, String)>,
    /// Ids changed because the hotel's FurnitureData.json uses them for another furni.
    pub renumbered: Vec<Renumbered>,
    /// Whether a hotel FurnitureData.json was given (ids checked).
    pub checked: bool,
}

impl MissingGamedata {
    pub fn floor(&self) -> usize {
        self.furnidata["roomitemtypes"]["furnitype"].as_array().map_or(0, Vec::len)
    }

    pub fn wall(&self) -> usize {
        self.furnidata["wallitemtypes"]["furnitype"].as_array().map_or(0, Vec::len)
    }
}

/// The source furnidata entries of the missing floor and wall furni, in the Nitro gamedata
/// format ([`crate::gamedata`]), with their productdata. With `hotel` (the target hotel's own
/// FurnitureData.json) a furni it already lists by class name is left out, and an id it gives
/// another furni becomes the next free id above its highest (floor and wall ids apart).
pub fn missing_gamedata(
    missing: &[MissingFurni],
    furnidata: &Furnidata,
    productdata: Option<&Productdata>,
    hotel: Option<&Furnidata>,
) -> MissingGamedata {
    let hotel_names: BTreeSet<String> = hotel
        .map(|h| h.floor.values().chain(h.wall.values()).map(|n| n.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let mut out = MissingGamedata {
        checked: hotel.is_some(),
        ..MissingGamedata::default()
    };
    let mut wanted: Vec<(char, &Value)> = Vec::new();
    for m in missing {
        let kind = match m.product_type.as_str() {
            "s" => 's',
            "i" => 'i',
            _ => continue,
        };
        let Some(entry) = m.class_id.and_then(|id| furnidata.entries.get(&(kind, id))) else {
            continue;
        };
        let name = entry.get("classname").and_then(Value::as_str).unwrap_or("").trim();
        if hotel_names.contains(&name.to_lowercase()) {
            out.in_hotel_furnidata.push((m.product_type.clone(), name.to_string()));
            continue;
        }
        wanted.push((kind, entry));
    }
    // Every id taken, per id space: the hotel's and the output's own.
    let mut taken: BTreeMap<char, BTreeSet<i64>> = BTreeMap::new();
    for kind in ['s', 'i'] {
        let ids = taken.entry(kind).or_default();
        if let Some(h) = hotel {
            let map = if kind == 's' { &h.floor } else { &h.wall };
            ids.extend(map.keys().map(|&id| i64::from(id)));
        }
    }
    let hotel_max: BTreeMap<char, i64> = taken
        .iter()
        .map(|(k, ids)| (*k, ids.last().copied().unwrap_or(0)))
        .collect();
    let mut floor = Vec::new();
    let mut wall = Vec::new();
    let mut products = Vec::new();
    let mut nitro_entries = Vec::new();
    for (kind, entry) in &wanted {
        let classname = entry.get("classname").and_then(Value::as_str).unwrap_or("").trim();
        let product = productdata.and_then(|p| p.get(classname));
        let nitro = gamedata::nitro_furni(*kind, entry, product);
        taken.entry(*kind).or_default().insert(nitro["id"].as_i64().unwrap_or(0));
        nitro_entries.push((*kind, nitro, product));
    }
    let mut next: BTreeMap<char, i64> = hotel_max.iter().map(|(k, max)| (*k, max + 1)).collect();
    for (kind, mut nitro, product) in nitro_entries {
        let classname = nitro["classname"].as_str().unwrap_or("").to_string();
        let id = nitro["id"].as_i64().unwrap_or(0);
        let clash = hotel.and_then(|h| {
            let map = if kind == 's' { &h.floor } else { &h.wall };
            i32::try_from(id).ok().and_then(|id| map.get(&id))
        });
        if clash.is_some_and(|other| !other.trim().eq_ignore_ascii_case(&classname)) {
            let ids = taken.entry(kind).or_default();
            let new = next.entry(kind).or_insert(1);
            while ids.contains(new) {
                *new += 1;
            }
            ids.insert(*new);
            nitro["id"] = json!(*new);
            out.renumbered.push(Renumbered {
                product_type: if kind == 's' { "s" } else { "i" }.to_string(),
                classname: classname.clone(),
                old_id: id,
                new_id: *new,
            });
        }
        let text = |field: &str, i: usize| {
            product
                .map(|(n, d)| if i == 0 { n } else { d })
                .filter(|s| !s.is_empty())
                .or_else(|| nitro[field].as_str())
                .unwrap_or("")
                .to_string()
        };
        products.push(gamedata::nitro_product(&classname, &text("name", 0), &text("description", 1)));
        if kind == 's' {
            floor.push(nitro);
        } else {
            wall.push(nitro);
        }
    }
    out.furnidata = gamedata::furniture_data(floor, wall);
    out.productdata = gamedata::product_data(products);
    out
}

/// The README.txt section about missing_furnidata.json, missing_productdata.json and
/// renumbered.json.
pub fn readme_section(out: &MissingGamedata) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    let _ = write!(
        text,
        "\nNITRO GAMEDATA (missing_furnidata.json, missing_productdata.json)\n\
         \x20 missing_furnidata.json: {} floor and {} wall furni in the format of a Nitro FurnitureData.json\n\
         \x20 (same fields, field order and value types; one line). missing_productdata.json: their names\n\
         \x20 and descriptions in the format of ProductData.json.\n\
         \x20 To add them to your hotel:\n\
         \x20   1. Append the entries of roomitemtypes.furnitype and wallitemtypes.furnitype to the same\n\
         \x20      lists of your gamedata/FurnitureData.json.\n\
         \x20   2. Append the entries of productdata.product to the list of your gamedata/ProductData.json.\n\
         \x20   3. Run the Manager's furni import with missing_furnidata.json: it adds the furni to items_base\n\
         \x20      (sprite_id = the furnidata id) from their bundles; then import the catalogue.\n",
        out.floor(),
        out.wall()
    );
    if !out.checked {
        text.push_str(
            "  The ids were not checked against your hotel's FurnitureData.json (none was given in the\n  ripper's settings): an id your hotel already uses for another furni would clash.\n",
        );
        return text;
    }
    let _ = writeln!(
        text,
        "  Checked against your hotel's FurnitureData.json: furni it already lists are left out, and\n  ids it uses for other furni were renumbered."
    );
    if !out.in_hotel_furnidata.is_empty() {
        let _ = writeln!(
            text,
            "  In your furnidata but not in items_base ({}; left out of both files: run the furni import with\n  your own FurnitureData.json to add them to items_base):",
            out.in_hotel_furnidata.len()
        );
        for (kind, name) in &out.in_hotel_furnidata {
            let _ = writeln!(text, "    {kind} {name}");
        }
    }
    if out.renumbered.is_empty() {
        text.push_str("  No id clashes with your hotel's furnidata.\n");
    } else {
        let _ = writeln!(
            text,
            "  Renumbered ({}, also in renumbered.json): the id was taken in your furnidata.",
            out.renumbered.len()
        );
        for r in &out.renumbered {
            let _ = writeln!(text, "    {} {}: {} -> {}", r.product_type, r.classname, r.old_id, r.new_id);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::RippedMode;
    use crate::packets::{CatalogIndex, CatalogPage, IndexNode, Offer, Product};
    use std::collections::HashMap;

    fn product(kind: &str, class_id: Option<i32>, extra: &str) -> Product {
        Product {
            product_type: kind.into(),
            class_id,
            extra_param: extra.into(),
            count: 1,
            limited: false,
            limited_series_size: 0,
            limited_items_left: 0,
        }
    }

    fn catalog(products: Vec<(i32, Product)>) -> RippedCatalog {
        let offers = products
            .into_iter()
            .map(|(id, p)| Offer {
                offer_id: id,
                localization_id: format!("offer_{id}"),
                rent: false,
                price_credits: 1,
                price_activity_points: 0,
                activity_point_type: 0,
                extra_int: None,
                giftable: true,
                products: vec![p],
                club_level: 0,
                bundle_purchase_allowed: true,
                is_pet: false,
                preview_image: String::new(),
                item_ids: None,
                have_offer: None,
            })
            .collect();
        let page = CatalogPage {
            page_id: 1,
            mode: "NORMAL".into(),
            layout: "default_3x3".into(),
            images: vec![],
            texts: vec![],
            offers,
            offer_id: -1,
            accept_season_currency_as_credits: None,
            frontpage_items: vec![],
            packet_layout: "classic".into(),
        };
        let node = |id, children| IndexNode {
            visible: true,
            icon: 0,
            page_id: id,
            parent_id: -1,
            name: String::new(),
            localization: String::new(),
            offer_ids: vec![],
            children,
        };
        RippedCatalog {
            hotel: "h".into(),
            host: "h".into(),
            hotel_version: String::new(),
            client_type: String::new(),
            ripped_at: String::new(),
            complete: true,
            modes: vec![RippedMode {
                mode: "NORMAL".into(),
                index: CatalogIndex {
                    root: node(-1, vec![node(1, vec![])]),
                    new_additions_available: false,
                    mode: "NORMAL".into(),
                    has_parent_ids: false,
                    tail: String::new(),
                },
                pages: HashMap::from([(1, page)]),
                failed: vec![],
            }],
        }
    }

    #[test]
    fn lists_what_items_base_lacks() {
        let catalog = catalog(vec![
            (10, product("s", Some(100), "")),
            (11, product("s", Some(200), "")),
            (12, product("s", Some(200), "")),
            (13, product("b", None, "ACH_X")),
            (14, product("i", Some(300), "")),
        ]);
        let mut items = ItemsBase::default();
        items.add(1, 999, "s", "chair", 0);
        let furni = Furnidata::parse(
            r#"{"roomitemtypes":{"furnitype":[{"id":100,"classname":"chair"},{"id":200,"classname":"table","name":"Table"}]},
                "wallitemtypes":{"furnitype":[{"id":300,"classname":"poster"}]}}"#,
        )
        .unwrap();
        let products = Productdata::parse(r#"{"productdata":{"product":[{"code":"table","name":"Nice table"}]}}"#).unwrap();
        let list = missing_furni(&catalog, Some(&items), Some(&furni), Some(&products));
        assert!(missing_furni(&catalog, None, None, None).is_empty());
        let names: Vec<(String, Option<String>, Vec<i32>)> = list
            .iter()
            .map(|m| (m.product_type.clone(), m.classname.clone(), m.offers.clone()))
            .collect();
        // chair is in items_base by name; table twice; the badge; the poster.
        assert_eq!(
            names,
            vec![
                ("b".to_string(), None, vec![13]),
                ("i".to_string(), Some("poster".to_string()), vec![14]),
                ("s".to_string(), Some("table".to_string()), vec![11, 12]),
            ]
        );
        assert_eq!(list[2].name.as_deref(), Some("Nice table"));
        let out = missing_gamedata(&list, &furni, Some(&products), None);
        let data = &out.furnidata;
        assert_eq!(data["roomitemtypes"]["furnitype"].as_array().unwrap().len(), 1);
        assert_eq!(data["roomitemtypes"]["furnitype"][0]["name"], "Table");
        assert_eq!(data["wallitemtypes"]["furnitype"][0]["classname"], "poster");
        assert!(!out.checked && out.renumbered.is_empty());
        // Productdata: the source productdata's name wins, else the furnidata's, else "".
        assert_eq!(
            gamedata::to_text(&out.productdata),
            r#"{"productdata":{"product":[{"code":"poster","name":"","description":""},{"code":"table","name":"Nice table","description":""}]}}"#
        );
    }

    fn missing(kind: &str, id: i32, classname: &str) -> MissingFurni {
        MissingFurni {
            product_type: kind.into(),
            class_id: Some(id),
            classname: Some(classname.into()),
            extra_param: String::new(),
            name: None,
            offers: vec![1],
            catalogs: BTreeSet::new(),
        }
    }

    /// A small hotel FurnitureData.json (real entries) and a source with clashing ids.
    #[test]
    fn checked_against_the_hotels_furnidata() {
        let hotel = Furnidata::parse(include_str!("../tests/fixtures/nitro_furnituredata.json")).unwrap();
        let floor_max = *hotel.floor.keys().max().unwrap();
        let wall_max = *hotel.wall.keys().max().unwrap();
        let (taken_floor, taken_name) = hotel.floor.iter().next().map(|(i, n)| (*i, n.clone())).unwrap();
        let taken_wall = *hotel.wall.keys().next().unwrap();
        let source = Furnidata::parse(&format!(
            r#"{{"roomitemtypes":{{"furnitype":[
                 {{"id":{taken_floor},"classname":"new_sofa","name":"Sofa"}},
                 {{"id":{},"classname":"own_id_above_max"}},
                 {{"id":5,"classname":"{}"}},
                 {{"id":424242,"classname":"keeps_its_id"}}]}},
               "wallitemtypes":{{"furnitype":[{{"id":{taken_wall},"classname":"new_poster"}}]}}}}"#,
            floor_max + 1,
            taken_name.to_uppercase()
        ))
        .unwrap();
        let list = vec![
            missing("s", taken_floor, "new_sofa"),
            missing("s", floor_max + 1, "own_id_above_max"),
            missing("s", 5, &taken_name),
            missing("s", 424242, "keeps_its_id"),
            missing("i", taken_wall, "new_poster"),
        ];
        let out = missing_gamedata(&list, &source, None, Some(&hotel));
        assert!(out.checked);
        assert_eq!(out.in_hotel_furnidata, vec![("s".to_string(), taken_name.to_uppercase())]);
        let floor_ids: Vec<i64> = out.furnidata["roomitemtypes"]["furnitype"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect();
        // new_sofa takes the first free id above the hotel's max (the next one is the output's own).
        let expected_sofa = i64::from(floor_max) + 2;
        let mut expected = vec![i64::from(floor_max) + 1, 424242, expected_sofa];
        expected.sort_unstable();
        let mut got = floor_ids.clone();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert_eq!(
            out.renumbered,
            vec![
                Renumbered {
                    product_type: "s".into(),
                    classname: "new_sofa".into(),
                    old_id: i64::from(taken_floor),
                    new_id: expected_sofa
                },
                Renumbered {
                    product_type: "i".into(),
                    classname: "new_poster".into(),
                    old_id: i64::from(taken_wall),
                    new_id: i64::from(wall_max) + 1
                },
            ]
        );
        assert_eq!(out.floor(), 3);
        assert_eq!(out.wall(), 1);
        let products = out.productdata["productdata"]["product"].as_array().unwrap();
        assert_eq!(products.len(), 4);
        assert!(products.iter().any(|p| p["code"] == "new_sofa" && p["name"] == "Sofa"));
    }

    /// Flash XML furnidata becomes Nitro entries (bools from 0/1, partcolors, text fields).
    #[test]
    fn xml_becomes_nitro() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><furnidata><roomitemtypes>
            <furnitype id="13" classname="shelves_norja"><revision>61856</revision><category>shelf</category><defaultdir>0</defaultdir>
            <xdim>1</xdim><ydim>1</ydim><partcolors><color>#ffffff</color><color>#F7EBBC</color></partcolors>
            <name>Beige Bookcase</name><description>For nic naks &amp; art deco books</description><adurl/>
            <offerid>5</offerid><buyout>1</buyout><rentofferid>-1</rentofferid><rentbuyout>0</rentbuyout><bc>1</bc>
            <excludeddynamic>0</excludeddynamic><customparams/><specialtype>1</specialtype><canstandon>0</canstandon>
            <cansiton>0</cansiton><canlayon>0</canlayon><furniline>iced</furniline><environment></environment><rare>0</rare></furnitype>
            </roomitemtypes><wallitemtypes>
            <furnitype id="4001" classname="poster"><revision>1</revision><name>7</name><description></description><adurl></adurl>
            <offerid>-1</offerid><buyout>0</buyout><rentofferid>-1</rentofferid><rentbuyout>0</rentbuyout><bc>0</bc>
            <excludeddynamic>0</excludeddynamic><specialtype>1</specialtype><furniline>100</furniline><environment/><rare>0</rare></furnitype>
            </wallitemtypes></furnidata>"#;
        let source = Furnidata::parse(xml).unwrap();
        let list = vec![missing("s", 13, "shelves_norja"), missing("i", 4001, "poster")];
        let out = missing_gamedata(&list, &source, None, None);
        assert_eq!(
            gamedata::to_text(&out.furnidata),
            concat!(
                r##"{"roomitemtypes":{"furnitype":[{"id":13,"classname":"shelves_norja","revision":61856,"category":"shelf","defaultdir":0,"xdim":1,"ydim":1,"partcolors":{"color":["#ffffff","#F7EBBC"]},"name":"Beige Bookcase","description":"For nic naks & art deco books","adurl":null,"offerid":5,"buyout":true,"rentofferid":-1,"rentbuyout":false,"bc":true,"excludeddynamic":false,"bcofferid":5,"customparams":null,"specialtype":1,"canstandon":false,"cansiton":false,"canlayon":false,"canputstuffon":false,"height":1.0,"furniline":"iced","environment":"","rare":false,"tradeable":true,"recyclable":true}]},"##,
                r##""wallitemtypes":{"furnitype":[{"id":4001,"classname":"poster","revision":1,"category":"other","name":"7","description":"","adurl":null,"specialtype":1,"furniline":"100","environment":null,"rare":false,"tradeable":true,"recyclable":true,"offerid":-1,"buyout":false,"rentofferid":-1,"rentbuyout":false,"bc":false,"excludeddynamic":false,"bcofferid":-1}]}}"##
            )
        );
    }

    /// The Manager's furni import reads missing_furnidata.json as it is written.
    #[test]
    fn the_furni_import_reads_it() {
        use aio_remote::furni_import::{plan_with, Bundle, DbState};
        let source = Furnidata::parse(include_str!("../tests/fixtures/nitro_furnituredata.json")).unwrap();
        let list: Vec<MissingFurni> = source
            .entries
            .iter()
            .map(|((kind, id), e)| missing(&kind.to_string(), *id, e["classname"].as_str().unwrap()))
            .collect();
        let out = missing_gamedata(&list, &source, None, None);
        let written: Value = serde_json::from_str(&gamedata::to_text(&out.furnidata)).unwrap();
        let read = |_: &str| {
            Some(Bundle {
                width: 1.0,
                length: 1.0,
                height: 1.0,
                modes: 1,
                multiheight: None,
            })
        };
        let plan = plan_with(&written, &DbState::default(), &read);
        assert_eq!(plan.new.len(), source.len());
        let shelf = plan.new.iter().find(|f| f.classname == "shelves_norja").unwrap();
        assert_eq!((shelf.sprite_id, shelf.offer_id, shelf.kind), (Some(13), 5, 's'));
        assert!(plan.new.iter().any(|f| f.kind == 'i'));
    }
}
