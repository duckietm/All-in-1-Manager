//! The catalogue's change history, shared with the emulator's Catalog Studio (Octane's catalogue admin): every change
//! the Manager makes to pages and offers is written to `catalog_manager_history` as the emulator writes its own (one
//! row per operation, a JSON list of entries with each page or offer before and after, in the emulator's snapshot
//! form), and `catalog_manager_state.revision` goes up, so Octane sees that the catalogue changed and its history and
//! undo know about it. New pages and offers take their id from `catalog_id_sequences`, as the emulator's do.
//!
//! A database without these tables (an emulator before Catalog Studio) has no history: then nothing is written and
//! ids come from AUTO_INCREMENT.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::catalog_admin::Catalogue;
use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

/// A page or an offer of one catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Entity {
    pub builders: bool,
    pub page: bool,
    pub id: i64,
}

impl Entity {
    pub fn page(cat: Catalogue, id: i64) -> Self {
        Entity { builders: cat == Catalogue::Builders, page: true, id }
    }

    pub fn offer(cat: Catalogue, id: i64) -> Self {
        Entity { builders: cat == Catalogue::Builders, page: false, id }
    }

    pub fn cat(&self) -> Catalogue {
        if self.builders {
            Catalogue::Builders
        } else {
            Catalogue::Normal
        }
    }

    fn catalog_type(&self) -> &'static str {
        if self.builders {
            "BUILDER"
        } else {
            "NORMAL"
        }
    }
}

/// The entities as they were: their snapshot JSON, None when the row did not exist. None as a whole when the database
/// has no history.
pub type Snapshot = Option<BTreeMap<Entity, Option<Value>>>;

/// The emulator's reads of live pages or offers (`JdbcCatalogLiveSnapshotRepository`), without a WHERE.
fn base_sql(database: &str, entity: Entity) -> String {
    let db = quote(database);
    match (entity.page, entity.builders) {
        (true, false) => format!(
            "SELECT id, parent_id, caption_save, caption, page_layout, icon_color, icon_image, min_rank, order_num, visible, enabled, \
             club_only, catalog_mode, vip_only, page_headline, page_teaser, COALESCE(page_special, ''), page_text1, page_text2, \
             page_text_details, page_text_teaser, COALESCE(room_id, 0), includes FROM {db}.catalog_pages"
        ),
        (true, true) => format!(
            "SELECT id, parent_id, '', caption, page_layout, icon_color, icon_image, 1, order_num, visible, enabled, 0, 'BUILDER', 0, \
             page_headline, page_teaser, COALESCE(page_special, ''), page_text1, page_text2, page_text_details, page_text_teaser, 0, '' \
             FROM {db}.catalog_pages_bc"
        ),
        (false, false) => format!(
            "SELECT id, item_ids, page_id, catalog_name, cost_credits, cost_points, points_type, amount, limited_stack, order_number, \
             offer_id, song_id, extradata, have_offer, club_only FROM {db}.catalog_items"
        ),
        (false, true) => format!(
            "SELECT id, item_ids, page_id, catalog_name, 0, 0, 0, 1, 0, order_number, -1, 0, extradata, 1, 0 FROM {db}.catalog_items_bc"
        ),
    }
}

/// [`base_sql`] for these ids.
fn select_sql(database: &str, entity: Entity, count: usize) -> String {
    format!("{} WHERE id IN ({})", base_sql(database, entity), vec!["?"; count.max(1)].join(", "))
}

/// Every page and offer of both catalogues as the emulator's snapshots (a missing Builders Club table is empty).
pub async fn capture_all(session: &SqlSession, database: &str) -> Result<BTreeMap<Entity, Value>> {
    let mut all = BTreeMap::new();
    for (builders, page) in [(false, true), (false, false), (true, true), (true, false)] {
        let probe = Entity { builders, page, id: 0 };
        let rows = match session.select(&format!("{} ORDER BY id", base_sql(database, probe)), Vec::new()).await {
            Ok(rows) => rows,
            Err(_) if builders => continue,
            Err(error) => return Err(error),
        };
        for row in rows {
            let entity = Entity { builders, page, id: int(row.first()) };
            all.insert(entity, snapshot_json(entity, &row));
        }
    }
    Ok(all)
}

