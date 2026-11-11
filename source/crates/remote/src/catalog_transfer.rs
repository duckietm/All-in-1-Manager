//! The whole catalogue (pages and offers of the catalogue and the Builders Club warehouse) as one SQL file, in the
//! format of Octane's Catalog Studio (`CatalogSqlExportService`), so a file moves between Octane and the Manager. The
//! import reads only that restricted SQL, never runs it: INSERT, UPDATE and DELETE of `catalog_pages` and
//! `catalog_items` (a `catalog_type` column says which catalogue), with integer, string and NULL values. A preview
//! shows what would change; applying writes those changes in one transaction and records them in the shared history.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::catalog_journal::{self as journal, Change, Entity};
use crate::server::{Result, ServerError};
use crate::sql_session::{quote, SqlSession, SqlValue};

const PAGE_COLUMNS: &[&str] = &[
    "id",
    "catalog_type",
    "parent_id",
    "caption_save",
    "caption",
    "page_layout",
    "icon_color",
    "icon_image",
    "min_rank",
    "order_num",
    "visible",
    "enabled",
    "club_only",
    "catalog_mode",
    "vip_only",
    "page_headline",
    "page_teaser",
    "page_special",
    "page_text1",
    "page_text2",
    "page_text_details",
    "page_text_teaser",
    "room_id",
    "includes",
];

const OFFER_COLUMNS: &[&str] = &[
    "id",
    "catalog_type",
    "item_ids",
    "page_id",
    "catalog_name",
    "cost_credits",
    "cost_points",
    "points_type",
    "amount",
    "limited_stack",
    "order_number",
    "offer_id",
    "song_id",
    "extradata",
    "have_offer",
    "club_only",
];

const PAGE_NUMBERS: &[&str] = &["id", "parent_id", "icon_color", "icon_image", "min_rank", "order_num", "room_id"];
const OFFER_NUMBERS: &[&str] = &["id", "page_id", "cost_credits", "cost_points", "points_type", "amount", "limited_stack", "order_number", "offer_id", "song_id"];
const PAGE_FLAGS: &[&str] = &["visible", "enabled", "club_only", "vip_only"];
const OFFER_FLAGS: &[&str] = &["have_offer", "club_only"];

/// The snapshot property of a column (`page_text1` → `pageText1`, `id` → `pageId`, an offer's `offer_id` →
/// `offerIdClient`).
fn property(column: &str, page: bool) -> String {
    match column {
        "id" => return if page { "pageId" } else { "offerId" }.into(),
        "catalog_type" => return "catalogType".into(),
        "offer_id" if !page => return "offerIdClient".into(),
        _ => {}
    }
    let mut out = String::new();
    let mut upper = false;
    for c in column.chars() {
        if c == '_' {
            upper = true;
        } else {
            out.push(if upper { c.to_ascii_uppercase() } else { c });
            upper = false;
        }
    }
    out
}

fn literal(value: Option<&Value>, column: &str, page: bool) -> String {
    let flags = if page { PAGE_FLAGS } else { OFFER_FLAGS };
    match value {
        None | Some(Value::Null) => "NULL".into(),
        Some(Value::Bool(b)) => if *b { "1" } else { "0" }.into(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) if flags.contains(&column) => s.clone(),
        Some(Value::String(s)) => format!("'{}'", s.replace('\'', "''")),
        Some(other) => format!("'{}'", other.to_string().replace('\'', "''")),
    }
}

/// The whole catalogue as Octane's Catalog Studio exports it.
pub fn export_text(all: &BTreeMap<Entity, Value>) -> String {
    let mut sql = String::from("-- Catalog Studio restricted SQL export.\nDELETE FROM catalog_items;\nDELETE FROM catalog_pages;\n");
    for page in [true, false] {
        let columns = if page { PAGE_COLUMNS } else { OFFER_COLUMNS };
        let table = if page { "catalog_pages" } else { "catalog_items" };
        let mut entities: Vec<&Entity> = all.keys().filter(|e| e.page == page).collect();
        entities.sort_by_key(|e| (e.builders, e.id));
        for entity in entities {
            let value = &all[entity];
            let values: Vec<String> = columns.iter().map(|c| literal(value.get(property(c, page)), c, page)).collect();
            sql.push_str(&format!("INSERT INTO {table} ({}) VALUES ({});\n", columns.join(", "), values.join(", ")));
        }
    }
    sql
}

