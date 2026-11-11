//! Catalogue bundles: a page with the `single_bundle` layout and the one offer on it, whose `item_ids` lists what the
//! bundle sells. An entry names a furni (`4501`, `4501:2` for two) or, since Polaris' bundle references, a catalogue
//! offer (`@1234`, `@1234:2` for twice what it sells): the bundle then follows that offer. Entries are separated by `;`.
//! The expansion here mirrors the emulator's BundleOfferReferences, so the Manager shows what a buyer really gets.

use std::collections::HashMap;

use crate::catalog_admin::{self, Catalogue, Offer, Page};
use crate::housekeeping::{int, text};
use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

pub const LAYOUT: &str = "single_bundle";
/// How deep offers may name offers, and the most of one furni a bundle hands out (the emulator's limits).
pub const MAX_DEPTH: usize = 5;
pub const MAX_AMOUNT: i64 = 1000;

/// One entry of a bundle's `item_ids`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A furni (items_base id) and how many; None: one (or the offer's amount when it is the only entry).
    Furni { id: i64, amount: Option<i64> },
    /// What catalogue offer `id` sells, `times` over (None: once).
    Offer { id: i64, times: Option<i64> },
}

impl Entry {
    pub fn id(self) -> i64 {
        match self {
            Entry::Furni { id, .. } | Entry::Offer { id, .. } => id,
        }
    }

    /// How many, as shown (1 when not written).
    pub fn count(self) -> i64 {
        match self {
            Entry::Furni { amount, .. } => amount.unwrap_or(1),
            Entry::Offer { times, .. } => times.unwrap_or(1),
        }
    }

    pub fn with_count(self, count: i64) -> Entry {
        let count = count.clamp(1, MAX_AMOUNT);
        match self {
            Entry::Furni { id, .. } => Entry::Furni { id, amount: Some(count) },
            Entry::Offer { id, .. } => Entry::Offer { id, times: Some(count) },
        }
    }

    pub fn text(self) -> String {
        match self {
            Entry::Furni { id, amount: None } => id.to_string(),
            Entry::Furni { id, amount: Some(n) } => format!("{id}:{n}"),
            Entry::Offer { id, times: None } => format!("@{id}"),
            Entry::Offer { id, times: Some(n) } => format!("@{id}:{n}"),
        }
    }
}

/// The entries of an `item_ids`, or None when one of them is not an entry (the emulator would skip it).
pub fn parse(value: &str) -> Option<Vec<Entry>> {
    let mut list = Vec::new();
    for token in value.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        let (offer, rest) = match token.strip_prefix('@') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() > 2 {
            return None;
        }
        let id = parts[0].trim().parse::<i32>().ok().filter(|id| *id > 0)? as i64;
        let count = match parts.get(1) {
            Some(n) => Some(n.trim().parse::<i32>().ok().filter(|n| *n > 0)? as i64),
            None => None,
        };
        list.push(if offer { Entry::Offer { id, times: count } } else { Entry::Furni { id, amount: count } });
    }
    Some(list)
}

/// The `item_ids` of the entries. A furni written as its count keeps that count; a single furni entry without a count
/// gets `:1` written when there are others (the emulator reads a lone entry as the offer's amount).
pub fn write(entries: &[Entry]) -> String {
    entries.iter().map(|e| e.text()).collect::<Vec<_>>().join(";")
}

/// What an offer sells, for the expansion: its `item_ids` and `amount`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub item_ids: String,
    pub amount: i64,
}

/// What a buyer of the bundle gets: (furni id, how many) in the order they were first named, and the offers that could
/// not be used (unknown, too deep, or part of a loop). As the emulator's BundleOfferReferences.expand.
pub fn expand(item_ids: &str, amount: i64, lookup: &dyn Fn(i64) -> Option<Content>) -> (Vec<(i64, i64)>, Vec<i64>) {
    let mut furni: Vec<(i64, i64)> = Vec::new();
    let mut missing = Vec::new();
    collect(item_ids, amount, 1, &mut Vec::new(), 0, lookup, &mut furni, &mut missing);
    (furni, missing)
}

