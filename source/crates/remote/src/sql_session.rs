//! MariaDB over an SSH tunnel to 127.0.0.1:3306 on the server, the way HeidiSQL connects. One connection runs the
//! query editor (so USE and variables last), the other one the database tree, table data and edits. (SqlSession.cs)
//!
//! The C# code uses MySqlConnector, which fills `@name` parameters in on the client. This port sends the same SQL
//! text with the parameters as `?` of a prepared statement (`*_sql` functions return the C# text, with `@name`).
//! Query results are read over the text protocol and decoded per column type into [`SqlValue`], rendered by
//! [`display`] like the C# `SqlSession.Display` renders MySqlConnector's values.

use std::time::{Duration, Instant};

use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{Column, Conn, Opts, OptsBuilder, Row, TxOpts, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::sql_script;
use crate::server::{PortForward, Result, Server, ServerError};

/// Rows per page of the table data tab (SqlEditorView.PageSize).
pub const PAGE_SIZE: i32 = 1000;
/// Rows kept per result of the query editor (SqlEditorView.MaxQueryRows).
pub const MAX_QUERY_ROWS: usize = 10_000;

/// MariaDB's ER_QUERY_INTERRUPTED (MySqlErrorCode.QueryInterrupted), what KILL QUERY makes a statement fail with.
const QUERY_INTERRUPTED: u16 = 1317;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlColumn {
    pub name: String,
    pub data_type: String,
}

impl SqlColumn {
    pub fn is_binary(&self) -> bool {
        let upper = self.data_type.to_uppercase();
        upper.contains("BLOB") || upper.contains("BINARY")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTableInfo {
    pub name: String,
    pub is_view: bool,
    pub rows: Option<i64>,
    pub bytes: Option<i64>,
    pub engine: Option<String>,
}

/// One cell, as MySqlConnector would have returned it (with AllowZeroDateTime, TreatTinyAsBoolean off, GuidFormat
/// None). How [`display`] renders each:
/// - `Null`: None (the grids show their NULL text);
/// - `Int` / `UInt`: the number; `Float` / `Double`: like .NET's `ToString(InvariantCulture)` (shortest round-trip,
///   "1E+20", "1E-05" style exponents); `Decimal`: the server's text, scale kept ("1.50");
/// - `Text`: as is; `Bytes` (binary columns): "0x" + upper-case hex, cut after `max_bytes` with "… (N bytes)";
/// - `Date`: "yyyy-MM-dd"; `DateTime`: "yyyy-MM-dd HH:mm:ss", plus ".ffffff" when the fraction is not zero;
///   zero / invalid dates keep their digits ("0000-00-00 00:00:00"); `Time`: "[-]HH:mm:ss" (hours can exceed 24,
///   no fraction). The date and time variants hold that rendered text already.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    UInt(u64),
    Float(f32),
    Double(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Date(String),
    DateTime(String),
    Time(String),
}

impl SqlValue {
    pub fn is_null(&self) -> bool {
        matches!(self, SqlValue::Null)
    }

    /// The value as a statement parameter.
    fn to_param(&self) -> Value {
        match self {
            SqlValue::Null => Value::NULL,
            SqlValue::Int(v) => Value::Int(*v),
            SqlValue::UInt(v) => Value::UInt(*v),
            SqlValue::Float(v) => Value::Float(*v),
            SqlValue::Double(v) => Value::Double(*v),
            SqlValue::Bytes(v) => Value::Bytes(v.clone()),
            SqlValue::Decimal(t) | SqlValue::Text(t) | SqlValue::Date(t) | SqlValue::DateTime(t) | SqlValue::Time(t) => {
                Value::Bytes(t.as_bytes().to_vec())
            }
        }
    }
}

/// How the text protocol's bytes of a column are decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Int,
    UInt,
    Float,
    Double,
    Decimal,
    Text,
    Bytes,
    Bit,
    Date,
    DateTime,
    Time,
}

/// What one statement returned: rows (also none) when it is a query, otherwise how many rows it changed.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlResult {
    pub statement: String,
    pub columns: Option<Vec<SqlColumn>>,
    pub rows: Vec<Vec<SqlValue>>,
    /// More rows were returned than were kept.
    pub truncated: bool,
    pub affected: u64,
    pub elapsed: Duration,
}

impl SqlResult {
    fn new(statement: &str, columns: Option<Vec<SqlColumn>>) -> Self {
        SqlResult { statement: statement.to_string(), columns, rows: Vec::new(), truncated: false, affected: 0, elapsed: Duration::ZERO }
    }
}

/// The results of a script up to the first error, which stops it like the mariadb client does.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlRun {
    pub results: Vec<SqlResult>,
    pub statements: usize,
    pub error: Option<String>,
    pub error_line: usize,
    pub cancelled: bool,
}

