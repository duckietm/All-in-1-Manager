//! JDK 25, the emulator account, build.sh, the database account, config.ini, the systemd service and the first
//! start of Polaris. (EmulatorInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use aio_core::{DatabaseCredentials, ServerLayout};

use crate::script::{self, fill, sql_literal, ScriptValues};
use crate::server::{Log, Result, Server};

pub fn repository() -> String {
    ServerLayout::current().emulator_repository.clone()
}

pub fn directory() -> String {
    format!("{}/Emulator", repository())
}

pub fn log_directory() -> String {
    ServerLayout::current().emulator_logs.clone()
}

pub fn account() -> String {
    ServerLayout::current().emulator_account.clone()
}

// The distribution's OpenJDK 25, or Temurin 25.
pub const FIND_JAVA: &str = "ls -d /usr/lib/jvm/java-25-openjdk-* /usr/lib/jvm/temurin-25-jdk-* 2>/dev/null | head -1";

const CONTROL_FUNCTIONS: &str = r#"emulator_running() { {{RunningCheck}}; }
stop_emulator() {
  # pkill fails when nothing runs, which would end the script under set -e.
  if [ -d /run/systemd/system ]; then systemctl stop emulator; else pkill -u {{Account}} -f 'jar-with-dependencies' || true; fi
  for i in $(seq 1 30); do emulator_running || return 0; sleep 1; done
  fail "The emulator did not stop within 30 seconds."
}
start_emulator() {
  # Start with an empty log so only this start counts.
  [ -f "$log" ] && mv "$log" "$log.$(date +%H%M%S_%d-%m-%Y)"
  if [ -d /run/systemd/system ]; then
    systemctl restart emulator
  else
    stop_emulator
    setsid runuser -u {{Account}} -- "$d/emulator" </dev/null >/dev/null 2>&1 &
  fi
}
wait_loaded() {
  for i in $(seq 1 300); do
    sleep 2
    if grep -q 'successfully loaded' "$log" 2>/dev/null; then echo "  loaded after $((i * 2)) seconds"; return 0; fi
    if grep -q 'startup was aborted' "$log" 2>/dev/null || ! emulator_running; then
      [ $i -gt 3 ] || continue
      echo "  --- last lines of $log ---"; tail -25 "$log" 2>/dev/null | sed 's/^/  /'
      fail "The emulator stopped during startup. See $log."
    fi
  done
  fail "The emulator did not finish loading within 10 minutes. See $log."
}"#;

/// Shell helpers that control the emulator (`emulator_running`, `stop_emulator`, `start_emulator` ...). The calling
/// script must set $d and $log. (ControlFunctions)
pub fn control_functions() -> String {
    fill(CONTROL_FUNCTIONS, &[("RunningCheck", &running_check()), ("Account", &account())])
}

const SET_SETTINGS: &str = r#"d={{Directory}}
log={{LogDirectory}}/emulator.log
db=$(grep -sE '^db.database=' "$d/config.ini" | cut -d= -f2)
if [ -z "$db" ] || ! mariadb -N "$db" -e "SELECT 1 FROM emulator_settings LIMIT 1" >/dev/null 2>&1; then
  echo "  The emulator is not set up yet (no emulator_settings): run the Emulator step, then this step again."
  echo "SETTINGS=no"
  exit 0
fi
{{setup}}
current=$(mariadb -N "$db" -e "SELECT GROUP_CONCAT(\`key\`, '=', \`value\` ORDER BY \`key\` SEPARATOR ',') FROM emulator_settings WHERE \`key\` IN ({{keys}})")
if [ "$current" = '{{wanted}}' ]; then
  echo "  {{what}} settings are already set"
  echo "SETTINGS=yes"
  exit 0
fi

{{ControlFunctions}}
was_running=
if emulator_running; then
  was_running=yes
  echo "> Stopping the emulator"
  stop_emulator
fi

echo "> emulator_settings: {{names}}"
mariadb "$db" <<'SQL'
{{sql}}
SQL
mariadb -N "$db" -e "SELECT \`key\`, \`value\` FROM emulator_settings WHERE \`key\` IN ({{keys}}) ORDER BY \`key\`" | sed 's/^/    /; s/\t/ = /'

