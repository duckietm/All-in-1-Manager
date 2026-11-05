//! Fix items_base: the values of furni already in items_base brought in line with FurnitureData.json and their .hab
//! files, the way Generate SQL works them out for a new furni (see furni_import). (All-in-1 Converter:
//! FixSpriteIDSettings.cs, FixItemSize.cs, FixItemSettings.cs, FixInteractionCount.cs)
//!
//! A row is matched to FurnitureData.json by item_name = classname. What can be fixed, each on its own:
//! - sprite_id: the FurnitureData.json id;
//! - type: 's' for a floor entry, 'i' for a wall one (on its own: furni players placed keep their floor or wall
//!   position, so a changed type can leave them broken in rooms);
//! - width / length / stack_height: logic.model.dimensions of the .hab, whole tiles (at least 1) and a height with
//!   two decimals; a wall furni is 1 x 1 x 0.00;
//! - allow_sit / allow_lay / allow_walk: cansiton / canlayon / canstandon;
//! - interaction_modes_count: from the .hab's animations, only for interaction_type 'default' (other interactions
//!   count their own states);
//!
//! allow_stack is never touched: no file has it (the .hab only knows the height, FurnitureData.json sit / lay /
//! walk), and Generate SQL's guess (a height other than 1.0) undoes hand-made values like a stackable table.
//!
//! Left alone: furni other than floor and wall (pets "a0 ...", effects, bots), and every multiheight furni
//! (interaction_type 'multiheight', or a multiheight value): their heights are set by hand per state.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::furni_import::{bundle_name, multiheight_count, Bundle, MAX_STACK_HEIGHT};
use crate::script::{self, fill};
use crate::server::{Result, Server, ServerError};
use crate::server_tools;

/// One items_base row as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRow {
    pub id: i64,
    pub name: String,
    pub kind: char,
    pub sprite_id: i64,
    pub width: i64,
    pub length: i64,
    pub stack_height: f64,
    pub allow_stack: bool,
    pub allow_sit: bool,
    pub allow_lay: bool,
    pub allow_walk: bool,
    pub interaction_type: String,
    pub modes: i64,
    pub multiheight: String,
}

impl ItemRow {

    pub fn is_multiheight(&self) -> bool {
        self.interaction_type.trim().eq_ignore_ascii_case("multiheight") || !matches!(self.multiheight.trim(), "" | "0")
    }
}

/// What to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    pub sprite: bool,
    pub kind: bool,
    pub size: bool,
    pub flags: bool,
    pub modes: bool,
    /// Multiheight where the .hab says it is one (logicType furniture_multiheight) and items_base does not; a
    /// multiheight furni in items_base is never changed (many are by their .hab furniture_multistate or _basic).
    pub multiheight: bool,
}

impl Default for Fields {
    /// Everything but type: placed furni keep their floor or wall position.
    fn default() -> Self {
        Self { sprite: true, kind: false, size: true, flags: true, modes: true, multiheight: true }
    }
}

/// One value that changes, for the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub id: i64,
    pub name: String,
    pub column: &'static str,
    pub from: String,
    pub to: String,
}

