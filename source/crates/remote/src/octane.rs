//! yarn install, Octane's configuration and the gamedata folders, yarn build, and serving dist/. (OctaneInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use aio_core::ServerLayout;

pub fn octane() -> String {
    ServerLayout::current().octane.clone()
}

pub fn renderer() -> String {
    ServerLayout::current().renderer.clone()
}

pub fn gamedata() -> String {
    ServerLayout::current().gamedata.clone()
}

pub fn dist() -> String {
    format!("{}/dist", octane())
}

pub fn configuration_directory() -> String {
    format!("{}/public/configuration", octane())
}

pub const PERMISSIONS_SCRIPT: &str = "/usr/local/sbin/gamedata-permissions";

// ---------------------------------------------------------------------------------------------------------------

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server};
use crate::web_access::{self, WebAccessSettings};

/// Octane state before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OctaneState {
    pub site: Option<WebAccessSettings>,
    pub octane_present: bool,
    pub renderer_present: bool,
    pub node_version: Option<String>,
    pub configured_socket_url: Option<String>,
    pub built: bool,
    pub site_serves_octane: bool,
    pub gamedata_present: bool,
    pub ui_camera_url: Option<String>,
    pub emulator_imager: Option<String>,
    pub config_base_url: Option<String>,
}

impl OctaneState {
    pub fn expected_socket_url(&self) -> Option<String> {
        self.site.as_ref().map(socket_url)
    }

    /// Both ui-config.json and the emulator's imager settings point at gamedata/camera.
    pub fn camera_configured(&self) -> bool {
        self.site.as_ref().is_some_and(|site| {
            self.ui_camera_url.as_deref() == Some(format!("{}/gamedata/camera", site.url()).as_str())
                && self.emulator_imager.as_deref() == Some(crate::emulator::imager_settings_line(&site.url()).as_str())
        })
    }

    /// client-mode.json uses /configuration/, or /client/configuration/ with a CMS.
    pub fn layout_configured(&self) -> bool {
        self.site.as_ref().is_some_and(|site| self.config_base_url.as_deref() == Some(format!("{}configuration/", site.client_url()).as_str()))
    }

    pub fn is_complete(&self) -> bool {
        self.site.is_some()
            && self.built
            && self.site_serves_octane
            && self.gamedata_present
            && self.configured_socket_url == self.expected_socket_url()
            && self.camera_configured()
            && self.layout_configured()
    }
}

/// Files the manual install renames from .example to .json.
pub const CONFIGURATION_FILES: [&str; 6] = ["client-mode", "hotlooks", "infostand_backgrounds", "news", "renderer-config", "ui-config"];

// Use Node.js 26 from /usr/bin, never an older copy in /usr/local/bin.
const ENVIRONMENT: &str = "export PATH=/usr/sbin:/usr/bin:/sbin:/bin COREPACK_ENABLE_DOWNLOAD_PROMPT=0 CI=1 NO_COLOR=1 FORCE_COLOR=0 HOME=/root";

