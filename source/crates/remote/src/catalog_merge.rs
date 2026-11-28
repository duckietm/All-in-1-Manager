//! The Catalogue manager's "Merge a rip": a ripped catalogue fitted into the hotel's (the file is made by the ripper's
//! merge, `aio_catalog_rip::merge_plan`) and, in the same preview and the same transaction, the furni interactions the
//! merge's furni should have.
//!
//! Interactions: for every items_base furni the rip's offers resolve to, the interaction its bundle (.hab or .nitro in
//! the owner's bundled/furniture folder) gives ([`furni_import::interaction_for`]) is proposed only where
//! items_base.interaction_type is 'default' or empty and the mapped value is something else: a specific type is never
//! changed. They are written with the catalogue changes in one transaction.
//!
//! Undo: the catalogue's history (`catalog_manager_history`) is shared with the emulator's Catalog Studio, which reads
//! only PAGE and OFFER entries, so the old interaction types go to a small table of the Manager's own,
//! [`RECORD_TABLE`], one row per merge with the history revision it was written with. Undoing that operation in the
//! Manager's History tab puts the old values back in the same transaction (only where items_base still holds what the
//! merge wrote). Undo from Octane's Catalog Studio puts back only pages and offers.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::catalog_journal::{self as journal, Change, Entity, Operation};
use crate::catalog_transfer;
use crate::furni_import::{self, Bundle};
use crate::server::{Result, ServerError};
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The Manager's record of the interaction types a merge changed (by history revision).
pub const RECORD_TABLE: &str = "aio_manager_items_history";

/// One items_base row's interaction_type, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRow {
    pub id: i64,
    pub item_name: String,
    pub interaction_type: Option<String>,
}

/// An interaction_type the merge changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionChange {
    pub item_id: i64,
    pub item_name: String,
    /// As it is (None: NULL).
    pub before: Option<String>,
    pub after: String,
}

/// 'default', empty or NULL: nothing specific.
pub fn is_plain(value: Option<&str>) -> bool {
    value.map(str::trim).is_none_or(|v| v.is_empty() || v.eq_ignore_ascii_case("default"))
}

/// The interaction changes for these rows: only where the row is plain and its bundle maps to something else. `read`
/// gives a bundle by its name (the classname before '*'); each bundle is read once.
pub fn propose(rows: &[ItemRow], read: &dyn Fn(&str) -> Option<Bundle>) -> Vec<InteractionChange> {
    let mut bundles: HashMap<String, Option<Bundle>> = HashMap::new();
    let mut list = Vec::new();
    for row in rows {
        if !is_plain(row.interaction_type.as_deref()) {
            continue;
        }
        let name = furni_import::bundle_name(&row.item_name);
        if name.is_empty() {
            continue;
        }
        let bundle = bundles.entry(name.to_lowercase()).or_insert_with(|| read(name));
        let Some(bundle) = *bundle else { continue };
        let mapped = furni_import::interaction_for(&row.item_name, &bundle);
        if is_plain(Some(mapped)) {
            continue;
        }
        list.push(InteractionChange { item_id: row.id, item_name: row.item_name.clone(), before: row.interaction_type.clone(), after: mapped.to_string() });
    }
    list
}

fn text(value: Option<&SqlValue>) -> Option<String> {
    match value {
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => Some(t.clone()),
        Some(SqlValue::Bytes(b)) => Some(String::from_utf8_lossy(b).into_owned()),
        Some(SqlValue::Int(v)) => Some(v.to_string()),
        Some(SqlValue::UInt(v)) => Some(v.to_string()),
        _ => None,
    }
}

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        other => text(other).and_then(|t| t.trim().parse().ok()).unwrap_or(0),
    }
}

/// id, item_name and interaction_type of these items_base rows.
pub async fn read_items(session: &SqlSession, database: &str, ids: &[i64]) -> Result<Vec<ItemRow>> {
    let db = quote(database);
    let mut rows = Vec::new();
    for chunk in ids.chunks(500) {
        let sql = format!("SELECT id, item_name, interaction_type FROM {db}.items_base WHERE id IN ({}) ORDER BY id", vec!["?"; chunk.len()].join(", "));
        for row in session.select(&sql, chunk.iter().map(|id| SqlValue::Int(*id)).collect()).await? {
            rows.push(ItemRow { id: int(row.first()), item_name: text(row.get(1)).unwrap_or_default(), interaction_type: text(row.get(2)) });
        }
    }
    Ok(rows)
}

