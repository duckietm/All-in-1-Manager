//! The Default catalogue addon: the hotel's catalogue (Catalog_Export.sql, which drops and recreates the catalogue
//! tables and items_base) into the hotel database, and its FurnitureData.json into gamedata/config. A safety backup of
//! the database comes first; the emulator is stopped for the import and started again, and when it does not load,
//! the database and the old FurnitureData.json go back.

use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};
use crate::{emulator, octane, server_tools};

/// What the server has before the step runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogState {
    /// The hotel database exists with the emulator's tables.
    pub database_ready: bool,
    pub database: Option<String>,
    pub gamedata_exists: bool,
    pub emulator_installed: bool,
    /// The catalogue version this addon installed last (the marker in gamedata/config), if any.
    pub installed_version: Option<String>,
    pub pages: i64,
    pub catalog_items: i64,
    pub furniture: i64,
}

impl CatalogState {
    /// Why the catalogue cannot be installed (yet), or None.
    pub fn problem(&self) -> Option<String> {
        if !self.database_ready {
            Some(match &self.database {
                Some(db) => format!(
                    "The hotel database {db} has no emulator tables yet: run Install New Server up to the Emulator step (the emulator creates its tables when it starts)."
                ),
                None => "There is no hotel database yet: run Install New Server first (MariaDB and the Emulator step).".into(),
            })
        } else if !self.gamedata_exists {
            Some(format!("{} does not exist yet: run Install New Server up to the Octane step first.", octane::gamedata()))
        } else {
            None
        }
    }
}

/// What the import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogResult {
    pub pages: i64,
    pub catalog_items: i64,
    pub furniture: i64,
    pub safety_backup: String,
    /// The FurnitureData.json that was there before, kept next to the new one.
    pub old_furniture_data: Option<String>,
}

/// The marker this addon writes: the version of the catalogue it installed.
pub fn marker() -> String {
    format!("{}/config/.all-in-1-catalog", octane::gamedata())
}

pub fn furniture_data() -> String {
    format!("{}/config/FurnitureData.json", octane::gamedata())
}

