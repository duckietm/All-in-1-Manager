//! Fix offer IDs: catalog_items.offer_id and FurnitureData.json's offerid brought in line, so the client can buy
//! every furni (it sends the offer id of FurnitureData.json). (All-in-1 Converter: Database/SetOfferID.cs)
//!
//! As the converter does it:
//! 1. the offer ids in use are the catalogue items' ids above 0;
//! 2. FurnitureData.json's entries are grouped by classname; a group's offer id is its first entry's offerid;
//! 3. a group with offerid -1 gets a new offer id: random, above 60000, not in use; every entry of it gets it;
//! 4. every catalogue item whose catalog_name is such a classname gets that offer id, where it has another one.
//!
//! Two things are added, so it does what the converter means on a hotel that is imported into more than once: the
//! offer ids FurnitureData.json has already count as in use too, and a -1 furni whose catalogue items already share
//! one offer id that nothing else uses keeps that one instead of a new random one (so running it again, or after
//! FurnitureData.json was put on the server anew, changes nothing that is right already).

use std::collections::{BTreeMap, HashMap, HashSet};

use rand::Rng;
use serde_json::Value;

use crate::script::{self, fill, sql_literal};
use crate::server::{Result, Server, ServerError};
use crate::server_tools;

/// New offer ids are above this (SetOfferID.cs: rng.Next(60001, int.MaxValue)).
pub const NEW_OFFER_IDS_ABOVE: i64 = 60_000;

/// One catalogue item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRow {
    pub id: i64,
    pub name: String,
    pub offer_id: i64,
}

/// What the fix changes.
#[derive(Debug, Clone, Default)]
pub struct OfferPlan {
    /// FurnitureData.json with the new offer ids.
    pub furnidata: Value,
    /// (classname, its new offer id) for the furni that had -1.
    pub generated: Vec<(String, i64)>,
    /// (classname, offer id) for the -1 furni that keep the offer id their catalogue items have.
    pub kept: Vec<(String, i64)>,
    /// (catalogue item id, catalog_name, its offer_id now, the new one).
    pub updates: Vec<(i64, String, i64, i64)>,
    /// Classnames in FurnitureData.json, catalogue items read.
    pub furni: usize,
    pub catalog_items: usize,
}

impl OfferPlan {
    /// FurnitureData.json changes (some furni had -1).
    pub fn furnidata_changes(&self) -> bool {
        !self.generated.is_empty() || !self.kept.is_empty()
    }
}

const SECTIONS: [&str; 2] = ["roomitemtypes", "wallitemtypes"];

fn offer_of(entry: &Value) -> i64 {
    match entry.get("offerid") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(-1),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(-1),
        _ => -1,
    }
}

/// Works out the fix; `rng` gives the new offer ids.
pub fn plan(furnidata: &Value, catalog: &[CatalogRow], rng: &mut impl Rng) -> OfferPlan {
    let mut furnidata = furnidata.clone();
    // The groups in FurnitureData.json's order: classname -> its first entry's offerid.
    let mut order: Vec<String> = Vec::new();
    let mut first: HashMap<String, i64> = HashMap::new();
    for section in SECTIONS {
        for entry in furnidata[section]["furnitype"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let Some(name) = entry.get("classname").and_then(Value::as_str) else { continue };
            if !first.contains_key(name) {
                first.insert(name.to_string(), offer_of(entry));
                order.push(name.to_string());
            }
        }
    }
    let mut used: HashSet<i64> = catalog.iter().map(|c| c.offer_id).filter(|id| *id > 0).collect();
    used.extend(first.values().copied().filter(|id| *id > 0));
    // Which names each offer id of the catalogue belongs to, and each name's offer ids there.
    let mut names_of: HashMap<i64, HashSet<&str>> = HashMap::new();
    let mut offers_of: HashMap<&str, HashSet<i64>> = HashMap::new();
    for row in catalog {
        if row.offer_id > 0 {
            names_of.entry(row.offer_id).or_default().insert(&row.name);
            offers_of.entry(&row.name).or_default().insert(row.offer_id);
        }
    }
    let furnidata_offers: HashMap<i64, usize> = first.values().filter(|id| **id > 0).fold(HashMap::new(), |mut m, id| {
        *m.entry(*id).or_default() += 1;
        m
    });
    let mut mapping: HashMap<String, i64> = HashMap::new();
    let (mut generated, mut kept) = (Vec::new(), Vec::new());
    for name in &order {
        let mut offer = first[name];
        if offer == -1 {
            // Its catalogue items' one offer id, when no other furni uses it: kept.
            let own = offers_of.get(name.as_str()).filter(|ids| ids.len() == 1).and_then(|ids| ids.iter().next().copied());
            let own = own.filter(|id| names_of.get(id).is_some_and(|n| n.len() == 1) && !furnidata_offers.contains_key(id));
            match own {
                Some(id) => {
                    offer = id;
                    kept.push((name.clone(), id));
                }
                None => {
                    let id = loop {
                        let id = rng.gen_range(NEW_OFFER_IDS_ABOVE + 1..i64::from(i32::MAX));
                        if !used.contains(&id) {
                            break id;
                        }
                    };
                    used.insert(id);
                    offer = id;
                    generated.push((name.clone(), id));
                }
            }
        }
        mapping.insert(name.clone(), offer);
    }
    // Every entry of a classname gets its offer id (as the converter sets it on the whole group).
    for section in SECTIONS {
        if let Some(list) = furnidata[section]["furnitype"].as_array_mut() {
            for entry in list {
                let Some(name) = entry.get("classname").and_then(Value::as_str).map(str::to_string) else { continue };
                if offer_of(entry) == -1 {
                    if let Some(id) = mapping.get(&name) {
                        entry["offerid"] = Value::from(*id);
                    }
                }
            }
        }
    }
    let updates = catalog
        .iter()
        .filter_map(|row| mapping.get(&row.name).filter(|id| **id != row.offer_id).map(|id| (row.id, row.name.clone(), row.offer_id, *id)))
        .collect();
    OfferPlan { furnidata, generated, kept, updates, furni: order.len(), catalog_items: catalog.len() }
}