const PAGE_FIELDS: &[(&str, char)] = &[
    ("pageId", 'i'),
    ("parentId", 'i'),
    ("captionSave", 's'),
    ("caption", 's'),
    ("pageLayout", 's'),
    ("iconColor", 'i'),
    ("iconImage", 'i'),
    ("minRank", 'i'),
    ("orderNum", 'i'),
    ("visible", 'b'),
    ("enabled", 'b'),
    ("clubOnly", 'b'),
    ("catalogMode", 's'),
    ("vipOnly", 'b'),
    ("pageHeadline", 's'),
    ("pageTeaser", 's'),
    ("pageSpecial", 's'),
    ("pageText1", 's'),
    ("pageText2", 's'),
    ("pageTextDetails", 's'),
    ("pageTextTeaser", 's'),
    ("roomId", 'i'),
    ("includes", 's'),
];

const OFFER_FIELDS: &[(&str, char)] = &[
    ("offerId", 'i'),
    ("itemIds", 's'),
    ("pageId", 'i'),
    ("catalogName", 's'),
    ("costCredits", 'i'),
    ("costPoints", 'i'),
    ("pointsType", 'i'),
    ("amount", 'i'),
    ("limitedStack", 'i'),
    ("orderNumber", 'i'),
    ("offerIdClient", 'i'),
    ("songId", 'i'),
    ("extradata", 's'),
    ("haveOffer", 'b'),
    ("clubOnly", 'b'),
];

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        Some(SqlValue::Float(v)) => *v as i64,
        Some(SqlValue::Double(v)) => *v as i64,
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: Option<&SqlValue>) -> Option<String> {
    match value {
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => Some(t.clone()),
        Some(SqlValue::Int(v)) => Some(v.to_string()),
        Some(SqlValue::UInt(v)) => Some(v.to_string()),
        _ => None,
    }
}

/// One row as the emulator's snapshot JSON (a NULL text is left out, as Gson does).
pub fn snapshot_json(entity: Entity, row: &[SqlValue]) -> Value {
    let fields = if entity.page { PAGE_FIELDS } else { OFFER_FIELDS };
    let mut object = Map::new();
    object.insert("catalogType".into(), json!(entity.catalog_type()));
    for (i, (name, kind)) in fields.iter().enumerate() {
        let value = row.get(i);
        let json = match kind {
            'i' => json!(int(value)),
            'b' => json!(int(value) == 1),
            _ => match text(value) {
                Some(t) => json!(t),
                None => continue,
            },
        };
        object.insert((*name).into(), json);
    }
    Value::Object(object)
}

/// Whether the database has the emulator's history.
pub async fn has_history(session: &SqlSession, database: &str) -> bool {
    session.select(&format!("SELECT revision FROM {}.catalog_manager_state WHERE singleton_id = 1", quote(database)), Vec::new()).await.is_ok_and(|rows| !rows.is_empty())
}

/// The entities as they are now (None when the database has no history, or they cannot be read).
pub async fn capture(session: &SqlSession, database: &str, entities: &[Entity]) -> Snapshot {
    if entities.is_empty() || !has_history(session, database).await {
        return None;
    }
    let mut found: BTreeMap<Entity, Option<Value>> = entities.iter().map(|e| (*e, None)).collect();
    let mut groups: BTreeMap<(bool, bool), Vec<i64>> = BTreeMap::new();
    for entity in entities {
        groups.entry((entity.builders, entity.page)).or_default().push(entity.id);
    }
    for ((builders, page), ids) in groups {
        let probe = Entity { builders, page, id: 0 };
        for chunk in ids.chunks(500) {
            let rows = session.select(&select_sql(database, probe, chunk.len()), chunk.iter().map(|id| SqlValue::Int(*id)).collect()).await.ok()?;
            for row in rows {
                let entity = Entity { builders, page, id: int(row.first()) };
                found.insert(entity, Some(snapshot_json(entity, &row)));
            }
        }
    }
    Some(found)
}

