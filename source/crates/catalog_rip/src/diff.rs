//! What changed between two rips of the same hotel: pages and offers added, changed (which
//! columns) and removed, as a report (`catalog_changes.json`) and as a Manager import file with
//! only those changes (`catalog_manager_changes.sql`: INSERT the new, UPDATE the changed columns,
//! and DELETE the removed when asked).
//!
//! The changes file assumes the hotel's catalogue holds the earlier rip with the ripped ids (it
//! was imported with `Replace` or `ReplaceRipped`): it is the "bring my catalogue up to date with
//! the hotel's new releases" file. The Manager's import preview refuses a change whose page or
//! offer is not there, so nothing is written blindly.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::export::{comment, RippedCatalog};
use crate::manager::{manager_rows, ManagerExport, ManagerMode, ManagerRow};

/// One page or offer in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffEntry {
    /// "page" or "offer".
    pub what: &'static str,
    /// NORMAL or BUILDER.
    pub catalog_type: &'static str,
    pub id: i64,
    pub label: String,
    /// For a changed row: the columns that changed, with (before, after).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<&'static str, (String, String)>,
}

/// The changes between two rips.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CatalogDiff {
    /// The earlier rip's folder.
    pub since: Option<PathBuf>,
    pub since_ripped_at: String,
    pub added: Vec<DiffEntry>,
    pub changed: Vec<DiffEntry>,
    pub removed: Vec<DiffEntry>,
}

impl CatalogDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    fn count(list: &[DiffEntry], what: &str) -> usize {
        list.iter().filter(|e| e.what == what).count()
    }

    /// "3 pages and 12 offers added, 4 offers changed, 1 page removed".
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no changes".into();
        }
        let mut parts = Vec::new();
        for (list, verb) in [(&self.added, "added"), (&self.changed, "changed"), (&self.removed, "removed")] {
            let pages = Self::count(list, "page");
            let offers = Self::count(list, "offer");
            let mut items = Vec::new();
            if pages > 0 {
                items.push(format!("{pages} page{}", if pages == 1 { "" } else { "s" }));
            }
            if offers > 0 {
                items.push(format!("{offers} offer{}", if offers == 1 { "" } else { "s" }));
            }
            if !items.is_empty() {
                parts.push(format!("{} {verb}", items.join(" and ")));
            }
        }
        parts.join(", ")
    }
}

fn entry(row: &ManagerRow) -> DiffEntry {
    DiffEntry {
        what: if row.page { "page" } else { "offer" },
        catalog_type: row.kind,
        id: row.id,
        label: row.label.clone(),
        changed: BTreeMap::new(),
    }
}