/// What the file changes in the catalogue; a file with no statements (a rip already merged) changes nothing. A merge
/// only creates pages and offers: a file that would change or delete one of the hotel's is refused.
pub fn catalogue_changes(current: &BTreeMap<Entity, Value>, sql: &str) -> std::result::Result<Vec<Change>, String> {
    if catalog_transfer::statements(sql)?.is_empty() {
        return Ok(Vec::new());
    }
    let target = catalog_transfer::target(current, sql)?;
    let changes = catalog_transfer::changes(current, &target);
    only_creations(&changes)?;
    Ok(changes)
}

/// Err when a change is not the creation of a new page or offer.
fn only_creations(changes: &[Change]) -> std::result::Result<(), String> {
    match changes.iter().find(|c| c.before.is_some()) {
        Some(c) => Err(format!(
            "a merge only adds pages and offers, but this file would {} the hotel's {} #{}",
            if c.after.is_some() { "change" } else { "delete" },
            if c.entity.page { "page" } else { "offer" },
            c.entity.id
        )),
        None => Ok(()),
    }
}

/// The preview of a merge: the catalogue as it was, what the file changes, the interactions changed.
#[derive(Debug, Clone)]
pub struct Preview {
    pub current: BTreeMap<Entity, Value>,
    pub changes: Vec<Change>,
    pub interactions: Vec<InteractionChange>,
    /// Furni of the rip whose bundle is not in the folder (their interaction is not checked).
    pub without_bundle: usize,
    /// The database has the catalogue history (so the merge can be undone).
    pub history: bool,
}

/// Reads the catalogue and the rip's furni, and works out the changes; nothing is written. `furniture`: the folder of
/// the owner's bundles (None: no interaction changes).
pub async fn preview(session: &SqlSession, database: &str, sql: &str, items: &BTreeSet<i64>, furniture: Option<&Path>) -> Result<Preview> {
    let current = journal::capture_all(session, database).await?;
    let changes = catalogue_changes(&current, sql).map_err(ServerError)?;
    let (interactions, without_bundle) = match furniture {
        Some(folder) => {
            let rows = read_items(session, database, &items.iter().copied().collect::<Vec<_>>()).await?;
            let folder: PathBuf = folder.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let missing = std::sync::atomic::AtomicUsize::new(0);
                let read = |name: &str| {
                    let bundle = furni_import::read_bundle_in(&folder, name);
                    if bundle.is_none() {
                        missing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    bundle
                };
                let list = propose(&rows, &read);
                (list, missing.into_inner())
            })
            .await
            .map_err(|e| ServerError(e.to_string()))?
        }
        None => (Vec::new(), 0),
    };
    let history = journal::has_history(session, database).await;
    Ok(Preview { current, changes, interactions, without_bundle, history })
}

/// The record of the changes, as [`RECORD_TABLE`] keeps it.
pub fn record_json(changes: &[InteractionChange]) -> String {
    Value::Array(changes.iter().map(|c| json!({"id": c.item_id, "name": c.item_name, "before": c.before, "after": c.after})).collect()).to_string()
}