/// The fields a move changes: a move is an update of only these.
fn move_fields(page: bool) -> &'static [&'static str] {
    if page {
        &["parentId", "orderNum"]
    } else {
        &["pageId", "orderNumber"]
    }
}

/// The history entries between two snapshots of the same entities, as the emulator writes them (`CatalogChangeEntry`).
pub fn entries(before: &BTreeMap<Entity, Option<Value>>, after: &BTreeMap<Entity, Option<Value>>) -> Vec<Value> {
    let mut list = Vec::new();
    for (entity, old) in before {
        let new = after.get(entity).cloned().flatten();
        if *old == new || entity.id <= 0 {
            continue;
        }
        let operation = match (old, &new) {
            (None, Some(_)) => "CREATE",
            (Some(_), None) => "DELETE",
            (Some(a), Some(b)) => {
                let moved = a.as_object().zip(b.as_object()).is_some_and(|(a, b)| {
                    a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).all(|k| move_fields(entity.page).contains(&k.as_str()))
                });
                if moved {
                    "MOVE"
                } else {
                    "UPDATE"
                }
            }
            (None, None) => continue,
        };
        let mut entry = Map::new();
        entry.insert("id".into(), json!(0));
        entry.insert("entityType".into(), json!(if entity.page { "PAGE" } else { "OFFER" }));
        entry.insert("catalogType".into(), json!(entity.catalog_type()));
        entry.insert("entityId".into(), json!(entity.id));
        entry.insert("operation".into(), json!(operation));
        if let Some(old) = old {
            entry.insert("beforeJson".into(), json!(old.to_string()));
        }
        if let Some(new) = &new {
            entry.insert("afterJson".into(), json!(new.to_string()));
        }
        list.push(Value::Object(entry));
    }
    list
}

/// The statements that add one history row (and raise the revision), in the transaction of the change or after it.
pub fn history_statements(database: &str, summary: &str, entries: &[Value]) -> Vec<(String, Vec<SqlValue>)> {
    let db = quote(database);
    let summary: String = format!("All-in-1 Manager: {summary}").chars().take(255).collect();
    vec![
        (format!("UPDATE {db}.catalog_manager_state SET revision = revision + 1 WHERE singleton_id = 1"), Vec::new()),
        (
            format!(
                "INSERT INTO {db}.catalog_manager_history (revision, actor_id, summary, source, changes_json) \
                 SELECT revision, 0, ?, 'UI', ? FROM {db}.catalog_manager_state WHERE singleton_id = 1"
            ),
            vec![SqlValue::Text(summary), SqlValue::Text(Value::Array(entries.to_vec()).to_string())],
        ),
    ]
}

/// Writes what changed since `before` to the history. Nothing when the database has no history or nothing changed;
/// a failure is not the change's failure, so it is only returned for the caller to show.
pub async fn record(session: &SqlSession, database: &str, summary: &str, before: Snapshot) -> Result<()> {
    let Some(before) = before else { return Ok(()) };
    let entities: Vec<Entity> = before.keys().copied().collect();
    let Some(after) = capture(session, database, &entities).await else { return Ok(()) };
    let list = entries(&before, &after);
    if list.is_empty() {
        return Ok(());
    }
    session.execute(&history_statements(database, summary, &list)).await?;
    Ok(())
}

/// Reserves `count` ids for new pages (`page`) or offers of `cat` from the emulator's `catalog_id_sequences`, never
/// below the table's highest id; returns the first. None when the database has no sequences (then AUTO_INCREMENT).
pub async fn reserve_ids(session: &SqlSession, database: &str, cat: Catalogue, page: bool, count: usize) -> Option<i64> {
    if count == 0 {
        return None;
    }
    let db = quote(database);
    let table = if page { cat.pages_table() } else { cat.items_table() };
    let sql = format!(
        "UPDATE {db}.catalog_id_sequences SET next_id = LAST_INSERT_ID(GREATEST(next_id, (SELECT COALESCE(MAX(id), 0) + 1 FROM {db}.{table})) + ?) \
         WHERE entity_type = ? AND catalog_type = ?"
    );
    let params = vec![
        SqlValue::Int(count as i64),
        SqlValue::Text(if page { "PAGE" } else { "OFFER" }.into()),
        SqlValue::Text(if cat == Catalogue::Builders { "BUILDER" } else { "NORMAL" }.into()),
    ];
    match session.execute(&[(sql, params)]).await {
        Ok((1, next)) if next as i64 > count as i64 => Some(next as i64 - count as i64),
        _ => None,
    }
}

