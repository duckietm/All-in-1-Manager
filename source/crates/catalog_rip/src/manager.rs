//! `catalog_manager.sql`: the rip in the All-in-1 Manager's catalogue import format (Octane's
//! Catalog Studio restricted SQL, read by `aio_remote::catalog_transfer`).
//!
//! That import reads only `INSERT`/`UPDATE`/`DELETE` of `catalog_pages` and `catalog_items` with
//! whole numbers, `'text'` (quotes doubled) and NULL, never a subquery, and keeps the normal
//! catalogue and the Builders Club warehouse apart with a `catalog_type` column (NORMAL /
//! BUILDER, each with its own ids). So, unlike the Arcturus files:
//! * a page in both catalogues is written twice, once per `catalog_type`;
//! * `item_ids` must be literal items_base ids: an offer with a product the items_base lookup
//!   does not know is left out (listed in a comment and counted).
//!
//! Four ways to put it in the hotel ([`ManagerMode`]):
//! * `Replace`: `DELETE FROM catalog_items; DELETE FROM catalog_pages;` first, as the Studio
//!   export does: the whole catalogue (normal and Builders Club) becomes the rip;
//! * `ReplaceRipped`: only the catalogues that were ripped are replaced (one DELETE per existing
//!   page and offer of those catalogues, so it needs the hotel's ids: [`ExistingCatalogue`]);
//! * `Add`: nothing is deleted; every ripped page and offer gets an id above the hotel's
//!   highest, so the rip lands next to the existing catalogue;
//! * `Merge`: nothing is deleted or changed; the rip is fitted into the hotel's catalogue (its
//!   page tree and the furni each page sells: [`ExistingCatalogue`]). A ripped page whose path
//!   (captions from the root, trimmed, case-insensitive, per catalog_type) the hotel has is
//!   joined: only its offers are added there, and an offer whose items_base ids (bundles: the
//!   same set) that page already sells is skipped. The other pages go under one page "Imported
//!   from <hotel>" per catalogue, keeping the rip's shape; merging the same rip again adds
//!   nothing.
//!
//! The Manager previews every change of the file before writing anything.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::export::{
    arcturus_layout, comment, first_free_page_id, fit, item_ids_sql_with, map_page_content,
    numbered_pages, ItemsBase, NumberedPage, RippedCatalog,
};
use crate::furnidata::Furnidata;
use crate::packets::{IndexNode, Offer};

/// How catalog_manager.sql changes the hotel's catalogue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerMode {
    /// Replace the whole catalogue, normal and Builders Club.
    #[default]
    Replace,
    /// Replace only the catalogues that were ripped (needs the hotel's ids).
    ReplaceRipped,
    /// Add the rip next to the existing catalogue, with new ids.
    Add,
    /// Merge the rip into the existing catalogue (needs its pages and offers): pages with the
    /// same path are joined (only offers added, never changed), the others go under an
    /// "Imported from <hotel>" folder, offers whose furni the page already sells are skipped.
    Merge,
}

impl ManagerMode {
    pub fn label(self) -> &'static str {
        match self {
            ManagerMode::Replace => "replace the whole catalogue",
            ManagerMode::ReplaceRipped => "replace the ripped catalogues",
            ManagerMode::Add => "add next to the existing catalogue",
            ManagerMode::Merge => "merge into the existing catalogue",
        }
    }
}

/// One page of the hotel's catalogue, as read for the merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPage {
    pub id: i64,
    /// -1 (or 0) for a page at the root.
    pub parent_id: i64,
    pub caption: String,
    /// "NORMAL" or "BUILDER".
    pub catalog_type: String,
    pub order_num: i64,
}

/// One offer of the hotel's catalogue, as read for the merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingOffer {
    pub id: i64,
    pub page_id: i64,
    /// "NORMAL" or "BUILDER".
    pub catalog_type: String,
    /// As in catalog_items: `12`, `12;13`, `12:2;13:1`, ...
    pub item_ids: String,
}

/// The hotel's catalogue, per `catalog_type` ("NORMAL" / "BUILDER"): the ids (for
/// `ReplaceRipped` / `Add`) and, when it was read whole, the page tree and the furni each
/// page's offers sell (for `Merge`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExistingCatalogue {
    pub pages: BTreeMap<String, BTreeSet<i64>>,
    pub offers: BTreeMap<String, BTreeSet<i64>>,
    /// Every page with its parent and caption; empty when only the ids are known.
    pub page_tree: Vec<ExistingPage>,
    /// Per catalog_type, page id → the items_base ids of each offer on it (sorted, no repeats).
    pub offers_by_page: BTreeMap<String, BTreeMap<i64, Vec<Vec<i64>>>>,
}

/// The items_base ids an offer sells, sorted, without repeats or counts (`12:2;13` → 12, 13).
pub fn offer_item_ids(item_ids: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = item_ids
        .split([';', ','])
        .filter_map(|part| part.split(':').next()?.trim().parse().ok())
        .filter(|id| *id > 0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A caption as pages are matched by: trimmed, case-insensitive.
pub fn caption_key(caption: &str) -> String {
    caption.trim().to_lowercase()
}

impl ExistingCatalogue {
    /// The catalogue from the rows read: ids, page tree and offers per page, per catalog_type.
    pub fn from_rows(pages: Vec<ExistingPage>, offers: Vec<ExistingOffer>) -> Self {
        let mut existing = ExistingCatalogue::default();
        for mut page in pages {
            let kind = catalog_type(page.catalog_type.trim());
            page.catalog_type = kind.to_string();
            existing.pages.entry(kind.to_string()).or_default().insert(page.id);
            existing.page_tree.push(page);
        }
        for offer in offers {
            let kind = catalog_type(offer.catalog_type.trim());
            existing.offers.entry(kind.to_string()).or_default().insert(offer.id);
            existing
                .offers_by_page
                .entry(kind.to_string())
                .or_default()
                .entry(offer.page_id)
                .or_default()
                .push(offer_item_ids(&offer.item_ids));
        }
        existing
    }

    /// Whether the page tree was read (an empty catalogue counts as read).
    pub fn has_tree(&self) -> bool {
        !self.page_tree.is_empty() || self.pages.values().all(BTreeSet::is_empty)
    }

    /// The matching keys of the captions from the root down to page `id` of `kind`; None when the
    /// page is not there or does not hang from the root (a missing parent or a loop).
    pub fn page_path(&self, kind: &str, id: i64) -> Option<Vec<String>> {
        let by_id: HashMap<i64, &ExistingPage> = self
            .page_tree
            .iter()
            .filter(|p| p.catalog_type == kind)
            .map(|p| (p.id, p))
            .collect();
        let mut path = Vec::new();
        let mut current = by_id.get(&id)?;
        loop {
            path.push(caption_key(&current.caption));
            if current.parent_id <= 0 {
                break;
            }
            if path.len() > by_id.len() {
                return None;
            }
            current = by_id.get(&current.parent_id)?;
        }
        path.reverse();
        Some(path)
    }

    fn max(map: &BTreeMap<String, BTreeSet<i64>>, kind: &str) -> i64 {
        map.get(kind)
            .and_then(|ids| ids.iter().next_back().copied())
            .unwrap_or(0)
            .max(0)
    }

    pub fn max_page(&self, kind: &str) -> i64 {
        Self::max(&self.pages, kind)
    }

    pub fn max_offer(&self, kind: &str) -> i64 {
        Self::max(&self.offers, kind)
    }
}

/// What to write catalog_manager.sql with.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManagerExport<'a> {
    pub items: Option<&'a ItemsBase>,
    /// The source hotel's furnidata: products matched by class name.
    pub furnidata: Option<&'a Furnidata>,
    pub mode: ManagerMode,
    pub existing: Option<&'a ExistingCatalogue>,
}

/// What `catalog_manager.sql` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagerStats {
    /// The mode the file was written in (a `ReplaceRipped` without the hotel's ids falls back).
    pub mode: ManagerMode,
    /// Why the mode is not the one asked for, or what to watch on import.
    pub note: String,
    pub pages: usize,
    pub offers: usize,
    /// Existing pages / offers the file deletes (`ReplaceRipped`).
    pub deleted_pages: usize,
    pub deleted_offers: usize,
    /// Offers left out: a product is not in items_base (or no items_base was given).
    pub offers_unresolved: usize,
    pub offers_without_products: usize,
    /// Offers shown on more than one page of a catalogue: new ids after the first.
    pub renumbered_offers: usize,
    /// Disabled pages (id -1 in the index) that got a new id.
    pub synthetic_page_ids: usize,
    /// Values with a backslash right before a quote or at the end, which the import's tokenizer
    /// reads as an escape: one backslash was added so the statement stays readable.
    pub adjusted_values: usize,
    pub truncated_values: usize,
    /// What the merge does (`Merge` only).
    pub merge: Option<MergeSummary>,
}

pub struct ManagerSql {
    pub text: String,
    pub stats: ManagerStats,
}

