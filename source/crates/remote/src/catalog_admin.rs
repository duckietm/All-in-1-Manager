//! The catalogue manager's data: the pages and offers of the hotel catalogue (`catalog_pages`, `catalog_items`), read
//! and changed through the SQL session's SSH tunnel, and the running emulator told to load the catalogue again over
//! its RCON port (`updatecatalog`), which also tells the players online to refresh it.
//!
//! The tables are Polaris's. Every change is one statement with `?` parameters; nothing typed in is put into SQL text.

use crate::server::{Result, Server, ServerError};
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The page layouts Polaris knows (the `page_layout` enum of `catalog_pages`).
pub const LAYOUTS: &[&str] = &[
    "default_3x3",
    "default_3x3_color_grouping",
    "frontpage",
    "frontpage_featured",
    "productpage1",
    "single_bundle",
    "room_bundle",
    "spaces",
    "spaces_new",
    "trophies",
    "plasto",
    "pets",
    "pets2",
    "pets3",
    "petcustomization",
    "bots",
    "badge_display",
    "soundmachine",
    "guilds",
    "guild_furni",
    "guild_forum",
    "club_buy",
    "club_gift",
    "vip_buy",
    "loyalty_vip_buy",
    "info_loyalty",
    "info_duckets",
    "info_rentables",
    "info_pets",
    "roomads",
    "recycler",
    "recycler_info",
    "recycler_prizes",
    "marketplace",
    "marketplace_own_items",
    "sold_ltd_items",
    "recent_purchases",
    "collectibles",
    "recolorable_furniture",
    "builders_club_frontpage",
    "builders_club_addons",
    "builders_club_loyalty",
    "root",
    "monkey",
    "niko",
    "mad_money",
];

/// Which catalogue a page belongs to (`catalog_pages.catalog_mode`).
pub const CATALOG_MODES: &[&str] = &["NORMAL", "BUILDER", "BOTH"];

/// The column sizes of the tables, so a save is refused here instead of cut off by MariaDB.
pub const MAX_CAPTION: usize = 128;
pub const MAX_CAPTION_SAVE: usize = 25;
pub const MAX_CATALOG_NAME: usize = 100;
pub const MAX_ITEM_IDS: usize = 666;
pub const MAX_EXTRADATA: usize = 500;

/// One catalogue page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub id: i64,
    pub parent_id: i64,
    pub caption: String,
    /// The internal name the client opens a page by (`catalog/open/<name>`).
    pub caption_save: String,
    pub layout: String,
    pub icon_image: i64,
    pub min_rank: i64,
    pub order_num: i64,
    pub visible: bool,
    pub enabled: bool,
    pub club_only: bool,
    pub catalog_mode: String,
}

/// One offer on a page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Offer {
    pub id: i64,
    pub page_id: i64,
    pub catalog_name: String,
    /// The furni (items_base ids) the offer gives: one id, or several as a bundle ("1;2;3").
    pub item_ids: String,
    pub cost_credits: i64,
    pub cost_points: i64,
    /// The currency of `cost_points` (0 duckets, 5 diamonds, ...).
    pub points_type: i64,
    pub amount: i64,
    /// More than 0 makes it a limited series of that size.
    pub limited_stack: i64,
    pub limited_sells: i64,
    pub order_number: i64,
    pub offer_id: i64,
    pub club_only: bool,
    pub have_offer: bool,
    pub extradata: String,
}

/// A page with how deep it sits in the tree, in the order the tree shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeRow {
    pub index: usize,
    pub depth: usize,
}

const PAGE_COLUMNS: &str =
    "id, parent_id, caption, caption_save, page_layout, icon_image, min_rank, order_num, visible, enabled, club_only, catalog_mode";

const OFFER_COLUMNS: &str = "id, page_id, catalog_name, item_ids, cost_credits, cost_points, points_type, amount, \
     limited_stack, limited_sells, order_number, offer_id, club_only, have_offer, extradata";

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        Some(SqlValue::Float(v)) => *v as i64,
        Some(SqlValue::Double(v)) => *v as i64,
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.clone(),
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// An `enum('0','1')` column.
fn flag(value: Option<&SqlValue>) -> bool {
    text(value).trim() == "1"
}

fn flag_value(on: bool) -> SqlValue {
    SqlValue::Text(if on { "1" } else { "0" }.into())
}

pub fn page_from_row(row: &[SqlValue]) -> Page {
    Page {
        id: int(row.first()),
        parent_id: int(row.get(1)),
        caption: text(row.get(2)),
        caption_save: text(row.get(3)),
        layout: text(row.get(4)),
        icon_image: int(row.get(5)),
        min_rank: int(row.get(6)),
        order_num: int(row.get(7)),
        visible: flag(row.get(8)),
        enabled: flag(row.get(9)),
        club_only: flag(row.get(10)),
        catalog_mode: text(row.get(11)),
    }
}

pub fn offer_from_row(row: &[SqlValue]) -> Offer {
    Offer {
        id: int(row.first()),
        page_id: int(row.get(1)),
        catalog_name: text(row.get(2)),
        item_ids: text(row.get(3)),
        cost_credits: int(row.get(4)),
        cost_points: int(row.get(5)),
        points_type: int(row.get(6)),
        amount: int(row.get(7)),
        limited_stack: int(row.get(8)),
        limited_sells: int(row.get(9)),
        order_number: int(row.get(10)),
        offer_id: int(row.get(11)),
        club_only: flag(row.get(12)),
        have_offer: flag(row.get(13)),
        extradata: text(row.get(14)),
    }
}

/// The pages in tree order: children under their parent, each level by `order_num` then caption. A page whose parent
/// is missing (or that would loop back to itself) is shown at the top level, so no page is ever hidden.
pub fn tree(pages: &[Page]) -> Vec<TreeRow> {
    use std::collections::{HashMap, HashSet};

    let ids: HashSet<i64> = pages.iter().map(|p| p.id).collect();
    let mut children: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();

    for (index, page) in pages.iter().enumerate() {
        if page.parent_id != page.id && ids.contains(&page.parent_id) {
            children.entry(page.parent_id).or_default().push(index);
        } else {
            roots.push(index);
        }
    }

    let sort = |list: &mut Vec<usize>| {
        list.sort_by(|a, b| {
            let (a, b) = (&pages[*a], &pages[*b]);
            a.order_num.cmp(&b.order_num).then_with(|| a.caption.to_lowercase().cmp(&b.caption.to_lowercase())).then(a.id.cmp(&b.id))
        })
    };
    sort(&mut roots);
    for list in children.values_mut() {
        sort(list);
    }

    let mut rows = Vec::with_capacity(pages.len());
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|index| (index, 0)).collect();

    while let Some((index, depth)) = stack.pop() {
        if !seen.insert(index) {
            continue;
        }
        rows.push(TreeRow { index, depth });
        if let Some(list) = children.get(&pages[index].id) {
            stack.extend(list.iter().rev().map(|child| (*child, depth + 1)));
        }
    }

    // Pages in a loop (a parent of its own parent) never hang off a root: put them at the top level.
    for index in 0..pages.len() {
        if !seen.contains(&index) {
            seen.insert(index);
            rows.push(TreeRow { index, depth: 0 });
        }
    }

    rows
}

/// Whether `ancestor` is `page` or above it in the tree: a page may not be moved under itself.
pub fn is_ancestor(pages: &[Page], ancestor: i64, page: i64) -> bool {
    let mut current = page;
    for _ in 0..=pages.len() {
        if current == ancestor {
            return true;
        }
        match pages.iter().find(|p| p.id == current) {
            Some(p) if p.parent_id != current => current = p.parent_id,
            _ => return false,
        }
    }
    false
}

/// Why the page cannot be saved as it is, or None.
pub fn page_problem(page: &Page, pages: &[Page]) -> Option<String> {
    if page.caption.trim().is_empty() {
        return Some("The page needs a name.".into());
    }
    if page.caption.chars().count() > MAX_CAPTION {
        return Some(format!("The name is longer than {MAX_CAPTION} characters."));
    }
    if page.caption_save.chars().count() > MAX_CAPTION_SAVE {
        return Some(format!("The internal name is longer than {MAX_CAPTION_SAVE} characters."));
    }
    if !LAYOUTS.contains(&page.layout.as_str()) {
        return Some(format!("Unknown layout: {}", page.layout));
    }
    if !CATALOG_MODES.contains(&page.catalog_mode.as_str()) {
        return Some(format!("Unknown catalogue: {}", page.catalog_mode));
    }
    if page.parent_id != -1 && page.id > 0 && is_ancestor(pages, page.id, page.parent_id) {
        return Some("A page cannot be put under itself.".into());
    }
    if page.parent_id != -1 && !pages.iter().any(|p| p.id == page.parent_id) {
        return Some(format!("There is no page {} to put it under.", page.parent_id));
    }
    if page.min_rank < 0 || page.icon_image < 0 {
        return Some("The rank and the icon cannot be negative.".into());
    }
    None
}

/// The furni ids of an offer's `item_ids` ("1;2;3" or "1:2" for a bundle with amounts), or None when it is not that.
pub fn item_ids(value: &str) -> Option<Vec<i64>> {
    let mut ids = Vec::new();
    for part in value.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let id = part.split(':').next().unwrap_or("").trim().parse::<i64>().ok().filter(|id| *id > 0)?;
        if let Some(amount) = part.split(':').nth(1) {
            amount.trim().parse::<i64>().ok().filter(|a| *a > 0)?;
        }
        ids.push(id);
    }
    (!ids.is_empty()).then_some(ids)
}

/// Why the offer cannot be saved as it is, or None. Selling fewer than already sold of a limited series is refused,
/// as the emulator keeps the numbers it made.
pub fn offer_problem(offer: &Offer) -> Option<String> {
    if offer.catalog_name.trim().is_empty() {
        return Some("The offer needs a name.".into());
    }
    if offer.catalog_name.chars().count() > MAX_CATALOG_NAME {
        return Some(format!("The name is longer than {MAX_CATALOG_NAME} characters."));
    }
    if offer.item_ids.len() > MAX_ITEM_IDS || item_ids(&offer.item_ids).is_none() {
        return Some("The furni ids must be numbers, separated by ; (for example 1234 or 100;200).".into());
    }
    if offer.extradata.len() > MAX_EXTRADATA {
        return Some(format!("The extra data is longer than {MAX_EXTRADATA} characters."));
    }
    if offer.cost_credits < 0 || offer.cost_points < 0 || offer.points_type < 0 {
        return Some("Prices cannot be negative.".into());
    }
    if offer.amount < 1 {
        return Some("The amount must be at least 1.".into());
    }
    if offer.limited_stack < 0 {
        return Some("The limited size cannot be negative.".into());
    }
    if offer.limited_stack > 0 && offer.limited_stack < offer.limited_sells {
        return Some(format!("{} are sold already: the limited size cannot be smaller.", offer.limited_sells));
    }
    None
}

/// `SELECT` of every page of the hotel database.
pub fn pages_sql(database: &str) -> String {
    format!("SELECT {PAGE_COLUMNS} FROM {}.catalog_pages ORDER BY parent_id, order_num, id", quote(database))
}

/// `SELECT` of the offers of one page (`?` = the page id).
pub fn offers_sql(database: &str) -> String {
    format!("SELECT {OFFER_COLUMNS} FROM {}.catalog_items WHERE page_id = ? ORDER BY order_number, id", quote(database))
}

