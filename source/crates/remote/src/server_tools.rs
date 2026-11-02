//! The Tools page's server side: restarts, database backup / import, the firewall, recover SQL password, system and
//! hotel updates. (ServerTools.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use aio_core::ServerLayout;

use crate::server::{Log, Result, Server};

/// Database dumps made by the tools. Only root can read them because they hold every password hash.
pub fn backup_directory() -> String {
    ServerLayout::current().backups.clone()
}

pub const KEPT_BACKUPS: usize = 10;

/// Sets $db: the hotel database from the emulator's config.ini, or the layout's database. (DatabaseName)
pub fn database_name() -> String {
    format!(
        "db=$(grep -sE '^db.database=' {}/config.ini | cut -d= -f2 | tr -d '[:space:]')\ndb=${{db:-{}}}",
        crate::emulator::directory(),
        ServerLayout::current().database
    )
}

/// docker compose pull and up -d in a service's folder, and a check that it runs (again). (UpdateImageAsync)
pub async fn update_image(
    server: &dyn Server,
    name: &str,
    directory: &str,
    container: &str,
    version_command: &str,
    ready_check: &str,
    log: Log<'_>,
) -> Result<()> {
    let body = fill(
        UPDATE_IMAGE,
        &[
            ("directory", directory),
            ("name", name),
            ("versionCommand", version_command),
            ("readyCheck", ready_check),
            ("container", container),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, &format!("Updating {name} failed"), None).await
}

// ---------------------------------------------------------------------------------------------------------------

use crate::script::{self, fill};
use crate::server::{shell_quote, ServerError};
use crate::{atom_cms, crowdsec, emulator, hotel_site, web_access};

/// HardenInstaller.UfwLog (not exported by harden.rs).
const UFW_LOG: &str = "/var/log/ufw.log";
/// Fail2BanInstaller.LogFile (not exported by fail2ban.rs).
const FAIL2BAN_LOG: &str = "/var/log/fail2ban.log";

/// A database dump on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseBackup {
    pub remote_path: String,
    pub database: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorUpdate {
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub changed: bool,
}

const SYSTEMD: &str = "[ -d /run/systemd/system ]";

const UPDATE_IMAGE: &str = r#"d={{directory}}
[ -f "$d/compose.yaml" ] || fail "{{name}} is not set up by the installer on this server ($d/compose.yaml is missing)."
before=$({{versionCommand}})
echo "> docker compose pull ({{name}} ${before:-?})"
if ! compose_in "$d" pull >"$d/.pull.log" 2>&1; then
  grep -vE '^ *$' "$d/.pull.log" | tail -3 | sed 's/^/  /'
  rm -f "$d/.pull.log"
  fail "Docker Hub did not answer (see above): nothing was changed, {{name}} keeps running ${before:-as it was}. Try again later."
fi
grep -vE '^ *$' "$d/.pull.log" | tail -3 | sed 's/^/  /'
rm -f "$d/.pull.log"
echo "> docker compose up -d   (recreates {{name}} only when its image changed)"
compose_in "$d" up -d 2>&1 | grep -v '^ *$' | sed 's/^/  /'
for i in $(seq 1 30); do {{readyCheck}} && break; sleep 2; done
{{readyCheck}} || { docker logs --tail 20 {{container}} 2>&1 | sed 's/^/  /'; fail "{{name}} does not run after the update (see its log above)."; }
after=$({{versionCommand}})
if [ "$before" = "$after" ]; then echo "  {{name}} $after runs (it had the newest image)"; else echo "  {{name}} ${before:-?} → $after runs"; fi
docker image prune -f >/dev/null 2>&1 || true"#;

const RESTART_EMULATOR: &str = r#"d={{Directory}}
log={{LogDirectory}}/emulator.log
[ -x "$d/emulator" ] || fail "The emulator is not set up on this server (no $d/emulator). Run the Emulator step first."
{{ControlFunctions}}
echo "> Restarting the emulator"
start_emulator
wait_loaded
grep -E 'Started (Game|RCON) Server' "$log" | sed 's/^.*| /  /' || true"#;

pub async fn restart_emulator(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(
        RESTART_EMULATOR,
        &[
            ("Directory", &emulator::directory()),
            ("LogDirectory", &emulator::log_directory()),
            ("ControlFunctions", &emulator::control_functions()),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Restarting the emulator failed", None).await
}

const RESTART_MARIADB: &str = r#"command -v mariadb >/dev/null || fail "MariaDB is not installed on this server."
echo "> Restarting MariaDB"
if {{Systemd}}; then systemctl restart mariadb; else service mariadb restart; fi
for i in $(seq 1 30); do mariadb -N -e 'SELECT 1' >/dev/null 2>&1 && break; sleep 1; done
mariadb -N -e 'SELECT 1' >/dev/null 2>&1 || fail "MariaDB does not answer after the restart (journalctl -u mariadb)."
echo "  MariaDB $(mariadb -N -e 'SELECT VERSION()') is running""#;

pub async fn restart_mariadb(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(RESTART_MARIADB, &[("Systemd", SYSTEMD)]);
    script::run(server, &script::with_prelude(&body), log, "Restarting MariaDB failed", None).await
}

const RESTART_NGINX: &str = r#"command -v nginx >/dev/null || fail "NGINX is not installed on this server."
echo "> nginx -t"
nginx -t 2>&1 | sed 's/^/  /'
nginx -t >/dev/null 2>&1 || fail "nginx -t refuses the configuration (see above): NGINX was not restarted."
if command -v php-fpm8.5 >/dev/null 2>&1; then
  echo "> Restarting PHP-FPM 8.5"
  if {{Systemd}}; then systemctl restart php8.5-fpm; else service php8.5-fpm restart; fi
fi
echo "> Restarting NGINX"
if {{Systemd}}; then systemctl restart nginx; else nginx -s stop 2>/dev/null; sleep 1; nginx; fi
sleep 1
code=$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' http://127.0.0.1:{{Port}}/ || true)
echo "  http://127.0.0.1:{{Port}}/ answers HTTP $code"
case "$code" in 200|302|403) ;; *) fail "NGINX does not answer on 127.0.0.1:{{Port}} after the restart." ;; esac"#;

/// Restarts NGINX, and PHP-FPM too when it is installed because AtomCMS runs on it.
pub async fn restart_nginx(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(RESTART_NGINX, &[("Systemd", SYSTEMD), ("Port", hotel_site::PORT)]);
    script::run(server, &script::with_prelude(&body), log, "Restarting NGINX failed", None).await
}

const RESTART_TRAEFIK: &str = r#"docker inspect traefik >/dev/null 2>&1 || fail "There is no Traefik container on this server. Run the Web access step first."
# A new container from compose.yaml: it starts with the newest downloaded traefik image, where docker restart
# would keep the image the old container was made from. The routes and certificates are files, so they stay.
if [ -f {{TraefikDirectory}}/compose.yaml ]; then
  echo "> docker compose up -d --force-recreate (Traefik, with the newest downloaded image)"
  docker compose -f {{TraefikDirectory}}/compose.yaml up -d --force-recreate 2>&1 | grep -v '^ *$' | sed 's/^/  /'
else
  echo "> docker restart traefik"
  docker restart traefik >/dev/null
fi
for i in $(seq 1 20); do ss -Hltn 'sport = :80' | grep -q . && break; sleep 1; done
ss -Hltn 'sport = :80' | grep -q . || { docker logs --tail 20 traefik 2>&1 | sed 's/^/  /'; fail "Traefik does not listen on port 80 after the restart."; }
echo "  $(docker exec traefik traefik version 2>/dev/null | awk '/^Version/ {print "Traefik " $2}') is running""#;

pub async fn restart_traefik(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(RESTART_TRAEFIK, &[("TraefikDirectory", web_access::TRAEFIK_DIRECTORY)]);
    script::run(server, &script::with_prelude(&body), log, "Restarting Traefik failed", None).await
}

/// Pulls the newest traefik:v3 image and recreates the container when it changed. Its routes, settings and
/// certificates stay; the players are disconnected for a few seconds.
pub async fn update_traefik(server: &dyn Server, log: Log<'_>) -> Result<()> {
    update_image(
        server,
        "Traefik",
        web_access::TRAEFIK_DIRECTORY,
        "traefik",
        "docker exec traefik traefik version 2>/dev/null | awk '/^Version/ {print $2}'",
        "ss -Hltn 'sport = :80' | grep -q .",
        log,
    )
    .await
}

const BACKUP_DATABASE: &str = r#"set -o pipefail
{{DatabaseName}}
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "Database $db does not exist or MariaDB does not run."
install -d -m 700 {{BackupDirectory}}
file={{BackupDirectory}}/$db-$(date +%Y%m%d-%H%M%S).sql.gz
echo "> mariadb-dump $db ($(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db'") tables) > $file"
umask 077
start=$(date +%s)
mariadb-dump --single-transaction --quick --routines --triggers --events --hex-blob --databases "$db" | gzip -6 > "$file" ||
  { rm -f "$file"; fail "mariadb-dump stopped (see above)."; }