// ---- The history as the Manager shows it, and its undo ---------------------------------------------------------

/// One operation of the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    pub id: i64,
    pub revision: i64,
    pub actor: String,
    pub summary: String,
    pub source: String,
    pub created_at: String,
    pub entries: Vec<Change>,
}

/// One page or offer an operation changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub entity: Entity,
    pub operation: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

impl Change {
    /// The fields that differ.
    pub fn fields(&self) -> Vec<String> {
        let empty = Map::new();
        let a = self.before.as_ref().and_then(Value::as_object).unwrap_or(&empty);
        let b = self.after.as_ref().and_then(Value::as_object).unwrap_or(&empty);
        let mut names: Vec<String> = a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect();
        names.sort();
        names.dedup();
        names
    }

    /// Its name: the caption of a page, the name of an offer.
    pub fn name(&self) -> String {
        let field = if self.entity.page { "caption" } else { "catalogName" };
        self.after.as_ref().or(self.before.as_ref()).and_then(|v| v.get(field)).and_then(Value::as_str).unwrap_or("").to_string()
    }
}

/// Reads one history row's changes (`changes_json`).
pub fn parse_changes(json: &str) -> Vec<Change> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    list.iter()
        .filter_map(|entry| {
            let builders = entry.get("catalogType").and_then(Value::as_str) == Some("BUILDER");
            let page = entry.get("entityType").and_then(Value::as_str) == Some("PAGE");
            let id = entry.get("entityId").and_then(Value::as_i64)?;
            let snapshot = |key: &str| entry.get(key).and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok());
            Some(Change {
                entity: Entity { builders, page, id },
                operation: entry.get("operation").and_then(Value::as_str).unwrap_or("").to_string(),
                before: snapshot("beforeJson"),
                after: snapshot("afterJson"),
            })
        })
        .collect()
}

/// The latest operations of the history, newest first.
pub async fn load_history(session: &SqlSession, database: &str, limit: usize) -> Result<Vec<Operation>> {
    let db = quote(database);
    let rows = session
        .select(
            &format!(
                "SELECT h.id, h.revision, COALESCE(u.username, ''), h.summary, h.source, CAST(h.created_at AS CHAR), h.changes_json \
                 FROM {db}.catalog_manager_history h LEFT JOIN {db}.users u ON u.id = h.actor_id ORDER BY h.revision DESC, h.id DESC LIMIT ?"
            ),
            vec![SqlValue::Int(limit as i64)],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| Operation {
            id: int(r.first()),
            revision: int(r.get(1)),
            actor: text(r.get(2)).unwrap_or_default(),
            summary: text(r.get(3)).unwrap_or_default(),
            source: text(r.get(4)).unwrap_or_default(),
            created_at: text(r.get(5)).unwrap_or_default(),
            entries: parse_changes(&text(r.get(6)).unwrap_or_default()),
        })
        .collect())
}

/// Equal snapshot values; a missing value, null and "" count as the same (TEXT columns read back as NULL or "").
fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    let blank = |v: Option<&Value>| match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(other) => Some(other.clone()),
    };
    match (blank(a), blank(b)) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (x, y) => x == y,
    }
}

/// The row still holds what the operation left it with (as the emulator's undo checks).
pub fn still_as_left(after: Option<&Value>, now: Option<&Value>) -> bool {
    match (after, now) {
        (None, None) => true,
        (Some(a), Some(n)) => a.as_object().is_some_and(|a| a.iter().all(|(k, v)| same(Some(v), n.get(k)))),
        _ => false,
    }
}

