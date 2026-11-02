//! The language of the furniture (names and descriptions in FurnitureData.json): the furnidata of one of the official
//! Habbo hotels, converted to JSON and put in gamedata/config, and the database brought in line with it. Ported from
//! the All-in-1 Converter: Download Classes/Furnidata.cs (the download), Model/FurnitureDataModel.cs (XML to JSON),
//! Database/FixSpriteIDSettings.cs and Database/SetOfferID.cs (the database).
//!
//! Two things differ from the converter, both to keep what the hotel has: furni the hotel has but the official
//! furnidata does not (custom furni) stay in FurnitureData.json, and a catalogue item that sells several furni (a
//! bundle, "1;2") keeps them instead of being reduced to one.

use std::collections::{HashMap, HashSet};

use rand::Rng;
use serde_json::{json, Value};

use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server, ServerError};
use crate::{emulator, octane, server_tools};

/// A hotel whose furnidata can be used: the language, its code in the window, and the hotel's domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FurniLanguage {
    pub code: &'static str,
    pub name: &'static str,
    pub domain: &'static str,
}

/// The official hotels (All-in-1 Converter: Tools/HabboHotels.cs), in the order of the request.
pub const LANGUAGES: [FurniLanguage; 9] = [
    FurniLanguage { code: "en", name: "English", domain: "habbo.com" },
    FurniLanguage { code: "es", name: "Spanish", domain: "habbo.es" },
    FurniLanguage { code: "pt", name: "Portuguese", domain: "habbo.com.br" },
    FurniLanguage { code: "de", name: "German", domain: "habbo.de" },
    FurniLanguage { code: "fr", name: "French", domain: "habbo.fr" },
    FurniLanguage { code: "it", name: "Italian", domain: "habbo.it" },
    FurniLanguage { code: "nl", name: "Dutch", domain: "habbo.nl" },
    FurniLanguage { code: "fi", name: "Finnish", domain: "habbo.fi" },
    FurniLanguage { code: "tr", name: "Turkish", domain: "habbo.com.tr" },
];

pub fn language(code: &str) -> Option<FurniLanguage> {
    LANGUAGES.iter().copied().find(|l| l.code == code)
}

/// The converter's browser identity: Habbo refuses requests without one.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

pub fn xml_url(language: FurniLanguage) -> String {
    format!("https://www.{}/gamedata/furnidata_xml/1", language.domain)
}

/// The marker with the code of the furniture language this installer put in gamedata/config.
pub fn marker() -> String {
    format!("{}/config/.all-in-1-furni-language", octane::gamedata())
}

/// Downloads the hotel's furnidata XML (on this computer).
pub async fn download_xml(language: FurniLanguage) -> Result<String> {
    download(&xml_url(language)).await
}

/// A text file from a Habbo hotel, with the converter's browser identity (on this computer).
pub(crate) async fn download(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| ServerError(e.to_string()))?;
    let response = client.get(url).send().await.map_err(|e| ServerError(format!("Could not download {url}: {e}")))?;
    if !response.status().is_success() {
        return Err(ServerError(format!("{url} answered {}", response.status())).into());
    }
    response.text().await.map_err(|e| ServerError(format!("Could not download {url}: {e}")).into())
}

// All text inside an element, like XElement's string cast.
fn text_of(node: roxmltree::Node) -> String {
    node.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect()
}

// The text of a child element, or None when there is no such child.
fn child_text(node: roxmltree::Node, name: &str) -> Option<String> {
    node.children().find(|c| c.has_tag_name(name)).map(text_of)
}

