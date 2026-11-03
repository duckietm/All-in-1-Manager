//! EpicNext CMS: Node.js / pnpm, .env, the capped build, the systemd service and the tests. (EpicNextInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub const SITE_NAME: &str = "epicnext";
pub const SERVICE: &str = "epicnext-cms";

// ---------------------------------------------------------------------------------------------------------------
//
// EpicNext CMS (Next.js, no PHP) as the hotel's website, like AtomCMS: cloned to /var/www/epicnext, built with pnpm on
// the server itself (not in Docker) and run by systemd as www-data on 127.0.0.1:3002. Its NGINX site on a free port
// from 85 to 95 passes the visitors to it; Octane is on /client/.

use regex::Regex;

use aio_core::DatabaseCredentials;

use crate::atom_cms::{self, AtomCmsState};
use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::WebAccessSettings;
use crate::{emulator, mariadb, octane};

/// What the server has for EpicNext CMS before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicNextState {
    pub server: AtomCmsState,
    pub hotel_name: Option<String>,
    pub origin: Option<String>,
    pub built: bool,
    pub service_installed: bool,
    pub service_running: bool,
    pub atom_tables: bool,
    pub source_branch: Option<String>,
    pub source_folder: Option<String>,
}

impl EpicNextState {
    pub fn site(&self) -> Option<&WebAccessSettings> {
        self.server.site.as_ref()
    }

    pub fn installed(&self) -> bool {
        self.site().is_some_and(|s| s.epic_next) && self.built && self.service_running
    }
}

/// EpicNext's own repository; the CMS is its top folder ([`REPOSITORY_FOLDER`] empty).
pub const REPOSITORY: &str = "https://gitea.epicnabbo.nl/Remco/EpicNext-Cms.git";
pub const REPOSITORY_FOLDER: &str = "";
/// A checkout of the repository when the CMS is in a folder of it; the folder is copied to [`DIRECTORY`].
pub const SOURCE_DIRECTORY: &str = "/var/www/epicnext-src";
const SOURCE_INFO: &str = "/var/www/epicnext-src/.all-in-1-source";
pub const NAME: &str = "EpicNext CMS";
pub const DIRECTORY: &str = "/var/www/epicnext";
pub const SERVICE_FILE: &str = "/etc/systemd/system/epicnext-cms.service";
pub const PORT: u16 = 3002;
pub const UPSTREAM: &str = "http://127.0.0.1:3002";
/// Its .nvmrc: >=26.12.1 <27.
pub const NODE_MINIMUM: &str = "26.12.1";
/// The link earlier versions made, from before EpicNext read /var/www/gamedata.
const OLD_GAMEDATA_LINK: &str = "/var/www/Gamedata";
const LOG_FILE: &str = "/var/log/epicnext-cms.log";

// Node.js 26 and corepack come from the Dev tools step; corepack gets the pnpm version package.json pins, without asking.
const ENVIRONMENT: &str =
    "export PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root CI=1 NO_COLOR=1 COREPACK_ENABLE_DOWNLOAD_PROMPT=0 NEXT_TELEMETRY_DISABLED=1";

/// Node script: the SQL for the base tables of drizzle/drafts/0000_premium_spirit.sql that the hotel database lacks.
/// Arguments: the draft, the migrations folder, a file with the existing tables and one with the applied migrations.
const BASE_SCHEMA_SCRIPT: &str = r#"// Prints the SQL for EpicNext's base tables that the hotel database does not have yet (EpicNext was built on an
// AtomCMS database), and drops the columns from those new tables that a pending migration adds again.
const fs = require("fs");
const [draftFile, migrationsDir, existingFile, appliedFile] = process.argv.slice(2);
const lines = (file) => new Set(fs.readFileSync(file, "utf8").split("\n").map((l) => l.trim().toLowerCase()).filter(Boolean));
const existing = lines(existingFile);
const applied = lines(appliedFile);
const created = new Map();
for (const statement of fs.readFileSync(draftFile, "utf8").split("--> statement-breakpoint")) {
	const table = statement.match(/CREATE TABLE `([^`]+)`/);
	if (!table || existing.has(table[1].toLowerCase())) continue;
	created.set(table[1], new Set([...statement.matchAll(/^\s*`([^`]+)`\s/gm)].map((c) => c[1])));
	// The collation of EpicNext's migrations, so their joins with these tables work.
	console.log(statement.trim().replace(/;$/, "").replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ") + " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;");
}
for (const file of fs.readdirSync(migrationsDir).filter((f) => f.endsWith(".sql")).sort()) {
	if (applied.has(file.replace(/\.sql$/, "").toLowerCase())) continue;
	const sql = fs.readFileSync(migrationsDir + "/" + file, "utf8");
	for (const alter of sql.matchAll(/ALTER TABLE\s+`?(\w+)`?([^;]*);/g)) {
		const columns = created.get(alter[1]);
		if (!columns) continue;
		for (const add of alter[2].matchAll(/ADD COLUMN\s+(?!IF NOT EXISTS)`?(\w+)`?/g))
			if (columns.has(add[1])) console.log("ALTER TABLE `" + alter[1] + "` DROP COLUMN `" + add[1] + "`;");
	}
}
console.error("  " + created.size + " base tables to create" + (created.size ? ": " + [...created.keys()].join(", ") : ""));"#;