#[allow(clippy::too_many_arguments)]
fn collect(item_ids: &str, offer_amount: i64, times: i64, path: &mut Vec<i64>, depth: usize, lookup: &dyn Fn(i64) -> Option<Content>, furni: &mut Vec<(i64, i64)>, missing: &mut Vec<i64>) {
    if item_ids.trim().is_empty() {
        return;
    }
    let single = !item_ids.contains(';');
    for token in item_ids.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        let offer = token.starts_with('@');
        let parts: Vec<&str> = token.trim_start_matches('@').split(':').collect();
        let Ok(id) = parts[0].trim().parse::<i32>() else { continue };
        let count = match parts.get(1) {
            Some(n) => match n.trim().parse::<i32>() {
                Ok(n) => n as i64,
                Err(_) => continue,
            },
            None if !offer && single => offer_amount,
            None => 1,
        };
        let id = id as i64;
        if id <= 0 || count <= 0 || parts.len() > 2 {
            continue;
        }
        if !offer {
            let add = (count * times).min(MAX_AMOUNT);
            match furni.iter_mut().find(|(f, _)| *f == id) {
                Some((_, n)) => *n = (*n + add).min(MAX_AMOUNT),
                None => furni.push((id, add)),
            }
            continue;
        }
        let content = if depth >= MAX_DEPTH || path.contains(&id) { None } else { lookup(id) };
        let Some(content) = content else {
            if !missing.contains(&id) {
                missing.push(id);
            }
            continue;
        };
        path.push(id);
        collect(&content.item_ids, content.amount, (times * count).min(MAX_AMOUNT), path, depth + 1, lookup, furni, missing);
        path.pop();
    }
}

/// The page's texts and images as the bundle page shows them (catalog_pages columns).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageTexts {
    /// page_headline: the image of the page header (`c_images/catalogue/<name>.gif`).
    pub headline: String,
    /// page_teaser: the big picture on the left.
    pub teaser: String,
    /// page_text1: the header description.
    pub text1: String,
    /// page_text_details: the description on top of the page.
    pub details: String,
    /// page_text_teaser: the special text over the bundle's contents.
    pub teaser_text: String,
    /// page_text2: the text at the bottom.
    pub text2: String,
}

pub const TEXT_COLUMNS: [&str; 6] = ["page_headline", "page_teaser", "page_text1", "page_text_details", "page_text_teaser", "page_text2"];

impl PageTexts {
    pub fn values(&self) -> [&String; 6] {
        [&self.headline, &self.teaser, &self.text1, &self.details, &self.teaser_text, &self.text2]
    }

    fn set(&mut self, column: &str, value: String) {
        match column {
            "page_headline" => self.headline = value,
            "page_teaser" => self.teaser = value,
            "page_text1" => self.text1 = value,
            "page_text_details" => self.details = value,
            "page_text_teaser" => self.teaser_text = value,
            "page_text2" => self.text2 = value,
            _ => {}
        }
    }

    /// The columns that differ from `saved`, the HTML texts cleaned as the client and gameserver show them.
    pub fn changes(&self, saved: &PageTexts) -> Vec<(String, Option<String>)> {
        TEXT_COLUMNS
            .iter()
            .zip(self.values().into_iter().zip(saved.values()))
            .filter(|(_, (now, before))| now != before)
            .map(|(column, (now, _))| {
                let value = if catalog_admin::PAGE_TEXT_COLUMNS.contains(column) { catalog_admin::safe_page_html(now) } else { now.trim().to_string() };
                (column.to_string(), Some(value))
            })
            .collect()
    }
}

/// The bundle pages, in the catalogue's order.
pub fn bundle_pages(pages: &[Page]) -> Vec<&Page> {
    let mut list: Vec<&Page> = pages.iter().filter(|p| p.layout.trim().eq_ignore_ascii_case(LAYOUT)).collect();
    list.sort_by_key(|p| (p.parent_id, p.order_num, p.id));
    list
}

/// A new bundle page under `parent` (-1: top level): hidden until it is ready, with its offer still to make.
pub fn new_bundle_page(pages: &[Page], parent: i64, name: &str) -> Page {
    let caption = if name.trim().is_empty() { "New bundle".to_string() } else { name.trim().to_string() };
    let save: String = caption.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    Page {
        caption,
        caption_save: format!("bundle_{}", save.trim_matches('_')),
        layout: LAYOUT.into(),
        visible: false,
        ..catalog_admin::new_page(pages, parent, Catalogue::Normal)
    }
}

/// The bundle's offer on a new page: no contents yet, 20 credits, one of it.
pub fn new_bundle_offer(page: &Page) -> Offer {
    let name: String = page.caption_save.chars().take(catalog_admin::MAX_CATALOG_NAME).collect();
    Offer { catalog_name: if name.is_empty() { "bundle".into() } else { name }, cost_credits: 20, ..catalog_admin::new_offer(&[], page.id) }
}