/// One part of a primary key: the column and the row's value.
pub type KeyPart = (String, SqlValue);

// ---- SQL text ---------------------------------------------------------------------------------------------------

pub const DATABASES_SQL: &str = "SHOW DATABASES";
pub const TABLES_SQL: &str = "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, DATA_LENGTH + INDEX_LENGTH, ENGINE FROM information_schema.TABLES WHERE TABLE_SCHEMA = @db ORDER BY TABLE_NAME";
pub const PRIMARY_KEY_SQL: &str = "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA = @db AND TABLE_NAME = @table AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY ORDINAL_POSITION";
pub const CURRENT_DATABASE_SQL: &str = "SELECT DATABASE()";

pub fn quote(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

/// `filter` is a WHERE condition as typed by the user, or empty.
pub fn table_data_sql(database: &str, table: &str, filter: &str, order_by: Option<&str>, descending: bool, offset: i64, limit: i32) -> String {
    let mut sql = format!("SELECT * FROM {}.{}", quote(database), quote(table));
    if !filter.trim().is_empty() {
        sql += &format!(" WHERE {filter}");
    }
    if let Some(order_by) = order_by {
        sql += &format!(" ORDER BY {}{}", quote(order_by), if descending { " DESC" } else { "" });
    }
    sql += &format!(" LIMIT {offset}, {limit}");
    sql
}

pub fn columns_sql(database: &str, table: &str) -> String {
    format!("SHOW FULL COLUMNS FROM {}.{}", quote(database), quote(table))
}

pub fn create_table_sql(database: &str, table: &str) -> String {
    format!("SHOW CREATE TABLE {}.{}", quote(database), quote(table))
}

// NULL-safe, so a key part that is NULL still matches.
fn key_condition_with(key_columns: &[&str], named: bool) -> String {
    key_columns
        .iter()
        .enumerate()
        .map(|(i, column)| format!("{} <=> {}", quote(column), if named { format!("@key{i}") } else { "?".into() }))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The C# KeyCondition: "`a` <=> @key0 AND `b` <=> @key1".
pub fn key_condition(key_columns: &[&str]) -> String {
    key_condition_with(key_columns, true)
}

fn update_cell_with(database: &str, table: &str, column: &str, key_columns: &[&str], named: bool) -> String {
    format!(
        "UPDATE {}.{} SET {} = {} WHERE {} LIMIT 1",
        quote(database),
        quote(table),
        quote(column),
        if named { "@value" } else { "?" },
        key_condition_with(key_columns, named)
    )
}

/// The UPDATE of [`SqlSession::update_cell`], as the C# writes it (parameters @value, @key0, ...).
pub fn update_cell_sql(database: &str, table: &str, column: &str, key_columns: &[&str]) -> String {
    update_cell_with(database, table, column, key_columns, true)
}

fn delete_row_with(database: &str, table: &str, key_columns: &[&str], named: bool) -> String {
    format!("DELETE FROM {}.{} WHERE {} LIMIT 1", quote(database), quote(table), key_condition_with(key_columns, named))
}

/// The DELETE of one row of [`SqlSession::delete_rows`], as the C# writes it (parameters @key0, ...).
pub fn delete_row_sql(database: &str, table: &str, key_columns: &[&str]) -> String {
    delete_row_with(database, table, key_columns, true)
}

// MariaDB announces itself as "5.5.5-10.11.x-MariaDB" for old MySQL clients.
pub fn clean_server_version(version: &str) -> String {
    version.strip_prefix("5.5.5-").unwrap_or(version).to_string()
}

// ---- Values ---------------------------------------------------------------------------------------------------

/// The kind and the type name (like MySqlConnector's GetDataTypeName) of a result column.
pub fn column_kind(column_type: ColumnType, flags: ColumnFlags, character_set: u16) -> (ColumnKind, &'static str) {
    use ColumnType::*;
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    // Character set 63 is "binary": BLOB, BINARY, VARBINARY and binary strings.
    let binary = character_set == 63;
    let int = |name| (if unsigned { ColumnKind::UInt } else { ColumnKind::Int }, name);
    match column_type {
        MYSQL_TYPE_TINY => int("TINYINT"),
        MYSQL_TYPE_SHORT => int("SMALLINT"),
        MYSQL_TYPE_INT24 => int("MEDIUMINT"),
        MYSQL_TYPE_LONG => int("INT"),
        MYSQL_TYPE_LONGLONG => int("BIGINT"),
        MYSQL_TYPE_YEAR => (ColumnKind::Int, "YEAR"),
        MYSQL_TYPE_FLOAT => (ColumnKind::Float, "FLOAT"),
        MYSQL_TYPE_DOUBLE => (ColumnKind::Double, "DOUBLE"),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => (ColumnKind::Decimal, "DECIMAL"),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => (ColumnKind::Date, "DATE"),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => (ColumnKind::DateTime, "DATETIME"),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => (ColumnKind::DateTime, "TIMESTAMP"),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => (ColumnKind::Time, "TIME"),
        MYSQL_TYPE_BIT => (ColumnKind::Bit, "BIT"),
        MYSQL_TYPE_NULL => (ColumnKind::Text, "NULL"),
        MYSQL_TYPE_JSON => (ColumnKind::Text, "JSON"),
        MYSQL_TYPE_GEOMETRY => (ColumnKind::Bytes, "GEOMETRY"),
        MYSQL_TYPE_ENUM => (ColumnKind::Text, "ENUM"),
        MYSQL_TYPE_SET => (ColumnKind::Text, "SET"),
        _ if flags.contains(ColumnFlags::ENUM_FLAG) => (ColumnKind::Text, "ENUM"),
        _ if flags.contains(ColumnFlags::SET_FLAG) => (ColumnKind::Text, "SET"),
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary { (ColumnKind::Bytes, "BLOB") } else { (ColumnKind::Text, "TEXT") }
        }
        MYSQL_TYPE_STRING => {
            if binary { (ColumnKind::Bytes, "BINARY") } else { (ColumnKind::Text, "CHAR") }
        }
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary { (ColumnKind::Bytes, "VARBINARY") } else { (ColumnKind::Text, "VARCHAR") }
        }
        _ if binary => (ColumnKind::Bytes, "BLOB"),
        _ => (ColumnKind::Text, "VARCHAR"),
    }
}

