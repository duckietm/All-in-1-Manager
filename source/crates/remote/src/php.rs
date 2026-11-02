//! PHP 8.5 (8.5.12 or newer) from packages.sury.org, PHP-FPM and its NGINX test. (PhpInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub const SOCKET: &str = "/run/php/php8.5-fpm.sock";

// ---------------------------------------------------------------------------------------------------------------

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server, ServerError};

/// PHP as found on the server before the PHP step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpState {
    pub codename: String,
    pub php_version: Option<String>,
    pub missing_packages: Vec<String>,
    pub other_versions: Vec<String>,
    pub repository_configured: bool,
    pub fpm_running: bool,
}

impl PhpState {
    pub fn version_ok(&self) -> bool {
        self.php_version.as_deref().is_some_and(is_at_least_minimum)
    }

    pub fn is_complete(&self) -> bool {
        self.missing_packages.is_empty() && self.version_ok() && self.fpm_running
    }
}

pub const BRANCH: &str = "8.5";

// Older 8.5 releases have known security problems.
pub const MINIMUM_VERSION: &str = "8.5.12";

// DEB.SURY.ORG Automatic Signing Key, shipped in debsuryorg-archive-keyring.deb.
pub const SURY_KEY_FINGERPRINT: &str = "15058500A0235D97F5D10063B188E2B695BD4743";

pub const PACKAGES: [&str; 16] = [
    "php8.5-fpm", "php8.5", "php8.5-common", "php8.5-mysql",
    "php8.5-xml", "php8.5-xmlrpc", "php8.5-curl", "php8.5-gd",
    "php8.5-imagick", "php8.5-cli", "php8.5-mbstring",
    "php8.5-soap", "php8.5-zip", "php8.5-intl", "php8.5-bcmath", "unzip",
];

// Lower-case names as listed by "php -m".
const EXTENSIONS: [&str; 12] = ["mysqli", "pdo_mysql", "xml", "xmlrpc", "curl", "gd", "imagick", "mbstring", "soap", "zip", "intl", "bcmath"];

/// Like C#'s System.Version: 2 to 4 non-negative components; a missing build or revision compares as -1.
fn parse_version(text: &str) -> Option<[i64; 4]> {
    let parts: Vec<&str> = text.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut version = [-1i64; 4];
    for (i, part) in parts.iter().enumerate() {
        let value: i32 = part.parse().ok()?;
        if value < 0 {
            return None;
        }
        version[i] = i64::from(value);
    }
    Some(version)
}

// Only the numeric x.y.z prefix is compared.
pub fn is_at_least_minimum(php_version: &str) -> bool {
    let numeric: String = php_version.chars().take_while(|c| c.is_numeric() || *c == '.').collect();
    match (parse_version(&numeric), parse_version(MINIMUM_VERSION)) {
        (Some(version), Some(minimum)) => version >= minimum,
        _ => false,
    }
}

const INSPECT: &str = r#". /etc/os-release
echo "codename=${VERSION_CODENAME:-$(lsb_release -cs 2>/dev/null)}"
echo "php=$(command -v php8.5 >/dev/null 2>&1 && php8.5 -r 'echo PHP_VERSION;')"
missing=""
for p in PACKAGES; do
  dpkg-query -W -f='${db:Status-Abbrev}' "$p" 2>/dev/null | grep -q '^ii' || missing="$missing $p"
done
echo "missing=$missing"
echo "others=$(ls /etc/php 2>/dev/null | grep -vx '8.5' | tr '\n' ' ')"
echo "repo=$(grep -qs 'packages.sury.org/php' /etc/apt/sources.list.d/*.list && echo yes)"
# [r] keeps pgrep from matching this script's own command line
echo "fpm=$(pgrep -f 'php-fpm: maste[r] process \(/etc/php/8\.5/' >/dev/null && echo yes)""#;

pub async fn inspect(server: &dyn Server) -> Result<PhpState> {
    let script = INSPECT.replace("PACKAGES", &PACKAGES.join(" "));
    let result = server.run_as_root(&script).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("Could not inspect PHP on the server: {}", result.error.trim())).into());
    }
    let values = ScriptValues::parse(&result.output);
    let list = |key: &str| values.or_empty(key).split(' ').filter(|p| !p.is_empty()).map(str::to_string).collect::<Vec<_>>();
    Ok(PhpState {
        codename: values.or_empty("codename").to_string(),
        php_version: values.string("php"),
        missing_packages: list("missing"),
        other_versions: list("others"),
        repository_configured: values.yes("repo"),
        fpm_running: values.yes("fpm"),
    })
}