pub fn update_page_sql(database: &str) -> String {
    format!(
        "UPDATE {}.catalog_pages SET parent_id = ?, caption = ?, caption_save = ?, page_layout = ?, icon_image = ?, min_rank = ?, \
         order_num = ?, visible = ?, enabled = ?, club_only = ?, catalog_mode = ? WHERE id = ?",
        quote(database)
    )
}

pub fn update_page_params(page: &Page) -> Vec<SqlValue> {
    vec![
        SqlValue::Int(page.parent_id),
        SqlValue::Text(page.caption.trim().to_string()),
        SqlValue::Text(page.caption_save.trim().to_string()),
        SqlValue::Text(page.layout.clone()),
        SqlValue::Int(page.icon_image),
        SqlValue::Int(page.min_rank),
        SqlValue::Int(page.order_num),
        flag_value(page.visible),
        flag_value(page.enabled),
        flag_value(page.club_only),
        SqlValue::Text(page.catalog_mode.clone()),
        SqlValue::Int(page.id),
    ]
}

pub fn update_offer_sql(database: &str) -> String {
    format!(
        "UPDATE {}.catalog_items SET page_id = ?, catalog_name = ?, item_ids = ?, cost_credits = ?, cost_points = ?, points_type = ?, \
         amount = ?, limited_stack = ?, order_number = ?, offer_id = ?, club_only = ?, have_offer = ?, extradata = ? WHERE id = ?",
        quote(database)
    )
}

pub fn update_offer_params(offer: &Offer) -> Vec<SqlValue> {
    vec![
        SqlValue::Int(offer.page_id),
        SqlValue::Text(offer.catalog_name.trim().to_string()),
        SqlValue::Text(offer.item_ids.trim().to_string()),
        SqlValue::Int(offer.cost_credits),
        SqlValue::Int(offer.cost_points),
        SqlValue::Int(offer.points_type),
        SqlValue::Int(offer.amount),
        SqlValue::Int(offer.limited_stack),
        SqlValue::Int(offer.order_number),
        SqlValue::Int(offer.offer_id),
        flag_value(offer.club_only),
        flag_value(offer.have_offer),
        SqlValue::Text(offer.extradata.clone()),
        SqlValue::Int(offer.id),
    ]
}

pub fn insert_page_sql(database: &str) -> String {
    format!(
        "INSERT INTO {}.catalog_pages (parent_id, caption, caption_save, page_layout, icon_image, min_rank, order_num, visible, enabled, \
         club_only, catalog_mode) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        quote(database)
    )
}

/// The values of [`update_page_params`] without the id at the end.
pub fn insert_page_params(page: &Page) -> Vec<SqlValue> {
    let mut params = update_page_params(page);
    params.pop();
    params
}

pub fn insert_offer_sql(database: &str) -> String {
    format!(
        "INSERT INTO {}.catalog_items (page_id, catalog_name, item_ids, cost_credits, cost_points, points_type, amount, limited_stack, \
         order_number, offer_id, club_only, have_offer, extradata) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        quote(database)
    )
}

/// The values of [`update_offer_params`] without the id at the end.
pub fn insert_offer_params(offer: &Offer) -> Vec<SqlValue> {
    let mut params = update_offer_params(offer);
    params.pop();
    params
}

/// A new page under `parent_id` (-1: the top level), after the pages already there; not saved yet (id 0).
pub fn new_page(pages: &[Page], parent_id: i64) -> Page {
    let order_num = pages.iter().filter(|p| p.parent_id == parent_id).map(|p| p.order_num).max().map_or(1, |o| o + 1);
    Page {
        id: 0,
        parent_id,
        caption: "New page".into(),
        caption_save: String::new(),
        layout: "default_3x3".into(),
        icon_image: 1,
        min_rank: 1,
        order_num,
        visible: false,
        enabled: true,
        club_only: false,
        catalog_mode: "NORMAL".into(),
    }
}

/// A new offer on the page, after the offers already there; not saved yet (id 0). The furni ids are left to fill in.
pub fn new_offer(offers: &[Offer], page_id: i64) -> Offer {
    let order_number = offers.iter().map(|o| o.order_number).max().map_or(1, |o| o + 1);
    Offer {
        id: 0,
        page_id,
        catalog_name: String::new(),
        item_ids: String::new(),
        cost_credits: 3,
        amount: 1,
        order_number,
        offer_id: -1,
        have_offer: true,
        ..Offer::default()
    }
}

/// Why the page cannot be deleted, or None. Its sub-pages go first (they would lose their place in the tree), and an
/// offer with limited sales keeps the record of who has which number.
pub fn page_delete_problem(pages: &[Page], offers: &[Offer], id: i64) -> Option<String> {
    let children = pages.iter().filter(|p| p.parent_id == id && p.id != id).count();
    if children > 0 {
        return Some(format!("The page has {children} sub-pages: move or delete them first."));
    }
    offers.iter().find(|o| o.page_id == id && o.limited_sells > 0).map(|o| {
        format!("The offer {} sold {} limited numbers: it cannot be deleted.", o.catalog_name, o.limited_sells)
    })
}

/// Why the offer cannot be deleted, or None: its limited sales would be lost.
pub fn offer_delete_problem(offer: &Offer) -> Option<String> {
    (offer.limited_sells > 0).then(|| format!("{} limited numbers are sold: the offer cannot be deleted.", offer.limited_sells))
}

/// Every page of the hotel database.
pub async fn load_pages(session: &SqlSession, database: &str) -> Result<Vec<Page>> {
    let rows = session.select(&pages_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| page_from_row(row)).collect())
}

/// The offers of one page.
pub async fn load_offers(session: &SqlSession, database: &str, page_id: i64) -> Result<Vec<Offer>> {
    let rows = session.select(&offers_sql(database), vec![SqlValue::Int(page_id)]).await?;
    Ok(rows.iter().map(|row| offer_from_row(row)).collect())
}

/// Saves the page; a new one (id 0) is added. Returns its id.
pub async fn save_page(session: &SqlSession, database: &str, page: &Page) -> Result<i64> {
    if page.id <= 0 {
        let (_, id) = session.execute(&[(insert_page_sql(database), insert_page_params(page))]).await?;
        return Ok(id as i64);
    }
    session.execute(&[(update_page_sql(database), update_page_params(page))]).await?;
    Ok(page.id)
}

/// Saves the offer; a new one (id 0) is added. Returns its id.
pub async fn save_offer(session: &SqlSession, database: &str, offer: &Offer) -> Result<i64> {
    if offer.id <= 0 {
        let (_, id) = session.execute(&[(insert_offer_sql(database), insert_offer_params(offer))]).await?;
        return Ok(id as i64);
    }
    session.execute(&[(update_offer_sql(database), update_offer_params(offer))]).await?;
    Ok(offer.id)
}

/// Deletes the page with its offers, in one transaction. Check [`page_delete_problem`] first.
pub async fn delete_page(session: &SqlSession, database: &str, id: i64) -> Result<u64> {
    let db = quote(database);
    let statements = vec![
        (format!("DELETE FROM {db}.catalog_items WHERE page_id = ?"), vec![SqlValue::Int(id)]),
        (format!("DELETE FROM {db}.catalog_pages WHERE id = ?"), vec![SqlValue::Int(id)]),
    ];
    Ok(session.execute(&statements).await?.0)
}

/// Deletes the offer. Check [`offer_delete_problem`] first.
pub async fn delete_offer(session: &SqlSession, database: &str, id: i64) -> Result<u64> {
    let statements = vec![(format!("DELETE FROM {}.catalog_items WHERE id = ?", quote(database)), vec![SqlValue::Int(id)])];
    Ok(session.execute(&statements).await?.0)
}

// ---- Front page featured slots (catalog_featured_pages) ------------------------------------------------------

/// What a front page slot opens (`catalog_featured_pages.type`).
pub const FEATURED_TYPES: &[&str] = &["page_name", "page_id", "product_name"];
pub const MAX_FEATURED_SLOTS: usize = 20;
pub const MAX_FEATURED_IMAGE: usize = 70;
pub const MAX_FEATURED_CAPTION: usize = 130;
pub const MAX_FEATURED_PAGE_NAME: usize = 30;
pub const MAX_FEATURED_PRODUCT: usize = 40;

/// One promo tile of the catalogue front page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Featured {
    pub slot_id: i64,
    pub image: String,
    pub caption: String,
    /// One of [`FEATURED_TYPES`].
    pub kind: String,
    /// Unix time it stops showing; -1 (or 0) never.
    pub expire_timestamp: i64,
    pub page_name: String,
    pub page_id: i64,
    pub product_name: String,
}

pub fn featured_sql(database: &str) -> String {
    format!(
        "SELECT slot_id, image, caption, type, expire_timestamp, page_name, page_id, product_name FROM {}.catalog_featured_pages ORDER BY slot_id",
        quote(database)
    )
}

pub fn featured_from_row(row: &[SqlValue]) -> Featured {
    Featured {
        slot_id: int(row.first()),
        image: text(row.get(1)),
        caption: text(row.get(2)),
        kind: text(row.get(3)),
        expire_timestamp: int(row.get(4)),
        page_name: text(row.get(5)),
        page_id: int(row.get(6)),
        product_name: text(row.get(7)),
    }
}

/// A new slot on the first free number (1, 2, ...), opening a page by name.
pub fn new_featured(slots: &[Featured]) -> Featured {
    let slot_id = (1..=99).find(|id| !slots.iter().any(|s| s.slot_id == *id)).unwrap_or(0);
    Featured { slot_id, kind: "page_name".into(), expire_timestamp: -1, ..Featured::default() }
}

/// Why the set of slots cannot be saved, or None: the table's column sizes, one entry per slot number, and a target
/// for each slot's type.
pub fn featured_problem(slots: &[Featured]) -> Option<String> {
    if slots.len() > MAX_FEATURED_SLOTS {
        return Some(format!("At most {MAX_FEATURED_SLOTS} slots."));
    }
    for (index, slot) in slots.iter().enumerate() {
        let id = slot.slot_id;
        if !(1..=99).contains(&id) {
            return Some(format!("Slot {id}: the number must be 1 to 99."));
        }
        if slots[..index].iter().any(|other| other.slot_id == id) {
            return Some(format!("Slot {id} is there twice."));
        }
        if !FEATURED_TYPES.contains(&slot.kind.as_str()) {
            return Some(format!("Slot {id}: unknown type {}.", slot.kind));
        }
        if slot.image.chars().count() > MAX_FEATURED_IMAGE
            || slot.caption.chars().count() > MAX_FEATURED_CAPTION
            || slot.page_name.chars().count() > MAX_FEATURED_PAGE_NAME
            || slot.product_name.chars().count() > MAX_FEATURED_PRODUCT
        {
            return Some(format!("Slot {id}: a text is too long."));
        }
        let missing = match slot.kind.as_str() {
            "page_name" => slot.page_name.trim().is_empty(),
            "page_id" => slot.page_id <= 0,
            _ => slot.product_name.trim().is_empty(),
        };
        if missing {
            return Some(format!("Slot {id}: fill in what it opens."));
        }
    }
    None
}

