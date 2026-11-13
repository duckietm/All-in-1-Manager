//! The only way a plugin reaches the hotel database: structured queries checked against its
//! permissions and turned into parameterised SQL.
//!
//! Rules (a plugin is untrusted; every refusal is an `Err("permission denied: ...")`):
//!
//! * identifiers (tables, columns) must match `^[A-Za-z_][A-Za-z0-9_]{0,63}$`; they are then quoted with
//!   backticks. No `*`, no expressions, no `db.table`;
//! * values are never part of the text: every value (`like` patterns included) is a `?` parameter. The
//!   only number written into the text is the host-computed `LIMIT`;
//! * a select needs `sql:read:<table>`, a write `sql:write:<table>`, with the table written exactly as
//!   declared (MariaDB table names can be case sensitive, so `ROOMS` is not an alias of `rooms`);
//! * private columns ([`is_private`]) need `sql:read-private:<table>.<column>` to be selected, compared
//!   in a condition (a comparison would let a plugin probe a hash) or written (setting a password or an
//!   SSO ticket takes over an account);
//! * a select returns at most [`MAX_LIMIT`] rows ([`DEFAULT_LIMIT`] when the plugin gives none);
//! * an update or delete needs at least one condition; a batch holds 1..=[`MAX_WRITE_OPS`] operations,
//!   refused as a whole if any operation is, or if its values total more than [`MAX_WRITE_BYTES`].
//!
//! Every statement runs on the hotel database the Manager connects to; the builder never names a
//! database.

use crate::bindings::aio::plugin::types::{CompareOp, Condition, Select, SqlValue as WitValue, WriteOp};
use crate::permissions::Permission;
use crate::runtime::sanitise;
use crate::services::{SqlValue, WriteBatch};

/// Rows a select returns when the plugin gives no limit.
pub const DEFAULT_LIMIT: u32 = 1_000;
/// Most rows a select may return.
pub const MAX_LIMIT: u32 = 10_000;
/// Most operations in one write batch.
pub const MAX_WRITE_OPS: usize = 1_000;
/// Most parameter data in one write batch (bytes of text and binary values; 8 per number), so the approval card,
/// which shows every value in full, stays small enough to draw.
pub const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;
/// Most columns a select names or an insert / update sets.
pub const MAX_COLUMNS: usize = 256;
/// Most conditions of one statement.
pub const MAX_CONDITIONS: usize = 64;
/// Characters of a text value shown in a summary line.
const SUMMARY_TEXT: usize = 80;
/// Bytes of a non UTF-8 value shown (in hex) in a summary line.
const SUMMARY_BYTES: usize = 32;

/// Private columns of the Arcturus schema, `(table, column)`, lower case.
const PRIVATE_COLUMNS: &[(&str, &str)] = &[
    ("users", "password"),
    ("users", "mail"),
    ("users", "ip_register"),
    ("users", "ip_current"),
    ("users", "auth_ticket"),
    ("users", "machine_id"),
];
/// A column with exactly one of these names (any case) is private on every table (`bans.ip`, ...).
const PRIVATE_NAMES: &[&str] = &["ip", "machine_id"];
/// A column whose name contains one of these (any case) is private on every table.
const PRIVATE_PARTS: &[&str] = &["password", "token", "secret", "ip_"];

fn deny(reason: impl std::fmt::Display) -> String {
    format!("permission denied: {reason}")
}

/// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`
pub fn valid_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// A MariaDB identifier in backticks (as `aio_remote::sql_session::quote`). Only ever called on
/// identifiers that passed [`valid_identifier`], which cannot hold a backtick anyway.
fn quote(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

/// The identifier itself, or a refusal that shows it sanitised (it is plugin text).
fn identifier<'a>(kind: &str, s: &'a str) -> Result<&'a str, String> {
    if valid_identifier(s) {
        Ok(s)
    } else {
        let shown: String = s.chars().take(80).collect();
        Err(deny(format!("invalid {kind} name '{}'", sanitise(&shown))))
    }
}

/// True for a column the Manager treats as private: the list above, a column named `ip` or
/// `machine_id`, or a name containing `password`, `token`, `secret` or `ip_`. Case is ignored for both table and column.
pub fn is_private(table: &str, column: &str) -> bool {
    let (t, c) = (table.to_ascii_lowercase(), column.to_ascii_lowercase());
    PRIVATE_COLUMNS.iter().any(|(pt, pc)| *pt == t && *pc == c)
        || PRIVATE_NAMES.contains(&c.as_str())
        || PRIVATE_PARTS.iter().any(|p| c.contains(p))
}

/// `table` must be listed exactly (case included) in one of the plugin's `sql:read` or `sql:write`
/// permissions.
fn check_table<'a>(table: &'a str, granted: &[Permission], write: bool) -> Result<&'a str, String> {
    let table = identifier("table", table)?;
    let listed = granted.iter().any(|p| match (p, write) {
        (Permission::SqlRead(tables), false) | (Permission::SqlWrite(tables), true) => tables.iter().any(|t| t == table),
        _ => false,
    });
    if listed {
        return Ok(table);
    }
    let kind = if write { "sql:write" } else { "sql:read" };
    let near = granted.iter().any(|p| match p {
        Permission::SqlRead(tables) | Permission::SqlWrite(tables) => tables.iter().any(|t| t.eq_ignore_ascii_case(table)),
        _ => false,
    });
    if near {
        Err(deny(format!("table '{table}' is not declared with {kind} (write the table name exactly as declared)")))
    } else {
        Err(deny(format!("table '{table}' is not declared with {kind}")))
    }
}

