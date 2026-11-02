//! Pure helpers of the SQL editor: sizes, numbers, the run summary and line positions (SqlEditorView.cs).

use aio_remote::sql_session::{self, SqlColumn, SqlRun, SqlValue, MAX_QUERY_ROWS};

use crate::i18n::{t, tf};

/// What the grids show for NULL.
pub const NULL_TEXT: &str = "(NULL)";

/// The C# Size: "1.5 GB", "2.0 MB", "12 KB"; None without a size.
pub fn size(bytes: Option<i64>) -> Option<String> {
    let bytes = bytes?;
    let (gb, mb) = ((1i64 << 30) as f64, (1i64 << 20) as f64);
    Some(if bytes >= 1 << 30 {
        format!("{:.1} GB", round_away(bytes as f64 / gb, 1))
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", round_away(bytes as f64 / mb, 1))
    } else {
        format!("{:.0} KB", round_away(bytes as f64 / 1024.0, 0))
    })
}

/// Rounds half away from zero, like .NET's custom number formats.
fn round_away(value: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (value * factor).round() / factor
}

/// A number with thousands separators (the C# "N0").
pub fn n0(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// The first 60 characters and "…" (the C# Shorten).
pub fn shorten(text: &str) -> String {
    if text.chars().count() > 60 { text.chars().take(60).collect::<String>() + "…" } else { text.to_string() }
}

/// A cell as a grid shows it.
pub fn cell(value: &SqlValue, column: &SqlColumn) -> String {
    sql_session::display(value, Some(column), 64).unwrap_or_else(|| NULL_TEXT.to_string())
}

/// A cell in full, for copying and CSV; None is NULL.
pub fn full(value: &SqlValue, column: &SqlColumn) -> Option<String> {
    sql_session::display(value, Some(column), usize::MAX)
}

fn newline() -> &'static str {
    if cfg!(windows) { "\r\n" } else { "\n" }
}

/// "Copy rows (with column names)": tab separated, NULL as "NULL".
pub fn copy_rows(columns: &[SqlColumn], rows: &[&Vec<SqlValue>]) -> String {
    let mut text = columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("\t");
    text += newline();
    for row in rows {
        let fields: Vec<String> =
            row.iter().zip(columns).map(|(v, c)| full(v, c).unwrap_or_else(|| "NULL".to_string())).collect();
        text += &fields.join("\t");
        text += newline();
    }
    text
}

/// Did MariaDB refuse the account (ER_ACCESS_DENIED_ERROR 1045)?
pub fn is_access_denied(message: &str) -> bool {
    message.contains("Access denied") || message.contains("1045")
}