/// Replaces every front page slot with these, in one transaction. Check [`featured_problem`] first.
pub async fn save_featured(session: &SqlSession, database: &str, slots: &[Featured]) -> Result<u64> {
    let db = quote(database);
    let mut statements = vec![(format!("DELETE FROM {db}.catalog_featured_pages"), Vec::new())];
    for slot in slots {
        statements.push((
            format!(
                "INSERT INTO {db}.catalog_featured_pages (slot_id, image, caption, type, expire_timestamp, page_name, page_id, product_name) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            ),
            vec![
                SqlValue::Int(slot.slot_id),
                SqlValue::Text(slot.image.trim().to_string()),
                SqlValue::Text(slot.caption.trim().to_string()),
                SqlValue::Text(slot.kind.clone()),
                SqlValue::Int(if slot.expire_timestamp > 0 { slot.expire_timestamp } else { -1 }),
                SqlValue::Text(slot.page_name.trim().to_string()),
                SqlValue::Int(slot.page_id.max(0)),
                SqlValue::Text(slot.product_name.trim().to_string()),
            ],
        ));
    }
    Ok(session.execute(&statements).await?.0)
}

pub async fn load_featured(session: &SqlSession, database: &str) -> Result<Vec<Featured>> {
    let rows = session.select(&featured_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| featured_from_row(row)).collect())
}

/// A unix time as "YYYY-MM-DD HH:MM" in the time zone of this computer; empty for none.
pub fn expiry_text(timestamp: i64) -> String {
    use chrono::TimeZone;
    if timestamp <= 0 {
        return String::new();
    }
    chrono::Local.timestamp_opt(timestamp, 0).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// "YYYY-MM-DD HH:MM" (or just the date) in the time zone of this computer as a unix time; -1 for empty, None when
/// it is not a date.
pub fn parse_expiry(text: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let text = text.trim();
    if text.is_empty() {
        return Some(-1);
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))?;
    Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp())
}

// ---- Limited series ---------------------------------------------------------------------------------------------

/// The size a limited series can have.
pub const MAX_LIMITED_STACK: i64 = 1_000_000;

/// A limited series: an offer with a limited size, or one whose numbers are still in `catalog_items_limited`.
/// `sold` counts the numbers a player holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LimitedSeries {
    pub offer_id: i64,
    pub catalog_name: String,
    pub page_id: i64,
    pub page_caption: String,
    pub stack: i64,
    pub sold: i64,
}

/// What can be done with a limited series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitedAction {
    /// A bigger size; the emulator makes the new numbers when it loads the catalogue.
    Grow(i64),
    /// No longer limited, while nothing is sold.
    End,
    /// No longer limited, and its numbers and the record of who has which one are deleted (the furni keeps its serial).
    RemoveWithSales,
}

pub fn limited_sql(database: &str) -> String {
    let db = quote(database);
    format!(
        "SELECT ci.id, ci.catalog_name, ci.page_id, COALESCE(cp.caption, ''), ci.limited_stack, COALESCE(sold.count, 0) \
         FROM {db}.catalog_items ci LEFT JOIN {db}.catalog_pages cp ON cp.id = ci.page_id \
         LEFT JOIN (SELECT catalog_item_id, SUM(user_id > 0) AS count, COUNT(*) AS total FROM {db}.catalog_items_limited GROUP BY catalog_item_id) sold \
         ON sold.catalog_item_id = ci.id WHERE ci.limited_stack > 0 OR sold.total > 0 ORDER BY ci.id DESC"
    )
}

pub fn limited_from_row(row: &[SqlValue]) -> LimitedSeries {
    LimitedSeries {
        offer_id: int(row.first()),
        catalog_name: text(row.get(1)),
        page_id: int(row.get(2)),
        page_caption: text(row.get(3)),
        stack: int(row.get(4)),
        sold: int(row.get(5)),
    }
}

/// Why the action cannot be done, or None. A series only grows: its numbers exist from the moment it went on sale, and
/// the emulator keeps a smaller size at the numbers it made.
pub fn limited_problem(series: &LimitedSeries, action: LimitedAction) -> Option<String> {
    match action {
        LimitedAction::Grow(size) if size > MAX_LIMITED_STACK => Some(format!("At most {MAX_LIMITED_STACK}.")),
        LimitedAction::Grow(size) if size <= series.stack.max(0) => Some(format!("A series can only grow: more than {}.", series.stack.max(0))),
        LimitedAction::End if series.sold > 0 => Some(format!("{} are sold: remove it with its sales instead.", series.sold)),
        _ => None,
    }
}

/// The statements of the action, in the order they run (one transaction).
pub fn limited_statements(database: &str, offer_id: i64, action: LimitedAction) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    let id = SqlValue::Int(offer_id);
    match action {
        LimitedAction::Grow(size) => vec![(format!("UPDATE {db}.catalog_items SET limited_stack = ? WHERE id = ?"), vec![SqlValue::Int(size), id])],
        LimitedAction::End => vec![(format!("UPDATE {db}.catalog_items SET limited_stack = 0 WHERE id = ?"), vec![id])],
        LimitedAction::RemoveWithSales => vec![
            (format!("DELETE FROM {db}.catalog_items_limited WHERE catalog_item_id = ?"), vec![id.clone()]),
            (format!("UPDATE {db}.catalog_items SET limited_stack = 0, limited_sells = 0 WHERE id = ?"), vec![id]),
        ],
    }
}

pub async fn load_limited(session: &SqlSession, database: &str) -> Result<Vec<LimitedSeries>> {
    let rows = session.select(&limited_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| limited_from_row(row)).collect())
}

/// Does the action. Check [`limited_problem`] first.
pub async fn change_limited(session: &SqlSession, database: &str, offer_id: i64, action: LimitedAction) -> Result<u64> {
    Ok(session.execute(&limited_statements(database, offer_id, action)).await?.0)
}

// ---- Vouchers -------------------------------------------------------------------------------------------------

/// `vouchers.code` is a varchar(10); the emulator matches it exactly as typed.
pub const MAX_VOUCHER_CODE: usize = 10;
/// Letters and digits that are easy to read and type (no 0/O, 1/I/L).
pub const VOUCHER_ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const MAX_GENERATED_VOUCHERS: usize = 500;

/// A voucher code and what it gives. `amount` is how many times it can be used in all (0 without end), `limit` how
/// many times by one player (-1 or 0 without end); `used` counts `voucher_history`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Voucher {
    pub id: i64,
    pub code: String,
    pub credits: i64,
    pub points: i64,
    pub points_type: i64,
    /// The catalogue offer (`catalog_items.id`) it gives, or 0.
    pub catalog_item_id: i64,
    pub amount: i64,
    pub limit: i64,
    pub used: i64,
}

/// One use of a voucher.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VoucherUse {
    pub user_id: i64,
    pub username: String,
    pub timestamp: i64,
}

pub fn vouchers_sql(database: &str) -> String {
    let db = quote(database);
    format!(
        "SELECT v.id, v.code, v.credits, v.points, v.points_type, v.catalog_item_id, v.amount, v.`limit`, \
         (SELECT COUNT(*) FROM {db}.voucher_history h WHERE h.voucher_id = v.id) FROM {db}.vouchers v ORDER BY v.id DESC"
    )
}

pub fn voucher_from_row(row: &[SqlValue]) -> Voucher {
    Voucher {
        id: int(row.first()),
        code: text(row.get(1)),
        credits: int(row.get(2)),
        points: int(row.get(3)),
        points_type: int(row.get(4)),
        catalog_item_id: int(row.get(5)),
        amount: int(row.get(6)),
        limit: int(row.get(7)),
        used: int(row.get(8)),
    }
}

/// The last 500 uses of a voucher, newest first, with the player's name (`?` = the voucher id).
pub fn voucher_uses_sql(database: &str) -> String {
    let db = quote(database);
    format!(
        "SELECT h.user_id, COALESCE(u.username, ''), h.timestamp FROM {db}.voucher_history h LEFT JOIN {db}.users u ON u.id = h.user_id \
         WHERE h.voucher_id = ? ORDER BY h.timestamp DESC, h.id DESC LIMIT 500"
    )
}

pub fn voucher_use_from_row(row: &[SqlValue]) -> VoucherUse {
    VoucherUse { user_id: int(row.first()), username: text(row.get(1)), timestamp: int(row.get(2)) }
}

/// A code the emulator can match: 1 to 10 letters, digits, - or _.
pub fn valid_voucher_code(code: &str) -> bool {
    (1..=MAX_VOUCHER_CODE).contains(&code.chars().count()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Why the voucher cannot be saved, or None. `others` are the other vouchers: a code already used would never be
/// matched to this one.
pub fn voucher_problem(voucher: &Voucher, others: &[Voucher]) -> Option<String> {
    let code = voucher.code.trim();
    if !valid_voucher_code(code) {
        return Some(format!("The code must be 1 to {MAX_VOUCHER_CODE} letters, digits, - or _."));
    }
    if others.iter().any(|o| o.id != voucher.id && o.code == code) {
        return Some(format!("The code {code} is used already."));
    }
    if voucher.credits < 0 || voucher.points < 0 || voucher.points_type < 0 || voucher.catalog_item_id < 0 {
        return Some("The values cannot be negative.".into());
    }
    if voucher.credits == 0 && voucher.points == 0 && voucher.catalog_item_id == 0 {
        return Some("The voucher gives nothing: set credits, points or an offer.".into());
    }
    if voucher.amount < 0 || voucher.limit < -1 {
        return Some("The number of uses cannot be negative.".into());
    }
    None
}

/// `count` new codes of `length` characters (`prefix` included) that no voucher has yet, from [`VOUCHER_ALPHABET`].
/// None when that does not fit: a prefix too long, more than [`MAX_GENERATED_VOUCHERS`], or too few possible codes.
pub fn generate_voucher_codes(prefix: &str, length: usize, count: usize, taken: &[String], rng: &mut impl rand::Rng) -> Option<Vec<String>> {
    use std::collections::HashSet;
    let prefix = prefix.trim().to_uppercase();
    if count == 0 || count > MAX_GENERATED_VOUCHERS || length > MAX_VOUCHER_CODE || !valid_voucher_code(&format!("{prefix}A")) {
        return None;
    }
    let random = length.checked_sub(prefix.chars().count()).filter(|n| *n >= 4)?;
    let alphabet: Vec<char> = VOUCHER_ALPHABET.chars().collect();
    let mut seen: HashSet<String> = taken.iter().cloned().collect();
    let mut codes = Vec::with_capacity(count);
    let mut tries = 0;
    while codes.len() < count {
        tries += 1;
        if tries > count * 50 {
            return None;
        }
        let code: String = prefix.chars().chain((0..random).map(|_| alphabet[rng.gen_range(0..alphabet.len())])).collect();
        if seen.insert(code.clone()) {
            codes.push(code);
        }
    }
    Some(codes)
}

/// [`generate_voucher_codes`] with the thread's random generator.
pub fn random_voucher_codes(prefix: &str, length: usize, count: usize, taken: &[String]) -> Option<Vec<String>> {
    generate_voucher_codes(prefix, length, count, taken, &mut rand::thread_rng())
}

pub async fn load_vouchers(session: &SqlSession, database: &str) -> Result<Vec<Voucher>> {
    let rows = session.select(&vouchers_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| voucher_from_row(row)).collect())
}

pub async fn load_voucher_uses(session: &SqlSession, database: &str, id: i64) -> Result<Vec<VoucherUse>> {
    let rows = session.select(&voucher_uses_sql(database), vec![SqlValue::Int(id)]).await?;
    Ok(rows.iter().map(|row| voucher_use_from_row(row)).collect())
}

fn voucher_values(voucher: &Voucher) -> Vec<SqlValue> {
    vec![
        SqlValue::Text(voucher.code.trim().to_string()),
        SqlValue::Int(voucher.credits),
        SqlValue::Int(voucher.points),
        SqlValue::Int(voucher.points_type),
        SqlValue::Int(voucher.catalog_item_id),
        SqlValue::Int(voucher.amount),
        SqlValue::Int(voucher.limit),
    ]
}

pub fn insert_voucher_sql(database: &str) -> String {
    format!(
        "INSERT INTO {}.vouchers (code, credits, points, points_type, catalog_item_id, amount, `limit`) VALUES (?, ?, ?, ?, ?, ?, ?)",
        quote(database)
    )
}

pub fn update_voucher_sql(database: &str) -> String {
    format!(
        "UPDATE {}.vouchers SET code = ?, credits = ?, points = ?, points_type = ?, catalog_item_id = ?, amount = ?, `limit` = ? WHERE id = ?",
        quote(database)
    )
}

/// Saves the voucher; a new one (id 0) is added.
pub async fn save_voucher(session: &SqlSession, database: &str, voucher: &Voucher) -> Result<u64> {
    let statement = if voucher.id <= 0 {
        (insert_voucher_sql(database), voucher_values(voucher))
    } else {
        let mut values = voucher_values(voucher);
        values.push(SqlValue::Int(voucher.id));
        (update_voucher_sql(database), values)
    };
    Ok(session.execute(&[statement]).await?.0)
}

/// Adds one voucher per code, each like `template`.
pub async fn add_vouchers(session: &SqlSession, database: &str, template: &Voucher, codes: &[String]) -> Result<u64> {
    let statements: Vec<_> = codes
        .iter()
        .map(|code| (insert_voucher_sql(database), voucher_values(&Voucher { code: code.clone(), ..template.clone() })))
        .collect();
    Ok(session.execute(&statements).await?.0)
}

/// Deletes the voucher and its history.
pub async fn delete_voucher(session: &SqlSession, database: &str, id: i64) -> Result<u64> {
    let db = quote(database);
    let statements = vec![
        (format!("DELETE FROM {db}.voucher_history WHERE voucher_id = ?"), vec![SqlValue::Int(id)]),
        (format!("DELETE FROM {db}.vouchers WHERE id = ?"), vec![SqlValue::Int(id)]),
    ];
    Ok(session.execute(&statements).await?.0)
}

// ---- Furni search (items_base) ----------------------------------------------------------------------------------

/// One furni definition, for picking an offer's furni.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Furni {
    pub id: i64,
    pub item_name: String,
    pub public_name: String,
    /// `s` floor, `i` wall, `e` effect, `b` badge, `r` bot, `p` pet, ...
    pub kind: String,
    pub sprite_id: i64,
}

