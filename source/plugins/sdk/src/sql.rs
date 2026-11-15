//! The hotel database: reads with [`Select`] and changes with [`write()`].
//!
//! Every query is built from parts (table, columns, conditions); the Manager checks each part against the plugin's
//! permissions and runs it with parameters, so values never become SQL text.
//!
//! # Reading
//!
//! Needs `sql:read:<table>` for the table, written exactly as declared (case included). Table and column names
//! are identifiers: a letter or `_`, then letters, digits or `_`, at most 64 in all. A [private
//! column](self#private-columns) needs one more permission.
//!
//! ```no_run
//! use aio_plugin_sdk::sql::{Op, Select};
//!
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! let rows = Select::from("users")
//!     .columns(["id", "username", "credits"])
//!     .filter("rank", Op::Ge, 5)
//!     .limit(50)
//!     .run()?;
//! for row in &rows {
//!     let name: String = row.get("username")?;
//!     let credits: Option<i64> = row.get("credits")?;
//!     println!("{name}: {credits:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The Manager's rules for a read:
//! - the columns must be listed: there is no `*`, and at most 256 columns;
//! - at most 64 conditions, combined with AND;
//! - a float in a condition must be finite (no NaN or infinity);
//! - at most 1,000 rows without [`limit`](Select::limit), and never more than 10,000 (a larger limit is lowered
//!   to 10,000);
//! - the query has 30 s.
//!
//! # Changing
//!
//! Needs `sql:write:<table>` for every table changed. The changes of one [`write()`] call are shown to the user in a
//! preview and applied together in one transaction only once the user approves them.
//!
//! ```no_run
//! use aio_plugin_sdk::sql::{self, Op};
//!
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! let changed = sql::write([
//!     sql::insert("catalog_pages").value("caption", "Plants").value("visible", true).into(),
//!     sql::update("users").set("credits", 100).filter("id", Op::Eq, 7).into(),
//!     sql::delete("bans").filter("user_id", Op::Eq, 7).into(),
//! ])?;
//! println!("{changed} rows changed");
//! # Ok(())
//! # }
//! ```
//!
//! The Manager's rules for a change:
//! - an update or a delete needs at least one condition (there is no "every row");
//! - an insert or an update sets at least one column, each column once, at most 256;
//! - a float must be finite (no NaN or infinity), in values and in conditions;
//! - a batch has 1 to 1,000 operations and at most 8 MiB of values;
//! - a user who says no gives [`ErrorKind::Rejected`]; applying the changes has 30 s.
//!
//! # Private columns
//!
//! A private column needs `sql:read-private:<table>.<column>` to be read, used in a filter, or written (set by an
//! insert or update), on top of `sql:read:<table>` or `sql:write:<table>`. These columns are private (case
//! ignored):
//! - on `users`: `password`, `mail`, `ip_register`, `ip_current`, `auth_ticket` and `machine_id`;
//! - on every table: a column named exactly `ip` or `machine_id`, and every column whose name contains `password`,
//!   `token`, `secret` or `ip_`.
//!
//! # Refusals
//!
//! A refused request is [`ErrorKind::Denied`]. The SDK refuses these cases itself, before calling the Manager and
//! with the Manager's own message: a read without columns; an empty batch or one of more than 1,000 operations; an
//! insert or update that sets no column; an update or delete without a condition; a float that is not finite, in a
//! value or in a condition other than [`Op::IsNull`] and [`Op::NotNull`]. The Manager checks everything again.

use std::sync::Arc;

use crate::bindings::aio::plugin::{host, types as wit};
use crate::error::{host_result, Error, ErrorKind, Result};

/// Most operations in one [`write()`] batch (the Manager's limit).
pub const MAX_WRITE_OPS: usize = 1_000;

/// A database value.
///
/// New variants may be added in a later 1.x version: match with a `_` arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// An integer. Unsigned columns past `i64::MAX` arrive as [`Text`](Value::Text).
    Int(i64),
    /// A floating-point number. The Manager refuses NaN and infinities in a query.
    Float(f64),
    /// Text. Also how `DECIMAL`, date and time columns arrive.
    Text(String),
    /// Raw bytes (`BLOB` columns).
    Bytes(Vec<u8>),
}

impl Value {
    /// What the value holds, in words, for error messages: `NULL`, `an integer`, `a float`, `text` or `bytes`.
    fn describe(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Int(_) => "an integer",
            Value::Float(_) => "a float",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
        }
    }
}

