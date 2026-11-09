//! MariaDB 12.3: repository, secure installation, bind-address, users and login tests. (MariaDbInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub const CMS_PRIVILEGES: &str = "EXECUTE, SELECT, SHOW VIEW, ALTER, ALTER ROUTINE, CREATE, CREATE ROUTINE, CREATE TEMPORARY TABLES, CREATE VIEW, DELETE, DROP, EVENT, INDEX, INSERT, REFERENCES, TRIGGER, UPDATE, LOCK TABLES";

// ---------------------------------------------------------------------------------------------------------------

use aio_core::{DatabaseCredentials, ServerLayout};

use crate::script::{self, fill, sql_literal, ScriptValues};
use crate::server::{Log, Result, Server, ServerError};

/// MariaDB as found on the server before the MariaDB step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MariaDbState {
    pub codename: String,
    pub installed_version: Option<String>,
    pub mysql_installed: bool,
    pub running: bool,
    pub bind_address: Option<String>,
    pub database_exists: bool,
    pub root_remote_exists: bool,
    pub cms_exists: bool,
}

impl MariaDbState {
    /// Without the "1:" epoch.
    pub fn display_version(&self) -> Option<String> {
        self.installed_version.as_deref().map(|v| v.split(':').next_back().unwrap_or("").to_string())
    }

    /// mariadb.org packages carry "+maria~" in their version, distribution packages do not.
    pub fn from_mariadb_org(&self) -> bool {
        self.installed_version.as_deref().is_some_and(|v| v.contains("+maria"))
    }

    pub fn is_target_branch(&self) -> bool {
        self.from_mariadb_org() && self.display_version().is_some_and(|v| v.starts_with(&format!("{BRANCH}.")))
    }

    /// A distribution or other-branch MariaDB whose data would be upgraded.
    pub fn needs_upgrade(&self) -> bool {
        self.installed_version.is_some() && !self.is_target_branch()
    }

    pub fn users_exist(&self) -> bool {
        self.root_remote_exists && self.cms_exists
    }

    /// Saved passwords are checked separately.
    pub fn is_complete(&self) -> bool {
        self.is_target_branch() && self.running && self.bind_address.as_deref() == Some("0.0.0.0") && self.database_exists && self.users_exist()
    }
}

/// The CMS and emulator accounts as AtomCMS' .env and the emulator's config.ini use them. Values that are not found
/// are None.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseAccounts {
    pub database: Option<String>,
    pub cms_user: Option<String>,
    pub cms_password: Option<String>,
    pub emulator_user: Option<String>,
    pub emulator_password: Option<String>,
}

pub const BRANCH: &str = "12.3";

// "MariaDB Signing Key <signing-key@mariadb.org>", which signs the mariadb.org repositories.
pub const KEY_FINGERPRINT: &str = "177F4010FE56CA3336300305F1656F24C74CD1D8";

pub const SERVER_CONFIG: &str = "/etc/mysql/mariadb.conf.d/50-server.cnf";

// Copies stdin into a root-only temp file "$secret" so secrets never appear on a command line.
const READ_SECRET: &str = r#"umask 077
secret=$(mktemp)
trap 'rm -f "$secret"' EXIT
cat > "$secret"
"#;

const INSPECT: &str = r#". /etc/os-release
echo "codename=${VERSION_CODENAME:-$(lsb_release -cs 2>/dev/null)}"
echo "version=$(dpkg-query -W -f='${db:Status-Abbrev}${Version}' mariadb-server 2>/dev/null | awk '/^ii/{print substr($0,4)}')"
echo "mysql=$(dpkg-query -W -f='${db:Status-Abbrev}${Package}\n' mysql-server 'mysql-server-8*' 'mysql-community-server' 2>/dev/null | grep -q '^ii' && echo yes)"
if pgrep -x mariadbd >/dev/null || pgrep -x mysqld >/dev/null; then
  echo "running=yes"
  echo "bind=$(mariadb -N -e 'SELECT @@bind_address' 2>/dev/null)"
  echo "database=$(mariadb -N -e "SELECT 1 FROM information_schema.schemata WHERE schema_name='{{Database}}'" 2>/dev/null)"
  echo "rootremote=$(mariadb -N -e "SELECT 1 FROM mysql.user WHERE user='root' AND host='%'" 2>/dev/null)"
  echo "cms=$(mariadb -N -e "SELECT 1 FROM mysql.user WHERE user='cms' AND host='%'" 2>/dev/null)"