pub const MAX_FURNI_RESULTS: i64 = 100;

/// Furni whose id is the text, or whose class name or name contains it (`?`: the id or -1, then the pattern twice).
pub fn furni_search_sql(database: &str) -> String {
    format!(
        "SELECT id, item_name, public_name, type, sprite_id FROM {}.items_base WHERE id = ? OR item_name LIKE ? OR public_name LIKE ? \
         ORDER BY (id = ?) DESC, item_name LIMIT {MAX_FURNI_RESULTS}",
        quote(database)
    )
}

/// The parameters of [`furni_search_sql`]: `%` and `_` in the text are matched as themselves.
pub fn furni_search_params(text: &str) -> Vec<SqlValue> {
    let text = text.trim();
    let id = text.parse::<i64>().ok().filter(|id| *id > 0).unwrap_or(-1);
    let escaped = text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = SqlValue::Text(format!("%{escaped}%"));
    vec![SqlValue::Int(id), pattern.clone(), pattern, SqlValue::Int(id)]
}

pub fn furni_from_row(row: &[SqlValue]) -> Furni {
    Furni { id: int(row.first()), item_name: text(row.get(1)), public_name: text(row.get(2)), kind: text(row.get(3)), sprite_id: int(row.get(4)) }
}

/// The furni of these ids (to show an offer's furni by name).
pub fn furni_by_ids_sql(database: &str, count: usize) -> String {
    let marks = vec!["?"; count.max(1)].join(", ");
    format!("SELECT id, item_name, public_name, type, sprite_id FROM {}.items_base WHERE id IN ({marks}) ORDER BY id", quote(database))
}

/// At least two characters (or an id), so a search never lists the whole table.
pub async fn search_furni(session: &SqlSession, database: &str, text: &str) -> Result<Vec<Furni>> {
    if text.trim().chars().count() < 2 && text.trim().parse::<i64>().is_err() {
        return Ok(Vec::new());
    }
    let rows = session.select(&furni_search_sql(database), furni_search_params(text)).await?;
    Ok(rows.iter().map(|row| furni_from_row(row)).collect())
}

pub async fn furni_by_ids(session: &SqlSession, database: &str, ids: &[i64]) -> Result<Vec<Furni>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = session.select(&furni_by_ids_sql(database, ids.len()), ids.iter().map(|id| SqlValue::Int(*id)).collect()).await?;
    Ok(rows.iter().map(|row| furni_from_row(row)).collect())
}

/// The offer's furni ids with one more: a bundle keeps every id once ("1;2").
pub fn add_furni_id(item_ids: &str, id: i64) -> String {
    let mut parts: Vec<String> = item_ids.split(';').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect();
    if !parts.iter().any(|p| p.split(':').next() == Some(&id.to_string())) {
        parts.push(id.to_string());
    }
    parts.join(";")
}

/// The offer's furni ids without this one.
pub fn remove_furni_id(item_ids: &str, id: i64) -> String {
    item_ids
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty() && p.split(':').next() != Some(&id.to_string()))
        .collect::<Vec<_>>()
        .join(";")
}

// ---- Checks: duplicates and order ------------------------------------------------------------------------------

/// Every offer of the hotel (the duplicate check).
pub fn all_offers_sql(database: &str) -> String {
    format!("SELECT {OFFER_COLUMNS} FROM {}.catalog_items ORDER BY page_id, order_number, id", quote(database))
}

pub async fn load_all_offers(session: &SqlSession, database: &str) -> Result<Vec<Offer>> {
    let rows = session.select(&all_offers_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| offer_from_row(row)).collect())
}

/// The decoration an offer's name chooses: the emulator gives a `wallpaper_single`, `floor_single` or
/// `landscape_single` offer the third part of its name as its pattern (`wallpaper_single_102` is pattern 102), so
/// those offers share their furni but are not the same. None for any other name.
pub fn decoration_variant(catalog_name: &str) -> Option<&str> {
    ["wallpaper_single", "floor_single", "landscape_single"]
        .iter()
        .any(|kind| catalog_name.contains(kind))
        .then(|| catalog_name.split('_').nth(2).unwrap_or(""))
}

/// What makes two offers the same: their furni (in any order, with amounts), their extra data (a poster or a trophy
/// text makes it another offer) and, for wallpaper, floors and landscapes, the pattern their name chooses.
fn offer_key(offer: &Offer) -> String {
    let mut parts: Vec<String> = offer.item_ids.split(';').map(|p| p.trim().replace(' ', "")).filter(|p| !p.is_empty()).collect();
    parts.sort();
    let variant = decoration_variant(&offer.catalog_name).unwrap_or("");
    format!("{}\u{1}{}\u{1}{}", parts.join(";"), offer.extradata.trim(), variant)
}

/// Offers that sell the same furni more than once, as groups of indexes into `offers` (biggest group first). Offers
/// without furni are left out.
pub fn duplicate_offer_groups(offers: &[Offer]) -> Vec<Vec<usize>> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for (index, offer) in offers.iter().enumerate() {
        if item_ids(&offer.item_ids).is_some() {
            groups.entry(offer_key(offer)).or_default().push(index);
        }
    }
    let mut groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
    groups
}

/// Pages that share an internal name (the client opens the first: `catalog/open/<name>` finds only one), as groups of
/// indexes into `pages`. Pages without an internal name are left out.
pub fn duplicate_page_groups(pages: &[Page]) -> Vec<Vec<usize>> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for (index, page) in pages.iter().enumerate() {
        let name = page.caption_save.trim().to_lowercase();
        if !name.is_empty() {
            groups.entry(name).or_default().push(index);
        }
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// The offers' new order numbers (1, 2, ...) sorted by name, case blind, then id: (offer id, order number).
pub fn sorted_by_name(offers: &[Offer]) -> Vec<(i64, i64)> {
    let mut sorted: Vec<&Offer> = offers.iter().collect();
    sorted.sort_by(|a, b| a.catalog_name.to_lowercase().cmp(&b.catalog_name.to_lowercase()).then(a.id.cmp(&b.id)));
    sorted.iter().enumerate().map(|(i, o)| (o.id, i as i64 + 1)).collect()
}

/// Sets the order numbers in one transaction.
pub async fn set_offer_order(session: &SqlSession, database: &str, order: &[(i64, i64)]) -> Result<u64> {
    let sql = format!("UPDATE {}.catalog_items SET order_number = ? WHERE id = ?", quote(database));
    let statements: Vec<_> = order.iter().map(|(id, n)| (sql.clone(), vec![SqlValue::Int(*n), SqlValue::Int(*id)])).collect();
    Ok(session.execute(&statements).await?.0)
}

/// Deletes these offers in one transaction. Leave out offers with limited sales ([`offer_delete_problem`]).
pub async fn delete_offers(session: &SqlSession, database: &str, ids: &[i64]) -> Result<u64> {
    let sql = format!("DELETE FROM {}.catalog_items WHERE id = ?", quote(database));
    let statements: Vec<_> = ids.iter().map(|id| (sql.clone(), vec![SqlValue::Int(*id)])).collect();
    Ok(session.execute(&statements).await?.0)
}

// ---- Unoffered furni ---------------------------------------------------------------------------------------------

/// Every floor (`s`) and wall (`i`) furni definition: the kinds a catalogue offer sells as furni.
pub fn sellable_furni_sql(database: &str) -> String {
    format!("SELECT id, item_name, public_name, type, sprite_id FROM {}.items_base WHERE type IN ('s', 'i') ORDER BY item_name", quote(database))
}

pub async fn load_sellable_furni(session: &SqlSession, database: &str) -> Result<Vec<Furni>> {
    let rows = session.select(&sellable_furni_sql(database), Vec::new()).await?;
    Ok(rows.iter().map(|row| furni_from_row(row)).collect())
}

/// The furni no offer sells: not in any offer's furni ids. `offers` are every offer of the catalogue.
pub fn unoffered_furni(furni: &[Furni], offers: &[Offer]) -> Vec<Furni> {
    let offered: std::collections::HashSet<i64> = offers
        .iter()
        .flat_map(|o| o.item_ids.split(';').filter_map(|p| p.split(':').next()?.trim().parse::<i64>().ok()).collect::<Vec<_>>())
        .collect();
    furni.iter().filter(|f| !offered.contains(&f.id)).cloned().collect()
}

/// New offers on `page_id`, one per furni, named after its class name, after the offers already there.
pub fn offers_for(furni: &[Furni], page_id: i64, existing: &[Offer], credits: i64, points: i64, points_type: i64) -> Vec<Offer> {
    let start = existing.iter().filter(|o| o.page_id == page_id).map(|o| o.order_number).max().unwrap_or(0);
    furni
        .iter()
        .enumerate()
        .map(|(i, f)| Offer {
            id: 0,
            page_id,
            catalog_name: f.item_name.chars().take(MAX_CATALOG_NAME).collect(),
            item_ids: f.id.to_string(),
            cost_credits: credits,
            cost_points: points,
            points_type,
            amount: 1,
            order_number: start + i as i64 + 1,
            offer_id: -1,
            have_offer: true,
            ..Offer::default()
        })
        .collect()
}

/// Adds the offers in one transaction.
pub async fn add_offers(session: &SqlSession, database: &str, offers: &[Offer]) -> Result<u64> {
    let statements: Vec<_> = offers.iter().map(|o| (insert_offer_sql(database), insert_offer_params(o))).collect();
    Ok(session.execute(&statements).await?.0)
}

// ---- Catalogue backups (on the server) --------------------------------------------------------------------------

/// The tables a catalogue backup holds. Sold limited numbers (`catalog_items_limited`) and voucher uses
/// (`voucher_history`) are left out: they record what players own and used, which a rollback must never take back.
pub const BACKUP_TABLES: &[&str] = &["catalog_pages", "catalog_items", "catalog_featured_pages", "vouchers", "catalog_pages_bc", "catalog_items_bc"];

/// One catalogue backup file on the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogBackup {
    pub file: String,
    pub bytes: i64,
    /// Unix time it was made.
    pub made: i64,
}

