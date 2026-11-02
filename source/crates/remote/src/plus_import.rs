//! Tools → Import Plus database: a PlusEMU dump (such as its fresh install) into the Polaris hotel database. The dump is
//! loaded into a staging database next to the hotel's, and sql/plus-to-polaris-*.sql convert it: the reverse of
//! plus-migrator (github.com/plus-emulator/plus-migrator), which goes from Arcturus/Polaris to Plus. (PlusImport.cs)

use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server};
use crate::{emulator, server_tools};

/// What the Plus import changes in the hotel database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlusImportMode {
    /// Adds the furniture Polaris does not have yet; the catalog stays as it is.
    FurnitureOnly,
    /// Adds the furniture and replaces the normal catalog with the Plus catalog (Builders Club and Habbicons stay).
    FurnitureAndCatalog,
}

/// The counts the import reports at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlusImportResult {
    pub furniture_existing: i64,
    pub furniture_new: i64,
    pub crackables_without_prizes: i64,
    pub pages: i64,
    pub catalog_items: i64,
    pub safety_backup: String,
}

/// Where the dump is loaded; dropped again at the end.
pub const STAGING_DATABASE: &str = "plus_import";

/// Embedded resources in the C# installer; byte-identical copies of SourceCode/Remote/Sql.
const FURNITURE_SQL: &str = include_str!("../sql/plus-to-polaris-furniture.sql");
const CATALOG_SQL: &str = include_str!("../sql/plus-to-polaris-catalog.sql");

/// The conversion for `mode`, with @S@ and @T@ still in it (the script fills them in).
pub fn conversion_sql(mode: PlusImportMode) -> String {
    match mode {
        PlusImportMode::FurnitureOnly => FURNITURE_SQL.to_string(),
        PlusImportMode::FurnitureAndCatalog => format!("{FURNITURE_SQL}\n{CATALOG_SQL}"),
    }
}

const IMPORT: &str = r#"set -o pipefail
{{DatabaseName}}
staging={{StagingDatabase}}
f={{RemoteFile}}
sql=$(mktemp)
out=$(mktemp)
# The conversion comes over stdin.
cat > "$sql"
cleanup() { rm -f "$sql" "$out" "$f"; mariadb -e "DROP DATABASE IF EXISTS \`$staging\`" 2>/dev/null || true; }
trap cleanup EXIT
[ -s "$f" ] || fail "The uploaded file $f is missing or empty."
chmod 600 "$f"
if gzip -t "$f" 2>/dev/null; then read_dump() { gunzip -c "$f"; }; else read_dump() { cat "$f"; }; fi
(set +o pipefail; read_dump | grep -qE 'CREATE TABLE `furniture`') ||
  fail "$(basename "$f") is not a PlusEMU dump: it has no furniture table (Polaris and Arcturus call it items_base)."

mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run."
for table in items_base catalog_pages catalog_items catalog_clothing items_crackable permission_ranks; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'")" = 1 ] ||
    fail "$db has no $table table: it is not a Polaris database (or the emulator never started on it)."
done

echo "> Safety backup of $db first"
install -d -m 700 {{BackupDirectory}}
safety={{BackupDirectory}}/$db-before-plus-import-$(date +%Y%m%d-%H%M%S).sql.gz
(umask 077; mariadb-dump --single-transaction --quick --routines --triggers --events --hex-blob --databases "$db" | gzip -6 > "$safety")
echo "  $safety ($(du -h "$safety" | cut -f1))"
echo "SAFETY=$safety"

d={{Directory}}
log={{LogDirectory}}/emulator.log
{{ControlFunctions}}
emulator=
if [ -x "$d/emulator" ]; then
  emulator=yes
  echo "> Stopping the emulator"
  stop_emulator
fi
# Whatever happens from here, the emulator runs again at the end.
restart() { [ -n "$emulator" ] && { echo "> Starting the emulator"; start_emulator; wait_loaded; } || true; }

echo "> Loading $(basename "$f") ($(du -h "$f" | cut -f1)) into the staging database $staging"
mariadb -e "DROP DATABASE IF EXISTS \`$staging\`; CREATE DATABASE \`$staging\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
start=$(date +%s)
if ! read_dump | sed -E "/^(CREATE DATABASE|USE) /s/\`[^\`]+\`/\`$staging\`/" | mariadb "$staging"; then
  restart
  fail "Loading the Plus dump failed (see above). The hotel database was not changed."
fi
echo "  $(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$staging'") tables in $(( $(date +%s) - start )) seconds"
for table in furniture catalog_pages catalog_offers catalog_offer_products catalog_page_offers catalog_page_texts catalog_page_images catalog_bot_presets; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$staging' AND table_name = '$table'")" = 1 ] ||
    { restart; fail "The Plus dump has no $table table; this Plus version is not supported. The hotel database was not changed."; }
done
# Tables of newer Polaris versions: without them, the lines that use them are left out.
for table in catalog_id_sequences catalog_manager_state; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'")" = 1 ] ||
    sed -i "/$table/d" "$sql"