/// Splits the file into statements, without comments, as the emulator's tokenizer does.
pub fn statements(sql: &str) -> std::result::Result<Vec<String>, String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut list = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut line_comment, mut block_comment) = (false, false, false);
    let mut i = 0;
    let push = |list: &mut Vec<String>, current: &mut String| {
        let statement = current.trim().to_string();
        current.clear();
        if !statement.is_empty() {
            list.push(statement);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if line_comment {
            if c == '\n' || c == '\r' {
                line_comment = false;
                current.push(' ');
            }
        } else if block_comment {
            if c == '*' && next == '/' {
                block_comment = false;
                i += 1;
                current.push(' ');
            }
        } else if quoted {
            current.push(c);
            // A backslash escape or a doubled quote: both characters stay inside the string.
            if (c == '\\' && next != '\0') || (c == '\'' && next == '\'') {
                current.push(next);
                i += 1;
            } else if c == '\'' {
                quoted = false;
            }
        } else if c == '\'' {
            quoted = true;
            current.push(c);
        } else if c == '-' && next == '-' {
            line_comment = true;
            i += 1;
        } else if c == '/' && next == '*' && chars.get(i + 2) == Some(&'!') {
            return Err("Executable SQL comments are forbidden.".into());
        } else if c == '/' && next == '*' {
            block_comment = true;
            i += 1;
        } else if c == ';' {
            push(&mut list, &mut current);
        } else {
            current.push(c);
        }
        i += 1;
    }
    if quoted || block_comment {
        return Err("Unterminated SQL string or comment.".into());
    }
    push(&mut list, &mut current);
    Ok(list)
}

/// One statement of the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Insert { page: bool, values: BTreeMap<String, Option<String>> },
    Update { page: bool, id: i64, values: BTreeMap<String, Option<String>> },
    Delete { page: bool, id: i64, builders: bool },
    DeleteAll { page: bool },
}

fn outside_literals(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::new();
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if quoted {
            out.push(' ');
            if (c == '\\' && next != '\0') || (c == '\'' && next == '\'') {
                out.push(' ');
                i += 1;
            } else if c == '\'' {
                quoted = false;
            }
        } else {
            out.push(if c == '\'' { ' ' } else { c });
            if c == '\'' {
                quoted = true;
            }
        }
        i += 1;
    }
    out
}

fn split_comma(text: &str) -> std::result::Result<Vec<String>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if quoted {
            current.push(c);
            if c == '\'' && next == '\'' {
                current.push(next);
                i += 1;
            } else if c == '\'' {
                quoted = false;
            }
        } else if c == '\'' {
            quoted = true;
            current.push(c);
        } else if c == ',' {
            values.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
        i += 1;
    }
    if quoted {
        return Err("Unterminated SQL literal.".into());
    }
    values.push(current.trim().to_string());
    Ok(values)
}

fn identifier(raw: &str) -> std::result::Result<String, String> {
    let value = raw.trim().replace('`', "").to_lowercase();
    let ok = value.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(value)
    } else {
        Err(format!("Invalid SQL name: {raw}"))
    }
}

fn parse_literal(raw: &str) -> std::result::Result<Option<String>, String> {
    let value = raw.trim();
    if value.eq_ignore_ascii_case("NULL") {
        return Ok(None);
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Some(value.to_string()));
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Ok(Some(value[1..value.len() - 1].replace("''", "'")));
    }
    Err(format!("Only whole numbers, 'text' and NULL are allowed as values, not {value}."))
}

fn check_column(page: bool, column: &str) -> std::result::Result<(), String> {
    let allowed = if page { PAGE_COLUMNS } else { OFFER_COLUMNS };
    if allowed.contains(&column) {
        Ok(())
    } else {
        Err(format!("Column {column} is not allowed."))
    }
}

