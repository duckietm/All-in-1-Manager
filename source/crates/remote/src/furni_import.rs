//! Import Assets: the Habbo furniture the hotel database does not have yet into items_base, each with a catalogue item
//! on a new catalogue page "import_<date>" (parent_id -1), so the owner can move it, or its furni, where they want.
//!
//! The furniture comes from Get Assets' Habbo download on this computer: gamedata/config/FurnitureData.json and
//! gamedata/furniture/<name>.hab. Every items_base value is worked out the way the All-in-1 Converter's Generate SQL
//! does for a .nitro / .hab bundle (Generate_SQL.cs ProcessNitroFile; HabFurniScan.cs reads the same .hab json):
//! - width / length / stack_height: logic.model.dimensions x / y / z of the bundle (1, 1, 0 when missing); a wall
//!   item ('i') is always 1 x 1 x 0.00;
//! - allow_sit / allow_lay / allow_walk: cansiton / canlayon / canstandon of the furnidata entry;
//! - allow_stack: the height is not 1.0 and the furni can not be sat or laid on;
//! - interaction_modes_count: the animation ids 0..49 of the first visualization that has any (the highest + 1
//!   with an animation 0, else their number; 1 without);
//! - interaction_type 'recolorable' for "*_recolorable", else 'default'; sprite_id the furnidata id, public_name the
//!   classname, gift / trade / marketplace / inventory stack on, recycle off;
//! - a colour variant ("rare_dragonlamp*5") uses the bundle of its base name ("rare_dragonlamp.hab").
//!
//! The catalogue item is the converter's too: 5 credits, amount 1, order 99, have_offer, the furnidata's offer id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::script::{self, fill, sql_literal};
use crate::server::{Log, Result, Server};
use crate::server_tools;

/// The size and interaction modes a furni bundle gives.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bundle {
    pub width: f64,
    pub length: f64,
    pub height: f64,
    pub modes: i64,
    /// A multiheight furni (logicType furniture_multiheight): the height of each state.
    pub multiheight: Option<Heights>,
}

/// The height of each state of a multiheight furni (at most [`Heights::MAX`] states).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Heights {
    len: usize,
    values: [f64; Heights::MAX],
}

impl Heights {
    pub const MAX: usize = 16;

    pub fn new(values: &[f64]) -> Option<Self> {
        if values.is_empty() || values.len() > Self::MAX {
            return None;
        }
        let mut heights = Self { len: values.len(), values: [0.0; Self::MAX] };
        heights.values[..values.len()].copy_from_slice(values);
        Some(heights)
    }

    pub fn as_slice(&self) -> &[f64] {
        &self.values[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// As items_base.multiheight holds them: "1.00;0.75;0.50;0.25;0.10".
    pub fn column(&self) -> String {
        self.as_slice().iter().map(|h| format!("{h:.2}")).collect::<Vec<_>>().join(";")
    }
}

/// How many values an items_base.multiheight holds ("0" and "" hold none). Hotels write them with ';' and
/// sometimes with ',' ("0.8,2.0,3.0").
pub fn multiheight_count(column: &str) -> usize {
    let column = column.trim();
    if matches!(column, "" | "0") {
        return 0;
    }
    column.split([';', ',']).filter(|v| !v.trim().is_empty()).count()
}

/// The lowest height a multiheight state gets: a state of height 0 would act as the floor.
pub const MIN_STATE_HEIGHT: f64 = 0.10;

/// The heights of a multiheight furni (logicType furniture_multiheight, or furniture_recolorable whose states change
/// its height), one per state (animation), from how far
/// each state's image moves up or down: in the 64 px images 32 px is one tile of height (16 px in the 32 px ones).
/// The highest state is the bundle's height (logic.model.dimensions.z); every height is rounded to a quarter, and at
/// least 0.10. None for other furni, or when the images do not tell.
pub fn multiheight_heights(json: &Value) -> Option<Heights> {
    // A recolourable furni (bc_block_1_recolorable) can be one too: its states then move its image up or down, and
    // the emulator gives it the height of its state from the multiheight list. One whose images stay put is not.
    let recolorable = match json.get("logicType").and_then(Value::as_str) {
        Some("furniture_multiheight") => false,
        Some("furniture_recolorable") => true,
        _ => return None,
    };
    let name = json.get("name").and_then(Value::as_str)?;
    let visualizations = json.get("visualizations")?.as_array()?;
    let visualization = [64, 32].iter().find_map(|size| visualizations.iter().find(|v| v.get("size").and_then(Value::as_i64) == Some(*size)))?;
    let size = visualization["size"].as_i64()?;
    let per_tile = size as f64 / 2.0;
    let layers = visualization.get("layerCount").and_then(Value::as_i64).unwrap_or(1).clamp(1, 26) as usize;
    let direction = visualization
        .get("directions")
        .and_then(Value::as_object)
        .and_then(|d| d.keys().filter_map(|k| k.parse::<i64>().ok()).min())
        .unwrap_or(0);
    let animations = visualization.get("animations")?.as_object()?;
    let mut states: Vec<i64> = animations.keys().filter_map(|k| k.parse().ok()).filter(|id| (0..50).contains(id)).collect();
    states.sort_unstable();
    if states.len() < 2 {
        return None;
    }
    let assets = json.get("assets")?.as_object()?;
    // Per state, the highest point of its image: the largest y offset over its layers (each with its own frame).
    let mut tops = Vec::new();
    for state in &states {
        let animation = &animations[&state.to_string()];
        let mut top: Option<i64> = None;
        for layer in 0..layers {
            let frame = animation["layers"][layer.to_string()]["frameSequences"]["0"]["frames"]["0"]["id"].as_i64().unwrap_or(0);
            let letter = (b'a' + layer as u8) as char;
            if let Some(y) = assets.get(&format!("{name}_{size}_{letter}_{direction}_{frame}")).and_then(|a| a.get("y")).and_then(Value::as_i64) {
                top = Some(top.map_or(y, |t| t.max(y)));
            }
        }
        tops.push(top?);
    }
    let highest = *tops.iter().max()?;
    let (_, _, z) = dimensions(json).unwrap_or((1.0, 1.0, 1.0));
    let heights: Vec<f64> = tops
        .iter()
        .map(|top| {
            let height = z - (highest - top) as f64 / per_tile;
            let quarter = (height * 4.0).round() / 4.0;
            if quarter < MIN_STATE_HEIGHT { MIN_STATE_HEIGHT } else { quarter }
        })
        .collect();
    if recolorable && heights.windows(2).all(|w| w[0] == w[1]) {
        return None;
    }
    Heights::new(&heights)
}

/// logic.model.dimensions x / y / z of a bundle json (a missing axis is 1, 1 and 0); None without dimensions.
/// (SQLGenerator.DimensionsFromNitroJson)
pub fn dimensions(json: &Value) -> Option<(f64, f64, f64)> {
    let dimensions = json.get("logic")?.get("model")?.get("dimensions")?.as_object()?;
    let axis = |name: &str, default: f64| dimensions.get(name).and_then(Value::as_f64).unwrap_or(default);
    Some((axis("x", 1.0), axis("y", 1.0), axis("z", 0.0)))
}

/// The interaction_modes_count of a bundle json. (SQLGenerator.InteractionModesCountFromNitroJson)
pub fn interaction_modes_count(json: &Value) -> i64 {
    let Some(visualizations) = json.get("visualizations").and_then(Value::as_array) else { return 1 };
    for visualization in visualizations {
        let Some(animations) = visualization.get("animations").and_then(Value::as_object) else { continue };
        let ids: Vec<i64> = animations.keys().filter_map(|k| k.parse::<i64>().ok()).filter(|id| (0..50).contains(id)).collect();
        if !ids.is_empty() {
            return if ids.contains(&0) { ids.iter().max().unwrap() + 1 } else { ids.len() as i64 };
        }
    }
    1
}

/// What the json of a bundle gives. (ProcessNitroFile)
pub fn bundle_from_json(json: &Value) -> Bundle {
    let (width, length, height) = dimensions(json).unwrap_or((1.0, 1.0, 0.0));
    Bundle { width, length, height, modes: interaction_modes_count(json), multiheight: multiheight_heights(json) }
}

/// Reads the first .json of a .hab file (as HabFurniScan does); None when it is missing or unreadable.
pub fn read_bundle(path: &Path) -> Option<Bundle> {
    let data = std::fs::read(path).ok()?;
    let files = aio_assets::hab::read(&data).ok()?;
    let (_, json) = files.iter().find(|(name, _)| name.to_ascii_lowercase().ends_with(".json"))?;
    let json: Value = serde_json::from_slice(json).ok()?;
    Some(bundle_from_json(&json))
}

/// 'recolorable' for Habbo's recolourable furni (*_recolorable), else 'default'. (SQLGenerator.InteractionTypeFor)
pub fn interaction_type(classname: &str) -> &'static str {
    if classname.to_ascii_lowercase().ends_with("_recolorable") { "recolorable" } else { "default" }
}

/// The bundle a classname draws from: the part before '*'. (HabFurniScan.BundleName)
pub fn bundle_name(classname: &str) -> &str {
    classname.split('*').next().unwrap_or("").trim()
}

/// One furni to add to items_base (and the catalogue).
#[derive(Debug, Clone, PartialEq)]
pub struct NewFurni {
    pub classname: String,
    /// The furnidata id; None: the new items_base id is used.
    pub sprite_id: Option<i64>,
    pub offer_id: i64,
    /// 's' (floor) or 'i' (wall).
    pub kind: char,
    pub width: f64,
    pub length: f64,
    pub height: f64,
    pub allow_stack: bool,
    pub allow_sit: bool,
    pub allow_lay: bool,
    pub allow_walk: bool,
    pub interaction_type: &'static str,
    pub modes: i64,
    /// A multiheight furni's heights (items_base.multiheight), from its .hab; None: '0'.
    pub multiheight: Option<String>,
}

/// An items_base row that no catalogue item sells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncatalogued {
    pub id: i64,
    pub name: String,
    pub sprite_id: i64,
    pub offer_id: i64,
}