/// The changes from `old` to `new`, and the Manager import file that makes them. Both are
/// exported with the ripped ids (the `Replace` layout) and the same items_base / furnidata.
/// Only what the new rip read is compared: a page it did not read (another branch, no answer)
/// is neither changed nor removed, nor are its offers; a catalogue it did not rip is left out.
pub fn diff(
    old: &RippedCatalog,
    new: &RippedCatalog,
    export: &ManagerExport,
    delete_removed: bool,
) -> (CatalogDiff, String) {
    let export = ManagerExport {
        mode: ManagerMode::Replace,
        existing: None,
        ..*export
    };
    let (old_rows, _, _) = manager_rows(old, &export);
    let (new_rows, _, _) = manager_rows(new, &export);
    let ripped_now: Vec<&str> = new
        .modes
        .iter()
        .map(|m| crate::manager::catalog_type(&m.mode))
        .collect();
    let old_by_key: BTreeMap<_, &ManagerRow> = old_rows.iter().map(|r| (r.key(), r)).collect();
    let new_by_key: BTreeMap<_, &ManagerRow> = new_rows.iter().map(|r| (r.key(), r)).collect();

    let mut report = CatalogDiff {
        since_ripped_at: old.ripped_at.clone(),
        ..CatalogDiff::default()
    };
    let mut inserts = Vec::new();
    let mut updates = Vec::new();
    // The pages whose content the new rip read: their offers are known completely.
    let read: std::collections::HashSet<(&str, String)> = new_rows
        .iter()
        .filter(|r| r.page && r.has_content)
        .map(|r| (r.kind, r.id.to_string()))
        .collect();
    let offer_page = |row: &ManagerRow| -> Option<String> {
        row.columns
            .iter()
            .find(|(c, _)| *c == "page_id")
            .map(|(_, v)| v.clone())
    };
    for row in &new_rows {
        match old_by_key.get(&row.key()) {
            // Not read this time: nothing to compare.
            Some(_) if row.page && !row.has_content => {}
            None => {
                inserts.push(row.insert());
                report.added.push(entry(row));
            }
            Some(before) => {
                let mut changed = BTreeMap::new();
                for ((column, after), (_, was)) in row.columns.iter().zip(&before.columns) {
                    if after != was {
                        changed.insert(*column, (was.clone(), after.clone()));
                    }
                }
                if !changed.is_empty() {
                    let columns: Vec<&str> = changed.keys().copied().collect();
                    updates.push(row.update(&columns));
                    report.changed.push(DiffEntry {
                        changed,
                        ..entry(row)
                    });
                }
            }
        }
    }
    let mut deletes = Vec::new();
    // Offers before pages, so no offer is left on a deleted page.
    let mut removed: Vec<&ManagerRow> = old_rows
        .iter()
        .filter(|r| ripped_now.contains(&r.kind) && !new_by_key.contains_key(&r.key()))
        // An offer is gone only from a page that was read this time.
        .filter(|r| r.page || offer_page(r).is_some_and(|p| read.contains(&(r.kind, p))))
        .collect();
    removed.sort_by_key(|r| r.page);
    for row in removed {
        if delete_removed {
            deletes.push(row.delete());
        }
        report.removed.push(entry(row));
    }

    let mut text = String::new();
    let _ = writeln!(
        text,
        "-- Catalog Studio restricted SQL export.\n-- Changes in the catalogue of {} ({}) since the rip of {}, ripped on {} by All-in-1 Manager {}.\n-- For a hotel whose catalogue holds the earlier rip with the ripped ids. Summary: {}.",
        comment(&new.hotel),
        comment(&new.host),
        comment(&old.ripped_at),
        comment(&new.ripped_at),
        env!("CARGO_PKG_VERSION"),
        report.summary(),
    );
    if !delete_removed && !report.removed.is_empty() {
        let _ = writeln!(
            text,
            "-- {} removed pages/offers are kept in the hotel (removing was not asked).",
            report.removed.len()
        );
    }
    for line in deletes.iter().chain(&inserts).chain(&updates) {
        text.push_str(line);
        text.push('\n');
    }
    (report, text)
}

/// The newest earlier rip of `host` under `root` that has recorded packets, other than `skip`.
pub fn previous_rip(root: &Path, host: &str, skip: Option<&Path>) -> Option<PathBuf> {
    let prefix = format!("{}_", crate::rip::hotel_name(host));
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && Some(p.as_path()) != skip
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
                && p.join(crate::recording::PACKETS_DIR)
                    .join(crate::recording::SESSION_FILE)
                    .is_file()
        })
        .collect();
    dirs.sort();
    dirs.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let e = |what| DiffEntry {
            what,
            catalog_type: "NORMAL",
            id: 1,
            label: String::new(),
            changed: BTreeMap::new(),
        };
        let mut d = CatalogDiff::default();
        assert_eq!(d.summary(), "no changes");
        d.added = vec![e("page"), e("offer"), e("offer")];
        d.removed = vec![e("page")];
        assert_eq!(d.summary(), "1 page and 2 offers added, 1 page removed");
    }

    #[test]
    fn finds_the_previous_rip() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["habbo.nl_2026-01-01_10-00-00", "habbo.nl_2026-02-01_10-00-00", "habbo.de_2026-03-01_10-00-00", "habbo.nl_2026-04-01_10-00-00"] {
            let packets = dir.path().join(name).join("packets");
            std::fs::create_dir_all(&packets).unwrap();
            if name != "habbo.nl_2026-04-01_10-00-00" {
                std::fs::write(packets.join("session.json"), "{}").unwrap();
            }
        }
        let found = previous_rip(dir.path(), "game-nl.habbo.com", None).unwrap();
        assert!(found.ends_with("habbo.nl_2026-02-01_10-00-00"));
        let found = previous_rip(dir.path(), "game-nl.habbo.com", Some(&found)).unwrap();
        assert!(found.ends_with("habbo.nl_2026-01-01_10-00-00"));
        assert!(previous_rip(dir.path(), "game.x.net", None).is_none());
    }
}
