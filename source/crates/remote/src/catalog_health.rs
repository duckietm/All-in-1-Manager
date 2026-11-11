//! The catalogue's health check: the problems the emulator's own check (Catalog Studio's `CatalogValidator`) finds,
//! for the normal catalogue and the Builders Club warehouse: offers on missing pages, furni ids that are wrong or not
//! in `items_base`, prices, currencies, song offers, limited sizes, missing parents, loops, layouts the emulator does
//! not know, sibling order numbers used twice, `includes` and visible pages under hidden ones.

use std::collections::{HashMap, HashSet};

use crate::catalog_admin::{self, Catalogue, Offer, Page};
use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The page layouts the emulator knows (`CatalogPageLayouts`); a page with another layout is not loaded.
pub const EMULATOR_LAYOUTS: &[&str] = &[
    "default_3x3",
    "guild_furni",
    "guilds",
    "guild_forum",
    "info_duckets",
    "info_rentables",
    "info_loyalty",
    "loyalty_vip_buy",
    "bots",
    "pets",
    "pets2",
    "pets3",
    "club_gift",
    "frontpage",
    "badge_display",
    "spaces_new",
    "spaces",
    "soundmachine",
    "info_pets",
    "club_buy",
    "roomads",
    "trophies",
    "single_bundle",
    "marketplace",
    "marketplace_own_items",
    "recycler",
    "recycler_info",
    "recycler_prizes",
    "sold_ltd_items",
    "plasto",
    "default_3x3_color_grouping",
    "recolorable_furniture",
    "recent_purchases",
    "room_bundle",
    "petcustomization",
    "root",
    "vip_buy",
    "frontpage_featured",
    "productpage1",
    "collectibles",
    "builders_club_addons",
    "builders_club_frontpage",
    "builders_club_loyalty",
    "monkey",
    "niko",
    "mad_money",
    "custom_prefix",
];

/// What a problem is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    OfferPageMissing,
    OfferItemInvalid,
    OfferItemMissing,
    OfferSongMissing,
    OfferPriceInvalid,
    OfferCurrencyUnknown,
    OfferLimitedBelowSales,
    PageParentMissing,
    PageCycle,
    PageLayoutUnknown,
    PageOrderTwice,
    PageIncludeInvalid,
    PageIncludeMissing,
    PageUnreachable,
}

impl Kind {
    /// The emulator's code for it.
    pub fn code(self) -> &'static str {
        match self {
            Kind::OfferPageMissing => "OFFER_PAGE_MISSING",
            Kind::OfferItemInvalid => "OFFER_ITEM_INVALID",
            Kind::OfferItemMissing => "OFFER_ITEM_MISSING",
            Kind::OfferSongMissing => "OFFER_SOUND_REFERENCE_MISSING",
            Kind::OfferPriceInvalid => "OFFER_PRICE_INVALID",
            Kind::OfferCurrencyUnknown => "OFFER_CURRENCY_UNSUPPORTED",
            Kind::OfferLimitedBelowSales => "OFFER_LIMITED_STACK_BELOW_SALES",
            Kind::PageParentMissing => "PAGE_PARENT_MISSING",
            Kind::PageCycle => "PAGE_CYCLE",
            Kind::PageLayoutUnknown => "PAGE_LAYOUT_UNSUPPORTED",
            Kind::PageOrderTwice => "PAGE_SIBLING_ORDER_DUPLICATE",
            Kind::PageIncludeInvalid => "PAGE_INCLUDE_INVALID",
            Kind::PageIncludeMissing => "PAGE_INCLUDE_MISSING",
            Kind::PageUnreachable => "PAGE_ANCESTOR_NOT_AVAILABLE",
        }
    }

    pub fn is_page(self) -> bool {
        matches!(
            self,
            Kind::PageParentMissing | Kind::PageCycle | Kind::PageLayoutUnknown | Kind::PageOrderTwice | Kind::PageIncludeInvalid | Kind::PageIncludeMissing | Kind::PageUnreachable
        )
    }
}

/// One problem: in which catalogue, on which page or offer (by id), and the value that is wrong (a parent id, a
/// layout, a furni id, a points type ...), which the view puts in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    pub cat: Catalogue,
    pub id: i64,
    /// The page's caption or the offer's name.
    pub name: String,
    pub value: String,
}