/// A cell from the text protocol's bytes.
pub fn decode(kind: ColumnKind, bytes: &[u8]) -> SqlValue {
    let text = || String::from_utf8_lossy(bytes).into_owned();
    match kind {
        ColumnKind::Int => text().parse().map(SqlValue::Int).unwrap_or_else(|_| SqlValue::Text(text())),
        ColumnKind::UInt => text().parse().map(SqlValue::UInt).unwrap_or_else(|_| SqlValue::Text(text())),
        ColumnKind::Float => text().parse().map(SqlValue::Float).unwrap_or_else(|_| SqlValue::Text(text())),
        ColumnKind::Double => text().parse().map(SqlValue::Double).unwrap_or_else(|_| SqlValue::Text(text())),
        ColumnKind::Decimal => SqlValue::Decimal(text()),
        ColumnKind::Text => SqlValue::Text(text()),
        ColumnKind::Bytes => SqlValue::Bytes(bytes.to_vec()),
        // BIT(n) is sent as big-endian bytes; MySqlConnector returns it as a ulong.
        ColumnKind::Bit => SqlValue::UInt(bytes.iter().fold(0u64, |v, b| (v << 8) | *b as u64)),
        ColumnKind::Date => SqlValue::Date(text()),
        ColumnKind::DateTime => SqlValue::DateTime(normalize_datetime(&text())),
        ColumnKind::Time => SqlValue::Time(normalize_time(&text())),
    }
}

// .NET prints a DateTime with ".ffffff" only when the fraction is not zero, and MySqlDateTime's zero/invalid dates
// without a fraction.
fn normalize_datetime(text: &str) -> String {
    let Some((whole, fraction)) = text.split_once('.') else { return text.to_string() };
    let date = whole.split(' ').next().unwrap_or("");
    let valid = date.split('-').all(|part| part.parse::<u32>().map(|n| n != 0).unwrap_or(false));
    if !valid || fraction.chars().all(|c| c == '0') {
        return whole.to_string();
    }
    format!("{whole}.{:0<6}", &fraction[..fraction.len().min(6)])
}

// A TimeSpan is shown as [-]HH:mm:ss with the total hours, without the fraction.
fn normalize_time(text: &str) -> String {
    let whole = text.split('.').next().unwrap_or(text);
    match whole.strip_prefix('-') {
        // -00:00:00.5 is negative, -00:00:00 is zero.
        Some(rest) if rest.chars().all(|c| c == '0' || c == ':') && text[1..].chars().all(|c| c == '0' || c == ':' || c == '.') => {
            rest.to_string()
        }
        _ => whole.to_string(),
    }
}