/// What the hotel database has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbState {
    pub database: String,
    /// The item_name of every items_base row, in lower case.
    pub names: HashSet<String>,
    /// Floor and wall furni of items_base that no catalogue item sells (pets "a0 ..." and the like left out).
    pub uncatalogued: Vec<Uncatalogued>,
    pub items_base: usize,
    /// The sprite_id and type of every items_base row, by item_name in lower case (the first row of a name).
    pub sprites: HashMap<String, (i64, char)>,
    /// The offer ids the catalogue items use (above 0).
    pub offer_ids: HashSet<i64>,
    /// (catalogue item id, offer_id) by catalog_name in lower case.
    pub catalog: HashMap<String, Vec<(i64, i64)>>,
    pub catalog_items: usize,
    /// The catalogue pages by id, with their caption (for choosing the new page's parent).
    pub pages: BTreeMap<i64, String>,
}

/// What an import would add.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub new: Vec<NewFurni>,
    /// Furnidata classnames the database lacks but whose bundle is not in the furniture folder (or unreadable).
    pub no_bundle: Vec<String>,
    pub uncatalogued: Vec<Uncatalogued>,
    /// Furni in items_base whose sprite_id has to follow FurnitureData.json (a custom furni got another id).
    pub moved: Vec<Moved>,
    /// Furni whose bundle height does not fit items_base.stack_height (double(4,2)): (classname, its height); they
    /// get the nearest height that fits.
    pub capped: Vec<(String, f64)>,
    /// Classnames longer than items_base.item_name holds (varchar(70)): left out, cut they would not match
    /// FurnitureData.json.
    pub too_long: Vec<String>,
    /// Catalogue items whose offer_id has to follow FurnitureData.json: (id, catalog_name, offer_id now, new).
    pub offers: Vec<(i64, String, i64, i64)>,
}

/// items_base.item_name is a varchar(70), public_name a varchar(56).
pub const MAX_ITEM_NAME: usize = 70;
pub const MAX_PUBLIC_NAME: usize = 56;

/// An id that fits the int(11) columns (sprite_id, offer_id).
fn fits_int(id: i64) -> bool {
    (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&id)
}

/// The highest stack_height items_base can hold: it is a double(4,2).
pub const MAX_STACK_HEIGHT: f64 = 99.99;

/// An items_base furni whose sprite_id changes: its FurnitureData.json id moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub name: String,
    /// 's' (floor) or 'i' (wall).
    pub kind: char,
    pub from: i64,
    pub to: i64,
}

impl Plan {
    /// Something to do: new furni, catalogue items, or sprite ids to follow.
    pub fn work(&self, with_uncatalogued: bool) -> usize {
        self.new.len() + self.moved.len() + self.offers.len() + if with_uncatalogued { self.uncatalogued.len() } else { 0 }
    }

    pub fn floor(&self) -> usize {
        self.new.iter().filter(|f| f.kind == 's').count()
    }

    pub fn wall(&self) -> usize {
        self.new.iter().filter(|f| f.kind == 'i').count()
    }

    pub fn variants(&self) -> usize {
        self.new.iter().filter(|f| f.classname.contains('*')).count()
    }
}

