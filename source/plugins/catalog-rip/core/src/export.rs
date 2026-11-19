//! What the rip produced, and how it is written: catalog.json, the Arcturus SQL and README.txt.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Write as _};

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};

use crate::furnidata::Furnidata;
use crate::packets::{CatalogIndex, CatalogPage, IndexNode, Offer, Product};

/// One catalogue mode (NORMAL or BUILDERS_CLUB) as ripped.
#[derive(Debug, Clone)]
pub struct RippedMode {
    pub mode: String,
    pub index: CatalogIndex,
    /// Page id -> page, for every page that answered.
    pub pages: HashMap<i32, CatalogPage>,
    /// Page ids that did not answer (after the retry).
    pub failed: Vec<i32>,
}

/// The whole rip.
#[derive(Debug, Clone)]
pub struct RippedCatalog {
    pub hotel: String,
    pub host: String,
    pub hotel_version: String,
    pub client_type: String,
    pub ripped_at: String,
    pub complete: bool,
    pub modes: Vec<RippedMode>,
    /// What made the files, for their headers ("Catalogue ripper plugin 0.1.0"; the built-in
    /// wrote "All-in-1 Manager <version>" there).
    pub generator: String,
}

/// One page of a mode's tree with the id it is written under.
#[derive(Debug, Clone, Copy)]
pub struct NumberedPage<'a> {
    /// The page id, or a new id for a page the index lists as disabled (-1).
    pub id: i32,
    pub parent_id: i32,
    /// 1-based position under the parent.
    pub order: usize,
    pub node: &'a IndexNode,
    /// The page content, when it answered.
    pub page: Option<&'a CatalogPage>,
}

/// The first id above every page id of `modes` (new ids for disabled pages start here).
pub fn first_free_page_id<'a>(modes: impl IntoIterator<Item = &'a RippedMode>) -> i32 {
    modes
        .into_iter()
        .flat_map(|m| m.index.walk())
        .map(|(n, _, _)| n.page_id)
        .max()
        .unwrap_or(0)
        .max(0)
        + 1
}

/// Every page below the root of a mode, depth first, with its parent's (possibly new) id.
/// Disabled pages (id -1) get ids from `next` upward, counted in `renumbered`. A page id that
/// is in the tree twice is listed twice; the writers keep the first.
pub fn numbered_pages<'a>(
    mode: &'a RippedMode,
    next: &mut i32,
    renumbered: &mut usize,
) -> Vec<NumberedPage<'a>> {
    fn rec<'a>(
        node: &'a IndexNode,
        parent: i32,
        mode: &'a RippedMode,
        out: &mut Vec<NumberedPage<'a>>,
        next: &mut i32,
        renumbered: &mut usize,
    ) {
        for (i, child) in node.children.iter().enumerate() {
            let id = if child.page_id > 0 {
                child.page_id
            } else {
                *renumbered += 1;
                *next += 1;
                *next - 1
            };
            out.push(NumberedPage {
                id,
                parent_id: parent,
                order: i + 1,
                node: child,
                page: mode
                    .pages
                    .get(&child.page_id)
                    .filter(|_| child.page_id > 0),
            });
            rec(child, id, mode, out, next, renumbered);
        }
    }
    let mut out = Vec::new();
    rec(&mode.index.root, -1, mode, &mut out, next, renumbered);
    out
}

// ---------------------------------------------------------------- json

fn node_json(node: &IndexNode, parent: i32, order: usize, ripped: &RippedMode) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(node.page_id));
    map.insert("parent_id".into(), json!(parent));
    map.insert("order".into(), json!(order));
    map.insert("name".into(), json!(node.name));
    map.insert("localization".into(), json!(node.localization));
    map.insert("icon".into(), json!(node.icon));
    map.insert("visible".into(), json!(node.visible));
    map.insert("enabled".into(), json!(node.page_id > 0));
    map.insert("index_offer_ids".into(), json!(node.offer_ids));
    match ripped.pages.get(&node.page_id) {
        Some(page) => {
            map.insert("layout".into(), json!(page.layout));
            map.insert("images".into(), json!(page.images));
            map.insert("texts".into(), json!(page.texts));
            if let Some(flag) = page.accept_season_currency_as_credits {
                map.insert("accept_season_currency_as_credits".into(), json!(flag));
            }
            map.insert("packet_layout".into(), json!(page.packet_layout));
            map.insert(
                "offers".into(),
                serde_json::to_value(&page.offers).unwrap_or(Value::Null),
            );
            map.insert(
                "frontpage_items".into(),
                serde_json::to_value(&page.frontpage_items).unwrap_or(Value::Null),
            );
        }
        None => {
            let status = if node.page_id <= 0 {
                "not requestable (disabled page, id -1)"
            } else if ripped.failed.contains(&node.page_id) {
                "no answer"
            } else {
                "not ripped"
            };
            map.insert("page_status".into(), json!(status));
        }
    }
    let children: Vec<Value> = node
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| node_json(c, node.page_id, i + 1, ripped))
        .collect();
    map.insert("children".into(), Value::Array(children));
    Value::Object(map)
}

/// catalog.json: the hotel, then per mode the full tree with every page's content merged in.
pub fn catalog_json(catalog: &RippedCatalog) -> Value {
    let modes: Vec<Value> = catalog
        .modes
        .iter()
        .map(|m| {
            let pages: Vec<Value> = m.index.root.children.iter().enumerate().map(|(i, c)| node_json(c, -1, i + 1, m)).collect();
            json!({
                "mode": m.mode,
                "new_additions_available": m.index.new_additions_available,
                "index_has_parent_ids": m.index.has_parent_ids,
                "root": { "name": m.index.root.name, "localization": m.index.root.localization, "icon": m.index.root.icon },
                "failed_pages": m.failed,
                "pages": pages,
            })
        })
        .collect();
    json!({
        "generator": format!("{} (G-Earth)", catalog.generator),
        "hotel": catalog.hotel,
        "host": catalog.host,
        "hotel_version": catalog.hotel_version,
        "client_type": catalog.client_type,
        "ripped_at": catalog.ripped_at,
        "complete": catalog.complete,
        "catalogs": modes,
    })
}

/// catalog.json as [`catalog_json`] makes it, serialized straight from the catalogue: the same
/// bytes (pretty-printed), without a JSON tree of the whole catalogue in memory. `extra`: one
/// more field at the end (`product_names`).
pub struct CatalogJson<'a, E> {
    pub catalog: &'a RippedCatalog,
    pub extra: Option<(&'a str, E)>,
}