fn child_int(node: roxmltree::Node, name: &str) -> i64 {
    child_text(node, name).and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

fn child_flag(node: roxmltree::Node, name: &str) -> bool {
    child_int(node, name) == 1
}

/// One <furnitype> as the converter writes it (FurnitureType.FromXElement); `floor` for <roomitemtypes>.
fn furnitype(node: roxmltree::Node, floor: bool) -> Value {
    let partcolors = if floor {
        node.children().find(|c| c.has_tag_name("partcolors")).map(|p| {
            let colors: Vec<String> = p
                .children()
                .filter(|c| c.has_tag_name("color"))
                .map(text_of)
                .filter(|c| !c.is_empty())
                .map(|c| if c.starts_with('#') { c } else { format!("#{c}") })
                .collect();
            json!({ "color": colors })
        })
    } else {
        None
    };
    json!({
        "id": node.attribute("id").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0),
        "classname": node.attribute("classname").unwrap_or(""),
        "revision": child_int(node, "revision"),
        "category": child_text(node, "category").unwrap_or_else(|| "unknown".into()),
        "defaultdir": if floor { child_int(node, "defaultdir") } else { 0 },
        "xdim": if floor { child_int(node, "xdim") } else { 0 },
        "ydim": if floor { child_int(node, "ydim") } else { 0 },
        "partcolors": partcolors,
        "name": child_text(node, "name").unwrap_or_default(),
        "description": child_text(node, "description").unwrap_or_default(),
        "adurl": child_text(node, "adurl").unwrap_or_default(),
        "offerid": child_int(node, "offerid"),
        "buyout": child_flag(node, "buyout"),
        "rentofferid": child_int(node, "rentofferid"),
        "rentbuyout": child_flag(node, "rentbuyout"),
        "bc": child_flag(node, "bc"),
        "excludeddynamic": child_flag(node, "excludeddynamic"),
        "customparams": child_text(node, "customparams").unwrap_or_default(),
        "specialtype": child_int(node, "specialtype"),
        "canstandon": floor && child_flag(node, "canstandon"),
        "cansiton": floor && child_flag(node, "cansiton"),
        "canlayon": floor && child_flag(node, "canlayon"),
        "furniline": child_text(node, "furniline").unwrap_or_default(),
        "environment": child_text(node, "environment").unwrap_or_default(),
        "rare": child_flag(node, "rare"),
    })
}

/// The furnidata XML as FurnitureData.json (FurnidataConverter.ConvertXmlToJson).
pub fn xml_to_json(xml: &str) -> Result<Value> {
    let document = roxmltree::Document::parse(xml).map_err(|e| ServerError(format!("The furnidata is not valid XML: {e}")))?;
    let root = document.root_element();
    if !root.has_tag_name("furnidata") {
        return Err(ServerError("Invalid XML format: missing 'furnidata' root element.".into()).into());
    }
    let list = |section: &str, floor: bool| -> Vec<Value> {
        root.children()
            .find(|c| c.has_tag_name(section))
            .map(|s| s.children().filter(|c| c.has_tag_name("furnitype")).map(|f| furnitype(f, floor)).collect())
            .unwrap_or_default()
    };
    let room = list("roomitemtypes", true);
    let wall = list("wallitemtypes", false);
    if room.is_empty() && wall.is_empty() {
        return Err(ServerError("The furnidata has no furniture.".into()).into());
    }
    Ok(json!({ "roomitemtypes": { "furnitype": room }, "wallitemtypes": { "furnitype": wall } }))
}

fn furnitypes_mut<'a>(data: &'a mut Value, section: &str) -> Option<&'a mut Vec<Value>> {
    data.get_mut(section)?.get_mut("furnitype")?.as_array_mut()
}

fn furnitypes<'a>(data: &'a Value, section: &str) -> &'a [Value] {
    data.get(section).and_then(|s| s.get("furnitype")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// Every furnitype, floor and wall.
pub fn all_items(data: &Value) -> impl Iterator<Item = &Value> {
    furnitypes(data, "roomitemtypes").iter().chain(furnitypes(data, "wallitemtypes"))
}

fn classname(item: &Value) -> &str {
    item.get("classname").and_then(Value::as_str).unwrap_or("")
}

fn id_of(item: &Value) -> i64 {
    item.get("id").and_then(Value::as_i64).unwrap_or(0)
}

/// Adds the furni of `hotel` that the official furnidata does not have (custom furni, by classname) to it, with their
/// own names, exactly as the hotel had them. Returns how many were added.
pub fn keep_custom_furni(official: &mut Value, hotel: &Value) -> usize {
    let mut classnames: HashSet<String> = all_items(official).map(|i| classname(i).to_string()).collect();
    let mut added = 0;
    for section in ["roomitemtypes", "wallitemtypes"] {
        let extra: Vec<Value> =
            furnitypes(hotel, section).iter().filter(|i| !classname(i).is_empty() && !classnames.contains(classname(i))).cloned().collect();
        for item in extra {
            classnames.insert(classname(&item).to_string());
            if let Some(list) = furnitypes_mut(official, section) {
                list.push(item);
                added += 1;
            }
        }
    }
    added
}

/// A row of items_base: id, item_name, sprite_id, public_name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseItem {
    pub id: i64,
    pub item_name: String,
    pub sprite_id: i64,
    pub public_name: String,
}