/// Sets or removes vite's --base in the start and build scripts of package.json, as the AtomCMS guide does. The
/// Octane clone step undoes it before a git pull.
pub fn package_base_command(site: &WebAccessSettings) -> String {
    let mut command = String::from(r#"sed -i -E -e 's#("(start|build)": "vite) --base=[^ "]+#\1#'"#);
    if site.has_cms() {
        command.push_str(&format!(r#" -e 's#("(start|build)": "vite)( |")#\1 --base={}\3#'"#, site.client_path()));
    }
    command.push_str(" package.json");
    command
}

pub fn socket_url(site: &WebAccessSettings) -> String {
    let url = site.url();
    let rest = &url[url.find("://").map_or(0, |i| i + 3)..];
    format!("{}://{}/ws", if site.https { "wss" } else { "ws" }, rest)
}

const INSPECT: &str = r#"{{Environment}}
[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
echo "octane=$([ -f {{Octane}}/package.json ] && echo yes)"
echo "renderer=$([ -f {{Renderer}}/package.json ] && echo yes)"
echo "node=$(node -v 2>/dev/null)"
echo "socket=$(node -e 'try { console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))["socket.url"] || "") } catch { }' {{ConfigurationDirectory}}/renderer-config.json 2>/dev/null)"
echo "built=$([ -f {{Dist}}/index.html ] && echo yes)"
echo "site=$(grep -Eqs '(root|alias) {{Dist}}/?;' {{SiteFile}} && echo yes)"
echo "configbase=$(node -e 'try { console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))["plainConfigBaseUrl"] || "") } catch { }' {{ConfigurationDirectory}}/client-mode.json 2>/dev/null)"
echo "gamedata=$([ -d {{Gamedata}}/config ] && echo yes)"
echo "uicamera=$(node -e 'try { console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))["camera.url"] || "") } catch { }' {{ConfigurationDirectory}}/ui-config.json 2>/dev/null)"
db=$(grep -sE '^db.database=' {{EmulatorDirectory}}/config.ini | cut -d= -f2)
[ -n "$db" ] && echo "emuimager=$(mariadb -N "$db" -e "SELECT GROUP_CONCAT(\`key\`, '=', \`value\` ORDER BY \`key\` SEPARATOR ',') FROM emulator_settings WHERE \`key\` IN ({{ImagerKeysSql}})" 2>/dev/null)""#;

pub async fn inspect(server: &dyn Server) -> Result<OctaneState> {
    let script = fill(
        INSPECT,
        &[
            ("Environment", ENVIRONMENT),
            ("TraefikDirectory", web_access::traefik_directory()),
            ("Octane", &octane()),
            ("Renderer", &renderer()),
            ("ConfigurationDirectory", &configuration_directory()),
            ("Dist", &dist()),
            ("SiteFile", web_access::site_file()),
            ("Gamedata", &gamedata()),
            ("EmulatorDirectory", &crate::emulator::directory()),
            ("ImagerKeysSql", &crate::emulator::imager_keys_sql()),
        ],
    );
    let result = server.run_as_root(&script).await?;
    Ok(parse_state(&ScriptValues::parse(&result.output)))
}

fn parse_state(values: &ScriptValues) -> OctaneState {
    OctaneState {
        site: WebAccessSettings::from_hotel_env(values),
        octane_present: values.yes("octane"),
        renderer_present: values.yes("renderer"),
        node_version: values.string("node"),
        configured_socket_url: values.string("socket"),
        built: values.yes("built"),
        site_serves_octane: values.yes("site"),
        gamedata_present: values.yes("gamedata"),
        ui_camera_url: values.string("uicamera"),
        emulator_imager: values.string("emuimager"),
        config_base_url: values.string("configbase"),
    }
}

const YARN_INSTALL: &str = r#"{{Environment}}
set -o pipefail
command -v node >/dev/null && command -v yarn >/dev/null || fail "Node.js and Yarn are missing in /usr/bin. Run the Node.js step first."
cd {{directory}}
{{fresh}}
echo "> cd {{directory}} && yarn install   (node $(node -v), yarn $(yarn --version))"
# Filter Yarn's step frames, but keep its exit code.
rc=$(mktemp)
(yarn install 2>&1; echo $? > "$rc") | grep -vE '^➤ YN0000: [│┌└]|^➤ YN0007|^➤ YN0085' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "yarn install in {{directory}} stopped with exit code $code (see above).""#;

const FRESH: &str = r#"echo "> rm -rf node_modules yarn.lock"
rm -rf node_modules yarn.lock
# CI mode makes Yarn refuse to write a lockfile, but here a new one is the point.
export YARN_ENABLE_IMMUTABLE_INSTALLS=false"#;

/// Uses Yarn 4 through Corepack, as package.json pins it. The update tool passes `fresh` to remove node_modules and
/// yarn.lock first so every package is resolved again.
pub async fn yarn_install(server: &dyn Server, directory: &str, log: Log<'_>, fresh: bool) -> Result<()> {
    let body = fill(YARN_INSTALL, &[("Environment", ENVIRONMENT), ("directory", directory), ("fresh", if fresh { FRESH } else { "" })]);
    script::run(server, &script::with_prelude(&body), log, &format!("yarn install in {directory} failed"), None).await
}

const CONFIGURE: &str = r#"{{Environment}}
# The emulator's timezone (config.ini) for timezone.settings in a new renderer-config.json.
tz=$(grep -oE 'serverTimezone=[^&[:space:]]+' {{EmulatorDirectory}}/config.ini 2>/dev/null | head -1 | cut -d= -f2)
echo "> {{ConfigurationDirectory}}: {{fileList}}"
SITE_URL='{{Url}}' CLIENT_URL='{{ClientUrl}}' CLIENT_PATH='{{ClientPath}}' ATOM='{{atom}}' LOGIN_SCREEN='{{loginScreen}}' SOCKET_URL='{{SocketUrl}}' CONFIG_DIR='{{ConfigurationDirectory}}' GAMEDATA_DIR='{{Gamedata}}' \
  FILES='{{files}}' TIMEZONE="$tz" node - <<'JS'
{{ConfigureScript}}
JS
echo "> {{Octane}}/package.json: {{base}}"
cd {{Octane}}
{{PackageBaseCommand}}
grep -E '"(start|build)": ' package.json | sed 's/^ */  /'
# The emulator, in group www-data, writes custom badges to c_images/album1584 and edits config/*.json.
# Default ACLs keep files copied in later, by the download tool or WinSCP, writable for it too.
command -v setfacl >/dev/null 2>&1 || { echo "> Installing acl (setfacl)"; apt_update >/dev/null; apt-get install -y acl >/dev/null; }
cat > {{PermissionsScript}} <<'PERMS'
{{PermissionsScriptContent}}
PERMS
chmod 755 {{PermissionsScript}}
echo "> {{PermissionsScript}}"
{{PermissionsScript}}

# A welcome message for the login screen news (login.news.url). Existing news is never overwritten.
install -d -m 2775 -o www-data -g www-data {{Gamedata}}/news
if [ -f {{Gamedata}}/news/news.json ]; then
  echo "> {{Gamedata}}/news/news.json: already there, kept"
else
  cat > {{Gamedata}}/news/news.json <<'NEWS'
{{WelcomeNews}}
NEWS
  chown www-data:www-data {{Gamedata}}/news/news.json
  chmod 664 {{Gamedata}}/news/news.json
  echo "> {{Gamedata}}/news/news.json: welcome message created"
fi"#;

/// Creates missing .json files from .example and sets the URL keys, backing up existing files first. Also creates
/// the asset folders they reference. `restore_login_screen`: after removing a CMS, Octane's own login screen on again.
pub async fn configure(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>, restore_login_screen: bool) -> Result<()> {
    let file_list = CONFIGURATION_FILES.iter().map(|f| format!("{f}.json")).collect::<Vec<_>>().join(", ");
    let base = if site.has_cms() {
        format!("vite --base={} (Octane on {})", site.client_path(), site.client_path())
    } else {
        "vite without --base (Octane on /)".to_string()
    };
    let text = fill(
        CONFIGURE,
        &[
            ("Environment", ENVIRONMENT),
            ("EmulatorDirectory", &crate::emulator::directory()),
            ("ConfigurationDirectory", &configuration_directory()),
            ("fileList", &file_list),
            ("Url", &site.url()),
            ("ClientUrl", &site.client_url()),
            ("ClientPath", site.client_path()),
            ("atom", if site.has_cms() { "yes" } else { "" }),
            ("loginScreen", if restore_login_screen && !site.has_cms() { "yes" } else { "" }),
            ("SocketUrl", &socket_url(site)),
            ("Gamedata", &gamedata()),
            ("files", &CONFIGURATION_FILES.join(" ")),
            ("Octane", &octane()),
            ("base", &base),
            ("PermissionsScript", PERMISSIONS_SCRIPT),
        ],
    );
    let body = fill(
        &text,
        &[
            ("ConfigureScript", CONFIGURE_SCRIPT),
            ("PackageBaseCommand", &package_base_command(site)),
            ("PermissionsScriptContent", &permissions_script_content()),
            ("WelcomeNews", &welcome_news(site)),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Writing the Octane configuration failed", None).await
}

const PERMISSIONS_SCRIPT_CONTENT: &str = r#"#!/bin/sh
# Read/write access to {{Gamedata}} for the www-data group (written by All-in-1 Manager).
# The emulator (a member of www-data) writes custom badges to c_images/album1584 and edits config/*.json;
# NGINX reads everything. The default ACLs give files and folders created later the same rights, also when
# they are copied in as root or another user. Run this again when an upload changed the permissions.
set -e
d={{Gamedata}}
[ -d "$d" ] || exit 0
chgrp -R www-data "$d"
find "$d" -type d -exec chmod 2775 {} +
find "$d" -type f -exec chmod ug+rw,o+r {} +
setfacl -R -m g:www-data:rwX -m d:g:www-data:rwX -m d:u::rwX -m d:o::rX "$d"
echo "  $d: group www-data can read and write (also new files: default ACL), others can read""#;

/// Restores www-data read/write access to gamedata; [`PERMISSIONS_SCRIPT`] holds it. Run it again after copying
/// assets in.
pub fn permissions_script_content() -> String {
    fill(PERMISSIONS_SCRIPT_CONTENT, &[("Gamedata", &gamedata())])
}

/// Contents of /gamedata/news/news.json.
pub fn welcome_news(site: &WebAccessSettings) -> String {
    WELCOME_NEWS_TEMPLATE.replace("@ADDRESS@", &site.address)
}

const WELCOME_NEWS_TEMPLATE: &str = r#"{
    "news": [
        {
            "id": 1,
            "title": "Welcome to @ADDRESS@!",
            "body": "The doors are open! Octane is the brand-new client of this hotel: create your avatar, decorate your own room, meet new friends and explore everything the hotel has to offer. Grab a drink in the lobby, say hello and make yourself at home. Have fun, and see you in the hotel!",
            "image": "",
            "link": "",
            "linkText": "Read more"
        }
    ]
}"#;

const CONFIGURE_SCRIPT: &str = r#"const fs = require('fs');
const path = require('path');
const { SITE_URL: site, CLIENT_URL: client, CLIENT_PATH: clientPath, ATOM: atom, LOGIN_SCREEN: loginScreen, SOCKET_URL: socket, CONFIG_DIR: dir, GAMEDATA_DIR: gamedata, TIMEZONE: timezone } = process.env;

// Always set: the hotel's URLs. Only in a file created from the .example: the folder layout and modes.
const urls = {
    'renderer-config': {
        'socket.url': socket,
        'api.url': site,
        'asset.url': `${site}/gamedata`,
        'image.library.url': `${site}/gamedata/c_images/`,
        'hof.furni.url': site,
        'furni.asset.icon.url': `${site}/gamedata/icons/%libname%%param%_icon.png`,
        'badge.asset.group.url': `${site}/gamedata/c_images/groupbadge/badge/%badgedata%`,
        'badge.asset.grouparts.url': `${site}/gamedata/c_images/groupbadge/badgeparts/badgepart_%part%.png`,
        'group.badge.url': '%imagerdata%',
        // Octane's own public/habbicons, so under /client/ with AtomCMS.
        'habbicons.asset.root': `${clientPath}habbicons`,
        // With a CMS the CMS logs in and opens the client with an SSO ticket. Without one again (the CMS was
        // removed), Octane's own login screen comes back.
        ...(atom ? { 'login.screen.enabled': false } : loginScreen ? { 'login.screen.enabled': true } : {})
    },
    'ui-config': {
        'camera.url': `${site}/gamedata/camera`,
        'thumbnails.url': `${site}/gamedata/camera/thumb/%thumbnail%.png`,
        'camera.render.mode': 'upload'
    },
    'client-mode': {
        'apiBaseUrl': site,
        'plainConfigBaseUrl': `${client}configuration/`,
        'plainGamedataBaseUrl': `${site}/gamedata/`
    }
};
const defaults = {
    'renderer-config': {
        ...(timezone ? { 'timezone.settings': timezone } : {})
    },
    'client-mode': { 'distObfuscationEnabled': false, 'secureAssetsEnabled': false, 'secureApiEnabled': false }
};

// JSON, or JSONC (comments, trailing commas) when the file was edited that way.
const parse = (file, text) => {
    try { return JSON.parse(text); } catch { }
    const stripped = text
        .replace(/("(?:[^"\\]|\\.)*")|\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, (m, s) => s ?? '')
        .replace(/,(\s*[}\]])/g, '$1');
    try { return JSON.parse(stripped); } catch (e) { throw new Error(`${file} is not valid JSON(C): ${e.message}`); }
};

// The settings files follow their .example: keys a newer Octane (or you, in the .example) added come in with the
// example's value, in the example's order; keys only the .json has stay after them. A value already in the .json is
// never changed, and a list stays as it is. The data files (news, hotlooks, ...) are left alone: their entries
// are the hotel's own.
const FOLLOW_EXAMPLE = new Set(['renderer-config', 'ui-config', 'client-mode']);
const isObject = v => v !== null && typeof v === 'object' && !Array.isArray(v);
const followExample = (example, current, added, prefix = '') => {
    if (!isObject(example) || !isObject(current)) return current;
    const out = {};
    for (const [key, value] of Object.entries(example)) {
        if (Object.prototype.hasOwnProperty.call(current, key)) {
            out[key] = isObject(value) && isObject(current[key]) ? followExample(value, current[key], added, `${prefix}${key}.`) : current[key];
        } else {
            out[key] = value;
            added.push(`${prefix}${key}`);
        }
    }
    for (const [key, value] of Object.entries(current)) if (!(key in out)) out[key] = value;
    return out;
};

const configs = {};
for (const name of process.env.FILES.split(' ')) {
    const file = path.join(dir, `${name}.json`);
    const example = path.join(dir, `${name}.example`);
    let created = false, config;
    if (fs.existsSync(file)) config = parse(file, fs.readFileSync(file, 'utf8'));
    else if (fs.existsSync(example)) { config = parse(example, fs.readFileSync(example, 'utf8')); created = true; }
    else { console.log(`  ${name}.json: no ${name}.example in this Octane version, skipped`); continue; }

    const changes = [];
    let layout = false;
    if (!created && FOLLOW_EXAMPLE.has(name) && fs.existsSync(example)) {
        const added = [];
        const before = JSON.stringify(config);
        config = followExample(parse(example, fs.readFileSync(example, 'utf8')), config, added);
        for (const key of added) changes.push(`${key} added from ${name}.example`);
        layout = added.length === 0 && JSON.stringify(config) !== before;
    }
    for (const [key, value] of Object.entries({ ...(created ? defaults[name] : {}), ...urls[name] })) {
        if (config[key] !== value) { changes.push(`${key} = ${JSON.stringify(value)}`); config[key] = value; }
    }
    if (layout) changes.push(`keys put in the order of ${name}.example`);
    if (created || changes.length > 0) {
        if (!created) fs.copyFileSync(file, `${file}.bak-${new Date().toISOString().replace(/\D/g, '').slice(0, 14)}`);
        fs.writeFileSync(file, JSON.stringify(config, null, 4) + '\n');
    }
    console.log(`  ${name}.json: ${created ? `created from ${name}.example` : changes.length ? 'updated (old file kept as .bak-<time>)' : 'already up to date'}`);
    for (const change of changes) console.log(`    ${change}`);
    configs[name] = config;
}

// Every folder the asset URLs point to, e.g. ${asset.url}/furniture/%libname%.hab -> furniture.
const settings = { ...configs['renderer-config'], ...configs['ui-config'] };
const expand = (value, depth = 0) => depth > 8 ? value
    : value.replace(/\$\{([^}]+)\}/g, (m, key) => typeof settings[key] === 'string' ? expand(settings[key], depth + 1) : m);
const folders = new Set();
const visit = value => {
    if (Array.isArray(value)) return value.forEach(visit);
    if (value && typeof value === 'object') return Object.values(value).forEach(visit);
    if (typeof value !== 'string' || !value) return;
    let url = expand(value);
    for (const prefix of [`${site}/gamedata/`, '/gamedata/']) {
        if (!url.startsWith(prefix)) continue;
        const parts = url.slice(prefix.length).split(/[?#]/)[0].split('/').filter(Boolean);
        const cut = parts.findIndex(p => p.includes('%') || p.includes('${'));
        const kept = cut < 0 ? parts : parts.slice(0, cut);
        if (cut < 0 && kept.length && kept[kept.length - 1].includes('.')) kept.pop();
        if (kept.length) folders.add(kept.join('/'));
        break;
    }
};
visit(settings);
let made = 0;
for (const folder of [...folders].sort()) {
    const full = path.join(gamedata, folder);
    if (!full.startsWith(gamedata + '/')) continue;
    if (!fs.existsSync(full)) { fs.mkdirSync(full, { recursive: true }); made++; }
}
fs.mkdirSync(gamedata, { recursive: true });
console.log(`> ${gamedata}: ${folders.size} asset folders (${made} new): ${[...folders].sort().join(', ')}`);"#;

const BUILD: &str = r#"{{Environment}}
set -o pipefail
cat > {{Octane}}/build.sh <<'BUILD'
#!/bin/sh
# Builds the Octane client into dist/ (written by All-in-1 Manager). Run it after changing
# public/configuration or after a git pull of Octane or Octane-Renderer.
set -e
export PATH=/usr/sbin:/usr/bin:/sbin:/bin COREPACK_ENABLE_DOWNLOAD_PROMPT=0 CI=1
cd {{Renderer}} && yarn install
cd {{Octane}}
# Octane on {{ClientPath}}: keeps package.json's --base right after a git pull.
{{PackageBaseCommand}}
yarn install && yarn build
BUILD
chmod 755 {{Octane}}/build.sh
cd {{Octane}}
echo "> cd {{Octane}} && yarn build"
start=$(date +%s)
rc=$(mktemp)
(yarn build </dev/null 2>&1; echo $? > "$rc") | grep -vE '^dist/|^\s*$|computing gzip|transforming|rendering chunks' || true
code=$(cat "$rc"); rm -f "$rc"
[ "$code" = 0 ] || fail "yarn build stopped with exit code $code (see above)."
[ -f {{Dist}}/index.html ] || fail "yarn build did not create {{Dist}}/index.html (see above)."
echo "  dist/ is $(du -sh {{Dist}} | cut -f1), built in $(( $(date +%s) - start )) seconds""#;

/// Writes build.sh for later rebuilds, then builds.
pub async fn build(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let text = fill(
        BUILD,
        &[
            ("Environment", ENVIRONMENT),
            ("Octane", &octane()),
            ("Renderer", &renderer()),
            ("ClientPath", site.client_path()),
            ("Dist", &dist()),
        ],
    );
    let body = fill(&text, &[("PackageBaseCommand", &package_base_command(site))]);
    script::run(server, &script::with_prelude(&body), log, "Building Octane failed", None).await
}

const PUBLISH: &str = r#"{{WriteScript}}

url="{{Scheme}}://{{name}}"
c() { curl -s --noproxy '*' -k --max-time 10 --resolve "{{name}}:{{port}}:127.0.0.1" "$@"; }
client="$url{{ClientPath}}"
echo "> $client (Octane)"
# The NGINX reload above finishes in the background, so give the new workers a moment.
for i in 1 2 3 4 5 6 7 8 9 10; do
  page=$(c "$client" || true)
  echo "$page" | grep -q 'configuration/bootstrap.js' && break
  sleep 1
done
echo "$page" | grep -q 'configuration/bootstrap.js' || fail "$client does not serve Octane's dist/index.html."
echo "  index.html OK"
echo "> ${client}configuration/renderer-config.json"
socket=$(c "${client}configuration/renderer-config.json" | grep -oE '"socket.url": *"[^"]*"' || true)
[ -n "$socket" ] || fail "${client}configuration/renderer-config.json is not served."
echo "  $socket"
echo "> $url/gamedata/"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/gamedata/config/" || true) (403/404 until the assets are copied in)""#;

/// Points the hotel site at dist/ and checks it through Traefik.
pub async fn publish(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let name = if site.is_ip_address() { "127.0.0.1".to_string() } else { site.address.clone() };
    let text = fill(
        PUBLISH,
        &[
            ("Scheme", site.scheme()),
            ("name", &name),
            ("port", if site.https { "443" } else { "80" }),
            ("ClientPath", site.client_path()),
        ],
    );
    let body = fill(&text, &[("WriteScript", &crate::hotel_site::write_script(site, Some(&dist())))]);
    script::run(server, &script::with_prelude(&body), log, "Publishing Octane failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_state() {
        let site = WebAccessSettings::new("hotel.example.com", true, false);
        assert_eq!(socket_url(&site), "wss://hotel.example.com/ws");
        assert_eq!(socket_url(&WebAccessSettings::new("2001:db8::1", false, false)), "ws://[2001:db8::1]/ws");
        assert_eq!(package_base_command(&site), r#"sed -i -E -e 's#("(start|build)": "vite) --base=[^ "]+#\1#' package.json"#);
        let atom = WebAccessSettings { atom_cms: true, ..site.clone() };
        assert!(package_base_command(&atom).contains(r#" -e 's#("(start|build)": "vite)( |")#\1 --base=/client/\3#' package.json"#));

        let values = ScriptValues::parse(
            "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\nconf_CLOUDFLARE=no\nconf_CMS=none\noctane=yes\nrenderer=yes\nnode=v26.1.0\n\
             socket=wss://hotel.example.com/ws\nbuilt=yes\nsite=yes\nconfigbase=https://hotel.example.com/configuration/\ngamedata=yes\n\
             uicamera=https://hotel.example.com/gamedata/camera\nemuimager=\n",
        );
        let state = parse_state(&values);
        assert_eq!(state.site, Some(site));
        assert_eq!(state.node_version.as_deref(), Some("v26.1.0"));
        assert!(state.layout_configured());
        assert_eq!(state.expected_socket_url(), state.configured_socket_url);
        assert_eq!(state.emulator_imager, None);
        let empty = parse_state(&ScriptValues::parse(""));
        assert!(!empty.is_complete() && !empty.camera_configured() && !empty.layout_configured());
    }
}
