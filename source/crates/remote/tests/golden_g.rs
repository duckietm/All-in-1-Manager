mod common;

use aio_remote::sql_script;
use aio_remote::sql_session::{self, decode, display, ColumnKind, SqlColumn, SqlValue};
use common::compare;

// The same scripts as tools/parity/Scenarios_G.cs (the golden files repeat each input, so a copy that drifted shows).
const SQL_SCRIPTS: &[(&str, &str)] = &[
    ("simple", "SELECT 1;\nSELECT 2;\n"),
    ("semicolon_in_strings", "SELECT 'a;b', \"c;d\";\nINSERT INTO t VALUES ('x;y');"),
    ("escaped_quotes", "SELECT 'it\\'s; ok', 'double '' quote;', \"say \\\"hi;\\\"\";\nSELECT 2"),
    ("backticks", "SELECT `we;ird``name` FROM `t;1`;\nSELECT `a\\`;\nSELECT 3;"),
    ("line_comments", "-- first comment; with semicolon\nSELECT 1; -- trailing; comment\n# hash; comment\nSELECT 2;\nSELECT 3 --not a comment\n;"),
    ("block_comments", "/* header; comment\n spanning lines */\nSELECT 1;\nSELECT /* inline; */ 2;\n/*!40101 SET NAMES utf8 */;\n/* only comment */;\nSELECT 3;"),
    ("delimiter_procedure", "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 'x$$y';\nEND$$\nDELIMITER ;\nCALL p();\n"),
    ("delimiter_trigger", "delimiter //\nCREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW\nBEGIN\n  SET NEW.a = 1;\nEND //\ndelimiter ;\nINSERT INTO t VALUES (1);"),
    ("delimiter_tab_crlf", "DELIMITER\t;;\r\nSELECT 1;;\r\nSELECT 2;;\r\nDELIMITER ;\r\nSELECT 3;\r\n"),
    ("delimiter_not_at_start", "SELECT 1 DELIMITER $$;\nSELECT 2;"),
    ("delimiter_empty_value", "DELIMITER \nSELECT 1;"),
    ("delimiter_word_last", "DELIMITER $$\nSELECT 1$$ SELECT 2 $$\nDELIMITER ;\nSELECT 3; DELIMITER"),
    ("empty_statements", ";;;\n  ;\nSELECT 1;;;SELECT 2;\n\n;"),
    ("crlf", "SELECT 1;\r\nSELECT\r\n  2;\r\n\r\nSELECT 3"),
    ("multiline", "INSERT INTO t (a, b)\nVALUES\n  (1, 'one\ntwo'),\n  (2, 'three');\n\nUPDATE t\nSET b = 'x'\nWHERE a = 1;"),
    ("backslash_newline_in_string", "SELECT 'a\\\nb';\nSELECT 2;"),
    ("unterminated_string", "SELECT 1;\nSELECT 'never closed;\nSELECT 2;"),
    ("unterminated_comment", "SELECT 1;\n/* never closed;\nSELECT 2;"),
    ("dashes", "SELECT 5--3;\nSELECT 5 - -3;\nSELECT 1 --\nSELECT 2;\n--"),
    ("unicode_whitespace", "\u{A0}SELECT 'é;ü';\u{A0}\nSELECT 2;\u{3000}"),
    ("comment_then_statement", "/* c */SELECT 1;\n# x\n  SELECT 2;"),
    ("trailing_comment", "SELECT 1\n-- done"),
    ("doubled_double_quote", "SELECT \"a\"\"b;\";SELECT 'end'"),
    ("only_comments", "-- a\n# b\n/* c */\n"),
];

fn show(text: &str) -> String {
    text.replace('\r', "<CR>").replace('\t', "<TAB>")
}

fn render_split(script: &str) -> String {
    let mut text = format!("=== input\n{}\n=== statements\n", show(script));
    for statement in sql_script::split(script) {
        text += &format!("--- line {}\n{}\n", statement.line, show(&statement.text));
    }
    text
}

#[test]
fn sql_script_split() {
    for (name, script) in SQL_SCRIPTS {
        compare(&format!("sql_script_{name}"), &render_split(script));
    }
}