if [ -n "$was_running" ]; then
  echo "> Starting the emulator"
  start_emulator
  wait_loaded
else
  echo "  The emulator was not running: the settings apply when it starts."
fi
echo "SETTINGS=yes""#;

/// Writes `emulator_settings` rows (stopping the emulator while they are written when it runs). Returns true when
/// something changed. (SetSettingsAsync)
///
/// Stops a running emulator while writing so it cannot write its own values back. Does nothing when the values are
/// already set, and returns false when the emulator is not set up yet.
pub async fn set_settings(server: &dyn Server, settings: &[(String, String)], what: &str, log: Log<'_>, setup: &str) -> Result<bool> {
    let keys = settings.iter().map(|(k, _)| sql_literal(k)).collect::<Vec<_>>().join(", ");
    let sql: String = settings
        .iter()
        .map(|(k, v)| {
            format!(
                "DELETE FROM emulator_settings WHERE `key` = {};\nINSERT INTO emulator_settings (`key`, `value`) VALUES ({}, {});\n",
                sql_literal(k),
                sql_literal(k),
                sql_literal(v)
            )
        })
        .collect();
    let wanted = settings_line(settings);
    let names = settings.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ");
    let body = fill(
        SET_SETTINGS,
        &[
            ("Directory", &directory()),
            ("LogDirectory", &log_directory()),
            ("ControlFunctions", &control_functions()),
            ("names", &names),
            ("keys", &keys),
            ("wanted", &wanted),
            ("what", what),
            ("sql", &sql),
            ("setup", setup),
        ],
    );
    let result = script::run_capturing(
        server,
        &script::with_prelude(&body),
        log,
        &format!("Setting the {what} in emulator_settings failed"),
        &["SETTINGS"],
        None,
    )
    .await?;
    Ok(result.yes("SETTINGS"))
}

const READ_SETTING: &str = r#"db=$(grep -sE '^db.database=' {{Directory}}/config.ini | cut -d= -f2)
[ -n "$db" ] && mariadb -N "$db" -e "SELECT \`value\` FROM emulator_settings WHERE \`key\` = {{key}}" 2>/dev/null"#;

/// One `emulator_settings` value, or None when the key is unset or the emulator is not set up. (ReadSettingAsync)
pub async fn read_setting(server: &dyn Server, key: &str) -> Result<Option<String>> {
    let command = fill(READ_SETTING, &[("Directory", &directory()), ("key", &sql_literal(key))]);
    let value = server.run_as_root(&command).await?.output.trim().to_string();
    Ok(if value.is_empty() { None } else { Some(value) })
}

/// "key=value,..." sorted by key, as the server's GROUP_CONCAT of the imager keys prints it. (ImagerSettingsLine)
pub fn imager_settings_line(site_url: &str) -> String {
    settings_line(&imager_settings(site_url))
}

/// The imager keys as SQL literals, comma separated. (ImagerKeysSql)
pub fn imager_keys_sql() -> String {
    imager_settings("").iter().map(|(k, _)| sql_literal(k)).collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------------------------------------------------------

/// The home folder a new account gets; an existing account (root, a person's account) keeps its own.
pub fn account_home() -> String {
    ServerLayout::current().emulator_home()
}

/// Adoptium's signing key, used for Temurin where the distribution has no OpenJDK 25.
pub const ADOPTIUM_KEY_FINGERPRINT: &str = "3B04D753C9050D9A5D343F39843C48A565F8F04B";

// Without systemd, look for the process instead.
fn running_check() -> String {
    format!(
        "if [ -d /run/systemd/system ]; then systemctl is-active --quiet emulator; else pgrep -u {} -f 'jar-with-dependencies' >/dev/null; fi",
        account()
    )
}

/// "key=value,..." sorted by key (ordinal).
fn settings_line(settings: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = settings.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
}

/// Emulator state on the server before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorState {
    pub repository_present: bool,
    pub java_home: Option<String>,
    pub account_ready: bool,
    pub jar: Option<String>,
    pub config_present: bool,
    pub running: bool,
    pub service_installed: bool,
    pub configured_timezone: Option<String>,
    pub configured_whitelist: Option<String>,
    pub configured_memory_mb: Option<i32>,
    pub ram_mb: i32,
    pub timezones: Vec<String>,
}