done

echo "> Converting to Polaris ({{Mode}})"
sed -i "s/@S@/$staging/g; s/@T@/$db/g" "$sql"
start=$(date +%s)
if ! mariadb -N "$staging" < "$sql" > "$out" 2>&1; then
  sed 's/^/  /' "$out" | tail -n 20
  echo "> The conversion stopped: putting $db back as it was"
  gunzip -c "$safety" | mariadb || echo "  Restoring failed: import $safety with Import full database."
  restart
  fail "Converting the Plus data failed (see above). The hotel database is as it was before."
fi
echo "  done in $(( $(date +%s) - start )) seconds"
# The report: the counts the tool reads stay KEY=value, the rest is written out.
awk -F'=' '
  /^(FURNI_EXISTING|FURNI_NEW|CRACKABLE_WITHOUT_PRIZES|PAGES|CATALOG_ITEMS)=/ { print; next }
  $1 == "FURNI_HOW" { split($2, a, ":"); print "  New furniture, interaction " a[1] ": " a[2]; next }
  $1 == "FALLBACK" { split($2, a, ":"); print "  No Polaris interaction for the Plus type " a[1] " (" a[2] "x, e.g. " a[3] "): plain furniture"; next }
  $1 == "BC_PAGES_KEPT" { print "  Builders Club: " $2 " Plus pages left out, Polaris keeps its own"; next }
  $1 == "PRODUCTS_SKIPPED" { split($2, a, ":"); print "  Left out: " a[2] " " a[1] " products (Polaris has no such product)"; next }
  $1 == "OFFERS_WITHOUT_ITEMS" { if ($2 > 0) print "  Offers with nothing Polaris can sell: " $2 " (left out)"; next }
  $1 == "POLARIS_PAGES_KEPT" { split($2, a, ":"); if (a[1] > 0) print "  Kept from the Polaris catalog: " a[2]; next }
  { print "  " $0 }' "$out"
# When the emulator does not load the converted database, the hotel does not stay down: the database goes
# back and the emulator starts on it. wait_loaded ends with fail, so it runs in a subshell here.
if [ -n "$emulator" ]; then
  echo "> Starting the emulator"
  start_emulator
  if ! (wait_loaded); then
    echo "> The emulator does not start with the converted database: putting $db back as it was"
    gunzip -c "$safety" | mariadb || fail "Restoring failed: import $safety with Import full database."
    start_emulator
    (wait_loaded) || true
    fail "The emulator does not start with the converted database (its log is above). $db is as it was before the import."
  fi
fi"#;

/// Loads the uploaded dump `remote_file` into the staging database, converts it into the hotel database after a
/// safety backup, and puts the database back when the conversion or the emulator's start fails.
pub async fn import(server: &dyn Server, remote_file: &str, mode: PlusImportMode, log: Log<'_>) -> Result<PlusImportResult> {
    let mode_text = match mode {
        PlusImportMode::FurnitureAndCatalog => "furniture and catalog",
        PlusImportMode::FurnitureOnly => "furniture only",
    };
    // The file name and the control functions go in last, so nothing in them can be taken for a placeholder.
    let body = fill(
        IMPORT,
        &[
            ("DatabaseName", &server_tools::database_name()),
            ("StagingDatabase", STAGING_DATABASE),
            ("BackupDirectory", &server_tools::backup_directory()),
            ("Directory", &emulator::directory()),
            ("LogDirectory", &emulator::log_directory()),
            ("Mode", mode_text),
            ("ControlFunctions", &emulator::control_functions()),
            ("RemoteFile", &shell_quote(remote_file)),
        ],
    );
    let values = script::run_capturing(
        server,
        &script::with_prelude(&body),
        log,
        "Importing the Plus database failed",
        &["SAFETY", "FURNI_EXISTING", "FURNI_NEW", "CRACKABLE_WITHOUT_PRIZES", "PAGES", "CATALOG_ITEMS"],
        Some(&conversion_sql(mode)),
    )
    .await?;
    Ok(PlusImportResult {
        furniture_existing: values.int("FURNI_EXISTING"),
        furniture_new: values.int("FURNI_NEW"),
        crackables_without_prizes: values.int("CRACKABLE_WITHOUT_PRIZES"),
        pages: values.int("PAGES"),
        catalog_items: values.int("CATALOG_ITEMS"),
        safety_backup: values.or_empty("SAFETY").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_sql_per_mode() {
        let furniture = conversion_sql(PlusImportMode::FurnitureOnly);
        let both = conversion_sql(PlusImportMode::FurnitureAndCatalog);
        assert_eq!(furniture, FURNITURE_SQL);
        assert!(both.starts_with(&furniture));
        assert_eq!(both.len(), FURNITURE_SQL.len() + 1 + CATALOG_SQL.len());
        assert!(furniture.contains("@S@") && furniture.contains("@T@"));
    }
}