macro_rules! value_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::Int(i64::from(v))
            }
        }
    )*};
}

value_from_int!(i8, i16, i32, i64, u8, u16, u32);

impl From<bool> for Value {
    /// `1` or `0`, as MariaDB stores a boolean.
    fn from(v: bool) -> Self {
        Value::Int(i64::from(v))
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(f64::from(v))
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::Text(v.clone())
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v)
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Bytes(v.to_vec())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    /// `None` is [`Value::Null`].
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// How a condition compares a column with its value.
///
/// New operators may be added in a later 1.x version: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `LIKE` (`%` and `_` are wildcards).
    Like,
    /// `IS NULL`; the value is ignored (pass [`Value::Null`]).
    IsNull,
    /// `IS NOT NULL`; the value is ignored (pass [`Value::Null`]).
    NotNull,
}

/// `column <op> value`.
#[derive(Debug, Clone, PartialEq)]
struct Condition {
    column: String,
    op: Op,
    value: Value,
}

impl Condition {
    fn new(column: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        Self { column: column.into(), op, value: value.into() }
    }
}

/// A read query, built step by step and run with [`run`](Select::run). See the [module docs](self) for the rules.
#[derive(Debug, Clone, PartialEq)]
#[must_use = "a query does nothing until it is run"]
pub struct Select {
    table: String,
    columns: Vec<String>,
    conditions: Vec<Condition>,
    limit: Option<u32>,
}

impl Select {
    /// A query on `table`, with no columns yet.
    #[allow(clippy::should_implement_trait)] // `Select::from("users")` reads like SQL; it is not a conversion.
    pub fn from(table: impl Into<String>) -> Self {
        Self { table: table.into(), columns: Vec::new(), conditions: Vec::new(), limit: None }
    }

    /// Adds columns to read, in the order the [`Row`]s list them.
    pub fn columns(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.columns.extend(columns.into_iter().map(Into::into));
        self
    }

    /// Keeps only rows where `column <op> value`. Several filters are combined with AND.
    pub fn filter(mut self, column: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        self.conditions.push(Condition::new(column, op, value));
        self
    }

    /// Reads at most `n` rows (default 1,000; the Manager lowers anything above 10,000 to 10,000).
    pub fn limit(mut self, n: u32) -> Self {
        self.limit = Some(n);
        self
    }

    /// Runs the query (permission `sql:read:<table>`).
    pub fn run(&self) -> Result<Vec<Row>> {
        if self.columns.is_empty() {
            return Err(Error::denied("list the columns to read (there is no 'every column')"));
        }
        if let Some(reason) = conditions_refusal(&self.conditions) {
            return Err(Error::denied(reason));
        }
        let rows = host_result(host::sql_select(&self.to_wit()))?;
        let columns: Arc<[String]> = rows.columns.into();
        Ok(rows
            .rows
            .into_iter()
            .map(|values| Row { columns: columns.clone(), values: values.into_iter().map(from_wit).collect() })
            .collect())
    }

    fn to_wit(&self) -> wit::Select {
        wit::Select {
            table: self.table.clone(),
            columns: self.columns.clone(),
            conditions: conditions_to_wit(&self.conditions),
            limit: self.limit,
        }
    }
}

/// One row of a [`Select`] result.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    columns: Arc<[String]>,
    values: Vec<Value>,
}