/// What the fix changes.
#[derive(Debug, Clone, Default)]
pub struct FixPlan {
    pub changes: Vec<Change>,
    /// The SET of each row that changes: (id, [(column, SQL value)]).
    pub updates: Vec<(i64, Vec<(&'static str, String)>)>,
    /// Rows checked, and multiheight furni left alone.
    pub checked: usize,
    pub multiheight: usize,
    /// item_names FurnitureData.json does not have (left as they are).
    pub not_in_furnidata: Vec<String>,
    /// Multiheight in items_base, but their .hab is not furniture_multiheight (stairs and stages often are
    /// furniture_multistate): left as they are, listed to check.
    pub multiheight_not_by_hab: Vec<String>,
    /// (item_name, items_base.multiheight, the .hab's heights) where the number of heights differs: left as they are.
    pub multiheight_count_differs: Vec<(String, String, String)>,
    /// item_names whose .hab is not here: their size and modes are left as they are.
    pub no_bundle: Vec<String>,
}

impl FixPlan {
    pub fn rows(&self) -> usize {
        self.updates.len()
    }
}

fn flag(entry: &Value, name: &str) -> bool {
    match entry.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn yes(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

/// FurnitureData.json by classname: (entry, 's' or 'i'). The exact name, and the name in lower case.
pub fn index(furnidata: &Value) -> (HashMap<String, (Value, char)>, HashMap<String, (Value, char)>) {
    let (mut exact, mut lower) = (HashMap::new(), HashMap::new());
    for (section, kind) in [("roomitemtypes", 's'), ("wallitemtypes", 'i')] {
        for entry in furnidata[section]["furnitype"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let Some(name) = entry.get("classname").and_then(Value::as_str) else { continue };
            exact.entry(name.trim().to_string()).or_insert((entry.clone(), kind));
            lower.entry(name.trim().to_lowercase()).or_insert((entry.clone(), kind));
        }
    }
    (exact, lower)
}

/// The bundle names the rows need (for reading them once).
pub fn bundle_names(rows: &[ItemRow]) -> Vec<String> {
    // Multiheight furni too: their .hab is compared with their multiheight.
    let names: HashSet<String> = rows
        .iter()
        .filter(|r| matches!(r.kind, 's' | 'i') && !r.name.starts_with("a0"))
        .map(|r| bundle_name(&r.name).to_string())
        .filter(|n| !n.is_empty())
        .collect();
    names.into_iter().collect()
}

/// Works out the fix. `bundles` holds what each bundle (by name in lower case) gives, None when it is not here.
pub fn plan(rows: &[ItemRow], furnidata: &Value, bundles: &HashMap<String, Option<Bundle>>, fields: Fields) -> FixPlan {
    let (exact, lower) = index(furnidata);
    let mut plan = FixPlan::default();
    for row in rows {
        if !matches!(row.kind, 's' | 'i') || row.name.starts_with("a0") {
            continue;
        }
        let bundle = bundles.get(&bundle_name(&row.name).to_lowercase()).copied().flatten();
        let mut set: Vec<(&'static str, String)> = Vec::new();
        let mut changes: Vec<Change> = Vec::new();
        let mut change = |column: &'static str, from: String, to: String, sql: String| {
            if from != to {
                changes.push(Change { id: row.id, name: row.name.clone(), column, from, to });
                set.push((column, sql));
            }
        };
        // Multiheight as the .hab says it (logicType furniture_multiheight): only for the plain interactions, so a
        // gate or wired furni is never turned into another one.
        let mut multiheight = row.is_multiheight();
        let interaction = row.interaction_type.trim().to_lowercase();
        let recolorable = interaction == "recolorable" || row.name.to_lowercase().ends_with("_recolorable");
        if let (true, Some(bundle), true) = (fields.multiheight, bundle, matches!(interaction.as_str(), "default" | "multiheight" | "recolorable")) {
            match (bundle.multiheight, multiheight) {
                // Multiheight by its .hab, not in items_base: the interaction and a height per state.
                (Some(heights), false) => {
                    let to = if recolorable { "recolorable" } else { "multiheight" };
                    change("interaction_type", row.interaction_type.clone(), to.into(), format!("'{to}'"));
                    change("multiheight", row.multiheight.clone(), heights.column(), format!("'{}'", heights.column()));
                    change("interaction_modes_count", row.modes.to_string(), heights.len().to_string(), heights.len().to_string());
                    if !recolorable {
                        // The emulator takes the state's height; stack_height stays out of it.
                        change("stack_height", format!("{:.2}", row.stack_height), "0.00".into(), "0.00".into());
                    }
                    multiheight = true;
                }
                // Multiheight in both: the heights set by hand stay; a different number is only listed.
                (Some(heights), true) => {
                    if multiheight_count(&row.multiheight) != heights.len() {
                        plan.multiheight_count_differs.push((row.name.clone(), row.multiheight.clone(), heights.column()));
                    }
                }
                // Multiheight in items_base, not by its .hab: many really are (stairs, stages): only listed.
                (None, true) => plan.multiheight_not_by_hab.push(row.name.clone()),
                (None, false) => {}
            }
        }
        'fields: {
            if multiheight {
                // Its heights are set by hand per state: nothing else is touched.
                plan.multiheight += 1;
                break 'fields;
            }
            plan.checked += 1;
            let Some((entry, kind)) = exact.get(row.name.trim()).or_else(|| lower.get(&row.name.trim().to_lowercase())) else {
                plan.not_in_furnidata.push(row.name.clone());
                break 'fields;
            };
            let kind = *kind;
            let default_modes = interaction == "default";
            let needs_bundle = fields.modes && default_modes || fields.size && kind == 's';
            if bundle.is_none() && needs_bundle {
                plan.no_bundle.push(row.name.clone());
            }
            if fields.sprite {
                if let Some(id) = entry.get("id").and_then(Value::as_i64).filter(|id| (1..=i64::from(i32::MAX)).contains(id)) {
                    change("sprite_id", row.sprite_id.to_string(), id.to_string(), id.to_string());
                }
            }
            if fields.kind {
                change("type", row.kind.to_string(), kind.to_string(), format!("'{kind}'"));
            }
            let (sit, lay, walk) = (flag(entry, "cansiton"), flag(entry, "canlayon"), flag(entry, "canstandon"));
            if fields.flags {
                change("allow_sit", yes(row.allow_sit).into(), yes(sit).into(), format!("'{}'", yes(sit)));
                change("allow_lay", yes(row.allow_lay).into(), yes(lay).into(), format!("'{}'", yes(lay)));
                change("allow_walk", yes(row.allow_walk).into(), yes(walk).into(), format!("'{}'", yes(walk)));
            }
            if fields.size {
                // Whole tiles, at least 1, and a height with two decimals that stack_height (double(4,2)) holds.
                let size = match (kind, bundle) {
                    ('i', _) => Some((1, 1, 0.0)),
                    (_, Some(b)) => Some((
                        (b.width.round() as i64).max(1),
                        (b.length.round() as i64).max(1),
                        ((b.height * 100.0).round() / 100.0).clamp(-MAX_STACK_HEIGHT, MAX_STACK_HEIGHT),
                    )),
                    _ => None,
                };
                if let Some((width, length, height)) = size {
                    change("width", row.width.to_string(), width.to_string(), width.to_string());
                    change("length", row.length.to_string(), length.to_string(), length.to_string());
                    change("stack_height", format!("{:.2}", row.stack_height), format!("{height:.2}"), format!("{height:.2}"));
                }
            }
            if fields.modes && default_modes {
                if let Some(b) = bundle {
                    change("interaction_modes_count", row.modes.to_string(), b.modes.to_string(), b.modes.to_string());
                }
            }
        }
        plan.changes.extend(changes);
        if !set.is_empty() {
            plan.updates.push((row.id, set));
        }
    }
    plan.not_in_furnidata.sort_by_key(|n| n.to_lowercase());
    plan.no_bundle.sort_by_key(|n| n.to_lowercase());
    plan
}

/// The updates as one transaction; a multiheight furni is never touched, also when it became one meanwhile.
pub fn sql(plan: &FixPlan) -> String {
    let mut out = String::from("-- Fix items_base (All-in-1 Installer): values as FurnitureData.json and the .hab files give them.\nSET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    for (id, set) in &plan.updates {
        // A row whose multiheight this fix sets or removes is meant; every other one must not be a multiheight furni.
        let guard = if set.iter().any(|(column, _)| *column == "multiheight") { "" } else { " AND `interaction_type` <> 'multiheight' AND (`multiheight` IS NULL OR `multiheight` IN ('', '0'))" };
        let set: Vec<String> = set.iter().map(|(column, value)| format!("`{column}` = {value}")).collect();
        out.push_str(&format!("UPDATE `items_base` SET {} WHERE `id` = {id}{guard};\n", set.join(", ")));
    }
    out.push_str("COMMIT;\n");
    out
}

const READ: &str = r#"{{DatabaseName}}
set -o pipefail
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run: run Install New Server first."
[ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = 'items_base'")" = 1 ] ||
  fail "$db has no items_base table: run the Emulator step first."
echo "DB=$db"
mariadb -N -B -r "$db" -e "SELECT 'I', id, type, sprite_id, width, length, stack_height, allow_stack, allow_sit, allow_lay, allow_walk, interaction_modes_count, REPLACE(interaction_type, '\t', ' '), REPLACE(IFNULL(multiheight, ''), '\t', ' '), item_name FROM items_base""#;

/// Parses READ's output: (database, rows).
pub fn parse_items(output: &str) -> (String, Vec<ItemRow>) {
    let mut database = String::new();
    let mut rows = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(db) = line.strip_prefix("DB=") {
            database = db.trim().to_string();
            continue;
        }
        let f: Vec<&str> = line.splitn(15, '\t').collect();
        if f.len() != 15 || f[0] != "I" {
            continue;
        }
        let int = |s: &str| s.trim().parse::<i64>().unwrap_or(0);
        rows.push(ItemRow {
            id: int(f[1]),
            kind: f[2].chars().next().unwrap_or('?'),
            sprite_id: int(f[3]),
            width: int(f[4]),
            length: int(f[5]),
            stack_height: f[6].trim().parse().unwrap_or(0.0),
            allow_stack: f[7] == "1",
            allow_sit: f[8] == "1",
            allow_lay: f[9] == "1",
            allow_walk: f[10] == "1",
            modes: int(f[11]),
            interaction_type: f[12].to_string(),
            multiheight: f[13].to_string(),
            name: f[14].to_string(),
        });
    }
    (database, rows)
}

/// Reads items_base (only reads).
pub async fn read_items(server: &dyn Server) -> Result<(String, Vec<ItemRow>)> {
    let body = fill(READ, &[("DatabaseName", &server_tools::database_name())]);
    let result = server.run_as_root(&script::with_prelude(&body)).await?;
    if !result.succeeded() {
        let reason = result.output.lines().find_map(|l| l.strip_prefix("ERROR: ")).map(str::to_string).unwrap_or_else(|| result.error.trim().to_string());
        return Err(ServerError(format!("Reading items_base failed: {reason}")).into());
    }
    Ok(parse_items(&result.output))
}

/// Reads the bundles `names` from the first of `folders` that has them (<name>.hab, or the name in lower case), on a
/// few threads. Keyed by the name in lower case.
pub fn read_bundles(names: &[String], folders: &[std::path::PathBuf]) -> HashMap<String, Option<Bundle>> {
    let read = |name: &str| {
        folders.iter().find_map(|dir| {
            crate::furni_import::read_bundle(&dir.join(format!("{name}.hab"))).or_else(|| crate::furni_import::read_bundle(&dir.join(format!("{}.hab", name.to_lowercase()))))
        })
    };
    crate::furni_import::read_all(names, &read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: i64, name: &str, kind: char) -> ItemRow {
        ItemRow {
            id,
            name: name.into(),
            kind,
            sprite_id: 0,
            width: 1,
            length: 1,
            stack_height: 0.0,
            allow_stack: false,
            allow_sit: false,
            allow_lay: false,
            allow_walk: false,
            interaction_type: "default".into(),
            modes: 1,
            multiheight: "0".into(),
        }
    }

    #[test]
    fn like_generate_sql() {
        let furnidata = json!({
            "roomitemtypes": {"furnitype": [
                {"id": 41, "classname": "bed_polyfon", "canlayon": true},
                {"id": 30, "classname": "chair_norja", "cansiton": true},
                {"id": 50, "classname": "table_plasto_4leg*1"},
                {"id": 60, "classname": "Gate_Thing"},
                {"id": 70, "classname": "stairs"},
                {"id": 80, "classname": "no_hab_here"}
            ]},
            "wallitemtypes": {"furnitype": [{"id": 4001, "classname": "poster"}]}
        });
        let mut bed = row(1, "bed_polyfon", 's');
        bed.sprite_id = 41;
        bed.width = 2;
        bed.length = 3;
        bed.stack_height = 1.73;
        bed.allow_lay = true;
        bed.interaction_type = "bed".into();
        let mut chair = row(2, "chair_norja", 's');
        chair.sprite_id = 99;
        let mut table = row(3, "table_plasto_4leg*1", 's');
        table.sprite_id = 50;
        table.width = 2;
        table.length = 2;
        table.stack_height = 1.0;
        table.allow_stack = true; // hand-made: stackable at 1.00
        let mut gate = row(4, "gate_thing", 's'); // other case than FurnitureData.json
        gate.interaction_type = "gate".into();
        gate.modes = 2;
        let mut multi = row(5, "stairs", 's');
        multi.interaction_type = "multiheight".into();
        let mut multi2 = row(6, "stairs", 's');
        multi2.multiheight = "0;1;2".into();
        let mut poster = row(7, "poster", 's'); // a wall entry with the wrong type and size
        poster.width = 2;
        poster.stack_height = 0.5;
        let pet = row(8, "a0 pet0", 's');
        let unknown = row(9, "only_in_db", 's');
        let no_hab = row(10, "no_hab_here", 's');
        let rows = [bed, chair, table, gate, multi, multi2, poster, pet, unknown, no_hab];
        let bundles: HashMap<String, Option<Bundle>> = [
            ("bed_polyfon", Some(Bundle { width: 2.0, length: 3.0, height: 1.73125, modes: 1, multiheight: None })),
            ("chair_norja", Some(Bundle { width: 1.0, length: 1.0, height: 1.0, modes: 1, multiheight: None })),
            ("table_plasto_4leg", Some(Bundle { width: 2.0, length: 2.0, height: 1.0, modes: 1, multiheight: None })),
            ("gate_thing", Some(Bundle { width: 1.0, length: 1.0, height: 0.0, modes: 3, multiheight: None })),
            ("poster", Some(Bundle { width: 1.0, length: 1.0, height: 0.5, modes: 1, multiheight: None })),
            ("no_hab_here", None),
        ]
        .into_iter()
        .map(|(n, b)| (n.to_string(), b))
        .collect();
        let plan = plan(&rows, &furnidata, &bundles, Fields::default());
        let changed: Vec<(&str, &str, &str, &str)> = plan.changes.iter().map(|c| (c.name.as_str(), c.column, c.from.as_str(), c.to.as_str())).collect();
        assert_eq!(
            changed,
            [
                // The bed is right (1.73125 is 1.73); the chair gets its sprite and sit.
                ("chair_norja", "sprite_id", "99", "30"),
                ("chair_norja", "allow_sit", "0", "1"),
                ("chair_norja", "stack_height", "0.00", "1.00"),
                // A gate keeps its modes (only 'default' ones count); its sprite comes from FurnitureData.json.
                ("gate_thing", "sprite_id", "0", "60"),
                // The poster is a wall furni: 1 x 1 x 0 (type only when chosen).
                ("poster", "sprite_id", "0", "4001"),
                ("poster", "width", "2", "1"),
                ("poster", "stack_height", "0.50", "0.00"),
                ("no_hab_here", "sprite_id", "0", "80"),
            ]
        );
        assert_eq!((plan.checked, plan.multiheight), (7, 2));
        assert_eq!(plan.not_in_furnidata, ["only_in_db"]);
        assert_eq!(plan.no_bundle, ["no_hab_here"]);
        // allow_stack is never touched: the hand-made stackable table stays.
        assert!(!plan.changes.iter().any(|c| c.column == "allow_stack"));
        let with_type = super::plan(&rows, &furnidata, &bundles, Fields { kind: true, ..Fields::default() });
        assert!(with_type.changes.iter().any(|c| c.name == "poster" && c.column == "type" && c.from == "s" && c.to == "i"));
        let text = sql(&plan);
        assert!(text.contains("UPDATE `items_base` SET `sprite_id` = 30, `allow_sit` = '1', `stack_height` = 1.00 WHERE `id` = 2 AND `interaction_type` <> 'multiheight'"), "{text}");
        assert!(text.trim_end().ends_with("COMMIT;"));
        // Nothing chosen: nothing changes.
        let none = super::plan(&rows, &furnidata, &bundles, Fields { sprite: false, kind: false, size: false, flags: false, modes: false, multiheight: false });
        assert!(none.changes.is_empty());
    }

    #[test]
    fn multiheight_as_the_hab_says() {
        let furnidata = json!({"roomitemtypes": {"furnitype": [
            {"id": 1, "classname": "block"}, {"id": 2, "classname": "block_hand"}, {"id": 3, "classname": "block_three"},
            {"id": 4, "classname": "not_multi"}, {"id": 5, "classname": "block_recolorable"}, {"id": 6, "classname": "gate_multi"}
        ]}});
        let heights = crate::furni_import::Heights::new(&[1.0, 0.75, 0.5, 0.25, 0.1]);
        let multi = Some(Bundle { width: 1.0, length: 1.0, height: 1.0, modes: 5, multiheight: heights });
        let plain = Some(Bundle { width: 2.0, length: 1.0, height: 0.5, modes: 1, multiheight: None });
        let bundles: HashMap<String, Option<Bundle>> = [
            ("block", multi), ("block_hand", multi), ("block_three", multi), ("not_multi", plain), ("block_recolorable", multi), ("gate_multi", multi),
        ]
        .into_iter()
        .map(|(n, b)| (n.to_string(), b))
        .collect();
        let with = |id: i64, name: &str, interaction: &str, multiheight: &str, modes: i64, stack: f64| ItemRow {
            sprite_id: id,
            interaction_type: interaction.into(),
            multiheight: multiheight.into(),
            modes,
            stack_height: stack,
            ..row(id, name, 's')
        };
        let rows = [
            with(1, "block", "default", "0", 1, 1.0),                              // .hab: multiheight, items_base: not
            with(2, "block_hand", "multiheight", "1.00;0.80;0.60;0.40;0.20", 5, 0.0), // hand-set, five values: stays
            with(3, "block_three", "multiheight", "1.00;0.50;0.10", 3, 0.0),        // three values for five states
            with(4, "not_multi", "multiheight", "1.00;0.50", 2, 0.0),               // .hab: no multiheight furni
            with(5, "block_recolorable", "recolorable", "0", 5, 1.0),               // recolorable keeps its interaction
            with(6, "gate_multi", "gate", "0", 2, 1.0),                             // another interaction: untouched
        ];
        let plan = plan(&rows, &furnidata, &bundles, Fields::default());
        let of = |name: &str| -> Vec<(&str, String)> { plan.changes.iter().filter(|c| c.name == name).map(|c| (c.column, c.to.clone())).collect() };
        let five = "1.00;0.75;0.50;0.25;0.10".to_string();
        assert_eq!(
            of("block"),
            [("interaction_type", "multiheight".into()), ("multiheight", five.clone()), ("interaction_modes_count", "5".into()), ("stack_height", "0.00".into())]
        );
        // A multiheight furni in items_base is never changed: other heights, another number, or no multiheight .hab.
        assert!(of("block_hand").is_empty() && of("block_three").is_empty() && of("not_multi").is_empty());
        assert_eq!(plan.multiheight_count_differs, [("block_three".to_string(), "1.00;0.50;0.10".to_string(), five.clone())]);
        assert_eq!(plan.multiheight_not_by_hab, ["not_multi"]);
        assert_eq!(of("block_recolorable"), [("multiheight", five.clone())], "recolorable: the heights, its interaction and modes stay");
        assert!(of("gate_multi").is_empty());
        assert_eq!(plan.multiheight, 5, "block, block_hand, block_three, not_multi, block_recolorable");
        let text = sql(&plan);
        assert!(text.contains("SET `interaction_type` = 'multiheight', `multiheight` = '1.00;0.75;0.50;0.25;0.10', `interaction_modes_count` = 5, `stack_height` = 0.00 WHERE `id` = 1;"), "{text}");
        // Off: the multiheight furni are left alone as before.
        let off = super::plan(&rows, &furnidata, &bundles, Fields { multiheight: false, ..Fields::default() });
        assert!(!off.changes.iter().any(|c| c.column == "multiheight"));
    }

    #[test]
    fn reads_items_base() {
        let (db, rows) = parse_items("DB=habbo\nI\t1\ts\t13\t1\t1\t1.00\t0\t0\t0\t0\t1\tvendingmachine\t0\tshelves_norja\nI\t2\ti\t4001\t1\t1\t0.00\t0\t0\t0\t0\t1\tdefault\t\tposter with\ttab\nbad\n");
        assert_eq!(db, "habbo");
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].sprite_id, rows[0].stack_height, rows[0].interaction_type.as_str()), (13, 1.0, "vendingmachine"));
        assert_eq!((rows[1].kind, rows[1].name.as_str(), rows[1].multiheight.as_str()), ('i', "poster with\ttab", ""));
    }
}