impl<E: Serialize> Serialize for CatalogJson<'_, E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let c = self.catalog;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("generator", &format!("{} (G-Earth)", c.generator))?;
        map.serialize_entry("hotel", &c.hotel)?;
        map.serialize_entry("host", &c.host)?;
        map.serialize_entry("hotel_version", &c.hotel_version)?;
        map.serialize_entry("client_type", &c.client_type)?;
        map.serialize_entry("ripped_at", &c.ripped_at)?;
        map.serialize_entry("complete", &c.complete)?;
        map.serialize_entry("catalogs", &ModesJson(&c.modes))?;
        if let Some((key, value)) = &self.extra {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

struct ModesJson<'a>(&'a [RippedMode]);

impl Serialize for ModesJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for mode in self.0 {
            seq.serialize_element(&ModeJson(mode))?;
        }
        seq.end()
    }
}

struct ModeJson<'a>(&'a RippedMode);

impl Serialize for ModeJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let m = self.0;
        let root = &m.index.root;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("mode", &m.mode)?;
        map.serialize_entry("new_additions_available", &m.index.new_additions_available)?;
        map.serialize_entry("index_has_parent_ids", &m.index.has_parent_ids)?;
        map.serialize_entry("root", &RootJson(root))?;
        map.serialize_entry("failed_pages", &m.failed)?;
        map.serialize_entry(
            "pages",
            &ChildrenJson {
                node: root,
                parent: -1,
                ripped: m,
            },
        )?;
        map.end()
    }
}

struct RootJson<'a>(&'a IndexNode);

impl Serialize for RootJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("name", &self.0.name)?;
        map.serialize_entry("localization", &self.0.localization)?;
        map.serialize_entry("icon", &self.0.icon)?;
        map.end()
    }
}

/// The children of `node`, each with `parent` as its parent id.
struct ChildrenJson<'a> {
    node: &'a IndexNode,
    parent: i32,
    ripped: &'a RippedMode,
}

impl Serialize for ChildrenJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.node.children.len()))?;
        for (i, child) in self.node.children.iter().enumerate() {
            seq.serialize_element(&NodeJson {
                node: child,
                parent: self.parent,
                order: i + 1,
                ripped: self.ripped,
            })?;
        }
        seq.end()
    }
}

/// [`node_json`] as it serializes.
struct NodeJson<'a> {
    node: &'a IndexNode,
    parent: i32,
    order: usize,
    ripped: &'a RippedMode,
}

impl Serialize for NodeJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let node = self.node;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &node.page_id)?;
        map.serialize_entry("parent_id", &self.parent)?;
        map.serialize_entry("order", &self.order)?;
        map.serialize_entry("name", &node.name)?;
        map.serialize_entry("localization", &node.localization)?;
        map.serialize_entry("icon", &node.icon)?;
        map.serialize_entry("visible", &node.visible)?;
        map.serialize_entry("enabled", &(node.page_id > 0))?;
        map.serialize_entry("index_offer_ids", &node.offer_ids)?;
        match self.ripped.pages.get(&node.page_id) {
            Some(page) => {
                map.serialize_entry("layout", &page.layout)?;
                map.serialize_entry("images", &page.images)?;
                map.serialize_entry("texts", &page.texts)?;
                if let Some(flag) = page.accept_season_currency_as_credits {
                    map.serialize_entry("accept_season_currency_as_credits", &flag)?;
                }
                map.serialize_entry("packet_layout", &page.packet_layout)?;
                map.serialize_entry("offers", &page.offers)?;
                map.serialize_entry("frontpage_items", &page.frontpage_items)?;
            }
            None => {
                let status = if node.page_id <= 0 {
                    "not requestable (disabled page, id -1)"
                } else if self.ripped.failed.contains(&node.page_id) {
                    "no answer"
                } else {
                    "not ripped"
                };
                map.serialize_entry("page_status", status)?;
            }
        }
        map.serialize_entry(
            "children",
            &ChildrenJson {
                node,
                parent: node.page_id,
                ripped: self.ripped,
            },
        )?;
        map.end()
    }
}

// ---------------------------------------------------------------- sql helpers

/// A MariaDB/MySQL string literal: backslash escapes for \ ' " NUL newline CR and Ctrl-Z.
pub fn sql_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x1a' => out.push_str("\\Z"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Cuts a value to a column's character limit.
pub(crate) fn fit(value: &str, max_chars: usize, truncated: &mut usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        *truncated += 1;
        value.chars().take(max_chars).collect()
    }
}

/// A `-- comment` that cannot break out of its line.
pub(crate) fn comment(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// The `page_layout` enum of Arcturus' catalog_pages.
pub const ARCTURUS_LAYOUTS: &[&str] = &[
    // Added to the enum by Polaris' migration V20261014120000 (RecolorableFurnitureLayout).
    "recolorable_furniture",
    "default_3x3",
    "club_buy",
    "club_gift",
    "frontpage",
    "spaces",
    "recycler",
    "recycler_info",
    "recycler_prizes",
    "trophies",
    "plasto",
    "marketplace",
    "marketplace_own_items",
    "spaces_new",
    "soundmachine",
    "guilds",
    "guild_furni",
    "info_duckets",
    "info_rentables",
    "info_pets",
    "roomads",
    "single_bundle",
    "sold_ltd_items",
    "badge_display",
    "bots",
    "pets",
    "pets2",
    "pets3",
    "productpage1",
    "room_bundle",
    "recent_purchases",
    "default_3x3_color_grouping",
    "guild_forum",
    "vip_buy",
    "info_loyalty",
    "loyalty_vip_buy",
    "collectibles",
    "petcustomization",
    "frontpage_featured",
    "builders_club_frontpage",
    "builders_club_addons",
    "builders_club_loyalty",
    "root",
    "monkey",
    "niko",
    "mad_money",
];

/// The layout code the client got -> Arcturus' enum value (`None`: not in the enum, written
/// as default_3x3). The emulator's layout classes write some codes under another name.
pub fn arcturus_layout(code: &str) -> Option<&'static str> {
    let code = code.trim().to_ascii_lowercase();
    let mapped = match code.as_str() {
        "frontpage4" => "frontpage",
        "guild_frontpage" => "guilds",
        "guild_custom_furni" => "guild_furni",
        "club_gifts" => "club_gift",
        "loyalty_info" => "info_loyalty",
        other => other,
    };
    ARCTURUS_LAYOUTS.iter().copied().find(|l| *l == mapped)
}

/// The page columns the Arcturus layouts serialize from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageContent {
    pub headline: String,
    pub teaser: String,
    pub special: String,
    pub text1: String,
    pub text2: String,
    pub text_details: String,
    pub text_teaser: String,
}

#[derive(Clone, Copy)]
enum Slot {
    Headline,
    Teaser,
    Special,
    Text1,
    Text2,
    Details,
    TextTeaser,
    Skip,
}

