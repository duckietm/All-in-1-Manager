//! The products of a rip that the target hotel's items_base lacks: what keeps offers out of the
//! Manager import. Written as `missing_furni.json` (a list), and — with the source furnidata —
//! `missing_furnidata.json`: a furnidata JSON of only those furni, the input the Manager's furni
//! import (`aio_remote::furni_import::plan`) reads, so the furni can be added to items_base
//! (with their client files) before the catalogue is imported.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::export::{ItemsBase, RippedCatalog};
use crate::furnidata::Furnidata;
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

/// The furnidata entries of the missing floor and wall furni (see the module docs).
pub fn missing_furnidata(missing: &[MissingFurni], furnidata: &Furnidata) -> Value {
    let wanted: Vec<(char, i32)> = missing
        .iter()
        .filter_map(|m| {
            let kind = match m.product_type.as_str() {
                "s" => 's',
                "i" => 'i',
                _ => return None,
            };
            Some((kind, m.class_id?))
        })
        .collect();
    furnidata.subset(&wanted)
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
        let data = missing_furnidata(&list, &furni);
        assert_eq!(data["roomitemtypes"]["furnitype"].as_array().unwrap().len(), 1);
        assert_eq!(data["roomitemtypes"]["furnitype"][0]["name"], "Table");
        assert_eq!(data["wallitemtypes"]["furnitype"][0]["classname"], "poster");
    }
}