const ADD_REPOSITORY: &str = r#"
echo "> apt-get update"
apt_update
echo "> Installing prerequisites"
apt-get install -y --no-install-recommends lsb-release ca-certificates curl gnupg
curl -fsSI "https://packages.sury.org/php/dists/$codename/Release" >/dev/null || fail "packages.sury.org has no PHP packages for $PRETTY_NAME ($codename)."

echo "> Downloading and checking the Sury keyring"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/keyring.deb" https://packages.sury.org/debsuryorg-archive-keyring.deb || fail "Could not download debsuryorg-archive-keyring.deb."
[ "$(dpkg-deb -f "$tmp/keyring.deb" Package)" = debsuryorg-archive-keyring ] || fail "The downloaded file is not the debsuryorg-archive-keyring package."
dpkg-deb -x "$tmp/keyring.deb" "$tmp/x"
fprs=$(gpg --show-keys --with-colons "$tmp/x/usr/share/keyrings/debsuryorg-archive-keyring.gpg" 2>/dev/null | awk -F: '/^pub/{p=1; next} /^fpr/ && p {print $10; p=0}')
[ "$fprs" = "{{SuryKeyFingerprint}}" ] || fail "The Sury keyring does not hold the expected key {{SuryKeyFingerprint}} (found: $fprs)."
echo "  key {{SuryKeyFingerprint}} ok"
dpkg -i "$tmp/keyring.deb"

echo "> Adding https://packages.sury.org/php/ ($codename)"
echo "deb [signed-by=/usr/share/keyrings/debsuryorg-archive-keyring.gpg] https://packages.sury.org/php/ $codename main" > /etc/apt/sources.list.d/php.list
# Always take PHP 8.5 from Sury, even where the distribution has its own older 8.5 packages.
printf 'Package: php8.5*\nPin: origin packages.sury.org\nPin-Priority: 900\n' > /etc/apt/preferences.d/99sury-php
apt_update"#;

// Follows packages.sury.org/php/README.txt, but checks the keyring before installing it.
pub async fn add_repository(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(ADD_REPOSITORY, &[("SuryKeyFingerprint", SURY_KEY_FINGERPRINT)]);
    script::run(server, &script::with_prelude(&body), log, "Adding the PHP repository failed", None).await
}

const INSTALL_PACKAGES: &str = r#"
echo "> apt-get install {{packages}}"
apt-get install -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold {{packages}}
v=$(php8.5 -r 'echo PHP_VERSION;')
dpkg --compare-versions "$v" ge {{MinimumVersion}} || fail "PHP $v is installed, but at least {{MinimumVersion}} is required: older versions have security problems."
# Make 'php' on the command line 8.5 even when other versions are installed.
update-alternatives --set php /usr/bin/php8.5 >/dev/null 2>&1 || true
echo "VERSION=$v""#;