fi"#;

pub async fn inspect(server: &dyn Server) -> Result<MariaDbState> {
    let script = fill(INSPECT, &[("Database", &ServerLayout::current().database)]);
    let result = server.run_as_root(&script).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("Could not inspect MariaDB on the server: {}", result.error.trim())).into());
    }
    let values = ScriptValues::parse(&result.output);
    Ok(MariaDbState {
        codename: values.or_empty("codename").to_string(),
        installed_version: values.string("version"),
        mysql_installed: values.yes("mysql"),
        running: values.yes("running"),
        bind_address: values.string("bind"),
        database_exists: values.get("database") == Some("1"),
        root_remote_exists: values.get("rootremote") == Some("1"),
        cms_exists: values.get("cms") == Some("1"),
    })
}

const ADD_REPOSITORY: &str = r#"
echo "> apt-get update"
apt_update
echo "> Installing prerequisites"
apt-get install -y --no-install-recommends curl gnupg ca-certificates lsb-release
url="https://dlm.mariadb.com/repo/mariadb-server/{{Branch}}/repo/$ID"
curl -fsSIL "$url/dists/$codename/Release" >/dev/null || fail "mariadb.org has no MariaDB {{Branch}} packages for $PRETTY_NAME ($codename)."

echo "> Downloading and checking the MariaDB signing key"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL https://mariadb.org/mariadb_release_signing_key.pgp -o "$tmp" || fail "Could not download the MariaDB signing key."
fprs=$(gpg --show-keys --with-colons "$tmp" 2>/dev/null | awk -F: '/^pub/{p=1; next} /^fpr/ && p {print $10; p=0}')
[ "$fprs" = "{{KeyFingerprint}}" ] || fail "The downloaded key is not the MariaDB signing key {{KeyFingerprint}} (found: $fprs)."
echo "  key {{KeyFingerprint}} ok"
gpg --dearmor --yes -o /usr/share/keyrings/mariadb-keyring.gpg "$tmp"

echo "> Adding $url ($codename)"
echo "deb [signed-by=/usr/share/keyrings/mariadb-keyring.gpg] $url $codename main" > /etc/apt/sources.list.d/mariadb.list
printf 'Package: *\nPin: origin dlm.mariadb.com\nPin-Priority: 900\n' > /etc/apt/preferences.d/99mariadb
apt_update"#;

// Checks the signing key against KEY_FINGERPRINT and pins the repository above the distribution's packages.
pub async fn add_repository(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(ADD_REPOSITORY, &[("Branch", BRANCH), ("KeyFingerprint", KEY_FINGERPRINT)]);
    script::run(server, &script::with_prelude(&body), log, "Adding the MariaDB repository failed", None).await
}

const BACKUP: &str = r#"
backup="/root/mariadb-backup-$(date +%Y%m%d-%H%M%S).sql.gz"
echo "> Backing up all databases to $backup"
dump=$(command -v mariadb-dump || command -v mysqldump)
( umask 077; "$dump" --all-databases --single-transaction --routines --events --triggers | gzip > "$backup" )
ls -l "$backup"
echo "BACKUP=$backup""#;

/// Dumps all databases to /root/mariadb-backup-<time>.sql.gz and returns the path.
pub async fn backup(server: &dyn Server, log: Log<'_>) -> Result<String> {
    let values = script::run_capturing(server, &script::with_prelude(BACKUP), log, "Backing up the databases failed", &["BACKUP"], None).await?;
    Ok(values.or_empty("BACKUP").to_string())
}

const INSTALL: &str = r#"
echo "> apt-get install mariadb-server mariadb-client"
apt-get install -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold mariadb-server mariadb-client
version=$(dpkg-query -W -f='${Version}' mariadb-server)
case "$version" in *:{{Branch}}.*+maria*) ;; *) fail "Installed mariadb-server $version is not MariaDB {{Branch}} from mariadb.org." ;; esac

# When package scripts may not restart services, the old server keeps running the replaced binary.
# Shut it down cleanly, otherwise the new client cannot talk to it.
for pid in $(pgrep -x mariadbd; pgrep -x mysqld); do
  if readlink "/proc/$pid/exe" | grep -q '(deleted)'; then
    echo "  Stopping the old MariaDB server (pid $pid) that still runs the replaced program"
    kill -TERM "$pid"
    for i in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$pid" 2>/dev/null && fail "The old MariaDB server (pid $pid) did not stop."
  fi