/// A double or float like .NET Core's `ToString(CultureInfo.InvariantCulture)`: the shortest round-trip digits,
/// in exponent form ("1E+20", "1E-05") when the exponent is at least max(digits, `precision`: 17 for a double, 9 for a float) or below -4.
fn dotnet_number(scientific: &str, precision: i32) -> String {
    // Rust's "{:e}" gives the shortest round-trip digits: "-1.5e20", "1e-5", "0e0".
    let (negative, rest) = match scientific.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, scientific),
    };
    let (mantissa, exponent) = rest.split_once('e').unwrap_or((rest, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let count = digits.len() as i32;
    let sign = if negative { "-" } else { "" };
    if exponent >= count.max(precision) || exponent < -4 {
        let (first, others) = digits.split_at(1);
        let others = if others.is_empty() { String::new() } else { format!(".{others}") };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{first}{others}E{exp_sign}{:02}", exponent.abs());
    }
    if exponent >= 0 {
        let whole = exponent as usize + 1;
        if digits.len() <= whole {
            format!("{sign}{digits}{}", "0".repeat(whole - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..whole], &digits[whole..])
        }
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    }
}

/// A value as the grids show it, binary data cut off after `max_bytes`. NULL is None.
pub fn display(value: &SqlValue, column: Option<&SqlColumn>, max_bytes: usize) -> Option<String> {
    let is_date_column = column.is_some_and(|c| c.data_type.eq_ignore_ascii_case("DATE"));
    Some(match value {
        SqlValue::Null => return None,
        SqlValue::Bytes(bytes) => {
            let shown: String = bytes[..bytes.len().min(max_bytes)].iter().map(|b| format!("{b:02X}")).collect();
            let more = if bytes.len() > max_bytes { format!("… ({} bytes)", bytes.len()) } else { String::new() };
            format!("0x{shown}{more}")
        }
        SqlValue::DateTime(text) if is_date_column => text.chars().take(10).collect(),
        SqlValue::Int(v) => v.to_string(),
        SqlValue::UInt(v) => v.to_string(),
        SqlValue::Float(v) => dotnet_number(&format!("{v:e}"), 9),
        SqlValue::Double(v) => dotnet_number(&format!("{v:e}"), 17),
        SqlValue::Decimal(t) | SqlValue::Text(t) | SqlValue::Date(t) | SqlValue::DateTime(t) | SqlValue::Time(t) => t.clone(),
    })
}

/// A CSV field: quoted when it holds a comma, quote or line break; NULL is empty.
pub fn csv_field(value: Option<&str>) -> String {
    match value {
        None => String::new(),
        Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
        Some(v) => v.to_string(),
    }
}

/// The CSV export of a result (header and rows, the full binary values), with the platform's line ends like
/// StringBuilder.AppendLine. None when it is not a query result.
pub fn csv(result: &SqlResult) -> Option<String> {
    let columns = result.columns.as_ref()?;
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut out = String::new();
    out += &columns.iter().map(|c| csv_field(Some(&c.name))).collect::<Vec<_>>().join(",");
    out += newline;
    for row in &result.rows {
        let fields: Vec<String> =
            row.iter().enumerate().map(|(i, v)| csv_field(display(v, columns.get(i), usize::MAX).as_deref())).collect();
        out += &fields.join(",");
        out += newline;
    }
    Some(out)
}

/// [`csv`] as UTF-8 with a byte order mark, as the C# writes the file.
pub fn csv_bytes(result: &SqlResult) -> Option<Vec<u8>> {
    let text = csv(result)?;
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Int(v) => Some(v.to_string()),
        Value::UInt(v) => Some(v.to_string()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(v) => Some(*v),
        Value::UInt(v) => Some(*v as i64),
        Value::Float(v) => Some(*v as i64),
        Value::Double(v) => Some(*v as i64),
        other => value_text(other).and_then(|t| t.parse::<f64>().ok()).map(|v| v as i64),
    }
}

// ---- The session --------------------------------------------------------------------------------------------------

/// Two connections through one SSH port forward. Drop it or call [`SqlSession::close`] to end both.
pub struct SqlSession {
    query: Mutex<Conn>,
    browse: Mutex<Conn>,
    query_id: u32,
    opts: Opts,
    user: String,
    server_version: String,
    // Last, so the connections close before the tunnel.
    _forward: PortForward,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

async fn connect(opts: &Opts) -> Result<Conn> {
    match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts.clone())).await {
        Ok(conn) => Ok(conn?),
        Err(_) => Err(ServerError("Connect Timeout expired.".into()).into()),
    }
}