/// Fails on a PHP older than [`MINIMUM_VERSION`] and returns the installed version.
pub async fn install_packages(server: &dyn Server, log: Log<'_>) -> Result<String> {
    let packages = PACKAGES.join(" ");
    let body = fill(INSTALL_PACKAGES, &[("packages", &packages), ("MinimumVersion", MINIMUM_VERSION)]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Installing PHP failed", &["VERSION"], None).await?;
    Ok(values.or_empty("VERSION").to_string())
}

const CHECK_EXTENSIONS: &str = r#"
echo "> php8.5 -m"
mods=$(php8.5 -m | tr 'A-Z' 'a-z')
missing=""
for m in {{extensions}}; do
  if echo "$mods" | grep -qx "$m"; then echo "  $m ok"; else missing="$missing $m"; fi
done
[ -z "$missing" ] || fail "These PHP extensions are not loaded:$missing"
echo "> php-fpm8.5 -t"
php-fpm8.5 -t"#;

pub async fn check_extensions(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(CHECK_EXTENSIONS, &[("extensions", &EXTENSIONS.join(" "))]);
    script::run(server, &script::with_prelude(&body), log, "Checking PHP failed", None).await
}

const START_AND_CONNECT: &str = r#"
echo "> Starting PHP-FPM"
if [ -d /run/systemd/system ]; then
  systemctl enable php8.5-fpm
  systemctl restart php8.5-fpm
else
  service php8.5-fpm restart
fi
for i in 1 2 3 4 5 6 7 8 9 10; do [ -S {{Socket}} ] && break; sleep 0.5; done
[ -S {{Socket}} ] || fail "PHP-FPM is not listening on {{Socket}}."
echo "  listening on {{Socket}}"

if ! command -v nginx >/dev/null 2>&1; then
  echo "  NGINX is not installed: skipping the NGINX + PHP test"
  echo "NGINX=no"
  exit 0
fi

# nginx.org's NGINX runs as 'nginx', but the PHP-FPM socket belongs to www-data with mode 660.
if id nginx >/dev/null 2>&1 && ! id -nG nginx | grep -qw www-data; then
  usermod -aG www-data nginx
  echo "  user nginx added to group www-data (for the PHP-FPM socket)"
fi

install -d -m 755 /etc/nginx/snippets
if [ ! -f /etc/nginx/snippets/fastcgi-php.conf ]; then
  cat > /etc/nginx/snippets/fastcgi-php.conf <<'SNIPPET'
# regex to split $uri to $fastcgi_script_name and $fastcgi_path
fastcgi_split_path_info ^(.+?\.php)(/.*)$;

# Check that the PHP script exists before passing it
try_files $fastcgi_script_name =404;

# Bypass the fact that try_files resets $fastcgi_path_info
set $path_info $fastcgi_path_info;
fastcgi_param PATH_INFO $path_info;

fastcgi_index index.php;
include fastcgi_params;
fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
SNIPPET
  echo "  /etc/nginx/snippets/fastcgi-php.conf created"
fi

reload() {
  if [ -d /run/systemd/system ]; then systemctl reload nginx; else nginx -s reload 2>/dev/null || nginx; fi
}

echo "> Testing PHP through NGINX"
# Temporary site on 127.0.0.1:8085. PHP-FPM has a private /tmp, so the test file lives in /var/www.
root=$(mktemp -d /var/www/aio-php-test.XXXXXX 2>/dev/null || { install -d -m 755 /var/www; mktemp -d /var/www/aio-php-test.XXXXXX; })
conf=/etc/nginx/conf.d/zz-aio-php-test.conf
cleanup() { rm -rf "$root"; rm -f "$conf"; reload >/dev/null 2>&1 || true; }
trap cleanup EXIT
chmod 755 "$root"
echo '<?php echo "PHP-OK " . PHP_VERSION;' > "$root/index.php"
chmod 644 "$root/index.php"
cat > "$conf" <<SITE
server {
    listen 127.0.0.1:8085;
    root $root;
    location ~ \.php\$ {
        include snippets/fastcgi-php.conf;
        fastcgi_pass unix:{{Socket}};
    }
}
SITE
nginx -t
reload
sleep 1
answer=$(curl -s --noproxy '*' http://127.0.0.1:8085/index.php || true)
echo "  answer: $answer"
case "$answer" in "PHP-OK "*) ;; *) fail "NGINX could not run PHP through PHP-FPM (answer: '$answer')." ;; esac
echo "NGINX=yes""#;

/// Starts PHP-FPM and, if NGINX is installed, wires it up and tests a real PHP request through it. Returns whether
/// NGINX was present.
pub async fn start_and_connect(server: &dyn Server, log: Log<'_>) -> Result<bool> {
    let body = fill(START_AND_CONNECT, &[("Socket", SOCKET)]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Starting PHP-FPM failed", &["NGINX"], None).await?;
    Ok(values.yes("NGINX"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_version() {
        assert!(is_at_least_minimum("8.5.12"));
        assert!(is_at_least_minimum("8.5.13-1+ubuntu24.04.1+deb.sury.org+1"));
        assert!(is_at_least_minimum("8.6.0"));
        assert!(is_at_least_minimum("8.5.12.1"));
        assert!(!is_at_least_minimum("8.5.11"));
        assert!(!is_at_least_minimum("8.5"));
        assert!(!is_at_least_minimum("8"));
        assert!(!is_at_least_minimum(""));
        assert!(!is_at_least_minimum("8.5."));
        assert!(!is_at_least_minimum("8.5.1.2.3"));
    }

    #[test]
    fn state() {
        let mut s = PhpState {
            codename: "noble".into(),
            php_version: Some("8.5.12".into()),
            missing_packages: vec![],
            other_versions: vec![],
            repository_configured: true,
            fpm_running: true,
        };
        assert!(s.is_complete());
        s.php_version = None;
        assert!(!s.version_ok());
        assert!(!s.is_complete());
    }
}