impl Row {
    /// The value of `column` as a `T`: [`i64`], [`f64`], [`String`], `Vec<u8>`, [`Value`], or an [`Option`] of
    /// one of them for a column that can be `NULL`.
    ///
    /// Fails (kind [`Other`](ErrorKind::Other)) if the query did not read `column`, or if the value is of another
    /// type, for example `NULL` read as `i64` or text read as `i64`. No conversion is made: an integer column
    /// reads as `i64` only, and `DECIMAL`, date and time columns arrive as text.
    pub fn get<T: FromValue>(&self, column: &str) -> Result<T> {
        let value = self.value(column).ok_or_else(|| {
            Error::new(ErrorKind::Other, format!("the row has no column '{column}' (read it in the query's columns)"))
        })?;
        T::from_value(value).map_err(|expected| {
            let hint = if *value == Value::Null { " (read it as an Option)" } else { "" };
            Error::new(ErrorKind::Other, format!("column '{column}' holds {}, not {expected}{hint}", value.describe()))
        })
    }

    /// The value of `column`, if the query read it. Column names are matched exactly first, then ignoring ASCII
    /// case.
    pub fn value(&self, column: &str) -> Option<&Value> {
        let i = self
            .columns
            .iter()
            .position(|c| c == column)
            .or_else(|| self.columns.iter().position(|c| c.eq_ignore_ascii_case(column)))?;
        self.values.get(i)
    }

    /// The column names, in the query's order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The values, in the order of [`columns`](Row::columns).
    pub fn values(&self) -> &[Value] {
        &self.values
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A type [`Row::get`] can read: [`i64`], [`f64`], [`String`], `Vec<u8>`, [`Value`] and [`Option`]s of them.
///
/// Sealed: it cannot be implemented outside the SDK.
pub trait FromValue: sealed::Sealed + Sized {
    /// The value as `Self`, or what was expected (`"an integer"`, ...) for the error message.
    #[doc(hidden)]
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str>;
}

impl sealed::Sealed for i64 {}
impl FromValue for i64 {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        match value {
            Value::Int(v) => Ok(*v),
            _ => Err("an integer"),
        }
    }
}

impl sealed::Sealed for f64 {}
impl FromValue for f64 {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        match value {
            Value::Float(v) => Ok(*v),
            _ => Err("a float"),
        }
    }
}

impl sealed::Sealed for String {}
impl FromValue for String {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        match value {
            Value::Text(v) => Ok(v.clone()),
            _ => Err("text"),
        }
    }
}

impl sealed::Sealed for Vec<u8> {}
impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        match value {
            Value::Bytes(v) => Ok(v.clone()),
            _ => Err("bytes"),
        }
    }
}

impl sealed::Sealed for Value {}
impl FromValue for Value {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        Ok(value.clone())
    }
}

impl<T: FromValue> sealed::Sealed for Option<T> {}
impl<T: FromValue> FromValue for Option<T> {
    /// `NULL` is `None`.
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        match value {
            Value::Null => Ok(None),
            v => T::from_value(v).map(Some),
        }
    }
}

/// An insert, made with [`insert`].
#[derive(Debug, Clone, PartialEq)]
#[must_use = "an operation does nothing until it is passed to sql::write"]
pub struct Insert {
    table: String,
    values: Vec<(String, Value)>,
}

impl Insert {
    /// Sets `column` to `value` in the new row.
    pub fn value(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.values.push((column.into(), value.into()));
        self
    }
}

/// An update, made with [`update`]. It needs at least one [`filter`](Update::filter).
#[derive(Debug, Clone, PartialEq)]
#[must_use = "an operation does nothing until it is passed to sql::write"]
pub struct Update {
    table: String,
    set: Vec<(String, Value)>,
    conditions: Vec<Condition>,
}

impl Update {
    /// Sets `column` to `value` in every matching row.
    pub fn set(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set.push((column.into(), value.into()));
        self
    }

    /// Changes only rows where `column <op> value`. Several filters are combined with AND.
    pub fn filter(mut self, column: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        self.conditions.push(Condition::new(column, op, value));
        self
    }
}

/// A delete, made with [`delete`]. It needs at least one [`filter`](Delete::filter).
#[derive(Debug, Clone, PartialEq)]
#[must_use = "an operation does nothing until it is passed to sql::write"]
pub struct Delete {
    table: String,
    conditions: Vec<Condition>,
}