/// The folder of the catalogue backups on the server.
pub fn backup_folder() -> String {
    format!("{}/catalogue", crate::server_tools::backup_directory())
}

/// A backup's label as it goes into the file name: letters, digits, - and _ only, at most 40.
pub fn backup_label(label: &str) -> String {
    let cleaned: String = label.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(40).collect();
    cleaned.trim_matches('-').to_string()
}

/// A file name the manager made: `<db>-catalogue-<date>-<time>[-label].sql.gz`, with nothing that leaves the folder.
pub fn valid_backup_file(file: &str) -> bool {
    file.ends_with(".sql.gz")
        && file.contains("-catalogue-")
        && !file.starts_with('.')
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !file.contains("..")
}

fn backup_tables_shell() -> String {
    BACKUP_TABLES.join(" ")
}

const MAKE_BACKUP: &str = r#"set -o pipefail
{{DatabaseName}}
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "Database $db does not exist or MariaDB does not run."
install -d -m 700 {{Folder}}
tables=
for t in {{Tables}}; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$t'")" = 1 ] && tables="$tables $t"
done
[ -n "$tables" ] || fail "No catalogue tables in $db."
label={{Label}}
file={{Folder}}/$db-catalogue-$(date +%Y%m%d-%H%M%S)${label:+-$label}.sql.gz
umask 077
mariadb-dump --single-transaction --quick --hex-blob --add-drop-table "$db" $tables | gzip -6 > "$file" || { rm -f "$file"; fail "mariadb-dump stopped (see above)."; }
gzip -t "$file" || fail "The backup $file is damaged."
echo "FILE=$(basename "$file")"
"#;

/// The script that dumps the catalogue tables that exist to a new backup file and prints `FILE=<name>`.
pub fn make_backup_script(label: &str) -> String {
    crate::script::fill(
        MAKE_BACKUP,
        &[
            ("DatabaseName", &crate::server_tools::database_name()),
            ("Folder", &crate::server::shell_quote(&backup_folder())),
            ("Tables", &backup_tables_shell()),
            ("Label", &crate::server::shell_quote(&backup_label(label))),
        ],
    )
}

/// The script that lists the backups, newest first: `name|bytes|unix time` per line.
pub fn list_backups_script() -> String {
    format!(
        "d={}\n[ -d \"$d\" ] || exit 0\nfind \"$d\" -maxdepth 1 -type f -name '*-catalogue-*.sql.gz' -printf '%f|%s|%T@\\n' | sort -t'|' -k3 -rn\n",
        crate::server::shell_quote(&backup_folder())
    )
}

pub fn parse_backups(output: &str) -> Vec<CatalogBackup> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().split('|');
            let file = parts.next()?.to_string();
            let bytes = parts.next()?.parse().ok()?;
            let made = parts.next()?.split('.').next()?.parse().ok()?;
            valid_backup_file(&file).then_some(CatalogBackup { file, bytes, made })
        })
        .collect()
}

const RESTORE_BACKUP: &str = r#"set -o pipefail
{{DatabaseName}}
f={{Folder}}/{{File}}
[ -s "$f" ] || fail "The backup $f is missing."
gzip -t "$f" || fail "The backup $f is damaged."
echo "> Safety backup of the catalogue first"
tables=
for t in {{Tables}}; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$t'")" = 1 ] && tables="$tables $t"
done
safety={{Folder}}/$db-catalogue-$(date +%Y%m%d-%H%M%S)-before-rollback.sql.gz
(umask 077; mariadb-dump --single-transaction --quick --hex-blob --add-drop-table "$db" $tables | gzip -6 > "$safety") || fail "The safety backup failed: nothing was changed."
echo "SAFETY=$(basename "$safety")"
echo "> Putting $(basename "$f") back"
gunzip -c "$f" | mariadb "$db" || fail "The rollback stopped (see above); the safety backup is $safety."
echo "DONE=1"
"#;

/// The script that makes a safety backup, then loads the chosen backup into the hotel database.
pub fn restore_backup_script(file: &str) -> Option<String> {
    valid_backup_file(file).then(|| {
        crate::script::fill(
            RESTORE_BACKUP,
            &[
                ("DatabaseName", &crate::server_tools::database_name()),
                ("Folder", &crate::server::shell_quote(&backup_folder())),
                ("File", &crate::server::shell_quote(file)),
                ("Tables", &backup_tables_shell()),
            ],
        )
    })
}

pub fn delete_backup_script(file: &str) -> Option<String> {
    valid_backup_file(file).then(|| format!("rm -f -- {}/{}\n", crate::server::shell_quote(&backup_folder()), crate::server::shell_quote(file)))
}

fn value_of(output: &str, key: &str) -> Option<String> {
    output.lines().find_map(|l| l.trim().strip_prefix(&format!("{key}="))).map(str::to_string)
}

async fn run_root(server: &dyn Server, script: &str, what: &str) -> Result<String> {
    let result = server.run_as_root(&crate::script::with_prelude(script)).await?;
    if !result.succeeded() {
        let reason = result.output.lines().find_map(|l| l.strip_prefix("ERROR: ")).map(str::to_string).unwrap_or_else(|| result.error.trim().to_string());
        return Err(ServerError(format!("{what}: {reason}")).into());
    }
    Ok(result.output)
}

pub async fn list_backups(server: &dyn Server) -> Result<Vec<CatalogBackup>> {
    Ok(parse_backups(&run_root(server, &list_backups_script(), "Listing the catalogue backups failed").await?))
}

/// Makes a backup and returns its file name.
pub async fn make_backup(server: &dyn Server, label: &str) -> Result<String> {
    let output = run_root(server, &make_backup_script(label), "The catalogue backup failed").await?;
    value_of(&output, "FILE").ok_or_else(|| ServerError("The catalogue backup did not say its file.".into()).into())
}

/// Puts the backup back after a safety backup; returns the safety backup's file name.
pub async fn restore_backup(server: &dyn Server, file: &str) -> Result<String> {
    let script = restore_backup_script(file).ok_or_else(|| ServerError(format!("Not a catalogue backup: {file}")))?;
    let output = run_root(server, &script, "The rollback failed").await?;
    if value_of(&output, "DONE").is_none() {
        return Err(ServerError("The rollback did not finish.".into()).into());
    }
    Ok(value_of(&output, "SAFETY").unwrap_or_default())
}

pub async fn delete_backup(server: &dyn Server, file: &str) -> Result<()> {
    let script = delete_backup_script(file).ok_or_else(|| ServerError(format!("Not a catalogue backup: {file}")))?;
    run_root(server, &script, "Deleting the backup failed").await.map(|_| ())
}

// ---- Page builder and page merge -----------------------------------------------------------------------------

pub const MAX_BUILDER_FURNI: i64 = 1000;

/// Floor and wall furni whose class name starts with the text (`?`: the pattern), at most [`MAX_BUILDER_FURNI`].
pub fn furni_by_prefix_sql(database: &str) -> String {
    format!(
        "SELECT id, item_name, public_name, type, sprite_id FROM {}.items_base WHERE type IN ('s', 'i') AND item_name LIKE ? ORDER BY item_name LIMIT {MAX_BUILDER_FURNI}",
        quote(database)
    )
}

