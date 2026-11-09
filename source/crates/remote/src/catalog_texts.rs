//! The catalogue's page texts (caption, page_text1, page_text2, page_text_details, page_text_teaser of catalog_pages and
//! catalog_pages_bc) in another language. The translations are dictionaries from the Dutch text (as in the packed
//! catalogue) to the text in a language, maintained with source/tools/catalog_texts.py. A text in the database is
//! recognised in any language of the dictionaries, so a hotel can switch from one language to any other; a text that
//! is in no dictionary (the hotel's own) is left alone.

use std::collections::HashMap;

use crate::script::{self, fill};
use crate::server::{Log, Result, Server, ServerError};
use crate::server_tools;

pub const TABLES: [&str; 2] = ["catalog_pages", "catalog_pages_bc"];
pub const COLUMNS: [&str; 5] = ["caption", "page_text1", "page_text2", "page_text_details", "page_text_teaser"];

/// The language the dictionaries translate from.
pub const SOURCE: &str = "nl";

/// One text of one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    pub table: String,
    pub id: i64,
    pub column: String,
    pub text: String,
}

/// Dutch text -> text in one language.
pub type Dictionary = HashMap<String, String>;

// HEX keeps line breaks, tabs and every other character intact through the command line.
const READ: &str = r#"{{DatabaseName}}
for table in catalog_pages catalog_pages_bc; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'")" = 1 ] || continue
  echo "@$table"
  mariadb -N -B "$db" -e "SELECT id, HEX(caption), HEX(page_text1), HEX(page_text2), HEX(page_text_details), HEX(page_text_teaser) FROM $table"
done"#;

/// The page texts of both tables (a table the database does not have is left out).
pub async fn read(server: &dyn Server) -> Result<Vec<PageText>> {
    let result = server.run_as_root(&fill(READ, &[("DatabaseName", &server_tools::database_name())])).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("Reading the catalogue pages failed: {}", result.error.trim())).into());
    }
    Ok(parse(&result.output))
}

fn unhex(hex: &str) -> Option<String> {
    if hex == "NULL" || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

fn parse(output: &str) -> Vec<PageText> {
    let mut texts = Vec::new();
    let mut table = String::new();
    for line in output.lines() {
        if let Some(name) = line.strip_prefix('@') {
            table = name.to_string();
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 1 + COLUMNS.len() || table.is_empty() {
            continue;
        }
        let Ok(id) = fields[0].parse::<i64>() else { continue };
        for (column, hex) in COLUMNS.iter().zip(&fields[1..]) {
            if let Some(text) = unhex(hex).filter(|t| !t.trim().is_empty()) {
                texts.push(PageText { table: table.clone(), id, column: column.to_string(), text });
            }
        }
    }
    texts
}

/// The new text of every page text that is in the dictionaries and differs in `target`: (table, id, column, text).
/// `dictionaries` holds every language but the source one.
pub fn plan(texts: &[PageText], dictionaries: &[(&str, Dictionary)], target: &str) -> Vec<(String, i64, String, String)> {
    // Any text, in any language, back to its Dutch original; the Dutch texts first, so they win a tie.
    let mut original: HashMap<&str, &str> = HashMap::new();
    for (_, dictionary) in dictionaries {
        for key in dictionary.keys() {
            original.insert(key, key);
        }
    }
    for (_, dictionary) in dictionaries {
        for (key, translated) in dictionary {
            original.entry(translated).or_insert(key);
        }
    }
    let wanted = dictionaries.iter().find(|(code, _)| *code == target).map(|(_, d)| d);
    texts
        .iter()
        .filter_map(|t| {
            let key = *original.get(t.text.as_str())?;
            let new = if target == SOURCE { key } else { wanted?.get(key).map(String::as_str).filter(|v| !v.trim().is_empty())? };
            (new != t.text).then(|| (t.table.clone(), t.id, t.column.clone(), new.to_string()))
        })
        .collect()
}

// Line breaks as escapes too: the SQL goes to the server over stdin as text, which would lose a carriage return.
fn sql_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 2);
    text.push('\'');
    for c in value.chars() {
        match c {
            '\\' => text.push_str("\\\\"),
            '\'' => text.push_str("\\'"),
            '\0' => text.push_str("\\0"),
            '\r' => text.push_str("\\r"),
            '\n' => text.push_str("\\n"),
            c => text.push(c),
        }
    }
    text.push('\'');
    text
}

/// The changes as UPDATE statements per table and column, in batches of 100, in one transaction.
pub fn plan_sql(updates: &[(String, i64, String, String)]) -> String {
    let mut sql = String::from("SET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    for table in TABLES {
        for column in COLUMNS {
            let rows: Vec<&(String, i64, String, String)> = updates.iter().filter(|u| u.0 == table && u.2 == column).collect();
            for chunk in rows.chunks(100) {
                let cases: String = chunk.iter().map(|u| format!(" WHEN {} THEN {}", u.1, sql_text(&u.3))).collect();
                let ids: Vec<String> = chunk.iter().map(|u| u.1.to_string()).collect();
                sql += &format!("UPDATE {table} SET {column} = CASE id{cases} ELSE {column} END WHERE id IN ({});\n", ids.join(","));
            }
        }
    }
    sql += "COMMIT;\n";
    sql
}