/// The character range (start, end) of a line (from 1) of `text`, without its line end; None when there is no such line.
pub fn line_range(text: &str, line: usize) -> Option<(usize, usize)> {
    if line < 1 {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    for _ in 1..line {
        let next = chars[index..].iter().position(|&c| c == '\n')?;
        index += next + 1;
    }
    let end = chars[index..].iter().position(|&c| c == '\n').map(|p| index + p).unwrap_or(chars.len());
    Some((index, end))
}

/// The header of a result tab: "Result 1 (1,234 rows)".
pub fn result_header(number: usize, rows: usize, truncated: bool) -> String {
    let count = format!("{}{}", n0(rows as i64), if truncated { "+" } else { "" });
    if rows == 1 {
        tf("Result {number} ({rows} row)", &[("number", &number.to_string()), ("rows", &count)])
    } else {
        tf("Result {number} ({rows} rows)", &[("number", &number.to_string()), ("rows", &count)])
    }
}

/// The Messages text of a run (the C# ShowResults); its first line is the summary.
pub fn run_messages(run: &SqlRun) -> String {
    let mut notes = String::new();
    let mut number = 0;
    let mut affected = 0u64;
    for result in &run.results {
        if result.columns.is_none() {
            affected += result.affected;
            continue;
        }
        number += 1;
        if result.truncated {
            notes += &tf(
                "Result {number}: only the first {max} rows are shown; add a LIMIT to see others.",
                &[("number", &number.to_string()), ("max", &n0(MAX_QUERY_ROWS as i64))],
            );
            notes += "\n";
        }
    }
    let mut statements: Vec<&str> = run.results.iter().map(|r| r.statement.as_str()).collect();
    statements.sort_unstable();
    statements.dedup();
    let done = statements.len().to_string();
    let total = run.statements.to_string();
    let mut first = if run.statements == 1 {
        tf("{done} of {total} statement executed", &[("done", &done), ("total", &total)])
    } else {
        tf("{done} of {total} statements executed", &[("done", &done), ("total", &total)])
    };
    if affected > 0 {
        let n = n0(affected as i64);
        first += &if affected == 1 { tf(", {n} row affected", &[("n", &n)]) } else { tf(", {n} rows affected", &[("n", &n)]) };
    }
    if let Some(max) = run.results.iter().map(|r| r.elapsed).max() {
        first += &format!(" · {:.3} s", max.as_secs_f64());
    }
    first += ".";
    let mut messages = format!("{first}\n{notes}");
    if let Some(error) = &run.error {
        messages += error;
        messages += "\n";
    }
    messages.trim_end().to_string()
}

/// The status line of an opened table: "hotel.users: Rows 1–1,000 · sorted by id · 0.012 s · ...".
#[allow(clippy::too_many_arguments)]
pub fn data_status(database: &str, table: &str, rows: usize, offset: i64, order_by: Option<&str>, descending: bool, seconds: f64, editable: bool, is_view: bool) -> String {
    let range = if rows == 0 {
        t("No rows").to_string()
    } else {
        tf("Rows {from}–{to}", &[("from", &n0(offset + 1)), ("to", &n0(offset + rows as i64))])
    };
    let order = match order_by {
        None => String::new(),
        Some(column) if descending => tf(" · sorted by {column} (descending)", &[("column", column)]),
        Some(column) => tf(" · sorted by {column}", &[("column", column)]),
    };
    let edit = if editable {
        t(" · Double-click a cell to change it (Enter saves, Esc cancels); right-click for NULL, copy and delete.")
    } else if is_view {
        t(" · A view: read-only.")
    } else {
        t(" · No primary key: read-only (change rows with a query).")
    };
    format!("{database}.{table}: {range}{order} · {seconds:.3} s{edit}")
}

/// The C# ValueComparer: NULL first, then numbers as numbers, else the shown text.
pub fn compare(a: &SqlValue, b: &SqlValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    use SqlValue::*;
    match (a, b) {
        (Null, Null) => Ordering::Equal,
        (Null, _) => Ordering::Less,
        (_, Null) => Ordering::Greater,
        (Int(x), Int(y)) => x.cmp(y),
        (UInt(x), UInt(y)) => x.cmp(y),
        (Text(x), Text(y)) | (Date(x), Date(y)) | (DateTime(x), DateTime(y)) | (Time(x), Time(y)) => x.cmp(y),
        _ => match (number(a), number(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => sql_session::display(a, None, 64).cmp(&sql_session::display(b, None, 64)),
        },
    }
}

fn number(value: &SqlValue) -> Option<f64> {
    match value {
        SqlValue::Int(v) => Some(*v as f64),
        SqlValue::UInt(v) => Some(*v as f64),
        SqlValue::Float(v) => Some(*v as f64),
        SqlValue::Double(v) => Some(*v),
        SqlValue::Decimal(v) => v.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aio_remote::sql_session::SqlResult;
    use std::time::Duration;

    #[test]
    fn sizes() {
        assert_eq!(size(None), None);
        assert_eq!(size(Some(16384)).as_deref(), Some("16 KB"));
        assert_eq!(size(Some(1536)).as_deref(), Some("2 KB"));
        assert_eq!(size(Some(0)).as_deref(), Some("0 KB"));
        assert_eq!(size(Some(3 << 20)).as_deref(), Some("3.0 MB"));
        assert_eq!(size(Some((1 << 30) + (1 << 29))).as_deref(), Some("1.5 GB"));
    }

    #[test]
    fn thousands() {
        assert_eq!(n0(0), "0");
        assert_eq!(n0(999), "999");
        assert_eq!(n0(1000), "1,000");
        assert_eq!(n0(1234567), "1,234,567");
        assert_eq!(n0(-10000), "-10,000");
    }

    #[test]
    fn shortens() {
        assert_eq!(shorten("abc"), "abc");
        let long = "x".repeat(61);
        assert_eq!(shorten(&long), format!("{}…", "x".repeat(60)));
    }

    #[test]
    fn lines() {
        let text = "SELECT 1;\nSELECT x;\nSELECT 3";
        assert_eq!(line_range(text, 1), Some((0, 9)));
        assert_eq!(line_range(text, 2), Some((10, 19)));
        assert_eq!(line_range(text, 3), Some((20, 28)));
        assert_eq!(line_range(text, 4), None);
        assert_eq!(line_range(text, 0), None);
        assert_eq!(line_range("é\nb", 2), Some((2, 3)));
    }

    fn result(statement: &str, columns: bool, rows: usize, affected: u64, ms: u64) -> SqlResult {
        SqlResult {
            statement: statement.into(),
            columns: columns.then(|| vec![SqlColumn { name: "a".into(), data_type: "INT".into() }]),
            rows: vec![vec![SqlValue::Int(1)]; rows],
            truncated: false,
            affected,
            elapsed: Duration::from_millis(ms),
        }
    }

    #[test]
    fn messages() {
        let run = SqlRun {
            results: vec![result("SELECT 1", true, 1, 0, 5), result("UPDATE x", false, 0, 3, 12)],
            statements: 3,
            error: Some("SQL Error (1064) in statement 3 (line 3): bad".into()),
            error_line: 3,
            cancelled: false,
        };
        assert_eq!(run_messages(&run), "2 of 3 statements executed, 3 rows affected · 0.012 s.\nSQL Error (1064) in statement 3 (line 3): bad");
        let one = SqlRun { results: vec![], statements: 1, error: None, error_line: 0, cancelled: false };
        assert_eq!(run_messages(&one), "0 of 1 statement executed.");
        let mut big = result("SELECT *", true, 2, 0, 1);
        big.truncated = true;
        let run = SqlRun { results: vec![big], statements: 1, error: None, error_line: 0, cancelled: false };
        assert_eq!(run_messages(&run), "1 of 1 statement executed · 0.001 s.\nResult 1: only the first 10,000 rows are shown; add a LIMIT to see others.");
        assert_eq!(result_header(1, 10000, true), "Result 1 (10,000+ rows)");
        assert_eq!(result_header(2, 1, false), "Result 2 (1 row)");
    }

    #[test]
    fn status_line() {
        assert_eq!(
            data_status("hotel", "users", 1000, 1000, Some("id"), true, 0.0123, false, true),
            "hotel.users: Rows 1,001–2,000 · sorted by id (descending) · 0.012 s · A view: read-only."
        );
        assert!(data_status("h", "t", 0, 0, None, false, 0.0, false, false).starts_with("h.t: No rows · 0.000 s · No primary key"));
    }

    #[test]
    fn comparing() {
        use std::cmp::Ordering::*;
        assert_eq!(compare(&SqlValue::Null, &SqlValue::Int(1)), Less);
        assert_eq!(compare(&SqlValue::Int(2), &SqlValue::Int(10)), Less);
        assert_eq!(compare(&SqlValue::Decimal("10.5".into()), &SqlValue::Int(9)), Greater);
        assert_eq!(compare(&SqlValue::Text("b".into()), &SqlValue::Text("a".into())), Greater);
    }

    #[test]
    fn copying() {
        let columns = vec![SqlColumn { name: "a".into(), data_type: "INT".into() }, SqlColumn { name: "b".into(), data_type: "BLOB".into() }];
        let row = vec![SqlValue::Null, SqlValue::Bytes(vec![1, 255])];
        let nl = newline();
        assert_eq!(copy_rows(&columns, &[&row]), format!("a\tb{nl}NULL\t0x01FF{nl}"));
        assert!(is_access_denied("ERROR 1045 (28000): Access denied for user 'root'@'localhost'"));
        assert!(!is_access_denied("Connection refused"));
    }
}