/// One statement, as the emulator's import reads it.
pub fn parse_statement(sql: &str) -> std::result::Result<Statement, String> {
    let upper = outside_literals(sql).to_uppercase();
    for token in ["SELECT", " JOIN ", "CALL ", "PROCEDURE", "FUNCTION", "INTO OUTFILE", "LOAD_FILE", "BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT", "SET @", "@@", "--", "/*", "*/"] {
        if upper.contains(token) {
            return Err(format!("Forbidden SQL: {}", token.trim()));
        }
    }
    let insert = Regex::new(r"(?is)^INSERT\s+INTO\s+(catalog_pages|catalog_items)\s*\(([^)]*)\)\s*VALUES\s*\((.*)\)$").unwrap();
    let update = Regex::new(r"(?is)^UPDATE\s+(catalog_pages|catalog_items)\s+SET\s+(.+)\s+WHERE\s+id\s*=\s*(\d+)(?:\s+AND\s+catalog_type\s*=\s*('[^']*'|[A-Za-z_]+))?\s*$").unwrap();
    let delete = Regex::new(r"(?is)^DELETE\s+FROM\s+(catalog_pages|catalog_items)\s+WHERE\s+id\s*=\s*(\d+)(?:\s+AND\s+catalog_type\s*=\s*('[^']*'|[A-Za-z_]+))?\s*$").unwrap();
    let delete_all = Regex::new(r"(?is)^DELETE\s+FROM\s+(catalog_pages|catalog_items)\s*$").unwrap();
    if let Some(m) = insert.captures(sql) {
        let page = m[1].eq_ignore_ascii_case("catalog_pages");
        let columns = split_comma(&m[2])?;
        let values = split_comma(&m[3])?;
        if columns.len() != values.len() {
            return Err("INSERT has a different number of columns and values.".into());
        }
        let mut mapped = BTreeMap::new();
        for (column, value) in columns.iter().zip(&values) {
            let column = identifier(column)?;
            check_column(page, &column)?;
            if mapped.insert(column.clone(), parse_literal(value)?).is_some() {
                return Err(format!("Column {column} is there twice."));
            }
        }
        let id_ok = mapped.get("id").cloned().flatten().and_then(|id| id.parse::<i64>().ok()).is_some_and(|id| id > 0);
        if !id_ok {
            return Err("An INSERT needs one positive id.".into());
        }
        return Ok(Statement::Insert { page, values: mapped });
    }
    if let Some(m) = update.captures(sql) {
        let page = m[1].eq_ignore_ascii_case("catalog_pages");
        let mut mapped = BTreeMap::new();
        for assignment in split_comma(&m[2])? {
            let mut quoted = false;
            let equals = assignment.char_indices().find(|(_, c)| {
                if *c == '\'' {
                    quoted = !quoted;
                }
                *c == '=' && !quoted
            });
            let Some((at, _)) = equals.filter(|(at, _)| *at > 0) else { return Err("Invalid SQL assignment.".into()) };
            let column = identifier(&assignment[..at])?;
            check_column(page, &column)?;
            if column == "id" {
                return Err("Ids cannot be changed.".into());
            }
            if mapped.insert(column.clone(), parse_literal(&assignment[at + 1..])?).is_some() {
                return Err(format!("Column {column} is there twice."));
            }
        }
        if let Some(kind) = m.get(4) {
            mapped.insert("catalog_type".into(), parse_literal(kind.as_str()).ok().flatten().or_else(|| Some(kind.as_str().to_string())));
        }
        return Ok(Statement::Update { page, id: m[3].parse().unwrap_or(0), values: mapped });
    }
    if let Some(m) = delete.captures(sql) {
        let page = m[1].eq_ignore_ascii_case("catalog_pages");
        let builders = m.get(3).is_some_and(|k| is_builders(Some(k.as_str().trim_matches('\''))));
        return Ok(Statement::Delete { page, id: m[2].parse().unwrap_or(0), builders });
    }
    if let Some(m) = delete_all.captures(sql) {
        return Ok(Statement::DeleteAll { page: m[1].eq_ignore_ascii_case("catalog_pages") });
    }
    Err("Only INSERT, UPDATE and DELETE of catalog_pages and catalog_items are allowed.".into())
}