/// A furnidata flag: true, 1 or "1".
fn flag(entry: &Value, name: &str) -> bool {
    match entry.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// The furni of `furnidata` that the database lacks, with their items_base values from the bundles in
/// `furniture_dir`; a classname listed twice counts once (the first entry, as the converter's FindFurni).
/// `read` reads one bundle (by base name), so tests need no files.
pub fn plan_with(furnidata: &Value, db: &DbState, read: &(dyn Fn(&str) -> Option<Bundle> + Sync)) -> Plan {
    let mut wanted: Vec<(&Value, char)> = Vec::new();
    let mut seen = HashSet::new();
    for (section, kind) in [("roomitemtypes", 's'), ("wallitemtypes", 'i')] {
        for entry in furnidata[section]["furnitype"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let Some(classname) = entry.get("classname").and_then(Value::as_str) else { continue };
            let key = classname.trim().to_lowercase();
            if key.is_empty() || bundle_name(classname).is_empty() || db.names.contains(&key) || !seen.insert(key) {
                continue;
            }
            wanted.push((entry, kind));
        }
    }
    // One read per bundle: the colour variants share theirs.
    let names: Vec<String> = wanted.iter().map(|(e, _)| bundle_name(e["classname"].as_str().unwrap_or("")).to_string()).collect::<HashSet<_>>().into_iter().collect();
    let bundles = read_all(&names, read);
    // Furni already in items_base get the offer id of their furnidata entry.
    let offers = offer_ids(furnidata);
    let uncatalogued = db
        .uncatalogued
        .iter()
        .map(|row| Uncatalogued { offer_id: offers.get(&row.name.to_lowercase()).copied().unwrap_or(-1), ..row.clone() })
        .collect();
    let mut plan = Plan { uncatalogued, ..Plan::default() };
    for (entry, kind) in wanted {
        let classname = entry["classname"].as_str().unwrap_or("").trim().to_string();
        if classname.chars().count() > MAX_ITEM_NAME {
            plan.too_long.push(classname);
            continue;
        }
        let Some(bundle) = bundles.get(&bundle_name(&classname).to_lowercase()).copied().flatten() else {
            plan.no_bundle.push(classname);
            continue;
        };
        let (allow_sit, allow_lay, allow_walk) = (flag(entry, "cansiton"), flag(entry, "canlayon"), flag(entry, "canstandon"));
        // Stackable: a height other than 1.0 and nothing to sit or lie on (worked out before a wall item's reset).
        let allow_stack = bundle.height != 1.0 && !allow_sit && !allow_lay;
        let (width, length, mut height) = if kind == 'i' { (1.0, 1.0, 0.0) } else { (bundle.width, bundle.length, bundle.height) };
        // A retro's bundle can have a height items_base cannot hold (MariaDB refuses the whole import then).
        if !(-MAX_STACK_HEIGHT..=MAX_STACK_HEIGHT).contains(&height) {
            plan.capped.push((classname.clone(), height));
            height = height.clamp(-MAX_STACK_HEIGHT, MAX_STACK_HEIGHT);
        }
        // A multiheight furni (its .hab says so): interaction multiheight with a height per state, and stack_height
        // 0.00 (the emulator takes the state's height). A recolorable one keeps 'recolorable' and gets the heights.
        let interaction = interaction_type(&classname);
        let (interaction, multiheight, modes, height) = match bundle.multiheight {
            Some(heights) if interaction == "recolorable" => (interaction, Some(heights.column()), heights.len() as i64, height),
            Some(heights) => ("multiheight", Some(heights.column()), heights.len() as i64, 0.0),
            None => (interaction, None, bundle.modes, height),
        };
        let mut furni = NewFurni {
            sprite_id: entry.get("id").and_then(Value::as_i64).filter(|id| fits_int(*id)),
            offer_id: entry.get("offerid").and_then(Value::as_i64).filter(|id| fits_int(*id)).unwrap_or(-1),
            kind,
            width,
            length,
            height,
            allow_stack,
            allow_sit,
            allow_lay,
            allow_walk,
            interaction_type: interaction,
            modes,
            multiheight,
            classname,
        };
        apply_exception(&mut furni);
        plan.new.push(furni);
    }
    plan.no_bundle.sort_by_key(|c| c.to_lowercase());
    plan
}

/// [`plan_with`] on the .hab files of `furniture_dir` (<name>.hab, or the name in lower case).
pub fn plan(furnidata: &Value, furniture_dir: &Path, db: &DbState) -> Plan {
    let read = |name: &str| {
        let exact = furniture_dir.join(format!("{name}.hab"));
        read_bundle(&exact).or_else(|| read_bundle(&furniture_dir.join(format!("{}.hab", name.to_lowercase()))))
    };
    plan_with(furnidata, db, &read)
}

/// Reads the bundles on a few threads (a new hotel can lack thousands). Keyed by the name in lower case.
pub(crate) fn read_all(names: &[String], read: &(dyn Fn(&str) -> Option<Bundle> + Sync)) -> HashMap<String, Option<Bundle>> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let chunk = names.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = names
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(|name| (name.to_lowercase(), read(name))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// The values of assets/items_base_exceptions.json over what was worked out for a new furni.
pub fn apply_exception(furni: &mut NewFurni) {
    let Some(e) = crate::exceptions::find(&furni.classname) else { return };
    if let Some(interaction) = e.interaction_type.as_deref() {
        furni.interaction_type = interaction;
    }
    if let Some(modes) = e.interaction_modes_count {
        furni.modes = modes;
    }
    if let Some(width) = e.width {
        furni.width = width as f64;
    }
    if let Some(length) = e.length {
        furni.length = length as f64;
    }
    if let Some(height) = e.stack_height {
        furni.height = height;
    }
    if let Some(sit) = e.sit() {
        furni.allow_sit = sit;
    }
    if let Some(lay) = e.lay() {
        furni.allow_lay = lay;
    }
    if let Some(walk) = e.walk() {
        furni.allow_walk = walk;
    }
    if let Some(stack) = e.stack() {
        furni.allow_stack = stack;
    }
    if let Some(heights) = &e.multiheight {
        furni.multiheight = Some(heights.clone()).filter(|h| multiheight_count(h) > 0);
    }
}

/// A number as the converter writes it (C# double.ToString(InvariantCulture)): 1, 0.5, 1.73.
fn number(value: f64) -> String {
    format!("{value}")
}

fn yes(value: bool) -> &'static str {
    if value { "'1'" } else { "'0'" }
}

const ITEMS_BASE_COLUMNS: &str = "(`id`, `sprite_id`, `item_name`, `public_name`, `width`, `length`, `stack_height`, `allow_stack`, `allow_sit`, `allow_lay`, `allow_walk`, `allow_gift`, `allow_trade`, `allow_recycle`, `allow_marketplace_sell`, `allow_inventory_stack`, `type`, `interaction_type`, `interaction_modes_count`, `vending_ids`, `multiheight`, `customparams`, `effect_id_male`, `effect_id_female`, `clothing_on_walk`)";
const CATALOG_ITEMS_COLUMNS: &str = "(`id`, `item_ids`, `page_id`, `offer_id`, `song_id`, `order_number`, `catalog_name`, `cost_credits`, `cost_points`, `points_type`, `amount`, `limited_sells`, `limited_stack`, `extradata`, `have_offer`, `club_only`, `rent_days`, `habbicon_id`)";

/// The items_base row of a new furni, with `id` the SQL expression of its new id. (ProcessNitroFile's INSERT)
pub fn items_base_values(furni: &NewFurni, id: &str) -> String {
    let name = sql_literal(&furni.classname);
    // public_name holds less than item_name: only what is shown is cut.
    let public: String = furni.classname.chars().take(MAX_PUBLIC_NAME).collect();
    let public = sql_literal(&public);
    let sprite = furni.sprite_id.map_or_else(|| id.to_string(), |s| s.to_string());
    format!(
        "({id}, {sprite}, {name}, {public}, {}, {}, {}, {}, {}, {}, {}, '1', '1', '0', '1', '1', '{}', '{}', {}, '0', {}, '0', 0, 0, '0')",
        number(furni.width),
        number(furni.length),
        number(furni.height),
        yes(furni.allow_stack),
        yes(furni.allow_sit),
        yes(furni.allow_lay),
        yes(furni.allow_walk),
        furni.kind,
        furni.interaction_type,
        furni.modes,
        sql_literal(furni.multiheight.as_deref().unwrap_or("0"))
    )
}

/// The catalogue item of a furni: 5 credits, amount 1, order 99, have_offer. (CatalogItemInsert)
pub fn catalog_item_values(id: &str, item_id: &str, offer_id: i64, classname: &str) -> String {
    format!("({id}, CAST({item_id} AS CHAR), @page, {offer_id}, 0, 99, {}, 5, 0, 0, 1, 0, 0, '', '1', '0', 0, 0)", sql_literal(classname))
}

/// The SQL of the n-th new id after `variable` (the highest id there was). A whole number: `@item + 1` alone is a
/// DECIMAL in MariaDB, which item_ids would get as "1100009694.000...".
fn new_id(variable: &str, n: usize) -> String {
    format!("CAST({variable} + {n} AS UNSIGNED)")
}

/// The catalogue page's caption: import_<yyyymmdd>.
pub fn page_caption(date: chrono::NaiveDate) -> String {
    format!("import_{}", date.format("%Y%m%d"))
}

/// The name group of a classname: the part before the first '_' or '*', in lower case ("adobe_kaars2" -> "adobe",
/// "rare_dragonlamp*4" -> "rare", "duck" -> "duck"). Furni of one group stay on one catalogue page where they fit.
pub fn name_group(classname: &str) -> String {
    let name = classname.trim().to_lowercase();
    let group = name.split(['_', '*']).next().unwrap_or("").to_string();
    if group.is_empty() { name } else { group }
}

/// The furni (by name) on catalogue pages of at most `per_page`, as (caption, indexes into `names`): sorted by name,
/// a name group never split over pages when it fits on one. A group bigger than a page gets pages of its own,
/// "<group>-1", "<group>-2" ...; small groups share a page, named after its first and last group ("bath - bed").
pub fn pages(names: &[&str], per_page: usize) -> Vec<(String, Vec<usize>)> {
    let per_page = per_page.max(1);
    let mut order: Vec<usize> = (0..names.len()).collect();
    // Each key made once: 26 000 names sorted with keys made per comparison took long enough to notice.
    order.sort_by_cached_key(|&i| (name_group(names[i]), names[i].to_lowercase()));
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for i in order {
        let group = name_group(names[i]);
        match groups.last_mut() {
            Some((name, items)) if *name == group => items.push(i),
            _ => groups.push((group, vec![i])),
        }
    }
    let mut pages: Vec<(String, Vec<usize>)> = Vec::new();
    // The page being filled: its first and last group, and its furni.
    let mut current: Option<(String, String, Vec<usize>)> = None;
    let flush = |current: &mut Option<(String, String, Vec<usize>)>, pages: &mut Vec<(String, Vec<usize>)>| {
        if let Some((first, last, items)) = current.take() {
            let caption = if first == last { first } else { format!("{first} - {last}") };
            pages.push((caption, items));
        }
    };
    for (group, items) in groups {
        if items.len() > per_page {
            flush(&mut current, &mut pages);
            for (n, chunk) in items.chunks(per_page).enumerate() {
                pages.push((format!("{group}-{}", n + 1), chunk.to_vec()));
            }
            continue;
        }
        if current.as_ref().is_some_and(|(_, _, have)| have.len() + items.len() > per_page) {
            flush(&mut current, &mut pages);
        }
        match &mut current {
            Some((_, last, have)) => {
                *last = group;
                have.extend(items);
            }
            None => current = Some((group.clone(), group, items)),
        }
    }
    flush(&mut current, &mut pages);
    pages
}

/// The import as one transaction: the new items_base rows, then their catalogue items (and, with
/// `with_uncatalogued`, one for every items_base row that has none). Up to `per_page` they go on one new page
/// `caption` under `parent_id` (-1: the top of the catalogue); more go on pages of at most `per_page` ([`pages`])
/// under a new page `caption`. The new ids follow the highest ones in the tables, so nothing the hotel has is
/// overwritten.
pub fn sql(plan: &Plan, caption: &str, parent_id: i64, visible: bool, with_uncatalogued: bool, per_page: usize) -> String {
    let mut out = String::new();
    out.push_str("-- Import Assets (All-in-1 Manager): furniture the database did not have.\n");
    out.push_str(&format!(
        "-- {} new furniture ({} floor, {} wall), {} catalogue items for furniture without one.\n",
        plan.new.len(),
        plan.floor(),
        plan.wall(),
        if with_uncatalogued { plan.uncatalogued.len() } else { 0 }
    ));
    out.push_str("SET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    // FurnitureData.json moved these furni to another id: items_base follows, or the client shows other furniture.
    for moved in &plan.moved {
        out.push_str(&format!(
            "UPDATE `items_base` SET `sprite_id` = {} WHERE `item_name` = {} AND `type` = '{}' AND `sprite_id` = {};\n",
            moved.to,
            sql_literal(&moved.name),
            moved.kind,
            moved.from
        ));
    }
    // FurnitureData.json gave these furni another offer id: their catalogue items follow, or they cannot be bought.
    for (id, name, from, to) in &plan.offers {
        out.push_str(&format!("UPDATE `catalog_items` SET `offer_id` = {to} WHERE `id` = {id} AND `offer_id` = {from}; -- {}\n", sql_literal(name).replace('\n', " ")));
    }
    // Every catalogue item: (name, the SQL of its items_base id, offer id).
    let mut items: Vec<(&str, String, i64)> = plan.new.iter().enumerate().map(|(i, f)| (f.classname.as_str(), new_id("@item", i + 1), f.offer_id)).collect();
    if with_uncatalogued {
        items.extend(plan.uncatalogued.iter().map(|u| (u.name.as_str(), u.id.to_string(), u.offer_id)));
    }
    if items.is_empty() {
        out.push_str("COMMIT;\n");
        return out;
    }
    out.push_str("SELECT COALESCE(MAX(`id`), 0) INTO @item FROM `items_base`;\n");
    out.push_str("SELECT COALESCE(MAX(`id`), 0) INTO @offer FROM `catalog_items`;\n");
    // In batches, so one statement stays small.
    for (batch, furni) in plan.new.chunks(500).enumerate() {
        let start = batch * 500;
        let rows: Vec<String> = furni.iter().enumerate().map(|(i, f)| items_base_values(f, &new_id("@item", start + i + 1))).collect();
        out.push_str(&format!("INSERT INTO `items_base` {ITEMS_BASE_COLUMNS} VALUES\n{};\n", rows.join(",\n")));
    }
    let page = |out: &mut String, parent: &str, caption: &str, order: usize, visible: bool| {
        out.push_str(&format!(
            "INSERT INTO `catalog_pages` (`parent_id`, `page_layout`, `caption_save`, `caption`, `icon_color`, `icon_image`, `min_rank`, `order_num`, `visible`, `enabled`) VALUES ({parent}, 'default_3x3', {c}, {c}, 1, 1, 1, {order}, {}, '1');\n",
            yes(visible),
            c = sql_literal(caption)
        ));
        out.push_str("SET @page = LAST_INSERT_ID();\n");
    };
    let names: Vec<&str> = items.iter().map(|i| i.0).collect();
    let groups = if items.len() <= per_page.max(1) { vec![(caption.to_string(), (0..items.len()).collect())] } else { pages(&names, per_page) };
    let mut offer = 0;
    // The page `caption`: the catalogue items' page, or with more pages the folder above them (visible, so the
    // folder alone decides whether players see them).
    page(&mut out, &parent_id.to_string(), caption, 99, visible);
    out.push_str("SET @top = @page;\n");
    for (n, (title, indexes)) in groups.iter().enumerate() {
        if groups.len() > 1 {
            page(&mut out, "@top", title, n + 1, true);
        }
        for chunk in indexes.chunks(500) {
            let rows: Vec<String> = chunk
                .iter()
                .map(|&i| {
                    offer += 1;
                    let (name, item, offer_id) = &items[i];
                    catalog_item_values(&new_id("@offer", offer), item, *offer_id, name)
                })
                .collect();
            out.push_str(&format!("INSERT INTO `catalog_items` {CATALOG_ITEMS_COLUMNS} VALUES\n{};\n", rows.join(",\n")));
        }
    }
    out.push_str(&format!("SELECT CONCAT('PAGE=', @top);\nSELECT 'PAGES={}';\nCOMMIT;\n", groups.len()));
    out
}

const INSPECT: &str = r#"{{DatabaseName}}
set -o pipefail
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run: run Install New Server first."
for table in items_base catalog_pages catalog_items; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'")" = 1 ] ||
    fail "$db has no $table table: run the Emulator step first (the emulator creates its tables when it starts)."
done
echo "DB=$db"
mariadb -N -B -r "$db" -e "SELECT 'B', id, sprite_id, type, item_name FROM items_base"
mariadb -N -B -r "$db" -e "SELECT 'C', item_ids, offer_id, id, catalog_name FROM catalog_items"
mariadb -N -B -r "$db" -e "SELECT 'P', id, caption FROM catalog_pages""#;

/// Parses the inspect output: "B<TAB>id<TAB>sprite<TAB>type<TAB>name" and "C<TAB>item_ids<TAB>offer_id" lines.
pub fn parse_state(output: &str) -> DbState {
    let mut state = DbState::default();
    // items_base rows that may get a catalogue item: (id, name, sprite_id).
    let mut furni: Vec<(i64, String, i64)> = Vec::new();
    let mut sold: HashSet<i64> = HashSet::new();
    // The offer id the catalogue gives a sprite already (a furni sold twice keeps one offer).
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(db) = line.strip_prefix("DB=") {
            state.database = db.trim().to_string();
            continue;
        }
        let fields: Vec<&str> = line.splitn(6, '\t').collect();
        match fields.as_slice() {
            ["B", id, sprite, kind, name] => {
                state.items_base += 1;
                state.names.insert(name.trim().to_lowercase());
                if let (Ok(sprite), Some(kind)) = (sprite.parse::<i64>(), kind.chars().next()) {
                    state.sprites.entry(name.trim().to_lowercase()).or_insert((sprite, kind));
                }
                // Floor and wall furni only; pets ("a0 pet0") and the like have no furni of their own. (HabFurniScan)
                if let (Ok(id), true) = (id.parse::<i64>(), matches!(*kind, "s" | "i") && !name.starts_with("a0")) {
                    furni.push((id, name.to_string(), sprite.parse().unwrap_or(0)));
                }
            }
            ["P", id, caption] => {
                if let Ok(id) = id.parse() {
                    state.pages.insert(id, caption.to_string());
                }
            }
            ["C", ids, offer, rest @ ..] => {
                state.catalog_items += 1;
                if let Ok(offer) = offer.trim().parse::<i64>() {
                    if offer > 0 {
                        state.offer_ids.insert(offer);
                    }
                    if let [id, name] = rest {
                        if let Ok(id) = id.trim().parse::<i64>() {
                            state.catalog.entry(name.trim().to_lowercase()).or_default().push((id, offer));
                        }
                    }
                }
                sold.extend(ids.split([',', ';']).filter_map(|id| id.trim().split(':').next()?.trim().parse::<i64>().ok()));
            }
            _ => {}
        }
    }
    state.uncatalogued = furni
        .into_iter()
        .filter(|(id, _, _)| !sold.contains(id))
        .map(|(id, name, sprite_id)| Uncatalogued { id, name, sprite_id, offer_id: -1 })
        .collect();
    state
}

/// Reads what the hotel database has (only reads).
pub async fn inspect(server: &dyn Server) -> Result<DbState> {
    let body = fill(INSPECT, &[("DatabaseName", &server_tools::database_name())]);
    let result = server.run_as_root(&script::with_prelude(&body)).await?;
    if !result.succeeded() {
        let reason = result.output.lines().find_map(|l| l.strip_prefix("ERROR: ")).map(str::to_string).unwrap_or_else(|| result.error.trim().to_string());
        return Err(crate::server::ServerError(format!("Reading the hotel database failed: {reason}")).into());
    }
    Ok(parse_state(&result.output))
}

/// The offer ids of FurnitureData.json by classname (lower case), for catalogue items of furni already in items_base.
fn offer_ids(furnidata: &Value) -> BTreeMap<String, i64> {
    let mut offers = BTreeMap::new();
    for section in ["roomitemtypes", "wallitemtypes"] {
        for entry in furnidata[section]["furnitype"].as_array().map(Vec::as_slice).unwrap_or_default() {
            if let (Some(name), Some(offer)) = (entry.get("classname").and_then(Value::as_str), entry.get("offerid").and_then(Value::as_i64)) {
                offers.entry(name.to_lowercase()).or_insert(offer);
            }
        }
    }
    offers
}

/// What the import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportResult {
    pub safety_backup: String,
    /// The new page (with more pages: the folder above them), 0 without one.
    pub page_id: i64,
    /// The pages the catalogue items went on.
    pub pages: i64,
}