// Like MySqlConnector's command cancel: KILL QUERY from another connection.
async fn kill_query(opts: &Opts, id: u32) {
    if let Ok(Ok(mut conn)) = tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts.clone())).await {
        let _ = conn.query_drop(format!("KILL QUERY {id}")).await;
        let _ = conn.disconnect().await;
    }
}

/// A value of the binary protocol (a prepared statement's row) as a [`SqlValue`]: bytes are text.
fn param_value(value: Value) -> SqlValue {
    match value {
        Value::NULL => SqlValue::Null,
        Value::Bytes(bytes) => SqlValue::Text(String::from_utf8_lossy(&bytes).into_owned()),
        Value::Int(v) => SqlValue::Int(v),
        Value::UInt(v) => SqlValue::UInt(v),
        Value::Float(v) => SqlValue::Float(v),
        Value::Double(v) => SqlValue::Double(v),
        other => SqlValue::Text(value_text(&other).unwrap_or_default()),
    }
}

fn row_values(row: Row, kinds: &[ColumnKind]) -> Vec<SqlValue> {
    row.unwrap()
        .into_iter()
        .enumerate()
        .map(|(i, value)| match value {
            Value::NULL => SqlValue::Null,
            Value::Bytes(bytes) => decode(kinds.get(i).copied().unwrap_or(ColumnKind::Text), &bytes),
            Value::Int(v) => SqlValue::Int(v),
            Value::UInt(v) => SqlValue::UInt(v),
            Value::Float(v) => SqlValue::Float(v),
            Value::Double(v) => SqlValue::Double(v),
            other => SqlValue::Text(value_text(&other).unwrap_or_default()),
        })
        .collect()
}

fn result_columns(columns: &[Column]) -> (Vec<SqlColumn>, Vec<ColumnKind>) {
    columns
        .iter()
        .map(|c| {
            let (kind, name) = column_kind(c.column_type(), c.flags(), c.character_set());
            (SqlColumn { name: c.name_str().into_owned(), data_type: name.to_string() }, kind)
        })
        .unzip()
}

// A statement can return several result sets (CALL); each becomes a result. Without any, the affected rows are one.
async fn execute_all(conn: &mut Conn, opts: &Opts, sql: &str, max_rows: usize) -> std::result::Result<Vec<SqlResult>, mysql_async::Error> {
    let id = conn.id();
    let watch = Instant::now();
    let mut results = Vec::new();
    let mut truncated = false;
    let mut affected = 0u64;
    let mut result = conn.query_iter(sql).await?;
    loop {
        match result.columns() {
            Some(columns) if !columns.is_empty() => {
                let (columns, kinds) = result_columns(&columns);
                let mut set = SqlResult::new(sql, Some(columns));
                while let Some(row) = result.next().await? {
                    if set.rows.len() >= max_rows {
                        set.truncated = true;
                        truncated = true;
                        break;
                    }
                    set.rows.push(row_values(row, &kinds));
                }
                set.elapsed = watch.elapsed();
                results.push(set);
                if truncated {
                    break;
                }
            }
            Some(_) => {
                affected += result.affected_rows();
                result.next().await?;
            }
            // An error of a later result set comes out here.
            None => {
                result.next().await?;
                if result.columns().is_none() {
                    break;
                }
            }
        }
        if result.is_empty() {
            break;
        }
    }
    if truncated {
        // Reading the rest of a huge result would take as long as the query; stop it on the server instead.
        kill_query(opts, id).await;
        let _ = result.drop_result().await;
    } else {
        drop(result);
    }
    if results.is_empty() {
        let mut set = SqlResult::new(sql, None);
        set.affected = affected;
        set.elapsed = watch.elapsed();
        results.push(set);
    }
    Ok(results)
}

async fn execute(conn: &mut Conn, opts: &Opts, sql: &str, max_rows: usize) -> Result<SqlResult> {
    let results = execute_all(conn, opts, sql, max_rows).await?;
    Ok(results.into_iter().find(|r| r.columns.is_some()).unwrap_or_else(|| SqlResult::new(sql, Some(Vec::new()))))
}

