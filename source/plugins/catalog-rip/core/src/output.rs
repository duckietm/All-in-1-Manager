//! What a rip writes, as bytes: the catalogue assembled from the answers, the data files
//! (catalog.json, the Arcturus SQL, catalog_manager.sql, missing_furni.json,
//! missing_furnidata.json, catalog_extras.json) and README.txt. The job hands them to its
//! `Sink`; nothing here does I/O.
//!
//! (`Output::write` and `recording::assemble` of the built-in crate. Not in this version:
//! `packets/`, catalog_changes.json and catalog_manager_changes.sql, so the README never has
//! their PACKETS and CHANGES sections.)

use std::collections::{BTreeMap, HashMap};

use crate::export::{self, ItemsBase, RippedCatalog, RippedMode, SqlStats};
use crate::extras;
use crate::furnidata::Furnidata;
use crate::manager::{self, ExistingCatalogue, ManagerExport, ManagerMode, ManagerStats};
use crate::missing::{self, MissingFurni};
use crate::packets::{CatalogIndex, CatalogPage};
use crate::productdata::Productdata;
use crate::profiles::HotelProfile;
use crate::ItemsBaseRow;

/// items_base rows as the lookup the exporters use (what the built-in did with the rows its
/// `ItemsSource` returned).
pub fn items_base(rows: &[ItemsBaseRow]) -> ItemsBase {
    let mut items = ItemsBase::default();
    for row in rows {
        items.add(
            row.id,
            row.sprite_id,
            &row.item_type,
            &row.item_name,
            row.effect_id,
        );
    }
    items
}

/// The hotel connection a catalogue was ripped from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source<'a> {
    pub host: &'a str,
    pub hotel_version: &'a str,
    pub client_type: &'a str,
    /// `YYYY-mm-dd HH:MM:SS`.
    pub ripped_at: &'a str,
    /// What makes the files ([`RippedCatalog::generator`]).
    pub generator: &'a str,
}

/// The catalogue as the rip writes it: per catalogue in `modes` that has an index, every page
/// of its tree that answered; `failed` lists the pages without an answer.
pub fn assemble(
    source: &Source,
    modes: &[String],
    indexes: &HashMap<String, CatalogIndex>,
    pages: &HashMap<(String, i32), CatalogPage>,
    failed: &BTreeMap<String, Vec<i32>>,
    complete: bool,
) -> RippedCatalog {
    let mut out = Vec::new();
    for mode in modes {
        let Some(index) = indexes.get(mode) else {
            continue;
        };
        let mut found = HashMap::new();
        for (node, _, _) in index.walk() {
            if node.page_id > 0 {
                if let Some(page) = pages.get(&(mode.clone(), node.page_id)) {
                    found.insert(node.page_id, page.clone());
                }
            }
        }
        out.push(RippedMode {
            mode: mode.clone(),
            index: index.clone(),
            pages: found,
            failed: failed.get(mode).cloned().unwrap_or_default(),
        });
    }
    RippedCatalog {
        hotel: crate::names::hotel_name(source.host),
        host: source.host.to_string(),
        hotel_version: source.hotel_version.to_string(),
        client_type: source.client_type.to_string(),
        ripped_at: source.ripped_at.to_string(),
        generator: source.generator.to_string(),
        complete,
        modes: out,
    }
}

/// What the data files are written from.
#[derive(Debug, Clone, Copy)]
pub struct ExportInput<'a> {
    pub catalog: &'a RippedCatalog,
    pub items: Option<&'a ItemsBase>,
    pub furnidata: Option<&'a Furnidata>,
    pub products: Option<&'a Productdata>,
    pub manager_mode: ManagerMode,
    pub existing: Option<&'a ExistingCatalogue>,
    /// The answers of the extra messages, by [`extras::Extra::key`].
    pub extra_answers: &'a BTreeMap<String, Vec<u8>>,
}

/// The data files, in the order the built-in writes them, with what the README and the summary
/// need.
#[derive(Debug, Clone)]
pub struct DataFiles {
    /// (file name, bytes).
    pub files: Vec<(String, Vec<u8>)>,
    pub sql: SqlStats,
    pub manager: ManagerStats,
    pub missing_furni: Vec<MissingFurni>,
    /// Log lines the built-in writes while making them.
    pub log: Vec<String>,
}

fn pretty(value: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_string_pretty(value)
        .unwrap_or_default()
        .into_bytes()
}