done

echo "> Starting MariaDB"
if [ -d /run/systemd/system ]; then
  systemctl enable mariadb
  systemctl restart mariadb
else
  service mariadb restart
fi
for i in $(seq 1 30); do mariadb-admin ping >/dev/null 2>&1 && break; sleep 1; done
mariadb-admin ping || fail "MariaDB did not start."
running=$(mariadb -N -e 'SELECT VERSION()')
echo "  running: $running"
case "$running" in {{Branch}}.*) ;; *) fail "The running server is $running, not MariaDB {{Branch}}." ;; esac
echo "> mariadb-upgrade"
mariadb-upgrade
echo "VERSION=${version#*:}""#;

/// Returns the installed package version.
pub async fn install(server: &dyn Server, log: Log<'_>) -> Result<String> {
    let body = fill(INSTALL, &[("Branch", BRANCH)]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Installing MariaDB failed", &["VERSION"], None).await?;
    Ok(values.or_empty("VERSION").to_string())
}

const SECURE_AND_LISTEN: &str = r#"
echo "> Removing anonymous users"
mariadb -N -e "SELECT CONCAT(QUOTE(User), '@', QUOTE(Host)) FROM mysql.user WHERE User = ''" | while read -r account; do
  mariadb -e "DROP USER $account" && echo "  dropped $account"
done
echo "> Removing the test database"
mariadb -e "DROP DATABASE IF EXISTS test; DELETE FROM mysql.db WHERE Db = 'test' OR Db = 'test\\_%'; FLUSH PRIVILEGES;"
echo "  done (root keeps its unix_socket login; remote root stays allowed)"

echo "> Setting bind-address = 0.0.0.0 in {{ServerConfig}}"
f={{ServerConfig}}
[ -f "$f" ] || fail "$f does not exist."
cp -p "$f" "$f.bak-$(date +%Y%m%d-%H%M%S)"
if grep -qE '^[#[:space:]]*bind-address' "$f"; then
  sed -i -E 's|^[#[:space:]]*bind-address[[:space:]]*=.*|bind-address            = 0.0.0.0|' "$f"
else
  sed -i '/^\[mysqld\]/a bind-address            = 0.0.0.0' "$f"
fi
grep -E '^bind-address' "$f"

echo "> Restarting MariaDB"
if [ -d /run/systemd/system ]; then systemctl restart mariadb; else service mariadb restart; fi
for i in $(seq 1 30); do mariadb-admin ping >/dev/null 2>&1 && break; sleep 1; done
bind=$(mariadb -N -e 'SELECT @@bind_address')
echo "  listening on $bind:$(mariadb -N -e 'SELECT @@port')"
[ "$bind" = "0.0.0.0" ] || fail "MariaDB listens on '$bind' instead of 0.0.0.0 (another config file may set bind-address)."
# Port 3306 is never opened in the firewall. Tools like HeidiSQL connect through an SSH tunnel with a key."#;

/// Applies mariadb-secure-installation with the hotel's answers (n, n, Y, n, Y, Y), then sets bind-address = 0.0.0.0.
/// The firewall keeps 3306 closed.
pub async fn secure_and_listen(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(SECURE_AND_LISTEN, &[("ServerConfig", SERVER_CONFIG)]);
    script::run(server, &script::with_prelude(&body), log, "Securing MariaDB failed", None).await
}

/// root@localhost keeps its socket login and also accepts the password. An SSH tunnel arrives from 127.0.0.1, which
/// MariaDB matches to localhost rather than root@'%'.
pub fn local_root_password_sql(credentials: &DatabaseCredentials) -> String {
    format!(
        "ALTER USER {}@'localhost' IDENTIFIED VIA unix_socket OR mysql_native_password USING PASSWORD({});",
        sql_literal(&credentials.root_user),
        sql_literal(&credentials.root_password)
    )
}

const ALLOW_TUNNEL_ROOT_LOGIN: &str = r#"
echo "> root@localhost: socket login, and the root password for SSH tunnels (HeidiSQL)"
mariadb < "$secret"
mariadb --protocol=socket -N -e 'SELECT CURRENT_USER()' >/dev/null || fail "The socket login of root stopped working."
echo "  done""#;

/// For servers set up before [`local_root_password_sql`] existed.
pub async fn allow_tunnel_root_login(server: &dyn Server, credentials: &DatabaseCredentials, log: Log<'_>) -> Result<()> {
    let script = format!("{}{READ_SECRET}{ALLOW_TUNNEL_ROOT_LOGIN}", script::PRELUDE);
    let stdin = format!("{}\nFLUSH PRIVILEGES;\n", local_root_password_sql(credentials));
    script::run(server, &script, log, "Changing root@localhost failed", Some(&stdin)).await
}

const RESET_ROOT_PASSWORD: &str = r#"
command -v mariadb >/dev/null || fail "MariaDB is not installed on this server."
service_mariadb() { if [ -d /run/systemd/system ]; then systemctl "$1" mariadb; else service mariadb "$1"; fi; }
answers() { for i in $(seq 1 30); do mariadb -N -e 'SELECT 1' >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
if ! pgrep -x mariadbd >/dev/null && ! pgrep -x mysqld >/dev/null; then
  echo "> MariaDB is not running: starting it"
  service_mariadb start || true
  answers || true
fi

if mariadb --protocol=socket -N -e 'SELECT 1' >/dev/null 2>&1; then
  echo "> root logs in through the socket: setting the new password"
  mariadb < "$secret" || fail "Changing the root password failed (see above)."
else
  echo "> root cannot log in through the socket: restarting MariaDB without privilege checks for a moment"
  echo "  (--skip-grant-tables --skip-networking: nobody can connect from outside meanwhile)"
  safe=$(command -v mariadbd-safe || command -v mysqld_safe) || fail "mariadbd-safe is missing, so MariaDB cannot be started without privilege checks."
  service_mariadb stop || true
  # Without systemd the init script stops MariaDB by logging in as root, which is what does not work: stop it
  # with SIGTERM instead (a normal shutdown), the mariadbd-safe wrapper first so it does not start it again.
  stopped() { ! pgrep -x mariadbd >/dev/null && ! pgrep -x mysqld >/dev/null; }
  for i in $(seq 1 10); do stopped && break; sleep 1; done
  if ! stopped; then
    echo "  stopping it with SIGTERM instead"
    # By process name: -f would match this script's own command line.
    pkill -TERM -x mariadbd-safe 2>/dev/null || pkill -TERM -x mysqld_safe 2>/dev/null || true
    pkill -TERM -x mariadbd 2>/dev/null || pkill -TERM -x mysqld 2>/dev/null || true
    for i in $(seq 1 60); do stopped && break; sleep 1; done
  fi
  stopped || fail "MariaDB does not stop, so it cannot be started without privilege checks."
  install -d -o mysql -g mysql /run/mysqld
  "$safe" --skip-grant-tables --skip-networking </dev/null >/dev/null 2>&1 &
  answers || { service_mariadb start || true; fail "MariaDB did not start with --skip-grant-tables (journalctl -u mariadb)."; }
  changed=yes
  mariadb < "$secret" || changed=
  echo "> Starting MariaDB normally again"
  mariadb-admin shutdown 2>/dev/null || mysqladmin shutdown 2>/dev/null || true
  for i in $(seq 1 60); do stopped && break; sleep 1; done
  service_mariadb start
  answers || fail "MariaDB does not answer after the restart (journalctl -u mariadb)."
  [ -n "$changed" ] || fail "Changing the root password failed (see above). MariaDB runs normally again."
fi
out=$(mariadb --protocol=socket -N -e 'SELECT CURRENT_USER()' 2>&1) || fail "The socket login of root does not work: $out"
echo "  root (socket): logged in as $out""#;

/// Gives root@localhost (socket login kept) and root@'%' when it exists the credentials' root password. When root
/// cannot log in through the socket any more, MariaDB is restarted for a moment with --skip-grant-tables and
/// --skip-networking to change it, then started normally again.
pub async fn reset_root_password(server: &dyn Server, credentials: &DatabaseCredentials, log: Log<'_>) -> Result<()> {
    let root = sql_literal(&credentials.root_user);
    let password = sql_literal(&credentials.root_password);
    // FLUSH PRIVILEGES first: without grant tables, ALTER USER only works after it has loaded them.
    let sql = format!(
        "FLUSH PRIVILEGES;\n{}\nALTER USER IF EXISTS {root}@'%' IDENTIFIED BY {password};\nFLUSH PRIVILEGES;\n",
        local_root_password_sql(credentials)
    );
    let script = format!("{}{READ_SECRET}{RESET_ROOT_PASSWORD}", script::PRELUDE);
    script::run(server, &script, log, "Resetting the root password failed", Some(&sql)).await
}

const READ_ACCOUNTS: &str = r#"env={{env}}/.env
ini={{ini}}/config.ini
value() { grep -sE "^$1=" "$2" | head -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/' | tr -d '\r'; }
echo "DB=$(value db.database "$ini")"
echo "CMSUSER=$(value DB_USERNAME "$env")"
echo "CMSPASS=$(value DB_PASSWORD "$env")"
echo "EMUUSER=$(value db.username "$ini")"
echo "EMUPASS=$(value db.password "$ini")""#;

/// The CMS and emulator accounts as AtomCMS' .env and the emulator's config.ini use them, for a new credentials file.
pub async fn read_accounts(server: &dyn Server) -> Result<DatabaseAccounts> {
    let script = fill(READ_ACCOUNTS, &[("env", &crate::atom_cms::directory()), ("ini", &crate::emulator::directory())]);
    // The passwords are only read into memory, never logged.
    let values = script::run_capturing(
        server,
        &script,
        &|_: &str| {},
        "Reading the database accounts failed",
        &["DB", "CMSUSER", "CMSPASS", "EMUUSER", "EMUPASS"],
        None,
    )
    .await?;
    Ok(DatabaseAccounts {
        database: values.string("DB"),
        cms_user: values.string("CMSUSER"),
        cms_password: values.string("CMSPASS"),
        emulator_user: values.string("EMUUSER"),
        emulator_password: values.string("EMUPASS"),
    })
}

const CREATE_USERS: &str = r#"
echo "> Creating {{RootUser}}@'%', {{CmsUser}}@'%' and the database {{Database}}"
mariadb < "$secret"
# The password hash stays out of the log.
mariadb -N -e "SHOW GRANTS FOR {{cms}}" | sed -E "s/(IDENTIFIED BY PASSWORD) '[^']*'/\1 '***'/""#;

/// The SQL goes over stdin so the passwords never appear on a command line.
pub async fn create_users(server: &dyn Server, credentials: &DatabaseCredentials, log: Log<'_>) -> Result<()> {
    let root = account(&credentials.root_user);
    let cms = account(&credentials.cms_user);
    let db = identifier(&credentials.database);
    let root_password = sql_literal(&credentials.root_password);
    let cms_password = sql_literal(&credentials.cms_password);
    let sql = format!(
        "CREATE USER IF NOT EXISTS {root} IDENTIFIED BY {root_password};\n\
         ALTER USER {root} IDENTIFIED BY {root_password};\n\
         GRANT ALL PRIVILEGES ON *.* TO {root} WITH GRANT OPTION;\n\
         {}\n\
         CREATE USER IF NOT EXISTS {cms} IDENTIFIED BY {cms_password};\n\
         ALTER USER {cms} IDENTIFIED BY {cms_password};\n\
         CREATE SCHEMA IF NOT EXISTS {db};\n\
         GRANT {CMS_PRIVILEGES} ON {db}.* TO {cms};\n\
         FLUSH PRIVILEGES;\n",
        local_root_password_sql(credentials)
    );
    let body = fill(
        CREATE_USERS,
        &[
            ("RootUser", &credentials.root_user),
            ("CmsUser", &credentials.cms_user),
            ("Database", &credentials.database),
            ("cms", &cms.replace('"', "\\\"")),
        ],
    );
    let script = format!("{}{READ_SECRET}{body}", script::PRELUDE);
    script::run(server, &script, log, "Creating the users failed", Some(&sql)).await
}

const TEST_LOGIN: &str = r#"ip=$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -vE '^(127\.|::1|fe80)' | head -1)
set --
{{remote}}
[ -n "$ip" ] && set -- "--protocol=tcp --host=$ip"
{{local}}
[ $# -gt 0 ] || { echo "  no network address to test a remote login from"; exit 0; }
for how in "$@"; do
  # Single quotes because the SQL has backticks, which bash would run inside double quotes.
  out=$(mariadb --defaults-extra-file="$secret" $how -N -e '{{check}}' 2>&1) || fail "Login as {{user}} ($how) failed: $out"
  echo "  {{user}} ($how): logged in as $(echo "$out" | head -1)"
done"#;

/// None if `user` can log in and use the database, else the reason. Connects to the server's own network address so
/// user@'%' is tested (unless `remote` is false). `local` also tests 127.0.0.1 and the socket.
pub async fn test_login(
    server: &dyn Server,
    user: &str,
    password: &str,
    database: Option<&str>,
    local: bool,
    log: Log<'_>,
    remote: bool,
) -> Result<Option<String>> {
    let options = format!("[client]\nuser={user}\npassword={password}\n");
    let check = match database {
        None => "SELECT CURRENT_USER()".to_string(),
        Some(database) => format!(
            "SELECT CURRENT_USER(); CREATE TABLE {0}.aio_install_check (id INT); DROP TABLE {0}.aio_install_check",
            identifier(database)
        ),
    };
    let body = fill(
        TEST_LOGIN,
        &[
            ("remote", if remote { "" } else { "ip=" }),
            ("local", if local { "set -- \"$@\" \"--protocol=tcp --host=127.0.0.1\" \"--protocol=socket\"" } else { "" }),
            ("check", &check),
            ("user", user),
        ],
    );
    let script = format!("{}{READ_SECRET}{body}", script::PRELUDE);
    caught(script::run(server, &script, log, &format!("Testing {user} failed"), Some(&options)).await)
}

const TEST_LOCAL_ROOT: &str = r#"out=$(mariadb --protocol=socket -N -e 'SELECT CURRENT_USER()' 2>&1) || fail "Local root login (unix_socket) failed: $out"
echo "  root (--protocol=socket, unix_socket): logged in as $out""#;

/// Local root must still log in through unix_socket without a password. None on success, else the reason.
pub async fn test_local_root(server: &dyn Server, log: Log<'_>) -> Result<Option<String>> {
    caught(script::run(server, &script::with_prelude(TEST_LOCAL_ROOT), log, "Testing local root failed", None).await)
}

/// A ServerError becomes the returned reason (C#: catch (ServerConnectionException ex) => ex.Message); other errors
/// pass through.
fn caught(result: Result<()>) -> Result<Option<String>> {
    match result {
        Ok(()) => Ok(None),
        Err(error) => match error.downcast::<ServerError>() {
            Ok(ServerError(message)) => Ok(Some(message)),
            Err(other) => Err(other),
        },
    }
}


fn account(user: &str) -> String {
    format!("{}@'%'", sql_literal(user))
}

fn identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(version: Option<&str>) -> MariaDbState {
        MariaDbState {
            codename: "noble".into(),
            installed_version: version.map(str::to_string),
            mysql_installed: false,
            running: true,
            bind_address: Some("0.0.0.0".into()),
            database_exists: true,
            root_remote_exists: true,
            cms_exists: true,
        }
    }

    #[test]
    fn versions() {
        let s = state(Some("1:12.3.2+maria~ubu2404"));
        assert_eq!(s.display_version().as_deref(), Some("12.3.2+maria~ubu2404"));
        assert!(s.from_mariadb_org());
        assert!(s.is_target_branch());
        assert!(!s.needs_upgrade());
        assert!(s.is_complete());

        let distro = state(Some("1:10.11.8-0ubuntu0.24.04.1"));
        assert!(!distro.from_mariadb_org());
        assert!(distro.needs_upgrade());
        assert!(!distro.is_complete());

        let other = state(Some("1:11.4.2+maria~ubu2404"));
        assert!(!other.is_target_branch());
        assert!(other.needs_upgrade());

        let none = state(None);
        assert!(!none.needs_upgrade());
        assert_eq!(none.display_version(), None);

        let mut local = state(Some("1:12.3.2+maria~ubu2404"));
        local.bind_address = Some("127.0.0.1".into());
        assert!(!local.is_complete());
        local.bind_address = Some("0.0.0.0".into());
        local.cms_exists = false;
        assert!(!local.users_exist());
        assert!(!local.is_complete());
    }

    #[test]
    fn sql_pieces() {
        assert_eq!(account("o'k"), "'o\\'k'@'%'");
        assert_eq!(identifier("ha`bbo"), "`ha``bbo`");
        assert_eq!(fill("a {{x}} {{y}} {{z}}", &[("x", "{{y}}"), ("y", "2")]), "a {{y}} 2 {{z}}");
    }

    #[test]
    fn caught_errors() {
        assert_eq!(caught(Ok(())).unwrap(), None);
        assert_eq!(caught(Err(ServerError("no".into()).into())).unwrap().as_deref(), Some("no"));
        assert!(caught(Err(anyhow::anyhow!("io"))).is_err());
    }
}