/// Puts the images and texts a page sent back into the columns its Arcturus layout class
/// reads them from (the inverse of every `layouts/*Layout.serialize`). Extra images go to
/// page_special (`;`-joined), extra texts to page_text2 (newline-joined).
pub fn map_page_content(layout: &str, images: &[String], texts: &[String]) -> PageContent {
    use Slot::*;
    let three_images: &[Slot] = &[Headline, Teaser, Special];
    let two_images: &[Slot] = &[Headline, Teaser];
    let one_image: &[Slot] = &[Headline];
    let standard_texts: &[Slot] = &[Text1, Details, TextTeaser];
    let (image_slots, text_slots): (&[Slot], &[Slot]) =
        match layout.trim().to_ascii_lowercase().as_str() {
            "bots" => (two_images, &[Text1, Details, Text2]),
            "club_buy" => (two_images, &[]),
            "club_gifts" | "club_gift" | "info_duckets" | "info_loyalty" => (one_image, &[Text1]),
            "frontpage4" | "frontpage" => (two_images, &[Text1, Text2, TextTeaser]),
            "guild_forum" | "guild_frontpage" | "guilds" | "guild_custom_furni" | "guild_furni" => {
                (two_images, standard_texts)
            }
            "info_pets" => (two_images, &[Text1, Skip, TextTeaser]),
            "info_rentables" => {
                // InfoRentablesLayout splits page_text1 on "||".
                let mut content = PageContent {
                    headline: images.first().cloned().unwrap_or_default(),
                    text1: texts.join("||"),
                    ..Default::default()
                };
                if images.len() > 1 {
                    content.special = images[1..].join(";");
                }
                return content;
            }
            "frontpage_featured" => {
                // Header, then the teaser images, then the special images (each split on ';'); the
                // split point is not in the packet: the first goes to teaser, the rest to special.
                let mut content = map_page_content("default_3x3", &[], texts);
                content.headline = images.first().cloned().unwrap_or_default();
                content.teaser = images.get(1).cloned().unwrap_or_default();
                if images.len() > 2 {
                    content.special = images[2..].join(";");
                }
                return content;
            }
            "mad_money" | "roomads" => (two_images, &[Text1, Text2]),
            "marketplace" | "marketplace_own_items" => (&[], &[]),
            "pets" | "pets2" | "pets3" | "productpage1" => {
                (two_images, &[Text1, Text2, Details, TextTeaser])
            }
            "recycler" => (two_images, &[Text1]),
            "room_bundle" | "single_bundle" => (
                &[Headline, Teaser, Skip],
                &[Text1, Details, TextTeaser, Text2],
            ),
            "sold_ltd_items" => (three_images, &[]),
            "soundmachine" => (two_images, &[Text1, Details]),
            _ => (three_images, &[Text1, Details, TextTeaser, Text2]),
        };
    let mut content = PageContent::default();
    let mut extra_images = Vec::new();
    let mut extra_texts = Vec::new();
    let mut put = |slot: Slot, value: &str, extra: &mut Vec<String>| {
        let target = match slot {
            Headline => &mut content.headline,
            Teaser => &mut content.teaser,
            Special => &mut content.special,
            Text1 => &mut content.text1,
            Text2 => &mut content.text2,
            Details => &mut content.text_details,
            TextTeaser => &mut content.text_teaser,
            Skip => return,
        };
        if target.is_empty() {
            *target = value.to_string();
        } else if !value.is_empty() {
            extra.push(value.to_string());
        }
    };
    for (i, image) in images.iter().enumerate() {
        match image_slots.get(i) {
            Some(slot) => put(*slot, image, &mut extra_images),
            None if !image.is_empty() => extra_images.push(image.clone()),
            None => {}
        }
    }
    for (i, text) in texts.iter().enumerate() {
        match text_slots.get(i) {
            Some(slot) => put(*slot, text, &mut extra_texts),
            None if !text.is_empty() => extra_texts.push(text.clone()),
            None => {}
        }
    }
    if !extra_images.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !content.special.is_empty() {
            parts.push(content.special.clone());
        }
        parts.extend(extra_images);
        content.special = parts.join(";");
    }
    if !extra_texts.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !content.text2.is_empty() {
            parts.push(content.text2.clone());
        }
        parts.extend(extra_texts);
        content.text2 = parts.join("\n");
    }
    content
}

// ---------------------------------------------------------------- items_base

/// items_base rows of the configured database, to turn product class ids into item ids.
#[derive(Debug, Clone, Default)]
pub struct ItemsBase {
    /// (type, sprite_id) -> lowest id.
    pub by_sprite: HashMap<(String, i32), u32>,
    /// effect_id_male -> lowest id of a type 'e' item.
    pub by_effect: HashMap<i32, u32>,
    /// badge code (item_name of a type 'b' item) -> lowest id.
    pub badges: HashMap<String, u32>,
    /// (type, item_name in lower case) -> lowest id: for class names from a furnidata.
    pub by_name: HashMap<(String, String), u32>,
}

impl ItemsBase {
    pub fn add(
        &mut self,
        id: u32,
        sprite_id: i32,
        item_type: &str,
        item_name: &str,
        effect_id: i32,
    ) {
        let item_type = item_type.trim().to_ascii_lowercase();
        let keep_lowest = |slot: &mut u32| *slot = (*slot).min(id);
        if item_type == "b" {
            keep_lowest(self.badges.entry(item_name.to_string()).or_insert(id));
        }
        if item_type == "e" && effect_id > 0 {
            keep_lowest(self.by_effect.entry(effect_id).or_insert(id));
        }
        if !item_name.is_empty() {
            keep_lowest(
                self.by_name
                    .entry((item_type.clone(), item_name.trim().to_lowercase()))
                    .or_insert(id),
            );
        }
        keep_lowest(self.by_sprite.entry((item_type, sprite_id)).or_insert(id));
    }

    /// Like [`ItemsBase::lookup`], but a floor/wall product whose class name the source
    /// furnidata knows is matched by item_name only (its sprite id is the source hotel's).
    pub fn lookup_with(&self, product: &Product, furnidata: Option<&Furnidata>) -> Option<u32> {
        if let Some(name) = furnidata.and_then(|f| f.classname(product)) {
            return self
                .by_name
                .get(&(product.product_type.clone(), name.to_lowercase()))
                .copied();
        }
        self.lookup(product)
    }

    pub fn lookup(&self, product: &Product) -> Option<u32> {
        if product.product_type == "b" {
            return self.badges.get(&product.extra_param).copied();
        }
        let class_id = product.class_id?;
        self.by_sprite
            .get(&(product.product_type.clone(), class_id))
            .copied()
            .or_else(|| {
                (product.product_type == "e")
                    .then(|| self.by_effect.get(&class_id).copied())
                    .flatten()
            })
    }
}