/// Reads a record back.
pub fn parse_record(text: &str) -> Vec<InteractionChange> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    list.iter()
        .filter_map(|v| {
            Some(InteractionChange {
                item_id: v.get("id")?.as_i64()?,
                item_name: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                before: v.get("before").and_then(Value::as_str).map(str::to_string),
                after: v.get("after")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn param(value: Option<&String>) -> SqlValue {
    value.map(|v| SqlValue::Text(v.clone())).unwrap_or(SqlValue::Null)
}

/// The UPDATEs of the interaction changes (each only while the row still holds the value it had).
pub fn interaction_statements(database: &str, changes: &[InteractionChange]) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    changes
        .iter()
        .map(|c| {
            (
                format!("UPDATE {db}.items_base SET interaction_type = ? WHERE id = ? AND interaction_type <=> ?"),
                vec![SqlValue::Text(c.after.clone()), SqlValue::Int(c.item_id), param(c.before.as_ref())],
            )
        })
        .collect()
}

/// The statement that creates [`RECORD_TABLE`] when it is not there (before the transaction: DDL commits).
pub fn record_table_sql(database: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {}.{RECORD_TABLE} (id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, revision BIGINT NOT NULL, \
         changes_json LONGTEXT NOT NULL, undone TINYINT(1) NOT NULL DEFAULT 0, created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
         KEY revision (revision)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        quote(database)
    )
}

/// The record row, with the revision the history row of the same transaction got (it comes after it).
pub fn record_statement(database: &str, changes: &[InteractionChange]) -> (String, Vec<SqlValue>) {
    let db = quote(database);
    (
        format!("INSERT INTO {db}.{RECORD_TABLE} (revision, changes_json) SELECT revision, ? FROM {db}.catalog_manager_state WHERE singleton_id = 1"),
        vec![SqlValue::Text(record_json(changes))],
    )
}

/// Every statement of a merge, in order: the catalogue changes with their history row, the interactions, the record.
pub fn apply_statements(database: &str, preview: &Preview, summary: &str) -> Vec<(String, Vec<SqlValue>)> {
    let mut statements = catalog_transfer::apply_statements(database, &preview.changes, preview.history.then_some(summary));
    statements.extend(interaction_statements(database, &preview.interactions));
    if preview.history && !preview.interactions.is_empty() {
        statements.push(record_statement(database, &preview.interactions));
    }
    statements
}

/// The interactions that are no longer as `expect` says (`after`: as the merge left them; else as before it).
fn moved_on(changes: &[InteractionChange], now: &[ItemRow], after: bool) -> Option<String> {
    let now: HashMap<i64, Option<&str>> = now.iter().map(|r| (r.id, r.interaction_type.as_deref())).collect();
    changes.iter().find_map(|c| {
        let expected = if after { Some(c.after.as_str()) } else { c.before.as_deref() };
        let value = now.get(&c.item_id).copied().flatten();
        (value != expected).then(|| format!("items_base #{} ({}) is now '{}'", c.item_id, c.item_name, value.unwrap_or("NULL")))
    })
}

/// Writes the previewed merge in one transaction, when the catalogue and the furni are still as at the preview.
pub async fn apply(session: &SqlSession, database: &str, preview: &Preview, summary: &str) -> Result<()> {
    only_creations(&preview.changes).map_err(ServerError)?;
    if journal::capture_all(session, database).await? != preview.current {
        return Err(ServerError("The catalogue changed since the preview: preview the merge again.".into()).into());
    }
    if !preview.interactions.is_empty() {
        let ids: Vec<i64> = preview.interactions.iter().map(|c| c.item_id).collect();
        if let Some(problem) = moved_on(&preview.interactions, &read_items(session, database, &ids).await?, false) {
            return Err(ServerError(format!("A furni changed since the preview ({problem}): preview the merge again.")).into());
        }
        if preview.history {
            if let Err(error) = session.execute(&[(record_table_sql(database), Vec::new())]).await {
                return Err(record_table_error(error));
            }
        }
    }
    session.execute(&apply_statements(database, preview, summary)).await?;
    Ok(())
}

/// The error of creating [`RECORD_TABLE`], said plainly when the MariaDB account may not create tables.
fn record_table_error(error: anyhow::Error) -> anyhow::Error {
    let denied = matches!(crate::sql_session::server_error_code(&error), Some(1142 | 1044)) || format!("{error:#}").contains("command denied");
    if denied {
        return ServerError(format!(
            "The MariaDB account may not create the table {RECORD_TABLE}, which keeps the old furni interactions so the merge can be undone \
             (it needs the CREATE privilege on the hotel database). Log in to MariaDB with an account that may create tables, or empty \
             the Furniture bundles field to merge without changing interactions. Nothing was written. ({error:#})"
        ))
        .into();
    }
    error.context(format!("creating {RECORD_TABLE}"))
}

/// The interaction records not undone yet, by history revision: none when the table is not there (MariaDB 1146, no
/// merge changed interactions yet); any other error is returned (then nothing may be undone: an undo without the
/// record would leave the interactions behind).
pub async fn records(session: &SqlSession, database: &str) -> Result<BTreeMap<i64, Vec<InteractionChange>>> {
    let sql = format!("SELECT revision, changes_json FROM {}.{RECORD_TABLE} WHERE undone = 0", quote(database));
    let rows = match session.select(&sql, Vec::new()).await {
        Ok(rows) => rows,
        Err(error) if crate::sql_session::server_error_code(&error) == Some(1146) => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.context(format!("reading {RECORD_TABLE}"))),
    };
    Ok(rows.iter().map(|r| (int(r.first()), parse_record(&text(r.get(1)).unwrap_or_default()))).filter(|(_, c)| !c.is_empty()).collect())
}

/// What an undo of an operation (with its interaction record) can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoPlan {
    /// Pages, offers and interactions back.
    Whole,
    /// The pages and offers are already back as they were (undone in Octane's Catalog Studio): only the interactions.
    InteractionsOnly,
    /// Why it cannot be undone.
    Blocked(String),
}

/// The undo plan from the operation's entries as they are `now`, and its furni as they are `items`.
pub fn plan_undo(operation: &Operation, now: &BTreeMap<Entity, Option<Value>>, interactions: &[InteractionChange], items: &[ItemRow]) -> UndoPlan {
    let mut last: BTreeMap<Entity, (Option<Value>, Option<Value>)> = BTreeMap::new();
    for change in &operation.entries {
        let entry = last.entry(change.entity).or_insert((change.before.clone(), None));
        entry.1 = change.after.clone();
    }
    let state = |e: &Entity| now.get(e).and_then(Option::as_ref);
    let as_left = last.iter().all(|(e, (_, after))| journal::still_as_left(after.as_ref(), state(e)));
    let back = !last.is_empty() && last.iter().all(|(e, (before, _))| journal::still_as_left(before.as_ref(), state(e)));
    if let Some(problem) = moved_on(interactions, items, true) {
        return UndoPlan::Blocked(format!("{problem}, not what the merge wrote: change it back by hand first."));
    }
    if as_left {
        UndoPlan::Whole
    } else if back && !interactions.is_empty() {
        UndoPlan::InteractionsOnly
    } else {
        let changed = last.iter().find(|(e, (_, after))| !journal::still_as_left(after.as_ref(), state(e))).map(|(e, _)| *e);
        let what = changed.map(|e| format!("{} #{}", if e.page { "Page" } else { "Offer" }, e.id)).unwrap_or_default();
        UndoPlan::Blocked(format!("{what} was changed after this operation: undo that first, or change it back by hand."))
    }
}

/// What an undo of the operation can do now.
pub async fn undo_plan(session: &SqlSession, database: &str, operation: &Operation, interactions: &[InteractionChange]) -> Result<UndoPlan> {
    let entities: Vec<Entity> = operation.entries.iter().map(|c| c.entity).collect::<BTreeSet<_>>().into_iter().collect();
    let now = if entities.is_empty() {
        BTreeMap::new()
    } else {
        match journal::capture(session, database, &entities).await {
            Some(now) => now,
            None => return Ok(UndoPlan::Blocked("The catalogue's history cannot be read.".into())),
        }
    };
    let ids: Vec<i64> = interactions.iter().map(|c| c.item_id).collect();
    let items = if ids.is_empty() { Vec::new() } else { read_items(session, database, &ids).await? };
    Ok(plan_undo(operation, &now, interactions, &items))
}

/// The statements that undo the operation and its interaction changes in one transaction; with
/// [`UndoPlan::InteractionsOnly`] only the interactions (the pages and offers are back already).
pub fn undo_statements(database: &str, operation: &Operation, interactions: &[InteractionChange], plan: &UndoPlan) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    let mut statements: Vec<(String, Vec<SqlValue>)> = interactions
        .iter()
        .map(|c| {
            (
                format!("UPDATE {db}.items_base SET interaction_type = ? WHERE id = ? AND interaction_type = ?"),
                vec![param(c.before.as_ref()), SqlValue::Int(c.item_id), SqlValue::Text(c.after.clone())],
            )
        })
        .collect();
    if !interactions.is_empty() {
        statements.push((format!("UPDATE {db}.{RECORD_TABLE} SET undone = 1 WHERE revision = ?"), vec![SqlValue::Int(operation.revision)]));
    }
    if *plan == UndoPlan::Whole {
        statements.extend(journal::undo_statements(database, operation));
    }
    statements
}