impl SqlSession {
    pub async fn open(server: &dyn Server, user: &str, password: &str, port: u16) -> Result<SqlSession> {
        let forward = server.forward_local_port("127.0.0.1", port).await?;
        let opts: Opts = OptsBuilder::default()
            .ip_or_hostname("127.0.0.1")
            .tcp_port(forward.local_port)
            .user(Some(user))
            .pass(Some(password))
            // The tunnel is encrypted already (no ssl_opts), and it is TCP, never a local socket.
            .prefer_socket(false)
            // Changed rows, as the mariadb client reports them, rather than matched rows.
            .client_found_rows(false)
            .into();
        let query = connect(&opts).await?;
        let mut browse = match connect(&opts).await {
            Ok(browse) => browse,
            Err(error) => {
                let _ = query.disconnect().await;
                return Err(error);
            }
        };
        let version: Option<String> = match browse.query_first("SELECT VERSION()").await {
            Ok(version) => version,
            Err(error) => {
                let _ = query.disconnect().await;
                let _ = browse.disconnect().await;
                return Err(error.into());
            }
        };
        Ok(SqlSession {
            query_id: query.id(),
            query: Mutex::new(query),
            browse: Mutex::new(browse),
            opts,
            user: user.to_string(),
            server_version: clean_server_version(&version.unwrap_or_default()),
            _forward: forward,
        })
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    /// Without the "5.5.5-" MariaDB puts first for old MySQL clients.
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        let mut conn = self.browse.lock().await;
        Ok(conn.query::<String, _>(DATABASES_SQL).await?)
    }