/// A subquery that finds a product's items_base id at import time (`select` is the column
/// expression, e.g. `` `id` `` or `` CONCAT(`id`, ':2') ``).
fn product_subquery(product: &Product, select: &str, classname: Option<&str>) -> String {
    if let Some(name) = classname {
        return format!(
            "(SELECT {select} FROM `items_base` WHERE `type` = {} AND `item_name` = {} ORDER BY `id` LIMIT 1)",
            sql_str(&product.product_type),
            sql_str(name)
        );
    }
    let condition = match (product.product_type.as_str(), product.class_id) {
        ("b", _) => format!(
            "`type` = 'b' AND `item_name` = {}",
            sql_str(&product.extra_param)
        ),
        ("e", Some(id)) => {
            format!("`type` = 'e' AND (`sprite_id` = {id} OR `effect_id_male` = {id})")
        }
        (t, Some(id)) => format!("`type` = {} AND `sprite_id` = {id}", sql_str(t)),
        (t, None) => format!(
            "`type` = {} AND `item_name` = {}",
            sql_str(t),
            sql_str(&product.extra_param)
        ),
    };
    format!("(SELECT {select} FROM `items_base` WHERE {condition} ORDER BY `id` LIMIT 1)")
}

/// How an offer's item_ids were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemIds {
    pub sql: String,
    pub resolved: usize,
    pub unresolved: usize,
}

/// item_ids for an offer: `id` for one product (its count goes to `amount`), else
/// `id:count;id:count` like Arcturus' bundles. Products the database knows are written as
/// numbers; the others as a subquery on items_base that runs when the SQL is imported.
pub fn item_ids_sql(offer: &Offer, items: Option<&ItemsBase>) -> ItemIds {
    item_ids_sql_with(offer, items, None)
}

/// [`item_ids_sql`] with the source hotel's furnidata: floor and wall products are matched (and
/// looked up on import) by class name instead of sprite id.
pub fn item_ids_sql_with(
    offer: &Offer,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
) -> ItemIds {
    let single = offer.products.len() == 1;
    let mut parts = Vec::new();
    let (mut resolved, mut unresolved) = (0, 0);
    let mut all_literal = true;
    for product in &offer.products {
        let suffix = if single {
            String::new()
        } else {
            format!(":{}", product.count.max(1))
        };
        match items.and_then(|i| i.lookup_with(product, furnidata)) {
            Some(id) => {
                resolved += 1;
                parts.push((true, format!("{id}{suffix}")));
            }
            None => {
                unresolved += 1;
                all_literal = false;
                let select = if suffix.is_empty() {
                    "`id`".to_string()
                } else {
                    format!("CONCAT(`id`, {})", sql_str(&suffix))
                };
                let classname = furnidata.and_then(|f| f.classname(product));
                parts.push((false, product_subquery(product, &select, classname)));
            }
        }
    }
    let sql = if all_literal {
        sql_str(
            &parts
                .iter()
                .map(|(_, p)| p.as_str())
                .collect::<Vec<_>>()
                .join(";"),
        )
    } else {
        let args: Vec<String> = parts
            .into_iter()
            .map(|(literal, p)| if literal { sql_str(&p) } else { p })
            .collect();
        format!("CONCAT_WS(';', {})", args.join(", "))
    };
    ItemIds {
        sql,
        resolved,
        unresolved,
    }
}

// ---------------------------------------------------------------- sql files

/// Counters for the README and the summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqlStats {
    pub pages: usize,
    pub items: usize,
    pub products_resolved: usize,
    pub products_unresolved: usize,
    pub offers_without_products: usize,
    pub duplicate_offers: usize,
    pub unknown_layouts: BTreeMap<String, usize>,
    pub synthetic_page_ids: usize,
    pub truncated_values: usize,
    /// Of pages / items: the Builders Club rows (catalog_pages_bc / catalog_items_bc).
    pub bc_pages: usize,
    pub bc_items: usize,
}

pub struct SqlFiles {
    pub pages: String,
    pub items: String,
    /// catalog_pages_bc.sql / catalog_items_bc.sql (empty when no Builders Club was ripped).
    pub pages_bc: String,
    pub items_bc: String,
    pub stats: SqlStats,
}

struct PageRow<'a> {
    id: i32,
    parent_id: i32,
    order: usize,
    node: &'a IndexNode,
    page: Option<&'a CatalogPage>,
}

const ROWS_PER_INSERT: usize = 250;

fn header(title: &str, catalog: &RippedCatalog) -> String {
    format!(
        "-- {title}\n-- Ripped from {} ({}, {} client {}) on {} by {}.\n-- Import catalog_pages.sql first, then catalog_items.sql (see README.txt).\n\nSET NAMES utf8mb4;\nSET FOREIGN_KEY_CHECKS = 0;\n\n",
        catalog.hotel,
        catalog.host,
        catalog.client_type,
        catalog.hotel_version,
        catalog.ripped_at,
        catalog.generator,
    )
}

fn write_inserts(out: &mut String, table_head: &str, rows: &[(String, String)]) {
    for chunk in rows.chunks(ROWS_PER_INSERT) {
        out.push_str(table_head);
        out.push_str(" VALUES\n");
        for (i, (note, row)) in chunk.iter().enumerate() {
            if !note.is_empty() {
                let _ = writeln!(out, "-- {}", comment(note));
            }
            out.push_str(row);
            out.push_str(if i + 1 == chunk.len() { ";\n\n" } else { ",\n" });
        }
    }
}

/// catalog_pages.sql and catalog_items.sql for Arcturus.
pub fn arcturus_sql(catalog: &RippedCatalog, items: Option<&ItemsBase>) -> SqlFiles {
    arcturus_sql_with(catalog, items, None)
}

