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
//! Three ways to put it in the hotel ([`ManagerMode`]):
//! * `Replace`: `DELETE FROM catalog_items; DELETE FROM catalog_pages;` first, as the Studio
//!   export does: the whole catalogue (normal and Builders Club) becomes the rip;
//! * `ReplaceRipped`: only the catalogues that were ripped are replaced (one DELETE per existing
//!   page and offer of those catalogues, so it needs the hotel's ids: [`ExistingCatalogue`]);
//! * `Add`: nothing is deleted; every ripped page and offer gets an id above the hotel's
//!   highest, so the rip lands next to the existing catalogue.
//!
//! The Manager previews every change of the file before writing anything.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::export::{
    arcturus_layout, comment, first_free_page_id, fit, item_ids_sql_with, map_page_content,
    numbered_pages, ItemsBase, RippedCatalog,
};
use crate::furnidata::Furnidata;

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
}

impl ManagerMode {
    pub fn label(self) -> &'static str {
        match self {
            ManagerMode::Replace => "replace the whole catalogue",
            ManagerMode::ReplaceRipped => "replace the ripped catalogues",
            ManagerMode::Add => "add next to the existing catalogue",
        }
    }
}

/// The ids already in the hotel, per `catalog_type` ("NORMAL" / "BUILDER").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExistingCatalogue {
    pub pages: BTreeMap<String, BTreeSet<i64>>,
    pub offers: BTreeMap<String, BTreeSet<i64>>,
}

impl ExistingCatalogue {
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
            let node = row.node;
            let layout_code = row.page.map(|p| p.layout.as_str()).unwrap_or("");
            let layout = arcturus_layout(layout_code).unwrap_or("default_3x3");
            let content = row
                .page
                .map(|p| map_page_content(&p.layout, &p.images, &p.texts))
                .unwrap_or_default();
            let a = &mut stats.adjusted_values;
            let t = &mut stats.truncated_values;
            let columns = vec![
                ("parent_id", page_id(row.parent_id).to_string()),
                ("caption_save", manager_str(&fit(&node.name, 128, t), a)),
                ("caption", manager_str(&fit(&node.localization, 128, t), a)),
                ("page_layout", format!("'{layout}'")),
                ("icon_color", "1".into()),
                ("icon_image", node.icon.to_string()),
                ("min_rank", "1".into()),
                ("order_num", row.order.to_string()),
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
            ];
            page_rows.push(ManagerRow {
                page: true,
                kind,
                id: page_id(row.id),
                columns,
                label: if node.localization.is_empty() {
                    node.name.clone()
                } else {
                    node.localization.clone()
                },
                has_content: row.page.is_some(),
            });
            stats.pages += 1;

            let Some(page) = row.page else { continue };
            for (position, offer) in page.offers.iter().enumerate() {
                if offer.products.is_empty() {
                    stats.offers_without_products += 1;
                    continue;
                }
                let ids = item_ids_sql_with(offer, export.items, export.furnidata);
                if ids.unresolved > 0 {
                    stats.offers_unresolved += 1;
                    if skipped_seen.insert(offer.offer_id) {
                        let products: Vec<String> = offer
                            .products
                            .iter()
                            .map(|p| {
                                let name = export.furnidata.and_then(|f| f.classname(p));
                                match (p.class_id, name) {
                                    (Some(c), Some(name)) => {
                                        format!("{}:{c} ({name})", p.product_type)
                                    }
                                    (Some(c), None) => format!("{}:{c}", p.product_type),
                                    (None, _) => format!("{}:{}", p.product_type, p.extra_param),
                                }
                            })
                            .collect();
                        skipped.push((
                            offer.offer_id,
                            format!(
                                "{kind} offer {} '{}' on page {}: {} not in items_base",
                                offer.offer_id,
                                offer.localization_id,
                                row.id,
                                products.join(", ")
                            ),
                        ));
                    }
                    continue;
                }
                // All literal: item_ids_sql wrote a MariaDB string; take its text back out.
                let literal = ids
                    .sql
                    .trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .to_string();
                let id = if offer.offer_id > 0 && seen_offers.insert(offer.offer_id) {
                    offer.offer_id
                } else {
                    stats.renumbered_offers += 1;
                    next_offer += 1;
                    next_offer - 1
                } as i64
                    + offer_offset;
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
                // Added offers keep no client offer id: it could be one the hotel already uses.
                let client_offer_id = if mode == ManagerMode::Add {
                    -1
                } else {
                    offer.offer_id
                };
                let a = &mut stats.adjusted_values;
                let t = &mut stats.truncated_values;
                let columns = vec![
                    ("item_ids", manager_str(&literal, a)),
                    ("page_id", page_id(row.id).to_string()),
                    ("catalog_name", manager_str(&fit(&offer.localization_id, 100, t), a)),
                    ("cost_credits", offer.price_credits.to_string()),
                    ("cost_points", offer.price_activity_points.to_string()),
                    ("points_type", offer.activity_point_type.to_string()),
                    ("amount", amount.to_string()),
                    ("limited_stack", stack.to_string()),
                    ("order_number", (position + 1).to_string()),
                    ("offer_id", client_offer_id.to_string()),
                    ("song_id", "0".into()),
                    ("extradata", manager_str(&fit(&first.extra_param, 500, t), a)),
                    ("have_offer", if offer.bundle_purchase_allowed { "1" } else { "0" }.into()),
                    ("club_only", if offer.club_level > 0 { "1" } else { "0" }.into()),
                ];
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
        ManagerMode::Add => {}
    }
    for row in &rows {
        text.push_str(&row.insert());
        text.push('\n');
    }
    for note in notes {
        let _ = writeln!(text, "-- skipped: {}", comment(&note));
    }
    ManagerSql { text, stats }
}

/// The README paragraph about catalog_manager.sql.
pub fn readme_section(stats: &ManagerStats, with_items_base: bool) -> String {
    let what = match stats.mode {
        ManagerMode::Replace => "It starts with DELETE FROM catalog_items / catalog_pages, so it REPLACES the whole\n  catalogue (normal and Builders Club).",
        ManagerMode::ReplaceRipped => "It deletes the hotel's pages and offers of the ripped catalogues first, so those are\n  REPLACED; the other catalogue stays as it is.",
        ManagerMode::Add => "It deletes nothing: the ripped pages and offers are ADDED next to the existing\n  catalogue, with ids above the hotel's highest.",
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
}