/// A row of catalog_items: id, catalog_name, item_ids, offer_id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogItem {
    pub id: i64,
    pub catalog_name: String,
    pub item_ids: String,
    pub offer_id: i64,
}

/// What the database needs to match the furnidata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixPlan {
    /// items_base: (id, new sprite_id).
    pub sprite_ids: Vec<(i64, i64)>,
    /// catalog_items: (id, new item_ids).
    pub item_ids: Vec<(i64, String)>,
    /// Bundles (several item ids) the converter would have reduced to one: kept.
    pub bundles_kept: usize,
    /// catalog_items: (id, new offer_id).
    pub offer_ids: Vec<(i64, i64)>,
    /// Furni without an offer id in the furnidata that got a new one (also written into the furnidata).
    pub offer_ids_generated: usize,
}

/// FixSpriteIDSettings and then SetOfferID: what to change in the database, and the new offer ids written into `furni`.
pub fn plan(furni: &mut Value, base: &[BaseItem], catalog: &[CatalogItem], rng: &mut impl Rng) -> FixPlan {
    let mut plan = FixPlan::default();

    // FixSpriteIDSettings: the sprite id of every furni is the furnidata's id for its classname (the last one wins).
    let mut sprite_by_class: HashMap<&str, i64> = HashMap::new();
    for item in all_items(furni) {
        sprite_by_class.insert(classname(item), id_of(item));
    }
    plan.sprite_ids =
        base.iter().filter_map(|b| sprite_by_class.get(b.item_name.as_str()).filter(|&&s| s != b.sprite_id).map(|&s| (b.id, s))).collect();

    // ... and a catalogue item sells the furni whose public name is its catalogue name; the *_single wallpapers,
    // floors and landscapes sell the wallpaper, floor and landscape furni (or what the catalogue item of that name sells).
    let first_by_public: HashMap<&str, i64> = base.iter().rev().map(|b| (b.public_name.as_str(), b.id)).collect();
    let by_name = |name: &str| -> Option<String> {
        base.iter()
            .find(|b| b.item_name == name)
            .map(|b| b.id.to_string())
            .or_else(|| catalog.iter().find(|c| c.catalog_name == name && c.id > 0).map(|c| c.item_ids.clone()))
    };
    let (wallpaper, floor, landscape) = (by_name("wallpaper"), by_name("floor"), by_name("landscape"));
    for c in catalog {
        let new = if c.catalog_name.starts_with("wallpaper_single") && wallpaper.is_some() {
            wallpaper.clone()
        } else if c.catalog_name.starts_with("floor_single") && floor.is_some() {
            floor.clone()
        } else if c.catalog_name.starts_with("landscape_single") && landscape.is_some() {
            landscape.clone()
        } else {
            first_by_public.get(c.catalog_name.as_str()).map(i64::to_string)
        };
        if let Some(new) = new.filter(|n| *n != c.item_ids) {
            if c.item_ids.contains([';', ',']) && !new.contains([';', ',']) {
                plan.bundles_kept += 1;
            } else {
                plan.item_ids.push((c.id, new));
            }
        }
    }

    // SetOfferID: per classname the furnidata's offer id; -1 gets a new unique one above 60000, in the furnidata too.
    let mut used: HashSet<i64> = catalog.iter().map(|c| c.offer_id).filter(|&o| o > 0).collect();
    let mut offer_by_class: HashMap<String, i64> = HashMap::new();
    for item in all_items(furni) {
        offer_by_class.entry(classname(item).to_string()).or_insert_with(|| item.get("offerid").and_then(Value::as_i64).unwrap_or(-1));
    }
    let mut names: Vec<String> = offer_by_class.keys().cloned().collect();
    names.sort();
    for name in names {
        if offer_by_class[&name] == -1 {
            let id = loop {
                let id = rng.gen_range(60_001..i64::from(i32::MAX));
                if used.insert(id) {
                    break id;
                }
            };
            offer_by_class.insert(name, id);
            plan.offer_ids_generated += 1;
        }
    }
    for section in ["roomitemtypes", "wallitemtypes"] {
        if let Some(list) = furnitypes_mut(furni, section) {
            for item in list.iter_mut() {
                if item.get("offerid").and_then(Value::as_i64) == Some(-1) {
                    if let Some(&id) = offer_by_class.get(classname(item)) {
                        if let Some(map) = item.as_object_mut() {
                            map.insert("offerid".into(), Value::from(id));
                        }
                    }
                }
            }
        }
    }
    plan.offer_ids = catalog.iter().filter_map(|c| offer_by_class.get(&c.catalog_name).filter(|&&o| o != c.offer_id).map(|&o| (c.id, o))).collect();
    plan
}

fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The plan as UPDATE statements in batches of 100 (CASE id WHEN ... THEN ...), like the converter.
pub fn plan_sql(plan: &FixPlan) -> String {
    let mut sql = String::from("SET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    let mut batches = |table: &str, column: &str, rows: Vec<(i64, String)>| {
        for chunk in rows.chunks(100) {
            let cases: String = chunk.iter().map(|(id, v)| format!(" WHEN {id} THEN {v}")).collect();
            let ids: Vec<String> = chunk.iter().map(|(id, _)| id.to_string()).collect();
            sql += &format!("UPDATE {table} SET {column} = CASE id{cases} ELSE {column} END WHERE id IN ({});\n", ids.join(","));
        }
    };
    batches("items_base", "sprite_id", plan.sprite_ids.iter().map(|(id, s)| (*id, s.to_string())).collect());
    batches("catalog_items", "item_ids", plan.item_ids.iter().map(|(id, s)| (*id, sql_text(s))).collect());
    batches("catalog_items", "offer_id", plan.offer_ids.iter().map(|(id, o)| (*id, o.to_string())).collect());
    sql += "COMMIT;\n";
    sql
}

// The tables as tab-separated rows; tabs and line breaks in names would split a row, so they become spaces.
const READ_TABLES: &str = r#"{{DatabaseName}}
clean() { echo "REPLACE(REPLACE(REPLACE($1, CHAR(9), ' '), CHAR(10), ' '), CHAR(13), ' ')"; }
echo "@items_base"
mariadb --default-character-set=utf8mb4 -N -r "$db" -e "SELECT id, $(clean item_name), sprite_id, $(clean public_name) FROM items_base"
echo "@catalog_items"
mariadb --default-character-set=utf8mb4 -N -r "$db" -e "SELECT id, $(clean catalog_name), $(clean item_ids), offer_id FROM catalog_items""#;

/// items_base and catalog_items of the hotel database.
pub async fn read_tables(server: &dyn Server) -> Result<(Vec<BaseItem>, Vec<CatalogItem>)> {
    let result = server.run_as_root(&fill(READ_TABLES, &[("DatabaseName", &server_tools::database_name())])).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("Reading items_base and catalog_items failed: {}", result.error.trim())).into());
    }
    Ok(parse_tables(&result.output))
}

fn parse_tables(output: &str) -> (Vec<BaseItem>, Vec<CatalogItem>) {
    let (mut base, mut catalog) = (Vec::new(), Vec::new());
    let mut section = "";
    for line in output.lines() {
        if line.starts_with('@') {
            section = line;
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 4 {
            continue;
        }
        let (Ok(id), Ok(number)) = (f[0].parse::<i64>(), f[if section == "@items_base" { 2 } else { 3 }].parse::<i64>()) else { continue };
        match section {
            "@items_base" => base.push(BaseItem { id, item_name: f[1].into(), sprite_id: number, public_name: f[3].into() }),
            "@catalog_items" => catalog.push(CatalogItem { id, catalog_name: f[1].into(), item_ids: f[2].into(), offer_id: number }),
            _ => {}
        }
    }
    (base, catalog)
}

/// The JSON as FurnitureData.json: indented, characters as they are (like the converter's UnsafeRelaxedJsonEscaping).
pub fn to_file(furni: &Value) -> String {
    serde_json::to_string_pretty(furni).unwrap_or_default()
}

/// What putting the furnidata in place did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageResult {
    pub safety_backup: String,
    pub old_furniture_data: Option<String>,
}

const APPLY: &str = r#"set -o pipefail
{{DatabaseName}}
json={{JsonFile}}
sql=$(mktemp)
out=$(mktemp)
cleanup() { rm -f "$json" "$sql" "$out"; }
trap cleanup EXIT
# The UPDATE statements come over stdin.
cat > "$sql"
[ -s "$json" ] || fail "The uploaded FurnitureData.json is missing or empty."
mariadb -N "$db" -e 'SELECT 1 FROM items_base LIMIT 1' >/dev/null 2>&1 || fail "The hotel database $db has no items_base: run the Emulator step first."
gamedata={{Gamedata}}
config="$gamedata/config"
[ -d "$gamedata" ] || fail "$gamedata does not exist: run the Octane step first."