/// [`arcturus_sql`] with the source hotel's furnidata (see [`item_ids_sql_with`]).
///
/// The normal catalogue goes to catalog_pages / catalog_items; the Builders Club catalogue to
/// catalog_pages_bc / catalog_items_bc (the warehouse tables of Arcturus Morningstar and its
/// forks), in their own files. Habbo numbers the two catalogues apart: the same page id can be
/// another page in each (on habbo.it 67 ids are in both, with other offers), so they are never
/// merged into one row.
pub fn arcturus_sql_with(
    catalog: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
) -> SqlFiles {
    let mut stats = SqlStats::default();
    let normal: Vec<&RippedMode> = catalog
        .modes
        .iter()
        .filter(|m| m.mode.eq_ignore_ascii_case("NORMAL"))
        .collect();
    let builders: Vec<&RippedMode> = catalog
        .modes
        .iter()
        .filter(|m| !m.mode.eq_ignore_ascii_case("NORMAL"))
        .collect();
    let collect = |modes: &[&RippedMode], bc: bool, stats: &mut SqlStats| {
        let (mut pages, mut offers) = (Vec::new(), Vec::new());
        arcturus_rows(
            modes,
            bc,
            items,
            furnidata,
            stats,
            Rows::Both,
            &mut |note, row| pages.push((note.to_string(), row)),
            &mut |note, row| offers.push((note.to_string(), row)),
        );
        (pages, offers)
    };
    let (page_rows, item_rows) = collect(&normal, false, &mut stats);
    let (bc_page_rows, bc_item_rows) = collect(&builders, true, &mut stats);
    stats.bc_pages = bc_page_rows.len();
    stats.bc_items = bc_item_rows.len();

    let unresolved_note = |sql: &mut String| {
        sql.push_str(items_note(items.is_some(), stats.products_unresolved));
    };

    let mut pages_sql = header(SqlFile::Pages.title(), catalog);
    pages_sql.push_str(SqlFile::Pages.note());
    write_inserts(&mut pages_sql, SqlFile::Pages.head(), &page_rows);
    pages_sql.push_str("SET FOREIGN_KEY_CHECKS = 1;\n");

    let mut items_sql = header(SqlFile::Items.title(), catalog);
    unresolved_note(&mut items_sql);
    write_inserts(&mut items_sql, SqlFile::Items.head(), &item_rows);
    items_sql.push_str("SET FOREIGN_KEY_CHECKS = 1;\n");

    let (mut pages_bc, mut items_bc) = (String::new(), String::new());
    if !builders.is_empty() {
        pages_bc = header(SqlFile::PagesBc.title(), catalog);
        pages_bc.push_str(SqlFile::PagesBc.note());
        write_inserts(&mut pages_bc, SqlFile::PagesBc.head(), &bc_page_rows);
        pages_bc.push_str("SET FOREIGN_KEY_CHECKS = 1;\n");
        items_bc = header(SqlFile::ItemsBc.title(), catalog);
        unresolved_note(&mut items_bc);
        write_inserts(&mut items_bc, SqlFile::ItemsBc.head(), &bc_item_rows);
        items_bc.push_str("SET FOREIGN_KEY_CHECKS = 1;\n");
    }

    SqlFiles {
        pages: pages_sql,
        items: items_sql,
        pages_bc,
        items_bc,
        stats,
    }
}

/// One of the Arcturus SQL files ([`arcturus_sql_with`] makes the four at once,
/// [`write_arcturus_file`] one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlFile {
    Pages,
    Items,
    PagesBc,
    ItemsBc,
}

impl SqlFile {
    pub fn name(self) -> &'static str {
        match self {
            SqlFile::Pages => "catalog_pages.sql",
            SqlFile::Items => "catalog_items.sql",
            SqlFile::PagesBc => "catalog_pages_bc.sql",
            SqlFile::ItemsBc => "catalog_items_bc.sql",
        }
    }

    fn bc(self) -> bool {
        matches!(self, SqlFile::PagesBc | SqlFile::ItemsBc)
    }

    fn pages(self) -> bool {
        matches!(self, SqlFile::Pages | SqlFile::PagesBc)
    }

    fn title(self) -> &'static str {
        match self {
            SqlFile::Pages => "catalog_pages for Arcturus",
            SqlFile::Items => "catalog_items for Arcturus",
            SqlFile::PagesBc => "catalog_pages_bc (Builders Club) for Arcturus",
            SqlFile::ItemsBc => "catalog_items_bc (Builders Club) for Arcturus",
        }
    }

    /// What a page file says after its header.
    fn note(self) -> &'static str {
        match self {
            SqlFile::Pages => "-- To replace the hotel's catalogue instead of adding to it, first run:\n--   DELETE FROM `catalog_items`; DELETE FROM `catalog_pages`;\n\n",
            SqlFile::PagesBc => "-- To replace the Builders Club catalogue instead of adding to it, first run:\n--   DELETE FROM `catalog_items_bc`; DELETE FROM `catalog_pages_bc`;\n\n",
            SqlFile::Items | SqlFile::ItemsBc => "",
        }
    }

    fn head(self) -> &'static str {
        match self {
            SqlFile::Pages => "INSERT INTO `catalog_pages` (`id`, `parent_id`, `caption_save`, `caption`, `page_layout`, `icon_color`, `icon_image`, `min_rank`, `order_num`, `visible`, `enabled`, `club_only`, `catalog_mode`, `vip_only`, `page_headline`, `page_teaser`, `page_special`, `page_text1`, `page_text2`, `page_text_details`, `page_text_teaser`, `room_id`, `includes`)",
            SqlFile::Items => "INSERT INTO `catalog_items` (`id`, `item_ids`, `page_id`, `catalog_name`, `cost_credits`, `cost_points`, `points_type`, `amount`, `limited_stack`, `limited_sells`, `order_number`, `offer_id`, `song_id`, `extradata`, `have_offer`, `club_only`)",
            SqlFile::PagesBc => "INSERT INTO `catalog_pages_bc` (`id`, `parent_id`, `caption`, `page_layout`, `icon_color`, `icon_image`, `order_num`, `visible`, `enabled`, `page_headline`, `page_teaser`, `page_special`, `page_text1`, `page_text2`, `page_text_details`, `page_text_teaser`)",
            SqlFile::ItemsBc => "INSERT INTO `catalog_items_bc` (`id`, `item_ids`, `page_id`, `catalog_name`, `order_number`, `extradata`)",
        }
    }
}

/// What an item file says after its header: how item_ids are found.
fn items_note(with_items_base: bool, products_unresolved: usize) -> &'static str {
    if !with_items_base {
        "-- item_ids are looked up in items_base while importing (subqueries on sprite_id + type,\n-- badges on item_name); a product the hotel does not have yields an empty entry.\n\n"
    } else if products_unresolved > 0 {
        "-- Products the database did not have when ripping are looked up in items_base while\n-- importing (subqueries); a product the hotel still does not have yields an empty entry.\n\n"
    } else {
        ""
    }
}

/// The catalogues of a table set: normal, or Builders Club (`bc`).
fn table_set(catalog: &RippedCatalog, bc: bool) -> Vec<&RippedMode> {
    catalog
        .modes
        .iter()
        .filter(|m| m.mode.eq_ignore_ascii_case("NORMAL") != bc)
        .collect()
}

/// The products of the rip that items_base has no id for (all of them without items_base):
/// [`SqlStats::products_unresolved`] of [`arcturus_sql_with`], counted without writing SQL.
pub fn products_unresolved(
    catalog: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
) -> usize {
    let mut count = 0;
    for bc in [false, true] {
        let modes = table_set(catalog, bc);
        for row in table_rows(&modes, &mut 0) {
            let Some(page) = row.page else { continue };
            for product in page.offers.iter().flat_map(|o| &o.products) {
                if items
                    .and_then(|i| i.lookup_with(product, furnidata))
                    .is_none()
                {
                    count += 1;
                }
            }
        }
    }
    count
}

