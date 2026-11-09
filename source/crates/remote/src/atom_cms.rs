//! AtomCMS: clone, .env, composer, migrations and settings, the theme build, Octane on /client and the tests.
//! (AtomCmsInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use aio_core::ServerLayout;

pub const REPOSITORY: &str = "https://github.com/ObjectRetros/atomcms.git";
pub const SITE_NAME: &str = "atomcms";

pub fn directory() -> String {
    ServerLayout::current().atom_cms.clone()
}

pub fn public() -> String {
    format!("{}/public", directory())
}

// ---------------------------------------------------------------------------------------------------------------

use aio_core::DatabaseCredentials;

use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::{self, WebAccessSettings};
use crate::{cms_site, emulator, harden, hotel_site, octane};

/// AtomCMS state before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomCmsState {
    pub site: Option<WebAccessSettings>,
    pub php_version: Option<String>,
    pub composer_present: bool,
    pub node_version: Option<String>,
    pub octane_built: bool,
    pub emulator_configured: bool,
    pub hotel_name: Option<String>,
    pub vendor_present: bool,
    pub theme_built: bool,
    pub migrated: bool,
    pub site_serves_atom: bool,
    pub installation_key: Option<String>,
    pub installation_completed: bool,
}

impl AtomCmsState {
    pub fn is_complete(&self) -> bool {
        self.site.as_ref().is_some_and(|s| s.atom_cms) && self.vendor_present && self.theme_built && self.migrated && self.site_serves_atom
    }
}

// Node.js 26 comes from /usr/bin. Composer and artisan run without prompts.
const ENVIRONMENT: &str =
    "export PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root CI=1 NO_COLOR=1 COMPOSER_ALLOW_SUPERUSER=1 COMPOSER_NO_INTERACTION=1 COMPOSER_PROCESS_TIMEOUT=1800";

// Run artisan as the PHP-FPM user so the caches and logs it writes stay writable for the site.
const ARTISAN: &str = "runuser -u www-data -- php artisan";