/// `'text'` for the Manager's import: quotes doubled. Its tokenizer pairs a backslash with the
/// next character, so an odd run of backslashes before a quote or at the end gets one more.
pub fn manager_str(value: &str, adjusted: &mut usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    let mut i = 0;
    let mut changed = false;
    while i < chars.len() {
        if chars[i] == '\\' {
            let start = i;
            while i < chars.len() && chars[i] == '\\' {
                out.push('\\');
                i += 1;
            }
            let before_quote_or_end = i == chars.len() || chars[i] == '\'';
            if (i - start) % 2 == 1 && before_quote_or_end {
                out.push('\\');
                changed = true;
            }
            continue;
        }
        if chars[i] == '\'' {
            out.push_str("''");
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out.push('\'');
    if changed {
        *adjusted += 1;
    }
    out
}

/// `catalog_type` for a ripped mode.
pub fn catalog_type(mode: &str) -> &'static str {
    if mode.eq_ignore_ascii_case("NORMAL") {
        "NORMAL"
    } else {
        "BUILDER"
    }
}

const PAGE_COLUMNS: &str = "id, catalog_type, parent_id, caption_save, caption, page_layout, icon_color, icon_image, min_rank, order_num, visible, enabled, club_only, catalog_mode, vip_only, page_headline, page_teaser, page_special, page_text1, page_text2, page_text_details, page_text_teaser, room_id, includes";
const OFFER_COLUMNS: &str = "id, catalog_type, item_ids, page_id, catalog_name, cost_credits, cost_points, points_type, amount, limited_stack, order_number, offer_id, song_id, extradata, have_offer, club_only";

/// The whole rip as one file for the Manager's catalogue import, replacing the whole catalogue.
pub fn manager_sql(catalog: &RippedCatalog, items: Option<&ItemsBase>) -> ManagerSql {
    manager_sql_with(
        catalog,
        &ManagerExport {
            items,
            ..ManagerExport::default()
        },
    )
}

/// The mode actually used, and why it differs.
fn effective_mode(catalog: &RippedCatalog, export: &ManagerExport) -> (ManagerMode, String) {
    let ripped: BTreeSet<&str> = catalog.modes.iter().map(|m| catalog_type(&m.mode)).collect();
    match (export.mode, export.existing) {
        (ManagerMode::ReplaceRipped, None) if ripped.len() == 2 => (
            ManagerMode::Replace,
            "Both catalogues were ripped: the whole catalogue is replaced.".into(),
        ),
        (ManagerMode::ReplaceRipped, None) => (
            ManagerMode::Add,
            "The hotel's catalogue ids were not available, so only the ripped catalogue cannot be replaced: the rip is added with its own ids instead (the import preview shows any id that is already taken).".into(),
        ),
        (ManagerMode::Add, None) => (
            ManagerMode::Add,
            "The hotel's catalogue ids were not available: the ripped ids are kept (the import preview shows any id that is already taken).".into(),
        ),
        (ManagerMode::Merge, None) => (
            ManagerMode::Add,
            "The hotel's catalogue was not available, so the rip cannot be merged into it: it is added with its own ids instead (the import preview shows any id that is already taken).".into(),
        ),
        (ManagerMode::Merge, Some(existing)) if !existing.has_tree() => (
            ManagerMode::Add,
            "Only the hotel's catalogue ids were available, not its pages, so the rip cannot be merged into it: it is added next to the catalogue with new ids instead.".into(),
        ),
        (mode, _) => (mode, String::new()),
    }
}

/// One row of catalog_manager.sql: a page or an offer with its column values (SQL literals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerRow {
    pub page: bool,
    /// "NORMAL" or "BUILDER".
    pub kind: &'static str,
    pub id: i64,
    /// Every column but `id` and `catalog_type`, in the import's column order.
    pub columns: Vec<(&'static str, String)>,
    /// What the row is, for reports: the page name or the offer's localization id.
    pub label: String,
    /// A page whose content was read (false: only its place in the tree is known). Offers: true.
    pub has_content: bool,
}

impl ManagerRow {
    pub fn key(&self) -> (bool, &'static str, i64) {
        (self.page, self.kind, self.id)
    }

    /// `INSERT INTO ... VALUES (...);`
    pub fn insert(&self) -> String {
        let table = if self.page { "catalog_pages" } else { "catalog_items" };
        let columns = if self.page { PAGE_COLUMNS } else { OFFER_COLUMNS };
        let values: Vec<&str> = self.columns.iter().map(|(_, v)| v.as_str()).collect();
        format!(
            "INSERT INTO {table} ({columns}) VALUES ({}, '{}', {});",
            self.id,
            self.kind,
            values.join(", ")
        )
    }

    /// `UPDATE ... SET <only these columns> WHERE id = .. AND catalog_type = ..;`
    pub fn update(&self, changed: &[&str]) -> String {
        let table = if self.page { "catalog_pages" } else { "catalog_items" };
        let set: Vec<String> = self
            .columns
            .iter()
            .filter(|(c, _)| changed.contains(c))
            .map(|(c, v)| format!("{c} = {v}"))
            .collect();
        format!(
            "UPDATE {table} SET {} WHERE id = {} AND catalog_type = '{}';",
            set.join(", "),
            self.id,
            self.kind
        )
    }

    /// `DELETE FROM ... WHERE id = .. AND catalog_type = ..;`
    pub fn delete(&self) -> String {
        let table = if self.page { "catalog_pages" } else { "catalog_items" };
        format!(
            "DELETE FROM {table} WHERE id = {} AND catalog_type = '{}';",
            self.id, self.kind
        )
    }
}

/// The rows of catalog_manager.sql, pages before offers (catalogue by catalogue), with the stats
/// and the `-- skipped:` notes of the offers left out. The DELETE lines of the mode are not rows.
pub fn manager_rows(
    catalog: &RippedCatalog,
    export: &ManagerExport,
) -> (Vec<ManagerRow>, ManagerStats, Vec<String>) {
    let (mode, note) = effective_mode(catalog, export);
    let mut stats = ManagerStats {
        mode,
        note,
        ..ManagerStats::default()
    };
    if let (ManagerMode::Merge, Some(existing)) = (mode, export.existing) {
        return merge_rows(catalog, export, existing, stats);
    }
    let mut page_rows = Vec::new();
    let mut offer_rows = Vec::new();
    let mut notes = Vec::new();
    for ripped in &catalog.modes {
        let kind = catalog_type(&ripped.mode);
        let (page_offset, offer_offset) = match (mode, export.existing) {
            (ManagerMode::Add, Some(existing)) => {
                (existing.max_page(kind), existing.max_offer(kind))
            }
            _ => (0, 0),
        };
        let page_id = |id: i32| -> i64 {
            if id > 0 {
                id as i64 + page_offset
            } else {
                id as i64
            }
        };
        let mut next = first_free_page_id([ripped]);
        let pages = numbered_pages(ripped, &mut next, &mut stats.synthetic_page_ids);
        let mut seen_pages = HashSet::new();
        let mut seen_offers: HashSet<i32> = HashSet::new();
        let mut next_offer = pages
            .iter()
            .filter_map(|r| r.page)
            .flat_map(|p| p.offers.iter().map(|o| o.offer_id))
            .max()
            .unwrap_or(0)
            .max(0)
            + 1;
        let mut skipped: Vec<(i32, String)> = Vec::new();
        let mut skipped_seen: HashSet<i32> = HashSet::new();
        for row in pages.iter().filter(|r| seen_pages.insert(r.id)) {
            let columns = page_columns(row, kind, page_id(row.parent_id), row.order, &mut stats);
            page_rows.push(ManagerRow {
                page: true,
                kind,
                id: page_id(row.id),
                columns,
                label: page_label(row.node).to_string(),
                has_content: row.page.is_some(),
            });
            stats.pages += 1;

            let Some(page) = row.page else { continue };
            for (position, offer) in page.offers.iter().enumerate() {
                let literal = match resolve_offer(offer, export) {
                    OfferIds::Literal(literal) => literal,
                    OfferIds::NoProducts => {
                        stats.offers_without_products += 1;
                        continue;
                    }
                    OfferIds::Unresolved(products) => {
                        stats.offers_unresolved += 1;
                        if skipped_seen.insert(offer.offer_id) {
                            skipped.push((
                                offer.offer_id,
                                format!(
                                    "skipped: {kind} offer {} '{}' on page {}: {products} not in items_base",
                                    offer.offer_id, offer.localization_id, row.id,
                                ),
                            ));
                        }
                        continue;
                    }
                };
                let id = if offer.offer_id > 0 && seen_offers.insert(offer.offer_id) {
                    offer.offer_id
                } else {
                    stats.renumbered_offers += 1;
                    next_offer += 1;
                    next_offer - 1
                } as i64
                    + offer_offset;
                // Added offers keep no client offer id: it could be one the hotel already uses.
                let client_offer_id = if mode == ManagerMode::Add {
                    -1
                } else {
                    offer.offer_id
                };
                let columns = offer_columns(
                    offer,
                    &literal,
                    page_id(row.id),
                    position + 1,
                    client_offer_id,
                    &mut stats,
                );
                offer_rows.push(ManagerRow {
                    page: false,
                    kind,
                    id,
                    columns,
                    label: offer.localization_id.clone(),
                    has_content: true,
                });
                stats.offers += 1;
            }
        }
        skipped.sort();
        notes.extend(skipped.into_iter().map(|(_, n)| n));
    }
    page_rows.extend(offer_rows);
    (page_rows, stats, notes)
}

/// The name a ripped page is shown (and merged) by: its caption, else its internal name.
fn page_label(node: &IndexNode) -> &str {
    if node.localization.trim().is_empty() {
        &node.name
    } else {
        &node.localization
    }
}

/// A ripped page's columns (all but id and catalog_type).
fn page_columns(
    row: &NumberedPage,
    kind: &str,
    parent_id: i64,
    order: usize,
    stats: &mut ManagerStats,
) -> Vec<(&'static str, String)> {
    let node = row.node;
    let layout_code = row.page.map(|p| p.layout.as_str()).unwrap_or("");
    let layout = arcturus_layout(layout_code).unwrap_or("default_3x3");
    let content = row
        .page
        .map(|p| map_page_content(&p.layout, &p.images, &p.texts))
        .unwrap_or_default();
    let a = &mut stats.adjusted_values;
    let t = &mut stats.truncated_values;
    vec![
        ("parent_id", parent_id.to_string()),
        ("caption_save", manager_str(&fit(&node.name, 128, t), a)),
        ("caption", manager_str(&fit(&node.localization, 128, t), a)),
        ("page_layout", format!("'{layout}'")),
        ("icon_color", "1".into()),
        ("icon_image", node.icon.to_string()),
        ("min_rank", "1".into()),
        ("order_num", order.to_string()),
        ("visible", if node.visible { "1" } else { "0" }.into()),
        ("enabled", if node.page_id > 0 { "1" } else { "0" }.into()),
        ("club_only", "0".into()),
        ("catalog_mode", format!("'{kind}'")),
        ("vip_only", "0".into()),
        ("page_headline", manager_str(&fit(&content.headline, 1024, t), a)),
        ("page_teaser", manager_str(&fit(&content.teaser, 64, t), a)),
        ("page_special", manager_str(&fit(&content.special, 2048, t), a)),
        ("page_text1", manager_str(&content.text1, a)),
        ("page_text2", manager_str(&content.text2, a)),
        ("page_text_details", manager_str(&content.text_details, a)),
        ("page_text_teaser", manager_str(&content.text_teaser, a)),
        ("room_id", "0".into()),
        ("includes", "''".into()),
    ]
}

/// An offer's item_ids for the import.
enum OfferIds {
    /// Every product is in items_base: `12`, `12:2;13:1`.
    Literal(String),
    NoProducts,
    /// Some product is not in items_base: the products, for the note.
    Unresolved(String),
}

fn resolve_offer(offer: &Offer, export: &ManagerExport) -> OfferIds {
    if offer.products.is_empty() {
        return OfferIds::NoProducts;
    }
    let ids = item_ids_sql_with(offer, export.items, export.furnidata);
    if ids.unresolved > 0 {
        let products: Vec<String> = offer
            .products
            .iter()
            .map(|p| {
                let name = export.furnidata.and_then(|f| f.classname(p));
                match (p.class_id, name) {
                    (Some(c), Some(name)) => format!("{}:{c} ({name})", p.product_type),
                    (Some(c), None) => format!("{}:{c}", p.product_type),
                    (None, _) => format!("{}:{}", p.product_type, p.extra_param),
                }
            })
            .collect();
        return OfferIds::Unresolved(products.join(", "));
    }
    // All literal: item_ids_sql wrote a MariaDB string; take its text back out.
    OfferIds::Literal(
        ids.sql
            .trim_start_matches('\'')
            .trim_end_matches('\'')
            .to_string(),
    )
}

/// An offer's columns (all but id and catalog_type).
fn offer_columns(
    offer: &Offer,
    literal: &str,
    page_id: i64,
    order: usize,
    client_offer_id: i32,
    stats: &mut ManagerStats,
) -> Vec<(&'static str, String)> {
    let first = &offer.products[0];
    let amount = if offer.products.len() == 1 {
        first.count.max(1)
    } else {
        1
    };
    let stack = offer
        .products
        .iter()
        .find(|p| p.limited)
        .map(|p| p.limited_series_size)
        .unwrap_or(0);
    let a = &mut stats.adjusted_values;
    let t = &mut stats.truncated_values;
    vec![
        ("item_ids", manager_str(literal, a)),
        ("page_id", page_id.to_string()),
        ("catalog_name", manager_str(&fit(&offer.localization_id, 100, t), a)),
        ("cost_credits", offer.price_credits.to_string()),
        ("cost_points", offer.price_activity_points.to_string()),
        ("points_type", offer.activity_point_type.to_string()),
        ("amount", amount.to_string()),
        ("limited_stack", stack.to_string()),
        ("order_number", order.to_string()),
        ("offer_id", client_offer_id.to_string()),
        ("song_id", "0".into()),
        ("extradata", manager_str(&fit(&first.extra_param, 500, t), a)),
        ("have_offer", if offer.bundle_purchase_allowed { "1" } else { "0" }.into()),
        ("club_only", if offer.club_level > 0 { "1" } else { "0" }.into()),
    ]
}

// ---------------------------------------------------------------- merge

/// What a merge does, for the page and README.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeSummary {
    /// Ripped pages whose path the hotel has: their offers go to the hotel's page.
    pub joined_pages: usize,
    /// New pages, the "Imported from" folders and path folders included.
    pub new_pages: usize,
    /// "Imported from <hotel>" folders created (at most one per catalogue).
    pub folders: usize,
    pub offers_added: usize,
    /// Offers whose furni the target page already sells.
    pub offers_skipped: usize,
    /// The hotel has the same caption twice under one parent: the first page was joined.
    pub ambiguous: Vec<String>,
}