impl Delete {
    /// Deletes only rows where `column <op> value`. Several filters are combined with AND.
    pub fn filter(mut self, column: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        self.conditions.push(Condition::new(column, op, value));
        self
    }
}

/// A new row for `table` (permission `sql:write:<table>`); add its values with [`Insert::value`].
pub fn insert(table: impl Into<String>) -> Insert {
    Insert { table: table.into(), values: Vec::new() }
}

/// A change to rows of `table` (permission `sql:write:<table>`); add [`Update::set`] and [`Update::filter`].
pub fn update(table: impl Into<String>) -> Update {
    Update { table: table.into(), set: Vec::new(), conditions: Vec::new() }
}

/// A removal of rows of `table` (permission `sql:write:<table>`); add [`Delete::filter`].
pub fn delete(table: impl Into<String>) -> Delete {
    Delete { table: table.into(), conditions: Vec::new() }
}

/// One operation of a [`write()`] batch: an [`Insert`], [`Update`] or [`Delete`] converted with `.into()` (or
/// [`WriteOp::from`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WriteOp(OpKind);

#[derive(Debug, Clone, PartialEq)]
enum OpKind {
    Insert(Insert),
    Update(Update),
    Delete(Delete),
}

impl From<Insert> for WriteOp {
    fn from(op: Insert) -> Self {
        WriteOp(OpKind::Insert(op))
    }
}

impl From<Update> for WriteOp {
    fn from(op: Update) -> Self {
        WriteOp(OpKind::Update(op))
    }
}

impl From<Delete> for WriteOp {
    fn from(op: Delete) -> Self {
        WriteOp(OpKind::Delete(op))
    }
}

impl WriteOp {
    /// Why the Manager would refuse this operation for a reason the SDK can see without asking it.
    fn refusal(&self) -> Option<String> {
        match &self.0 {
            OpKind::Insert(i) => assignments_refusal(&i.values),
            OpKind::Update(u) => assignments_refusal(&u.set).or_else(|| {
                if u.conditions.is_empty() {
                    Some(format!("an update of '{}' needs at least one condition", u.table))
                } else {
                    conditions_refusal(&u.conditions)
                }
            }),
            OpKind::Delete(d) => {
                if d.conditions.is_empty() {
                    Some(format!("a delete from '{}' needs at least one condition", d.table))
                } else {
                    conditions_refusal(&d.conditions)
                }
            }
        }
    }

    fn to_wit(&self) -> wit::WriteOp {
        match &self.0 {
            OpKind::Insert(i) => wit::WriteOp::Insert(wit::InsertOp { table: i.table.clone(), values: pairs_to_wit(&i.values) }),
            OpKind::Update(u) => wit::WriteOp::Update(wit::UpdateOp {
                table: u.table.clone(),
                set: pairs_to_wit(&u.set),
                conditions: conditions_to_wit(&u.conditions),
            }),
            OpKind::Delete(d) => {
                wit::WriteOp::Delete(wit::DeleteOp { table: d.table.clone(), conditions: conditions_to_wit(&d.conditions) })
            }
        }
    }
}

/// Applies `ops` in one transaction once the user approves them in a preview (permission `sql:write:<table>` for
/// each table). Returns the number of rows changed.
///
/// See the [module docs](self) for the rules; the user saying no is [`ErrorKind::Rejected`].
pub fn write(ops: impl IntoIterator<Item = WriteOp>) -> Result<u64> {
    let ops = checked_batch(ops)?;
    host_result(host::sql_write(&ops))
}

/// The batch in the WIT types, or the refusal the Manager would give for an empty batch, more than
/// [`MAX_WRITE_OPS`] operations, or an update or delete without a condition.
fn checked_batch(ops: impl IntoIterator<Item = WriteOp>) -> Result<Vec<wit::WriteOp>> {
    let ops: Vec<WriteOp> = ops.into_iter().collect();
    if ops.is_empty() {
        return Err(Error::denied("an empty write batch"));
    }
    if ops.len() > MAX_WRITE_OPS {
        return Err(Error::denied(format!("more than {MAX_WRITE_OPS} operations in one batch")));
    }
    if let Some(reason) = ops.iter().find_map(WriteOp::refusal) {
        return Err(Error::denied(reason));
    }
    Ok(ops.iter().map(WriteOp::to_wit).collect())
}