    pub async fn tables(&self, database: &str) -> Result<Vec<SqlTableInfo>> {
        let mut conn = self.browse.lock().await;
        let rows: Vec<Row> = conn.exec(TABLES_SQL.replace("@db", "?"), (database,)).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let values = row.unwrap();
                let get = |i: usize| values.get(i).cloned().unwrap_or(Value::NULL);
                SqlTableInfo {
                    name: value_text(&get(0)).unwrap_or_default(),
                    is_view: value_text(&get(1)).as_deref() == Some("VIEW"),
                    rows: value_i64(&get(2)),
                    bytes: value_i64(&get(3)),
                    engine: value_text(&get(4)),
                }
            })
            .collect())
    }

    /// The primary key's columns; empty when the table has none, and then its rows cannot be edited.
    pub async fn primary_key(&self, database: &str, table: &str) -> Result<Vec<String>> {
        let mut conn = self.browse.lock().await;
        Ok(conn.exec::<String, _, _>(PRIMARY_KEY_SQL.replace("@db", "?").replace("@table", "?"), (database, table)).await?)
    }

    /// `filter` is a WHERE condition as typed by the user, or empty.
    #[allow(clippy::too_many_arguments)]
    pub async fn table_data(&self, database: &str, table: &str, filter: &str, order_by: Option<&str>, descending: bool, offset: i64, limit: i32) -> Result<SqlResult> {
        let sql = table_data_sql(database, table, filter, order_by, descending, offset, limit);
        let mut conn = self.browse.lock().await;
        execute(&mut conn, &self.opts, &sql, limit.max(0) as usize).await
    }

    /// SHOW FULL COLUMNS of the table.
    pub async fn columns(&self, database: &str, table: &str) -> Result<SqlResult> {
        let mut conn = self.browse.lock().await;
        execute(&mut conn, &self.opts, &columns_sql(database, table), usize::MAX).await
    }

    /// SHOW CREATE TABLE's statement, or empty.
    pub async fn create_statement(&self, database: &str, table: &str) -> Result<String> {
        let mut conn = self.browse.lock().await;
        let result = execute(&mut conn, &self.opts, &create_table_sql(database, table), 1).await?;
        Ok(result.rows.first().and_then(|row| row.get(1)).and_then(|v| display(v, None, 64)).unwrap_or_default())
    }

    /// Sets one cell of the row with this primary key. Returns the number of rows changed (0 when the value was the
    /// same).
    pub async fn update_cell(&self, database: &str, table: &str, column: &str, value: Option<&str>, key: &[KeyPart]) -> Result<u64> {
        let columns: Vec<&str> = key.iter().map(|(c, _)| c.as_str()).collect();
        let sql = update_cell_with(database, table, column, &columns, false);
        let mut params = vec![value.map(|v| Value::Bytes(v.as_bytes().to_vec())).unwrap_or(Value::NULL)];
        params.extend(key.iter().map(|(_, v)| v.to_param()));
        let mut conn = self.browse.lock().await;
        conn.exec_drop(sql, params).await?;
        Ok(conn.affected_rows())
    }

    /// Deletes the rows with these primary keys in one transaction.
    pub async fn delete_rows(&self, database: &str, table: &str, keys: &[Vec<KeyPart>]) -> Result<u64> {
        let mut conn = self.browse.lock().await;
        let mut transaction = conn.start_transaction(TxOpts::default()).await?;
        let mut deleted = 0;
        for key in keys {
            let columns: Vec<&str> = key.iter().map(|(c, _)| c.as_str()).collect();
            let params: Vec<Value> = key.iter().map(|(_, v)| v.to_param()).collect();
            transaction.exec_drop(delete_row_with(database, table, &columns, false), params).await?;
            deleted += transaction.affected_rows();
        }
        transaction.commit().await?;
        Ok(deleted)
    }

    /// Runs one query with `?` parameters on the browse connection and returns its rows. Text comes back as
    /// [`SqlValue::Text`], numbers as numbers. For tools that read tables themselves (the catalogue manager).
    pub async fn select(&self, sql: &str, params: Vec<SqlValue>) -> Result<Vec<Vec<SqlValue>>> {
        let params: Vec<Value> = params.iter().map(SqlValue::to_param).collect();
        let mut conn = self.browse.lock().await;
        let rows: Vec<Row> = conn.exec(sql, params).await?;
        Ok(rows.into_iter().map(|row| row.unwrap().into_iter().map(param_value).collect()).collect())
    }

    /// Runs statements with `?` parameters in one transaction on the browse connection: all of them or none.
    /// Returns the rows changed and the last inserted id.
    pub async fn execute(&self, statements: &[(String, Vec<SqlValue>)]) -> Result<(u64, u64)> {
        let mut conn = self.browse.lock().await;
        let mut transaction = conn.start_transaction(TxOpts::default()).await?;
        let mut changed = 0;
        let mut last_id = 0;
        for (sql, params) in statements {
            let params: Vec<Value> = params.iter().map(SqlValue::to_param).collect();
            transaction.exec_drop(sql.as_str(), params).await?;
            changed += transaction.affected_rows();
            if let Some(id) = transaction.last_insert_id() {
                last_id = id;
            }
        }
        transaction.commit().await?;
        Ok((changed, last_id))
    }

    /// The database the query editor uses (changed by USE).
    pub async fn current_database(&self) -> Result<Option<String>> {
        let mut conn = self.query.lock().await;
        Ok(conn.query_first::<Option<String>, _>(CURRENT_DATABASE_SQL).await?.flatten())
    }

    /// Runs the statements of `script` one by one in `database` and stops at the first error. `progress` gets the
    /// number of the statement that starts and the total. `cancel` (the Stop button) sends KILL QUERY for the running
    /// statement.
    pub async fn run_script(
        &self,
        script: &str,
        database: Option<&str>,
        max_rows: usize,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
        cancel: &CancellationToken,
    ) -> Result<SqlRun> {
        let statements = sql_script::split(script);
        let mut results = Vec::new();
        let mut conn = self.query.lock().await;
        if let Some(database) = database.filter(|d| !d.is_empty()) {
            let current: Option<String> = conn.query_first::<Option<String>, _>(CURRENT_DATABASE_SQL).await?.flatten();
            if current.as_deref() != Some(database) {
                conn.query_drop(format!("USE {}", quote(database))).await?;
            }
        }
        let total = statements.len();
        for (i, statement) in statements.iter().enumerate() {
            if let Some(progress) = progress {
                progress(i + 1, total);
            }
            let stopped = || SqlRun {
                results: Vec::new(),
                statements: total,
                error: Some(format!("Stopped at statement {} (line {}).", i + 1, statement.line)),
                error_line: statement.line,
                cancelled: true,
            };
            if cancel.is_cancelled() {
                return Ok(SqlRun { results, ..stopped() });
            }
            let outcome = {
                let run = execute_all(&mut conn, &self.opts, &statement.text, max_rows);
                tokio::pin!(run);
                tokio::select! {
                    outcome = &mut run => outcome,
                    _ = cancel.cancelled() => {
                        kill_query(&self.opts, self.query_id).await;
                        run.await
                    }
                }
            };
            match outcome {
                Ok(statement_results) => results.extend(statement_results),
                Err(mysql_async::Error::Server(error)) if cancel.is_cancelled() && error.code == QUERY_INTERRUPTED => {
                    return Ok(SqlRun { results, ..stopped() });
                }
                Err(mysql_async::Error::Server(error)) => {
                    return Ok(SqlRun {
                        results,
                        statements: total,
                        error: Some(format!("SQL Error ({}) in statement {} (line {}): {}", error.code, i + 1, statement.line, error.message)),
                        error_line: statement.line,
                        cancelled: false,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(SqlRun { results, statements: total, error: None, error_line: 0, cancelled: false })
    }

    /// Closes both connections and the tunnel.
    pub async fn close(self) {
        let _ = self.query.into_inner().disconnect().await;
        let _ = self.browse.into_inner().disconnect().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_text() {
        assert_eq!(quote("a`b"), "`a``b`");
        assert_eq!(table_data_sql("db", "t", "  ", None, false, 0, 1000), "SELECT * FROM `db`.`t` LIMIT 0, 1000");
        assert_eq!(
            table_data_sql("db", "t", "id > 5", Some("name"), true, 2000, 1000),
            "SELECT * FROM `db`.`t` WHERE id > 5 ORDER BY `name` DESC LIMIT 2000, 1000"
        );
        assert_eq!(
            update_cell_sql("db", "users", "look", &["id", "x`y"]),
            "UPDATE `db`.`users` SET `look` = @value WHERE `id` <=> @key0 AND `x``y` <=> @key1 LIMIT 1"
        );
        assert_eq!(update_cell_with("db", "users", "look", &["id"], false), "UPDATE `db`.`users` SET `look` = ? WHERE `id` <=> ? LIMIT 1");
        assert_eq!(delete_row_sql("db", "t", &["a"]), "DELETE FROM `db`.`t` WHERE `a` <=> @key0 LIMIT 1");
        assert_eq!(delete_row_with("db", "t", &["a", "b"], false), "DELETE FROM `db`.`t` WHERE `a` <=> ? AND `b` <=> ? LIMIT 1");
        assert_eq!(columns_sql("d", "t"), "SHOW FULL COLUMNS FROM `d`.`t`");
        assert_eq!(create_table_sql("d", "t"), "SHOW CREATE TABLE `d`.`t`");
        assert_eq!(clean_server_version("5.5.5-10.11.6-MariaDB-0+deb12u1"), "10.11.6-MariaDB-0+deb12u1");
        assert_eq!(clean_server_version("11.4.2-MariaDB"), "11.4.2-MariaDB");
    }

    #[test]
    fn values() {
        let d = |kind, text: &str| display(&decode(kind, text.as_bytes()), None, 64).unwrap();
        assert_eq!(d(ColumnKind::DateTime, "2024-01-02 03:04:05.120"), "2024-01-02 03:04:05.120000");
        assert_eq!(d(ColumnKind::DateTime, "2024-01-02 03:04:05.000"), "2024-01-02 03:04:05");
        assert_eq!(d(ColumnKind::DateTime, "0000-00-00 00:00:00.000000"), "0000-00-00 00:00:00");
        assert_eq!(d(ColumnKind::Time, "-838:59:59.500000"), "-838:59:59");
        assert_eq!(d(ColumnKind::Time, "-00:00:00.000000"), "00:00:00");
        assert_eq!(d(ColumnKind::Time, "-00:00:00.5"), "-00:00:00");
        assert_eq!(d(ColumnKind::Double, "1e15"), "1000000000000000");
        assert_eq!(d(ColumnKind::Double, "1e20"), "1E+20");
        assert_eq!(d(ColumnKind::Double, "123456789012345"), "123456789012345");
        assert_eq!(d(ColumnKind::Double, "0.0001"), "0.0001");
        assert_eq!(d(ColumnKind::Double, "0.00001"), "1E-05");
        assert_eq!(d(ColumnKind::Double, "-2.5e-7"), "-2.5E-07");
        assert_eq!(d(ColumnKind::Double, "100"), "100");
        assert_eq!(d(ColumnKind::Float, "3.14159"), "3.14159");
        assert_eq!(d(ColumnKind::Bit, "\u{1}\u{2}"), "258");
        assert_eq!(display(&SqlValue::Bytes(vec![0, 0xAB, 0xFF]), None, 2).unwrap(), "0x00AB… (3 bytes)");
        assert_eq!(display(&SqlValue::Null, None, 64), None);
    }

    #[test]
    fn csv_export() {
        let mut result = SqlResult::new("q", Some(vec![
            SqlColumn { name: "a,b".into(), data_type: "VARCHAR".into() },
            SqlColumn { name: "n".into(), data_type: "INT".into() },
        ]));
        result.rows.push(vec![SqlValue::Text("say \"hi\"\nnow".into()), SqlValue::Null]);
        let nl = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(csv(&result).unwrap(), format!("\"a,b\",n{nl}\"say \"\"hi\"\"\nnow\",{nl}"));
        assert_eq!(&csv_bytes(&result).unwrap()[..3], &[0xEF, 0xBB, 0xBF]);
        assert!(csv(&SqlResult::new("u", None)).is_none());
    }
}