/// "exported 2026-11-01" from the first line of Catalog_Export.sql, as the catalogue's version.
pub fn version_of(sql: &str) -> Option<String> {
    let first = sql.lines().next()?;
    let at = first.find("exported ")? + "exported ".len();
    let date: String = first[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    (date.len() == 10).then_some(date)
}

const INSPECT: &str = r#"{{DatabaseName}}
echo "database=$db"
ready=yes
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || ready=
for table in items_base catalog_pages catalog_items items permission_ranks; do
  [ -n "$ready" ] || break
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'" 2>/dev/null)" = 1 ] || ready=
done
echo "ready=$ready"
if [ -n "$ready" ]; then
  echo "pages=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM catalog_pages' 2>/dev/null)"
  echo "catalog_items=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM catalog_items' 2>/dev/null)"
  echo "furniture=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM items_base' 2>/dev/null)"
fi
echo "gamedata=$([ -d {{Gamedata}} ] && echo yes)"
echo "emulator=$([ -x {{Directory}}/emulator ] && echo yes)"
echo "installed=$(head -c 64 {{Marker}} 2>/dev/null | tr -cd '0-9A-Za-z.-')"
true"#;

pub async fn inspect(server: &dyn Server) -> Result<CatalogState> {
    let body = fill(
        INSPECT,
        &[
            ("DatabaseName", &server_tools::database_name()),
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("Directory", &emulator::directory()),
            ("Marker", &shell_quote(&marker())),
        ],
    );
    let result = server.run_as_root(&body).await?;
    Ok(parse_state(&ScriptValues::parse(&result.output)))
}

fn parse_state(values: &ScriptValues) -> CatalogState {
    CatalogState {
        database_ready: values.yes("ready"),
        database: values.string("database"),
        gamedata_exists: values.yes("gamedata"),
        emulator_installed: values.yes("emulator"),
        installed_version: values.string("installed"),
        pages: values.int("pages"),
        catalog_items: values.int("catalog_items"),
        furniture: values.int("furniture"),
    }
}

const INSTALL: &str = r#"set -o pipefail
{{DatabaseName}}
sql={{SqlFile}}
json={{JsonFile}}
out=$(mktemp)
cleanup() { rm -f "$sql" "$json" "$out"; }
trap cleanup EXIT
[ -s "$sql" ] && [ -s "$json" ] || fail "The uploaded catalogue files are missing or empty."
chmod 600 "$sql" "$json"

mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run: run Install New Server first."
for table in items_base catalog_pages catalog_items items permission_ranks; do
  [ "$(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db' AND table_name = '$table'")" = 1 ] ||
    fail "$db has no $table table: run the Emulator step first (the emulator creates its tables when it starts)."
done
gamedata={{Gamedata}}
config="$gamedata/config"
[ -d "$gamedata" ] || fail "$gamedata does not exist: run the Octane step first."

echo "> Safety backup of $db first"
install -d -m 700 {{BackupDirectory}}
safety={{BackupDirectory}}/$db-before-catalog-$(date +%Y%m%d-%H%M%S).sql.gz
(umask 077; mariadb-dump --single-transaction --quick --routines --triggers --events --hex-blob --databases "$db" | gzip -6 > "$safety")
echo "  $safety ($(du -h "$safety" | cut -f1))"
echo "SAFETY=$safety"

d={{Directory}}
log={{LogDirectory}}/emulator.log
{{ControlFunctions}}
emulator=
# Without {{Manage}} the caller has stopped the emulator and starts it again afterwards.
if [ -n "{{Manage}}" ] && [ -x "$d/emulator" ]; then
  emulator=yes
  echo "> Stopping the emulator"
  stop_emulator
fi
restart() { [ -n "$emulator" ] && { echo "> Starting the emulator"; start_emulator; wait_loaded; } || true; }
restore_database() {
  echo "> Putting $db back as it was"
  gunzip -c "$safety" | mariadb || echo "  Restoring failed: import $safety with Tools → Import full database."
}

echo "> Importing the catalogue ({{VersionText}}, $(du -h "$sql" | cut -f1)) into $db"
start=$(date +%s)
if ! mariadb "$db" < "$sql" > "$out" 2>&1; then
  # MariaDB's own error lines, not the statement it quotes (an INSERT line can be megabytes).
  { grep -E '^ERROR' "$out" || tail -n 3 "$out"; } | head -n 5 | cut -c1-300 | sed 's/^/  /'
  restore_database
  restart
  fail "Importing the catalogue failed (see above). The hotel database is as it was before."
fi
echo "  done in $(( $(date +%s) - start )) seconds"

echo "> $config/FurnitureData.json"
install -d -o www-data -g www-data -m 2775 "$config"
old=
if [ -f "$config/FurnitureData.json" ]; then
  old="$config/FurnitureData.json.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$config/FurnitureData.json" "$old"
  echo "  The old one is kept as $old"
  echo "OLD=$old"
fi
install -o www-data -g www-data -m 664 "$json" "$config/FurnitureData.json"

pages=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM catalog_pages')
items=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM catalog_items')
furniture=$(mariadb -N "$db" -e 'SELECT COUNT(*) FROM items_base')
echo "  $pages catalogue pages, $items catalogue items, $furniture furniture"
echo "PAGES=$pages"
echo "CATALOG_ITEMS=$items"
echo "FURNITURE=$furniture"

# When the emulator does not load the new catalogue, the hotel does not stay down: the database and FurnitureData.json
# go back and the emulator starts on them. wait_loaded ends with fail, so it runs in a subshell here.
if [ -n "$emulator" ]; then
  echo "> Starting the emulator"
  start_emulator
  if ! (wait_loaded); then
    echo "> The emulator does not start with the new catalogue"
    restore_database
    if [ -n "$old" ]; then cp -p "$old" "$config/FurnitureData.json"; else rm -f "$config/FurnitureData.json"; fi
    start_emulator
    (wait_loaded) || true
    fail "The emulator does not start with the new catalogue (its log is above). The database and FurnitureData.json are as they were."
  fi
fi
printf '%s\n' {{Version}} > {{Marker}}
chown www-data:www-data {{Marker}}
true"#;

/// Imports the uploaded `sql_file` into the hotel database and puts `json_file` in gamedata/config as
/// FurnitureData.json, after a safety backup; puts everything back when the import or the emulator's start fails.
/// With `manage_emulator` it stops the emulator first and starts it at the end; without, the caller does both
/// (`server_tools::stop_emulator` / `start_emulator`), so several steps share one stop and one start.
pub async fn install(server: &dyn Server, sql_file: &str, json_file: &str, version: &str, manage_emulator: bool, log: Log<'_>) -> Result<CatalogResult> {
    // Only letters, digits, dots and dashes: the version also goes into a log line and the marker unquoted.
    let version: String = version.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).take(64).collect();
    let version = if version.is_empty() { "unknown".to_string() } else { version };
    // The file names and the control functions go in last, so nothing in them can be taken for a placeholder.
    let body = fill(
        INSTALL,
        &[
            ("DatabaseName", &server_tools::database_name()),
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("BackupDirectory", &server_tools::backup_directory()),
            ("Directory", &emulator::directory()),
            ("LogDirectory", &emulator::log_directory()),
            ("VersionText", &version),
            ("Version", &shell_quote(&version)),
            ("Manage", if manage_emulator { "yes" } else { "" }),
            ("Marker", &shell_quote(&marker())),
            ("ControlFunctions", &emulator::control_functions()),
            ("SqlFile", &shell_quote(sql_file)),
            ("JsonFile", &shell_quote(json_file)),
        ],
    );
    let values = script::run_capturing(
        server,
        &script::with_prelude(&body),
        log,
        "Installing the catalogue failed",
        &["SAFETY", "OLD", "PAGES", "CATALOG_ITEMS", "FURNITURE"],
        None,
    )
    .await?;
    Ok(CatalogResult {
        pages: values.int("PAGES"),
        catalog_items: values.int("CATALOG_ITEMS"),
        furniture: values.int("FURNITURE"),
        safety_backup: values.or_empty("SAFETY").to_string(),
        old_furniture_data: values.string("OLD"),
    })
}

