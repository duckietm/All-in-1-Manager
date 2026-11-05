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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bundle {
    pub width: f64,
    pub length: f64,
    pub height: f64,
    pub modes: i64,
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
    Bundle { width, length, height, modes: interaction_modes_count(json) }
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
    pub catalog_items: usize,
}

/// What an import would add.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub new: Vec<NewFurni>,
    /// Furnidata classnames the database lacks but whose bundle is not in the furniture folder (or unreadable).
    pub no_bundle: Vec<String>,
    pub uncatalogued: Vec<Uncatalogued>,
}

impl Plan {
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
        let Some(bundle) = bundles.get(&bundle_name(&classname).to_lowercase()).copied().flatten() else {
            plan.no_bundle.push(classname);
            continue;
        };
        let (allow_sit, allow_lay, allow_walk) = (flag(entry, "cansiton"), flag(entry, "canlayon"), flag(entry, "canstandon"));
        // Stackable: a height other than 1.0 and nothing to sit or lie on (worked out before a wall item's reset).
        let allow_stack = bundle.height != 1.0 && !allow_sit && !allow_lay;
        let (width, length, height) = if kind == 'i' { (1.0, 1.0, 0.0) } else { (bundle.width, bundle.length, bundle.height) };
        plan.new.push(NewFurni {
            sprite_id: entry.get("id").and_then(Value::as_i64),
            offer_id: entry.get("offerid").and_then(Value::as_i64).unwrap_or(-1),
            kind,
            width,
            length,
            height,
            allow_stack,
            allow_sit,
            allow_lay,
            allow_walk,
            interaction_type: interaction_type(&classname),
            modes: bundle.modes,
            classname,
        });
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
fn read_all(names: &[String], read: &(dyn Fn(&str) -> Option<Bundle> + Sync)) -> HashMap<String, Option<Bundle>> {
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
    let sprite = furni.sprite_id.map_or_else(|| id.to_string(), |s| s.to_string());
    format!(
        "({id}, {sprite}, {name}, {name}, {}, {}, {}, {}, {}, {}, {}, '1', '1', '0', '1', '1', '{}', '{}', {}, '0', '0', '0', 0, 0, '0')",
        number(furni.width),
        number(furni.length),
        number(furni.height),
        yes(furni.allow_stack),
        yes(furni.allow_sit),
        yes(furni.allow_lay),
        yes(furni.allow_walk),
        furni.kind,
        furni.interaction_type,
        furni.modes
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

/// The import as one transaction: the page, then the new items_base rows with their catalogue items, and (with
/// `with_uncatalogued`) a catalogue item for every items_base row that has none. The new ids follow the highest ones
/// in the tables, so nothing the hotel has is overwritten.
pub fn sql(plan: &Plan, caption: &str, visible: bool, with_uncatalogued: bool) -> String {
    let mut out = String::new();
    out.push_str("-- Import Assets (All-in-1 Installer): Habbo furniture the database did not have.\n");
    out.push_str(&format!(
        "-- {} new furniture ({} floor, {} wall), {} catalogue items for furniture without one.\n",
        plan.new.len(),
        plan.floor(),
        plan.wall(),
        if with_uncatalogued { plan.uncatalogued.len() } else { 0 }
    ));
    out.push_str("SET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    out.push_str(&format!(
        "INSERT INTO `catalog_pages` (`parent_id`, `page_layout`, `caption_save`, `caption`, `icon_color`, `icon_image`, `min_rank`, `order_num`, `visible`, `enabled`) VALUES (-1, 'default_3x3', {c}, {c}, 1, 1, 1, 99, {}, '1');\n",
        yes(visible),
        c = sql_literal(caption)
    ));
    out.push_str("SET @page = LAST_INSERT_ID();\n");
    out.push_str("SELECT COALESCE(MAX(`id`), 0) INTO @item FROM `items_base`;\n");
    out.push_str("SELECT COALESCE(MAX(`id`), 0) INTO @offer FROM `catalog_items`;\n");
    // In batches, so one statement stays small.
    for (batch, furni) in plan.new.chunks(500).enumerate() {
        let start = batch * 500;
        let rows: Vec<String> = furni.iter().enumerate().map(|(i, f)| items_base_values(f, &new_id("@item", start + i + 1))).collect();
        out.push_str(&format!("INSERT INTO `items_base` {ITEMS_BASE_COLUMNS} VALUES\n{};\n", rows.join(",\n")));
        let rows: Vec<String> = furni
            .iter()
            .enumerate()
            .map(|(i, f)| catalog_item_values(&new_id("@offer", start + i + 1), &new_id("@item", start + i + 1), f.offer_id, &f.classname))
            .collect();
        out.push_str(&format!("INSERT INTO `catalog_items` {CATALOG_ITEMS_COLUMNS} VALUES\n{};\n", rows.join(",\n")));
    }
    if with_uncatalogued {
        let offset = plan.new.len();
        for (batch, rows) in plan.uncatalogued.chunks(500).enumerate() {
            let values: Vec<String> = rows
                .iter()
                .enumerate()
                .map(|(i, row)| catalog_item_values(&new_id("@offer", offset + batch * 500 + i + 1), &row.id.to_string(), row.offer_id, &row.name))
                .collect();
            out.push_str(&format!("INSERT INTO `catalog_items` {CATALOG_ITEMS_COLUMNS} VALUES\n{};\n", values.join(",\n")));
        }
    }
    out.push_str("SELECT CONCAT('PAGE=', @page);\nCOMMIT;\n");
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
mariadb -N -B -r "$db" -e "SELECT 'C', item_ids, offer_id FROM catalog_items""#;

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
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["B", id, sprite, kind, name] => {
                state.items_base += 1;
                state.names.insert(name.trim().to_lowercase());
                // Floor and wall furni only; pets ("a0 pet0") and the like have no furni of their own. (HabFurniScan)
                if let (Ok(id), true) = (id.parse::<i64>(), matches!(*kind, "s" | "i") && !name.starts_with("a0")) {
                    furni.push((id, name.to_string(), sprite.parse().unwrap_or(0)));
                }
            }
            ["C", ids, _offer] => {
                state.catalog_items += 1;
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
    pub page_id: i64,
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
grep -E '^PAGE=' "$out" || true"#;

/// Runs the uploaded SQL of [`sql`] on the hotel database, after a safety backup of it.
pub async fn import(server: &dyn Server, remote_sql: &str, log: Log<'_>) -> Result<ImportResult> {
    let backup = server_tools::backup_database(server, log).await?;
    let body = fill(IMPORT, &[("DatabaseName", &server_tools::database_name()), ("SqlFile", &crate::server::shell_quote(remote_sql))]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Importing the furniture failed", &["PAGE"], None).await?;
    Ok(ImportResult { safety_backup: backup.remote_path, page_id: values.long("PAGE") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundle(width: f64, length: f64, height: f64, modes: i64) -> Option<Bundle> {
        Some(Bundle { width, length, height, modes })
    }

    #[test]
    fn bundle_json_like_the_converter() {
        let json = json!({
            "logic": {"model": {"dimensions": {"x": 2, "y": 3, "z": 0.7}}},
            "visualizations": [{"size": 32}, {"animations": {"0": {}, "1": {}, "3": {}, "99": {}}}]
        });
        assert_eq!(bundle_from_json(&json), Bundle { width: 2.0, length: 3.0, height: 0.7, modes: 4 });
        // Without an animation 0: their number; without any: 1; without dimensions: 1 x 1 x 0.
        assert_eq!(interaction_modes_count(&json!({"visualizations": [{"animations": {"1": {}, "2": {}}}]})), 2);
        assert_eq!(bundle_from_json(&json!({})), Bundle { width: 1.0, length: 1.0, height: 0.0, modes: 1 });
        assert_eq!(dimensions(&json!({"logic": {"model": {"dimensions": {"x": 2}}}})), Some((2.0, 1.0, 0.0)));
        assert_eq!(interaction_type("bc_block_recolorable"), "recolorable");
        assert_eq!(interaction_type("chair_norja"), "default");
        assert_eq!(bundle_name("rare_dragonlamp*5"), "rare_dragonlamp");
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
        let text = sql(&plan, "import_20261105", false, true);
        assert!(text.contains("START TRANSACTION;") && text.trim_end().ends_with("COMMIT;"));
        assert!(text.contains("VALUES (-1, 'default_3x3', 'import_20261105', 'import_20261105', 1, 1, 1, 99, '0', '1');"));
        assert!(text.contains("(CAST(@offer + 2 AS UNSIGNED), CAST(77 AS CHAR), @page, 3, 0, 99, 'lamp',"), "{text}");
        assert!(text.contains("(CAST(@item + 1 AS UNSIGNED), 18, 'chair_o"), "{text}");
        assert!(!sql(&plan, "x", true, false).contains("'lamp'"));
    }

    #[test]
    fn reads_the_database_state() {
        let output = "DB=hotel\nB\t1\t13\ts\tshelves_norja\nB\t2\t14\ts\tChair_Norja\nB\t3\t15\ti\tposter\nB\t4\t16\ts\ta0 pet0\nB\t5\t17\tr\trobot\nC\t1,3\t5\nC\t\t-1\n";
        let state = parse_state(output);
        assert_eq!(state.database, "hotel");
        assert_eq!((state.items_base, state.catalog_items), (5, 2));
        assert!(state.names.contains("chair_norja"));
        assert_eq!(state.uncatalogued, vec![Uncatalogued { id: 2, name: "Chair_Norja".into(), sprite_id: 14, offer_id: -1 }]);
    }
}