fn flag(value: &Value, key: &str) -> SqlValue {
    SqlValue::Text(if value.get(key).and_then(Value::as_bool).unwrap_or(false) { "1" } else { "0" }.into())
}

fn number(value: &Value, key: &str) -> SqlValue {
    SqlValue::Int(value.get(key).and_then(Value::as_i64).unwrap_or(0))
}

fn string(value: &Value, key: &str) -> SqlValue {
    match value.get(key).and_then(Value::as_str) {
        Some(s) => SqlValue::Text(s.to_string()),
        None => SqlValue::Null,
    }
}

/// The statement that puts an entity back to `state` (None: deleted), as the emulator's live writer does it.
pub fn write_statement(database: &str, entity: Entity, state: Option<&Value>) -> (String, Vec<SqlValue>) {
    let db = quote(database);
    let cat = entity.cat();
    let table = if entity.page { cat.pages_table() } else { cat.items_table() };
    let Some(v) = state else {
        return (format!("DELETE FROM {db}.{table} WHERE id = ?"), vec![SqlValue::Int(entity.id)]);
    };
    let upsert = |columns: &[&str]| {
        let marks = vec!["?"; columns.len()].join(",");
        let update: Vec<String> = columns.iter().skip(1).map(|c| format!("{c}=VALUES({c})")).collect();
        format!("INSERT INTO {db}.{table} ({}) VALUES ({marks}) ON DUPLICATE KEY UPDATE {}", columns.join(","), update.join(","))
    };
    let id = SqlValue::Int(entity.id);
    match (entity.page, entity.builders) {
        (true, false) => (
            upsert(&[
                "id", "parent_id", "caption_save", "caption", "page_layout", "icon_color", "icon_image", "min_rank", "order_num", "visible", "enabled", "club_only", "catalog_mode",
                "vip_only", "page_headline", "page_teaser", "page_special", "page_text1", "page_text2", "page_text_details", "page_text_teaser", "room_id", "includes",
            ]),
            vec![
                id,
                number(v, "parentId"),
                string(v, "captionSave"),
                string(v, "caption"),
                string(v, "pageLayout"),
                number(v, "iconColor"),
                number(v, "iconImage"),
                number(v, "minRank"),
                number(v, "orderNum"),
                flag(v, "visible"),
                flag(v, "enabled"),
                flag(v, "clubOnly"),
                string(v, "catalogMode"),
                flag(v, "vipOnly"),
                string(v, "pageHeadline"),
                string(v, "pageTeaser"),
                string(v, "pageSpecial"),
                string(v, "pageText1"),
                string(v, "pageText2"),
                string(v, "pageTextDetails"),
                string(v, "pageTextTeaser"),
                number(v, "roomId"),
                string(v, "includes"),
            ],
        ),
        (true, true) => (
            upsert(&[
                "id", "parent_id", "caption", "page_layout", "icon_color", "icon_image", "order_num", "visible", "enabled", "page_headline", "page_teaser", "page_special",
                "page_text1", "page_text2", "page_text_details", "page_text_teaser",
            ]),
            vec![
                id,
                number(v, "parentId"),
                string(v, "caption"),
                string(v, "pageLayout"),
                number(v, "iconColor"),
                number(v, "iconImage"),
                number(v, "orderNum"),
                flag(v, "visible"),
                flag(v, "enabled"),
                string(v, "pageHeadline"),
                string(v, "pageTeaser"),
                string(v, "pageSpecial"),
                string(v, "pageText1"),
                string(v, "pageText2"),
                string(v, "pageTextDetails"),
                string(v, "pageTextTeaser"),
            ],
        ),
        (false, false) => (
            upsert(&[
                "id", "item_ids", "page_id", "catalog_name", "cost_credits", "cost_points", "points_type", "amount", "limited_stack", "order_number", "offer_id", "song_id",
                "extradata", "have_offer", "club_only",
            ]),
            vec![
                id,
                string(v, "itemIds"),
                number(v, "pageId"),
                string(v, "catalogName"),
                number(v, "costCredits"),
                number(v, "costPoints"),
                number(v, "pointsType"),
                number(v, "amount"),
                number(v, "limitedStack"),
                number(v, "orderNumber"),
                number(v, "offerIdClient"),
                number(v, "songId"),
                string(v, "extradata"),
                flag(v, "haveOffer"),
                flag(v, "clubOnly"),
            ],
        ),
        (false, true) => (
            upsert(&["id", "item_ids", "page_id", "catalog_name", "order_number", "extradata"]),
            vec![id, string(v, "itemIds"), number(v, "pageId"), string(v, "catalogName"), number(v, "orderNumber"), string(v, "extradata")],
        ),
    }
}