/// catalog.json, catalog_pages.sql, catalog_items.sql (+ the `_bc` pair when the Builders Club
/// was ripped), catalog_manager.sql, missing_furni.json and missing_furnidata.json (when
/// products are missing; the latter only with a furnidata), catalog_extras.json (when an extra
/// answered).
pub fn data_files(input: &ExportInput) -> DataFiles {
    let catalog = input.catalog;
    let mut files = Vec::new();
    let mut log = Vec::new();
    let mut json = export::catalog_json(catalog);
    if let Some(products) = input.products {
        json["product_names"] = product_names(catalog, input.furnidata, products);
    }
    files.push(("catalog.json".to_string(), pretty(&json)));
    let sql = export::arcturus_sql_with(catalog, input.items, input.furnidata);
    files.push(("catalog_pages.sql".into(), sql.pages.into_bytes()));
    files.push(("catalog_items.sql".into(), sql.items.into_bytes()));
    if !sql.pages_bc.is_empty() {
        files.push(("catalog_pages_bc.sql".into(), sql.pages_bc.into_bytes()));
        files.push(("catalog_items_bc.sql".into(), sql.items_bc.into_bytes()));
    }
    // The hotel's ids are only asked for in the modes that keep what is there.
    let existing = if input.manager_mode == ManagerMode::Replace {
        None
    } else {
        input.existing
    };
    let manager = manager::manager_sql_with(
        catalog,
        &ManagerExport {
            items: input.items,
            furnidata: input.furnidata,
            mode: input.manager_mode,
            existing,
        },
    );
    if !manager.stats.note.is_empty() {
        log.push(format!("Manager import file: {}", manager.stats.note));
    }
    files.push(("catalog_manager.sql".into(), manager.text.into_bytes()));

    let missing_furni =
        missing::missing_furni(catalog, input.items, input.furnidata, input.products);
    if !missing_furni.is_empty() {
        log.push(format!(
            "{} products are not in the hotel's items_base: missing_furni.json{}.",
            n0(missing_furni.len()),
            if input.furnidata.is_some() {
                " and missing_furnidata.json (for the furni import)"
            } else {
                ""
            }
        ));
        files.push((
            "missing_furni.json".into(),
            pretty(&serde_json::json!({
                "count": missing_furni.len(),
                "furni": missing_furni,
            })),
        ));
        if let Some(data) = input.furnidata {
            files.push((
                "missing_furnidata.json".into(),
                pretty(&missing::missing_furnidata(&missing_furni, data)),
            ));
        }
    }
    if !input.extra_answers.is_empty() {
        files.push((
            "catalog_extras.json".into(),
            pretty(&extras::extras_json(input.extra_answers)),
        ));
    }
    DataFiles {
        files,
        sql: sql.stats,
        manager: manager.stats,
        missing_furni,
        log,
    }
}

/// `product_names` of catalog.json: the productdata name and description of every offer's
/// localization id and product class name that has one.
pub fn product_names(
    catalog: &RippedCatalog,
    furnidata: Option<&Furnidata>,
    products: &Productdata,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for mode in &catalog.modes {
        for page in mode.pages.values() {
            for offer in &page.offers {
                let mut codes = vec![offer.localization_id.clone()];
                codes.extend(
                    offer
                        .products
                        .iter()
                        .filter_map(|p| furnidata.and_then(|f| f.classname(p)))
                        .map(str::to_string),
                );
                for code in codes {
                    if let Some((name, description)) = products.get(&code) {
                        out.entry(code).or_insert_with(
                            || serde_json::json!({ "name": name, "description": description }),
                        );
                    }
                }
            }
        }
    }
    serde_json::Value::Object(out)
}

/// What README.txt is written from.
#[derive(Debug, Clone, Copy)]
pub struct ReadmeInput<'a> {
    pub catalog: &'a RippedCatalog,
    pub sql: &'a SqlStats,
    /// The IMAGES line ("12 downloaded, 1 not on the server", "not downloaded (turned off)", ...).
    pub images: &'a str,
    pub database_note: &'a str,
    pub manager: &'a ManagerStats,
    pub with_items_base: bool,
    pub profile: &'a HotelProfile,
    pub missing_furni: usize,
    /// Class names in the furnidata used (None: no furnidata).
    pub furnidata_names: Option<usize>,
    /// The user cancelled the rip: a CANCELLED section says so (not in the built-in).
    pub cancelled: bool,
    /// Windows line ends (the built-in writes them on Windows).
    pub crlf: bool,
}

/// The CANCELLED section (the built-in has none; see [`ReadmeInput::cancelled`]).
pub const CANCELLED_SECTION: &str =
    "\nCANCELLED\n  The rip was cancelled: the files hold what was read until then.\n";

/// README.txt.
pub fn readme(input: &ReadmeInput) -> String {
    let mut readme = export::readme(input.catalog, input.sql, input.images, input.database_note);
    readme.push_str(&manager::readme_section(
        input.manager,
        input.with_items_base,
    ));
    readme.push_str(&format!(
        "\nHOTEL PROFILE\n  {} ({})\n",
        input.profile.name,
        input.profile.kind.label()
    ));
    if input.missing_furni > 0 {
        readme.push_str(&format!(
            "\nMISSING FURNI (missing_furni.json)\n  {} products are not in the hotel's items_base, so their offers are not in catalog_manager.sql.\n  {}\n",
            input.missing_furni,
            if input.furnidata_names.is_some() {
                "missing_furnidata.json lists them as a furnidata: give it to the Manager's furni import first."
            } else {
                "With the source hotel's furnidata they would also be written as a furnidata for the furni import."
            }
        ));
    }
    if let Some(names) = input.furnidata_names {
        readme.push_str(&format!(
            "\nFURNIDATA\n  {names} class names of the source hotel: floor and wall products are matched by\n  class name (items_base.item_name), not by sprite id.\n",
        ));
    }
    if input.cancelled {
        readme.push_str(CANCELLED_SECTION);
    }
    if input.crlf {
        readme.replace('\n', "\r\n")
    } else {
        readme
    }
}

/// 12345 -> "12,345".
pub fn n0(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(n0(0), "0");
        assert_eq!(n0(1234), "1,234");
        assert_eq!(n0(1234567), "1,234,567");
    }

    #[test]
    fn items_base_from_rows() {
        let rows = [ItemsBaseRow {
            id: 10,
            sprite_id: 4001,
            item_type: "s".into(),
            item_name: "rare_dragon".into(),
            effect_id: 0,
        }];
        let items = items_base(&rows);
        assert_eq!(items.by_sprite[&("s".to_string(), 4001)], 10);
    }
}