/// A column of `table`; a private one only with `sql:read-private:<table>.<column>`.
fn check_column<'a>(table: &str, column: &'a str, granted: &[Permission]) -> Result<&'a str, String> {
    let column = identifier("column", column)?;
    if is_private(table, column) {
        let allowed = granted.iter().any(|p| {
            matches!(p, Permission::SqlReadPrivate(t, c) if t == table && c.eq_ignore_ascii_case(column))
        });
        if !allowed {
            return Err(deny(format!("column '{table}.{column}' is private (needs sql:read-private:{table}.{column})")));
        }
    }
    Ok(column)
}

fn value(v: &WitValue) -> Result<SqlValue, String> {
    Ok(match v {
        WitValue::Null => SqlValue::Null,
        WitValue::Int(i) => SqlValue::Int(*i),
        WitValue::Float(f) if f.is_finite() => SqlValue::Float(*f),
        WitValue::Float(_) => return Err(deny("a float value must be finite")),
        WitValue::Text(s) => SqlValue::Text(s.clone()),
        WitValue::Bytes(b) => SqlValue::Bytes(b.clone()),
    })
}

/// Text in quotes, clipped to [`SUMMARY_TEXT`] characters (with the real length) and sanitised.
fn shown_text(s: &str) -> String {
    let total = s.chars().count();
    let clipped: String = s.chars().take(SUMMARY_TEXT).collect();
    let text = sanitise(&clipped.replace('\'', "''"));
    if total > SUMMARY_TEXT {
        format!("'{text}\u{2026}' ({total} chars)")
    } else {
        format!("'{text}'")
    }
}

/// A value as the user reads it in a preview. Bytes end up in text columns too, so UTF-8 bytes are
/// shown as text; other bytes as hex (the first [`SUMMARY_BYTES`]) with their length.
fn shown(v: &SqlValue) -> String {
    match v {
        SqlValue::Null => "NULL".into(),
        SqlValue::Int(i) => i.to_string(),
        SqlValue::Float(f) => f.to_string(),
        SqlValue::Text(s) => shown_text(s),
        SqlValue::Bytes(b) => match std::str::from_utf8(b) {
            Ok(s) => shown_text(s),
            Err(_) => {
                let hex: String = b.iter().take(SUMMARY_BYTES).map(|x| format!("{x:02x}")).collect();
                let more = if b.len() > SUMMARY_BYTES { "\u{2026}" } else { "" };
                format!("0x{hex}{more} ({} bytes)", b.len())
            }
        },
    }
}

/// ` WHERE a = ? AND ...` (empty for no conditions), its parameters and its summary text.
fn conditions(table: &str, list: &[Condition], granted: &[Permission]) -> Result<(String, Vec<SqlValue>, String), String> {
    if list.len() > MAX_CONDITIONS {
        return Err(deny(format!("more than {MAX_CONDITIONS} conditions")));
    }
    let (mut sql, mut params, mut summary) = (Vec::new(), Vec::new(), Vec::new());
    for c in list {
        let column = check_column(table, &c.column, granted)?;
        let op = match c.op {
            CompareOp::Eq => "=",
            CompareOp::Ne => "<>",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
            CompareOp::Like => "LIKE",
            CompareOp::IsNull => "IS NULL",
            CompareOp::NotNull => "IS NOT NULL",
        };
        if matches!(c.op, CompareOp::IsNull | CompareOp::NotNull) {
            sql.push(format!("{} {op}", quote(column)));
            summary.push(format!("{column} {op}"));
        } else {
            let v = value(&c.value)?;
            sql.push(format!("{} {op} ?", quote(column)));
            summary.push(format!("{column} {op} {}", shown(&v)));
            params.push(v);
        }
    }
    if sql.is_empty() {
        return Ok((String::new(), params, String::new()));
    }
    Ok((format!(" WHERE {}", sql.join(" AND ")), params, format!(" WHERE {}", summary.join(" AND "))))
}

/// Builds a checked, parameterised `SELECT` for `q`.
pub fn build_select(q: &Select, granted: &[Permission]) -> Result<(String, Vec<SqlValue>), String> {
    let table = check_table(&q.table, granted, false)?;
    if q.columns.is_empty() {
        return Err(deny("list the columns to read (there is no 'every column')"));
    }
    if q.columns.len() > MAX_COLUMNS {
        return Err(deny(format!("more than {MAX_COLUMNS} columns")));
    }
    let columns = q
        .columns
        .iter()
        .map(|c| check_column(table, c, granted).map(quote))
        .collect::<Result<Vec<_>, _>>()?;
    let (filter, params, _) = conditions(table, &q.conditions, granted)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    Ok((format!("SELECT {} FROM {}{filter} LIMIT {limit}", columns.join(", "), quote(table)), params))
}