gzip -t "$file" || fail "The backup $file is damaged."
echo "  $(du -h "$file" | cut -f1) in $(( $(date +%s) - start )) seconds"
# Keep the newest backups of this database, not counting the safety backups made before an import.
ls -1t {{BackupDirectory}}/"$db"-2*.sql.gz 2>/dev/null | tail -n +{{KeptBackupsPlusOne}} | while read -r old; do
  rm -f "$old"; echo "  Removed the old backup $old"
done
echo "FILE=$file"
echo "DB=$db"
echo "BYTES=$(stat -c %s "$file")""#;

/// Dumps the hotel database with CREATE DATABASE and USE to a .sql.gz and keeps the newest [`KEPT_BACKUPS`].
pub async fn backup_database(server: &dyn Server, log: Log<'_>) -> Result<DatabaseBackup> {
    let body = fill(
        BACKUP_DATABASE,
        &[
            ("DatabaseName", &database_name()),
            ("BackupDirectory", &backup_directory()),
            ("KeptBackupsPlusOne", &(KEPT_BACKUPS + 1).to_string()),
        ],
    );
    let result =
        script::run_capturing(server, &script::with_prelude(&body), log, "The database backup failed", &["FILE", "DB", "BYTES"], None).await?;
    Ok(DatabaseBackup {
        remote_path: result.or_empty("FILE").to_string(),
        database: result.or_empty("DB").to_string(),
        bytes: result.long("BYTES"),
    })
}