const RESTORE: &str = r#"set -o pipefail
safety={{Safety}}
config={{Gamedata}}/config
[ -s "$safety" ] || fail "The safety backup $safety is missing: import a backup with Tools → Import full database."
echo "> Putting the database back as it was ($(basename "$safety"))"
gunzip -c "$safety" | mariadb || fail "Restoring failed: import $safety with Tools → Import full database."
old={{Old}}
if [ -n "$old" ] && [ -f "$old" ]; then
  cp -p "$old" "$config/FurnitureData.json"
  echo "  FurnitureData.json as it was ($(basename "$old"))"
elif [ -z "$old" ]; then
  rm -f "$config/FurnitureData.json"
fi
rm -f {{Markers}}
true"#;

/// Puts the database (from `safety`) and FurnitureData.json (the `old` copy, or none) back as they were before the
/// catalogue, when the emulator does not load at the end.
pub async fn restore(server: &dyn Server, safety: &str, old: Option<&str>, log: Log<'_>) -> Result<()> {
    let body = fill(
        RESTORE,
        &[
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("Markers", &format!("{} {}", shell_quote(&marker()), shell_quote(&crate::furniture_language::marker()))),
            ("Old", &shell_quote(old.unwrap_or(""))),
            ("Safety", &shell_quote(safety)),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Putting the catalogue back failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_the_export_header() {
        assert_eq!(
            version_of("-- Catalog_Export.sql: the catalogue of camwijsnew, exported 2026-11-01 by Export_Catalog.sh.\n--\n").as_deref(),
            Some("2026-11-01")
        );
        assert_eq!(version_of("SET NAMES utf8mb4;"), None);
        assert_eq!(version_of(""), None);
    }

    #[test]
    fn state_and_problems() {
        let state = parse_state(&ScriptValues::parse(
            "database=habbo\nready=yes\npages=412\ncatalog_items=9001\nfurniture=12000\ngamedata=yes\nemulator=yes\ninstalled=2026-11-01\n",
        ));
        assert!(state.database_ready && state.gamedata_exists && state.emulator_installed);
        assert_eq!((state.pages, state.catalog_items, state.furniture), (412, 9001, 12000));
        assert_eq!(state.installed_version.as_deref(), Some("2026-11-01"));
        assert_eq!(state.problem(), None);
        let empty = parse_state(&ScriptValues::parse("database=habbo\nready=\ngamedata=\n"));
        assert!(empty.problem().unwrap().contains("habbo has no emulator tables"));
        let no_gamedata = parse_state(&ScriptValues::parse("database=habbo\nready=yes\ngamedata=\n"));
        assert!(no_gamedata.problem().unwrap().contains("does not exist yet"));
    }
}