const APPLY: &str = r#"{{DatabaseName}}
out=$(mktemp)
trap 'rm -f "$out"' EXIT
echo "> The catalogue's page texts ({{Count}} in {{Language}})"
# The UPDATE statements come over stdin, in one transaction: all or nothing.
if ! mariadb "$db" > "$out" 2>&1; then
  { grep -E '^ERROR' "$out" || tail -n 3 "$out"; } | head -n 5 | cut -c1-300 | sed 's/^/  /'
  fail "Translating the catalogue pages failed (see above); they are as they were."
fi
echo "  done""#;

/// Translates the catalogue pages into `target` (a language code): reads them, works out the changes and writes them
/// in one transaction. Meant for while the emulator is stopped (it keeps the pages in memory). Returns how many texts
/// changed.
pub async fn translate(server: &dyn Server, dictionaries: &[(&str, Dictionary)], target: &str, language_name: &str, log: Log<'_>) -> Result<usize> {
    let texts = read(server).await?;
    let updates = plan(&texts, dictionaries, target);
    let untranslated = texts.len() - updates.len();
    if updates.is_empty() {
        log(&format!("> The catalogue's page texts: nothing to change ({} texts)", texts.len()));
        return Ok(0);
    }
    let body = fill(APPLY, &[("DatabaseName", &server_tools::database_name()), ("Count", &updates.len().to_string()), ("Language", language_name)]);
    script::run(server, &script::with_prelude(&body), log, "Translating the catalogue pages failed", Some(&plan_sql(&updates))).await?;
    log(&format!("  {} of {} texts changed; the others are already right or the hotel's own", updates.len(), untranslated + updates.len()));
    Ok(updates.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, &str)]) -> Dictionary {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn text(table: &str, id: i64, column: &str, value: &str) -> PageText {
        PageText { table: table.into(), id, column: column.into(), text: value.into() }
    }

    #[test]
    fn reads_hex_with_line_breaks() {
        // "Meubels" and "a\nb" in HEX.
        let texts = parse("@catalog_pages\n2\t4D657562656C73\t610A62\tNULL\t\tNULL\nbad\n@catalog_pages_bc\n1\t4D657562656C73\tNULL\tNULL\tNULL\tNULL\n");
        assert_eq!(
            texts,
            vec![
                text("catalog_pages", 2, "caption", "Meubels"),
                text("catalog_pages", 2, "page_text1", "a\nb"),
                text("catalog_pages_bc", 1, "caption", "Meubels"),
            ]
        );
    }

    #[test]
    fn any_language_to_any_other() {
        let dictionaries = vec![
            ("en", dict(&[("Meubels", "Furni"), ("Voorpagina", "Front Page")])),
            ("de", dict(&[("Meubels", "Möbel")])),
        ];
        let texts = vec![
            text("catalog_pages", 1, "caption", "Voorpagina"),
            text("catalog_pages", 2, "caption", "Furni"),
            text("catalog_pages", 3, "caption", "Mijn eigen pagina"),
            text("catalog_pages_bc", 4, "caption", "Meubels"),
        ];
        let to_german = plan(&texts, &dictionaries, "de");
        assert_eq!(
            to_german,
            vec![("catalog_pages".into(), 2, "caption".into(), "Möbel".into()), ("catalog_pages_bc".into(), 4, "caption".into(), "Möbel".into())],
            "Voorpagina has no German yet and stays; the hotel's own page is never touched"
        );
        let to_dutch = plan(&texts, &dictionaries, "nl");
        assert_eq!(to_dutch, vec![("catalog_pages".into(), 2, "caption".into(), "Meubels".into())]);
        let to_english = plan(&texts, &dictionaries, "en");
        assert_eq!(to_english.len(), 2);
    }

    #[test]
    fn sql_in_one_transaction() {
        let updates = vec![
            ("catalog_pages".to_string(), 7, "page_text1".to_string(), "It's <b>new</b>\nback\\slash".to_string()),
            ("catalog_pages_bc".to_string(), 3, "caption".to_string(), "Bouw".to_string()),
        ];
        let sql = plan_sql(&updates);
        assert!(sql.starts_with("SET NAMES utf8mb4;\nSTART TRANSACTION;\n") && sql.ends_with("COMMIT;\n"));
        assert!(sql.contains("UPDATE catalog_pages SET page_text1 = CASE id WHEN 7 THEN 'It\\'s <b>new</b>\\nback\\\\slash' ELSE page_text1 END WHERE id IN (7);"));
        assert_eq!(sql_text("a\r\nb"), "'a\\r\\nb'", "a carriage return survives stdin");
        assert!(sql.contains("UPDATE catalog_pages_bc SET caption = CASE id WHEN 3 THEN 'Bouw' ELSE caption END WHERE id IN (3);"));
    }
}
