//! One FurnitureData.json for Habbo's furniture and the custom furniture of the Nitro retro profiles.
//!
//! Habbo's own FurnitureData.json (Get Assets' download) is the source of truth: it goes in first, untouched. Then
//! every merged profile, in the order it was first merged:
//! 1. a classname listed more than once in the profile counts once (the first entry);
//! 2. a classname that is already in (Habbo's, or an earlier profile's) is left out;
//! 3. an id that is already taken in its section (floor or wall: Habbo numbers them apart) gets a random free id
//!    above 100000, free in both sections;
//! 4. its offerid: a random one above 60000 (as SetOfferID gives them) that neither FurnitureData.json nor the
//!    hotel's catalogue uses; a retro's own offer ids mean nothing here. Habbo's offer ids stay as they are.
//!
//! The ids handed out are remembered per profile and classname ([`MergeState`]), so building again (Habbo released
//! new furniture, or a profile is added) gives every custom furni the id it had, unless Habbo now uses that id.

use std::collections::{BTreeMap, HashSet};

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// New ids of custom furni are above this.
pub const CUSTOM_IDS_ABOVE: i64 = 100_000;
/// ... and at most this (the client keeps ids as int32; six digits are easy to read).
pub const CUSTOM_IDS_UP_TO: i64 = 999_999;

const SECTIONS: [&str; 2] = ["roomitemtypes", "wallitemtypes"];

/// What was merged before: the profiles in their order, and the id each custom furni got (classname in lower case).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeState {
    pub profiles: Vec<String>,
    #[serde(default)]
    pub ids: BTreeMap<String, BTreeMap<String, i64>>,
    /// The offer id each custom furni got, per profile and classname: it keeps it, so its catalogue items stay right.
    #[serde(default)]
    pub offers: BTreeMap<String, BTreeMap<String, i64>>,
}

/// New offer ids of custom furni are above this (the range SetOfferID gives).
pub const CUSTOM_OFFERS_ABOVE: i64 = 60_000;

impl MergeState {
    /// The profiles to merge with `profile` among them: the merged ones in their order, a new one last.
    pub fn order_with(&self, profile: &str) -> Vec<String> {
        let mut order = self.profiles.clone();
        if !order.iter().any(|p| p == profile) {
            order.push(profile.to_string());
        }
        order
    }
}

/// What the merge did with one profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileReport {
    pub profile: String,
    /// Entries in the profile's FurnitureData.json.
    pub entries: usize,
    /// Classnames listed more than once (the extra entries are left out).
    pub duplicates: Vec<String>,
    /// Classnames Habbo or an earlier profile has already.
    pub already: Vec<String>,
    /// (classname, its id in the profile, its id now) for the furni whose id was taken.
    pub renumbered: Vec<(String, i64, i64)>,
    /// (classname, offer id) for the furni that got a new offer id (not the one remembered from before).
    pub new_offers: Vec<(String, i64)>,
    /// What is added: FurnitureData.json with only this profile's furni, with their ids now.
    pub added: Value,
}

impl ProfileReport {
    pub fn added_count(&self) -> usize {
        SECTIONS.iter().map(|s| self.added[s]["furnitype"].as_array().map_or(0, Vec::len)).sum()
    }
}

/// The merged FurnitureData.json, a report per profile, and the ids to remember.
#[derive(Debug, Clone)]
pub struct Merged {
    pub furnidata: Value,
    pub reports: Vec<ProfileReport>,
    pub state: MergeState,
}

fn entries(furnidata: &Value, section: &str) -> Vec<Value> {
    furnidata[section]["furnitype"].as_array().cloned().unwrap_or_default()
}

fn classname(entry: &Value) -> String {
    entry.get("classname").and_then(Value::as_str).unwrap_or("").trim().to_lowercase()
}

fn id_of(entry: &Value) -> Option<i64> {
    match entry.get("id") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    // An id the client and items_base.sprite_id (int(11)) cannot hold counts as taken: it gets a free one.
    .filter(|id| *id > 0 && *id <= i64::from(i32::MAX))
}

fn empty() -> Value {
    serde_json::json!({ "roomitemtypes": { "furnitype": [] }, "wallitemtypes": { "furnitype": [] } })
}

fn offer_of(entry: &Value) -> Option<i64> {
    match entry.get("offerid") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|id| *id > 0)
}