/// website_settings that point the CMS at this server's layout. The rcon_port setting is copied separately from the
/// emulator's config.ini.
pub fn website_settings(hotel_name: &str) -> Vec<(String, String)> {
    let gamedata = octane::gamedata();
    [
        ("hotel_name", hotel_name.to_string()),
        ("theme", "atom".into()),
        ("nitro_path", "/client".into()),
        ("nitro_external_texts_file", format!("{gamedata}/config/ExternalTexts.json")),
        ("badges_path", "/gamedata/c_images/album1584".into()),
        ("badge_path_filesystem", format!("{gamedata}/c_images/album1584")),
        ("group_badge_path", "/gamedata/c_images/groupbadge/badge".into()),
        ("furniture_icons_path", "/gamedata/icons".into()),
        ("ads_picture_path", "/gamedata/custom".into()),
        ("ads_path_filesystem", format!("{gamedata}/custom")),
        ("rcon_ip", "127.0.0.1".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

const INSPECT: &str = r#"{{Environment}}
[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
echo "php=$(php8.5 -r 'echo PHP_VERSION;' 2>/dev/null)"
echo "composer=$(command -v composer >/dev/null && echo yes)"
echo "node=$(node -v 2>/dev/null)"
echo "octane=$([ -f {{Dist}}/index.html ] && echo yes)"
db=$(grep -sE '^db.database=' {{EmulatorDirectory}}/config.ini | cut -d= -f2)
echo "emulator=$([ -n "$db" ] && echo yes)"
d={{Directory}}
echo "name=$(grep -sE '^APP_NAME=' "$d/.env" | head -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/')"
echo "vendor=$([ -f "$d/vendor/autoload.php" ] && echo yes)"
echo "theme=$([ -f "$d/public/build/manifest.json" ] && echo yes)"
dbname=$(grep -sE '^DB_DATABASE=' "$d/.env" | cut -d= -f2)
if [ -n "$dbname" ]; then
  echo "migrated=$(mariadb -N "$dbname" -e "SELECT 1 FROM website_installation LIMIT 1" >/dev/null 2>&1 && echo yes)"
  echo "key=$(mariadb -N "$dbname" -e "SELECT installation_key FROM website_installation ORDER BY id LIMIT 1" 2>/dev/null)"
  echo "completed=$(mariadb -N "$dbname" -e "SELECT 1 FROM website_installation WHERE completed = 1 LIMIT 1" 2>/dev/null)"
fi
# Its own NGINX site (enabled), or the hotel site from before AtomCMS had one.
echo "site=$( { [ -L /etc/nginx/sites-enabled/cms-{{SiteName}} ] && grep -qs 'root {{Public}};' {{CmsSiteFile}}; } || grep -qs 'root {{Public}};' {{HotelSiteFile}} && echo yes)""#;

pub async fn inspect(server: &dyn Server) -> Result<AtomCmsState> {
    let script = fill(
        INSPECT,
        &[
            ("Environment", ENVIRONMENT),
            ("TraefikDirectory", web_access::traefik_directory()),
            ("Dist", &octane::dist()),
            ("EmulatorDirectory", &emulator::directory()),
            ("Directory", &directory()),
            ("SiteName", SITE_NAME),
            ("Public", &public()),
            ("CmsSiteFile", &cms_site::file(SITE_NAME)),
            ("HotelSiteFile", hotel_site::file()),
        ],
    );
    let result = server.run_as_root(&script).await?;
    let values = ScriptValues::parse(&result.output);
    Ok(AtomCmsState {
        site: WebAccessSettings::from_hotel_env(&values),
        php_version: values.string("php"),
        composer_present: values.yes("composer"),
        node_version: values.string("node"),
        octane_built: values.yes("octane"),
        emulator_configured: values.yes("emulator"),
        hotel_name: values.string("name"),
        vendor_present: values.yes("vendor"),
        theme_built: values.yes("theme"),
        migrated: values.yes("migrated"),
        site_serves_atom: values.yes("site"),
        installation_key: values.string("key"),
        installation_completed: values.get("completed") == Some("1"),
    })
}

// Double-quotes a .env value and escapes anything phpdotenv would expand or read as the end of the string.
fn env_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"))
}

const CONFIGURE_ENV: &str = r#"cd {{Directory}}
if [ -f .env ]; then
  cp .env ".env.bak-$(date +%Y%m%d-%H%M%S)"
  echo "> {{Directory}}/.env (the old one is kept as .env.bak-<time>)"
else
  cp .env.example .env
  echo "> {{Directory}}/.env from .env.example"
fi
env_set() {
  if grep -q "^$1=" .env; then
    KEY="$1" VALUE="$2" awk 'index($0, ENVIRON["KEY"] "=") == 1 { print ENVIRON["KEY"] "=" ENVIRON["VALUE"]; next } { print }' .env > .env.new
    cat .env.new > .env && rm -f .env.new
  else
    printf '%s=%s\n' "$1" "$2" >> .env
  fi
  case "$1" in *PASSWORD*) echo "  $1=***" ;; *) echo "  $1=$2" ;; esac
}
{{sets}}
rcon=$(grep -sE '^rcon.port=' {{EmulatorDirectory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
env_set RCON_PORT "${rcon:-3001}"
grep -qE '^DB_PASSWORD=.+' .env && ! grep -qE '^DB_USERNAME=atomcms$' .env ||
  fail "The database login for AtomCMS is unknown: the MariaDB passwords of this server are not saved on this computer (configuration/database/<server>/mariadb.txt). Run the MariaDB step from this computer first."
chmod 640 .env
chown www-data:www-data .env"#;

/// Creates .env from .env.example when it is missing and sets the keys this server decides. Without saved credentials
/// the database login already in .env is kept.
pub async fn configure_env(
    server: &dyn Server,
    site: &WebAccessSettings,
    hotel_name: &str,
    credentials: Option<&DatabaseCredentials>,
    log: Log<'_>,
) -> Result<()> {
    let host = harden::whitelist_host(site);
    let secure = if site.https { "true" } else { "false" };
    let mut values: Vec<(&str, String)> = vec![
        ("APP_NAME", env_value(hotel_name)),
        ("APP_ENV", "production".into()),
        ("APP_DEBUG", "false".into()),
        ("APP_URL", site.url()),
        ("LOG_LEVEL", "warning".into()),
        ("DB_CONNECTION", "mariadb".into()),
        ("DB_HOST", "127.0.0.1".into()),
        ("DB_PORT", "3306".into()),
        ("FORCE_HTTPS", secure.into()),
        ("SESSION_SECURE_COOKIE", secure.into()),
        ("SANCTUM_STATEFUL_DOMAINS", if site.is_ip_address() { site.address.clone() } else { format!("{host},www.{host}") }),
        ("RCON_HOST", "127.0.0.1".into()),
    ];
    if let Some(credentials) = credentials {
        values.push(("DB_DATABASE", credentials.database.clone()));
        values.push(("DB_USERNAME", credentials.cms_user.clone()));
        values.push(("DB_PASSWORD", env_value(&credentials.cms_password)));
    }
    let sets = values.iter().map(|(k, v)| format!("env_set {k} {}", shell_quote(v))).collect::<Vec<_>>().join("\n");
    let body = fill(CONFIGURE_ENV, &[("Directory", &directory()), ("EmulatorDirectory", &emulator::directory()), ("sets", &sets)]);
    script::run(server, &script::with_prelude(&body), log, "Writing AtomCMS' .env failed", None).await
}

const COMPOSER_INSTALL: &str = r#"{{Environment}}
set -o pipefail
command -v composer >/dev/null || fail "Composer is not installed. Run the Dev tools step first."
cd {{Directory}}
echo "> composer install   ($(composer --version 2>/dev/null | awk '{print "Composer " $3}'), PHP $(php -r 'echo PHP_VERSION;'))"
rc=$(mktemp)
(composer install --no-interaction --no-progress --optimize-autoloader 2>&1; echo $? > "$rc") \
  | grep -vE '^  - (Downloading|Installing|Syncing) |^    (Failed to download|Now trying)|^\s*$' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "composer install stopped with exit code $code (see above)."
[ -f vendor/autoload.php ] || fail "composer install did not create vendor/autoload.php."
echo "  $(ls vendor | wc -l) vendors installed"
chown -R www-data:www-data {{Directory}}
chmod -R ug+rwX storage bootstrap/cache"#;

/// Runs composer install with dev packages, because the seeder uses Faker for the first article's author.
pub async fn composer_install(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(COMPOSER_INSTALL, &[("Environment", ENVIRONMENT), ("Directory", &directory())]);
    script::run(server, &script::with_prelude(&body), log, "composer install failed", None).await
}

const MIGRATE: &str = r#"{{Environment}}
set -o pipefail
cd {{Directory}}
db=$(grep -E '^DB_DATABASE=' .env | cut -d= -f2)
[ -n "$db" ] || fail "DB_DATABASE is not set in {{Directory}}/.env."
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "Database $db does not exist (run the MariaDB step first)."

if ! grep -qE '^APP_KEY=.+' .env; then
  echo "> php artisan key:generate"
  {{Artisan}} key:generate --force --no-interaction 2>&1 | sed 's/^ */  /'
fi
echo "> php artisan storage:link"
{{Artisan}} storage:link --force --no-interaction 2>&1 | sed 's/^ */  /' || true

first=yes
mariadb -N "$db" -e "SELECT 1 FROM website_installation LIMIT 1" >/dev/null 2>&1 && first=
if [ -n "$first" ]; then
  echo "> php artisan migrate --seed   (first install: AtomCMS' website_* tables next to the emulator's)"
  seed=--seed
else
  echo "> php artisan migrate   (AtomCMS is already installed: only new migrations)"
  seed=
fi
rc=$(mktemp)
({{Artisan}} migrate --force --no-interaction $seed 2>&1; echo $? > "$rc") | grep -vE '^\s*$| RUNNING\s*$' | sed 's/^ */  /' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "php artisan migrate stopped with exit code $code (see above)."

if [ -n "$first" ]; then
  rcon=$(grep -sE '^rcon.port=' {{EmulatorDirectory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
  echo "> website_settings: hotel name, theme atom, nitro_path /client and the gamedata paths"
  mariadb "$db" <<'SQL'
{{settings}}
SQL
  mariadb "$db" -e "UPDATE website_settings SET \`value\` = '${rcon:-3001}' WHERE \`key\` = 'rcon_port'"
  mariadb -N "$db" -e "SELECT CONCAT('  ', \`key\`, ' = ', \`value\`) FROM website_settings WHERE \`key\` IN ('hotel_name', 'theme', 'nitro_path', 'badges_path', 'group_badge_path', 'furniture_icons_path', 'rcon_port')"
fi
{{Artisan}} optimize:clear --no-interaction >/dev/null 2>&1 || true
echo "FIRST=$first""#;

/// Runs key:generate once, storage:link and migrate, plus the seeders and website settings on the first install.
/// Returns whether this was the first install.
pub async fn migrate(server: &dyn Server, hotel_name: &str, log: Log<'_>) -> Result<bool> {
    let settings = website_settings(hotel_name)
        .iter()
        .map(|(k, v)| format!("UPDATE website_settings SET `value` = {} WHERE `key` = {};", script::sql_literal(v), script::sql_literal(k)))
        .collect::<Vec<_>>()
        .join("\n");
    let body = fill(
        MIGRATE,
        &[
            ("Environment", ENVIRONMENT),
            ("Directory", &directory()),
            ("Artisan", ARTISAN),
            ("EmulatorDirectory", &emulator::directory()),
            ("settings", &settings),
        ],
    );
    let result = script::run_capturing(server, &script::with_prelude(&body), log, "Setting up AtomCMS' database failed", &["FIRST"], None).await?;
    Ok(result.yes("FIRST"))
}

const INSTALLATION_KEY: &str = r#"cd {{Directory}}
db=$(grep -E '^DB_DATABASE=' .env | cut -d= -f2)
key() { mariadb -N "$db" -e "SELECT installation_key FROM website_installation ORDER BY id LIMIT 1" 2>/dev/null; }
url="{{Scheme}}://{{name}}"
# A cached "installed" flag would hide the web installer while the database has no finished installation.
if ! mariadb -N "$db" -e "SELECT 1 FROM website_installation WHERE completed = 1" 2>/dev/null | grep -q 1; then
  {{Artisan}} cache:forget app_installed >/dev/null 2>&1 || true
  {{Artisan}} cache:forget installation_record >/dev/null 2>&1 || true
fi
if [ -z "$(key)" ]; then
  echo "> $url/installation (AtomCMS creates its installation key on the first visit)"
  curl -s --noproxy '*' -k --max-time 20 --resolve "{{name}}:{{port}}:127.0.0.1" -o /dev/null -w '  HTTP %{http_code}\n' "$url/installation" || true
fi
if [ -z "$(key)" ]; then
  echo "  No key yet: adding one to website_installation"
  mariadb "$db" -e "INSERT INTO website_installation (step, completed, installation_key, created_at, updated_at)
    SELECT 0, 0, UUID(), NOW(), NOW() FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM website_installation)"
fi
k=$(key)
[ -n "$k" ] || fail "AtomCMS has no installation key (SELECT installation_key FROM website_installation; is empty)."
completed=$(mariadb -N "$db" -e "SELECT 1 FROM website_installation WHERE completed = 1 LIMIT 1" 2>/dev/null)
echo ""
if [ "$completed" = 1 ]; then
  echo "  AtomCMS' web installer is already finished (installation key $k)."
else
  echo "  ============================================================"
  echo "  AtomCMS installation key:  $k"
  echo "  Open $url and paste it in the web installer."
  echo "  ============================================================"
fi
echo "KEY=$k"
echo "COMPLETED=$completed""#;

/// The host name curl asks Traefik for, and its port.
pub(crate) fn traefik_target(site: &WebAccessSettings) -> (String, &'static str) {
    let name = if site.is_ip_address() { "127.0.0.1".to_string() } else { site.address.clone() };
    (name, if site.https { "443" } else { "80" })
}

/// Gets the key AtomCMS' web installer asks for. AtomCMS creates it on the first visit, so this opens /installation
/// through Traefik and inserts a key into website_installation if there is still none. Returns (key, completed).
pub async fn installation_key(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<(Option<String>, bool)> {
    let (name, port) = traefik_target(site);
    let body = fill(
        INSTALLATION_KEY,
        &[("Directory", &directory()), ("Scheme", site.scheme()), ("name", &name), ("port", port), ("Artisan", ARTISAN)],
    );
    let result =
        script::run_capturing(server, &script::with_prelude(&body), log, "Reading AtomCMS' installation key failed", &["KEY", "COMPLETED"], None)
            .await?;
    Ok((result.string("KEY"), result.get("COMPLETED") == Some("1")))
}

const BUILD_THEME: &str = r#"{{Environment}}
set -o pipefail
command -v node >/dev/null && command -v npm >/dev/null || fail "Node.js is missing in /usr/bin. Run the Dev tools step first."
cd {{Directory}}
echo "> npm ci   (node $(node -v), npm $(npm -v))"
rc=$(mktemp)
(npm ci --no-audit --no-fund 2>&1; echo $? > "$rc") | grep -vE '^npm (warn|WARN) (deprecated|EBADENGINE)|^\s*$' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "npm ci stopped with exit code $code (see above)."
echo "> npm run build:atom"
(npm run build:atom </dev/null 2>&1; echo $? > "$rc") | grep -vE '^public/build|^\s*$|computing gzip|transforming|rendering chunks' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "npm run build:atom stopped with exit code $code (see above)."
echo "  public/build $(du -sh public/build | cut -f1), public/build-housekeeping $(du -sh public/build-housekeeping 2>/dev/null | cut -f1)"
chown -R www-data:www-data public/build public/build-housekeeping 2>/dev/null || true"#;

/// Runs npm ci and npm run build:atom, which also builds the housekeeping theme.
pub async fn build_theme(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(BUILD_THEME, &[("Environment", ENVIRONMENT), ("Directory", &directory())]);
    script::run(server, &script::with_prelude(&body), log, "Building the AtomCMS theme failed", None).await
}

const TEST: &str = r#"url="{{Scheme}}://{{name}}"
c() { curl -s --noproxy '*' -k --max-time 15 --resolve "{{name}}:{{port}}:127.0.0.1" "$@"; }
echo "> $url/ (AtomCMS)"
code=$(c -o /dev/null -w '%{http_code} %{redirect_url}' "$url/" || true)
echo "  HTTP $code"
case "$code" in 200*|302*) ;; *) tail -5 {{Directory}}/storage/logs/laravel.log 2>/dev/null | cut -c1-300 | sed 's/^/  /'; fail "AtomCMS does not answer on $url/ (HTTP '$code')." ;; esac
echo "> $url/client/ (Octane)"
c "$url/client/" | grep -q 'configuration/bootstrap.js' || fail "$url/client/ does not serve Octane."
echo "  index.html OK"
echo "> $url/api/online-count (AtomCMS' API)"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/api/online-count" || true)"
echo "> $url/api/health (Polaris)"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/api/health" || true)""#;

/// Checks through Traefik that the site, /client/, AtomCMS' /api and Polaris' /api/health answer.
pub async fn test(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let (name, port) = traefik_target(site);
    let body = fill(TEST, &[("Scheme", site.scheme()), ("name", &name), ("port", port), ("Directory", &directory())]);
    script::run(server, &script::with_prelude(&body), log, "Testing AtomCMS failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_are_quoted() {
        assert_eq!(env_value(r#"My "Hotel" $5 \o/"#), r#""My \"Hotel\" \$5 \\o/""#);
    }

    #[test]
    fn complete_needs_everything() {
        let mut state = AtomCmsState {
            site: Some(WebAccessSettings { atom_cms: true, ..WebAccessSettings::new("example.com", true, false) }),
            php_version: None,
            composer_present: false,
            node_version: None,
            octane_built: false,
            emulator_configured: false,
            hotel_name: None,
            vendor_present: true,
            theme_built: true,
            migrated: true,
            site_serves_atom: true,
            installation_key: None,
            installation_completed: false,
        };
        assert!(state.is_complete());
        state.theme_built = false;
        assert!(!state.is_complete());
        state.theme_built = true;
        state.site = Some(WebAccessSettings::new("example.com", true, false));
        assert!(!state.is_complete());
    }
}