const ROOT: i64 = -1;

/// One ripped page's place in the hotel's catalogue.
#[derive(Clone, Copy)]
struct Placed<'a> {
    row: NumberedPage<'a>,
    /// The hotel's page at the same path (from the root).
    owner: Option<i64>,
    /// The page at the same path under an "Imported from" folder of an earlier merge.
    mirror: Option<i64>,
    /// Where its offers go.
    dest: i64,
    /// `dest` is created by this file.
    new: bool,
    /// A page created to keep the path of its new sub-pages (its offers went to the hotel's page).
    path_copy: Option<i64>,
}

/// The merge of one ripped catalogue (NORMAL or BUILDER) into the hotel's.
struct Merger<'a, 'e> {
    kind: &'static str,
    export: &'e ManagerExport<'e>,
    folder_caption: String,
    /// The hotel's pages by (parent, caption key), in catalogue order.
    children: HashMap<(i64, String), Vec<i64>>,
    /// The hotel's pages: (parent, caption).
    owner_pages: HashMap<i64, (i64, String)>,
    /// The items_base sets each page sells (the hotel's and the ones this file adds), by page.
    sold: HashMap<i64, HashSet<Vec<i64>>>,
    /// The next order_number per page.
    next_order: HashMap<i64, usize>,
    next_page: i64,
    next_offer: i64,
    root_order: i64,
    /// The "Imported from" folder of an earlier merge.
    old_folder: Option<i64>,
    new_folder: Option<i64>,
    placed: HashMap<i32, Placed<'a>>,
    /// Pages this file creates, by (parent, caption key).
    created: HashMap<(i64, String), i64>,
    /// Display paths of the pages this file creates.
    created_paths: HashMap<i64, String>,
    ambiguous_seen: HashSet<(i64, String)>,
    pages: Vec<ManagerRow>,
    offers: Vec<ManagerRow>,
    notes: Vec<String>,
    skipped: Vec<String>,
    summary: MergeSummary,
}

impl<'a, 'e> Merger<'a, 'e> {
    fn new(
        kind: &'static str,
        hotel: &str,
        existing: &ExistingCatalogue,
        export: &'e ManagerExport<'e>,
    ) -> Self {
        let mut ordered: Vec<&ExistingPage> = existing
            .page_tree
            .iter()
            .filter(|p| p.catalog_type == kind)
            .collect();
        ordered.sort_by_key(|p| (p.order_num, p.id));
        let mut children: HashMap<(i64, String), Vec<i64>> = HashMap::new();
        let mut owner_pages = HashMap::new();
        let mut root_order = 0;
        for page in &ordered {
            let parent = if page.parent_id <= 0 { ROOT } else { page.parent_id };
            if parent == ROOT {
                root_order = root_order.max(page.order_num);
            }
            children
                .entry((parent, caption_key(&page.caption)))
                .or_default()
                .push(page.id);
            owner_pages.insert(page.id, (parent, page.caption.clone()));
        }
        let folder_caption = format!("Imported from {}", hotel.trim());
        let old_folder = children
            .get(&(ROOT, caption_key(&folder_caption)))
            .and_then(|ids| ids.first().copied());
        let by_page = existing.offers_by_page.get(kind);
        Merger {
            kind,
            export,
            folder_caption,
            children,
            owner_pages,
            sold: HashMap::new(),
            next_order: by_page
                .map(|m| m.iter().map(|(page, offers)| (*page, offers.len() + 1)).collect())
                .unwrap_or_default(),
            next_page: existing.max_page(kind) + 1,
            next_offer: existing.max_offer(kind) + 1,
            root_order,
            old_folder,
            new_folder: None,
            placed: HashMap::new(),
            created: HashMap::new(),
            created_paths: HashMap::new(),
            ambiguous_seen: HashSet::new(),
            pages: Vec::new(),
            offers: Vec::new(),
            notes: Vec::new(),
            skipped: Vec::new(),
            summary: MergeSummary::default(),
        }
    }

    /// `Furniture › Iced` for a page of the hotel or of this file.
    fn path(&self, id: i64) -> String {
        if id == ROOT {
            return String::new();
        }
        if let Some(path) = self.created_paths.get(&id) {
            return path.clone();
        }
        let mut parts = Vec::new();
        let mut current = id;
        while let Some((parent, caption)) = self.owner_pages.get(&current) {
            parts.push(caption.trim().to_string());
            if *parent == ROOT || parts.len() > self.owner_pages.len() {
                break;
            }
            current = *parent;
        }
        parts.reverse();
        parts.join(" › ")
    }