/// Why the operation cannot be undone, or None: a page or offer it changed was changed again since (by anyone, in
/// the history or not).
pub async fn undo_problem(session: &SqlSession, database: &str, operation: &Operation) -> Result<Option<String>> {
    let mut last: BTreeMap<Entity, Option<Value>> = BTreeMap::new();
    for change in &operation.entries {
        last.insert(change.entity, change.after.clone());
    }
    let entities: Vec<Entity> = last.keys().copied().collect();
    let Some(now) = capture(session, database, &entities).await else {
        return Ok(Some("The catalogue's history cannot be read.".into()));
    };
    for (entity, after) in &last {
        if !still_as_left(after.as_ref(), now.get(entity).and_then(Option::as_ref)) {
            let kind = if entity.page { "Page" } else { "Offer" };
            return Ok(Some(format!("{kind} #{} was changed after this operation: undo that first, or change it back by hand.", entity.id)));
        }
    }
    Ok(None)
}

/// Undoes the operation: every page and offer back as it was before, in one transaction, recorded in the history as
/// an undo. Check [`undo_problem`] first.
pub async fn undo(session: &SqlSession, database: &str, operation: &Operation) -> Result<()> {
    let mut statements: Vec<(String, Vec<SqlValue>)> = Vec::new();
    let mut inverse = Vec::new();
    for change in operation.entries.iter().rev() {
        statements.push(write_statement(database, change.entity, change.before.as_ref()));
        let mut after = BTreeMap::new();
        after.insert(change.entity, change.before.clone());
        let mut before = BTreeMap::new();
        before.insert(change.entity, change.after.clone());
        inverse.extend(entries(&before, &after));
    }
    let db = quote(database);
    let summary: String = format!("Undo operation #{}", operation.id).chars().take(255).collect();
    statements.push((format!("UPDATE {db}.catalog_manager_state SET revision = revision + 1 WHERE singleton_id = 1"), Vec::new()));
    statements.push((
        format!(
            "INSERT INTO {db}.catalog_manager_history (revision, actor_id, summary, source, changes_json) \
             SELECT revision, 0, ?, 'UNDO', ? FROM {db}.catalog_manager_state WHERE singleton_id = 1"
        ),
        vec![SqlValue::Text(summary), SqlValue::Text(Value::Array(inverse).to_string())],
    ));
    session.execute(&statements).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_row(caption: &str, order: i64) -> Vec<SqlValue> {
        let mut row = vec![SqlValue::Int(12), SqlValue::Int(-1), SqlValue::Text("rares".into()), SqlValue::Text(caption.into()), SqlValue::Text("default_3x3".into())];
        row.extend([1, 1, 1, order].map(SqlValue::Int));
        row.extend(["1", "1", "0"].map(|f| SqlValue::Text(f.into())));
        row.push(SqlValue::Text("NORMAL".into()));
        row.push(SqlValue::Text("0".into()));
        row.extend(["", "", ""].map(|s| SqlValue::Text(s.into())));
        row.extend([SqlValue::Null, SqlValue::Null, SqlValue::Null, SqlValue::Null]);
        row.push(SqlValue::Int(0));
        row.push(SqlValue::Text(String::new()));
        row
    }

    #[test]
    fn a_row_becomes_the_emulators_snapshot() {
        let entity = Entity::page(Catalogue::Normal, 12);
        let snapshot = snapshot_json(entity, &page_row("Rares", 3));
        assert_eq!(snapshot["catalogType"], "NORMAL");
        assert_eq!(snapshot["pageId"], 12);
        assert_eq!(snapshot["caption"], "Rares");
        assert_eq!(snapshot["visible"], true);
        assert_eq!(snapshot["clubOnly"], false);
        assert_eq!(snapshot["orderNum"], 3);
        assert!(snapshot.get("pageText1").is_none(), "NULL is left out, as Gson does");
    }

    #[test]
    fn changes_become_history_entries() {
        let entity = Entity::page(Catalogue::Normal, 12);
        let before = BTreeMap::from([(entity, Some(snapshot_json(entity, &page_row("Rares", 3))))]);
        let moved = BTreeMap::from([(entity, Some(snapshot_json(entity, &page_row("Rares", 5))))]);
        let renamed = BTreeMap::from([(entity, Some(snapshot_json(entity, &page_row("Rare furni", 3))))]);
        let gone = BTreeMap::from([(entity, None)]);
        assert_eq!(entries(&before, &moved)[0]["operation"], "MOVE");
        assert_eq!(entries(&before, &renamed)[0]["operation"], "UPDATE");
        assert_eq!(entries(&before, &gone)[0]["operation"], "DELETE");
        assert_eq!(entries(&gone, &before)[0]["operation"], "CREATE");
        assert!(entries(&before, &before).is_empty());
        let entry = &entries(&before, &renamed)[0];
        assert_eq!(entry["entityType"], "PAGE");
        assert_eq!(entry["entityId"], 12);
        // The snapshots are JSON text inside the entry, as the emulator stores them.
        let after: Value = serde_json::from_str(entry["afterJson"].as_str().unwrap()).unwrap();
        assert_eq!(after["caption"], "Rare furni");
        let parsed = parse_changes(&Value::Array(entries(&before, &renamed)).to_string());
        assert_eq!(parsed[0].entity, entity);
        assert_eq!(parsed[0].fields(), vec!["caption".to_string()]);
        assert_eq!(parsed[0].name(), "Rare furni");
    }

    #[test]
    fn undo_only_while_the_rows_hold_what_the_operation_left() {
        let a = json!({"caption": "Rares", "pageText1": "", "orderNum": 3});
        assert!(still_as_left(Some(&a), Some(&json!({"caption": "Rares", "orderNum": 3.0, "minRank": 1}))));
        assert!(!still_as_left(Some(&a), Some(&json!({"caption": "Edited", "orderNum": 3}))));
        assert!(still_as_left(None, None));
        assert!(!still_as_left(None, Some(&a)));
    }

    #[test]
    fn undo_writes_the_rows_back_as_the_emulator_does() {
        let entity = Entity::offer(Catalogue::Builders, 7);
        let (sql, params) = write_statement("hotel", entity, Some(&json!({"itemIds": "5", "pageId": 2, "catalogName": "chair", "orderNumber": 1, "extradata": ""})));
        assert!(sql.starts_with("INSERT INTO `hotel`.catalog_items_bc (id,item_ids,page_id,catalog_name,order_number,extradata)"));
        assert_eq!(params.len(), sql.matches('?').count());
        let (sql, _) = write_statement("hotel", Entity::page(Catalogue::Normal, 3), None);
        assert_eq!(sql, "DELETE FROM `hotel`.catalog_pages WHERE id = ?");
        let (sql, params) = write_statement("hotel", Entity::page(Catalogue::Normal, 3), Some(&json!({"caption": "x"})));
        assert_eq!(params.len(), sql.matches('?').count());
    }

    #[test]
    fn history_rows_name_the_manager() {
        let statements = history_statements("hotel", "Moved 2 offers", &[json!({"x": 1})]);
        assert_eq!(statements.len(), 2);
        assert!(statements[1].0.contains("catalog_manager_history"));
        assert_eq!(statements[1].1[0], SqlValue::Text("All-in-1 Manager: Moved 2 offers".into()));
    }
}