impl EmulatorState {
    pub fn is_complete(&self) -> bool {
        self.repository_present && self.java_home.is_some() && self.account_ready && self.jar.is_some() && self.config_present && self.running
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorStartResult {
    pub jar: String,
    pub systemd: bool,
    pub errors: i64,
}

const INSPECT: &str = r#"d={{Directory}}
echo "repo=$([ -f "$d/pom.xml" ] && echo yes)"
echo "java=$({{FindJava}})"
echo "account=$(id -nG {{Account}} 2>/dev/null | grep -qw www-data && echo yes)"
echo "jar=$(ls -t "$d"/target/Polaris-*-jar-with-dependencies.jar "$d"/target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)"
echo "config=$([ -f "$d/config.ini" ] && echo yes)"
echo "running=$({{RunningCheck}} && echo yes)"
echo "service=$([ -f /etc/systemd/system/emulator.service ] && echo yes)"
echo "configtz=$(grep -oE 'serverTimezone=[^&[:space:]]+' "$d/config.ini" 2>/dev/null | head -1 | cut -d= -f2)"
echo "memory=$(grep -oE '^MEMORY_MB=[0-9]+|Xmx[0-9]+m' "$d/emulator" 2>/dev/null | head -1 | tr -dc 0-9)"
echo "whitelist=$(mariadb -N -e "SELECT value FROM habbo.emulator_settings WHERE \`key\` = 'ws.whitelist'" 2>/dev/null)"
echo "ram=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)"
echo "zones=$( (timedatectl list-timezones 2>/dev/null || (cd /usr/share/zoneinfo && find [A-Z]* -type f | grep -vE '^(posix|right|Etc/)' | sort)) | tr '\n' ' ')""#;

pub async fn inspect(server: &dyn Server) -> Result<EmulatorState> {
    let command = fill(
        INSPECT,
        &[("Directory", &directory()), ("FindJava", FIND_JAVA), ("Account", &account()), ("RunningCheck", &running_check())],
    );
    let result = server.run_as_root(&command).await?;
    Ok(parse_state(&result.output))
}

fn parse_state(output: &str) -> EmulatorState {
    let values = ScriptValues::parse(output);
    let zones: Vec<String> = values.or_empty("zones").split(' ').filter(|z| !z.is_empty()).map(str::to_string).collect();
    EmulatorState {
        repository_present: values.yes("repo"),
        java_home: values.string("java"),
        account_ready: values.yes("account"),
        jar: values.string("jar"),
        config_present: values.yes("config"),
        running: values.yes("running"),
        service_installed: values.yes("service"),
        configured_timezone: values.string("configtz"),
        configured_whitelist: values.string("whitelist"),
        configured_memory_mb: values.get("memory").and_then(|m| m.parse().ok()),
        ram_mb: values.get("ram").and_then(|m| m.parse().ok()).unwrap_or(0),
        timezones: if zones.is_empty() { vec!["Etc/UTC".to_string()] } else { zones },
    }
}

const INSTALL_JAVA: &str = r#"echo "> apt-get update"
apt_update
if apt-cache show openjdk-25-jdk-headless >/dev/null 2>&1; then
  echo "> apt-get install openjdk-25-jdk-headless"
  apt-get install -y openjdk-25-jdk-headless curl unzip ca-certificates
else
  echo "> $PRETTY_NAME has no OpenJDK 25: using Eclipse Temurin 25 from Adoptium"
  apt-get install -y --no-install-recommends curl gnupg ca-certificates unzip
  tmp=$(mktemp)
  trap 'rm -f "$tmp"' EXIT
  curl -fsSL https://packages.adoptium.net/artifactory/api/gpg/key/public -o "$tmp" || fail "Could not download the Adoptium signing key."
  fprs=$(gpg --show-keys --with-colons "$tmp" 2>/dev/null | awk -F: '/^pub/{p=1; next} /^fpr/ && p {print $10; p=0}')
  [ "$fprs" = "{{AdoptiumKeyFingerprint}}" ] || fail "The downloaded key is not the Adoptium key {{AdoptiumKeyFingerprint}} (found: $fprs)."
  echo "  key {{AdoptiumKeyFingerprint}} ok"
  install -d -m 0755 /etc/apt/keyrings
  gpg --dearmor --yes -o /etc/apt/keyrings/adoptium.gpg "$tmp"
  echo "deb [signed-by=/etc/apt/keyrings/adoptium.gpg] https://packages.adoptium.net/artifactory/deb $codename main" > /etc/apt/sources.list.d/adoptium.list
  apt_update
  apt-get install -y temurin-25-jdk
fi
java_home=$({{FindJava}})
[ -n "$java_home" ] || fail "JDK 25 is not in /usr/lib/jvm after installing."
"$java_home/bin/java" -version 2>&1 | head -1
echo "JAVA_HOME=$java_home""#;

/// Installs JDK 25, since the build needs 25 or 26. Uses Adoptium's Temurin where the distribution has none, as on
/// Debian 11 and 12. Returns JAVA_HOME.
pub async fn install_java(server: &dyn Server, log: Log<'_>) -> Result<String> {
    let body = fill(INSTALL_JAVA, &[("AdoptiumKeyFingerprint", ADOPTIUM_KEY_FINGERPRINT), ("FindJava", FIND_JAVA)]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Installing Java failed", &["JAVA_HOME"], None).await?;
    Ok(values.or_empty("JAVA_HOME").to_string())
}

const CREATE_ACCOUNT: &str = r#"getent group www-data >/dev/null || groupadd --system www-data
if ! id {{Account}} >/dev/null 2>&1; then
  useradd --system --user-group --home-dir {{AccountHome}} --create-home --shell /usr/sbin/nologin {{Account}}
  echo "  account {{Account}} created"
fi
usermod -aG www-data {{Account}}
echo "  $(id {{Account}})"
chown -R {{Account}}:www-data {{Repository}}
echo "  {{Repository}} belongs to {{Account}}:www-data"
# setgid so log files created here belong to group www-data and the website can read them.
install -d -m 2750 -o {{Account}} -g www-data {{LogDirectory}}
chmod 2750 {{LogDirectory}}
chown {{Account}}:www-data {{LogDirectory}}/emulator.log* 2>/dev/null || true
echo "  {{LogDirectory}}: $(stat -c '%A %U:%G' {{LogDirectory}})""#;

/// Creates a system account without a login shell. Its home holds Maven's cache.
pub async fn create_account(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(
        CREATE_ACCOUNT,
        &[("Account", &account()), ("AccountHome", &account_home()), ("Repository", &repository()), ("LogDirectory", &log_directory())],
    );
    script::run(server, &script::with_prelude(&body), log, "Creating the emulator account failed", None).await
}

const BUILD: &str = r#"d={{Directory}}
# mvnw downloads Maven silently and without a timeout, which can hang forever on a server with broken IPv6.
# Download the version it asks for here instead, with visible output and timeouts.
props="$d/.mvn/wrapper/maven-wrapper.properties"
url=$(sed -n 's/^distributionUrl=//p' "$props" 2>/dev/null | tr -d '\r')
sum=$(sed -n 's/^distributionSha256Sum=//p' "$props" 2>/dev/null | tr -d '\r')
maven=
if [ -n "$url" ]; then
  name=$(basename "$url" -bin.zip)
  maven=/opt/$name/bin/mvn
  if [ ! -x "$maven" ]; then
    echo "> Downloading $name (only the first build)"
    echo "  $url"
    tmp=$(mktemp -d)
    # New Maven releases can reach the Apache download servers before Maven Central, so try those too.
    version=${name#apache-maven-}
    apache="maven/maven-${version%%.*}/$version/binaries/$name-bin.zip"
    got=
    for from in "$url" "https://dlcdn.apache.org/$apache" "https://archive.apache.org/dist/$apache"; do
      if download "$from" "$tmp/maven.zip"; then got=$from; break; fi
      echo "  Not available from $(echo "$from" | cut -d/ -f3), trying the next server ..."
    done
    [ -n "$got" ] || { rm -rf "$tmp"; fail "Could not download $name. Check that the server can reach repo.maven.apache.org and dlcdn.apache.org (DNS, firewall, proxy)."; }
    [ "$got" = "$url" ] || echo "  from $got"
    echo "  $(du -h "$tmp/maven.zip" | cut -f1) downloaded"
    if [ -n "$sum" ] && ! echo "$sum  $tmp/maven.zip" | sha256sum -c --status; then
      rm -rf "$tmp"; fail "The downloaded Maven does not match the sha256 in maven-wrapper.properties."
    fi
    rm -rf "/opt/$name"
    unzip -q "$tmp/maven.zip" -d /opt
    rm -rf "$tmp"
    [ -x "$maven" ] || fail "$maven is missing after unpacking."
  fi
  echo "  $("$maven" -v 2>/dev/null | head -1 | sed 's/\x1b\[[0-9;]*m//g')"
fi
cat > "$d/build.sh" <<'BUILD'
#!/bin/sh
# Builds the Polaris emulator into target/ (written by All-in-1 Installer).
#   ./build.sh               build without the unit tests (about 1 minute)
#   ./build.sh --with-tests  also run the 4000+ unit tests (about 4 minutes)
# pom.xml needs Maven 3.9.11+ and JDK 25, newer than most distributions ship: the installer puts the
# Maven version of the Maven Wrapper in /opt; without it, mvnw downloads it.
set -e
cd "$(dirname "$0")"
export JAVA_HOME=JAVA_HOME_HERE
mvn=MAVEN_HERE
[ -x "$mvn" ] || mvn=./mvnw
tests=-DskipTests
[ "$1" = "--with-tests" ] && tests=
exec "$mvn" -B -ntp clean package $tests -f pom.xml
BUILD
sed -i -e "s#JAVA_HOME_HERE#{{javaHome}}#" -e "s#MAVEN_HERE#${maven:-./mvnw}#" "$d/build.sh"
chown {{Account}}:www-data "$d/build.sh"
chmod 755 "$d/build.sh" "$d/mvnw"
echo "> $d/build.sh (as {{Account}})"
# The account's real home (an existing account keeps its own), for Maven's ~/.m2.
home=$(getent passwd {{Account}} | cut -d: -f6)
home=${home:-{{AccountHome}}}
[ -d "$home/.m2/repository" ] || echo "  The first build downloads the libraries (about 150 MB): this takes a few minutes."
cd "$d"
# Maven prints little while it downloads, so report progress every 30 s and give up after 30 minutes.
start=$(date +%s)
timeout 1800 runuser -u {{Account}} -- env HOME="$home" "$d/build.sh" &
build=$!
(
  while sleep 30 </dev/null >/dev/null 2>&1; do
    kill -0 $build 2>/dev/null || exit 0
    echo "  ... still building: $(( ($(date +%s) - start) / 60 )) min, $(du -sm "$home/.m2" 2>/dev/null | cut -f1) MB in ~/.m2"
  done
) &
beat=$!
if wait $build; then rc=0; else rc=$?; fi
kill $beat 2>/dev/null || true
wait $beat 2>/dev/null || true
[ $rc -ne 124 ] || fail "The build did not finish within 30 minutes. Run $d/build.sh on the server to see where it stops."
[ $rc -eq 0 ] || fail "build.sh stopped with exit code $rc (see the Maven output above)."
jar=$(ls -t target/Polaris-*-jar-with-dependencies.jar target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)
[ -n "$jar" ] || fail "The build finished, but there is no *-jar-with-dependencies.jar in $d/target."
echo "JAR=$d/$jar""#;

/// Runs Emulator/build.sh as the emulator account. Returns the built JAR.
pub async fn build(server: &dyn Server, java_home: &str, log: Log<'_>) -> Result<String> {
    let body = fill(
        BUILD,
        &[("Directory", &directory()), ("Account", &account()), ("AccountHome", &account_home()), ("javaHome", java_home)],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Building the emulator failed", &["JAR"], None).await?;
    Ok(values.or_empty("JAR").to_string())
}

const CONFIGURE: &str = r#"umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cat > "$work/input"
awk -v dir="$work" '/^-----OVERRIDES-----$/ {part=1; next} { print > (dir "/" (part ? "overrides" : "sql")) }' "$work/input"

echo "> MariaDB account {{EmulatorUser}}@localhost (all privileges on {{Database}})"
mariadb < "$work/sql"

d={{Directory}}
base="$d/config.ini"
if [ -f "$base" ]; then
  cp -p "$base" "$base.bak-$(date +%Y%m%d-%H%M%S)"
  echo "> Updating the database settings in $base (a backup was made)"
else
  base="{{Repository}}/config example/config.ini.example"
  [ -f "$base" ] || fail "There is no config.ini.example in {{Repository}}/config example."
  echo "> Writing $d/config.ini from config.ini.example"
fi
awk -F'=' '
  FNR == NR { soft = ($0 ~ /^\?/); line = soft ? substr($0, 2) : $0; k = substr(line, 1, index(line, "=") - 1)
              ov[k] = line; isSoft[k] = soft; next }
  /^[ \t]*#/ || index($0, "=") == 0 { print; next }
  { k = $1; gsub(/[ \t]+$/, "", k); v = substr($0, index($0, "=") + 1); gsub(/^[ \t]+|[ \t]+$/, "", v)
    if ((k in ov) && (!isSoft[k] || v == "" || v ~ /change-me/)) { print ov[k]; done[k] = 1 }
    else { print; if (k in ov) done[k] = 1 } }
  END { for (k in ov) if (!(k in done)) print ov[k] }
' "$work/overrides" "$base" > "$work/config.ini"
install -m 0640 -o {{Account}} -g www-data "$work/config.ini" "$d/config.ini"
grep -E '^db\.(hostname|port|database|username|params)=' "$d/config.ini" | sed 's/^/  /'

echo "> Writing the start script $d/emulator"
cat > "$d/emulator" <<'START'
#!/bin/sh
# Starts the Polaris emulator; used by emulator.service (written by All-in-1 Installer).
# The previous log is kept as emulator.log.<time>; kept logs older than 30 days are removed.
JAVA_HOME=JAVA_HOME_HERE
MEMORY_MB=MEMORY_HERE
LOG_DIR={{LogDirectory}}
# Logs and backups: writable by the emulator's account, readable by group www-data, not by others.
umask 027
cd "$(dirname "$0")"
jar=$(ls -t target/Polaris-*-jar-with-dependencies.jar target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)
[ -n "$jar" ] || { echo "No emulator jar in $(pwd)/target: run ./build.sh first." >&2; exit 1; }
if [ -f "$LOG_DIR/emulator.log" ]; then
  mv "$LOG_DIR/emulator.log" "$LOG_DIR/emulator.log.$(date +%H%M_%d-%m-%Y)"
fi
find "$LOG_DIR" -name 'emulator.log.*' -mtime +30 -delete 2>/dev/null
exec "$JAVA_HOME/bin/java" -Dfile.encoding=UTF8 -Xmx${MEMORY_MB}m -jar "$jar" >> "$LOG_DIR/emulator.log" 2>&1
START
sed -i -e "s#JAVA_HOME_HERE#{{javaHome}}#" -e "s#MEMORY_HERE#{{memoryMb}}#" "$d/emulator"
chown {{Account}}:www-data "$d/emulator"
chmod 755 "$d/emulator"

echo "> Writing /etc/systemd/system/emulator.service"
cat > /etc/systemd/system/emulator.service <<'UNIT'
[Unit]
Description=Polaris emulator
# The database must be running before the emulator starts.
After=network-online.target mariadb.service
Wants=network-online.target
Requires=mariadb.service

[Service]
User={{Account}}
SupplementaryGroups=www-data
# config.ini, build.sh and the start script live here; target/ only holds the build output.
WorkingDirectory={{Directory}}
ExecStart={{Directory}}/emulator
SuccessExitStatus=143
TimeoutStopSec=30
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
chmod 644 /etc/systemd/system/emulator.service
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload
  systemctl enable emulator
else
  echo "  (no systemd on this machine: the emulator is started directly, not at boot)"
fi"#;

/// What [`configure`] sends on stdin: the SQL for the database account, then the config.ini overrides.
/// "key=value" replaces the line in config.ini; "?key=value" only replaces an empty or change-me value.
fn configure_input(credentials: &DatabaseCredentials, timezone: &str) -> String {
    let user = sql_literal(&credentials.emulator_user);
    let password = sql_literal(&credentials.emulator_password);
    let database = credentials.database.replace('`', "``");
    let sql = format!(
        "CREATE USER IF NOT EXISTS {user}@'localhost' IDENTIFIED BY {password};\n\
         ALTER USER {user}@'localhost' IDENTIFIED BY {password};\n\
         CREATE SCHEMA IF NOT EXISTS `{database}`;\n\
         GRANT ALL PRIVILEGES ON `{database}`.* TO {user}@'localhost';\n\
         FLUSH PRIVILEGES;"
    );
    let new_password = aio_core::credentials::new_password;
    let overrides = [
        "db.hostname=127.0.0.1".to_string(),
        format!("db.port={}", credentials.port),
        format!("db.database={}", credentials.database),
        format!("db.username={}", credentials.emulator_user),
        format!("db.password={}", credentials.emulator_password),
        format!("db.params=?characterEncoding=utf8&useSSL=false&serverTimezone={timezone}"),
        format!("?nitro.secure.master_key={}{}", new_password(), new_password()),
        format!("?login.remember.jwt.secret={}{}", new_password(), new_password()),
    ]
    .join("\n");
    format!("{sql}\n-----OVERRIDES-----\n{overrides}\n")
}

/// Secrets go over stdin. An existing config.ini keeps everything except the database settings and placeholder
/// secrets.
pub async fn configure(
    server: &dyn Server,
    credentials: &DatabaseCredentials,
    timezone: &str,
    memory_mb: i32,
    java_home: &str,
    log: Log<'_>,
) -> Result<()> {
    let stdin = configure_input(credentials, timezone);
    let body = fill(
        CONFIGURE,
        &[
            ("Directory", &directory()),
            ("Repository", &repository()),
            ("Account", &account()),
            ("LogDirectory", &log_directory()),
            ("memoryMb", &memory_mb.to_string()),
            ("javaHome", java_home),
            ("EmulatorUser", &credentials.emulator_user),
            ("Database", &credentials.database),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Configuring the emulator failed", Some(&stdin)).await
}

/// Camera and badge imager settings for emulator_settings.
pub fn imager_settings(site_url: &str) -> Vec<(String, String)> {
    let camera = crate::hotel_site::camera();
    let gamedata = crate::octane::gamedata();
    vec![
        ("camera.url".into(), format!("{site_url}/gamedata/camera/")),
        ("imager.location.output.camera".into(), camera.clone()),
        ("imager.location.output.thumbnail".into(), format!("{camera}/thumb/")),
        ("camera.render.mode".into(), "auto".into()),
        ("imager.location.output.badges".into(), format!("{gamedata}/c_images/album1584/")),
        ("imager.location.badgeparts".into(), format!("{gamedata}/c_images/groupbadge/badgeparts/")),
    ]
}

const IMAGER_FOLDERS: &str = r#"install -d -m 2775 -o www-data -g www-data {{Camera}} {{Camera}}/thumb {{g}}/c_images {{g}}/c_images/album1584 \
  {{g}}/c_images/groupbadge {{g}}/c_images/groupbadge/badge {{g}}/c_images/groupbadge/badgeparts"#;

/// Writes [`imager_settings`] via [`set_settings`] and creates their folders.
pub async fn configure_imager(server: &dyn Server, site_url: &str, log: Log<'_>) -> Result<bool> {
    let folders = fill(IMAGER_FOLDERS, &[("Camera", &crate::hotel_site::camera()), ("g", &crate::octane::gamedata())]);
    set_settings(server, &imager_settings(site_url), "camera and badge imager", log, &folders).await
}

const START: &str = r#"umask 077
sql=$(mktemp)
trap 'rm -f "$sql"' EXIT
cat > "$sql"
d={{Directory}}
log={{LogDirectory}}/emulator.log
db=$(grep -E '^db.database=' "$d/config.ini" | cut -d= -f2)

{{ControlFunctions}}

echo "> Starting the emulator (the first start creates the database tables)"
start_emulator
wait_loaded

echo "> emulator_settings: console.mode = 0, ws.whitelist"
before=$(mariadb -N "$db" -e "SELECT GROUP_CONCAT(\`key\`, '=', \`value\` ORDER BY \`key\`) FROM emulator_settings WHERE \`key\` IN ('console.mode', 'ws.whitelist', 'websockets.whitelist')")
mariadb "$db" < "$sql"
after=$(mariadb -N "$db" -e "SELECT GROUP_CONCAT(\`key\`, '=', \`value\` ORDER BY \`key\`) FROM emulator_settings WHERE \`key\` IN ('console.mode', 'ws.whitelist', 'websockets.whitelist')")
echo "  $after"
if [ "$before" != "$after" ]; then
  echo "> Restarting the emulator to use the new settings"
  start_emulator
  wait_loaded
fi

grep -E 'Started (Game|RCON) Server' "$log" | sed 's/^.*| /  /'
ss -ltn 2>/dev/null | grep -q ':3000 ' || fail "The emulator is running, but nothing listens on port 3000."
ufw_allow 3000/tcp
echo "ERRORS=$(grep -c ' ERROR ' "$log")"
echo "SYSTEMD=$([ -d /run/systemd/system ] && echo yes)"
echo "JAR=$d/$(cd "$d" && ls -t target/Polaris-*-jar-with-dependencies.jar target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)""#;

/// Restarts the emulator and waits until it has loaded. The first start creates the database. Then sets
/// console.mode and the websocket whitelist, restarting again if they changed.
pub async fn start(server: &dyn Server, whitelist: &str, log: Log<'_>) -> Result<EmulatorStartResult> {
    let w = sql_literal(whitelist);
    let settings = format!(
        "UPDATE emulator_settings SET `value` = '0' WHERE `key` = 'console.mode';\n\
         UPDATE emulator_settings SET `value` = {w} WHERE `key` IN ('ws.whitelist', 'websockets.whitelist');\n\
         INSERT INTO emulator_settings (`key`, `value`) SELECT 'ws.whitelist', {w} FROM DUAL\n  \
         WHERE NOT EXISTS (SELECT 1 FROM emulator_settings WHERE `key` = 'ws.whitelist');\n"
    );
    let body = fill(START, &[("Directory", &directory()), ("LogDirectory", &log_directory()), ("ControlFunctions", &control_functions())]);
    let values = script::run_capturing(
        server,
        &script::with_prelude(&body),
        log,
        "Starting the emulator failed",
        &["ERRORS", "SYSTEMD", "JAR"],
        Some(&settings),
    )
    .await?;
    Ok(EmulatorStartResult { jar: values.or_empty("JAR").to_string(), systemd: values.yes("SYSTEMD"), errors: values.int("ERRORS") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_parsing() {
        let state = parse_state(
            "repo=yes\njava=/usr/lib/jvm/java-25-openjdk-amd64\naccount=yes\njar=/x.jar\nconfig=yes\nrunning=yes\nmemory=2048\nram=7900\nzones=Europe/Rome Etc/UTC \n",
        );
        assert!(state.is_complete());
        assert_eq!(state.configured_memory_mb, Some(2048));
        assert_eq!(state.ram_mb, 7900);
        assert_eq!(state.timezones, vec!["Europe/Rome", "Etc/UTC"]);
        let empty = parse_state("memory=\nram=x");
        assert!(!empty.is_complete());
        assert_eq!(empty.configured_memory_mb, None);
        assert_eq!(empty.ram_mb, 0);
        assert_eq!(empty.timezones, vec!["Etc/UTC"]);
    }

    #[test]
    fn settings_lines() {
        let line = settings_line(&[("b".into(), "2".into()), ("a.b".into(), "1".into()), ("a".into(), "0".into())]);
        assert_eq!(line, "a=0,a.b=1,b=2");
        assert!(imager_keys_sql().starts_with("'camera.url', 'imager.location.output.camera'"));
    }
}