    /// The hotel's page `key` under `parent`; the first one when there are more (reported).
    fn find(&mut self, parent: i64, key: &str) -> Option<i64> {
        let ids = self.children.get(&(parent, key.to_string()))?;
        let first = *ids.first()?;
        if ids.len() > 1 && self.ambiguous_seen.insert((parent, key.to_string())) {
            let list: Vec<String> = ids.iter().map(i64::to_string).collect();
            let text = format!(
                "{} catalogue: {} pages '{}' under {} (ids {}): page {first} was joined",
                self.kind,
                ids.len(),
                self.path(first).rsplit(" › ").next().unwrap_or(key),
                if parent == ROOT { "the root".to_string() } else { format!("'{}'", self.path(parent)) },
                list.join(", ")
            );
            self.summary.ambiguous.push(text);
        }
        Some(first)
    }

    fn page_id(&mut self) -> i64 {
        self.next_page += 1;
        self.next_page - 1
    }

    /// The "Imported from <hotel>" folder: the earlier merge's, else a new one at the end of the root.
    fn folder(&mut self, stats: &mut ManagerStats) -> i64 {
        if let Some(id) = self.old_folder.or(self.new_folder) {
            return id;
        }
        let id = self.page_id();
        let a = &mut stats.adjusted_values;
        let t = &mut stats.truncated_values;
        let caption = manager_str(&fit(&self.folder_caption, 128, t), a);
        let save: String = self
            .folder_caption
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let columns = vec![
            ("parent_id", ROOT.to_string()),
            ("caption_save", manager_str(&fit(&save, 128, t), a)),
            ("caption", caption),
            ("page_layout", "'default_3x3'".into()),
            ("icon_color", "1".into()),
            ("icon_image", "1".into()),
            ("min_rank", "1".into()),
            ("order_num", (self.root_order + 1).to_string()),
            ("visible", "1".into()),
            ("enabled", "1".into()),
            ("club_only", "0".into()),
            ("catalog_mode", format!("'{}'", self.kind)),
            ("vip_only", "0".into()),
            ("page_headline", "''".into()),
            ("page_teaser", "''".into()),
            ("page_special", "''".into()),
            ("page_text1", "''".into()),
            ("page_text2", "''".into()),
            ("page_text_details", "''".into()),
            ("page_text_teaser", "''".into()),
            ("room_id", "0".into()),
            ("includes", "''".into()),
        ];
        self.pages.push(ManagerRow {
            page: true,
            kind: self.kind,
            id,
            columns,
            label: self.folder_caption.clone(),
            has_content: false,
        });
        self.created_paths.insert(id, self.folder_caption.clone());
        self.notes.push(format!(
            "merge: new {} page {id} '{}' (the folder of the new pages)",
            self.kind, self.folder_caption
        ));
        self.new_folder = Some(id);
        self.summary.folders += 1;
        self.summary.new_pages += 1;
        stats.pages += 1;
        id
    }

    /// Where the new sub-pages of ripped page `rip_id` go (ROOT: the rip's root).
    fn location(&mut self, rip_id: i32, stats: &mut ManagerStats) -> i64 {
        if rip_id == ROOT as i32 {
            return self.folder(stats);
        }
        let Some(placed) = self.placed.get(&rip_id).copied() else {
            return self.folder(stats);
        };
        if placed.new {
            return placed.dest;
        }
        if let Some(id) = placed.mirror.or(placed.path_copy) {
            return id;
        }
        // A joined page with new sub-pages: a page of the same name under the folder keeps their path.
        let parent = self.location(placed.row.parent_id, stats);
        let (id, created) = self.create(&placed.row, parent, stats);
        if created {
            self.notes.push(format!(
                "merge: new {} page {id} '{}' (keeps the path of ripped page {}, whose offers went to page {})",
                self.kind,
                self.path(id),
                placed.row.id,
                placed.dest
            ));
        }
        if let Some(p) = self.placed.get_mut(&rip_id) {
            p.path_copy = Some(id);
        }
        id
    }

    /// A new page for ripped page `row` under `parent`, or the one this file already creates there
    /// with the same caption (false).
    fn create(&mut self, row: &NumberedPage, parent: i64, stats: &mut ManagerStats) -> (i64, bool) {
        let label = page_label(row.node);
        let key = (parent, caption_key(label));
        if let Some(id) = self.created.get(&key) {
            return (*id, false);
        }
        let id = self.page_id();
        let columns = page_columns(row, self.kind, parent, row.order, stats);
        self.pages.push(ManagerRow {
            page: true,
            kind: self.kind,
            id,
            columns,
            label: label.to_string(),
            has_content: row.page.is_some(),
        });
        let path = format!("{} › {}", self.path(parent), label.trim());
        self.created_paths.insert(id, path);
        self.created.insert(key, id);
        self.summary.new_pages += 1;
        stats.pages += 1;
        (id, true)
    }

    fn place(&mut self, row: NumberedPage<'a>, stats: &mut ManagerStats) {
        let key = caption_key(page_label(row.node));
        let (parent_owner, parent_mirror) = match self.placed.get(&row.parent_id) {
            Some(parent) => (parent.owner, parent.mirror),
            None => (Some(ROOT), self.old_folder),
        };
        let owner = match parent_owner {
            Some(parent) => self.find(parent, &key),
            None => None,
        };
        let mirror = match parent_mirror {
            Some(parent) => self.find(parent, &key),
            None => None,
        };
        let (dest, new, note) = match owner.or(mirror) {
            Some(id) => {
                self.summary.joined_pages += 1;
                (id, false, format!("merge: joined {} page {id} '{}' ← ripped page {}", self.kind, self.path(id), row.id))
            }
            None => {
                let parent = self.location(row.parent_id, stats);
                let (id, created) = self.create(&row, parent, stats);
                let what = if created { "new" } else { "same caption, joined new" };
                (id, true, format!("merge: {what} {} page {id} '{}' ← ripped page {}", self.kind, self.path(id), row.id))
            }
        };
        self.placed.insert(
            row.id,
            Placed { row, owner, mirror, dest, new, path_copy: None },
        );
        let Some(page) = row.page else {
            self.notes.push(note);
            return;
        };
        let (mut added, mut skipped) = (0, 0);
        for offer in &page.offers {
            let literal = match resolve_offer(offer, self.export) {
                OfferIds::Literal(literal) => literal,
                OfferIds::NoProducts => {
                    stats.offers_without_products += 1;
                    continue;
                }
                OfferIds::Unresolved(products) => {
                    stats.offers_unresolved += 1;
                    self.skipped.push(format!(
                        "skipped: {} offer {} '{}' on page {}: {products} not in items_base",
                        self.kind, offer.offer_id, offer.localization_id, row.id
                    ));
                    continue;
                }
            };
            let set = offer_item_ids(&literal);
            if !new {
                let sold = self.sold.entry(dest).or_insert_with(|| {
                    self.export
                        .existing
                        .and_then(|e| e.offers_by_page.get(self.kind))
                        .and_then(|m| m.get(&dest))
                        .map(|offers| offers.iter().cloned().collect())
                        .unwrap_or_default()
                });
                if !sold.insert(set.clone()) {
                    skipped += 1;
                    self.summary.offers_skipped += 1;
                    let ids: Vec<String> = set.iter().map(i64::to_string).collect();
                    self.skipped.push(format!(
                        "skipped: {} offer {} '{}' on page {}: page {dest} already sells items_base {}",
                        self.kind,
                        offer.offer_id,
                        offer.localization_id,
                        row.id,
                        ids.join(" + ")
                    ));
                    continue;
                }
            }
            let order = self.next_order.entry(dest).or_insert(1);
            let position = *order;
            *order += 1;
            let id = self.next_offer;
            self.next_offer += 1;
            // No client offer id: it could be one the hotel already uses.
            let columns = offer_columns(offer, &literal, dest, position, -1, stats);
            self.offers.push(ManagerRow {
                page: false,
                kind: self.kind,
                id,
                columns,
                label: offer.localization_id.clone(),
                has_content: true,
            });
            added += 1;
            self.summary.offers_added += 1;
            stats.offers += 1;
        }
        self.notes.push(format!("{note}: {added} offers added, {skipped} already sold"));
    }
}

/// [`manager_rows`] for [`ManagerMode::Merge`]: only INSERTs of new pages and offers.
fn merge_rows(
    catalog: &RippedCatalog,
    export: &ManagerExport,
    existing: &ExistingCatalogue,
    mut stats: ManagerStats,
) -> (Vec<ManagerRow>, ManagerStats, Vec<String>) {
    let hotel = if catalog.hotel.trim().is_empty() { &catalog.host } else { &catalog.hotel };
    let mut summary = MergeSummary::default();
    let (mut pages, mut offers, mut notes) = (Vec::new(), Vec::new(), Vec::new());
    // Both catalogues in one rip share nothing: each is merged into the hotel's of its type.
    for ripped in &catalog.modes {
        let kind = catalog_type(&ripped.mode);
        let mut merger = Merger::new(kind, hotel, existing, export);
        let mut next = first_free_page_id([ripped]);
        let numbered = numbered_pages(ripped, &mut next, &mut stats.synthetic_page_ids);
        let mut seen = HashSet::new();
        for row in numbered.into_iter().filter(|r| seen.insert(r.id)) {
            merger.place(row, &mut stats);
        }
        pages.append(&mut merger.pages);
        offers.append(&mut merger.offers);
        notes.extend(merger.summary.ambiguous.iter().map(|a| format!("merge: ambiguous: {a}")));
        notes.append(&mut merger.notes);
        notes.append(&mut merger.skipped);
        let s = merger.summary;
        summary.joined_pages += s.joined_pages;
        summary.new_pages += s.new_pages;
        summary.folders += s.folders;
        summary.offers_added += s.offers_added;
        summary.offers_skipped += s.offers_skipped;
        summary.ambiguous.extend(s.ambiguous);
    }
    stats.merge = Some(summary);
    pages.extend(offers);
    (pages, stats, notes)
}