/// The host's text for a NaN or infinite float (host sql.rs `value`).
const NOT_FINITE: &str = "a float value must be finite";

fn finite(v: &Value) -> bool {
    !matches!(v, Value::Float(f) if !f.is_finite())
}

/// The host's refusal of an insert's or update's values that the SDK can see: no column set, or a float that is
/// not finite.
fn assignments_refusal(list: &[(String, Value)]) -> Option<String> {
    if list.is_empty() {
        return Some("an insert or update must set at least one column".into());
    }
    list.iter().any(|(_, v)| !finite(v)).then(|| NOT_FINITE.into())
}

/// The host's refusal of conditions that the SDK can see: a float that is not finite (the value of `IsNull` and
/// `NotNull` is ignored, as on the host).
fn conditions_refusal(list: &[Condition]) -> Option<String> {
    list.iter()
        .any(|c| !matches!(c.op, Op::IsNull | Op::NotNull) && !finite(&c.value))
        .then(|| NOT_FINITE.into())
}

fn value_to_wit(v: &Value) -> wit::SqlValue {
    match v {
        Value::Null => wit::SqlValue::Null,
        Value::Int(i) => wit::SqlValue::Int(*i),
        Value::Float(f) => wit::SqlValue::Float(*f),
        Value::Text(s) => wit::SqlValue::Text(s.clone()),
        Value::Bytes(b) => wit::SqlValue::Bytes(b.clone()),
    }
}

fn from_wit(v: wit::SqlValue) -> Value {
    match v {
        wit::SqlValue::Null => Value::Null,
        wit::SqlValue::Int(i) => Value::Int(i),
        wit::SqlValue::Float(f) => Value::Float(f),
        wit::SqlValue::Text(s) => Value::Text(s),
        wit::SqlValue::Bytes(b) => Value::Bytes(b),
    }
}

fn op_to_wit(op: Op) -> wit::CompareOp {
    match op {
        Op::Eq => wit::CompareOp::Eq,
        Op::Ne => wit::CompareOp::Ne,
        Op::Lt => wit::CompareOp::Lt,
        Op::Le => wit::CompareOp::Le,
        Op::Gt => wit::CompareOp::Gt,
        Op::Ge => wit::CompareOp::Ge,
        Op::Like => wit::CompareOp::Like,
        Op::IsNull => wit::CompareOp::IsNull,
        Op::NotNull => wit::CompareOp::NotNull,
    }
}

fn conditions_to_wit(list: &[Condition]) -> Vec<wit::Condition> {
    list.iter()
        .map(|c| wit::Condition { column: c.column.clone(), op: op_to_wit(c.op), value: value_to_wit(&c.value) })
        .collect()
}