/// Why the bundle cannot be saved, or None.
pub fn problem(offer: &Offer) -> Option<String> {
    let entries = parse(&offer.item_ids)?;
    if entries.is_empty() {
        return Some("The bundle has nothing in it yet: add furni or an offer.".into());
    }
    if offer.item_ids.len() > catalog_admin::MAX_ITEM_IDS {
        return Some(format!("The contents are longer than {} characters: fewer entries.", catalog_admin::MAX_ITEM_IDS));
    }
    if offer.catalog_name.trim().is_empty() {
        return Some("The bundle needs a name.".into());
    }
    if offer.cost_credits < 0 || offer.cost_points < 0 {
        return Some("Prices cannot be negative.".into());
    }
    None
}

/// The page's texts and images.
pub async fn load_texts(session: &SqlSession, database: &str, page: i64) -> Result<PageTexts> {
    let row = catalog_admin::load_row(session, database, Catalogue::Normal.pages_table(), page).await?;
    let mut texts = PageTexts::default();
    for (column, value) in row {
        texts.set(&column, value.unwrap_or_default());
    }
    Ok(texts)
}

/// Saves the changed texts and images of the page (in the catalogue history).
pub async fn save_texts(session: &SqlSession, database: &str, page: i64, texts: &PageTexts, saved: &PageTexts) -> Result<u64> {
    catalog_admin::save_row(session, database, Catalogue::Normal.pages_table(), page, &texts.changes(saved)).await
}

/// An offer to name in a bundle: its id, name, page and what it sells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfferInfo {
    pub id: i64,
    pub name: String,
    pub page: String,
    pub item_ids: String,
    pub amount: i64,
}

fn offer_from_row(row: &[SqlValue]) -> OfferInfo {
    OfferInfo { id: int(row.first()), name: text(row.get(1)), page: text(row.get(2)), item_ids: text(row.get(3)), amount: int(row.get(4)).max(1) }
}

/// Offers whose name contains the text, or the offer with that id (at most 100).
pub async fn search_offers(session: &SqlSession, database: &str, search: &str) -> Result<Vec<OfferInfo>> {
    let db = quote(database);
    let search = search.trim();
    let id = search.trim_start_matches('@').parse::<i64>().unwrap_or(-1);
    let pattern = format!("%{}%", search.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let sql = format!(
        "SELECT i.id, i.catalog_name, COALESCE(p.caption, ''), i.item_ids, i.amount FROM {db}.catalog_items i LEFT JOIN {db}.catalog_pages p ON p.id = i.page_id \
         WHERE i.id = ? OR i.catalog_name LIKE ? ORDER BY (i.id = ?) DESC, i.catalog_name LIMIT 100"
    );
    Ok(session.select(&sql, vec![SqlValue::Int(id), SqlValue::Text(pattern), SqlValue::Int(id)]).await?.iter().map(|r| offer_from_row(r)).collect())
}

/// The offers a bundle names, and the ones they name, up to [`MAX_DEPTH`] deep: what [`expand`] needs.
pub async fn referenced_offers(session: &SqlSession, database: &str, item_ids: &str) -> Result<HashMap<i64, OfferInfo>> {
    let db = quote(database);
    let mut found: HashMap<i64, OfferInfo> = HashMap::new();
    let mut wanted: Vec<i64> = offer_ids(item_ids);
    for _ in 0..MAX_DEPTH {
        wanted.retain(|id| !found.contains_key(id));
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            break;
        }
        let marks = vec!["?"; wanted.len()].join(", ");
        let sql = format!(
            "SELECT i.id, i.catalog_name, COALESCE(p.caption, ''), i.item_ids, i.amount FROM {db}.catalog_items i LEFT JOIN {db}.catalog_pages p ON p.id = i.page_id WHERE i.id IN ({marks})"
        );
        let rows = session.select(&sql, wanted.iter().map(|id| SqlValue::Int(*id)).collect()).await?;
        let mut next = Vec::new();
        for row in rows {
            let offer = offer_from_row(&row);
            next.extend(offer_ids(&offer.item_ids));
            found.insert(offer.id, offer);
        }
        // Ids asked for but not there stay out of `found`: expand reports them as missing.
        for id in &wanted {
            found.entry(*id).or_insert_with(|| OfferInfo { id: -1, ..OfferInfo::default() });
        }
        wanted = next;
    }
    found.retain(|_, o| o.id > 0);
    Ok(found)
}