const IMPORT_DATABASE: &str = r#"set -o pipefail
{{DatabaseName}}
f={{RemoteFile}}
[ -s "$f" ] || fail "The uploaded file $f is missing or empty."
# The upload is only needed for this import, whatever happens.
chmod 600 "$f"
trap 'rm -f "$f"' EXIT
if gzip -t "$f" 2>/dev/null; then read_dump() { gunzip -c "$f"; }; else read_dump() { cat "$f"; }; fi
# No pipefail here because head ends the pipe early on purpose.
(set +o pipefail; read_dump | head -c 65536 | grep -qiE '^(CREATE TABLE|INSERT INTO|-- MariaDB dump|-- MySQL dump)') ||
  fail "$(basename "$f") does not look like a SQL dump (no CREATE TABLE or INSERT INTO near the start)."

echo "> Safety backup of the current $db first"
install -d -m 700 {{BackupDirectory}}
safety={{BackupDirectory}}/$db-before-import-$(date +%Y%m%d-%H%M%S).sql.gz
if mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1; then
  (umask 077; mariadb-dump --single-transaction --quick --routines --triggers --events --hex-blob --databases "$db" | gzip -6 > "$safety")
  echo "  $safety ($(du -h "$safety" | cut -f1))"
else
  echo "  $db does not exist yet: nothing to back up"
  safety=
fi

d={{Directory}}
log={{LogDirectory}}/emulator.log
{{ControlFunctions}}
emulator=
if [ -x "$d/emulator" ]; then
  emulator=yes
  echo "> Stopping the emulator"
  stop_emulator
fi

charset=$(mariadb -N -e "SELECT CONCAT(default_character_set_name, ' ', default_collation_name) FROM information_schema.schemata WHERE schema_name = '$db'")
set -- ${charset:-utf8mb4 utf8mb4_unicode_ci}
echo "> Recreating $db (character set $1, collation $2)"
mariadb -e "DROP DATABASE IF EXISTS \`$db\`; CREATE DATABASE \`$db\` CHARACTER SET $1 COLLATE $2"