/// `catalog_type` as the emulator reads it (BUILDER, BUILDERS_CLUB or BC: the warehouse).
fn is_builders(value: Option<&str>) -> bool {
    matches!(value.map(|v| v.trim().to_uppercase()).as_deref(), Some("BUILDER" | "BUILDERS_CLUB" | "BC"))
}

fn apply_values(object: &mut Map<String, Value>, values: &BTreeMap<String, Option<String>>, page: bool) -> std::result::Result<(), String> {
    let numbers = if page { PAGE_NUMBERS } else { OFFER_NUMBERS };
    let flags = if page { PAGE_FLAGS } else { OFFER_FLAGS };
    for (column, value) in values {
        let key = property(column, page);
        let json = match value {
            // A NULL text is left out of a snapshot, as Gson leaves it out.
            None => {
                object.remove(&key);
                continue;
            }
            Some(v) if numbers.contains(&column.as_str()) => json!(v.parse::<i64>().map_err(|_| format!("{column} must be a whole number, not {v}."))?),
            Some(v) if flags.contains(&column.as_str()) => json!(v != "0"),
            Some(v) => json!(v),
        };
        object.insert(key, json);
    }
    Ok(())
}

/// The catalogue the file makes of `current`, as the emulator's dry run builds it.
pub fn target(current: &BTreeMap<Entity, Value>, sql: &str) -> std::result::Result<BTreeMap<Entity, Value>, String> {
    let parsed: Vec<Statement> = statements(sql)?.iter().map(|s| parse_statement(s)).collect::<std::result::Result<_, _>>()?;
    if parsed.is_empty() {
        return Err("The file holds no statements.".into());
    }
    let mut result = current.clone();
    for statement in parsed {
        match statement {
            Statement::DeleteAll { page } => result.retain(|e, _| e.page != page),
            Statement::Delete { page, id, builders } => {
                if result.remove(&Entity { builders, page, id }).is_none() {
                    return Err(format!("{} {id} is not there to delete.", if page { "Page" } else { "Offer" }));
                }
            }
            Statement::Insert { page, values } => {
                let builders = is_builders(values.get("catalog_type").cloned().flatten().as_deref());
                let id = values.get("id").cloned().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
                let entity = Entity { builders, page, id };
                if result.contains_key(&entity) {
                    return Err(format!("{} {id} is already there.", if page { "Page" } else { "Offer" }));
                }
                let mut object = Map::new();
                apply_values(&mut object, &values, page)?;
                object.insert("catalogType".into(), json!(if builders { "BUILDER" } else { "NORMAL" }));
                result.insert(entity, Value::Object(object));
            }
            Statement::Update { page, id, values } => {
                let builders = is_builders(values.get("catalog_type").cloned().flatten().as_deref());
                let entity = Entity { builders, page, id };
                let Some(Value::Object(object)) = result.get_mut(&entity) else {
                    return Err(format!("{} {id} is not there to change.", if page { "Page" } else { "Offer" }));
                };
                apply_values(object, &values, page)?;
                object.insert("catalogType".into(), json!(if builders { "BUILDER" } else { "NORMAL" }));
            }
        }
    }
    // Every offer is on a page of its catalogue.
    for (entity, value) in &result {
        if !entity.page {
            let page = value.get("pageId").and_then(Value::as_i64).unwrap_or(0);
            if !result.contains_key(&Entity { builders: entity.builders, page: true, id: page }) {
                return Err(format!("Offer {} is on page {page}, which does not exist.", entity.id));
            }
        }
    }
    Ok(result)
}