const IMPORT: &str = r#"{{DatabaseName}}
set -o pipefail
sql={{SqlFile}}
out=$(mktemp)
trap 'rm -f "$sql" "$out"' EXIT
[ -s "$sql" ] || fail "The uploaded SQL file is missing or empty."
chmod 600 "$sql"
echo "> Importing into $db ($(du -h "$sql" | cut -f1))"
if ! mariadb -N "$db" < "$sql" > "$out" 2>&1; then
  { grep -E '^ERROR' "$out" || tail -n 3 "$out"; } | head -n 5 | cut -c1-300 | sed 's/^/  /'
  fail "The import failed (see above): nothing was changed, it ran as one transaction."
fi
grep -E '^PAGES?=' "$out" || true"#;

/// Runs the uploaded SQL of [`sql`] on the hotel database, after a safety backup of it.
pub async fn import(server: &dyn Server, remote_sql: &str, log: Log<'_>) -> Result<ImportResult> {
    import_with(server, remote_sql, true, log).await
}

/// [`import`], with the backup first only when `backup` (the fixes let the user turn it off); without it
/// `safety_backup` is empty.
pub async fn import_with(server: &dyn Server, remote_sql: &str, backup: bool, log: Log<'_>) -> Result<ImportResult> {
    let safety_backup = if backup {
        server_tools::backup_database(server, log).await?.remote_path
    } else {
        log("> No backup first (turned off)");
        String::new()
    };
    let body = fill(IMPORT, &[("DatabaseName", &server_tools::database_name()), ("SqlFile", &crate::server::shell_quote(remote_sql))]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Importing the furniture failed", &["PAGE", "PAGES"], None).await?;
    Ok(ImportResult { safety_backup, page_id: values.long("PAGE"), pages: values.long("PAGES") })
}

const PUT_FURNITURE_DATA: &str = r#"set -o pipefail
json={{JsonFile}}
trap 'rm -f "$json"' EXIT
[ -s "$json" ] || fail "The uploaded FurnitureData.json is missing or empty."
gamedata={{Gamedata}}
config="$gamedata/config"
[ -d "$gamedata" ] || fail "$gamedata does not exist: run the Octane step first."
install -d -o www-data -g www-data -m 2775 "$config"
if [ -f "$config/FurnitureData.json" ]; then
  old="$config/FurnitureData.json.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$config/FurnitureData.json" "$old"
  echo "  The old one is kept as $old"
  echo "OLD=$old"
fi
install -o www-data -g www-data -m 664 "$json" "$config/FurnitureData.json"
echo "  $config/FurnitureData.json ($(du -h "$config/FurnitureData.json" | cut -f1))""#;

/// Puts the uploaded `remote_json` in gamedata/config as FurnitureData.json; the one that was there is kept next to
/// it as .bak-<time> (returned).
pub async fn put_furniture_data(server: &dyn Server, remote_json: &str, log: Log<'_>) -> Result<Option<String>> {
    log("> FurnitureData.json to the server");
    let body = fill(
        PUT_FURNITURE_DATA,
        &[("Gamedata", &crate::server::shell_quote(&crate::octane::gamedata())), ("JsonFile", &crate::server::shell_quote(remote_json))],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Putting FurnitureData.json on the server failed", &["OLD"], None).await?;
    Ok(values.string("OLD"))
}

/// A file the client needs for an imported furni: where it is here, and where it goes below gamedata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFile {
    pub local: std::path::PathBuf,
    /// "furniture" (the .hab bundle) or "icons" (<classname>_icon.png).
    pub folder: &'static str,
    pub name: String,
    pub size: u64,
}

/// The .hab bundles and icons of `furni` that are in `gamedata` (a downloaded gamedata folder): one bundle per base
/// name, one icon per colour ("rare_dragonlamp*4" -> rare_dragonlamp_4_icon.png).
pub fn client_files(furni: &[NewFurni], gamedata: &Path) -> Vec<ClientFile> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    // The real names in each folder: on Windows "Chair.hab" opens chair.hab too, but the server is case-sensitive,
    // so the name sent must be the one on disk. The exact name first, else the lower-case one.
    let mut listings: HashMap<&'static str, HashSet<String>> = HashMap::new();
    let mut add = |folder: &'static str, name: String| {
        let dir = gamedata.join(folder);
        let names = listings.entry(folder).or_insert_with(|| {
            std::fs::read_dir(&dir)
                .map(|entries| entries.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect())
                .unwrap_or_default()
        });
        let found = [name.clone(), name.to_lowercase()].into_iter().find(|n| names.contains(n)).map(|n| (dir.join(&n), n));
        if let Some((local, name)) = found {
            if seen.insert((folder, name.to_lowercase())) {
                let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
                files.push(ClientFile { local, folder, name, size });
            }
        }
    };
    for f in furni {
        add("furniture", format!("{}.hab", bundle_name(&f.classname)));
        add("icons", format!("{}_icon.png", f.classname.trim().replace('*', "_")));
    }
    files
}