/// `INSERT ... VALUES` statements of [`ROWS_PER_INSERT`] rows, written as the rows come
/// ([`write_inserts`] for rows that are not all there at once).
struct Inserts {
    head: &'static str,
    rows: usize,
}

impl Inserts {
    fn row(&mut self, out: &mut dyn fmt::Write, note: &str, row: &str) {
        if self.rows.is_multiple_of(ROWS_PER_INSERT) {
            let _ = out.write_str(self.head);
            let _ = out.write_str(" VALUES\n");
        } else {
            let _ = out.write_str(",\n");
        }
        if !note.is_empty() {
            let _ = writeln!(out, "-- {}", comment(note));
        }
        let _ = out.write_str(row);
        self.rows += 1;
        if self.rows.is_multiple_of(ROWS_PER_INSERT) {
            let _ = out.write_str(";\n\n");
        }
    }

    fn finish(&self, out: &mut dyn fmt::Write) {
        if !self.rows.is_multiple_of(ROWS_PER_INSERT) {
            let _ = out.write_str(";\n\n");
        }
    }
}

/// One Arcturus SQL file, written into `out` row by row: the bytes [`arcturus_sql_with`] makes
/// for it, without the rows of the other files or a list of rows in memory. `false` (nothing
/// written): a `_bc` file and no Builders Club catalogue was ripped. `products_unresolved`:
/// [`products_unresolved`]. Adds what the file counts to `stats` (a page file its pages and
/// synthetic page ids, an item file its offers and products; the `_bc` ones also `bc_pages` /
/// `bc_items`), so the four files together count what [`arcturus_sql_with`] does.
pub fn write_arcturus_file(
    out: &mut dyn fmt::Write,
    catalog: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
    file: SqlFile,
    products_unresolved: usize,
    stats: &mut SqlStats,
) -> bool {
    let modes = table_set(catalog, file.bc());
    if file.bc() && modes.is_empty() {
        return false;
    }
    let _ = out.write_str(&header(file.title(), catalog));
    if file.pages() {
        let _ = out.write_str(file.note());
    } else {
        let _ = out.write_str(items_note(items.is_some(), products_unresolved));
    }
    let mut inserts = Inserts {
        head: file.head(),
        rows: 0,
    };
    {
        let mut write = |note: &str, row: String| inserts.row(out, note, &row);
        let mut skip = |_: &str, _: String| {};
        let (pages, offers): (RowSink, RowSink) = if file.pages() {
            (&mut write, &mut skip)
        } else {
            (&mut skip, &mut write)
        };
        let want = if file.pages() { Rows::Pages } else { Rows::Items };
        arcturus_rows(&modes, file.bc(), items, furnidata, stats, want, pages, offers);
    }
    inserts.finish(out);
    let _ = out.write_str("SET FOREIGN_KEY_CHECKS = 1;\n");
    match file {
        SqlFile::PagesBc => stats.bc_pages += inserts.rows,
        SqlFile::ItemsBc => stats.bc_items += inserts.rows,
        SqlFile::Pages | SqlFile::Items => {}
    }
    true
}

/// Which rows [`arcturus_rows`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rows {
    Pages,
    Items,
    Both,
}

/// Gets each row as it is made: (note, `(values)`).
type RowSink<'s> = &'s mut dyn FnMut(&str, String);

/// One row per page id of a table set; disabled pages (id -1 in the index) get new ids above
/// the highest, counted in `synthetic`.
fn table_rows<'a>(modes: &[&'a RippedMode], synthetic: &mut usize) -> Vec<PageRow<'a>> {
    let mut next_synthetic = first_free_page_id(modes.iter().copied());
    let mut rows: Vec<PageRow> = Vec::new();
    let mut row_of: HashMap<i32, usize> = HashMap::new();
    for mode in modes {
        for numbered in numbered_pages(mode, &mut next_synthetic, synthetic) {
            match row_of.get(&numbered.id) {
                Some(&existing) => {
                    let row = &mut rows[existing];
                    if row.page.is_none() {
                        row.page = numbered.page;
                    }
                }
                None => {
                    row_of.insert(numbered.id, rows.len());
                    rows.push(PageRow {
                        id: numbered.id,
                        parent_id: numbered.parent_id,
                        order: numbered.order,
                        node: numbered.node,
                        page: numbered.page,
                    });
                }
            }
        }
    }
    rows
}

/// A page row's `(values)`, with its note.
fn page_row_values(row: &PageRow, bc: bool, stats: &mut SqlStats) -> (String, String) {
    let node = row.node;
    let layout_code = row.page.map(|p| p.layout.as_str()).unwrap_or("");
    let mut note = String::new();
    let layout = match arcturus_layout(layout_code) {
        Some(l) => l,
        None if layout_code.is_empty() => "default_3x3",
        None => {
            *stats
                .unknown_layouts
                .entry(layout_code.to_string())
                .or_default() += 1;
            note = format!("page {}: layout '{layout_code}' is not in Arcturus' page_layout enum, written as default_3x3", row.id);
            "default_3x3"
        }
    };
    let content = row
        .page
        .map(|p| map_page_content(&p.layout, &p.images, &p.texts))
        .unwrap_or_default();
    let t = &mut stats.truncated_values;
    let visible = sql_str(if node.visible { "1" } else { "0" });
    let enabled = sql_str(if node.page_id > 0 { "1" } else { "0" });
    let values: Vec<String> = if bc {
        vec![
            row.id.to_string(),
            row.parent_id.to_string(),
            sql_str(&fit(&node.localization, 128, t)),
            sql_str(layout),
            "1".into(),
            node.icon.to_string(),
            row.order.to_string(),
            visible,
            enabled,
            sql_str(&fit(&content.headline, 1024, t)),
            sql_str(&fit(&content.teaser, 64, t)),
            sql_str(&fit(&content.special, 2048, t)),
            sql_str(&content.text1),
            sql_str(&content.text2),
            sql_str(&content.text_details),
            sql_str(&content.text_teaser),
        ]
    } else {
        vec![
            row.id.to_string(),
            row.parent_id.to_string(),
            sql_str(&fit(&node.name, 128, t)),
            sql_str(&fit(&node.localization, 128, t)),
            sql_str(layout),
            "1".into(),
            node.icon.to_string(),
            "1".into(),
            row.order.to_string(),
            visible,
            enabled,
            "'0'".into(),
            sql_str("NORMAL"),
            "'0'".into(),
            sql_str(&fit(&content.headline, 1024, t)),
            sql_str(&fit(&content.teaser, 64, t)),
            sql_str(&fit(&content.special, 2048, t)),
            sql_str(&content.text1),
            sql_str(&content.text2),
            sql_str(&content.text_details),
            sql_str(&content.text_teaser),
            "0".into(),
            "''".into(),
        ]
    };
    (note, format!("({})", values.join(", ")))
}