echo "> Safety backup of $db first"
install -d -m 700 {{BackupDirectory}}
safety={{BackupDirectory}}/$db-before-furni-language-$(date +%Y%m%d-%H%M%S).sql.gz
(umask 077; mariadb-dump --single-transaction --quick --routines --triggers --events --hex-blob --databases "$db" | gzip -6 > "$safety")
echo "  $safety ($(du -h "$safety" | cut -f1))"
echo "SAFETY=$safety"

d={{Directory}}
log={{LogDirectory}}/emulator.log
{{ControlFunctions}}
emulator=
# Without {{Manage}} the caller has stopped the emulator and starts it again afterwards.
if [ -n "{{Manage}}" ] && [ -x "$d/emulator" ]; then
  emulator=yes
  echo "> Stopping the emulator"
  stop_emulator
fi
restore_database() {
  echo "> Putting $db back as it was"
  gunzip -c "$safety" | mariadb || echo "  Restoring failed: import $safety with Tools → Import full database."
}

echo "> Updating items_base and catalog_items ({{Summary}})"
if ! mariadb "$db" < "$sql" > "$out" 2>&1; then
  { grep -E '^ERROR' "$out" || tail -n 3 "$out"; } | head -n 5 | cut -c1-300 | sed 's/^/  /'
  restore_database
  [ -n "$emulator" ] && { echo "> Starting the emulator"; start_emulator; (wait_loaded) || true; }
  fail "Updating the database failed (see above). It is as it was before."
fi

echo "> $config/FurnitureData.json ({{Language}})"
install -d -o www-data -g www-data -m 2775 "$config"
old=
if [ -f "$config/FurnitureData.json" ]; then
  old="$config/FurnitureData.json.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$config/FurnitureData.json" "$old"
  echo "  The old one is kept as $old"
  echo "OLD=$old"
fi
install -o www-data -g www-data -m 664 "$json" "$config/FurnitureData.json"

if [ -n "$emulator" ]; then
  echo "> Starting the emulator"
  start_emulator
  if ! (wait_loaded); then
    echo "> The emulator does not start with the new furniture data"
    restore_database
    if [ -n "$old" ]; then cp -p "$old" "$config/FurnitureData.json"; else rm -f "$config/FurnitureData.json"; fi
    start_emulator
    (wait_loaded) || true
    fail "The emulator does not start with the new furniture data (its log is above). The database and FurnitureData.json are as they were."
  fi
fi
printf '%s\n' {{LanguageCode}} > {{Marker}}
chown www-data:www-data {{Marker}}
true"#;

/// Uploads `json_file` (the new FurnitureData.json, already on the server) into gamedata/config and runs the plan's
/// SQL, after a safety backup, with the emulator stopped; puts everything back when something fails.
/// With `manage_emulator` it stops the emulator first and starts it at the end; without, the caller does both.
pub async fn apply(server: &dyn Server, json_file: &str, language: FurniLanguage, plan: &FixPlan, manage_emulator: bool, log: Log<'_>) -> Result<LanguageResult> {
    let summary = format!(
        "{} sprite ids, {} catalogue items, {} offer ids",
        plan.sprite_ids.len(),
        plan.item_ids.len(),
        plan.offer_ids.len()
    );
    let body = fill(
        APPLY,
        &[
            ("DatabaseName", &server_tools::database_name()),
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("BackupDirectory", &server_tools::backup_directory()),
            ("Directory", &emulator::directory()),
            ("LogDirectory", &emulator::log_directory()),
            ("Summary", &summary),
            ("Language", &format!("{}, {}", language.name, language.domain)),
            ("LanguageCode", language.code),
            ("Manage", if manage_emulator { "yes" } else { "" }),
            ("Marker", &shell_quote(&marker())),
            ("ControlFunctions", &emulator::control_functions()),
            ("JsonFile", &shell_quote(json_file)),
        ],
    );
    let values =
        script::run_capturing(server, &script::with_prelude(&body), log, "Setting the furniture language failed", &["SAFETY", "OLD"], Some(&plan_sql(plan))).await?;
    Ok(LanguageResult { safety_backup: values.or_empty("SAFETY").to_string(), old_furniture_data: values.string("OLD") })
}