/// The offer ids an item_ids names.
pub fn offer_ids(item_ids: &str) -> Vec<i64> {
    parse(item_ids).unwrap_or_default().into_iter().filter_map(|e| match e {
        Entry::Offer { id, .. } => Some(id),
        Entry::Furni { .. } => None,
    }).collect()
}

/// The furni ids an item_ids names directly.
pub fn furni_ids(item_ids: &str) -> Vec<i64> {
    parse(item_ids).unwrap_or_default().into_iter().filter_map(|e| match e {
        Entry::Furni { id, .. } => Some(id),
        Entry::Offer { .. } => None,
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_read_and_write() {
        let entries = parse("@1234; @1240:2;4501 ;4502:3").unwrap();
        assert_eq!(
            entries,
            vec![
                Entry::Offer { id: 1234, times: None },
                Entry::Offer { id: 1240, times: Some(2) },
                Entry::Furni { id: 4501, amount: None },
                Entry::Furni { id: 4502, amount: Some(3) },
            ]
        );
        assert_eq!(write(&entries), "@1234;@1240:2;4501;4502:3");
        assert_eq!(parse(""), Some(vec![]));
        for bad in ["abc", "@", "@x", "1:2:3", "0", "5:0", "@7:-1"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert_eq!(Entry::Furni { id: 1, amount: None }.with_count(5).text(), "1:5");
        assert_eq!(Entry::Offer { id: 9, times: Some(2) }.with_count(5000).text(), "@9:1000");
    }

    /// The emulator's own cases (BundleOfferReferencesTest).
    #[test]
    fn expansion_matches_the_emulator() {
        let offers: HashMap<i64, Content> = [
            (1234, Content { item_ids: "100".into(), amount: 1 }),
            (1240, Content { item_ids: "200".into(), amount: 3 }),
            (1300, Content { item_ids: "100:2;300".into(), amount: 1 }),
            (1400, Content { item_ids: "@1401".into(), amount: 1 }),
            (1401, Content { item_ids: "@1400".into(), amount: 1 }),
        ]
        .into_iter()
        .collect();
        let lookup = |id: i64| offers.get(&id).cloned();
        // A lone furni entry counts the offer's amount; a named offer brings its own amount.
        assert_eq!(expand("@1234;@1240:2;4501", 1, &lookup), (vec![(100, 1), (200, 6), (4501, 1)], vec![]));
        assert_eq!(expand("4501", 4, &lookup), (vec![(4501, 4)], vec![]));
        // The same furni adds up.
        assert_eq!(expand("@1300;@1234", 1, &lookup).0, vec![(100, 3), (300, 1)]);
        // Unknown offers and loops are left out and reported.
        assert_eq!(expand("@999;4501", 1, &lookup), (vec![(4501, 1)], vec![999]));
        assert_eq!(expand("@1400", 1, &lookup), (vec![], vec![1400]));
        // Capped at 1000.
        assert_eq!(expand("@1240:900", 1, &lookup).0, vec![(200, 1000)]);
    }

    #[test]
    fn new_bundles_and_their_texts() {
        let pages = vec![Page { id: 3, parent_id: -1, order_num: 4, ..Page::default() }];
        let page = new_bundle_page(&pages, -1, "Sunny Café!");
        assert_eq!((page.layout.as_str(), page.caption.as_str(), page.caption_save.as_str(), page.visible, page.order_num), (LAYOUT, "Sunny Café!", "bundle_sunny_caf", false, 5));
        let offer = new_bundle_offer(&Page { id: 77, ..page });
        assert_eq!((offer.page_id, offer.amount, offer.cost_credits), (77, 1, 20));
        assert!(problem(&offer).unwrap().contains("nothing in it"));
        assert_eq!(problem(&Offer { item_ids: "@1;2:3".into(), ..offer.clone() }), None);
        let saved = PageTexts { teaser: "old".into(), details: "<b>Hi</b>".into(), ..PageTexts::default() };
        let now = PageTexts { teaser: " room_teaser ".into(), details: "<b>Hi</b><script>x</script>".into(), ..saved.clone() };
        let changes = now.changes(&saved);
        assert_eq!(changes[0], ("page_teaser".to_string(), Some("room_teaser".to_string())));
        assert_eq!(changes[1].0, "page_text_details");
        assert!(!changes[1].1.as_ref().unwrap().contains("script"));
        assert_eq!(bundle_pages(&[Page { id: 1, layout: "Single_Bundle".into(), ..Page::default() }, Page { id: 2, layout: "default_3x3".into(), ..Page::default() }]).len(), 1);
    }
}