/// The page and offer rows of some catalogues (one table set: normal, or the Builders Club's
/// `_bc` tables with their fewer columns), handed one at a time to `page_row` / `item_row`.
/// `want` says which: the stats count only what is made (the synthetic page ids with the
/// pages).
#[allow(clippy::too_many_arguments)]
fn arcturus_rows(
    modes: &[&RippedMode],
    bc: bool,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
    stats: &mut SqlStats,
    want: Rows,
    page_row: RowSink,
    item_row: RowSink,
) {
    let mut scratch = 0;
    let synthetic = if want == Rows::Items {
        &mut scratch
    } else {
        &mut stats.synthetic_page_ids
    };
    let rows = table_rows(modes, synthetic);
    let mut seen_offers: HashSet<i32> = HashSet::new();
    // Repeated offers get explicit ids above every Habbo offer id: an AUTO_INCREMENT (NULL) id
    // could take a Habbo id that a later row inserts explicitly (duplicate key on import).
    let mut next_automatic_id = rows
        .iter()
        .filter_map(|r| r.page)
        .flat_map(|p| p.offers.iter().map(|o| o.offer_id))
        .max()
        .unwrap_or(0)
        .max(0)
        + 1;
    for row in &rows {
        if want != Rows::Items {
            let (note, values) = page_row_values(row, bc, stats);
            page_row(&note, values);
            stats.pages += 1;
        }
        if want == Rows::Pages {
            continue;
        }

        let Some(page) = row.page else { continue };
        for (position, offer) in page.offers.iter().enumerate() {
            if offer.products.is_empty() {
                stats.offers_without_products += 1;
                continue;
            }
            let ids = item_ids_sql_with(offer, items, furnidata);
            stats.products_resolved += ids.resolved;
            stats.products_unresolved += ids.unresolved;
            let first = &offer.products[0];
            let amount = if offer.products.len() == 1 {
                first.count.max(1)
            } else {
                1
            };
            let limited = offer.products.iter().find(|p| p.limited);
            let (stack, sells) = limited
                .map(|p| {
                    (
                        p.limited_series_size,
                        (p.limited_series_size - p.limited_items_left).max(0),
                    )
                })
                .unwrap_or((0, 0));
            let id = if seen_offers.insert(offer.offer_id) && offer.offer_id > 0 {
                offer.offer_id.to_string()
            } else {
                stats.duplicate_offers += 1;
                next_automatic_id += 1;
                (next_automatic_id - 1).to_string()
            };
            let products: Vec<String> = offer
                .products
                .iter()
                .map(|p| match p.class_id {
                    Some(c) => format!(
                        "{}:{c} x{}{}",
                        p.product_type,
                        p.count,
                        if p.extra_param.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", p.extra_param)
                        }
                    ),
                    None => format!("{}:{}", p.product_type, p.extra_param),
                })
                .collect();
            let note = if ids.unresolved > 0 {
                format!(
                    "offer {}: {} (looked up in items_base on import)",
                    offer.offer_id,
                    products.join(", ")
                )
            } else {
                String::new()
            };
            let t = &mut stats.truncated_values;
            let values: Vec<String> = if bc {
                vec![
                    id,
                    ids.sql,
                    row.id.to_string(),
                    sql_str(&fit(&offer.localization_id, 100, t)),
                    (position + 1).to_string(),
                    sql_str(&fit(&first.extra_param, 500, t)),
                ]
            } else {
                vec![
                    id,
                    ids.sql,
                    row.id.to_string(),
                    sql_str(&fit(&offer.localization_id, 100, t)),
                    offer.price_credits.to_string(),
                    offer.price_activity_points.to_string(),
                    offer.activity_point_type.to_string(),
                    amount.to_string(),
                    stack.to_string(),
                    sells.to_string(),
                    (position + 1).to_string(),
                    offer.offer_id.to_string(),
                    "0".into(),
                    sql_str(&fit(&first.extra_param, 500, t)),
                    sql_str(if offer.bundle_purchase_allowed {
                        "1"
                    } else {
                        "0"
                    }),
                    sql_str(if offer.club_level > 0 { "1" } else { "0" }),
                ]
            };
            item_row(&note, format!("({})", values.join(", ")));
            stats.items += 1;
        }
    }
}