const LIST_CLIENT_FILES: &str = r#"g={{Gamedata}}
for folder in furniture icons; do
  [ -d "$g/$folder" ] && find "$g/$folder" -maxdepth 1 -type f -printf "$folder/%f\n"
done
true"#;

/// The `files` the server's gamedata does not have yet. Names count exactly: the web server is case-sensitive, so
/// Leet_Chair.hab there does not serve the client's leet_chair.hab.
pub async fn missing_on_server(server: &dyn Server, files: &[ClientFile]) -> Result<Vec<ClientFile>> {
    let body = fill(LIST_CLIENT_FILES, &[("Gamedata", &crate::server::shell_quote(&crate::octane::gamedata()))]);
    let output = script::root_output(server, &body, "Listing the server's furniture files failed").await?;
    let there: HashSet<&str> = output.lines().map(str::trim).collect();
    Ok(files.iter().filter(|f| !there.contains(format!("{}/{}", f.folder, f.name).as_str())).cloned().collect())
}

const UPLOAD_CLIENT_FILES: &str = r#"set -e
install -d -m 2775 -o www-data -g www-data {{target}}
tar -x -f - -C {{target}} --no-same-owner --no-same-permissions
chown www-data:www-data {{target}}/* 2>/dev/null || true
echo "OK""#;

/// Uploads `files` into the server's gamedata (furniture/ and icons/), owned by www-data like the rest of it.
/// `progress` gets the bytes sent so far, over all of them.
pub async fn upload_client_files(server: &dyn Server, files: &[ClientFile], progress: crate::server::Progress<'_>) -> Result<usize> {
    let sent = std::sync::atomic::AtomicU64::new(0);
    for folder in ["furniture", "icons"] {
        let part: Vec<&ClientFile> = files.iter().filter(|f| f.folder == folder).collect();
        if part.is_empty() {
            continue;
        }
        let target = crate::server::shell_quote(&format!("{}/{folder}", crate::octane::gamedata()));
        let script = fill(UPLOAD_CLIENT_FILES, &[("target", &target)]);
        let items: Vec<crate::asset_sync::tar_stream::TarItem> = part.iter().map(|f| crate::asset_sync::tar_stream::TarItem::file(&f.local, &f.name)).collect();
        let before = sent.load(std::sync::atomic::Ordering::Relaxed);
        let result = crate::asset_sync::tar_stream::upload(server, &script, &items, &|bytes| progress(before + bytes)).await?;
        if !result.succeeded() || !result.output.contains("OK") {
            return Err(crate::server::ServerError(format!("Unpacking the {folder} files on the server failed: {}", format!("{}{}", result.output, result.error).trim())).into());
        }
        sent.store(before + part.iter().map(|f| f.size).sum::<u64>(), std::sync::atomic::Ordering::Relaxed);
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundle(width: f64, length: f64, height: f64, modes: i64) -> Option<Bundle> {
        Some(Bundle { width, length, height, modes, multiheight: None })
    }

    /// bc_block_1_recolorable.hab: logicType furniture_recolorable, 5 states whose images step down.
    fn recolorable_json(tops: [[i64; 4]; 5]) -> Value {
        let mut assets = serde_json::Map::new();
        for (state, ys) in tops.iter().enumerate() {
            for (layer, y) in ys.iter().enumerate() {
                let letter = (b'a' + layer as u8) as char;
                assets.insert(format!("blk_rc_64_{letter}_0_{state}"), json!({ "y": y }));
            }
        }
        let mut animations = serde_json::Map::new();
        for state in 0..5 {
            let layers: serde_json::Map<String, Value> =
                (0..4).map(|l| (l.to_string(), json!({"frameSequences": {"0": {"frames": {"0": {"id": state}}}}}))).collect();
            animations.insert(state.to_string(), json!({ "layers": layers }));
        }
        json!({
            "name": "blk_rc",
            "logicType": "furniture_recolorable",
            "logic": {"model": {"dimensions": {"x": 1, "y": 1, "z": 1}}},
            "visualizations": [{"size": 64, "layerCount": 4, "directions": {"0": {}, "90": {}}, "animations": animations}],
            "assets": assets,
        })
    }

    #[test]
    fn a_recolorable_whose_states_move_is_multiheight() {
        let moving = recolorable_json([[52, 52, 35, 53], [44, 44, 27, 45], [35, 35, 18, 36], [26, 26, 9, 27], [17, 17, 1, 18]]);
        assert_eq!(multiheight_heights(&moving).map(|h| h.column()).as_deref(), Some("1.00;0.75;0.50;0.25;0.10"));
        let still = recolorable_json([[52, 52, 35, 53]; 5]);
        assert_eq!(multiheight_heights(&still), None, "images that stay put: no heights");
    }

    #[test]
    fn bundle_json_like_the_converter() {
        let json = json!({
            "logic": {"model": {"dimensions": {"x": 2, "y": 3, "z": 0.7}}},
            "visualizations": [{"size": 32}, {"animations": {"0": {}, "1": {}, "3": {}, "99": {}}}]
        });
        assert_eq!(bundle_from_json(&json), Bundle { width: 2.0, length: 3.0, height: 0.7, modes: 4, multiheight: None });
        // Without an animation 0: their number; without any: 1; without dimensions: 1 x 1 x 0.
        assert_eq!(interaction_modes_count(&json!({"visualizations": [{"animations": {"1": {}, "2": {}}}]})), 2);
        assert_eq!(bundle_from_json(&json!({})), Bundle { width: 1.0, length: 1.0, height: 0.0, modes: 1, multiheight: None });
        assert_eq!(dimensions(&json!({"logic": {"model": {"dimensions": {"x": 2}}}})), Some((2.0, 1.0, 0.0)));
        assert_eq!(interaction_type("bc_block_recolorable"), "recolorable");
        assert_eq!(interaction_type("chair_norja"), "default");
        assert_eq!(bundle_name("rare_dragonlamp*5"), "rare_dragonlamp");
        assert_eq!((multiheight_count("1.00;0.75;0.50"), multiheight_count("0.8,2.0,3.0,4.0,5.0"), multiheight_count("0"), multiheight_count("")), (3, 5, 0, 0));
    }

    #[test]
    fn the_plan_takes_what_the_database_lacks() {
        let furnidata = json!({
            "roomitemtypes": {"furnitype": [
                {"id": 30, "classname": "chair_norja", "offerid": 5, "cansiton": true},
                {"id": 31, "classname": "table_norja_med", "offerid": 6},
                {"id": 32, "classname": "rare_dragonlamp*4", "offerid": -1},
                {"id": 33, "classname": "rare_dragonlamp*0"},
                {"id": 34, "classname": "bed_trad", "canlayon": true},
                {"id": 35, "classname": "missing_bundle"},
                {"id": 36, "classname": "TABLE_NORJA_MED"}
            ]},
            "wallitemtypes": {"furnitype": [{"id": 4001, "classname": "poster", "offerid": 7}]}
        });
        let db = DbState {
            names: ["chair_norja".to_string()].into(),
            uncatalogued: vec![Uncatalogued { id: 9, name: "Chair_Norja".into(), sprite_id: 30, offer_id: -1 }],
            ..DbState::default()
        };
        let read = |name: &str| match name {
            "table_norja_med" => bundle(2.0, 2.0, 0.8, 1),
            "rare_dragonlamp" => bundle(1.0, 1.0, 1.0, 2),
            "bed_trad" => bundle(2.0, 3.0, 1.9, 1),
            "poster" => bundle(1.0, 1.0, 0.5, 1),
            _ => None,
        };
        let plan = plan_with(&furnidata, &db, &read);
        let names: Vec<&str> = plan.new.iter().map(|f| f.classname.as_str()).collect();
        assert_eq!(names, ["table_norja_med", "rare_dragonlamp*4", "rare_dragonlamp*0", "bed_trad", "poster"]);
        assert_eq!(plan.no_bundle, ["missing_bundle"]);
        assert!(plan.capped.is_empty());
        // A height items_base cannot hold gets the nearest one that fits.
        let tall = json!({"roomitemtypes": {"furnitype": [{"id": 1, "classname": "tower"}, {"id": 2, "classname": "pit"}]}});
        let tall_read = |name: &str| bundle(1.0, 1.0, if name == "tower" { 150.0 } else { -120.5 }, 1);
        let tall = plan_with(&tall, &DbState::default(), &tall_read);
        assert_eq!(tall.new.iter().map(|f| f.height).collect::<Vec<_>>(), [99.99, -99.99]);
        assert_eq!(tall.capped, [("tower".to_string(), 150.0), ("pit".to_string(), -120.5)]);
        // Too long for item_name: left out; ids that do not fit int(11): none / -1; public_name cut at 56.
        let long_name = "x".repeat(71);
        let odd = json!({"roomitemtypes": {"furnitype": [{"id": 1, "classname": long_name}, {"id": 3_000_000_000_i64, "offerid": 4_000_000_000_i64, "classname": "y".repeat(60)}]}});
        let odd = plan_with(&odd, &DbState::default(), &|_| bundle(1.0, 1.0, 1.0, 1));
        assert_eq!(odd.too_long, [long_name]);
        assert_eq!((odd.new.len(), odd.new[0].sprite_id, odd.new[0].offer_id), (1, None, -1));
        assert!(items_base_values(&odd.new[0], "@i").contains(&format!(", '{}', '{}', ", "y".repeat(60), "y".repeat(56))));
        assert_eq!((plan.floor(), plan.wall(), plan.variants()), (4, 1, 2));
        assert_eq!(plan.uncatalogued[0].offer_id, 5, "the offer id of its furnidata entry");

        let table = &plan.new[0];
        assert_eq!((table.width, table.length, table.height, table.allow_stack, table.sprite_id, table.offer_id), (2.0, 2.0, 0.8, true, Some(31), 6));
        // Height 1.0 is not stackable; the variant uses its base's bundle; no offerid is -1.
        let lamp = &plan.new[2];
        assert_eq!((lamp.allow_stack, lamp.modes, lamp.offer_id, lamp.sprite_id), (false, 2, -1, Some(33)));
        // A bed is laid on, so not stackable.
        assert!(plan.new[3].allow_lay && !plan.new[3].allow_stack);
        // A wall item is 1 x 1 x 0, stackable worked out before (0.5 != 1).
        let poster = &plan.new[4];
        assert_eq!((poster.kind, poster.width, poster.length, poster.height, poster.allow_stack), ('i', 1.0, 1.0, 0.0, true));
    }

    #[test]
    fn rows_like_the_converter() {
        let furni = NewFurni {
            classname: "chair_o'neil".into(),
            sprite_id: Some(18),
            offer_id: 5,
            kind: 's',
            width: 1.0,
            length: 1.0,
            height: 1.1,
            allow_stack: false,
            allow_sit: true,
            allow_lay: false,
            allow_walk: false,
            interaction_type: "default",
            modes: 2,
            multiheight: None,
        };
        assert_eq!(
            items_base_values(&furni, "@item + 1"),
            r"(@item + 1, 18, 'chair_o\'neil', 'chair_o\'neil', 1, 1, 1.1, '0', '1', '0', '0', '1', '1', '0', '1', '1', 's', 'default', 2, '0', '0', '0', 0, 0, '0')"
        );
        assert_eq!(
            catalog_item_values("@offer + 1", "@item + 1", -1, "wf_xtra_array_capture_variable"),
            "(@offer + 1, CAST(@item + 1 AS CHAR), @page, -1, 0, 99, 'wf_xtra_array_capture_variable', 5, 0, 0, 1, 0, 0, '', '1', '0', 0, 0)"
        );
        assert_eq!(page_caption(chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()), "import_20261105");
        let plan = Plan { new: vec![furni], uncatalogued: vec![Uncatalogued { id: 77, name: "lamp".into(), sprite_id: 9, offer_id: 3 }], ..Plan::default() };
        let text = sql(&plan, "import_20261105", -1, false, true, 500);
        assert!(text.contains("START TRANSACTION;") && text.trim_end().ends_with("COMMIT;"));
        assert!(text.contains("VALUES (-1, 'default_3x3', 'import_20261105', 'import_20261105', 1, 1, 1, 99, '0', '1');"));
        assert!(text.contains("(CAST(@offer + 2 AS UNSIGNED), CAST(77 AS CHAR), @page, 3, 0, 99, 'lamp',"), "{text}");
        assert!(text.contains("(CAST(@item + 1 AS UNSIGNED), 18, 'chair_o"), "{text}");
        assert!(!sql(&plan, "x", -1, true, false, 500).contains("'lamp'"));
        let moved = Plan { moved: vec![Moved { name: "leet_table".into(), kind: 's', from: 600, to: 123456 }], ..Plan::default() };
        assert_eq!(moved.work(false), 1);
        let text = sql(&moved, "x", -1, false, false, 500);
        assert!(text.contains("UPDATE `items_base` SET `sprite_id` = 123456 WHERE `item_name` = 'leet_table' AND `type` = 's' AND `sprite_id` = 600;"), "{text}");
        assert!(!text.contains("catalog_pages") && text.trim_end().ends_with("COMMIT;"), "no page without catalogue items");
        // Name groups stay together; a group bigger than a page gets pages of its own.
        assert_eq!(name_group("adobe_kaars2"), "adobe");
        assert_eq!(name_group("rare_dragonlamp*4"), "rare");
        assert_eq!(name_group("Duck"), "duck");
        let names = ["adobe_kaars2", "bath_a", "adobe_kekook", "rare_x*1", "rare_x*2", "rare_y", "bed_a", "bed_b", "chair"];
        let got: Vec<(String, Vec<&str>)> = pages(&names, 3).into_iter().map(|(c, i)| (c, i.into_iter().map(|i| names[i]).collect())).collect();
        assert_eq!(
            got,
            vec![
                ("adobe - bath".to_string(), vec!["adobe_kaars2", "adobe_kekook", "bath_a"]),
                ("bed - chair".to_string(), vec!["bed_a", "bed_b", "chair"]),
                ("rare".to_string(), vec!["rare_x*1", "rare_x*2", "rare_y"]),
            ]
        );
        let got: Vec<String> = pages(&names, 2).into_iter().map(|(c, _)| c).collect();
        assert_eq!(got, ["adobe", "bath", "bed", "chair", "rare-1", "rare-2"]);
        let many = Plan { new: (0..5).map(|i| NewFurni { classname: format!("grp_{i}"), ..plan.new[0].clone() }).collect(), ..Plan::default() };
        let text = sql(&many, "import_x", 7, false, false, 2);
        assert!(text.contains("VALUES (7, 'default_3x3', 'import_x', 'import_x', 1, 1, 1, 99, '0', '1');"), "the folder, hidden");
        assert!(text.contains("VALUES (@top, 'default_3x3', 'grp-1', 'grp-1', 1, 1, 1, 1, '1', '1');") && text.contains("'grp-3'"), "{text}");
        assert!(text.contains("SELECT 'PAGES=3';"));
        assert!(!sql(&many, "import_x", 7, false, false, 500).contains("@top, 'default_3x3'"), "one page when they fit");
        assert!(sql(&plan, "x", 2, true, false, 500).contains("VALUES (2, 'default_3x3', 'x', 'x', 1, 1, 1, 99, '1', '1');"));
    }

    #[test]
    fn the_files_of_the_imported_furni() {
        let dir = std::env::temp_dir().join(format!("aio-client-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("furniture")).unwrap();
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        for f in ["furniture/rare_dragonlamp.hab", "furniture/chair.hab", "icons/rare_dragonlamp_4_icon.png", "icons/chair_icon.png"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let furni = |name: &str| NewFurni {
            classname: name.into(),
            sprite_id: None,
            offer_id: -1,
            kind: 's',
            width: 1.0,
            length: 1.0,
            height: 1.0,
            allow_stack: false,
            allow_sit: false,
            allow_lay: false,
            allow_walk: false,
            interaction_type: "default",
            modes: 1,
            multiheight: None,
        };
        let files = client_files(&[furni("rare_dragonlamp*4"), furni("rare_dragonlamp*5"), furni("Chair"), furni("gone")], &dir);
        let names: Vec<String> = files.iter().map(|f| format!("{}/{}", f.folder, f.name)).collect();
        // One bundle per base name, an icon only where there is one, the lower-case file for "Chair".
        assert_eq!(names, ["furniture/rare_dragonlamp.hab", "icons/rare_dragonlamp_4_icon.png", "furniture/chair.hab", "icons/chair_icon.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_exception_wins() {
        let furnidata = json!({"roomitemtypes": {"furnitype": [{"id": 1, "classname": "olympics_c16_pommel"}, {"id": 2, "classname": "olympics_c16_pommel*2"}]}});
        let read = |_: &str| Some(Bundle { width: 1.0, length: 2.0, height: 1.0, modes: 1, multiheight: None });
        let plan = plan_with(&furnidata, &DbState::default(), &read);
        for f in &plan.new {
            assert_eq!(f.interaction_type, "multiheight");
            assert_eq!((f.modes, f.height, f.allow_sit, f.allow_stack, f.allow_walk), (5, 2.0, true, false, false));
            assert_eq!(f.multiheight.as_deref(), Some("2.00;2.25;2.50;2.75;3.00"));
            assert_eq!((f.width, f.length), (1.0, 2.0), "what the exception does not set comes from the .hab");
        }
        assert!(items_base_values(&plan.new[0], "@i").contains("2, '0', '1', '0', '0', '1', '1', '0', '1', '1', 's', 'multiheight', 5, '0', '2.00;2.25;2.50;2.75;3.00'"), "{}", items_base_values(&plan.new[0], "@i"));
    }

    #[test]
    fn reads_the_database_state() {
        let output = "DB=hotel\nP\t2\tMeubels\nB\t1\t13\ts\tshelves_norja\nB\t2\t14\ts\tChair_Norja\nB\t3\t15\ti\tposter\nB\t4\t16\ts\ta0 pet0\nB\t5\t17\tr\trobot\nC\t1,3\t5\t40\tshelves_norja\nC\t\t-1\t41\tdeal\n";
        let state = parse_state(output);
        assert_eq!(state.database, "hotel");
        assert_eq!((state.items_base, state.catalog_items), (5, 2));
        assert!(state.names.contains("chair_norja"));
        assert_eq!(state.pages.get(&2).map(String::as_str), Some("Meubels"));
        assert_eq!(state.sprites.get("chair_norja"), Some(&(14, 's')));
        assert_eq!(state.catalog.get("shelves_norja"), Some(&vec![(40, 5)]));
        assert!(state.offer_ids.contains(&5) && !state.offer_ids.contains(&-1));
        assert_eq!(state.uncatalogued, vec![Uncatalogued { id: 2, name: "Chair_Norja".into(), sprite_id: 14, offer_id: -1 }]);
    }
}