/// [`plan`] with the system's random generator.
pub fn plan_new(furnidata: &Value, catalog: &[CatalogRow]) -> OfferPlan {
    plan(furnidata, catalog, &mut rand::thread_rng())
}

/// The updates as one transaction. Each names the offer id the item had, so an item changed meanwhile is left.
pub fn sql(plan: &OfferPlan) -> String {
    let mut out = String::from("-- Fix offer IDs (All-in-1 Installer): catalog_items.offer_id as FurnitureData.json has it.\nSET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    for (id, name, from, to) in &plan.updates {
        out.push_str(&format!("UPDATE `catalog_items` SET `offer_id` = {to} WHERE `id` = {id} AND `offer_id` = {from}; -- {}\n", sql_literal(name).replace('\n', " ")));
    }
    out.push_str("COMMIT;\n");
    out
}

const READ: &str = r#"{{DatabaseName}}
set -o pipefail
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run: run Install New Server first."
[ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = 'catalog_items'")" = 1 ] ||
  fail "$db has no catalog_items table: run the Emulator step first."
echo "DB=$db"
mariadb -N -B -r "$db" -e "SELECT 'C', id, offer_id, catalog_name FROM catalog_items""#;

/// The server's FurnitureData.json (gamedata/config).
pub fn furniture_data_path() -> String {
    format!("{}/config/FurnitureData.json", crate::octane::gamedata())
}

/// Parses READ's output: (database, catalogue items).
pub fn parse_catalog(output: &str) -> (String, Vec<CatalogRow>) {
    let mut database = String::new();
    let mut rows = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(db) = line.strip_prefix("DB=") {
            database = db.trim().to_string();
            continue;
        }
        let fields: Vec<&str> = line.splitn(4, '\t').collect();
        if let ["C", id, offer, name] = fields.as_slice() {
            if let (Ok(id), Ok(offer_id)) = (id.parse(), offer.parse()) {
                rows.push(CatalogRow { id, name: name.to_string(), offer_id });
            }
        }
    }
    (database, rows)
}

/// Reads the catalogue items (only reads); the FurnitureData.json is downloaded by the caller.
pub async fn read_catalog(server: &dyn Server) -> Result<(String, Vec<CatalogRow>)> {
    let body = fill(READ, &[("DatabaseName", &server_tools::database_name()), ("FurnitureData", &crate::server::shell_quote(&furniture_data_path()))]);
    let result = server.run_as_root(&script::with_prelude(&body)).await?;
    if !result.succeeded() {
        let reason = result.output.lines().find_map(|l| l.strip_prefix("ERROR: ")).map(str::to_string).unwrap_or_else(|| result.error.trim().to_string());
        return Err(ServerError(format!("Reading the catalogue failed: {reason}")).into());
    }
    Ok(parse_catalog(&result.output))
}