echo "> Importing $(basename "$f") ($(du -h "$f" | cut -f1)) into $db"
start=$(date +%s)
if ! read_dump | sed -E "/^(CREATE DATABASE|USE) /s/\`[^\`]+\`/\`$db\`/" | mariadb "$db"; then
  echo "  The import stopped."
  [ -n "$safety" ] && echo "  The database as it was before is in $safety (Import full database with that file restores it)."
  fail "Importing $(basename "$f") failed (see above). The emulator was not started."
fi
echo "  $(mariadb -N -e "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$db'") tables in $(( $(date +%s) - start )) seconds"

if [ -d {{AtomCmsDirectory}}/vendor ]; then
  echo "> AtomCMS: php artisan cache:clear"
  (cd {{AtomCmsDirectory}} && runuser -u www-data -- php artisan cache:clear --no-interaction >/dev/null 2>&1) || true
fi
if [ -n "$emulator" ]; then
  echo "> Starting the emulator (it updates the database to its version)"
  start_emulator
  wait_loaded
fi"#;

/// Replaces the hotel database with a .sql or .sql.gz dump after a safety backup. The database keeps its character
/// set, and names in CREATE DATABASE and USE lines become the hotel's. The emulator runs its migrations on restart.
pub async fn import_database(server: &dyn Server, remote_file: &str, log: Log<'_>) -> Result<()> {
    // The file name and the control functions go in last, so nothing in them can be taken for a placeholder.
    let body = fill(
        IMPORT_DATABASE,
        &[
            ("DatabaseName", &database_name()),
            ("BackupDirectory", &backup_directory()),
            ("Directory", &emulator::directory()),
            ("LogDirectory", &emulator::log_directory()),
            ("AtomCmsDirectory", &atom_cms::directory()),
            ("ControlFunctions", &emulator::control_functions()),
            ("RemoteFile", &shell_quote(remote_file)),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Importing the database failed", None).await
}

const UPDATE_SYSTEM: &str = r#"set -o pipefail
echo "> apt-get update"
apt_update 2>&1 | grep -E '^(Get|Err|E:|W:)' | grep -vE '^Get:[0-9]+ .*(Translation|Packages|Contents)' || true
for action in upgrade dist-upgrade; do
  echo "> apt-get $action"
  rc=$(mktemp)
  (apt-get {{Options}} $action 2>&1; echo $? > "$rc") | grep -E '^(Setting up|Removing|[0-9]+ upgraded|E:|W:)' | sed 's/^/  /' || true
  code=$(cat "$rc"); rm -f "$rc"
  [ "$code" = 0 ] || fail "apt-get $action stopped with exit code $code."
done
if [ -f /var/run/reboot-required ]; then
  echo "  A reboot is needed to finish the update ($(sort -u /var/run/reboot-required.pkgs 2>/dev/null | head -5 | tr '\n' ' '))"
  echo "REBOOT=yes"
fi"#;

/// apt-get update, upgrade and dist-upgrade, keeping changed configuration files. Returns whether a reboot is needed.
pub async fn update_system(server: &dyn Server, log: Log<'_>) -> Result<bool> {
    const OPTIONS: &str = "-y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";
    let body = fill(UPDATE_SYSTEM, &[("Options", OPTIONS)]);
    let result = script::run_capturing(server, &script::with_prelude(&body), log, "Updating the system failed", &["REBOOT"], None).await?;
    Ok(result.yes("REBOOT"))
}

const EMULATOR_VERSION: &str = r#"d={{Repository}}
echo "commit=$(git -c safe.directory="$d" -C "$d" rev-parse --short HEAD 2>/dev/null)"
jar=$(ls -t {{Directory}}/target/Polaris-*-jar-with-dependencies.jar {{Directory}}/target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)
echo "version=$(basename "$jar" -jar-with-dependencies.jar 2>/dev/null)"
echo "java=$({{FindJava}})""#;

/// The emulator's git commit and the version of its built JAR.
pub async fn emulator_version(server: &dyn Server) -> Result<(Option<String>, Option<String>)> {
    let command = fill(
        EMULATOR_VERSION,
        &[("Repository", &emulator::repository()), ("Directory", &emulator::directory()), ("FindJava", emulator::FIND_JAVA)],
    );
    let values = script::ScriptValues::parse(&server.run_as_root(&command).await?.output);
    Ok((values.string("commit"), values.string("version")))
}

pub async fn java_home(server: &dyn Server) -> Result<String> {
    let java = server.run_as_root(emulator::FIND_JAVA).await?.output.trim().to_string();
    if java.is_empty() {
        return Err(ServerError("JDK 25 is not installed. Run the Emulator step first.".into()).into());
    }
    Ok(java)
}

/// Gives the emulator account its repository back after a git pull as root.
pub async fn fix_emulator_owner(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = format!("chown -R {}:www-data {}\n", emulator::account(), emulator::repository());
    script::run(server, &script::with_prelude(&body), log, "Changing the owner of the emulator files failed", None).await
}

// The Debug page's live logs (DebugView.Sources in the C# app): the text lives in the view there, the commands are
// here so the GUI and the parity tests share them.

/// A log the Debug page follows: the button's name, what it shows, what it follows, and the command (it never ends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugLogSource {
    pub name: String,
    pub description: String,
    pub follows: String,
    pub command: String,
}

impl DebugLogSource {
    /// The script for [`Server::follow_as_root`]: the command in a process group that stops with the SSH channel.
    pub fn follow_script(&self) -> String {
        crate::server::follow_script(&self.command)
    }
}

/// The emulator, NGINX, Traefik, the firewall, Fail2Ban and CrowdSec, in the order of the Debug page's buttons.
pub fn debug_log_sources() -> Vec<DebugLogSource> {
    let logs = emulator::log_directory();
    let nginx = hotel_site::LOG_DIRECTORY;
    let ufw = UFW_LOG;
    let fail2ban = FAIL2BAN_LOG;
    let source = |name: &str, description: String, follows: String, command: String| DebugLogSource {
        name: name.into(),
        description,
        follows,
        command,
    };
    vec![
        source(
            "Emulator",
            format!("{logs}/emulator.log: what Polaris prints (startup, errors, wired, players)"),
            format!("{logs}/emulator.log"),
            format!("tail -n 200 -F {logs}/emulator.log"),
        ),
        source(
            "NGINX errors",
            format!("{nginx}/error.log and /var/log/nginx/error.log"),
            format!("{nginx}/error.log, /var/log/nginx/error.log"),
            format!("tail -n 100 -F {nginx}/error.log /var/log/nginx/error.log"),
        ),
        source(
            "NGINX access",
            format!("{nginx}/access.log: every request that reaches the hotel site"),
            format!("{nginx}/access.log"),
            format!("tail -n 100 -F {nginx}/access.log"),
        ),
        source(
            "Traefik",
            "docker logs of the traefik container: certificates, routing and connection errors".into(),
            "docker logs traefik".into(),
            "if command -v docker >/dev/null 2>&1 && docker inspect traefik >/dev/null 2>&1; then docker logs -f --tail 200 traefik; \
             else echo 'There is no traefik container on this server (Web access step).'; sleep infinity; fi"
                .into(),
        ),
        source(
            "Firewall",
            format!("{ufw}: connections ufw blocked (BLOCK) or let through and logged"),
            format!("{ufw} (or the journal without rsyslog)"),
            format!(
                "if [ -f {ufw} ]; then tail -n 200 -F {ufw}; \
                 else echo 'No ufw.log (no rsyslog): following the journal instead.'; journalctl -k -f -n 200 -o short-iso | grep --line-buffered UFW; fi"
            ),
        ),
        source(
            "Fail2Ban",
            format!("{fail2ban}: failed SSH logins it counted (Found), bans (Ban) and unbans (Unban)"),
            format!("{fail2ban} (or the journal)"),
            format!(
                "if ! command -v fail2ban-client >/dev/null 2>&1; then echo 'Fail2ban is not installed on this server (Addons -> Fail2Ban).'; sleep infinity; \
                 elif [ -f {fail2ban} ]; then tail -n 200 -F {fail2ban}; \
                 else echo 'No fail2ban.log: following the journal instead.'; journalctl -u fail2ban -f -n 200 -o short-iso; fi"
            ),
        ),
        source(
            "CrowdSec",
            "docker logs of the crowdsec container: bans it decides, the hub, the console and the bouncer's connections".into(),
            "docker logs crowdsec".into(),
            format!(
                "if docker inspect {c} >/dev/null 2>&1; then docker logs -f --tail 200 {c}; \
                 else echo 'CrowdSec is not installed on this server (Addons -> CrowdSec).'; sleep infinity; fi",
                c = crowdsec::CONTAINER
            ),
        ),
    ]
}