/// README.txt for the output folder.
pub fn readme(
    catalog: &RippedCatalog,
    stats: &SqlStats,
    images: &str,
    database_note: &str,
) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "Habbo catalogue rip - {} ({})",
        catalog.hotel, catalog.host
    );
    let _ = writeln!(
        text,
        "Client: {} {}",
        catalog.client_type, catalog.hotel_version
    );
    let _ = writeln!(
        text,
        "Ripped: {} by {}",
        catalog.ripped_at, catalog.generator
    );
    if !catalog.complete {
        let _ = writeln!(text, "\n*** INCOMPLETE: the hotel connection ended during the rip; the files hold what was read until then. ***");
    }
    text.push_str(
        "\nFILES\n\
         \x20 catalog.json        Everything the hotel sent, per catalogue mode (NORMAL, BUILDERS_CLUB): the page tree\n\
         \x20                     with each page's id, parent, name, localization, icon, visibility, layout, images,\n\
         \x20                     texts, offers (prices, points type, club level, products with class id, type,\n\
         \x20                     extra data, amount and limited series) and frontpage items.\n\
         \x20 catalog_pages.sql   INSERTs for Arcturus' catalog_pages.\n\
         \x20 catalog_items.sql   INSERTs for Arcturus' catalog_items.\n\
         \x20 catalog_pages_bc.sql, catalog_items_bc.sql  The Builders Club catalogue, for catalog_pages_bc\n\
         \x20                     and catalog_items_bc (only when it was ripped; Habbo numbers it apart).\n\
         \x20 images/             Page images (headers, teasers, ...) from the catalogue image URL,\n\
         \x20 images/icons/       page icons icon_<n>.png from the icon URL,\n\
         \x20 images/frontpage/   frontpage promo images.\n\
         \nIMPORT (Arcturus / Morningstar, MariaDB or MySQL)\n\
         \x20 1. Back up your database.\n\
         \x20 2. Import catalog_pages.sql, then catalog_items.sql (catalog_items references catalog_pages);\n\
         \x20    the same for catalog_pages_bc.sql and catalog_items_bc.sql when the hotel has those tables.\n\
         \x20    The page and offer ids are Habbo's: on a hotel that already has a catalogue they will collide.\n\
         \x20    To replace the catalogue, first run: DELETE FROM catalog_items; DELETE FROM catalog_pages;\n\
         \x20 3. Copy images/ into your catalogue image folder (c_images/catalogue) and images/icons into the\n\
         \x20    icon folder; frontpage promo images keep their c_images relative path in catalog.json.\n\
         \x20 4. Reload the catalogue (:update_catalogue) or restart the emulator.\n\
         \nHOW THE COLUMNS ARE FILLED\n\
         \x20 catalog_pages: id = Habbo page id, parent_id = parent page (-1 at the top), caption_save = page name,\n\
         \x20   caption = localization, icon_image = icon, order_num = position under the parent, visible from\n\
         \x20   the index, enabled = 0 for pages Habbo lists as disabled (id -1; they get new ids above the\n\
         \x20   highest page id), catalog_mode = NORMAL (the Builders Club is in the _bc files). page_layout: Habbo's layout code\n\
         \x20   mapped to the Arcturus enum (frontpage4 -> frontpage, guild_frontpage -> guilds,\n\
         \x20   guild_custom_furni -> guild_furni, club_gifts -> club_gift; codes the enum lacks -> default_3x3).\n\
         \x20   The page images and texts go into page_headline, page_teaser, page_special, page_text1,\n\
         \x20   page_text2, page_text_details and page_text_teaser in the order the emulator's layout class\n\
         \x20   writes them back (e.g. default_3x3: images headline, teaser, special; texts text1, details,\n\
         \x20   teaser). min_rank, club_only and vip_only are not in the packets: 1, 0, 0.\n\
         \x20 catalog_items: id = offer_id = Habbo offer id (an offer shown on several pages gets an automatic\n\
         \x20   id after the first), catalog_name = offer localization id, cost_credits / cost_points /\n\
         \x20   points_type from the offer, amount = product count (single products; bundles write\n\
         \x20   id:count;id:count in item_ids), limited_stack = series size, limited_sells = size - left,\n\
         \x20   order_number = position on the page, extradata = the first product's extra data,\n\
         \x20   have_offer = bundle purchase allowed, club_only = club level > 0.\n\
         \x20   item_ids point at items_base ids: product class id = items_base.sprite_id with the same type\n\
         \x20   (s floor, i wall, e effect also by effect_id_male, b badge by item_name).\n",
    );
    let _ = writeln!(text, "\n  {database_note}");
    let _ = writeln!(text, "\nNUMBERS");
    let _ = writeln!(text, "  catalog_pages rows:            {}", stats.pages);
    let _ = writeln!(text, "  catalog_items rows:            {}", stats.items);
    let _ = writeln!(
        text,
        "  products with a known id:      {}",
        stats.products_resolved
    );
    let _ = writeln!(
        text,
        "  products looked up on import:  {}",
        stats.products_unresolved
    );
    if stats.offers_without_products > 0 {
        let _ = writeln!(
            text,
            "  offers without products:       {} (not written to SQL; in catalog.json)",
            stats.offers_without_products
        );
    }
    if stats.duplicate_offers > 0 {
        let _ = writeln!(
            text,
            "  offers on more than one page:  {} (automatic ids)",
            stats.duplicate_offers
        );
    }
    if stats.synthetic_page_ids > 0 {
        let _ = writeln!(
            text,
            "  disabled pages with new ids:   {}",
            stats.synthetic_page_ids
        );
    }
    if stats.truncated_values > 0 {
        let _ = writeln!(
            text,
            "  values cut to the column size: {}",
            stats.truncated_values
        );
    }
    if !stats.unknown_layouts.is_empty() {
        let list: Vec<String> = stats
            .unknown_layouts
            .iter()
            .map(|(l, n)| format!("{l} ({n})"))
            .collect();
        let _ = writeln!(
            text,
            "  layouts written as default_3x3: {}",
            list.join(", ")
        );
    }
    for mode in &catalog.modes {
        if !mode.failed.is_empty() {
            let ids: Vec<String> = mode.failed.iter().map(|i| i.to_string()).collect();
            let _ = writeln!(
                text,
                "  {} pages without an answer:  {}",
                mode.mode,
                ids.join(", ")
            );
        }
    }
    let _ = writeln!(text, "  images:                        {images}");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(sql_str("a'b\\c\n\"\0\x1a"), "'a\\'b\\\\c\\n\\\"\\0\\Z'");
    }

    #[test]
    fn layouts() {
        assert_eq!(arcturus_layout("frontpage4"), Some("frontpage"));
        assert_eq!(arcturus_layout("guild_custom_furni"), Some("guild_furni"));
        assert_eq!(arcturus_layout("default_3x3"), Some("default_3x3"));
        assert_eq!(arcturus_layout("recolorable_furniture"), Some("recolorable_furniture"));
        assert_eq!(arcturus_layout("default_3x3_extrainfo"), None);
    }

    #[test]
    fn content_mapping_inverts_layouts() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let c = map_page_content(
            "default_3x3",
            &s(&["h", "t", "sp"]),
            &s(&["one", "det", "tz"]),
        );
        assert_eq!(
            (c.headline.as_str(), c.teaser.as_str(), c.special.as_str()),
            ("h", "t", "sp")
        );
        assert_eq!(
            (
                c.text1.as_str(),
                c.text_details.as_str(),
                c.text_teaser.as_str()
            ),
            ("one", "det", "tz")
        );
        let c = map_page_content("frontpage4", &s(&["h", "t"]), &s(&["one", "two", "tz"]));
        assert_eq!(
            (c.text1.as_str(), c.text2.as_str(), c.text_teaser.as_str()),
            ("one", "two", "tz")
        );
        let c = map_page_content("pets", &s(&["h", "t"]), &s(&["1", "2", "d", "tz"]));
        assert_eq!(
            (
                c.text2.as_str(),
                c.text_details.as_str(),
                c.text_teaser.as_str()
            ),
            ("2", "d", "tz")
        );
        let c = map_page_content(
            "single_bundle",
            &s(&["h", "t", ""]),
            &s(&["1", "d", "tz", "2"]),
        );
        assert_eq!((c.special.as_str(), c.text2.as_str()), ("", "2"));
        let c = map_page_content("info_rentables", &s(&["h"]), &s(&["a", "b"]));
        assert_eq!(c.text1, "a||b");
        let c = map_page_content(
            "frontpage_featured",
            &s(&["h", "t1", "s1", "s2"]),
            &s(&["1", "d", "tz"]),
        );
        assert_eq!((c.teaser.as_str(), c.special.as_str()), ("t1", "s1;s2"));
    }
}