/// The LIKE pattern for a class name prefix, with `%` and `_` matched as themselves.
pub fn prefix_pattern(prefix: &str) -> String {
    format!("{}%", prefix.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// At least three characters, so a prefix never takes half the furni.
pub async fn furni_by_prefix(session: &SqlSession, database: &str, prefix: &str) -> Result<Vec<Furni>> {
    if prefix.trim().chars().count() < 3 {
        return Ok(Vec::new());
    }
    let rows = session.select(&furni_by_prefix_sql(database), vec![SqlValue::Text(prefix_pattern(prefix))]).await?;
    Ok(rows.iter().map(|row| furni_from_row(row)).collect())
}

/// The statements that merge `sources` into `target`: their offers, sub-pages and front page tiles go to the target,
/// then they are deleted. Offers keep their order numbers.
pub fn merge_pages_statements(database: &str, target: i64, sources: &[i64]) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    let mut statements = Vec::new();
    for source in sources.iter().filter(|s| **s != target) {
        let pair = || vec![SqlValue::Int(target), SqlValue::Int(*source)];
        statements.push((format!("UPDATE {db}.catalog_items SET page_id = ? WHERE page_id = ?"), pair()));
        statements.push((format!("UPDATE {db}.catalog_pages SET parent_id = ? WHERE parent_id = ?"), pair()));
        statements.push((format!("UPDATE {db}.catalog_featured_pages SET page_id = ? WHERE page_id = ?"), pair()));
        statements.push((format!("DELETE FROM {db}.catalog_pages WHERE id = ?"), vec![SqlValue::Int(*source)]));
    }
    statements
}

/// Why the pages cannot be merged into `target`, or None: the target must not sit under one of them.
pub fn merge_problem(pages: &[Page], target: i64, sources: &[i64]) -> Option<String> {
    if !pages.iter().any(|p| p.id == target) {
        return Some(format!("There is no page {target}."));
    }
    sources
        .iter()
        .find(|source| **source != target && is_ancestor(pages, **source, target))
        .map(|source| format!("Page {target} is under page {source}: it cannot take it in."))
}

pub async fn merge_pages(session: &SqlSession, database: &str, target: i64, sources: &[i64]) -> Result<u64> {
    Ok(session.execute(&merge_pages_statements(database, target, sources)).await?.0)
}

/// The script that finds the hotel database the emulator uses (config.ini) and prints its name.
pub fn database_script() -> String {
    format!("{}\nprintf '%s\\n' \"$db\"\n", crate::server_tools::database_name())
}

/// The hotel database the emulator on the server uses.
pub async fn hotel_database(server: &dyn Server) -> Result<String> {
    let result = server.run_as_root(&database_script()).await?;
    let name = result.output.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
    if !result.succeeded() || name.is_empty() {
        return Err(ServerError("Could not find the hotel database in the emulator's config.ini.".into()).into());
    }
    Ok(name)
}

/// The RCON request that makes the emulator load the catalogue again and tell the players online.
pub const RELOAD_REQUEST: &str = r#"{"key":"updatecatalog","data":{}}"#;

/// Sends [`RELOAD_REQUEST`] to the emulator's RCON port (config.ini `rcon.port`, default 3001) on 127.0.0.1 of the
/// server, and prints the answer.
pub fn reload_script() -> String {
    crate::rcon::script(RELOAD_REQUEST)
}

/// Whether the emulator's RCON answer says the request worked (`"status":0`).
pub fn reload_succeeded(answer: &str) -> bool {
    crate::rcon::succeeded(answer)
}

/// Makes the running emulator load the catalogue again; the players online are told to refresh it.
pub async fn reload_catalog(server: &dyn Server) -> Result<()> {
    let answer = crate::rcon::send(server, RELOAD_REQUEST).await?;
    if answer.ok() {
        return Ok(());
    }
    Err(ServerError(format!("The emulator did not reload the catalogue: {}", if answer.message.is_empty() { format!("status {}", answer.status) } else { answer.message })).into())
}

// ---- Catalogue explorer: offers dragged onto a page --------------------------------------------------------

/// A page's offer ids after `dragged` are put before `before` (None, or an id not on the page: at the end). An id
/// already on the page moves; the others are added. Every id once.
pub fn arranged(current: &[i64], dragged: &[i64], before: Option<i64>) -> Vec<i64> {
    let mut moving = Vec::new();
    for id in dragged {
        if !moving.contains(id) {
            moving.push(*id);
        }
    }
    // Dropped before one of the dragged offers themselves: before the first offer after it that stays.
    let anchor = before.and_then(|b| {
        let from = current.iter().position(|id| *id == b)?;
        current[from..].iter().copied().find(|id| !moving.contains(id))
    });
    let mut order: Vec<i64> = current.iter().copied().filter(|id| !moving.contains(id)).collect();
    let at = anchor.and_then(|a| order.iter().position(|id| *id == a)).unwrap_or(order.len());
    order.splice(at..at, moving);
    order
}

/// Puts the offers of `order` on `page_id`, numbered 1, 2, 3 ... in that order.
pub fn arrange_statements(database: &str, page_id: i64, order: &[i64]) -> Vec<(String, Vec<SqlValue>)> {
    let sql = format!("UPDATE {}.catalog_items SET page_id = ?, order_number = ? WHERE id = ?", quote(database));
    order.iter().enumerate().map(|(i, id)| (sql.clone(), vec![SqlValue::Int(page_id), SqlValue::Int(i as i64 + 1), SqlValue::Int(*id)])).collect()
}

/// What a drop changed, to put it back: each offer's page and number before, and the copies it added.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaceUndo {
    pub before: Vec<(i64, i64, i64)>,
    pub inserted: Vec<i64>,
    /// The pages whose offers changed.
    pub pages: Vec<i64>,
}

/// The statements that undo a drop: the copies removed, every offer back on its page with its number.
pub fn undo_statements(database: &str, undo: &PlaceUndo) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    let mut statements: Vec<(String, Vec<SqlValue>)> =
        undo.inserted.iter().map(|id| (format!("DELETE FROM {db}.catalog_items WHERE id = ?"), vec![SqlValue::Int(*id)])).collect();
    statements.extend(undo.before.iter().map(|(id, page, number)| {
        (format!("UPDATE {db}.catalog_items SET page_id = ?, order_number = ? WHERE id = ?"), vec![SqlValue::Int(*page), SqlValue::Int(*number), SqlValue::Int(*id)])
    }));
    statements
}

/// The offers of these ids.
pub fn offers_by_ids_sql(database: &str, count: usize) -> String {
    let marks = vec!["?"; count.max(1)].join(", ");
    format!("SELECT {OFFER_COLUMNS} FROM {}.catalog_items WHERE id IN ({marks}) ORDER BY id", quote(database))
}

/// Moves `ids` (or, with `copy`, copies of them) onto `page_id` before the offer `before` (None: at the end), and
/// numbers the page's offers again, in one transaction. The page and the offers are read again first, so what
/// others changed meanwhile counts. Returns what undoes it.
pub async fn place_offers(session: &SqlSession, database: &str, ids: &[i64], page_id: i64, before: Option<i64>, copy: bool) -> Result<PlaceUndo> {
    if ids.is_empty() {
        return Ok(PlaceUndo::default());
    }
    let target = load_offers(session, database, page_id).await?;
    let rows = session.select(&offers_by_ids_sql(database, ids.len()), ids.iter().map(|id| SqlValue::Int(*id)).collect()).await?;
    let dragged: Vec<Offer> = rows.iter().map(|row| offer_from_row(row)).collect();
    // In the order they were dragged.
    let dragged: Vec<Offer> = ids.iter().filter_map(|id| dragged.iter().find(|o| o.id == *id).cloned()).collect();
    let mut inserted = Vec::new();
    let placed: Vec<i64> = if copy {
        for offer in &dragged {
            let copy = Offer { id: 0, page_id, ..offer.clone() };
            match save_offer(session, database, &copy).await {
                Ok(id) => inserted.push(id),
                Err(error) => {
                    let _ = session.execute(&undo_statements(database, &PlaceUndo { inserted, ..PlaceUndo::default() })).await;
                    return Err(error);
                }
            }
        }
        inserted.clone()
    } else {
        dragged.iter().map(|o| o.id).collect()
    };
    let order = arranged(&target.iter().map(|o| o.id).collect::<Vec<_>>(), &placed, before);
    let mut undo = PlaceUndo { inserted: inserted.clone(), pages: vec![page_id], ..PlaceUndo::default() };
    for offer in target.iter().chain(dragged.iter().filter(|_| !copy)) {
        if !undo.before.iter().any(|(id, _, _)| *id == offer.id) {
            undo.before.push((offer.id, offer.page_id, offer.order_number));
        }
        if !undo.pages.contains(&offer.page_id) {
            undo.pages.push(offer.page_id);
        }
    }
    if let Err(error) = session.execute(&arrange_statements(database, page_id, &order)).await {
        let _ = session.execute(&undo_statements(database, &PlaceUndo { inserted, ..PlaceUndo::default() })).await;
        return Err(error);
    }
    Ok(undo)
}

/// Puts a drop back (see [`place_offers`]), in one transaction.
pub async fn undo_place(session: &SqlSession, database: &str, undo: &PlaceUndo) -> Result<u64> {
    Ok(session.execute(&undo_statements(database, undo)).await?.0)
}

/// Gives `furni` (no offer sells them yet) an offer each on `page_id`, before the offer `before` (None: at the end):
/// named after the class name, 3 credits, as a new offer starts. Returns what undoes it (the new offers removed).
pub async fn place_new_furni(session: &SqlSession, database: &str, furni: &[Furni], page_id: i64, before: Option<i64>) -> Result<PlaceUndo> {
    let mut inserted = Vec::new();
    for offer in offers_for(furni, page_id, &[], 3, 0, 0) {
        match save_offer(session, database, &offer).await {
            Ok(id) => inserted.push(id),
            Err(error) => {
                let _ = session.execute(&undo_statements(database, &PlaceUndo { inserted, ..PlaceUndo::default() })).await;
                return Err(error);
            }
        }
    }
    match place_offers(session, database, &inserted, page_id, before, false).await {
        Ok(mut undo) => {
            undo.before.retain(|(id, _, _)| !inserted.contains(id));
            undo.inserted = inserted;
            Ok(undo)
        }
        Err(error) => {
            let _ = session.execute(&undo_statements(database, &PlaceUndo { inserted, ..PlaceUndo::default() })).await;
            Err(error)
        }
    }
}

/// What a page move changed, to put it back: each page's parent and order before.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageUndo {
    pub before: Vec<(i64, i64, i64)>,
}

/// Why `page` cannot go under `parent`, or None: never under itself or one of its own pages.
pub fn page_move_problem(pages: &[Page], page: i64, parent: i64) -> Option<String> {
    if page == parent || is_ancestor(pages, page, parent) {
        return Some("A page cannot go under itself or one of its own pages.".into());
    }
    None
}

/// Puts the pages of `order` under `parent`, numbered 1, 2, 3 ... in that order.
pub fn page_order_statements(database: &str, parent: i64, order: &[i64]) -> Vec<(String, Vec<SqlValue>)> {
    let sql = format!("UPDATE {}.catalog_pages SET parent_id = ?, order_num = ? WHERE id = ?", quote(database));
    order.iter().enumerate().map(|(i, id)| (sql.clone(), vec![SqlValue::Int(parent), SqlValue::Int(i as i64 + 1), SqlValue::Int(*id)])).collect()
}

/// Moves `page` under `parent`, before the page `before` (None: last), and numbers the pages under `parent` again, in
/// one transaction. The pages are read again first. Returns what undoes it.
pub async fn move_page(session: &SqlSession, database: &str, page: i64, parent: i64, before: Option<i64>) -> Result<PageUndo> {
    let pages = load_pages(session, database).await?;
    if let Some(problem) = page_move_problem(&pages, page, parent) {
        return Err(ServerError(problem).into());
    }
    let mut siblings: Vec<&Page> = pages.iter().filter(|p| p.parent_id == parent && p.id != parent).collect();
    siblings.sort_by_key(|p| (p.order_num, p.id));
    let order = arranged(&siblings.iter().map(|p| p.id).collect::<Vec<_>>(), &[page], before);
    let before: Vec<(i64, i64, i64)> = order.iter().filter_map(|id| pages.iter().find(|p| p.id == *id)).map(|p| (p.id, p.parent_id, p.order_num)).collect();
    session.execute(&page_order_statements(database, parent, &order)).await?;
    Ok(PageUndo { before })
}