/// What the check reads besides the pages and offers.
#[derive(Debug, Clone, Default)]
pub struct Reference {
    /// Every items_base id.
    pub furni: HashSet<i64>,
    /// The points types the hotel uses (0 duckets always).
    pub currencies: HashSet<i64>,
}

/// One catalogue as the check reads it: its pages with their `includes`, its offers with their song id.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub pages: Vec<Page>,
    pub offers: Vec<Offer>,
    pub includes: HashMap<i64, String>,
    pub songs: HashMap<i64, i64>,
}

/// The problems of one catalogue, pages first, as the emulator's check finds them.
pub fn problems(cat: Catalogue, catalog: &Catalog, reference: &Reference) -> Vec<Problem> {
    let mut found = Vec::new();
    let pages: HashMap<i64, &Page> = catalog.pages.iter().map(|p| (p.id, p)).collect();
    let page_problem = |found: &mut Vec<Problem>, kind: Kind, page: &Page, value: String| {
        found.push(Problem { kind, cat, id: page.id, name: page.caption.clone(), value });
    };
    let mut orders: HashMap<(i64, i64), Vec<&Page>> = HashMap::new();
    for page in &catalog.pages {
        if page.parent_id > 0 && !pages.contains_key(&page.parent_id) {
            page_problem(&mut found, Kind::PageParentMissing, page, page.parent_id.to_string());
        }
        if !EMULATOR_LAYOUTS.contains(&page.layout.as_str()) {
            page_problem(&mut found, Kind::PageLayoutUnknown, page, page.layout.clone());
        }
        orders.entry((page.parent_id, page.order_num)).or_default().push(page);
        if let Some(includes) = catalog.includes.get(&page.id).filter(|i| !i.trim().is_empty()) {
            for raw in includes.split(';') {
                match raw.trim().parse::<i64>() {
                    Ok(id) if id > 0 => {
                        if !pages.contains_key(&id) {
                            page_problem(&mut found, Kind::PageIncludeMissing, page, id.to_string());
                        }
                    }
                    _ => page_problem(&mut found, Kind::PageIncludeInvalid, page, raw.trim().to_string()),
                }
            }
        }
        // Only reachable when every page above it is visible.
        if page.visible {
            let mut seen = HashSet::new();
            let mut parent = page.parent_id;
            while parent > 0 && seen.insert(parent) {
                let Some(above) = pages.get(&parent) else { break };
                if !above.visible {
                    page_problem(&mut found, Kind::PageUnreachable, page, above.id.to_string());
                    break;
                }
                parent = above.parent_id;
            }
        }
    }
    let mut twice: Vec<&Page> = orders.values().filter(|g| g.len() > 1).flatten().copied().collect();
    twice.sort_by_key(|p| p.id);
    for page in twice {
        page_problem(&mut found, Kind::PageOrderTwice, page, page.order_num.to_string());
    }
    // Loops in the tree: each page of a loop once.
    let mut in_loop = HashSet::new();
    for page in &catalog.pages {
        let mut path: Vec<i64> = Vec::new();
        let mut current = page.id;
        while current > 0 {
            let Some(at) = pages.get(&current) else { break };
            if let Some(start) = path.iter().position(|id| *id == current) {
                for id in &path[start..] {
                    if in_loop.insert(*id) {
                        if let Some(p) = pages.get(id) {
                            page_problem(&mut found, Kind::PageCycle, p, String::new());
                        }
                    }
                }
                break;
            }
            path.push(current);
            current = at.parent_id;
        }
    }

    for offer in &catalog.offers {
        let mut problem = |kind: Kind, value: String| found.push(Problem { kind, cat, id: offer.id, name: offer.catalog_name.clone(), value });
        if !pages.contains_key(&offer.page_id) {
            problem(Kind::OfferPageMissing, offer.page_id.to_string());
        }
        if offer.item_ids.trim().is_empty() {
            problem(Kind::OfferItemInvalid, String::new());
        } else {
            for token in offer.item_ids.split(';').filter(|t| !t.trim().is_empty()) {
                // `@1234`: what offer 1234 sells (Polaris' bundle references); it must be an offer of this catalogue.
                if let Some(reference) = token.trim().strip_prefix('@') {
                    match crate::bundle_admin::parse(token.trim()).and_then(|e| e.first().copied()) {
                        Some(entry) => {
                            if !catalog.offers.iter().any(|o| o.id == entry.id()) {
                                problem(Kind::OfferItemMissing, format!("@{}", entry.id()));
                            }
                        }
                        None => problem(Kind::OfferItemInvalid, format!("@{reference}")),
                    }
                    continue;
                }
                let parts: Vec<&str> = token.trim().split(':').collect();
                let id = parts.first().and_then(|p| p.trim().parse::<i64>().ok()).filter(|id| *id > 0);
                let amount_ok = parts.len() == 1 || (parts.len() == 2 && parts[1].trim().parse::<i64>().is_ok_and(|a| a > 0));
                match id {
                    Some(id) if amount_ok => {
                        if !reference.furni.is_empty() && !reference.furni.contains(&id) {
                            problem(Kind::OfferItemMissing, id.to_string());
                        }
                    }
                    _ => problem(Kind::OfferItemInvalid, token.trim().to_string()),
                }
            }
        }
        if offer.catalog_name.starts_with("SONG ") && (catalog.songs.get(&offer.id).copied().unwrap_or(0) <= 0 || offer.extradata.trim().is_empty()) {
            problem(Kind::OfferSongMissing, String::new());
        }
        if offer.cost_credits < 0 || offer.cost_points < 0 || offer.amount <= 0 {
            problem(Kind::OfferPriceInvalid, String::new());
        }
        if offer.cost_points > 0 && !reference.currencies.contains(&offer.points_type) {
            problem(Kind::OfferCurrencyUnknown, offer.points_type.to_string());
        }
        if offer.limited_stack < offer.limited_sells {
            problem(Kind::OfferLimitedBelowSales, format!("{}/{}", offer.limited_stack, offer.limited_sells));
        }
    }
    found
}

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Text(t)) => t.clone(),
        Some(SqlValue::Int(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// Reads one catalogue for the check.
pub async fn load_catalog(session: &SqlSession, database: &str, cat: Catalogue) -> Result<Catalog> {
    let pages = catalog_admin::load_pages(session, database, cat).await?;
    let offers = catalog_admin::load_all_offers(session, database, cat).await?;
    let db = quote(database);
    let mut catalog = Catalog { pages, offers, ..Catalog::default() };
    if cat == Catalogue::Normal {
        // Older databases may miss these columns: then there is nothing to check.
        if let Ok(rows) = session.select(&format!("SELECT id, includes FROM {db}.catalog_pages WHERE includes <> ''"), Vec::new()).await {
            catalog.includes = rows.iter().map(|r| (int(r.first()), text(r.get(1)))).collect();
        }
        if let Ok(rows) = session.select(&format!("SELECT id, song_id FROM {db}.catalog_items WHERE catalog_name LIKE 'SONG %'"), Vec::new()).await {
            catalog.songs = rows.iter().map(|r| (int(r.first()), int(r.get(1)))).collect();
        }
    }
    Ok(catalog)
}

/// The furni ids and the currencies the hotel uses (as the emulator reads them).
pub async fn load_reference(session: &SqlSession, database: &str) -> Result<Reference> {
    let db = quote(database);
    let furni = session.select(&format!("SELECT id FROM {db}.items_base"), Vec::new()).await?;
    let mut currencies: HashSet<i64> = HashSet::from([0]);
    if let Ok(rows) = session.select(&format!("SELECT DISTINCT type FROM {db}.users_currency"), Vec::new()).await {
        currencies.extend(rows.iter().map(|r| int(r.first())));
    }
    if let Ok(rows) = session.select(&format!("SELECT `value` FROM {db}.emulator_settings WHERE `key` LIKE '%points.type%'"), Vec::new()).await {
        currencies.extend(rows.iter().filter_map(|r| text(r.first()).trim().parse::<i64>().ok()));
    }
    Ok(Reference { furni: furni.iter().map(|r| int(r.first())).collect(), currencies })
}

/// The problems of both catalogues.
pub async fn check(session: &SqlSession, database: &str) -> Result<Vec<Problem>> {
    let reference = load_reference(session, database).await?;
    let mut found = Vec::new();
    for cat in [Catalogue::Normal, Catalogue::Builders] {
        match load_catalog(session, database, cat).await {
            Ok(catalog) => found.extend(problems(cat, &catalog, &reference)),
            // A hotel without the Builders Club tables has nothing to check there.
            Err(_) if cat == Catalogue::Builders => {}
            Err(error) => return Err(error),
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: i64, parent_id: i64, order_num: i64, visible: bool) -> Page {
        Page { id, parent_id, order_num, visible, layout: "default_3x3".into(), caption: format!("Page {id}"), ..Page::default() }
    }

    fn offer(id: i64, page_id: i64, item_ids: &str) -> Offer {
        Offer { id, page_id, item_ids: item_ids.into(), amount: 1, catalog_name: format!("offer_{id}"), ..Offer::default() }
    }

    fn kinds(found: &[Problem]) -> Vec<(Kind, i64)> {
        let mut kinds: Vec<(Kind, i64)> = found.iter().map(|p| (p.kind, p.id)).collect();
        kinds.sort();
        kinds
    }

    #[test]
    fn a_healthy_catalogue_has_no_problems() {
        let catalog = Catalog { pages: vec![page(1, -1, 1, true), page(2, 1, 1, true)], offers: vec![offer(10, 2, "5;6:2")], ..Catalog::default() };
        let reference = Reference { furni: HashSet::from([5, 6]), currencies: HashSet::from([0]) };
        assert!(problems(Catalogue::Normal, &catalog, &reference).is_empty());
    }

    /// Polaris' bundle references: `@id` names another offer, which must exist in the same catalogue.
    #[test]
    fn bundle_references_must_name_an_offer() {
        let catalog = Catalog { pages: vec![page(1, -1, 1, true)], offers: vec![offer(10, 1, "5"), offer(11, 1, "@10;@10:2;5:3"), offer(12, 1, "@99;@x")], ..Catalog::default() };
        let reference = Reference { furni: HashSet::from([5]), currencies: HashSet::from([0]) };
        let found = problems(Catalogue::Normal, &catalog, &reference);
        assert_eq!(kinds(&found), vec![(Kind::OfferItemInvalid, 12), (Kind::OfferItemMissing, 12)]);
        assert!(found.iter().any(|p| p.value == "@99"));
    }

    #[test]
    fn finds_what_the_emulator_finds() {
        let mut odd = page(4, 1, 2, true);
        odd.layout = "frontpage4".into();
        let mut loop_a = page(7, 8, 1, false);
        loop_a.parent_id = 8;
        let loop_b = page(8, 7, 1, false);
        let catalog = Catalog {
            pages: vec![page(1, -1, 1, false), page(2, 1, 1, true), page(3, 1, 1, true), odd, page(5, 99, 1, true), loop_a, loop_b],
            offers: vec![
                offer(10, 2, "5"),
                offer(11, 42, "5"),
                offer(12, 2, "abc"),
                offer(13, 2, "77"),
                Offer { cost_points: 5, points_type: 103, ..offer(14, 2, "5") },
                Offer { limited_stack: 2, limited_sells: 5, ..offer(15, 2, "5") },
                Offer { amount: 0, ..offer(16, 2, "5") },
                Offer { catalog_name: "SONG test".into(), ..offer(17, 2, "5") },
            ],
            includes: HashMap::from([(2, "3;x;55".into())]),
            ..Catalog::default()
        };
        let reference = Reference { furni: HashSet::from([5]), currencies: HashSet::from([0, 5]) };
        let found = problems(Catalogue::Normal, &catalog, &reference);
        let k = kinds(&found);
        for expected in [
            (Kind::PageParentMissing, 5),
            (Kind::PageLayoutUnknown, 4),
            (Kind::PageOrderTwice, 2),
            (Kind::PageOrderTwice, 3),
            (Kind::PageIncludeInvalid, 2),
            (Kind::PageIncludeMissing, 2),
            (Kind::PageUnreachable, 2),
            (Kind::PageCycle, 7),
            (Kind::PageCycle, 8),
            (Kind::OfferPageMissing, 11),
            (Kind::OfferItemInvalid, 12),
            (Kind::OfferItemMissing, 13),
            (Kind::OfferCurrencyUnknown, 14),
            (Kind::OfferLimitedBelowSales, 15),
            (Kind::OfferPriceInvalid, 16),
            (Kind::OfferSongMissing, 17),
        ] {
            assert!(k.contains(&expected), "{expected:?} not in {k:?}");
        }
        assert!(!k.iter().any(|(_, id)| *id == 10), "offer 10 is fine");
        assert_eq!(k.iter().filter(|(kind, _)| *kind == Kind::PageCycle).count(), 2);
    }
}