/// Merges `profiles` ((name, its FurnitureData.json), in order) into Habbo's `original`, which stays as it is.
/// `catalog_offers` are the offer ids the hotel's catalog_items use: a new offer id is none of them.
pub fn merge(original: &Value, profiles: &[(String, Value)], remembered: &MergeState, catalog_offers: &HashSet<i64>, rng: &mut impl Rng) -> Merged {
    let mut merged = original.clone();
    for section in SECTIONS {
        if !merged[section]["furnitype"].is_array() {
            merged[section] = serde_json::json!({ "furnitype": [] });
        }
    }
    let mut names: HashSet<String> = HashSet::new();
    // Ids per section (Habbo numbers floor and wall furni apart), and every id for the new ones.
    let mut ids: [HashSet<i64>; 2] = [HashSet::new(), HashSet::new()];
    for (i, section) in SECTIONS.iter().enumerate() {
        for entry in entries(original, section) {
            names.insert(classname(&entry));
            if let Some(id) = id_of(&entry) {
                ids[i].insert(id);
            }
        }
    }
    // The offer ids of the merged FurnitureData.json: Habbo's, then what the profiles get.
    let mut offers: HashSet<i64> = SECTIONS.iter().flat_map(|s| entries(original, s)).filter_map(|e| offer_of(&e)).collect();
    let mut state = MergeState { profiles: profiles.iter().map(|(name, _)| name.clone()).collect(), ..MergeState::default() };
    let mut reports = Vec::new();
    for (profile, furnidata) in profiles {
        let mut report = ProfileReport { profile: profile.clone(), added: empty(), ..ProfileReport::default() };
        let before = remembered.ids.get(profile);
        let offers_before = remembered.offers.get(profile);
        let mut given = BTreeMap::new();
        let mut given_offers = BTreeMap::new();
        let mut seen = HashSet::new();
        for (i, section) in SECTIONS.iter().enumerate() {
            for mut entry in entries(furnidata, section) {
                report.entries += 1;
                let name = classname(&entry);
                if name.is_empty() {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    report.duplicates.push(entry["classname"].as_str().unwrap_or("").to_string());
                    continue;
                }
                if names.contains(&name) {
                    report.already.push(entry["classname"].as_str().unwrap_or("").to_string());
                    continue;
                }
                let own = id_of(&entry);
                let free = |id: i64| !ids[i].contains(&id);
                // The id it had last time, else its own, else a new one.
                let id = match (before.and_then(|b| b.get(&name)).copied(), own) {
                    (Some(last), _) if free(last) => last,
                    (_, Some(own)) if free(own) => own,
                    _ => loop {
                        let id = rng.gen_range(CUSTOM_IDS_ABOVE + 1..=CUSTOM_IDS_UP_TO);
                        if !ids[0].contains(&id) && !ids[1].contains(&id) {
                            break id;
                        }
                    },
                };
                if Some(id) != own {
                    report.renumbered.push((entry["classname"].as_str().unwrap_or("").to_string(), own.unwrap_or(0), id));
                }
                entry["id"] = Value::from(id);
                // The offer id it had last time, unless FurnitureData.json has it now; else a new one that nothing
                // uses (the catalogue's own offer ids count: the remembered one is in it from the last import).
                let offer = match offers_before.and_then(|o| o.get(&name)).copied() {
                    Some(last) if !offers.contains(&last) => last,
                    _ => {
                        let offer = loop {
                            let offer = rng.gen_range(CUSTOM_OFFERS_ABOVE + 1..i64::from(i32::MAX));
                            if !offers.contains(&offer) && !catalog_offers.contains(&offer) {
                                break offer;
                            }
                        };
                        report.new_offers.push((entry["classname"].as_str().unwrap_or("").to_string(), offer));
                        offer
                    }
                };
                entry["offerid"] = Value::from(offer);
                offers.insert(offer);
                given_offers.insert(name.clone(), offer);
                names.insert(name.clone());
                ids[i].insert(id);
                given.insert(name, id);
                merged[section]["furnitype"].as_array_mut().unwrap().push(entry.clone());
                report.added[section]["furnitype"].as_array_mut().unwrap().push(entry);
            }
        }
        state.ids.insert(profile.clone(), given);
        state.offers.insert(profile.clone(), given_offers);
        reports.push(report);
    }
    Merged { furnidata: merged, reports, state }
}