/// What switching the furniture language did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwitchResult {
    pub furni: usize,
    pub custom_kept: usize,
    pub plan: FixPlan,
    pub applied: LanguageResult,
}

/// A downloaded and converted furnidata, with the hotel's own furni added: ready to put in place.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub language: FurniLanguage,
    pub furni: Value,
    pub custom_kept: usize,
}

/// Downloads the hotel's furnidata (on this computer), converts it and adds the furni of `hotel` that it does not have.
/// Nothing on the server changes, so it can run while the hotel is still open.
pub async fn prepare(language: FurniLanguage, hotel: Option<&Value>, log: Log<'_>) -> Result<Prepared> {
    log(&format!("> Downloading the furnidata of {} ({})", language.domain, xml_url(language)));
    let xml = download_xml(language).await?;
    let mut furni = xml_to_json(&xml)?;
    log(&format!("  {} furni, {:.1} MB", all_items(&furni).count(), xml.len() as f64 / (1024.0 * 1024.0)));
    let custom_kept = hotel.map(|h| keep_custom_furni(&mut furni, h)).unwrap_or(0);
    if custom_kept > 0 {
        log(&format!("  {custom_kept} furni of the hotel that {} does not have are kept, with their own names", language.domain));
    }
    Ok(Prepared { language, furni, custom_kept })
}