/// Undoes the operation and its interaction changes as `plan` ([`undo_plan`]) says; a blocked plan writes nothing.
pub async fn undo(session: &SqlSession, database: &str, operation: &Operation, interactions: &[InteractionChange], plan: &UndoPlan) -> Result<()> {
    if let UndoPlan::Blocked(problem) = plan {
        return Err(ServerError(problem.clone()).into());
    }
    session.execute(&undo_statements(database, operation, interactions, plan)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, name: &str, interaction: Option<&str>) -> ItemRow {
        ItemRow { id, item_name: name.into(), interaction_type: interaction.map(str::to_string) }
    }

    fn bundle(interaction: Option<&'static str>) -> Bundle {
        Bundle { width: 1.0, length: 1.0, height: 1.0, modes: 2, multiheight: None, interaction }
    }

    fn bundles(name: &str) -> Option<Bundle> {
        match name {
            "dice_master" => Some(bundle(Some("dice"))),
            "trophy_gold" => Some(bundle(Some("trophy"))),
            "chair_plain" => Some(bundle(None)),
            _ => None,
        }
    }

    #[test]
    fn interactions_change_only_where_default() {
        let rows = [
            row(1, "dice_master", Some("default")),
            row(2, "dice_master*2", Some("")),
            row(3, "trophy_gold", None),
            row(4, "dice_master*3", Some("wf_trg_state_changed")),
            row(5, "trophy_gold*1", Some("trophy")),
            row(6, "chair_plain", Some("default")),
            row(7, "not_in_folder", Some("default")),
            row(8, "DICE_MASTER*4", Some("DEFAULT")),
        ];
        let reads = std::cell::Cell::new(0);
        let read = |name: &str| {
            reads.set(reads.get() + 1);
            bundles(&name.to_lowercase())
        };
        let found = propose(&rows, &read);
        let ids: Vec<(i64, &str)> = found.iter().map(|c| (c.item_id, c.after.as_str())).collect();
        // A specific type (4) is never replaced, not even downgraded; 'default' in the bundle (6) changes nothing.
        assert_eq!(ids, vec![(1, "dice"), (2, "dice"), (3, "trophy"), (8, "dice")]);
        assert_eq!(found[2].before, None);
        assert_eq!(found[1].before.as_deref(), Some(""));
        assert_eq!(reads.get(), 4, "one read per bundle: dice_master, trophy_gold, chair_plain, not_in_folder");
    }

    #[test]
    fn records_read_back_and_undo_restores_the_old_values() {
        let changes = vec![
            InteractionChange { item_id: 1, item_name: "dice_master".into(), before: Some("default".into()), after: "dice".into() },
            InteractionChange { item_id: 3, item_name: "trophy_gold".into(), before: None, after: "trophy".into() },
        ];
        assert_eq!(parse_record(&record_json(&changes)), changes);

        let preview = Preview { current: BTreeMap::new(), changes: Vec::new(), interactions: changes.clone(), without_bundle: 0, history: true };
        let statements = apply_statements("hotel", &preview, "All-in-1 Manager: merge");
        let updates: Vec<&(String, Vec<SqlValue>)> = statements.iter().filter(|(s, _)| s.contains("items_base")).collect();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].1, vec![SqlValue::Text("dice".into()), SqlValue::Int(1), SqlValue::Text("default".into())]);
        assert_eq!(updates[1].1[2], SqlValue::Null, "a NULL before is matched with <=>");
        // The record comes after the history row, so it gets that row's revision.
        let history = statements.iter().position(|(s, _)| s.contains("catalog_manager_history")).unwrap();
        let record = statements.iter().position(|(s, _)| s.contains(RECORD_TABLE)).unwrap();
        assert!(history < record);
        // Without the history there is nothing to undo with: no record.
        let without = Preview { history: false, ..preview.clone() };
        assert!(!apply_statements("hotel", &without, "x").iter().any(|(s, _)| s.contains(RECORD_TABLE) || s.contains("catalog_manager_history")));

        let operation = Operation { id: 9, revision: 41, actor: String::new(), summary: "merge".into(), source: "SQL".into(), created_at: String::new(), entries: Vec::new() };
        let undo = undo_statements("hotel", &operation, &changes, &UndoPlan::Whole);
        assert_eq!(undo[0].0, "UPDATE `hotel`.items_base SET interaction_type = ? WHERE id = ? AND interaction_type = ?");
        assert_eq!(undo[0].1, vec![SqlValue::Text("default".into()), SqlValue::Int(1), SqlValue::Text("dice".into())]);
        assert_eq!(undo[1].1[0], SqlValue::Null, "back to NULL");
        assert!(undo[2].0.contains("SET undone = 1") && undo[2].1 == vec![SqlValue::Int(41)]);
        assert!(undo.iter().any(|(s, _)| s.contains("'UNDO'")), "recorded as an undo in the history");

        // Undo only while items_base holds what the merge wrote.
        assert_eq!(moved_on(&changes, &[row(1, "dice_master", Some("dice")), row(3, "trophy_gold", Some("trophy"))], true), None);
        assert!(moved_on(&changes, &[row(1, "dice_master", Some("dice")), row(3, "trophy_gold", Some("default"))], true).unwrap().contains("#3"));
        // Apply only while it holds what the preview read.
        assert_eq!(moved_on(&changes, &[row(1, "dice_master", Some("default")), row(3, "trophy_gold", None)], false), None);
    }

    #[test]
    fn a_rip_already_merged_changes_nothing() {
        let file = "-- Catalog Studio restricted SQL export.\n-- skipped: NORMAL offer 1 'x' on page 2: page 7 already sells items_base 5\n";
        assert_eq!(catalogue_changes(&BTreeMap::new(), file).unwrap(), Vec::new());
        assert!(catalogue_changes(&BTreeMap::new(), "DROP TABLE x").is_err());
    }

    fn snapshot(caption: &str) -> Value {
        json!({"catalogType": "NORMAL", "pageId": 7, "parentId": -1, "caption": caption})
    }

    #[test]
    fn a_missing_create_privilege_is_said_plainly() {
        let error = record_table_error(anyhow::anyhow!("ERROR 1142 (42000): CREATE command denied to user 'hotel'@'localhost' for table 'aio_manager_items_history'"));
        let text = format!("{error:#}");
        assert!(text.contains("CREATE privilege") && text.contains("Nothing was written"), "{text}");
        let other = format!("{:#}", record_table_error(anyhow::anyhow!("lost connection")));
        assert!(!other.contains("CREATE privilege") && other.contains("creating aio_manager_items_history"), "{other}");
    }

    #[test]
    fn a_merge_never_changes_or_deletes_the_hotels_rows() {
        let page = Entity { builders: false, page: true, id: 7 };
        let current = BTreeMap::from([(page, snapshot("Mine"))]);
        let error = catalogue_changes(&current, "UPDATE catalog_pages SET caption = 'Theirs' WHERE id = 7").unwrap_err();
        assert!(error.contains("would change the hotel's page #7"), "{error}");
        assert!(catalogue_changes(&current, "DELETE FROM catalog_pages WHERE id = 7").unwrap_err().contains("delete"));
        assert_eq!(catalogue_changes(&current, "INSERT INTO catalog_pages (id, caption) VALUES (8, 'New')").unwrap().len(), 1);
    }

    #[test]
    fn undo_after_octane_undid_the_pages_puts_back_the_interactions_only() {
        let page = Entity { builders: false, page: true, id: 8 };
        let operation = Operation {
            id: 9,
            revision: 41,
            actor: String::new(),
            summary: "merge".into(),
            source: "SQL".into(),
            created_at: String::new(),
            entries: vec![Change { entity: page, operation: "CREATE".into(), before: None, after: Some(snapshot("New")) }],
        };
        let changes = vec![InteractionChange { item_id: 1, item_name: "dice_master".into(), before: Some("default".into()), after: "dice".into() }];
        let merged = [row(1, "dice_master", Some("dice"))];
        let still = BTreeMap::from([(page, Some(snapshot("New")))]);
        let gone = BTreeMap::from([(page, None)]);
        let edited = BTreeMap::from([(page, Some(snapshot("Edited")))]);
        assert_eq!(plan_undo(&operation, &still, &changes, &merged), UndoPlan::Whole);
        // Octane undid the pages: the interactions alone go back, without another catalogue undo.
        let only = plan_undo(&operation, &gone, &changes, &merged);
        assert_eq!(only, UndoPlan::InteractionsOnly);
        let statements = undo_statements("hotel", &operation, &changes, &only);
        assert_eq!(statements.len(), 2, "{statements:?}");
        assert!(statements[0].0.contains("items_base") && statements[1].0.contains("SET undone = 1"));
        // Pages changed since, or a furni no longer as the merge left it: nothing.
        assert!(matches!(plan_undo(&operation, &edited, &changes, &merged), UndoPlan::Blocked(p) if p.contains("Page #8")));
        assert!(matches!(plan_undo(&operation, &still, &changes, &[row(1, "dice_master", Some("wf_x"))]), UndoPlan::Blocked(p) if p.contains("#1")));
        // Without interactions an operation already undone is not "undone" again.
        assert!(matches!(plan_undo(&operation, &gone, &[], &[]), UndoPlan::Blocked(_)));
    }
}