/// [`merge`] with new ids from the system's random generator.
pub fn merge_new(original: &Value, profiles: &[(String, Value)], remembered: &MergeState, catalog_offers: &HashSet<i64>) -> Merged {
    merge(original, profiles, remembered, catalog_offers, &mut rand::thread_rng())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use serde_json::json;

    fn furnidata(room: &[(i64, &str)], wall: &[(i64, &str)]) -> Value {
        let list = |items: &[(i64, &str)]| items.iter().map(|(id, name)| json!({"id": id, "classname": name, "name": name})).collect::<Vec<_>>();
        json!({ "roomitemtypes": { "furnitype": list(room) }, "wallitemtypes": { "furnitype": list(wall) } })
    }

    fn ids(furnidata: &Value, section: &str) -> Vec<(String, i64)> {
        entries(furnidata, section).iter().map(|e| (e["classname"].as_str().unwrap().to_string(), e["id"].as_i64().unwrap())).collect()
    }

    #[test]
    fn habbo_first_then_the_profiles_in_order() {
        let original = furnidata(&[(13, "shelves_norja"), (14, "chair_norja")], &[(1, "post.it"), (13, "poster")]);
        let one = furnidata(
            &[(500, "wf_act_give_userbadge"), (501, "WF_ACT_GIVE_USERBADGE"), (13, "custom_chair"), (14, "chair_norja"), (600, "custom_table")],
            // Wall 14 is free (Habbo's 14 is a floor item): it stays.
            &[(14, "custom_poster")],
        );
        let two = furnidata(&[(600, "other_table"), (700, "custom_table"), (701, "two_only")], &[]);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let merged = merge(&original, &[("one".into(), one.clone()), ("two".into(), two.clone())], &MergeState::default(), &HashSet::new(), &mut rng);

        // Habbo's entries first and as they were.
        let room = ids(&merged.furnidata, "roomitemtypes");
        assert_eq!(room[..2], [("shelves_norja".to_string(), 13), ("chair_norja".to_string(), 14)]);
        let r1 = &merged.reports[0];
        assert_eq!(r1.duplicates, ["WF_ACT_GIVE_USERBADGE"]);
        assert_eq!(r1.already, ["chair_norja"]);
        // custom_chair had Habbo's floor id 13: a new one above 100000.
        assert_eq!(r1.renumbered.len(), 1);
        let (name, old, new) = &r1.renumbered[0];
        assert!(name == "custom_chair" && *old == 13 && *new > CUSTOM_IDS_ABOVE && *new <= CUSTOM_IDS_UP_TO);
        assert_eq!(r1.added_count(), 4);
        assert_eq!(ids(&merged.furnidata, "wallitemtypes"), [("post.it".to_string(), 1), ("poster".to_string(), 13), ("custom_poster".to_string(), 14)]);

        // Profile two: custom_table is profile one's; other_table's 600 is profile one's id.
        let r2 = &merged.reports[1];
        assert_eq!(r2.already, ["custom_table"]);
        assert_eq!(r2.renumbered.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["other_table"]);
        let names: Vec<String> = room.iter().map(|r| r.0.clone()).collect();
        assert_eq!(names, ["shelves_norja", "chair_norja", "wf_act_give_userbadge", "custom_chair", "custom_table", "other_table", "two_only"]);
        // No id twice in a section, no classname twice anywhere.
        for section in SECTIONS {
            let all = ids(&merged.furnidata, section);
            assert_eq!(all.iter().map(|r| r.1).collect::<HashSet<_>>().len(), all.len(), "{section}");
        }
        assert_eq!(merged.state.profiles, ["one", "two"]);

        // Building again keeps every custom id, also the random ones.
        let mut other_rng = rand::rngs::StdRng::seed_from_u64(99);
        let again = merge(&original, &[("one".into(), one.clone()), ("two".into(), two.clone())], &merged.state, &HashSet::new(), &mut other_rng);
        assert_eq!(again.furnidata, merged.furnidata);
        assert_eq!(again.state, merged.state);

        // Habbo releases custom_chair's new id as a furni of its own: Habbo keeps it, custom_chair moves again.
        let taken = *new;
        let newer = furnidata(&[(13, "shelves_norja"), (14, "chair_norja"), (taken, "habbo_new"), (600, "habbo_600")], &[(1, "post.it"), (13, "poster")]);
        let rebuilt = merge(&newer, &[("one".into(), one), ("two".into(), two)], &merged.state, &HashSet::new(), &mut other_rng);
        let room = ids(&rebuilt.furnidata, "roomitemtypes");
        assert!(room.contains(&("habbo_new".to_string(), taken)) && room.contains(&("habbo_600".to_string(), 600)));
        let chair = room.iter().find(|r| r.0 == "custom_chair").unwrap().1;
        assert!(chair != taken && chair > CUSTOM_IDS_ABOVE);
        let table = room.iter().find(|r| r.0 == "custom_table").unwrap().1;
        assert!(table != 600 && table > CUSTOM_IDS_ABOVE, "Habbo's 600 now: custom_table moves");
        assert_eq!(room.iter().find(|r| r.0 == "wf_act_give_userbadge").unwrap().1, 500, "untouched ids stay");
    }

    #[test]
    fn offer_ids_habbo_first_then_new_ones() {
        let original = json!({"roomitemtypes": {"furnitype": [
            {"id": 1, "classname": "chair", "offerid": 60500},
            {"id": 2, "classname": "lamp", "offerid": -1}
        ]}});
        // The retro's offer ids: one is Habbo's, one is the catalogue's, one is -1; they mean nothing here.
        let custom = json!({"roomitemtypes": {"furnitype": [
            {"id": 10, "classname": "c_a", "offerid": 60500},
            {"id": 11, "classname": "c_b", "offerid": 61000},
            {"id": 12, "classname": "c_c", "offerid": -1}
        ]}});
        let catalog: HashSet<i64> = [61000, 70000].into();
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        let merged = merge(&original, &[("p".into(), custom.clone())], &MergeState::default(), &catalog, &mut rng);
        let room = entries(&merged.furnidata, "roomitemtypes");
        // Habbo's stay as they are (also its -1).
        assert_eq!((room[0]["offerid"].as_i64(), room[1]["offerid"].as_i64()), (Some(60500), Some(-1)));
        let offers: Vec<i64> = room[2..].iter().map(|e| e["offerid"].as_i64().unwrap()).collect();
        assert_eq!(offers.iter().collect::<HashSet<_>>().len(), 3, "{offers:?}");
        for offer in &offers {
            assert!(*offer > CUSTOM_OFFERS_ABOVE && *offer < i64::from(i32::MAX) && ![60500, 61000, 70000].contains(offer));
        }
        assert_eq!(merged.reports[0].new_offers.len(), 3);
        // Merged again (the catalogue has them now): the same offer ids, none new.
        let catalog_after: HashSet<i64> = catalog.iter().copied().chain(offers.iter().copied()).collect();
        let again = merge(&original, &[("p".into(), custom.clone())], &merged.state, &catalog_after, &mut rng);
        assert_eq!(again.furnidata, merged.furnidata);
        assert!(again.reports[0].new_offers.is_empty());
        // Habbo gives one of them to a new furni of its own: Habbo keeps it, that custom furni gets another.
        let taken = offers[1];
        let newer = json!({"roomitemtypes": {"furnitype": [
            {"id": 1, "classname": "chair", "offerid": 60500},
            {"id": 2, "classname": "lamp", "offerid": -1},
            {"id": 3, "classname": "habbo_new", "offerid": taken}
        ]}});
        let rebuilt = merge(&newer, &[("p".into(), custom)], &merged.state, &catalog_after, &mut rng);
        let moved: Vec<&str> = rebuilt.reports[0].new_offers.iter().map(|n| n.0.as_str()).collect();
        assert_eq!(moved, ["c_b"]);
        let room = entries(&rebuilt.furnidata, "roomitemtypes");
        assert_eq!(room[2]["offerid"].as_i64(), Some(taken), "Habbo's");
        assert_ne!(room.iter().find(|e| e["classname"] == "c_b").unwrap()["offerid"].as_i64(), Some(taken));
    }

    #[test]
    fn the_order_of_the_profiles() {
        let state = MergeState { profiles: vec!["one".into(), "two".into()], ..MergeState::default() };
        assert_eq!(state.order_with("two"), ["one", "two"]);
        assert_eq!(state.order_with("three"), ["one", "two", "three"]);
    }

    #[test]
    fn odd_entries() {
        // A string id, no id, an empty classname, no sections at all.
        let original = furnidata(&[(1, "a")], &[]);
        let custom = json!({ "roomitemtypes": { "furnitype": [{"id": "1", "classname": "b"}, {"classname": "c"}, {"id": 5, "classname": " "}] } });
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let merged = merge(&original, &[("p".into(), custom)], &MergeState::default(), &HashSet::new(), &mut rng);
        let room = ids(&merged.furnidata, "roomitemtypes");
        assert_eq!(room.len(), 3);
        assert!(room[1].1 > CUSTOM_IDS_ABOVE && room[2].1 > CUSTOM_IDS_ABOVE && room[1].1 != room[2].1);
        assert!(merged.furnidata["wallitemtypes"]["furnitype"].as_array().unwrap().is_empty());
        let nothing = merge(&original, &[("p".into(), json!({}))], &MergeState::default(), &HashSet::new(), &mut rng);
        assert_eq!(nothing.reports[0].added_count(), 0);
    }
}
