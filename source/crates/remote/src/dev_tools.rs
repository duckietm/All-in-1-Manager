//! Node.js from NodeSource, Composer from getcomposer.org and Yarn through Corepack. (DevToolsInstaller.cs)

use std::sync::Mutex;

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server, ServerError};

/// Node.js, Composer and Yarn as found on the server before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevToolsState {
    pub node_version: Option<String>,
    pub node_from_node_source: bool,
    pub yarn_version: Option<String>,
    pub composer_version: Option<String>,
    pub php_installed: bool,
    pub shadowing: Vec<String>,
}

impl DevToolsState {
    pub fn node_major(&self) -> Option<i32> {
        let version = self.node_version.as_deref()?;
        version.trim_start_matches('v').split('.').next().unwrap_or("").trim().parse().ok()
    }

    pub fn is_complete(&self) -> bool {
        self.node_from_node_source && self.node_major() == Some(NODE_MAJOR) && self.yarn_version.is_some() && self.composer_version.is_some()
    }
}

/// Installed versions, plus older copies that come first in root's PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevToolsVersions {
    pub node: String,
    pub npm: String,
    pub yarn: String,
    pub composer: String,
    pub shadowing: Vec<String>,
}

pub const NODE_MAJOR: i32 = 26;

// "NSolid <nsolid-gpg@nodesource.com>", which signs deb.nodesource.com.
pub const NODE_SOURCE_KEY_FINGERPRINT: &str = "6F71F525282841EEDAF851B42F59B5F99B1BE0B4";

// Pins PATH to the /usr/bin tools this step installs. shadowing() lists older copies earlier in root's PATH.
const TOOL_ENVIRONMENT: &str = r#"login_path="$PATH"
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
shadowing() {
  for t in node npm yarn composer; do
    p=$(PATH="$login_path" command -v "$t" 2>/dev/null) || continue
    [ "$p" != "/usr/bin/$t" ] && printf '%s ' "$p"
  done
  return 0
}
# Composer refuses to run as root without this, and the installer runs as root on purpose.
export COMPOSER_ALLOW_SUPERUSER=1
# Keep npm's update notices out of the log.
export npm_config_update_notifier=false
# Corepack would otherwise ask before downloading Yarn.
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
"#;

fn words(text: &str) -> Vec<String> {
    text.split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

const INSPECT: &str = r#"echo "node=$(node --version 2>/dev/null)"
echo "nodesource=$(dpkg-query -W -f='${Version}' nodejs 2>/dev/null | grep -q nodesource && echo yes)"
echo "yarn=$(cd /tmp && yarn --version 2>/dev/null)"
echo "composer=$(composer --version --no-ansi 2>/dev/null | awk '{print $3}')"
echo "php=$(command -v php >/dev/null 2>&1 && echo yes)"
echo "shadowing=$(shadowing)""#;

pub async fn inspect(server: &dyn Server) -> Result<DevToolsState> {
    let result = server.run_as_root(&format!("{TOOL_ENVIRONMENT}{INSPECT}")).await?;
    let values = ScriptValues::parse(&result.output);
    Ok(DevToolsState {
        node_version: values.string("node"),
        node_from_node_source: values.yes("nodesource"),
        yarn_version: values.string("yarn"),
        composer_version: values.string("composer"),
        php_installed: values.yes("php"),
        shadowing: words(values.or_empty("shadowing")),
    })
}

const ADD_NODE_SOURCE: &str = r#"
echo "> apt-get update"
apt_update
echo "> Installing prerequisites"
apt-get install -y --no-install-recommends ca-certificates curl gnupg
install -d -m 0755 /etc/apt/keyrings

echo "> Downloading and checking the NodeSource signing key"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key -o "$tmp" || fail "Could not download the NodeSource signing key."
fprs=$(gpg --show-keys --with-colons "$tmp" 2>/dev/null | awk -F: '/^pub/{p=1; next} /^fpr/ && p {print $10; p=0}')
[ "$fprs" = "{{NodeSourceKeyFingerprint}}" ] || fail "The downloaded key is not the NodeSource key {{NodeSourceKeyFingerprint}} (found: $fprs)."
echo "  key {{NodeSourceKeyFingerprint}} ok"
gpg --dearmor --yes -o /etc/apt/keyrings/nodesource.gpg "$tmp"

echo "> Adding https://deb.nodesource.com/node_{{NodeMajor}}.x"
echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_{{NodeMajor}}.x nodistro main" > /etc/apt/sources.list.d/nodesource.list
# Always take Node.js from NodeSource, even where the distribution ships its own nodejs package.
printf 'Package: nodejs\nPin: origin deb.nodesource.com\nPin-Priority: 900\n' > /etc/apt/preferences.d/99nodesource
apt_update"#;

pub async fn add_node_source(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let major = NODE_MAJOR.to_string();
    let body = fill(ADD_NODE_SOURCE, &[("NodeSourceKeyFingerprint", NODE_SOURCE_KEY_FINGERPRINT), ("NodeMajor", &major)]);
    script::run(server, &script::with_prelude(&body), log, "Adding the NodeSource repository failed", None).await
}

const INSTALL_NODE: &str = r#"echo "> apt-get install nodejs"
apt-get install -y nodejs
version=$(node --version)
case "$version" in v{{NodeMajor}}.*) ;; *) fail "node $version is installed instead of Node.js {{NodeMajor}}." ;; esac
echo "  node $version, npm $(npm --version)"
echo "VERSION=$version""#;