/// Checked `column = value` pairs of an insert or update: SQL column names, summary names, values.
fn assignments(
    table: &str,
    list: &[(String, WitValue)],
    granted: &[Permission],
) -> Result<Vec<(String, String, SqlValue)>, String> {
    if list.is_empty() {
        return Err(deny("an insert or update must set at least one column"));
    }
    if list.len() > MAX_COLUMNS {
        return Err(deny(format!("more than {MAX_COLUMNS} columns")));
    }
    let mut out: Vec<(String, String, SqlValue)> = Vec::with_capacity(list.len());
    for (column, v) in list {
        let column = check_column(table, column, granted)?;
        if out.iter().any(|(_, c, _)| c.eq_ignore_ascii_case(column)) {
            return Err(deny(format!("column '{column}' is set twice")));
        }
        out.push((quote(column), column.to_string(), value(v)?));
    }
    Ok(out)
}

/// The parameter data of a batch as [`MAX_WRITE_BYTES`] counts it (saturating, never overflows).
fn write_bytes(ops: &[WriteOp]) -> usize {
    let size = |v: &WitValue| match v {
        WitValue::Text(s) => s.len(),
        WitValue::Bytes(b) => b.len(),
        _ => 8,
    };
    let conditions = |list: &[Condition]| list.iter().fold(0usize, |n, c| n.saturating_add(size(&c.value)));
    let values = |list: &[(String, WitValue)]| list.iter().fold(0usize, |n, (_, v)| n.saturating_add(size(v)));
    ops.iter().fold(0usize, |n, op| {
        n.saturating_add(match op {
            WriteOp::Insert(i) => values(&i.values),
            WriteOp::Update(u) => values(&u.set).saturating_add(conditions(&u.conditions)),
            WriteOp::Delete(d) => conditions(&d.conditions),
        })
    })
}

/// Builds the checked, parameterised statements of a write batch and one summary line per statement.
pub fn build_writes(ops: &[WriteOp], granted: &[Permission]) -> Result<WriteBatch, String> {
    if ops.is_empty() {
        return Err(deny("an empty write batch"));
    }
    if ops.len() > MAX_WRITE_OPS {
        return Err(deny(format!("more than {MAX_WRITE_OPS} operations in one batch")));
    }
    if write_bytes(ops) > MAX_WRITE_BYTES {
        return Err(deny(format!(
            "the changes are too large to show (more than {} MiB of values in one batch)",
            MAX_WRITE_BYTES / (1024 * 1024)
        )));
    }
    let mut batch = WriteBatch::default();
    for op in ops {
        let (sql, params, summary) = match op {
            WriteOp::Insert(i) => {
                let table = check_table(&i.table, granted, true)?;
                let set = assignments(table, &i.values, granted)?;
                let cols: Vec<&str> = set.iter().map(|(q, _, _)| q.as_str()).collect();
                let names: Vec<&str> = set.iter().map(|(_, n, _)| n.as_str()).collect();
                let shown_values: Vec<String> = set.iter().map(|(_, _, v)| shown(v)).collect();
                let marks = vec!["?"; set.len()].join(", ");
                let sql = format!("INSERT INTO {} ({}) VALUES ({marks})", quote(table), cols.join(", "));
                let summary = format!("INSERT INTO {table} ({}) VALUES ({})", names.join(", "), shown_values.join(", "));
                (sql, set.into_iter().map(|(_, _, v)| v).collect(), summary)
            }
            WriteOp::Update(u) => {
                let table = check_table(&u.table, granted, true)?;
                let set = assignments(table, &u.set, granted)?;
                if u.conditions.is_empty() {
                    return Err(deny(format!("an update of '{table}' needs at least one condition")));
                }
                let (filter, cond_params, cond_summary) = conditions(table, &u.conditions, granted)?;
                let sql_set: Vec<String> = set.iter().map(|(q, _, _)| format!("{q} = ?")).collect();
                let sum_set: Vec<String> = set.iter().map(|(_, n, v)| format!("{n} = {}", shown(v))).collect();
                let mut params: Vec<SqlValue> = set.into_iter().map(|(_, _, v)| v).collect();
                params.extend(cond_params);
                (
                    format!("UPDATE {} SET {}{filter}", quote(table), sql_set.join(", ")),
                    params,
                    format!("UPDATE {table} SET {}{cond_summary}", sum_set.join(", ")),
                )
            }
            WriteOp::Delete(d) => {
                let table = check_table(&d.table, granted, true)?;
                if d.conditions.is_empty() {
                    return Err(deny(format!("a delete from '{table}' needs at least one condition")));
                }
                let (filter, params, cond_summary) = conditions(table, &d.conditions, granted)?;
                (format!("DELETE FROM {}{filter}", quote(table)), params, format!("DELETE FROM {table}{cond_summary}"))
            }
        };
        batch.statements.push((sql, params));
        batch.summary.push(summary);
    }
    Ok(batch)
}