/// Works out the database fixes for a prepared furnidata and puts both in place (see `apply` for `manage_emulator`).
pub async fn finish(server: &dyn Server, prepared: Prepared, manage_emulator: bool, log: Log<'_>) -> Result<SwitchResult> {
    let Prepared { language, mut furni, custom_kept } = prepared;
    log("> Reading items_base and catalog_items");
    let (base, catalog) = read_tables(server).await?;
    let plan = plan(&mut furni, &base, &catalog, &mut rand::thread_rng());
    log(&format!(
        "  {} furni, {} catalogue items: {} sprite ids, {} catalogue items and {} offer ids to change{}; {} new offer ids",
        base.len(),
        catalog.len(),
        plan.sprite_ids.len(),
        plan.item_ids.len(),
        plan.offer_ids.len(),
        if plan.bundles_kept > 0 { format!(" ({} bundles kept as they are)", plan.bundles_kept) } else { String::new() },
        plan.offer_ids_generated
    ));
    // Through a file on this computer, because the upload sends files.
    let local = std::env::temp_dir().join(format!("all-in-1-furnidata-{}-{}.json", std::process::id(), chrono::Local::now().format("%Y%m%d-%H%M%S%f")));
    std::fs::write(&local, to_file(&furni))?;
    let directory = server_tools::backup_directory();
    let remote = format!("{directory}/furnidata-{}-{}.json", language.code, chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let uploaded = async {
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        server.upload(&local, &remote, &|_| {}).await
    }
    .await;
    let _ = std::fs::remove_file(&local);
    uploaded?;
    let applied = apply(server, &remote, language, &plan, manage_emulator, log).await?;
    Ok(SwitchResult { furni: all_items(&furni).count(), custom_kept, plan, applied })
}

/// Records the language when the catalogue's own FurnitureData.json is used as it is.
pub async fn mark(server: &dyn Server, language: FurniLanguage) -> Result<()> {
    let marker = shell_quote(&marker());
    server.run_as_root(&format!("printf '%s\\n' {} > {marker} && chown www-data:www-data {marker}", language.code)).await?;
    Ok(())
}

const RESTORE: &str = r#"set -o pipefail
safety={{Safety}}
config={{Gamedata}}/config
[ -s "$safety" ] || fail "The safety backup $safety is missing: import a backup with Tools → Import full database."
echo "> Putting the database back as it was ($(basename "$safety"))"
gunzip -c "$safety" | mariadb || fail "Restoring failed: import $safety with Tools → Import full database."
old={{Old}}
if [ -n "$old" ] && [ -f "$old" ]; then
  cp -p "$old" "$config/FurnitureData.json"
  echo "  FurnitureData.json as it was ($(basename "$old"))"
fi
previous={{Previous}}
if [ -n "$previous" ]; then printf '%s\n' "$previous" > {{Marker}}; else rm -f {{Marker}}; fi
true"#;

/// Puts the database (from `safety`), FurnitureData.json (the `old` copy) and the language marker (`previous`) back as
/// they were before a switch, when the emulator does not load afterwards.
pub async fn restore(server: &dyn Server, safety: &str, old: Option<&str>, previous: Option<&str>, log: Log<'_>) -> Result<()> {
    let previous: String = previous.unwrap_or("").chars().filter(char::is_ascii_lowercase).collect();
    let body = fill(
        RESTORE,
        &[
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("Marker", &shell_quote(&marker())),
            ("Old", &shell_quote(old.unwrap_or(""))),
            ("Previous", &shell_quote(&previous)),
            ("Safety", &shell_quote(safety)),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Putting the furniture language back failed", None).await
}

/// The furniture language this installer put on the server last (its code), if any.
pub async fn installed(server: &dyn Server) -> Result<Option<String>> {
    let result = server.run_as_root(&format!("head -c 8 {} 2>/dev/null | tr -cd 'a-z'", shell_quote(&marker()))).await?;
    Ok(Some(result.output.trim().to_string()).filter(|c| !c.is_empty()))
}

/// The hotel's FurnitureData.json now (for the furni the official furnidata does not have), or None.
pub async fn current_furniture_data(server: &dyn Server) -> Result<Option<Value>> {
    let result = server.run_as_root(&format!("cat {} 2>/dev/null", shell_quote(&format!("{}/config/FurnitureData.json", octane::gamedata())))).await?;
    Ok(serde_json::from_str(result.output.trim_start_matches('\u{feff}')).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<furnidata>
  <roomitemtypes>
    <furnitype id="13" classname="shelves_norja">
      <revision>61856</revision><defaultdir>0</defaultdir><xdim>1</xdim><ydim>1</ydim>
      <partcolors><color>ffffff</color><color>#F7EBBC</color></partcolors>
      <name>Beige Bookcase</name><description>For nic nacs &amp; art books</description>
      <adurl/><offerid>5</offerid><buyout>1</buyout><rentofferid>-1</rentofferid><rentbuyout>0</rentbuyout><bc>1</bc>
      <excludeddynamic>0</excludeddynamic><customparams/><specialtype>1</specialtype>
      <canstandon>0</canstandon><cansiton>0</cansiton><canlayon>0</canlayon><furniline>iced</furniline><category>shelf</category>
    </furnitype>
    <furnitype id="14" classname="chair_norja"><name>Chair</name><offerid>-1</offerid><cansiton>1</cansiton></furnitype>
  </roomitemtypes>
  <wallitemtypes>
    <furnitype id="1" classname="post.it"><revision>0</revision><name>Pad of stickies</name><offerid>-1</offerid><specialtype>5</specialtype></furnitype>
  </wallitemtypes>
</furnidata>"#;

    #[test]
    fn xml_to_json_like_the_converter() {
        let data = xml_to_json(XML).unwrap();
        let room = furnitypes(&data, "roomitemtypes");
        assert_eq!(room.len(), 2);
        let shelf = &room[0];
        let keys: Vec<&str> = shelf.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys[..8], ["id", "classname", "revision", "category", "defaultdir", "xdim", "ydim", "partcolors"]);
        assert_eq!(shelf["partcolors"]["color"], json!(["#ffffff", "#F7EBBC"]));
        assert_eq!(shelf["description"], "For nic nacs & art books");
        assert_eq!((shelf["buyout"].as_bool(), shelf["bc"].as_bool(), shelf["category"].as_str()), (Some(true), Some(true), Some("shelf")));
        assert_eq!(room[1]["category"], "unknown");
        assert!(room[1]["partcolors"].is_null() && room[1]["cansiton"] == true);
        let wall = &furnitypes(&data, "wallitemtypes")[0];
        assert!(wall["partcolors"].is_null() && wall["canstandon"] == false);
        assert_eq!(wall["specialtype"], 5);
        assert!(xml_to_json("<other/>").is_err());
        assert!(xml_to_json("not xml").is_err());
    }

    #[test]
    fn custom_furni_stay() {
        let mut official = xml_to_json(XML).unwrap();
        let hotel = json!({
            "roomitemtypes": { "furnitype": [
                { "id": 13, "classname": "shelves_norja", "name": "Boekenkast" },
                { "id": 900001, "classname": "my_custom_sofa", "name": "Mijn bank" },
                { "id": 14, "classname": "clashing_id", "name": "Botsing" }
            ]},
            "wallitemtypes": { "furnitype": [ { "id": 900002, "classname": "my_poster", "name": "Poster" } ] }
        });
        assert_eq!(keep_custom_furni(&mut official, &hotel), 3);
        let names: Vec<&str> = all_items(&official).map(classname).collect();
        assert!(names.contains(&"my_custom_sofa") && names.contains(&"my_poster") && names.contains(&"clashing_id"));
        assert_eq!(keep_custom_furni(&mut official, &hotel), 0, "only once");
        assert_eq!(furnitypes(&official, "roomitemtypes")[0]["name"], "Beige Bookcase", "the official name wins");
    }

    fn base(id: i64, name: &str, sprite: i64, public: &str) -> BaseItem {
        BaseItem { id, item_name: name.into(), sprite_id: sprite, public_name: public.into() }
    }

    fn cat(id: i64, name: &str, items: &str, offer: i64) -> CatalogItem {
        CatalogItem { id, catalog_name: name.into(), item_ids: items.into(), offer_id: offer }
    }

    #[test]
    fn plan_like_fix_sprite_id_and_set_offer_id() {
        let mut furni = xml_to_json(XML).unwrap();
        let base = vec![
            base(1, "shelves_norja", 99, "shelves_norja"),
            base(2, "chair_norja", 14, "chair_norja"),
            base(3, "post.it", 1, "post.it"),
            base(4, "wallpaper", 3001, "wallpaper"),
            base(5, "custom_thing", 777, "custom_thing"),
        ];
        let catalog = vec![
            cat(10, "shelves_norja", "1", 1),
            cat(11, "chair_norja", "2", 50),
            cat(12, "wallpaper_single_101", "99", 0),
            cat(13, "post.it", "3;3", 0),
            cat(14, "custom_thing", "5", 70),
        ];
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = plan(&mut furni, &base, &catalog, &mut rng);
        assert_eq!(plan.sprite_ids, vec![(1, 13)], "only the shelf's sprite differs");
        assert_eq!(plan.item_ids, vec![(12, "4".to_string())], "the wallpaper single sells the wallpaper furni");
        assert_eq!(plan.bundles_kept, 1, "post.it 3;3 stays a bundle");
        assert_eq!(plan.offer_ids_generated, 2, "chair_norja and post.it had -1");
        let offers: HashMap<i64, i64> = plan.offer_ids.iter().copied().collect();
        assert_eq!(offers[&10], 5, "the furnidata's offer id");
        let chair = offers[&11];
        assert!(chair > 60_000 && chair < i64::from(i32::MAX));
        assert!(!offers.contains_key(&14), "custom furni keep their offer id");
        let chair_json = furnitypes(&furni, "roomitemtypes").iter().find(|i| classname(i) == "chair_norja").unwrap();
        assert_eq!(chair_json["offerid"], chair, "the new offer id is in the furnidata too");
        let sql = plan_sql(&plan);
        assert!(sql.contains("UPDATE items_base SET sprite_id = CASE id WHEN 1 THEN 13 ELSE sprite_id END WHERE id IN (1);"));
        assert!(sql.contains("UPDATE catalog_items SET item_ids = CASE id WHEN 12 THEN '4' ELSE item_ids END WHERE id IN (12);"));
        assert!(sql.starts_with("SET NAMES utf8mb4;\nSTART TRANSACTION;") && sql.ends_with("COMMIT;\n"));
    }

    #[test]
    fn batches_of_100() {
        let plan = FixPlan { sprite_ids: (1..=250).map(|i| (i, i + 1)).collect(), ..Default::default() };
        assert_eq!(plan_sql(&plan).matches("UPDATE items_base").count(), 3);
    }

    #[test]
    fn tables() {
        let (base, catalog) = parse_tables("@items_base\n1\tshelves_norja\t13\tShelf\nbad line\n@catalog_items\n10\tshelves_norja\t1;2\t5\n");
        assert_eq!(base, vec![BaseItem { id: 1, item_name: "shelves_norja".into(), sprite_id: 13, public_name: "Shelf".into() }]);
        assert_eq!(catalog, vec![CatalogItem { id: 10, catalog_name: "shelves_norja".into(), item_ids: "1;2".into(), offer_id: 5 }]);
        assert_eq!(language("pt").unwrap().domain, "habbo.com.br");
        assert_eq!(xml_url(language("en").unwrap()), "https://www.habbo.com/gamedata/furnidata_xml/1");
    }
}