const STOP_SERVICE: &str = r#"if [ -d /run/systemd/system ]; then systemctl stop {{Service}} 2>/dev/null || true
else pkill -u www-data -f '{{Directory}}/node_modules/next/' 2>/dev/null || true; fi"#;

fn stop_service() -> String {
    fill(STOP_SERVICE, &[("Service", SERVICE), ("Directory", DIRECTORY)])
}

const INSPECT: &str = r#"d={{Directory}}
echo "name=$(grep -sE '^HOTEL_NAME=' "$d/.env" | head -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/')"
if [ -f {{SourceInfo}} ]; then
  # url, branch and folder of the last run with a folder of a repository.
  sed -n '1s/^/origin=/p; 2s/^/branch=/p; 3s/^/folder=/p' {{SourceInfo}}
else
  echo "origin=$(git -c safe.directory="$d" -C "$d" remote get-url origin 2>/dev/null)"
fi
echo "built=$([ -f "$d/.next/BUILD_ID" ] && echo yes)"
echo "service=$([ -f {{ServiceFile}} ] && echo yes)"
echo "running=$(curl -s --noproxy '*' -o /dev/null --max-time 5 -w '%{http_code}' {{Upstream}}/ 2>/dev/null | grep -qvE '^(000)?$' && echo yes)"
db=$(grep -sE '^db.database=' {{EmulatorDirectory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
[ -n "$db" ] && echo "atom=$(mariadb -N "$db" -e "SELECT 1 FROM website_installation LIMIT 1" >/dev/null 2>&1 && echo yes)""#;

pub async fn inspect(server: &dyn Server) -> Result<EpicNextState> {
    let state = atom_cms::inspect(server).await?;
    let script = fill(
        INSPECT,
        &[
            ("Directory", DIRECTORY),
            ("SourceInfo", SOURCE_INFO),
            ("ServiceFile", SERVICE_FILE),
            ("Upstream", UPSTREAM),
            ("EmulatorDirectory", &emulator::directory()),
        ],
    );
    let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
    Ok(EpicNextState {
        server: state,
        hotel_name: values.string("name"),
        origin: values.string("origin"),
        built: values.yes("built"),
        service_installed: values.yes("service"),
        service_running: values.yes("running"),
        atom_tables: values.yes("atom"),
        source_branch: values.string("branch"),
        source_folder: values.string("folder"),
    })
}

/// A folder inside the repository: "NextCMS/epicnext-cms", or "" for the top folder.
pub fn folder_problem(folder: &str) -> Option<String> {
    let valid = folder.is_empty()
        || (Regex::new("^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$").unwrap().is_match(folder)
            && !folder.split('/').any(|part| part == "." || part == ".."));
    if valid {
        None
    } else {
        Some("The folder is a path inside the repository such as NextCMS/epicnext-cms (letters, digits, '.', '_', '-' and '/'), or empty for the top folder.".into())
    }
}

/// A branch name, or "" for the repository's default branch.
pub fn branch_problem(branch: &str) -> Option<String> {
    let valid = branch.is_empty()
        || (Regex::new("^[A-Za-z0-9._/-]{1,100}$").unwrap().is_match(branch)
            && !branch.contains("..")
            && !branch.starts_with('-')
            && !branch.starts_with('/')
            && !branch.ends_with('/'));
    if valid {
        None
    } else {
        Some("Enter a branch name such as main, or leave it empty for the repository's default branch.".into())
    }
}

const GET_FROM_FOLDER: &str = r#"{{Environment}}
set -o pipefail
for tool in git rsync; do
  if ! command -v $tool >/dev/null 2>&1; then
    echo "> Installing $tool"
    apt_update >/dev/null
    apt-get install -y $tool >/dev/null
  fi
done
src={{SourceDirectory}}
d={{Directory}}
u={{url}}
b={{branch}}
f={{folder}}
g() { git -c safe.directory="$src" -C "$src" "$@"; }
# The checkout fits when it is of the same repository, branch and folder; otherwise it is made again.
if [ -d "$src/.git" ] && [ "$(sed -n 1p {{SourceInfo}} 2>/dev/null)" = "$u" ] &&
   [ "$(sed -n 2p {{SourceInfo}} 2>/dev/null)" = "$b" ] && [ "$(sed -n 3p {{SourceInfo}} 2>/dev/null)" = "$f" ]; then
  current=$(g rev-parse --abbrev-ref HEAD)
  echo "> git fetch origin $current  ($src)"
  GIT_TERMINAL_PROMPT=0 g fetch --quiet --depth 1 origin "$current" || fail "git fetch failed (see above)."
  g reset --quiet --hard FETCH_HEAD
else
  rm -rf "$src"
  echo "> git clone --sparse${b:+ --branch $b} $u  (only $f, into $src)"
  GIT_TERMINAL_PROMPT=0 git clone --quiet --depth 1 --filter=blob:none --sparse ${b:+--branch "$b"} "$u" "$src" 2>&1 ||
    fail "Cloning $u${b:+ (branch $b)} failed (see above)."
  g sparse-checkout set "$f" || fail "git sparse-checkout failed (see above)."
  printf '%s\n%s\n%s\n' "$u" "$b" "$f" > {{SourceInfo}}
fi
if [ ! -f "$src/$f/package.json" ]; then
  fail "$f is not in the $(g rev-parse --abbrev-ref HEAD) branch of $u (no package.json there). Enter the branch that has it, or check the folder."
fi
[ -d "$d" ] && [ -n "$(ls -A "$d" 2>/dev/null)" ] && [ ! -f "$d/package.json" ] &&
  echo "  WARNING: $d had other content; files the CMS does not use stay there"
install -d -m 755 "$d"
echo "> $f -> $d"
# What the CMS writes while it runs stays: settings, packages, build, uploads, imported assets.
rsync -a --delete               --exclude=/.env --exclude='/.env.bak-*' --exclude=/.env.local               --exclude=/node_modules/ --exclude=/.next/ --exclude=/storage/ --exclude=/logs/ --exclude=/backups/               --exclude=/public/nitro-assets/ --exclude=/public/swf/ --exclude=/public/cache/ --exclude=/public/assets/images/badges/               --exclude=/.git/               "$src/$f/" "$d/"
echo "  $(find "$d" -path "$d/node_modules" -prune -o -type f -print | wc -l) files"
echo "COMMIT=$(g rev-parse --abbrev-ref HEAD) $(g log -1 --format='%h %cd %s' --date=short)""#;

/// The CMS from a folder of a repository (such as the installer's own, with EpicNext in NextCMS/epicnext-cms): a sparse
/// checkout of that folder in /var/www/epicnext-src, copied to /var/www/epicnext. The copy keeps what the CMS writes at
/// runtime (.env, node_modules, .next, storage, imported assets). Returns the branch and commit.
pub async fn get_from_folder(server: &dyn Server, url: &str, branch: &str, folder: &str, log: Log<'_>) -> Result<String> {
    let body = fill(
        GET_FROM_FOLDER,
        &[
            ("Environment", ENVIRONMENT),
            ("SourceDirectory", SOURCE_DIRECTORY),
            ("Directory", DIRECTORY),
            ("url", &shell_quote(url)),
            ("branch", &shell_quote(branch)),
            ("folder", &shell_quote(folder)),
            ("SourceInfo", SOURCE_INFO),
        ],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Getting EpicNext failed", &["COMMIT"], None).await?;
    Ok(values.or_empty("COMMIT").to_string())
}

const PREPARE_TOOLS: &str = r#"{{Environment}}
# sort -V puts the lower version first.
older() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" != "$2" ]; }
v=$(node -v 2>/dev/null | tr -d v)
[ -n "$v" ] || fail "Node.js is missing: run the Dev tools step first."
case "$v" in 26.*) ;; *) fail "Node.js $v is installed; EpicNext needs Node.js 26 ({{NodeMinimum}} or newer). Run the Dev tools step first." ;; esac
if older "$v" {{NodeMinimum}}; then
  echo "> Node.js $v is older than {{NodeMinimum}}: apt-get install --only-upgrade nodejs"
  apt_update >/dev/null
  apt-get install -y --only-upgrade nodejs >/dev/null
  v=$(node -v | tr -d v)
  older "$v" {{NodeMinimum}} && fail "Node.js is still $v after the upgrade; EpicNext needs {{NodeMinimum}} or newer."
fi
echo "  Node.js $v"
if ! command -v corepack >/dev/null 2>&1; then
  echo "> npm install -g corepack"
  npm install -g --no-fund --no-audit corepack >/dev/null
fi
cd {{Directory}}
# The version in package.json's packageManager.
p=$(corepack pnpm --version 2>&1) || { echo "$p" | tail -5; fail "corepack could not get pnpm (see above)."; }
echo "  pnpm $p (corepack)""#;

/// Node.js 26.12.1 or newer (apt upgrades an older Node.js 26 from NodeSource) and pnpm through corepack.
pub async fn prepare_tools(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(PREPARE_TOOLS, &[("Environment", ENVIRONMENT), ("NodeMinimum", NODE_MINIMUM), ("Directory", DIRECTORY)]);
    script::run(server, &script::with_prelude(&body), log, "Preparing Node.js and pnpm failed", None).await
}

// Double-quotes a .env value; dotenv-expand reads \$ as a plain $.
fn env_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"))
}

const CONFIGURE_ENV: &str = r#"cd {{Directory}}
if [ -f .env ]; then
  cp -p .env ".env.bak-$(date +%Y%m%d-%H%M%S)"
  echo "> {{Directory}}/.env (the old one is kept as .env.bak-<time>)"
elif [ -f .env.example ]; then
  cp .env.example .env
  echo "> {{Directory}}/.env from .env.example"
else
  : > .env
  echo "> {{Directory}}/.env (new: the clone has no .env.example)"
fi
env_set() {
  if grep -q "^$1=" .env; then
    KEY="$1" VALUE="$2" awk 'index($0, ENVIRON["KEY"] "=") == 1 { print ENVIRON["KEY"] "=" ENVIRON["VALUE"]; next } { print }' .env > .env.new
    cat .env.new > .env && rm -f .env.new
  else
    printf '%s=%s\n' "$1" "$2" >> .env
  fi
  case "$1" in DATABASE_URL) echo "  $1=$(echo "$2" | sed -E 's#://([^:]*):[^@]*@#://\1:***@#')" ;; *SECRET*) echo "  $1=***" ;; *) echo "  $1=$2" ;; esac
}
{{sets}}
secret=$(grep -sE '^AUTH_SECRET=' .env | cut -d= -f2- | tr -d '"')
if [ ${#secret} -lt 32 ] || echo "$secret" | grep -qiE 'random|change|your'; then
  env_set AUTH_SECRET "$(openssl rand -hex 32)"
else
  echo "  AUTH_SECRET kept"
fi
ini={{EmulatorDirectory}}/config.ini
rcon=$(grep -sE '^rcon.port=' "$ini" | cut -d= -f2 | tr -d '[:space:]')
env_set RCON_PORT "${rcon:-3001}"
# Without Valkey/Redis on this server EpicNext keeps its cache in memory; a URL to nothing only gives errors.
if ss -Hltn 'sport = :6379' 2>/dev/null | grep -q .; then echo "  REDIS_URL kept (something listens on 6379)"; else
  sed -i -E 's/^REDIS_URL=/# REDIS_URL=/' .env
  echo "  REDIS_URL left unset (no Valkey/Redis on 6379)"
fi

# The database login: over TCP to 127.0.0.1, as EpicNext connects.
db=$(grep -sE '^db.database=' "$ini" | cut -d= -f2 | tr -d '[:space:]')
[ -n "$db" ] || fail "No db.database in $ini: run the Emulator step first."
db_ok() { [ -n "$1" ] && [ -n "$2" ] && MYSQL_PWD="$2" mariadb --protocol=tcp -h 127.0.0.1 -P 3306 -u "$1" -N -e 'SELECT 1 FROM users LIMIT 1' "$db" >/dev/null 2>&1; }
enc() { node -e 'process.stdout.write(encodeURIComponent(process.argv[1]))' "$1"; }
dbuser= dbpass= from=
try_login() { if [ -z "$from" ] && db_ok "$1" "$2"; then dbuser=$1 dbpass=$2 from=$3; fi; }
try_login {{CmsUser}} {{CmsPassword}} "the cms login saved on this computer"
cur=$(grep -sE '^DATABASE_URL=' .env | head -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/')
if [ -n "$cur" ]; then
  { read -r u; read -r p; } < <(node -e 'try { const u = new URL(process.argv[1]); console.log(decodeURIComponent(u.username)); console.log(decodeURIComponent(u.password)) } catch { console.log(""); console.log("") }' "$cur")
  [ "$u" = user ] && [ "$p" = password ] || try_login "$u" "$p" "the login already in .env"
fi
atom={{AtomDirectory}}/.env
value() { grep -sE "^$1=" "$atom" | head -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/' | tr -d '\r'; }
try_login "$(value DB_USERNAME)" "$(value DB_PASSWORD)" "AtomCMS' cms login ($atom)"
if [ -z "$from" ]; then
  # An account of its own, made with root (the socket login), with the cms account's rights on the hotel database only.
  pw=$(openssl rand -hex 24)
  mariadb <<SQL || fail "Making the MariaDB account epicnext failed (see above). Set DATABASE_URL in {{Directory}}/.env yourself and run this step again."
CREATE USER IF NOT EXISTS 'epicnext'@'localhost' IDENTIFIED BY '$pw';
ALTER USER 'epicnext'@'localhost' IDENTIFIED BY '$pw';
CREATE USER IF NOT EXISTS 'epicnext'@'127.0.0.1' IDENTIFIED BY '$pw';
ALTER USER 'epicnext'@'127.0.0.1' IDENTIFIED BY '$pw';
GRANT {{CmsPrivileges}} ON \`$db\`.* TO 'epicnext'@'localhost', 'epicnext'@'127.0.0.1';
FLUSH PRIVILEGES;
SQL
  try_login epicnext "$pw" "a new MariaDB account epicnext (only on $db)"
  [ -n "$from" ] || fail "The new MariaDB account epicnext cannot log in on 127.0.0.1:3306."
fi
echo "  Database login: $from"
env_set DATABASE_URL "mysql://$(enc "$dbuser"):$(enc "$dbpass")@127.0.0.1:3306/$(enc "$db")?charset=utf8mb4&connection_limit=25&connect_timeout=5"
# EpicNext's checks (src/env.ts) refuse an empty value for a URL or a number (such as DISCORD_WEBHOOK_URL= from
# .env.example), while an unset one is fine: empty settings are commented out.
empty=$(grep -cE '^[A-Za-z0-9_]+=("")?[[:space:]]*$' .env || true)
if [ "$empty" -gt 0 ]; then
  sed -i -E 's/^([A-Za-z0-9_]+)=("")?[[:space:]]*$/# \1=   (empty: left unset by All-in-1 Installer)/' .env
  echo "  $empty empty settings left unset (commented out): EpicNext refuses empty values"
fi
chmod 640 .env
chown www-data:www-data .env"#;

/// .env from .env.example when it is missing, with the keys this server decides: the hotel database, the site's
/// address, RCON. A random AUTH_SECRET once; later runs keep it, so logins stay valid. The database login is the first
/// that works of: the cms login saved on this computer, the one already in .env, AtomCMS' cms login, or else a MariaDB
/// account of its own (epicnext, only on the hotel database) made with root.
pub async fn configure_env(
    server: &dyn Server,
    site: &WebAccessSettings,
    hotel_name: &str,
    credentials: Option<&DatabaseCredentials>,
    log: Log<'_>,
) -> Result<()> {
    let values: [(&str, String); 6] = [
        ("HOTEL_NAME", env_value(hotel_name)),
        ("APP_URL", site.url()),
        // EpicNext's settings check (src/env.ts) refuses an APP_URL that differs from NEXT_PUBLIC_APP_URL,
        // which .env.example sets to http://localhost:3002.
        ("NEXT_PUBLIC_APP_URL", site.url()),
        ("AUTH_URL", site.url()),
        // Behind Traefik and NGINX: NextAuth takes the host from the forwarded headers.
        ("AUTH_TRUST_HOST", "true".into()),
        ("RCON_HOST", "127.0.0.1".into()),
    ];
    let sets = values.iter().map(|(k, v)| format!("env_set {k} {}", shell_quote(v))).collect::<Vec<_>>().join("\n");
    let body = fill(
        CONFIGURE_ENV,
        &[
            ("Directory", DIRECTORY),
            ("sets", &sets),
            ("EmulatorDirectory", &emulator::directory()),
            ("CmsUser", &shell_quote(credentials.map(|c| c.cms_user.as_str()).unwrap_or(""))),
            ("CmsPassword", &shell_quote(credentials.map(|c| c.cms_password.as_str()).unwrap_or(""))),
            ("AtomDirectory", &atom_cms::directory()),
            ("CmsPrivileges", mariadb::CMS_PRIVILEGES),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Writing EpicNext's .env failed", None).await
}

const BUILD: &str = r#"{{Environment}}
cd {{Directory}}
# Runs a command with its output as it comes (without empty lines), and returns the command's own exit code.
run() {
  rc=$(mktemp)
  ("$@" 2>&1; echo $? > "$rc") | grep --line-buffered -vE '^\s*$' || true
  code=$(cat "$rc"); rm -f "$rc"
  return "$code"
}
# The hotel's gamedata: EpicNext gets its folder as the gamedata_root setting (below). The
# /var/www/Gamedata link of earlier versions is no longer needed.
g={{Gamedata}}
if [ -L {{OldGamedataLink}} ]; then
  rm -f {{OldGamedataLink}}
  echo "> {{OldGamedataLink}} link removed: EpicNext reads $g"
fi
# Free disk space in MB where the CMS is, and what uses the disk when there is too little.
free_mb() { df -Pm {{Directory}} | awk 'NR == 2 {print $4}'; }
need_disk() {
  [ "$(free_mb)" -ge "$1" ] && return 0
  echo "  The disk of {{Directory}}:"
  df -h {{Directory}} | sed 's/^/    /'
  echo "  What uses the most:"
  du -xsh /var/www/* /root/.cache /root/.local/share/pnpm /var/lib/docker /var/lib/mysql /var/backups /home/* /var/log /var/cache/apt /tmp 2>/dev/null | sort -rh | head -12 | sed 's/^/    /'
  command -v docker >/dev/null 2>&1 && docker system df 2>/dev/null | sed 's/^/    /'
  fail "Only $(free_mb) MB free on the disk; $2 needs at least $(( $1 / 1024 )) GB. Make room (old backups, *.bak-* folders, unused Docker images: docker image prune -a, logs: journalctl --vacuum-size=200M) or give the server a bigger disk, then run this step again."
}
# Caches that are only downloads: pnpm keeps what node_modules uses.
apt-get clean 2>/dev/null || true
corepack pnpm store prune >/dev/null 2>&1 || true
echo "> Disk: $(free_mb) MB free"
need_disk 3072 "pnpm install"
echo "> pnpm install --frozen-lockfile"
run corepack pnpm install --frozen-lockfile --reporter=append-only || fail "pnpm install stopped (see above)."
# EpicNext was built on an AtomCMS database: its migrations expect AtomCMS' tables (such as
# website_housekeeping_permissions). The ones the hotel database lacks come from its own base schema.
db=$(grep -sE '^db.database=' {{EmulatorDirectory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
[ -n "$db" ] || fail "No db.database in {{EmulatorDirectory}}/config.ini: run the Emulator step first."
draft=drizzle/drafts/0000_premium_spirit.sql
if [ -f "$draft" ]; then
  echo "> Base tables from $draft (only the ones $db does not have; the emulator's stay as they are)"
  tmp=$(mktemp -d)
  mariadb -N -e 'SHOW TABLES' "$db" > "$tmp/existing"
  mariadb -N -e 'SELECT migration FROM cms_migrations' "$db" > "$tmp/applied" 2>/dev/null || : > "$tmp/applied"
  node - "$draft" drizzle/migrations "$tmp/existing" "$tmp/applied" > "$tmp/base.sql" <<'JS' || { rm -rf "$tmp"; fail "Reading $draft failed (see above)."; }
{{BaseSchemaScript}}
JS
  if [ -s "$tmp/base.sql" ]; then
    mariadb "$db" < "$tmp/base.sql" || { rm -rf "$tmp"; fail "Creating the base tables in $db failed (see above)."; }
  fi
  rm -rf "$tmp"
else
  echo "  (no $draft in this version: the migrations make all tables)"
fi
# The settings checks the build runs, in seconds instead of after minutes of building.
echo "> Checking .env against EpicNext's settings (src/env.ts)"
check=$(mktemp --suffix=.mts)
cat > "$check" <<'TS'
await import("{{Directory}}/scripts/load-env.ts");
try {
	await import("{{Directory}}/src/env.ts");
	console.log("  .env OK");
} catch (error) {
	const issues = (error as { issues?: { path: string[]; message: string }[] }).issues;
	for (const issue of issues ?? []) console.log(`  ${issue.path.join(".")}: ${issue.message}`);
	if (!issues) console.log(`  ${(error as Error).message}`);
	process.exit(1);
}
TS
run corepack pnpm exec tsx "$check" || { rm -f "$check"; fail "{{Directory}}/.env does not pass EpicNext's checks (see above): fix those settings and run this step again."; }
rm -f "$check"
# New passwords are Argon2id hashes of about 97 characters; Polaris (like Arcturus) makes
# users.password VARCHAR(64), so registration failed. The emulator never reads the password.
len=$(mariadb -N "$db" -e "SELECT CHARACTER_MAXIMUM_LENGTH FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'users' AND COLUMN_NAME = 'password'")
if [ -n "$len" ] && [ "$len" -lt 255 ]; then
  mariadb "$db" -e 'ALTER TABLE `users` MODIFY `password` VARCHAR(255) NOT NULL' || fail "Widening users.password failed (see above)."
  echo "> users.password: VARCHAR($len) -> VARCHAR(255), room for Argon2id hashes"
fi
echo "> pnpm db:migrate (only the CMS' own tables; the emulator's stay as they are)"
run corepack pnpm db:migrate || fail "pnpm db:migrate stopped (see above)."
# Settings this server decides, unless staff already set them in Admin -> CMS Settings (OLD: values
# that are replaced too, such as EpicNext's own defaults).
setting_default() {
  v=$(printf '%s' "$2" | sed "s/'/''/g")
  old=$(printf '%s' "$3" | sed "s/'/''/g")
  mariadb "$db" -e "UPDATE website_settings SET value = '$v' WHERE \`key\` = '$1' AND (value = '' OR value = '$old');
    INSERT INTO website_settings (\`key\`, value) SELECT '$1', '$v' FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM website_settings WHERE \`key\` = '$1');" ||
    { echo "  WARNING: could not set $1 (set it in Admin -> CMS Settings: $2)"; return 0; }
  echo "  $1: $(mariadb -N "$db" -e "SELECT value FROM website_settings WHERE \`key\` = '$1' LIMIT 1" 2>/dev/null)"
}
# The /client page frames Octane on /client/ of this site (the hotel site), with ?sso=<ticket>.
setting_default nitro_client_url /client/index.html /nitro-client/
# The folder served on /gamedata, which the CMS reads and imports assets into.
setting_default gamedata_root "$g" /var/www/Gamedata
# A build is skipped when the code, .env and Node.js are what the current build was made from
# (a re-run that only changed settings, or an update without new commits).
if [ -f {{SourceInfo}} ]; then
  tree=$(git -c safe.directory={{SourceDirectory}} -C {{SourceDirectory}} rev-parse "HEAD:$(sed -n 3p {{SourceInfo}})" 2>/dev/null || true)
else
  tree=$(git -c safe.directory="$PWD" rev-parse 'HEAD^{tree}' 2>/dev/null || true)
fi
stamp="$tree $(sha256sum .env | cut -c1-16) $(node -v)"
if [ -n "$tree" ] && [ -f .next/BUILD_ID ] && [ "$(cat .next/.all-in-1-build 2>/dev/null)" = "$stamp" ]; then
  echo "> pnpm build skipped: code, .env and Node.js are unchanged since build $(cat .next/BUILD_ID)"
else
avail=$(awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo)
{{StopService}}
# The CMS is stopped: the old build goes. Its cache (.next/cache) stays, so webpack only
# rebuilds what changed instead of starting from nothing ("No build cache found").
[ -d .next ] && find .next -mindepth 1 -maxdepth 1 ! -name cache -exec rm -rf {} +
need_disk 5120 "pnpm build"
echo "> pnpm build (the CMS is stopped meanwhile; ${avail} MB memory, $(free_mb) MB disk free$([ -d .next/cache ] && echo ", with the cache of the last build"))"
[ "$avail" -lt 3000 ] && echo "  WARNING: under 3 GB free: the build may run out of memory (add swap, or stop other services for the build)."
# The build runs in a cgroup of its own (scripts/with-memory-cap.sh): leave 1 GB for MariaDB, the emulator and NGINX.
if [ -d /run/systemd/system ]; then export CMS_MEMORY_CAP_RESERVE=1g; else export CMS_MEMORY_CAP_BACKEND=none; echo "  (no systemd: the build runs without a memory cap)"; fi
# The code was type-checked before it was published; the build skips its own check (about 20 s).
export CMS_SKIP_BUILD_TYPECHECK=1
run corepack pnpm build || fail "pnpm build stopped (see above). The CMS stays stopped until a build succeeds."
[ -f .next/BUILD_ID ] || fail "pnpm build did not make .next/BUILD_ID."
echo "  build $(cat .next/BUILD_ID)"
[ -n "$tree" ] && echo "$stamp" > .next/.all-in-1-build
# The build cache only makes the next build faster: on a nearly full disk the space matters more.
if [ "$(free_mb)" -lt 3072 ]; then
  echo "  $(du -sh .next/cache 2>/dev/null | cut -f1) build cache removed: under 3 GB disk free ($(free_mb) MB)"
  rm -rf .next/cache
fi
fi
install -d public/nitro-assets public/swf storage
chown -R www-data:www-data {{Directory}}
chmod -R u+rwX,g+rX,o-w {{Directory}}"#;

/// pnpm install, its own migrations (only the CMS' tables), and the memory-capped build. The CMS is stopped for the
/// build, which replaces the .next folder it runs from.
pub async fn build(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(
        BUILD,
        &[
            ("Environment", ENVIRONMENT),
            ("Directory", DIRECTORY),
            ("Gamedata", &octane::gamedata()),
            ("OldGamedataLink", OLD_GAMEDATA_LINK),
            ("EmulatorDirectory", &emulator::directory()),
            ("SourceInfo", SOURCE_INFO),
            ("SourceDirectory", SOURCE_DIRECTORY),
            ("StopService", &stop_service()),
            ("BaseSchemaScript", BASE_SCHEMA_SCRIPT),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Building EpicNext failed", None).await
}

const START_SERVICE: &str = r#"{{Environment}}
pid=$(ss -Hltnp "sport = :{{Port}}" 2>/dev/null | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2)
# Next.js' own server process runs in the CMS folder.
if [ -n "$pid" ] && [ "$(readlink /proc/$pid/cwd)" != {{Directory}} ]; then
  ps -o pid=,user=,args= -p "$pid" | sed 's/^/  /'
  fail "Port {{Port}} is used by another program (above): EpicNext runs on 127.0.0.1:{{Port}}. Stop that program first."
fi
echo "> {{ServiceFile}}"
cat > {{ServiceFile}} <<'UNIT'
# EpicNext CMS (written by All-in-1 Installer, Addons → CMS). NGINX (cms-{{SiteName}}) passes the site here.
[Unit]
Description=EpicNext CMS
After=network-online.target mariadb.service
Wants=network-online.target

[Service]
Type=simple
User=www-data
Group=www-data
WorkingDirectory={{Directory}}
Environment=NODE_ENV=production NEXT_TELEMETRY_DISABLED=1
ExecStart={{start}}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
chmod 644 {{ServiceFile}}
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload
  systemctl enable {{Service}} >/dev/null 2>&1
  systemctl restart {{Service}}
  echo "  {{Service}} enabled and started"
else
  {{StopService}}
  touch {{LogFile}} && chown www-data {{LogFile}}
  # Only the server itself in the background, with no handle on this session's output (it would keep it open).
  cd {{Directory}}
  setsid runuser -u www-data -- env NODE_ENV=production {{start}} </dev/null >>{{LogFile}} 2>&1 &
  echo "  (no systemd on this machine: started directly, not at boot; log {{LogFile}})"
fi
for i in $(seq 1 60); do
  code=$(curl -s --noproxy '*' -o /dev/null --max-time 5 -w '%{http_code}' {{Upstream}}/ || true)
  [ -n "$code" ] && [ "$code" != 000 ] && break
  sleep 2
done
if [ -z "$code" ] || [ "$code" = 000 ]; then
  if [ -d /run/systemd/system ]; then journalctl -u {{Service}} -n 30 --no-pager 2>/dev/null | sed 's/^/  /'; else tail -30 {{LogFile}} | sed 's/^/  /'; fi
  fail "EpicNext does not answer on {{Upstream}} after 2 minutes (its log is above)."
fi
echo "  {{Upstream}}/ answers HTTP $code""#;

/// The systemd service (as www-data, on 127.0.0.1:3002, at boot), started and waited for.
pub async fn start_service(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let start = format!("/usr/bin/node {DIRECTORY}/node_modules/next/dist/bin/next start -H 127.0.0.1 -p {PORT}");
    let body = fill(
        START_SERVICE,
        &[
            ("Environment", ENVIRONMENT),
            ("Port", &PORT.to_string()),
            ("Directory", DIRECTORY),
            ("ServiceFile", SERVICE_FILE),
            ("SiteName", SITE_NAME),
            ("start", &start),
            ("Service", SERVICE),
            ("StopService", &stop_service()),
            ("LogFile", LOG_FILE),
            ("Upstream", UPSTREAM),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Starting EpicNext failed", None).await
}

const REMOVE_SERVICE: &str = r#"{{StopService}}
if [ -d /run/systemd/system ]; then systemctl disable {{Service}} >/dev/null 2>&1 || true; fi
echo "> {{Service}} stopped and off at boot"
if [ {{delete}} = yes ]; then
  rm -f {{ServiceFile}}
  [ -d /run/systemd/system ] && systemctl daemon-reload
  echo "  {{ServiceFile}} deleted"
  [ -L {{OldGamedataLink}} ] && rm -f {{OldGamedataLink}} && echo "  {{OldGamedataLink}} link deleted (the gamedata stays)"
  [ -d {{SourceDirectory}} ] && rm -rf {{SourceDirectory}} && echo "  {{SourceDirectory}} (the git checkout) deleted"
fi
true"#;

/// Stops and disables the service; with `delete` also its unit, the /var/www/Gamedata link and the git checkout.
/// (Used by the Remove CMS step.)
pub async fn remove_service(server: &dyn Server, delete: bool, log: Log<'_>) -> Result<()> {
    let body = fill(
        REMOVE_SERVICE,
        &[
            ("StopService", &stop_service()),
            ("Service", SERVICE),
            ("delete", if delete { "yes" } else { "no" }),
            ("ServiceFile", SERVICE_FILE),
            ("OldGamedataLink", OLD_GAMEDATA_LINK),
            ("SourceDirectory", SOURCE_DIRECTORY),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Stopping EpicNext failed", None).await
}

const TEST: &str = r#"url="{{Scheme}}://{{name}}"
c() { curl -s --noproxy '*' -k --max-time 20 --resolve "{{name}}:{{port}}:127.0.0.1" "$@"; }
echo "> $url/ (EpicNext)"
code=$(c -o /dev/null -w '%{http_code}' "$url/" || true)
echo "  HTTP $code"
case "$code" in
  2*|3*) ;;
  000|"") fail "Nothing answers on $url/." ;;
  502|504) fail "$url/ answers HTTP $code: NGINX does not reach EpicNext on {{Upstream}}." ;;
  *) echo "  EpicNext answers HTTP $code: see its log (journalctl -u {{Service}})." ;;
esac
echo "> $url/client (EpicNext's client page)"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/client" || true)"
echo "> $url/client/ (Octane)"
c "$url/client/" | grep -q 'configuration/bootstrap.js' || fail "$url/client/ does not serve Octane."
echo "  index.html OK"
echo "> $url/api/health (Polaris)"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/api/health" || true)""#;

/// Checks through Traefik that the CMS on / and /client, Octane on /client/ and Polaris' /api/health answer.
pub async fn test(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let (name, port) = atom_cms::traefik_target(site);
    let body = fill(TEST, &[("Scheme", site.scheme()), ("name", &name), ("port", port), ("Upstream", UPSTREAM), ("Service", SERVICE)]);
    script::run(server, &script::with_prelude(&body), log, "Testing EpicNext failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_and_branches() {
        assert_eq!(folder_problem(""), None);
        assert_eq!(folder_problem("NextCMS/epicnext-cms"), None);
        assert!(folder_problem("/NextCMS").is_some());
        assert!(folder_problem("a/../b").is_some());
        assert!(folder_problem("a/").is_some());
        assert_eq!(branch_problem(""), None);
        assert_eq!(branch_problem("feature/x"), None);
        assert!(branch_problem("-x").is_some());
        assert!(branch_problem("a..b").is_some());
        assert!(branch_problem("x/").is_some());
        assert!(branch_problem("a b").is_some());
    }

    #[test]
    fn scripts_keep_their_tabs() {
        assert!(BASE_SCHEMA_SCRIPT.contains("\n\t\t\tif (columns.has"));
        assert!(BUILD.contains("\n\tprocess.exit(1);\n"));
        assert!(GET_FROM_FOLDER.contains(&format!("--delete{}--exclude=/.env ", " ".repeat(15))));
    }
}