fn pairs_to_wit(list: &[(String, Value)]) -> Vec<(String, wit::SqlValue)> {
    list.iter().map(|(c, v)| (c.clone(), value_to_wit(v))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WIT types have no `PartialEq`; their `Debug` text shows every field.
    fn same<T: std::fmt::Debug>(actual: &T, expected: &T) {
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }

    fn cond(column: &str, op: wit::CompareOp, value: wit::SqlValue) -> wit::Condition {
        wit::Condition { column: column.into(), op, value }
    }

    #[test]
    fn select_builder_produces_expected_query() {
        let q = Select::from("users")
            .columns(["id", "username"])
            .columns(vec!["credits".to_string()])
            .filter("rank", Op::Ge, 5)
            .filter("username", Op::Like, "a%")
            .filter("mail", Op::IsNull, Value::Null)
            .limit(50)
            .to_wit();
        assert_eq!(q.table, "users");
        assert_eq!(q.columns, ["id", "username", "credits"]);
        same(
            &q.conditions,
            &vec![
                cond("rank", wit::CompareOp::Ge, wit::SqlValue::Int(5)),
                cond("username", wit::CompareOp::Like, wit::SqlValue::Text("a%".into())),
                cond("mail", wit::CompareOp::IsNull, wit::SqlValue::Null),
            ]
        );
        assert_eq!(q.limit, Some(50));

        let q = Select::from("t").columns(["a"]).to_wit();
        assert!(q.conditions.is_empty());
        assert_eq!(q.limit, None, "no limit means the Manager's default (1,000)");
    }

    #[test]
    fn select_without_columns_is_refused_before_the_host() {
        let error = Select::from("users").run().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Denied);
        assert_eq!(error.message(), "permission denied: list the columns to read (there is no 'every column')");

        let error = Select::from("t").columns(["a"]).filter("b", Op::Gt, f64::NAN).run().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Denied);
        assert_eq!(error.message(), "permission denied: a float value must be finite");
        // The value of IS NULL is ignored: nothing to refuse (not run, it would call the host).
        assert!(conditions_refusal(&[Condition::new("b", Op::IsNull, f64::NAN)]).is_none());
    }

    #[test]
    fn every_op_converts() {
        let ops = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge, Op::Like, Op::IsNull, Op::NotNull];
        let wit_ops = [
            wit::CompareOp::Eq,
            wit::CompareOp::Ne,
            wit::CompareOp::Lt,
            wit::CompareOp::Le,
            wit::CompareOp::Gt,
            wit::CompareOp::Ge,
            wit::CompareOp::Like,
            wit::CompareOp::IsNull,
            wit::CompareOp::NotNull,
        ];
        for (op, w) in ops.into_iter().zip(wit_ops) {
            assert_eq!(op_to_wit(op), w);
        }
    }

    #[test]
    fn write_builders_produce_expected_ops() {
        let ops = checked_batch([
            insert("pages").value("caption", "Plants").value("visible", true).value("icon", None::<i64>).into(),
            update("users").set("credits", 100u32).set("motto", String::from("hi")).filter("id", Op::Eq, 7).into(),
            WriteOp::from(delete("bans").filter("user_id", Op::Eq, 7i64).filter("expires", Op::Lt, 2.5)),
        ])
        .unwrap();
        same(
            &ops,
            &vec![
                wit::WriteOp::Insert(wit::InsertOp {
                    table: "pages".into(),
                    values: vec![
                        ("caption".into(), wit::SqlValue::Text("Plants".into())),
                        ("visible".into(), wit::SqlValue::Int(1)),
                        ("icon".into(), wit::SqlValue::Null),
                    ],
                }),
                wit::WriteOp::Update(wit::UpdateOp {
                    table: "users".into(),
                    set: vec![
                        ("credits".into(), wit::SqlValue::Int(100)),
                        ("motto".into(), wit::SqlValue::Text("hi".into())),
                    ],
                    conditions: vec![cond("id", wit::CompareOp::Eq, wit::SqlValue::Int(7))],
                }),
                wit::WriteOp::Delete(wit::DeleteOp {
                    table: "bans".into(),
                    conditions: vec![
                        cond("user_id", wit::CompareOp::Eq, wit::SqlValue::Int(7)),
                        cond("expires", wit::CompareOp::Lt, wit::SqlValue::Float(2.5)),
                    ],
                }),
            ]
        );
    }

    #[test]
    fn write_refuses_what_the_host_would_refuse() {
        let refused = |ops: Vec<WriteOp>| {
            let error = checked_batch(ops).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Denied);
            error.message().to_string()
        };
        assert_eq!(refused(vec![]), "permission denied: an empty write batch");
        assert_eq!(
            refused(vec![update("users").set("credits", 1).into()]),
            "permission denied: an update of 'users' needs at least one condition"
        );
        assert_eq!(
            refused(vec![insert("a").value("x", 1).into(), delete("users").into()]),
            "permission denied: a delete from 'users' needs at least one condition"
        );
        // The host's texts (host sql.rs `assignments`, `value`).
        let no_column = "permission denied: an insert or update must set at least one column";
        assert_eq!(refused(vec![insert("pages").into()]), no_column);
        assert_eq!(refused(vec![update("users").filter("id", Op::Eq, 1).into()]), no_column);
        let not_finite = "permission denied: a float value must be finite";
        assert_eq!(refused(vec![insert("a").value("x", f64::NAN).into()]), not_finite);
        assert_eq!(refused(vec![update("a").set("x", f64::INFINITY).filter("y", Op::Eq, 1).into()]), not_finite);
        assert_eq!(refused(vec![update("a").set("x", 1).filter("y", Op::Lt, f64::INFINITY).into()]), not_finite);
        assert_eq!(refused(vec![delete("a").filter("y", Op::Eq, f32::NEG_INFINITY).into()]), not_finite);
        // The value of IS NULL / IS NOT NULL is ignored, as on the host.
        checked_batch([delete("a").filter("y", Op::IsNull, f64::NAN).into()]).unwrap();

        let many: Vec<WriteOp> = (0..=MAX_WRITE_OPS).map(|i| insert("a").value("x", i as i64).into()).collect();
        assert_eq!(refused(many), "permission denied: more than 1000 operations in one batch");

        let full: Vec<WriteOp> = (0..MAX_WRITE_OPS).map(|i| insert("a").value("x", i as i64).into()).collect();
        assert_eq!(checked_batch(full).unwrap().len(), MAX_WRITE_OPS);
    }

    #[test]
    fn values_convert_from_rust_types() {
        assert_eq!(Value::from(-3i8), Value::Int(-3));
        assert_eq!(Value::from(u32::MAX), Value::Int(i64::from(u32::MAX)));
        assert_eq!(Value::from(false), Value::Int(0));
        assert_eq!(Value::from(1.5f32), Value::Float(1.5));
        assert_eq!(Value::from("a"), Value::Text("a".into()));
        assert_eq!(Value::from(&String::from("b")), Value::Text("b".into()));
        assert_eq!(Value::from(&[1u8, 2][..]), Value::Bytes(vec![1, 2]));
        assert_eq!(Value::from(vec![3u8]), Value::Bytes(vec![3]));
        assert_eq!(Value::from(Some("c")), Value::Text("c".into()));
        assert_eq!(Value::from(None::<&str>), Value::Null);
    }

    fn row() -> Row {
        Row {
            columns: vec!["id".to_string(), "name".into(), "score".into(), "avatar".into(), "motto".into()].into(),
            values: vec![
                Value::Int(7),
                Value::Text("ann".into()),
                Value::Float(0.5),
                Value::Bytes(vec![1, 2]),
                Value::Null,
            ],
        }
    }

    #[test]
    fn row_get_reads_typed_values() {
        let row = row();
        assert_eq!(row.get::<i64>("id").unwrap(), 7);
        assert_eq!(row.get::<String>("name").unwrap(), "ann");
        assert_eq!(row.get::<f64>("score").unwrap(), 0.5);
        assert_eq!(row.get::<Vec<u8>>("avatar").unwrap(), [1, 2]);
        assert_eq!(row.get::<Option<String>>("motto").unwrap(), None);
        assert_eq!(row.get::<Option<i64>>("id").unwrap(), Some(7));
        assert_eq!(row.get::<Value>("motto").unwrap(), Value::Null);
        assert_eq!(row.get::<i64>("ID").unwrap(), 7, "ASCII case is ignored after an exact miss");
        assert_eq!(row.columns().len(), 5);
        assert_eq!(row.values()[0], Value::Int(7));
    }

    #[test]
    fn row_get_explains_a_mismatch() {
        let row = row();
        let error = row.get::<i64>("name").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert_eq!(error.message(), "column 'name' holds text, not an integer");
        assert_eq!(row.get::<String>("motto").unwrap_err().message(), "column 'motto' holds NULL, not text (read it as an Option)");
        assert_eq!(row.get::<Option<f64>>("id").unwrap_err().message(), "column 'id' holds an integer, not a float");
        assert_eq!(
            row.get::<i64>("missing").unwrap_err().message(),
            "the row has no column 'missing' (read it in the query's columns)"
        );
    }
}