/// The groups of the plan by what changes, for an overview: offer id -> how many catalogue items get it.
pub fn updates_by_offer(plan: &OfferPlan) -> BTreeMap<i64, usize> {
    let mut map = BTreeMap::new();
    for (_, _, _, to) in &plan.updates {
        *map.entry(*to).or_default() += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use serde_json::json;

    fn row(id: i64, name: &str, offer_id: i64) -> CatalogRow {
        CatalogRow { id, name: name.into(), offer_id }
    }

    #[test]
    fn like_set_offer_id() {
        let furnidata = json!({
            "roomitemtypes": {"furnitype": [
                {"id": 1, "classname": "chair", "offerid": 5},
                {"id": 2, "classname": "lamp", "offerid": -1},
                {"id": 3, "classname": "lamp", "offerid": -1},
                {"id": 4, "classname": "kept", "offerid": -1},
                {"id": 5, "classname": "shared", "offerid": -1},
                {"id": 6, "classname": "nothing_sold"}
            ]},
            "wallitemtypes": {"furnitype": [{"id": 1, "classname": "poster", "offerid": 7}]}
        });
        let catalog = [
            row(10, "chair", 99),     // FurnitureData says 5: the catalogue follows
            row(11, "lamp", -1),      // a new offer id
            row(12, "lamp", -1),
            row(13, "kept", 70001),   // its own offer id, used by nothing else: kept
            row(14, "shared", 70002), // shares 70002 with "other": a new one
            row(15, "other", 70002),
            row(16, "deal_5coins", 3), // not a furni: untouched
            row(17, "poster", 7),      // right already
        ];
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let plan = plan(&furnidata, &catalog, &mut rng);
        assert_eq!(plan.furni, 6);
        assert_eq!(plan.kept, [("kept".to_string(), 70001)]);
        let new: HashMap<&str, i64> = plan.generated.iter().map(|(n, id)| (n.as_str(), *id)).collect();
        assert_eq!(new.len(), 3, "lamp, shared and nothing_sold");
        for id in new.values() {
            assert!(*id > NEW_OFFER_IDS_ABOVE && *id < i64::from(i32::MAX) && ![5, 7, 3, 99, 70001, 70002].contains(id));
        }
        assert_ne!(new["lamp"], new["shared"]);
        // Every lamp entry got the lamp's id; a furni with an offer id keeps it.
        let room = plan.furnidata["roomitemtypes"]["furnitype"].as_array().unwrap();
        assert_eq!(room[1]["offerid"], new["lamp"]);
        assert_eq!(room[2]["offerid"], new["lamp"]);
        assert_eq!(room[0]["offerid"], 5);
        assert_eq!(room[3]["offerid"], 70001);
        let updates: Vec<(i64, i64, i64)> = plan.updates.iter().map(|u| (u.0, u.2, u.3)).collect();
        assert_eq!(updates, [(10, 99, 5), (11, -1, new["lamp"]), (12, -1, new["lamp"]), (14, 70002, new["shared"])]);
        let text = sql(&plan);
        assert!(text.contains("UPDATE `catalog_items` SET `offer_id` = 5 WHERE `id` = 10 AND `offer_id` = 99; -- 'chair'"));
        assert!(text.starts_with("-- Fix") && text.trim_end().ends_with("COMMIT;"));

        // Run again on the result: nothing to do.
        let after: Vec<CatalogRow> = catalog
            .iter()
            .map(|c| plan.updates.iter().find(|u| u.0 == c.id).map_or(c.clone(), |u| row(c.id, &c.name, u.3)))
            .collect();
        let again = super::plan(&plan.furnidata, &after, &mut rng);
        assert!(again.updates.is_empty() && again.generated.is_empty() && again.kept.is_empty());
    }

    #[test]
    fn after_furnidata_is_put_on_the_server_again() {
        // FurnitureData.json came back with -1 (a new merge); the catalogue has the ids of the last fix: kept.
        let furnidata = json!({"roomitemtypes": {"furnitype": [{"id": 1, "classname": "a", "offerid": -1}, {"id": 2, "classname": "b", "offerid": -1}]}});
        let catalog = [row(1, "a", 61000), row(2, "b", 62000), row(3, "b", 62000)];
        let plan = plan(&furnidata, &catalog, &mut rand::rngs::StdRng::seed_from_u64(1));
        assert!(plan.updates.is_empty() && plan.generated.is_empty());
        assert_eq!(plan.kept.len(), 2);
        assert_eq!(plan.furnidata["roomitemtypes"]["furnitype"][1]["offerid"], 62000);
    }

    #[test]
    fn reads_the_catalogue() {
        let (db, rows) = parse_catalog("DB=habbo\nC\t1\t5\tchair\nC\t2\t-1\tname\twith tab\njunk\n");
        assert_eq!(db, "habbo");
        assert_eq!(rows, [row(1, "chair", 5), row(2, "name\twith tab", -1)]);
    }
}