/// What a file would change: the pages and offers created, changed and deleted.
pub fn changes(current: &BTreeMap<Entity, Value>, target: &BTreeMap<Entity, Value>) -> Vec<Change> {
    let keys: Vec<Entity> = current.keys().chain(target.keys()).copied().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let before: BTreeMap<Entity, Option<Value>> = keys.iter().map(|k| (*k, current.get(k).cloned())).collect();
    let after: BTreeMap<Entity, Option<Value>> = keys.iter().map(|k| (*k, target.get(k).cloned())).collect();
    journal::parse_changes(&Value::Array(journal::entries(&before, &after)).to_string())
}

/// The whole catalogue as a file.
pub async fn export(session: &SqlSession, database: &str) -> Result<String> {
    Ok(export_text(&journal::capture_all(session, database).await?))
}

/// The preview of a file: the catalogue as it is, and what the file changes.
pub async fn preview(session: &SqlSession, database: &str, sql: &str) -> Result<(BTreeMap<Entity, Value>, Vec<Change>)> {
    let current = journal::capture_all(session, database).await?;
    let target = target(&current, sql).map_err(ServerError)?;
    let changes = changes(&current, &target);
    Ok((current, changes))
}

/// Writes the previewed changes in one transaction, when the catalogue is still as it was at the preview; recorded in
/// the shared history as an SQL import.
pub async fn apply(session: &SqlSession, database: &str, previewed: &BTreeMap<Entity, Value>, changes: &[Change]) -> Result<()> {
    if journal::capture_all(session, database).await? != *previewed {
        return Err(ServerError("The catalogue changed since the preview: preview the file again.".into()).into());
    }
    let mut ordered: Vec<&Change> = changes.iter().collect();
    // Pages before their offers when added; offers before their pages when deleted.
    ordered.sort_by_key(|c| match (c.after.is_some(), c.entity.page) {
        (true, true) => 0,
        (true, false) => 1,
        (false, false) => 2,
        (false, true) => 3,
    });
    let mut statements: Vec<(String, Vec<SqlValue>)> = ordered.iter().map(|c| journal::write_statement(database, c.entity, c.after.as_ref())).collect();
    if journal::has_history(session, database).await {
        let db = quote(database);
        for (kind, cat, table) in [("PAGE", "NORMAL", "catalog_pages"), ("OFFER", "NORMAL", "catalog_items"), ("PAGE", "BUILDER", "catalog_pages_bc"), ("OFFER", "BUILDER", "catalog_items_bc")] {
            statements.push((
                format!("UPDATE {db}.catalog_id_sequences SET next_id = GREATEST(next_id, (SELECT COALESCE(MAX(id), 0) + 1 FROM {db}.{table})) WHERE entity_type = ? AND catalog_type = ?"),
                vec![SqlValue::Text(kind.into()), SqlValue::Text(cat.into())],
            ));
        }
        let before: BTreeMap<Entity, Option<Value>> = changes.iter().map(|c| (c.entity, c.before.clone())).collect();
        let after: BTreeMap<Entity, Option<Value>> = changes.iter().map(|c| (c.entity, c.after.clone())).collect();
        let entries = journal::entries(&before, &after);
        let summary = format!("All-in-1 Manager: SQL import, {} changes", changes.len());
        statements.push((format!("UPDATE {db}.catalog_manager_state SET revision = revision + 1 WHERE singleton_id = 1"), Vec::new()));
        statements.push((
            format!(
                "INSERT INTO {db}.catalog_manager_history (revision, actor_id, summary, source, changes_json) \
                 SELECT revision, 0, ?, 'SQL', ? FROM {db}.catalog_manager_state WHERE singleton_id = 1"
            ),
            vec![SqlValue::Text(summary), SqlValue::Text(Value::Array(entries).to_string())],
        ));
    }
    session.execute(&statements).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue() -> BTreeMap<Entity, Value> {
        let mut all = BTreeMap::new();
        all.insert(
            Entity { builders: false, page: true, id: 1 },
            json!({"catalogType": "NORMAL", "pageId": 1, "parentId": -1, "captionSave": "shop", "caption": "Shop's", "pageLayout": "default_3x3",
                   "iconColor": 1, "iconImage": 1, "minRank": 1, "orderNum": 1, "visible": true, "enabled": true, "clubOnly": false,
                   "catalogMode": "NORMAL", "vipOnly": false, "pageHeadline": "", "pageTeaser": "", "pageSpecial": "", "roomId": 0, "includes": ""}),
        );
        all.insert(
            Entity { builders: false, page: false, id: 5 },
            json!({"catalogType": "NORMAL", "offerId": 5, "itemIds": "10", "pageId": 1, "catalogName": "chair", "costCredits": 3, "costPoints": 0,
                   "pointsType": 0, "amount": 1, "limitedStack": 0, "orderNumber": 1, "offerIdClient": -1, "songId": 0, "extradata": "",
                   "haveOffer": true, "clubOnly": false}),
        );
        all.insert(
            Entity { builders: true, page: true, id: 1 },
            json!({"catalogType": "BUILDER", "pageId": 1, "parentId": -1, "captionSave": "", "caption": "Blocks", "pageLayout": "default_3x3",
                   "iconColor": 1, "iconImage": 1, "minRank": 1, "orderNum": 1, "visible": true, "enabled": true, "clubOnly": false,
                   "catalogMode": "BUILDER", "vipOnly": false, "roomId": 0, "includes": ""}),
        );
        all
    }

    #[test]
    fn the_export_reads_back_as_the_same_catalogue() {
        let all = catalogue();
        let text = export_text(&all);
        assert!(text.starts_with("-- Catalog Studio restricted SQL export.\nDELETE FROM catalog_items;\nDELETE FROM catalog_pages;\n"));
        assert!(text.contains("'Shop''s'"), "quotes doubled");
        assert!(text.contains("VALUES (1, 'BUILDER', -1, '', 'Blocks'"));
        let back = target(&BTreeMap::new(), &text).unwrap();
        assert!(changes(&all, &back).is_empty(), "{:?}", changes(&all, &back));
    }

    #[test]
    fn a_file_changes_only_what_it_says() {
        let all = catalogue();
        let file = "UPDATE catalog_items SET cost_credits = 10, catalog_name = 'chair; red' WHERE id = 5;\n\
                    INSERT INTO catalog_items (id, catalog_type, item_ids, page_id, catalog_name) VALUES (6, 'BUILDER', '11', 1, 'block');\n\
                    DELETE FROM catalog_pages WHERE id = 1 AND catalog_type = 'NORMAL'; INSERT INTO catalog_pages (id, caption) VALUES (1, 'Shop')";
        let found = changes(&all, &target(&all, file).unwrap());
        let ops: Vec<(Entity, String)> = found.iter().map(|c| (c.entity, c.operation.clone())).collect();
        assert!(ops.contains(&(Entity { builders: false, page: false, id: 5 }, "UPDATE".into())));
        assert!(ops.contains(&(Entity { builders: true, page: false, id: 6 }, "CREATE".into())));
        assert_eq!(found.iter().find(|c| c.entity.id == 5).unwrap().fields(), vec!["catalogName".to_string(), "costCredits".to_string()]);
    }

    #[test]
    fn anything_else_is_refused() {
        let all = catalogue();
        for bad in [
            "DROP TABLE users",
            "UPDATE users SET rank = 7 WHERE id = 1",
            "INSERT INTO catalog_items (id, page_id) VALUES ((SELECT 1), 1)",
            "INSERT INTO catalog_items (id, rank) VALUES (9, 1)",
            "UPDATE catalog_items SET id = 9 WHERE id = 5",
            "/*!50000 DROP TABLE x */",
            "INSERT INTO catalog_items (id, page_id) VALUES (9, 77)",
            "DELETE FROM catalog_items WHERE id = 404",
            "INSERT INTO catalog_items (id, page_id) VALUES (5, 1)",
            "UPDATE catalog_items SET catalog_name = 'x' WHERE id = 5; SELECT 1",
        ] {
            assert!(target(&all, bad).is_err(), "{bad} was accepted");
        }
        assert_eq!(statements("SELECT 'a;b'; -- c\nx").unwrap(), vec!["SELECT 'a;b'".to_string(), "x".to_string()]);
    }
}