/// Returns the installed version, such as "v26.12.1". npm comes with the nodejs package.
pub async fn install_node(server: &dyn Server, log: Log<'_>) -> Result<String> {
    let body = fill(INSTALL_NODE, &[("NodeMajor", &NODE_MAJOR.to_string())]);
    let script = format!("{}{TOOL_ENVIRONMENT}{body}", script::PRELUDE);
    let values = script::run_capturing(server, &script, log, "Installing Node.js failed", &["VERSION"], None).await?;
    Ok(values.or_empty("VERSION").to_string())
}

const INSTALL_COMPOSER: &str = r#"command -v php >/dev/null 2>&1 || fail "Composer needs PHP, which is not installed. Run the PHP step first."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
base=https://getcomposer.org/download/latest-stable
echo "> Downloading $base/composer.phar"
download "$base/composer.phar.sha256" "$tmp/composer.phar.sha256" || fail "Could not download the Composer checksum from getcomposer.org."
download "$base/composer.phar" "$tmp/composer.phar" || fail "Could not download Composer from getcomposer.org."
expected=$(tr -dc '0-9a-f' < "$tmp/composer.phar.sha256")
echo "$expected  $tmp/composer.phar" | sha256sum -c --status || fail "The downloaded Composer does not match its published checksum."
echo "  checksum ok (sha256 $expected)"
php "$tmp/composer.phar" --version --no-ansi >/dev/null 2>&1 || fail "The downloaded composer.phar does not run."
install -m 755 "$tmp/composer.phar" /usr/bin/composer
echo "  installed as /usr/bin/composer"
composer --version --no-ansi 2>/dev/null"#;

/// Installs the latest stable composer.phar after checking its published SHA-256. composer-setup.php is avoided
/// because it downloads the phar without a timeout.
pub async fn install_composer(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let script = format!("{}{TOOL_ENVIRONMENT}{INSTALL_COMPOSER}", script::PRELUDE);
    script::run(server, &script, log, "Installing Composer failed", None).await
}

const INSTALL_YARN: &str = r#"echo "> npm install -g corepack"
npm install -g --no-fund --no-audit corepack
echo "> corepack enable"
corepack enable
# Projects pin Yarn with packageManager in package.json. This sets the default everywhere else.
echo "> corepack install --global yarn@stable"
corepack install --global yarn@stable"#;

pub async fn install_yarn(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let script = format!("{}{TOOL_ENVIRONMENT}{INSTALL_YARN}", script::PRELUDE);
    script::run(server, &script, log, "Installing Yarn failed", None).await
}

const VERSIONS: &str = r#"cd /tmp
echo "NODE=$(node --version)"
echo "NPM=$(npm --version)"
echo "YARN=$(yarn --version)"
echo "COMPOSER=$(composer --version --no-ansi 2>/dev/null | awk 'NR==1 {print $3}')"
echo "SHADOWING=$(shadowing)""#;

pub async fn versions(server: &dyn Server, log: Log<'_>) -> Result<DevToolsVersions> {
    let script = format!("{}{TOOL_ENVIRONMENT}{VERSIONS}", script::PRELUDE);
    let lines = Mutex::new(Vec::<String>::new());
    script::run(
        server,
        &script,
        &|line: &str| {
            lines.lock().unwrap().push(line.to_string());
            log(line);
        },
        "Checking the versions failed",
        None,
    )
    .await?;
    let lines = lines.into_inner().unwrap();
    let values = ScriptValues::from_lines(lines.iter().map(String::as_str));
    let get = |key: &str| -> Result<String> {
        values.string(key).ok_or_else(|| ServerError(format!("{} --version printed nothing.", key.to_lowercase())).into())
    };
    Ok(DevToolsVersions {
        node: get("NODE")?,
        npm: get("NPM")?,
        yarn: get("YARN")?,
        composer: get("COMPOSER")?,
        shadowing: words(values.or_empty("SHADOWING")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_major() {
        let mut s = DevToolsState {
            node_version: Some("v26.12.1".into()),
            node_from_node_source: true,
            yarn_version: Some("4.9.0".into()),
            composer_version: Some("2.8.10".into()),
            php_installed: true,
            shadowing: vec![],
        };
        assert_eq!(s.node_major(), Some(26));
        assert!(s.is_complete());
        s.node_version = Some("v22.1.0".into());
        assert!(!s.is_complete());
        s.node_version = Some("vx".into());
        assert_eq!(s.node_major(), None);
        s.node_version = None;
        assert_eq!(s.node_major(), None);
    }
}