// The C# scenarios pass .NET values (DateTime, TimeSpan, double ...); here the same values come from the text
// protocol's bytes, as MariaDB sends them, through decode.
#[test]
fn sql_session_display() {
    let mut out = String::new();
    let mut d = |name: &str, value: SqlValue, data_type: Option<&str>, max_bytes: usize| {
        let column = data_type.map(|t| SqlColumn { name: "c".into(), data_type: t.into() });
        out += &format!("{name}: {}\n", display(&value, column.as_ref(), max_bytes).unwrap_or_else(|| "<null>".into()));
    };
    let text = |kind: ColumnKind, text: &str| decode(kind, text.as_bytes());
    let hundred: Vec<u8> = (0..100u8).collect();
    d("null", SqlValue::Null, None, 64);
    d("bytes_short", decode(ColumnKind::Bytes, &[0x00, 0xAB, 0xFF]), Some("VARBINARY"), 64);
    d("bytes_cut", decode(ColumnKind::Bytes, &hundred), Some("BLOB"), 64);
    d("bytes_full", decode(ColumnKind::Bytes, &hundred), Some("BLOB"), usize::MAX);
    d("bytes_empty", decode(ColumnKind::Bytes, &[]), Some("BLOB"), 64);
    d("datetime", text(ColumnKind::DateTime, "2024-01-02 03:04:05"), Some("DATETIME"), 64);
    d("datetime_fraction", text(ColumnKind::DateTime, "2024-01-02 03:04:05.120"), Some("DATETIME"), 64);
    d("datetime_zero_fraction", text(ColumnKind::DateTime, "2024-01-02 03:04:05.000"), Some("DATETIME"), 64);
    d("datetime_micro", text(ColumnKind::DateTime, "2024-01-02 03:04:05.000001"), Some("TIMESTAMP"), 64);
    d("date_column", text(ColumnKind::Date, "2024-01-02"), Some("DATE"), 64);
    d("zero_datetime", text(ColumnKind::DateTime, "0000-00-00 00:00:00"), Some("DATETIME"), 64);
    d("zero_date", text(ColumnKind::Date, "0000-00-00"), Some("DATE"), 64);
    d("mysql_datetime", text(ColumnKind::DateTime, "2024-05-06 07:08:09.123456"), Some("DATETIME"), 64);
    d("mysql_date", text(ColumnKind::Date, "2024-05-06"), Some("DATE"), 64);
    d("time_long", text(ColumnKind::Time, "838:59:59"), Some("TIME"), 64);
    d("time_negative", text(ColumnKind::Time, "-01:02:03"), Some("TIME"), 64);
    d("time_fraction", text(ColumnKind::Time, "12:00:00.500000"), Some("TIME"), 64);
    let doubles = [
        "1.5", "0.30000000000000004", "1e15", "1e16", "1e20", "12345678901234567890", "0.0001", "0.00001", "-2.5e-7",
        "123456789012345", "0", "100", "1e-300", "1.7976931348623157e308", "-1234.5678",
    ];
    for (i, v) in doubles.iter().enumerate() {
        d(&format!("double_{i}"), text(ColumnKind::Double, v), Some("DOUBLE"), 64);
    }
    let floats = ["3.14159", "1.1", "1e10", "1e7", "1e8", "1234567", "12345678", "0.5", "1e-5", "0.0001", "-7.25"];
    for (i, v) in floats.iter().enumerate() {
        d(&format!("float_{i}"), text(ColumnKind::Float, v), Some("FLOAT"), 64);
    }
    d("decimal", text(ColumnKind::Decimal, "1.50"), Some("DECIMAL"), 64);
    d("decimal_negative", text(ColumnKind::Decimal, "-0.001"), Some("DECIMAL"), 64);
    d("long", text(ColumnKind::Int, "-42"), Some("BIGINT"), 64);
    d("ulong_max", text(ColumnKind::UInt, "18446744073709551615"), Some("BIGINT"), 64);
    d("sbyte", text(ColumnKind::Int, "-1"), Some("TINYINT"), 64);
    d("int", text(ColumnKind::Int, "2147483647"), Some("INT"), 64);
    d("year", text(ColumnKind::Int, "2024"), Some("YEAR"), 64);
    d("string", text(ColumnKind::Text, "héllo, wörld"), Some("VARCHAR"), 64);
    d("bit", decode(ColumnKind::Bit, &[0x01, 0x02]), Some("BIT"), 64);
    compare("sql_session_display", &out);
}

#[test]
fn sql_session_misc() {
    let mut out = String::new();
    for t in ["BLOB", "VARBINARY", "binary", "VARCHAR", "GEOMETRY", "LONGBLOB", "TEXT", "INT"] {
        let binary = SqlColumn { name: "c".into(), data_type: t.into() }.is_binary();
        out += &format!("is_binary {t}: {}\n", if binary { "True" } else { "False" });
    }
    for name in ["users", "we`ird", "", "a``b"] {
        out += &format!("quote {name}: {}\n", sql_session::quote(name));
    }
    for columns in [vec!["id"], vec!["user_id", "room`id", "slot"]] {
        out += &format!("key_condition {}: {}\n", columns.join(","), sql_session::key_condition(&columns));
    }
    compare("sql_session_misc", &out);
}