/// Puts a page move back, in one transaction.
pub async fn undo_page_move(session: &SqlSession, database: &str, undo: &PageUndo) -> Result<u64> {
    let sql = format!("UPDATE {}.catalog_pages SET parent_id = ?, order_num = ? WHERE id = ?", quote(database));
    let statements: Vec<_> = undo.before.iter().map(|(id, parent, order)| (sql.clone(), vec![SqlValue::Int(*parent), SqlValue::Int(*order), SqlValue::Int(*id)])).collect();
    Ok(session.execute(&statements).await?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_are_arranged_like_a_drop() {
        // Onto another page: before an offer, or at the end.
        assert_eq!(arranged(&[1, 2, 3], &[9, 8], Some(2)), [1, 9, 8, 2, 3]);
        assert_eq!(arranged(&[1, 2, 3], &[9], None), [1, 2, 3, 9]);
        assert_eq!(arranged(&[1, 2, 3], &[9], Some(77)), [1, 2, 3, 9], "not on the page: at the end");
        // Within the page: 3 moves before 1; 1 and 2 dropped before 2 itself land before 3.
        assert_eq!(arranged(&[1, 2, 3], &[3], Some(1)), [3, 1, 2]);
        assert_eq!(arranged(&[1, 2, 3, 4], &[1, 2], Some(2)), [1, 2, 3, 4]);
        assert_eq!(arranged(&[1, 2, 3, 4], &[4, 4], Some(2)), [1, 4, 2, 3], "every id once");
        assert_eq!(arranged(&[], &[5, 6], Some(1)), [5, 6]);
    }

    #[test]
    fn pages_move_but_never_under_themselves() {
        let pages = vec![page(1, -1, "Furni", 1), page(2, 1, "Area", 1), page(3, 2, "Area Blue", 1), page(4, 1, "Mode", 2)];
        assert!(page_move_problem(&pages, 2, 3).is_some(), "under its own page");
        assert!(page_move_problem(&pages, 2, 2).is_some(), "under itself");
        assert_eq!(page_move_problem(&pages, 3, 4), None);
        let statements = page_order_statements("hotel", 1, &[4, 2]);
        assert_eq!(statements[0].0, "UPDATE `hotel`.catalog_pages SET parent_id = ?, order_num = ? WHERE id = ?");
        assert_eq!(statements[1].1, vec![SqlValue::Int(1), SqlValue::Int(2), SqlValue::Int(2)]);
    }

    #[test]
    fn a_drop_is_written_and_undone() {
        let statements = arrange_statements("hotel", 7, &[3, 1]);
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].0, "UPDATE `hotel`.catalog_items SET page_id = ?, order_number = ? WHERE id = ?");
        assert_eq!(statements[1].1, vec![SqlValue::Int(7), SqlValue::Int(2), SqlValue::Int(1)]);
        let undo = PlaceUndo { before: vec![(3, 5, 4)], inserted: vec![40], pages: vec![7, 5] };
        let back = undo_statements("hotel", &undo);
        assert_eq!(back[0].0, "DELETE FROM `hotel`.catalog_items WHERE id = ?");
        assert_eq!(back[1].1, vec![SqlValue::Int(5), SqlValue::Int(4), SqlValue::Int(3)]);
    }

    fn page(id: i64, parent_id: i64, caption: &str, order_num: i64) -> Page {
        Page {
            id,
            parent_id,
            caption: caption.into(),
            layout: "default_3x3".into(),
            catalog_mode: "NORMAL".into(),
            order_num,
            visible: true,
            enabled: true,
            ..Page::default()
        }
    }

    fn offer() -> Offer {
        Offer { id: 1, page_id: 5, catalog_name: "throne".into(), item_ids: "230".into(), amount: 1, ..Offer::default() }
    }

    #[test]
    fn the_tree_puts_children_under_their_parent_in_order() {
        let pages = vec![page(1, -1, "Shop", 2), page(2, -1, "Front", 1), page(3, 1, "Rares", 1), page(4, 1, "Alpha", 1), page(5, 3, "Old", 0)];
        let rows: Vec<(i64, usize)> = tree(&pages).iter().map(|r| (pages[r.index].id, r.depth)).collect();

        assert_eq!(rows, vec![(2, 0), (1, 0), (4, 1), (3, 1), (5, 2)]);
    }

    #[test]
    fn a_page_without_its_parent_or_in_a_loop_still_shows() {
        let pages = vec![page(1, 99, "Lost", 0), page(2, 3, "A", 0), page(3, 2, "B", 0), page(4, 4, "Self", 0)];
        let mut ids: Vec<i64> = tree(&pages).iter().map(|r| pages[r.index].id).collect();
        ids.sort();

        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_page_cannot_go_under_itself() {
        let pages = vec![page(1, -1, "Shop", 0), page(2, 1, "Rares", 0), page(3, 2, "Old", 0)];
        let mut moved = pages[0].clone();
        moved.parent_id = 3;

        assert!(is_ancestor(&pages, 1, 3));
        assert!(!is_ancestor(&pages, 3, 1));
        assert_eq!(page_problem(&moved, &pages), Some("A page cannot be put under itself.".into()));
        assert_eq!(page_problem(&pages[2], &pages), None);
    }

    #[test]
    fn a_page_needs_a_known_layout_and_a_name() {
        let pages = vec![page(1, -1, "Shop", 0)];
        let mut p = pages[0].clone();
        p.layout = "nope".into();
        assert_eq!(page_problem(&p, &pages), Some("Unknown layout: nope".into()));
        p.layout = "frontpage".into();
        p.caption = " ".into();
        assert_eq!(page_problem(&p, &pages), Some("The page needs a name.".into()));
        p.caption = "Shop".into();
        p.parent_id = 7;
        assert_eq!(page_problem(&p, &pages), Some("There is no page 7 to put it under.".into()));
    }

    #[test]
    fn item_ids_are_numbers_or_bundles() {
        assert_eq!(item_ids("230"), Some(vec![230]));
        assert_eq!(item_ids("100;200; 300"), Some(vec![100, 200, 300]));
        assert_eq!(item_ids("100:2;200"), Some(vec![100, 200]));
        assert_eq!(item_ids(""), None);
        assert_eq!(item_ids("abc"), None);
        assert_eq!(item_ids("0"), None);
        assert_eq!(item_ids("5:0"), None);
    }

    #[test]
    fn an_offer_keeps_its_limited_sales() {
        assert_eq!(offer_problem(&offer()), None);
        let mut o = offer();
        o.limited_stack = 10;
        o.limited_sells = 12;
        assert_eq!(offer_problem(&o), Some("12 are sold already: the limited size cannot be smaller.".into()));
        o.limited_stack = 12;
        assert_eq!(offer_problem(&o), None);
        o.amount = 0;
        assert_eq!(offer_problem(&o), Some("The amount must be at least 1.".into()));
        let mut bad = offer();
        bad.item_ids = "x".into();
        assert!(offer_problem(&bad).is_some());
    }

    #[test]
    fn rows_turn_into_pages_and_offers() {
        let row = vec![
            SqlValue::Int(5),
            SqlValue::Int(-1),
            SqlValue::Text("Rares".into()),
            SqlValue::Text("rares".into()),
            SqlValue::Text("default_3x3".into()),
            SqlValue::Int(3),
            SqlValue::Int(1),
            SqlValue::Int(2),
            SqlValue::Text("1".into()),
            SqlValue::Text("0".into()),
            SqlValue::Text("0".into()),
            SqlValue::Text("NORMAL".into()),
        ];
        let p = page_from_row(&row);
        assert_eq!((p.id, p.parent_id, p.caption.as_str(), p.visible, p.enabled), (5, -1, "Rares", true, false));
        assert_eq!(update_page_params(&p).len(), update_page_sql("hotel").matches('?').count());
        assert_eq!(update_offer_params(&offer()).len(), update_offer_sql("hotel").matches('?').count());
        assert!(pages_sql("my`db").contains("`my``db`.catalog_pages"));
    }

    #[test]
    fn new_pages_and_offers_go_after_the_others() {
        let pages = vec![page(1, -1, "Shop", 4), page(2, 1, "Rares", 7), page(3, 1, "Old", 2)];
        let new = new_page(&pages, 1);
        assert_eq!((new.id, new.parent_id, new.order_num, new.visible), (0, 1, 8, false));
        assert_eq!(new_page(&pages, -1).order_num, 5);
        assert_eq!(new_page(&[], -1).order_num, 1);
        assert_eq!(page_problem(&new, &pages), None);

        let mut first = offer();
        first.order_number = 3;
        let made = new_offer(&[first], 5);
        assert_eq!((made.id, made.page_id, made.order_number, made.amount), (0, 5, 4, 1));
        assert!(offer_problem(&made).is_some(), "the furni ids still have to be filled in");
        assert_eq!(insert_page_params(&new).len(), insert_page_sql("hotel").matches('?').count());
        assert_eq!(insert_offer_params(&made).len(), insert_offer_sql("hotel").matches('?').count());
    }

    #[test]
    fn deleting_keeps_sub_pages_and_limited_sales() {
        let pages = vec![page(1, -1, "Shop", 0), page(2, 1, "Rares", 0)];
        let mut sold = offer();
        sold.page_id = 2;
        sold.limited_stack = 10;
        sold.limited_sells = 3;

        assert_eq!(page_delete_problem(&pages, &[], 1), Some("The page has 1 sub-pages: move or delete them first.".into()));
        assert_eq!(page_delete_problem(&pages, &[offer()], 2), None);
        assert!(page_delete_problem(&pages, std::slice::from_ref(&sold), 2).is_some());
        assert_eq!(offer_delete_problem(&offer()), None);
        assert_eq!(offer_delete_problem(&sold), Some("3 limited numbers are sold: the offer cannot be deleted.".into()));
    }

    fn slot(slot_id: i64, kind: &str) -> Featured {
        Featured { slot_id, kind: kind.into(), page_name: "pets".into(), page_id: 5, product_name: "throne".into(), expire_timestamp: -1, ..Featured::default() }
    }

    #[test]
    fn front_page_slots_follow_the_table() {
        assert_eq!(featured_problem(&[slot(1, "page_name"), slot(2, "page_id"), slot(3, "product_name")]), None);
        assert_eq!(featured_problem(&[]), None);
        assert_eq!(featured_problem(&[slot(1, "page_id"), slot(1, "page_id")]), Some("Slot 1 is there twice.".into()));
        assert_eq!(featured_problem(&[slot(0, "page_id")]), Some("Slot 0: the number must be 1 to 99.".into()));
        assert_eq!(featured_problem(&[slot(4, "nope")]), Some("Slot 4: unknown type nope.".into()));
        let mut empty = slot(5, "page_name");
        empty.page_name = " ".into();
        assert_eq!(featured_problem(&[empty]), Some("Slot 5: fill in what it opens.".into()));
        let mut long = slot(6, "page_id");
        long.image = "x".repeat(71);
        assert_eq!(featured_problem(&[long]), Some("Slot 6: a text is too long.".into()));
        assert_eq!(new_featured(&[slot(1, "page_id"), slot(3, "page_id")]).slot_id, 2);
    }

    #[test]
    fn expiry_dates_round_trip() {
        assert_eq!(parse_expiry(""), Some(-1));
        assert_eq!(parse_expiry("not a date"), None);
        assert_eq!(expiry_text(-1), "");
        let time = parse_expiry("2026-12-24 18:30").unwrap();
        assert_eq!(expiry_text(time), "2026-12-24 18:30");
        assert_eq!(expiry_text(parse_expiry("2026-12-24").unwrap()), "2026-12-24 00:00");
    }

    #[test]
    fn a_limited_series_only_grows_and_keeps_its_sales() {
        let series = LimitedSeries { offer_id: 12, catalog_name: "throne_ltd".into(), stack: 100, sold: 40, ..LimitedSeries::default() };
        assert_eq!(limited_problem(&series, LimitedAction::Grow(150)), None);
        assert_eq!(limited_problem(&series, LimitedAction::Grow(100)), Some("A series can only grow: more than 100.".into()));
        assert_eq!(limited_problem(&series, LimitedAction::Grow(2_000_000)), Some("At most 1000000.".into()));
        assert_eq!(limited_problem(&series, LimitedAction::End), Some("40 are sold: remove it with its sales instead.".into()));
        assert_eq!(limited_problem(&series, LimitedAction::RemoveWithSales), None);
        let unsold = LimitedSeries { sold: 0, ..series.clone() };
        assert_eq!(limited_problem(&unsold, LimitedAction::End), None);

        let remove = limited_statements("hotel", 12, LimitedAction::RemoveWithSales);
        assert_eq!(remove.len(), 2);
        assert!(remove[0].0.contains("DELETE FROM `hotel`.catalog_items_limited"));
        for (sql, params) in remove.iter().chain(limited_statements("hotel", 12, LimitedAction::Grow(150)).iter()) {
            assert_eq!(sql.matches('?').count(), params.len());
        }
    }

    fn voucher(id: i64, code: &str) -> Voucher {
        Voucher { id, code: code.into(), credits: 100, amount: 1, limit: -1, ..Voucher::default() }
    }

    #[test]
    fn a_voucher_has_a_unique_code_and_gives_something() {
        let others = vec![voucher(1, "SUMMER")];
        assert_eq!(voucher_problem(&voucher(0, "WINTER"), &others), None);
        assert_eq!(voucher_problem(&voucher(1, "SUMMER"), &others), None, "saving itself is fine");
        assert_eq!(voucher_problem(&voucher(0, "SUMMER"), &others), Some("The code SUMMER is used already.".into()));
        assert!(voucher_problem(&voucher(0, "WAY_TOO_LONG"), &others).is_some());
        assert!(voucher_problem(&voucher(0, "with space"), &others).is_some());
        assert!(voucher_problem(&voucher(0, ""), &others).is_some());
        let nothing = Voucher { credits: 0, ..voucher(0, "EMPTY") };
        assert_eq!(voucher_problem(&nothing, &others), Some("The voucher gives nothing: set credits, points or an offer.".into()));
        let furni = Voucher { credits: 0, catalog_item_id: 230, ..voucher(0, "THRONE") };
        assert_eq!(voucher_problem(&furni, &others), None);
        assert_eq!(update_voucher_sql("hotel").matches('?').count(), 8);
        assert_eq!(insert_voucher_sql("hotel").matches('?').count(), 7);
    }

    #[test]
    fn generated_codes_are_new_and_readable() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let codes = generate_voucher_codes("xmas", 10, 50, &["XMASAAAAAA".into()], &mut rng).unwrap();
        assert_eq!(codes.len(), 50);
        assert!(codes.iter().all(|c| c.len() == 10 && c.starts_with("XMAS") && valid_voucher_code(c)));
        assert!(codes.iter().all(|c| c[4..].chars().all(|ch| VOUCHER_ALPHABET.contains(ch))));
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), 50);
        assert!(generate_voucher_codes("TOOLONGPRE", 10, 1, &[], &mut rng).is_none(), "fewer than 4 random characters");
        assert!(generate_voucher_codes("", 11, 1, &[], &mut rng).is_none());
        assert!(generate_voucher_codes("", 8, 501, &[], &mut rng).is_none());
        assert!(generate_voucher_codes("a b", 10, 1, &[], &mut rng).is_none());
    }

    #[test]
    fn the_furni_search_matches_ids_and_names_literally() {
        let params = furni_search_params(" throne ");
        assert_eq!(params[0], SqlValue::Int(-1));
        assert_eq!(params[1], SqlValue::Text("%throne%".into()));
        assert_eq!(furni_search_params("230")[0], SqlValue::Int(230));
        assert_eq!(furni_search_params("50%_off")[1], SqlValue::Text("%50\\%\\_off%".into()));
        assert_eq!(furni_search_sql("hotel").matches('?').count(), params.len());
        assert_eq!(furni_by_ids_sql("hotel", 3).matches('?').count(), 3);
    }

    #[test]
    fn furni_ids_are_added_once_and_removed() {
        assert_eq!(add_furni_id("", 230), "230");
        assert_eq!(add_furni_id("230", 12), "230;12");
        assert_eq!(add_furni_id("230;12", 230), "230;12");
        assert_eq!(add_furni_id("230:2", 230), "230:2");
        assert_eq!(remove_furni_id("230;12;5:2", 12), "230;5:2");
        assert_eq!(remove_furni_id("230;5:2", 5), "230");
    }

    fn offer_of(id: i64, page_id: i64, name: &str, item_ids: &str) -> Offer {
        Offer { id, page_id, catalog_name: name.into(), item_ids: item_ids.into(), amount: 1, ..Offer::default() }
    }

    #[test]
    fn duplicate_offers_sell_the_same_furni() {
        let offers = vec![
            offer_of(1, 5, "throne", "230"),
            offer_of(2, 9, "throne_copy", "230"),
            offer_of(3, 5, "bundle", "1;2"),
            offer_of(4, 7, "bundle_again", "2; 1"),
            offer_of(5, 5, "chair", "12"),
            Offer { extradata: "Gold".into(), ..offer_of(6, 5, "trophy", "230") },
            offer_of(7, 5, "broken", ""),
            offer_of(8, 5, "broken2", ""),
            offer_of(9, 6, "throne3", "230"),
        ];
        let groups: Vec<Vec<i64>> = duplicate_offer_groups(&offers).iter().map(|g| g.iter().map(|i| offers[*i].id).collect()).collect();
        assert_eq!(groups, vec![vec![1, 2, 9], vec![3, 4]]);
    }

    #[test]
    fn wallpapers_floors_and_landscapes_differ_by_the_number_in_their_name() {
        let offers = vec![
            offer_of(1, 5, "wallpaper_single_102", "19896"),
            offer_of(2, 5, "wallpaper_single_103", "19896"),
            offer_of(3, 5, "floor_single_501", "19894"),
            offer_of(4, 5, "floor_single_502", "19894"),
            offer_of(5, 5, "landscape_single_1", "19895"),
            offer_of(6, 5, "landscape_single_2", "19895"),
            offer_of(7, 9, "wallpaper_single_102", "19896"),
        ];
        let groups: Vec<Vec<i64>> = duplicate_offer_groups(&offers).iter().map(|g| g.iter().map(|i| offers[*i].id).collect()).collect();
        assert_eq!(groups, vec![vec![1, 7]], "only the same pattern twice is a copy");
        assert_eq!(decoration_variant("wallpaper_single_1001"), Some("1001"));
        assert_eq!(decoration_variant("throne"), None);
    }

    #[test]
    fn duplicate_pages_share_an_internal_name() {
        let mut pages = vec![page(1, -1, "Shop", 0), page(2, 1, "Rares", 0), page(3, 1, "Rares 2", 0), page(4, 1, "Empty", 0)];
        pages[1].caption_save = "rares".into();
        pages[2].caption_save = "Rares ".into();
        assert_eq!(duplicate_page_groups(&pages), vec![vec![1, 2]]);
    }

    #[test]
    fn offers_sort_by_name() {
        let offers = vec![offer_of(10, 5, "zebra", "1"), offer_of(11, 5, "Apple", "2"), offer_of(12, 5, "apple", "3")];
        assert_eq!(sorted_by_name(&offers), vec![(11, 1), (12, 2), (10, 3)]);
    }

    #[test]
    fn unoffered_furni_are_in_no_offer() {
        let furni: Vec<Furni> = [(230, "throne"), (12, "chair"), (5, "lamp"), (7, "sofa")]
            .iter()
            .map(|(id, name)| Furni { id: *id, item_name: name.to_string(), kind: "s".into(), ..Furni::default() })
            .collect();
        let offers = vec![offer_of(1, 5, "throne", "230"), offer_of(2, 5, "bundle", "12:2;99")];
        let names: Vec<String> = unoffered_furni(&furni, &offers).iter().map(|f| f.item_name.clone()).collect();
        assert_eq!(names, vec!["lamp", "sofa"]);

        let mut last = offer_of(3, 9, "x", "1");
        last.order_number = 4;
        let made = offers_for(&unoffered_furni(&furni, &offers), 9, &[last], 5, 0, 0);
        assert_eq!(made.iter().map(|o| (o.catalog_name.as_str(), o.item_ids.as_str(), o.order_number, o.page_id)).collect::<Vec<_>>(), vec![("lamp", "5", 5, 9), ("sofa", "7", 6, 9)]);
        assert!(made.iter().all(|o| offer_problem(o).is_none()));
    }

    #[test]
    fn catalogue_backups_stay_in_their_folder() {
        assert_eq!(backup_label(" Before summer sale! "), "Before-summer-sale");
        assert_eq!(backup_label("../../etc"), "------etc".trim_matches('-'));
        assert!(valid_backup_file("hotel-catalogue-20261107-120000-before-sale.sql.gz"));
        assert!(!valid_backup_file("../hotel-catalogue-1.sql.gz"));
        assert!(!valid_backup_file("hotel-catalogue-1.sql.gz; rm -rf /"));
        assert!(!valid_backup_file("hotel-2026.sql.gz"), "a full database backup is not one of these");
        assert!(restore_backup_script("x; rm -rf /").is_none());
        assert!(delete_backup_script("../x-catalogue-.sql.gz").is_none());

        let script = make_backup_script("summer");
        assert!(script.contains("catalog_pages catalog_items catalog_featured_pages vouchers"));
        assert!(!script.contains("catalog_items_limited") && !script.contains("voucher_history"));
        assert!(script.contains("'summer'"));
        let restore = restore_backup_script("hotel-catalogue-20261107-120000.sql.gz").unwrap();
        assert!(restore.find("before-rollback").unwrap() < restore.find("gunzip -c").unwrap(), "the safety backup comes first");

        let list = parse_backups("hotel-catalogue-20261107-120000-sale.sql.gz|2048|1794045600.123\nbad line\nevil;x-catalogue-.sql.gz|1|2\n");
        assert_eq!(list, vec![CatalogBackup { file: "hotel-catalogue-20261107-120000-sale.sql.gz".into(), bytes: 2048, made: 1794045600 }]);
    }

    #[test]
    fn the_builder_matches_a_class_name_prefix_literally() {
        assert_eq!(prefix_pattern(" rare_dragon "), "rare\\_dragon%");
        assert_eq!(prefix_pattern("50%"), "50\\%%");
        assert_eq!(furni_by_prefix_sql("hotel").matches('?').count(), 1);
    }

    #[test]
    fn merging_moves_offers_sub_pages_and_tiles_then_deletes() {
        let statements = merge_pages_statements("hotel", 2, &[2, 7]);
        assert_eq!(statements.len(), 4, "the target is not merged into itself");
        assert!(statements[0].0.contains("catalog_items SET page_id"));
        assert!(statements[1].0.contains("catalog_pages SET parent_id"));
        assert!(statements[2].0.contains("catalog_featured_pages SET page_id"));
        assert!(statements[3].0.contains("DELETE FROM `hotel`.catalog_pages"));
        assert_eq!(statements[3].1, vec![SqlValue::Int(7)]);
        for (sql, params) in &statements {
            assert_eq!(sql.matches('?').count(), params.len());
        }

        let pages = vec![page(1, -1, "Shop", 0), page(2, 1, "Rares", 0), page(3, 2, "Old rares", 0)];
        assert_eq!(merge_problem(&pages, 2, &[2, 3]), None, "a sub-page merges into its parent");
        assert_eq!(merge_problem(&pages, 3, &[3, 2]), Some("Page 3 is under page 2: it cannot take it in.".into()));
        assert_eq!(merge_problem(&pages, 9, &[2]), Some("There is no page 9.".into()));
    }

    #[test]
    fn the_reload_asks_the_emulator_over_rcon() {
        let script = reload_script();
        assert!(script.contains(RELOAD_REQUEST));
        assert!(script.contains("/dev/tcp/127.0.0.1/$port"));
        assert!(reload_succeeded(r#"{"status":0,"message":""}"#));
        assert!(reload_succeeded("{ \"status\" : 0 }"));
        assert!(!reload_succeeded(r#"{"status":1,"message":"invalid request"}"#));
        assert!(!reload_succeeded(""));
    }
}