/// The whole rip as one file for the Manager's catalogue import.
pub fn manager_sql_with(catalog: &RippedCatalog, export: &ManagerExport) -> ManagerSql {
    let (rows, mut stats, notes) = manager_rows(catalog, export);
    let mode = stats.mode;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "-- Catalog Studio restricted SQL export.\n-- Catalogue ripped from {} ({}, {} client {}) on {} by All-in-1 Manager {}.\n-- Import it in the All-in-1 Manager's catalogue page ({}); the import shows every change before applying it.",
        comment(&catalog.hotel),
        comment(&catalog.host),
        comment(&catalog.client_type),
        comment(&catalog.hotel_version),
        comment(&catalog.ripped_at),
        env!("CARGO_PKG_VERSION"),
        mode.label(),
    );
    if !stats.note.is_empty() {
        let _ = writeln!(text, "-- {}", comment(&stats.note));
    }
    match mode {
        ManagerMode::Replace => {
            text.push_str("DELETE FROM catalog_items;\nDELETE FROM catalog_pages;\n")
        }
        ManagerMode::ReplaceRipped => {
            let existing = export.existing.cloned().unwrap_or_default();
            let kinds: BTreeSet<&str> =
                catalog.modes.iter().map(|m| catalog_type(&m.mode)).collect();
            for kind in &kinds {
                for id in existing.offers.get(*kind).into_iter().flatten() {
                    let _ = writeln!(
                        text,
                        "DELETE FROM catalog_items WHERE id = {id} AND catalog_type = '{kind}';"
                    );
                    stats.deleted_offers += 1;
                }
            }
            for kind in &kinds {
                for id in existing.pages.get(*kind).into_iter().flatten() {
                    let _ = writeln!(
                        text,
                        "DELETE FROM catalog_pages WHERE id = {id} AND catalog_type = '{kind}';"
                    );
                    stats.deleted_pages += 1;
                }
            }
        }
        ManagerMode::Add | ManagerMode::Merge => {}
    }
    if let Some(merge) = &stats.merge {
        let _ = writeln!(
            text,
            "-- Merge: {} ripped pages joined to the hotel's, {} new pages, {} offers added, {} offers skipped (their furni is already sold on that page); the hotel's own pages and offers are not changed.",
            merge.joined_pages, merge.new_pages, merge.offers_added, merge.offers_skipped
        );
    }
    for row in &rows {
        text.push_str(&row.insert());
        text.push('\n');
    }
    for note in notes {
        let _ = writeln!(text, "-- {}", comment(&note));
    }
    ManagerSql { text, stats }
}

/// The README paragraph about catalog_manager.sql.
pub fn readme_section(stats: &ManagerStats, with_items_base: bool) -> String {
    let what = match stats.mode {
        ManagerMode::Replace => "It starts with DELETE FROM catalog_items / catalog_pages, so it REPLACES the whole\n  catalogue (normal and Builders Club).",
        ManagerMode::ReplaceRipped => "It deletes the hotel's pages and offers of the ripped catalogues first, so those are\n  REPLACED; the other catalogue stays as it is.",
        ManagerMode::Add => "It deletes nothing: the ripped pages and offers are ADDED next to the existing\n  catalogue, with ids above the hotel's highest.",
        ManagerMode::Merge => "It deletes and changes nothing: the rip is MERGED into the existing catalogue. A ripped\n  page with the same path (captions from the root) as one of the hotel's is joined to it: only\n  offers are added there, and an offer whose furni that page already sells is skipped. The other\n  pages go under a page \"Imported from <hotel>\". New ids are above the hotel's highest.",
    };
    let mut text = format!(
        "\nALL-IN-1 MANAGER IMPORT (catalog_manager.sql)\n\
         \x20 The same catalogue in the Manager's import format (Catalog Studio restricted SQL, with a\n\
         \x20 catalog_type column: NORMAL / BUILDER). Import it in the Manager's catalogue page.\n\
         \x20 {what}\n\
         \x20 The Manager shows every change before it applies anything.\n\
         \x20 That format allows no subqueries: item_ids are the hotel's items_base ids, so offers whose\n\
         \x20 products items_base lacks are left out (listed at the end of the file as comments).\n"
    );
    if !stats.note.is_empty() {
        let _ = writeln!(text, "  Note: {}", stats.note);
    }
    let _ = writeln!(text, "  pages:                         {}", stats.pages);
    let _ = writeln!(text, "  offers:                        {}", stats.offers);
    if let Some(merge) = &stats.merge {
        let _ = writeln!(text, "  pages joined to the hotel's:   {}", merge.joined_pages);
        let _ = writeln!(
            text,
            "  offers skipped (already sold): {} (listed at the end of the file)",
            merge.offers_skipped
        );
        for ambiguous in &merge.ambiguous {
            let _ = writeln!(text, "  Same caption twice: {ambiguous}");
        }
    }
    if stats.deleted_pages + stats.deleted_offers > 0 {
        let _ = writeln!(
            text,
            "  deleted first:                 {} pages, {} offers",
            stats.deleted_pages, stats.deleted_offers
        );
    }
    if stats.offers_unresolved > 0 {
        let _ = writeln!(
            text,
            "  offers left out:               {}{}",
            stats.offers_unresolved,
            if with_items_base {
                " (products not in items_base)"
            } else {
                " (no items_base was available while ripping)"
            }
        );
    }
    if stats.renumbered_offers > 0 {
        let _ = writeln!(
            text,
            "  offers on more than one page:  {} (new ids)",
            stats.renumbered_offers
        );
    }
    if stats.adjusted_values > 0 {
        let _ = writeln!(
            text,
            "  values with an added '\\':      {}",
            stats.adjusted_values
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        let mut n = 0;
        assert_eq!(manager_str("Shop's", &mut n), "'Shop''s'");
        assert_eq!(manager_str("a\\b", &mut n), "'a\\b'");
        assert_eq!(manager_str("a\\\\'b", &mut n), "'a\\\\''b'");
        assert_eq!(n, 0);
        assert_eq!(manager_str("end\\", &mut n), "'end\\\\'");
        assert_eq!(manager_str("x\\'y", &mut n), "'x\\\\''y'");
        assert_eq!(n, 2);
        assert_eq!(catalog_type("BUILDERS_CLUB"), "BUILDER");
    }

    #[test]
    fn existing_maxima() {
        let mut e = ExistingCatalogue::default();
        assert_eq!(e.max_page("NORMAL"), 0);
        e.pages.insert("NORMAL".into(), [3, 90, 7].into_iter().collect());
        e.offers.insert("BUILDER".into(), [5].into_iter().collect());
        assert_eq!((e.max_page("NORMAL"), e.max_offer("BUILDER"), e.max_offer("NORMAL")), (90, 5, 0));
    }

    fn page(id: i64, parent_id: i64, caption: &str, kind: &str) -> ExistingPage {
        ExistingPage { id, parent_id, caption: caption.into(), catalog_type: kind.into(), order_num: id }
    }

    fn offer(id: i64, page_id: i64, kind: &str, item_ids: &str) -> ExistingOffer {
        ExistingOffer { id, page_id, catalog_type: kind.into(), item_ids: item_ids.into() }
    }

    #[test]
    fn page_paths_are_built_per_catalog_type() {
        let existing = ExistingCatalogue::from_rows(
            vec![
                page(1, -1, " Furniture ", "NORMAL"),
                page(2, 1, "ICED", "NORMAL"),
                page(3, 0, "Root page", "NORMAL"),
                page(9, 77, "Orphan", "NORMAL"),
                // The same ids in the Builders Club warehouse are other pages.
                page(1, -1, "Blocks", "BUILDER"),
                page(2, 1, "Walls", "BUILDERS_CLUB"),
                page(5, 6, "Loop a", "BUILDER"),
                page(6, 5, "Loop b", "BUILDER"),
            ],
            vec![
                offer(10, 2, "NORMAL", "300:2;301;300"),
                offer(11, 2, "NORMAL", "302"),
                offer(10, 2, "BUILDER", "400"),
            ],
        );
        assert!(existing.has_tree());
        assert_eq!(existing.page_path("NORMAL", 2).unwrap(), vec!["furniture", "iced"]);
        assert_eq!(existing.page_path("NORMAL", 3).unwrap(), vec!["root page"]);
        assert_eq!(existing.page_path("BUILDER", 2).unwrap(), vec!["blocks", "walls"]);
        assert_eq!(existing.page_path("NORMAL", 9), None, "a missing parent");
        assert_eq!(existing.page_path("BUILDER", 5), None, "a loop");
        assert_eq!(existing.page_path("BUILDER", 3), None);
        assert_eq!(existing.max_page("NORMAL"), 9);
        assert_eq!(existing.max_page("BUILDER"), 6);
        assert_eq!(existing.max_offer("NORMAL"), 11);
        assert_eq!(existing.offers_by_page["NORMAL"][&2], vec![vec![300, 301], vec![302]]);
        assert_eq!(existing.offers_by_page["BUILDER"][&2], vec![vec![400]]);
        assert_eq!(offer_item_ids(" 12 : 1 ; x;0;12,7"), vec![7, 12]);

        // Only ids (an older source): no tree.
        let mut ids = ExistingCatalogue::default();
        ids.pages.insert("NORMAL".into(), [1].into_iter().collect());
        assert!(!ids.has_tree());
        assert!(ExistingCatalogue::default().has_tree());
    }
}

#[cfg(test)]
mod merge_tests {
    use std::time::Instant;

    use aio_remote::catalog_journal::Entity;
    use aio_remote::catalog_transfer;
    use serde_json::{json, Value};

    use super::*;
    use crate::export::RippedMode;
    use crate::packets::{CatalogIndex, CatalogPage, Product};

    const HOTEL: &str = "Habbo.nl";

    /// A floor furni with class id `class`: items_base id `class + 1000`.
    fn rip_offer(id: i32, classes: &[i32]) -> Offer {
        Offer {
            offer_id: id,
            localization_id: format!("offer_{id}"),
            rent: false,
            price_credits: 3,
            price_activity_points: 0,
            activity_point_type: 0,
            extra_int: None,
            giftable: true,
            products: classes
                .iter()
                .map(|c| Product {
                    product_type: "s".into(),
                    class_id: Some(*c),
                    extra_param: String::new(),
                    count: if classes.len() > 1 { 2 } else { 1 },
                    limited: false,
                    limited_series_size: 0,
                    limited_items_left: 0,
                })
                .collect(),
            club_level: 0,
            bundle_purchase_allowed: true,
            is_pet: false,
            preview_image: String::new(),
            item_ids: None,
            have_offer: None,
        }
    }

    fn node(page_id: i32, caption: &str, children: Vec<IndexNode>) -> IndexNode {
        IndexNode {
            visible: true,
            icon: 7,
            page_id,
            parent_id: -1,
            name: caption.trim().to_lowercase().replace(' ', "_"),
            localization: caption.into(),
            offer_ids: Vec::new(),
            children,
        }
    }

    /// A ripped catalogue: its tree and the offers of its pages.
    fn mode(name: &str, tree: Vec<IndexNode>, offers: Vec<(i32, Vec<Offer>)>) -> RippedMode {
        let pages = offers
            .into_iter()
            .map(|(id, offers)| {
                (
                    id,
                    CatalogPage {
                        page_id: id,
                        mode: name.into(),
                        layout: "default_3x3".into(),
                        images: Vec::new(),
                        texts: Vec::new(),
                        offers,
                        offer_id: -1,
                        accept_season_currency_as_credits: None,
                        frontpage_items: Vec::new(),
                        packet_layout: "classic".into(),
                    },
                )
            })
            .collect();
        RippedMode {
            mode: name.into(),
            index: CatalogIndex {
                root: node(-1, "root", tree),
                new_additions_available: false,
                mode: name.into(),
                has_parent_ids: false,
                tail: "nothing".into(),
            },
            pages,
            failed: Vec::new(),
        }
    }

    fn rip(modes: Vec<RippedMode>) -> RippedCatalog {
        RippedCatalog {
            hotel: HOTEL.into(),
            host: "game-nl.habbo.com".into(),
            hotel_version: "WIN63".into(),
            client_type: "FLASH".into(),
            ripped_at: "2026-11-27".into(),
            complete: true,
            modes,
        }
    }

    fn items() -> ItemsBase {
        let mut items = ItemsBase::default();
        for class in 1..40_000 {
            items.add(class as u32 + 1000, class, "s", &format!("furni_{class}"), 0);
        }
        items
    }

    /// The hotel: its catalogue for the merge and as the import's snapshots.
    struct Hotel {
        existing: ExistingCatalogue,
        current: BTreeMap<Entity, Value>,
    }

    fn hotel(pages: &[(i64, i64, &str, &str)], offers: &[(i64, i64, &str, &str)]) -> Hotel {
        let mut current = BTreeMap::new();
        let mut page_rows = Vec::new();
        let mut offer_rows = Vec::new();
        for (order, (id, parent, caption, kind)) in pages.iter().enumerate() {
            current.insert(
                Entity { builders: *kind == "BUILDER", page: true, id: *id },
                json!({"catalogType": kind, "pageId": id, "parentId": parent, "captionSave": "mine", "caption": caption,
                       "pageLayout": "default_3x3", "iconColor": 3, "iconImage": 99, "minRank": 4, "orderNum": order + 1,
                       "visible": false, "enabled": true, "clubOnly": true, "catalogMode": kind, "vipOnly": false,
                       "pageHeadline": "my headline", "roomId": 0, "includes": ""}),
            );
            page_rows.push(ExistingPage {
                id: *id,
                parent_id: *parent,
                caption: caption.to_string(),
                catalog_type: kind.to_string(),
                order_num: order as i64 + 1,
            });
        }
        for (id, page, kind, item_ids) in offers {
            current.insert(
                Entity { builders: *kind == "BUILDER", page: false, id: *id },
                json!({"catalogType": kind, "offerId": id, "itemIds": item_ids, "pageId": page, "catalogName": "mine",
                       "costCredits": 1, "amount": 1, "orderNumber": 1, "offerIdClient": id}),
            );
            offer_rows.push(ExistingOffer {
                id: *id,
                page_id: *page,
                catalog_type: kind.to_string(),
                item_ids: item_ids.to_string(),
            });
        }
        Hotel { existing: ExistingCatalogue::from_rows(page_rows, offer_rows), current }
    }

    /// The hotel after importing `text`.
    fn imported(hotel: &Hotel, text: &str) -> Hotel {
        let current = catalog_transfer::target(&hotel.current, text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let (mut pages, mut offers) = (Vec::new(), Vec::new());
        for (entity, value) in &current {
            let kind = if entity.builders { "BUILDER" } else { "NORMAL" };
            if entity.page {
                pages.push(ExistingPage {
                    id: entity.id,
                    parent_id: value["parentId"].as_i64().unwrap(),
                    caption: value["caption"].as_str().unwrap().into(),
                    catalog_type: kind.into(),
                    order_num: value["orderNum"].as_i64().unwrap(),
                });
            } else {
                offers.push(ExistingOffer {
                    id: entity.id,
                    page_id: value["pageId"].as_i64().unwrap(),
                    catalog_type: kind.into(),
                    item_ids: value["itemIds"].as_str().unwrap().into(),
                });
            }
        }
        Hotel { existing: ExistingCatalogue::from_rows(pages, offers), current }
    }

    fn merge_with(catalog: &RippedCatalog, hotel: &Hotel, items: &ItemsBase) -> ManagerSql {
        manager_sql_with(
            catalog,
            &ManagerExport {
                items: Some(items),
                furnidata: None,
                mode: ManagerMode::Merge,
                existing: Some(&hotel.existing),
            },
        )
    }

    fn merge(catalog: &RippedCatalog, hotel: &Hotel) -> ManagerSql {
        merge_with(catalog, hotel, &items())
    }

    fn summary(file: &ManagerSql) -> &MergeSummary {
        file.stats.merge.as_ref().expect("a merge")
    }

    /// The pages and offers the file creates, with the import's snapshots; nothing else changes.
    fn created(hotel: &Hotel, text: &str) -> BTreeMap<Entity, Value> {
        let target = catalog_transfer::target(&hotel.current, text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let changes = catalog_transfer::changes(&hotel.current, &target);
        assert!(changes.iter().all(|c| c.operation == "CREATE"), "{changes:?}");
        for (entity, value) in &hotel.current {
            assert_eq!(&target[entity], value, "the hotel's {entity:?} is unchanged");
        }
        target.into_iter().filter(|(e, _)| !hotel.current.contains_key(e)).collect()
    }

    fn page_of<'v>(created: &'v BTreeMap<Entity, Value>, caption: &str) -> (i64, &'v Value) {
        let found: Vec<_> = created.iter().filter(|(e, v)| e.page && v["caption"] == caption).collect();
        assert_eq!(found.len(), 1, "one page '{caption}': {created:#?}");
        (found[0].0.id, found[0].1)
    }

    fn offers_on(created: &BTreeMap<Entity, Value>, builders: bool, page: i64) -> Vec<String> {
        created
            .iter()
            .filter(|(e, v)| !e.page && e.builders == builders && v["pageId"] == page)
            .map(|(_, v)| v["itemIds"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn same_path_joins_page_keeps_owner_settings() {
        let hotel = hotel(
            &[(1, -1, "Furniture", "NORMAL"), (2, 1, "Iced", "NORMAL")],
            &[(40, 2, "NORMAL", "1001")],
        );
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![node(10, " FURNITURE ", vec![node(11, "iced", vec![])])],
            vec![(11, vec![rip_offer(500, &[1]), rip_offer(501, &[2])])],
        )]);
        let file = merge(&catalog, &hotel);
        let text = &file.text;
        assert!(!text.contains("UPDATE") && !text.contains("DELETE"), "{text}");
        assert_eq!(file.stats.mode, ManagerMode::Merge);
        let s = summary(&file);
        assert_eq!((s.joined_pages, s.new_pages, s.folders, s.offers_added, s.offers_skipped), (2, 0, 0, 1, 1));
        assert_eq!(file.stats.pages, 0);
        let created = created(&hotel, text);
        assert_eq!(created.len(), 1, "only the offer: {created:#?}");
        let (entity, offer) = created.iter().next().unwrap();
        assert_eq!(entity.id, 41, "above the hotel's highest offer");
        assert_eq!(offer["pageId"], 2);
        assert_eq!(offer["itemIds"], "1002");
        assert_eq!(offer["offerIdClient"], -1);
        assert_eq!(offer["orderNumber"], 2, "after the page's own offer");
        assert!(text.contains("-- merge: joined NORMAL page 2 'Furniture › Iced' ← ripped page 11: 1 offers added, 1 already sold"), "{text}");
        assert!(text.contains("-- merge: joined NORMAL page 1 'Furniture' ← ripped page 10\n"), "{text}");
    }

    #[test]
    fn same_caption_other_parent_not_joined() {
        let hotel = hotel(
            &[
                (1, -1, "A", "NORMAL"),
                (2, 1, "X", "NORMAL"),
                (3, -1, "B", "NORMAL"),
                (5, -1, "Rares", "NORMAL"),
                (6, -1, " rares", "NORMAL"),
            ],
            &[],
        );
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![node(10, "B", vec![node(11, "X", vec![])]), node(12, "RARES", vec![])],
            vec![(11, vec![rip_offer(500, &[1])]), (12, vec![rip_offer(501, &[2])])],
        )]);
        let file = merge(&catalog, &hotel);
        let created = created(&hotel, &file.text);
        let (folder, folder_page) = page_of(&created, "Imported from Habbo.nl");
        let (b_copy, b) = page_of(&created, "B");
        let (x, x_page) = page_of(&created, "X");
        assert_eq!(folder_page["parentId"], -1);
        assert_eq!(b["parentId"], folder, "B keeps the path of X");
        assert_eq!(x_page["parentId"], b_copy);
        assert_ne!(x, 2, "X under A is another page");
        assert_eq!(offers_on(&created, false, x), vec!["1001"]);
        assert!(offers_on(&created, false, b_copy).is_empty(), "B's offers go to the hotel's B");
        // Rares twice at the root: the first is joined, and that is reported.
        assert_eq!(offers_on(&created, false, 5), vec!["1002"]);
        let s = summary(&file);
        assert_eq!(s.ambiguous.len(), 1, "{:?}", s.ambiguous);
        assert!(s.ambiguous[0].contains("2 pages 'Rares' under the root (ids 5, 6): page 5 was joined"), "{:?}", s.ambiguous);
        assert!(file.text.contains("-- merge: ambiguous: NORMAL catalogue: 2 pages"), "{}", file.text);
        assert_eq!((s.joined_pages, s.new_pages, s.folders), (2, 3, 1));
    }

    #[test]
    fn new_pages_under_imported_folder() {
        let hotel = hotel(
            &[(1, -1, "Furniture", "NORMAL"), (9, -1, "Pets", "NORMAL"), (4, 1, "Iced", "NORMAL")],
            &[(70, 4, "NORMAL", "1001")],
        );
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![
                node(10, "Furniture", vec![]),
                node(20, "Deals", vec![node(21, "Dogs", vec![]), node(22, "Cats", vec![])]),
                node(30, "Rares", vec![]),
            ],
            vec![
                (21, vec![rip_offer(600, &[1])]),
                (22, vec![rip_offer(601, &[2]), rip_offer(602, &[3])]),
                (30, vec![rip_offer(603, &[1])]),
            ],
        )]);
        let file = merge(&catalog, &hotel);
        let created = created(&hotel, &file.text);
        let (folder, f) = page_of(&created, "Imported from Habbo.nl");
        assert_eq!(folder, 10, "above the hotel's highest page (9)");
        assert_eq!((f["parentId"].clone(), f["orderNum"].clone()), (json!(-1), json!(3)), "at the end of the root");
        assert_eq!((f["visible"].clone(), f["enabled"].clone()), (json!(true), json!(true)));
        let (deals, d) = page_of(&created, "Deals");
        let (dogs, dog) = page_of(&created, "Dogs");
        let (cats, cat) = page_of(&created, "Cats");
        let (rares, r) = page_of(&created, "Rares");
        assert_eq!((d["parentId"].clone(), d["orderNum"].clone()), (json!(folder), json!(2)));
        assert_eq!((dog["parentId"].clone(), cat["parentId"].clone()), (json!(deals), json!(deals)));
        assert_eq!((dog["orderNum"].clone(), cat["orderNum"].clone()), (json!(1), json!(2)));
        assert_eq!(r["parentId"], folder);
        assert_eq!(dog["iconImage"], 7, "the rip's page settings");
        assert_eq!(offers_on(&created, false, dogs), vec!["1001"], "not sold on that page");
        assert_eq!(offers_on(&created, false, cats).len(), 2);
        assert_eq!(offers_on(&created, false, rares), vec!["1001"]);
        assert_eq!(created.keys().filter(|e| !e.page).map(|e| e.id).min(), Some(71));
        assert_eq!(created.keys().filter(|e| e.page).count(), 5);
        assert_eq!(file.stats.pages, 5);
        assert_eq!(file.stats.offers, 4);
    }

    #[test]
    fn offers_already_sold_skipped() {
        let hotel = hotel(&[(1, -1, "Shop", "NORMAL")], &[(5, 1, "NORMAL", "1001"), (6, 1, "NORMAL", "1005:3")]);
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![node(10, "Shop", vec![])],
            vec![(
                10,
                vec![
                    rip_offer(500, &[1]),
                    rip_offer(501, &[2]),
                    rip_offer(502, &[2]),
                    rip_offer(503, &[5]),
                    rip_offer(504, &[39_999]),
                    Offer { products: Vec::new(), ..rip_offer(505, &[]) },
                ],
            )],
        )]);
        let mut few = ItemsBase::default();
        for class in [1, 2, 5] {
            few.add(class as u32 + 1000, class, "s", "x", 0);
        }
        let file = merge_with(&catalog, &hotel, &few);
        let created = created(&hotel, &file.text);
        assert_eq!(offers_on(&created, false, 1), vec!["1002"]);
        let s = summary(&file);
        assert_eq!((s.offers_added, s.offers_skipped), (1, 3));
        assert_eq!((file.stats.offers_unresolved, file.stats.offers_without_products), (1, 1));
        let text = &file.text;
        assert!(text.contains("-- skipped: NORMAL offer 500 'offer_500' on page 10: page 1 already sells items_base 1001"), "{text}");
        assert!(text.contains("-- skipped: NORMAL offer 502 'offer_502' on page 10: page 1 already sells items_base 1002"), "the same furni twice in the rip");
        assert!(text.contains("-- skipped: NORMAL offer 504 'offer_504' on page 10: s:39999 not in items_base"), "{text}");
    }

    #[test]
    fn bundles_need_same_set() {
        let hotel = hotel(
            &[(1, -1, "Bundles", "NORMAL")],
            &[(5, 1, "NORMAL", "1001:1;1002:4"), (6, 1, "NORMAL", "1003")],
        );
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![node(10, "Bundles", vec![])],
            vec![(
                10,
                vec![
                    rip_offer(500, &[2, 1]),    // the same set (other counts): sold
                    rip_offer(501, &[1, 3]),    // another set
                    rip_offer(502, &[1]),       // sold only in a bundle
                    rip_offer(503, &[1, 2, 3]), // more
                    rip_offer(504, &[3]),       // sold
                ],
            )],
        )]);
        let file = merge(&catalog, &hotel);
        let created = created(&hotel, &file.text);
        assert_eq!(offers_on(&created, false, 1), vec!["1001:2;1003:2", "1001", "1001:2;1002:2;1003:2"]);
        assert_eq!(summary(&file).offers_skipped, 2);
    }

    #[test]
    fn merge_twice_adds_nothing() {
        let hotel = hotel(
            &[(1, -1, "Furniture", "NORMAL"), (2, 1, "Iced", "NORMAL"), (1, -1, "Blocks", "BUILDER")],
            &[(40, 2, "NORMAL", "1001")],
        );
        let catalog = rip(vec![
            mode(
                "NORMAL",
                vec![
                    node(10, "Furniture", vec![node(11, "Iced", vec![]), node(12, "Mode", vec![node(13, "Deep", vec![])])]),
                    node(20, "Rares", vec![node(-1, "Hidden", vec![])]),
                    node(30, "Rares", vec![]),
                ],
                vec![
                    (11, vec![rip_offer(500, &[1]), rip_offer(501, &[2])]),
                    (12, vec![rip_offer(502, &[3])]),
                    (13, vec![rip_offer(503, &[4]), rip_offer(504, &[4])]),
                    (20, vec![rip_offer(505, &[5, 6])]),
                    (30, vec![rip_offer(506, &[7])]),
                ],
            ),
            mode("BUILDERS_CLUB", vec![node(5, "Blocks", vec![]), node(6, "Walls", vec![])], vec![(5, vec![rip_offer(507, &[8])]), (6, vec![rip_offer(508, &[9])])]),
        ]);
        let first = merge(&catalog, &hotel);
        assert_eq!(summary(&first).folders, 2, "one per catalogue");
        assert!(first.stats.offers > 0);
        let after = imported(&hotel, &first.text);
        let second = merge(&catalog, &after);
        let s = summary(&second);
        assert_eq!((second.stats.pages, second.stats.offers, s.new_pages, s.folders, s.offers_added), (0, 0, 0, 0, 0), "{}", second.text);
        assert!(!second.text.contains("INSERT"), "{}", second.text);
        assert_eq!(s.offers_skipped, first.stats.offers + summary(&first).offers_skipped);

        // A rip that is all in the hotel already: no folder either.
        let same = rip(vec![mode("NORMAL", vec![node(10, "furniture", vec![node(11, "ICED", vec![])])], vec![(11, vec![rip_offer(1, &[1])])])]);
        let file = merge(&same, &hotel);
        assert!(!file.text.contains("INSERT"), "{}", file.text);
        assert_eq!(summary(&file).folders, 0);
    }

    #[test]
    fn bc_and_normal_kept_apart() {
        let hotel = hotel(
            &[(1, -1, "Furniture", "NORMAL"), (20, -1, "Big", "NORMAL"), (1, -1, "Blocks", "BUILDER")],
            &[(50, 1, "NORMAL", "1001"), (7, 1, "BUILDER", "1002")],
        );
        let catalog = rip(vec![
            mode("NORMAL", vec![node(10, "Furniture", vec![]), node(11, "Blocks", vec![])], vec![(10, vec![rip_offer(1, &[2])]), (11, vec![rip_offer(2, &[2])])]),
            mode("BUILDERS_CLUB", vec![node(10, "Blocks", vec![]), node(11, "Furniture", vec![])], vec![(10, vec![rip_offer(3, &[1])]), (11, vec![rip_offer(4, &[1])])]),
        ]);
        let file = merge(&catalog, &hotel);
        let created = created(&hotel, &file.text);
        // NORMAL: Furniture joined (1002 is sold there only in the warehouse), Blocks new; ids above NORMAL's.
        assert_eq!(offers_on(&created, false, 1), vec!["1002"]);
        let normal_pages: Vec<i64> = created.keys().filter(|e| e.page && !e.builders).map(|e| e.id).collect();
        assert_eq!(normal_pages, vec![21, 22], "folder and Blocks above page 20");
        assert_eq!(created[&Entity { builders: false, page: true, id: 22 }]["caption"], "Blocks");
        // BUILDER: Blocks joined, Furniture new; ids above the warehouse's.
        assert_eq!(offers_on(&created, true, 1), vec!["1001"]);
        let bc_pages: Vec<i64> = created.keys().filter(|e| e.page && e.builders).map(|e| e.id).collect();
        assert_eq!(bc_pages, vec![2, 3]);
        assert_eq!(created[&Entity { builders: true, page: true, id: 3 }]["caption"], "Furniture");
        assert_eq!(created[&Entity { builders: true, page: true, id: 2 }]["catalogMode"], "BUILDER");
        let bc_offers: Vec<i64> = created.keys().filter(|e| !e.page && e.builders).map(|e| e.id).collect();
        assert_eq!(bc_offers, vec![8, 9]);
        let normal_offers: Vec<i64> = created.keys().filter(|e| !e.page && !e.builders).map(|e| e.id).collect();
        assert_eq!(normal_offers, vec![51, 52]);
    }

    #[test]
    fn file_parses_with_catalog_transfer() {
        let hotel = hotel(
            &[(1, -1, "Furniture", "NORMAL"), (2, 1, "Iced", "NORMAL"), (3, -1, "Rares", "NORMAL")],
            &[(40, 2, "NORMAL", "1001"), (41, 3, "NORMAL", "1003;1004")],
        );
        let catalog = rip(vec![mode(
            "NORMAL",
            vec![
                node(10, "Furniture", vec![node(11, "Iced", vec![]), node(12, "Plastic", vec![])]),
                node(13, "It's \"new\"", vec![node(14, "Sub \\ x", vec![])]),
            ],
            vec![
                (11, vec![rip_offer(1, &[1]), rip_offer(2, &[2])]),
                (12, vec![rip_offer(3, &[3])]),
                (13, vec![rip_offer(4, &[4])]),
                (14, vec![rip_offer(5, &[5, 6])]),
            ],
        )]);
        let file = merge(&catalog, &hotel);
        let target = catalog_transfer::target(&hotel.current, &file.text).unwrap_or_else(|e| panic!("{e}\n{}", file.text));
        let changes = catalog_transfer::changes(&hotel.current, &target);
        let pages = changes.iter().filter(|c| c.entity.page).count();
        let offers = changes.iter().filter(|c| !c.entity.page).count();
        // Folder, Furniture (path of Plastic), Plastic, It's "new", Sub; Iced's 1002 and three more offers.
        assert_eq!((pages, offers), (5, 4), "{}", file.text);
        assert_eq!((file.stats.pages, file.stats.offers), (pages, offers));
        assert!(changes.iter().all(|c| c.operation == "CREATE" && c.before.is_none()));
        let created = created(&hotel, &file.text);
        page_of(&created, "It's \"new\"");
        page_of(&created, "Sub \\ x");
    }

    #[test]
    fn without_the_page_tree_it_adds() {
        let mut ids = ExistingCatalogue::default();
        ids.pages.insert("NORMAL".into(), [1, 30].into_iter().collect());
        let catalog = rip(vec![mode("NORMAL", vec![node(10, "Shop", vec![])], vec![])]);
        let export = ManagerExport { mode: ManagerMode::Merge, existing: Some(&ids), ..ManagerExport::default() };
        let file = manager_sql_with(&catalog, &export);
        assert_eq!(file.stats.mode, ManagerMode::Add);
        assert!(file.stats.merge.is_none());
        assert!(file.text.contains("INSERT INTO catalog_pages (id, catalog_type, parent_id, caption_save, caption, page_layout, icon_color, icon_image, min_rank, order_num, visible, enabled, club_only, catalog_mode, vip_only, page_headline, page_teaser, page_special, page_text1, page_text2, page_text_details, page_text_teaser, room_id, includes) VALUES (40, 'NORMAL'"), "{}", file.text);
        let none = ManagerExport { mode: ManagerMode::Merge, ..ManagerExport::default() };
        assert_eq!(manager_sql_with(&catalog, &none).stats.mode, ManagerMode::Add);
    }

    /// Two Habbo-size catalogues (≈3,500 pages, ≈30,000 offers each): the merge takes seconds at most.
    #[test]
    fn habbo_size_merge_is_fast() {
        // The rip: 50 categories of 70 pages of 9 offers. The hotel: the same categories; in the even
        // ones the same pages selling half of that furni and 12 more, in the odd ones other pages.
        let mut tree = Vec::new();
        let mut pages = Vec::new();
        let mut hotel_pages = Vec::new();
        let mut hotel_offers = Vec::new();
        let mut class = 0;
        for category in 0..50 {
            let category_id = 10_000 + category;
            let mut children = Vec::new();
            hotel_pages.push((category_id as i64, -1, format!("Category {category}")));
            for page in 0..70 {
                let id = category * 100 + page + 1;
                children.push(node(id, &format!("Page {page}"), vec![]));
                let offers: Vec<Offer> = (0..9)
                    .map(|i| {
                        class += 1;
                        rip_offer(id * 10 + i, &[class])
                    })
                    .collect();
                if category % 2 == 0 {
                    hotel_pages.push((id as i64 + 20_000, category_id as i64, format!("PAGE {page}")));
                    for offer in offers.iter().step_by(2) {
                        let item = offer.products[0].class_id.unwrap() + 1000;
                        hotel_offers.push((offer.offer_id as i64 + 100_000, id as i64 + 20_000, item.to_string()));
                    }
                    for i in 0..12 {
                        hotel_offers.push((offer_count(&hotel_offers), id as i64 + 20_000, (900_000 + i).to_string()));
                    }
                } else {
                    hotel_pages.push((id as i64 + 20_000, category_id as i64, format!("Other {page}")));
                    for i in 0..3 {
                        hotel_offers.push((offer_count(&hotel_offers), id as i64 + 20_000, (900_000 + i).to_string()));
                    }
                }
                pages.push((id, offers));
            }
            tree.push(node(category_id, &format!("Category {category}"), children));
        }
        fn offer_count(offers: &[(i64, i64, String)]) -> i64 {
            500_000 + offers.len() as i64
        }
        let catalog = rip(vec![mode("NORMAL", tree, pages)]);
        let page_refs: Vec<(i64, i64, &str, &str)> = hotel_pages.iter().map(|(i, p, c)| (*i, *p, c.as_str(), "NORMAL")).collect();
        let offer_refs: Vec<(i64, i64, &str, &str)> = hotel_offers.iter().map(|(i, p, ids)| (*i, *p, "NORMAL", ids.as_str())).collect();
        let hotel = hotel(&page_refs, &offer_refs);
        assert_eq!(hotel.existing.page_tree.len(), 50 + 50 * 70);
        assert_eq!(hotel.existing.offers["NORMAL"].len(), 25 * 70 * (5 + 12) + 25 * 70 * 3);
        let items = items();

        let started = Instant::now();
        let file = merge_with(&catalog, &hotel, &items);
        let took = started.elapsed();
        println!("merge of 3,550 pages / 31,500 offers into {} pages / {} offers: {took:?}", hotel.existing.page_tree.len(), hotel.existing.offers["NORMAL"].len());
        assert!(took.as_secs_f64() < 5.0, "{took:?}");

        let s = summary(&file);
        assert_eq!(s.joined_pages, 50 + 25 * 70, "every category, the even ones' pages");
        assert_eq!(s.new_pages, 1 + 25 + 25 * 70);
        assert_eq!(s.offers_skipped, 25 * 70 * 5);
        assert_eq!(s.offers_added, 50 * 70 * 9 - 25 * 70 * 5);

        // The whole file through the import's tokenizer, every 97th statement through its parser (the
        // whole import, catalog_transfer::target, takes about 30 s in a debug build: it compiles its
        // regexes per statement).
        let started = Instant::now();
        let statements = catalog_transfer::statements(&file.text).unwrap();
        assert_eq!(statements.len(), s.new_pages + s.offers_added);
        for statement in statements.iter().step_by(97) {
            catalog_transfer::parse_statement(statement).unwrap_or_else(|e| panic!("{e}: {statement}"));
        }
        println!("read back by the import's tokenizer in {:?}", started.elapsed());
    }
}
