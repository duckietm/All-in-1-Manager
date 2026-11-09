//! CrowdSec in Docker with the Traefik bouncer plugin. (CrowdSecInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

/// CrowdSec's folder (Settings → Server layout).
pub fn directory() -> &'static str {
    &aio_core::ServerLayout::current().crowdsec
}
pub const CONTAINER: &str = "crowdsec";
pub const IMAGE: &str = "crowdsecurity/crowdsec:latest";
pub const CONSOLE_URL: &str = "https://app.crowdsec.net";
pub const BOUNCER_NAME: &str = "traefik";
pub const ALLOWLIST_NAME: &str = "all-in-1";
pub const MIDDLEWARE: &str = "crowdsec";
pub const PLUGIN: &str = "crowdsec";
pub const PLUGIN_MODULE: &str = "github.com/maxlerebourg/crowdsec-bouncer-traefik-plugin";
pub const PLUGIN_VERSION: &str = "v1.8.1";
/// Traefik's access log folder and file (Settings → Server layout).
pub fn access_log_directory() -> &'static str {
    &aio_core::ServerLayout::current().traefik_logs
}

pub fn access_log() -> &'static str {
    static LOG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LOG.get_or_init(|| format!("{}/access.log", access_log_directory()))
}
pub const COLLECTIONS: &str = "crowdsecurity/traefik crowdsecurity/http-cve crowdsecurity/whitelist-good-actors";
/// The version that runs: "v1.8.1".
pub const VERSION_COMMAND: &str = "docker exec crowdsec cscli version 2>&1 | awk '/^version:/ {print $2}' | cut -d- -f1";

pub fn middleware_file() -> String {
    format!("{}/dynamic/crowdsec.yml", crate::web_access::traefik_directory())
}

// ---------------------------------------------------------------------------------------------------------------
//
// CrowdSec (crowdsec.net) in Docker next to the hotel's Traefik: the security engine reads Traefik's access log, bans
// addresses that attack the site (scans, probing, known CVEs, brute force) and shares them with the community; its
// blocklist comes back. Traefik's bouncer plugin (github.com/maxlerebourg/crowdsec-bouncer-traefik-plugin) answers 403
// to banned addresses on every route. Behind Cloudflare the plugin takes the visitor from X-Forwarded-For past
// Cloudflare's ranges, as Traefik does. The engine is enrolled in the CrowdSec console (app.crowdsec.net) with the key
// from there.

use crate::fail;
use crate::script::{self, fill, ScriptValues, PRELUDE};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::{self, WebAccessSettings};
use crate::{fail2ban, server_tools};

/// What the CrowdSec addon asks: the console's enrollment key (empty on a rerun keeps the enrollment), a name, the
/// whitelist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrowdSecSettings {
    pub enroll_key: String,
    pub instance_name: String,
    pub whitelist: Vec<String>,
}

/// What the server has for CrowdSec before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrowdSecState {
    pub site: Option<WebAccessSettings>,
    pub docker_running: bool,
    pub traefik_running: bool,
    pub installed: bool,
    pub running: bool,
    pub bouncer_configured: bool,
    pub enrolled: bool,
    pub version: Option<String>,
    pub instance_name: Option<String>,
    pub port: i64,
    pub whitelist: Vec<String>,
    pub banned: Vec<String>,
    pub dns_addresses: Option<String>,
    pub dns_behind_cloudflare: Option<bool>,
    pub client_ip: Option<String>,
}

impl CrowdSecState {
    pub fn is_complete(&self) -> bool {
        self.installed && self.running && self.bouncer_configured && self.enrolled
    }
}

/// The bouncer pulled the ban list from CrowdSec, and CrowdSec reads Traefik's access log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrowdSecResult {
    pub bouncer_connected: bool,
    pub log_read: bool,
}

// The hotel's own files: a room loads hundreds of furniture files at once, which would look like a crawler.
const ASSET_PATHS: [&str; 7] = ["/gamedata/", "/client/", "/custom/", "/swf/", "/nitro-assets/", "/_next/static/", "/assets/"];

// Go template braces: filled in for {{GoRunning}}.
const GO_RUNNING: &str = "{{.State.Running}}";

/// `^[A-Za-z0-9]{n,m}$` as .NET matches it: `$` also matches before a final "\n".
fn matches_pattern(text: &str, min: usize, max: usize, allowed: impl Fn(char) -> bool) -> bool {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let count = text.chars().count();
    (min..=max).contains(&count) && text.chars().all(allowed)
}

/// The enrollment key from a paste: the key alone, or the whole command the console shows
/// ("sudo cscli console enroll -e context cm1abc..."). None when there is none.
pub fn key_from(text: &str) -> Option<String> {
    let word = text.split([' ', '\t', '\r', '\n']).filter(|w| !w.is_empty()).rfind(|w| !w.starts_with('-'))?;
    let key = word.trim_matches(['"', '\'']);
    matches_pattern(key, 16, 64, |c| c.is_ascii_alphanumeric()).then(|| key.to_string())
}

pub fn key_problem(text: &str, enrolled: bool) -> Option<String> {
    if text.trim().is_empty() {
        if enrolled {
            None
        } else {
            Some("Paste the enrollment key from the CrowdSec console (see 1 and 2 above): it is required.".into())
        }
    } else if key_from(text).is_none() {
        Some("That is not an enrollment key: copy the key (or the whole cscli console enroll command) from app.crowdsec.net.".into())
    } else {
        None
    }
}

pub fn name_problem(name: &str) -> Option<String> {
    if matches_pattern(name, 1, 64, |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        None
    } else {
        Some("The name in the console: letters, digits, '.', '-' and '_', at most 64.".into())
    }
}

const INSPECT: &str = r#"[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
echo "docker=$(docker info >/dev/null 2>&1 && echo yes)"
echo "traefik=$(docker inspect -f '{{GoRunning}}' traefik 2>/dev/null)"
c={{Directory}}
[ -f $c/.env ] && sed -n 's/^\(LAPI_PORT\|ENROLLED\|INSTANCE\|WHITELIST\)=/cs_\1=/p' $c/.env
echo "installed=$([ -f $c/compose.yaml ] && echo yes)"
echo "bouncer=$([ -f {{MiddlewareFile}} ] && echo yes)"
echo "running=$(docker inspect -f '{{GoRunning}}' {{Container}} 2>/dev/null)"
if [ "$(docker inspect -f '{{GoRunning}}' {{Container}} 2>/dev/null)" = true ]; then
  echo "version=$(docker exec {{Container}} cscli version 2>&1 | awk '/^version:/ {print $2}' | cut -d- -f1)"
  # Local bans only (not the community blocklist): what this server decided, or someone by hand.
  echo "banned=$(docker exec {{Container}} cscli decisions list -o json 2>/dev/null | python3 -c 'import json, sys
d = json.load(sys.stdin) or []
print(" ".join(sorted({x["value"] for a in d for x in (a.get("decisions") or []) if x.get("scope", "").lower() in ("ip", "range")})))' 2>/dev/null)"
fi
# Is the hotel's domain behind Cloudflare's proxy (its address one of Cloudflare's)?
a=$(sed -n 's/^ADDRESS=//p' {{TraefikDirectory}}/hotel.env 2>/dev/null)
if [ -n "$a" ] && ! echo "$a" | grep -qE '^[0-9.]+$|:'; then
  ips=$(getent ahostsv4 "$a" 2>/dev/null | awk '{print $1}' | sort -u | paste -sd' ' -)
  echo "dns=$ips"
  if [ -n "$ips" ]; then
    ranges=$(load_cloudflare_ranges >/dev/null 2>&1 && printf '%s' "$ranges") || ranges=
    [ -n "$ranges" ] && echo "dnscf=$(IPS="$ips" RANGES="$ranges" python3 -c 'import ipaddress, os
nets = [ipaddress.ip_network(r) for r in os.environ["RANGES"].split()]
print("yes" if all(any(ipaddress.ip_address(i) in n for n in nets) for i in os.environ["IPS"].split()) else "no")' 2>/dev/null)"
  fi
fi"#;

pub async fn inspect(server: &dyn Server) -> Result<CrowdSecState> {
    let body = fill(
        INSPECT,
        &[
            ("TraefikDirectory", web_access::traefik_directory()),
            ("Directory", directory()),
            ("MiddlewareFile", &middleware_file()),
            ("Container", CONTAINER),
            ("GoRunning", GO_RUNNING),
        ],
    );
    // Without set -e: a missing file or a stopped container is an answer, not a failure.
    let script = PRELUDE.replace("set -e\n", "") + &body;
    let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
    let client_ip = fail2ban::client_ip(server).await;
    Ok(state_from(&values, client_ip))
}

fn state_from(values: &ScriptValues, client_ip: Option<String>) -> CrowdSecState {
    let words = |key: &str| values.or_empty(key).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>();
    CrowdSecState {
        site: WebAccessSettings::from_hotel_env(values),
        docker_running: values.yes("docker"),
        traefik_running: values.get("traefik") == Some("true"),
        installed: values.yes("installed"),
        running: values.get("running") == Some("true"),
        bouncer_configured: values.yes("bouncer"),
        enrolled: values.yes("cs_ENROLLED"),
        version: values.string("version"),
        instance_name: values.string("cs_INSTANCE"),
        port: values.int("cs_LAPI_PORT"),
        whitelist: words("cs_WHITELIST"),
        banned: words("banned"),
        dns_addresses: values.string("dns"),
        dns_behind_cloudflare: values.get("dnscf").map(|cf| cf == "yes"),
        client_ip,
    }
}

/// Why CrowdSec cannot be set up (yet), or None. Behind Cloudflare without Traefik trusting Cloudflare, every visitor
/// would look like one of Cloudflare's addresses: a ban would shut out everyone behind that address.
pub fn problem(state: &CrowdSecState) -> Option<String> {
    let Some(site) = &state.site else {
        return Some("Run the Web access step first: CrowdSec works with its Docker and Traefik.".into());
    };
    if !state.docker_running {
        Some("Docker does not run: run the Web access step first.".into())
    } else if !state.traefik_running {
        Some("Traefik does not run: run the Web access step first.".into())
    } else if state.dns_behind_cloudflare == Some(true) && !site.cloudflare {
        Some(format!(
            "{} points to Cloudflare (proxy on), but Web access was set up without Cloudflare. Traefik then sees Cloudflare's addresses \
instead of the visitors', and CrowdSec would ban Cloudflare (everyone). Run Install New Server → Web access again and choose Yes for Cloudflare, then this addon.",
            site.address
        ))
    } else {
        None
    }
}

const INSTALL_ENGINE: &str = r#"set -o pipefail
c={{Directory}}
docker info >/dev/null 2>&1 || fail "Docker does not run. Run the Web access step first."
install -d -m 755 "$c" "$c/config" "$c/data" "$c/parsers" {{AccessLogDirectory}}
touch {{AccessLog}}

# The port CrowdSec's API listens on for the bouncer (127.0.0.1 only): kept, or the first free one from 8080.
port=$(sed -n 's/^LAPI_PORT=//p' "$c/.env" 2>/dev/null) || port=
if [ -z "$port" ]; then
  for p in $(seq 8080 8099); do ss -Hltn "sport = :$p" | grep -q . || { port=$p; break; }; done
  [ -n "$port" ] || fail "Ports 8080 to 8099 are all in use: free one for CrowdSec's API."
fi
# The bouncer's key: made once, at random; CrowdSec registers it at every start (BOUNCER_KEY_traefik).
key=$(sed -n 's/^BOUNCER_KEY=//p' "$c/.env" 2>/dev/null) || key=
[ -n "$key" ] || key=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
enrolled=$(sed -n 's/^ENROLLED=//p' "$c/.env" 2>/dev/null) || enrolled=
umask 077
cat > "$c/.env" <<EOF
# CrowdSec (written by All-in-1 Manager, Addons → CrowdSec).
LAPI_PORT=$port
BOUNCER_KEY=$key
INSTANCE={{InstanceName}}
WHITELIST={{Whitelist}}
ENROLLED=$enrolled
EOF
umask 022
echo "> $c/.env: CrowdSec's API on 127.0.0.1:$port"

cat > "$c/acquis.yaml" <<'EOF'
# What CrowdSec reads (written by All-in-1 Manager): Traefik's access log, every request to the hotel.
filenames:
  - {{AccessLog}}
labels:
  type: traefik
EOF

cat > "$c/parsers/hotel-assets.yaml" <<'EOF'
# The hotel's own files are not suspicious (written by All-in-1 Manager): a room loads hundreds of furniture
# files at once, and missing ones answer 404. Without this, players would look like crawlers or scanners.
name: all-in-1/hotel-assets
description: "Requests for the hotel's assets (gamedata, client) are never counted"
whitelist:
  reason: "hotel assets"
  expression:
    - "{{assets}}"
EOF

cat > "$c/compose.yaml" <<'EOF'
# CrowdSec for the hotel (written by All-in-1 Manager). Update: docker compose pull && docker compose up -d
services:
  crowdsec:
    image: {{Image}}
    container_name: {{Container}}
    restart: unless-stopped
    environment:
      COLLECTIONS: "{{Collections}}"
      BOUNCER_KEY_{{BouncerName}}: ${BOUNCER_KEY}
    ports:
      - "127.0.0.1:${LAPI_PORT}:8080"
    volumes:
      - ./acquis.yaml:/etc/crowdsec/acquis.yaml:ro
      - ./parsers/hotel-assets.yaml:/etc/crowdsec/parsers/s02-enrich/all-in-1-hotel-assets.yaml:ro
      - ./config:/etc/crowdsec
      - ./data:/var/lib/crowdsec/data
      - {{AccessLogDirectory}}:{{AccessLogDirectory}}:ro
EOF
echo "> $c/compose.yaml ({{Image}}): collections {{Collections}}"

# Traefik's access log grows with every request: daily, a week kept.
cat > /etc/logrotate.d/traefik-access <<'EOF'
{{AccessLog}} {
  daily
  rotate 7
  compress
  delaycompress
  missingok
  notifempty
  copytruncate
}
EOF

echo "> docker compose pull"
pulled=
for i in 1 2 3; do
  if compose_in "$c" pull -q 2>"$c/pull.log"; then pulled=yes; break; fi
  tail -1 "$c/pull.log" | sed 's/^/  /'
  [ $i -lt 3 ] && { echo "  Trying again in $((i * 20)) seconds ..."; sleep $((i * 20)); }
done
rm -f "$c/pull.log"
if [ -z "$pulled" ]; then
  docker image inspect {{Image}} >/dev/null 2>&1 || fail "Could not download the CrowdSec image from Docker Hub (see above). Try again later."
  echo "  Docker Hub did not answer: using the image that is already on this server"
fi
echo "> docker compose up -d"
compose_in "$c" up -d 2>&1 | grep -v '^ *$'
# The first start downloads the collections from CrowdSec's hub: up to a few minutes.
echo "  Waiting for CrowdSec's API ..."
ok=
for i in $(seq 1 90); do
  [ "$(docker inspect -f '{{GoRunning}}' {{Container}} 2>/dev/null)" = true ] || { [ $i -gt 10 ] && break; }
  docker exec {{Container}} cscli lapi status >/dev/null 2>&1 && { ok=yes; break; }
  sleep 2
done
[ -n "$ok" ] || { docker logs --tail 25 {{Container}} 2>&1 | sed 's/^/  /'; fail "CrowdSec does not start (see its log above)."; }
echo "  CrowdSec $(docker exec {{Container}} cscli version 2>&1 | awk '/^version:/ {print $2}') runs"
docker exec {{Container}} cscli collections list -o raw 2>/dev/null | awk -F, 'NR > 1 {print "  collection " $1 " (" $2 ")"}'
docker exec {{Container}} cscli bouncers list -o raw 2>/dev/null | grep -q '^{{BouncerName}},' || fail "CrowdSec did not register the bouncer key (see: docker logs crowdsec)."

{{AllowlistScript}}"#;

/// The engine: compose file, acquisition of Traefik's log, the hotel assets filter, start, and the whitelist.
pub async fn install_engine(server: &dyn Server, settings: &CrowdSecSettings, log: Log<'_>) -> Result<()> {
    let assets = ASSET_PATHS.iter().map(|p| format!("evt.Meta.http_path startsWith '{p}'")).collect::<Vec<_>>().join(" || ");
    let body = fill(
        INSTALL_ENGINE,
        &[
            ("Directory", directory()),
            ("AccessLogDirectory", access_log_directory()),
            ("AccessLog", access_log()),
            ("InstanceName", &settings.instance_name),
            ("Whitelist", &settings.whitelist.join(" ")),
            ("assets", &assets),
            ("Image", IMAGE),
            ("Container", CONTAINER),
            ("Collections", COLLECTIONS),
            ("BouncerName", BOUNCER_NAME),
            ("AllowlistScript", &allowlist_script(&settings.whitelist)),
            ("GoRunning", GO_RUNNING),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Installing CrowdSec failed", None).await
}

const ALLOWLIST: &str = r#"cs() { docker exec {{Container}} cscli "$@"; }
cs allowlists delete {{AllowlistName}} >/dev/null 2>&1 || true
cs allowlists create {{AllowlistName}} -d "Never banned (All-in-1 Manager)" >/dev/null || fail "Could not make CrowdSec's allowlist."
for e in {{entries}}; do
  cs allowlists add {{AllowlistName}} "$e" >/dev/null || fail "CrowdSec does not take $e for the allowlist."
  case "$e" in */*) cs decisions delete -r "$e" >/dev/null 2>&1 ;; *) cs decisions delete -i "$e" >/dev/null 2>&1 ;; esac
done
echo "  whitelist: {{shown}}""#;

// The whitelist as CrowdSec's allowlist: never banned, also not by the community blocklist. Bans of whitelisted
// addresses are lifted.
fn allowlist_script(whitelist: &[String]) -> String {
    let entries = whitelist.iter().map(|e| shell_quote(e)).collect::<Vec<_>>().join(" ");
    let shown = if whitelist.is_empty() { "(empty)".to_string() } else { whitelist.join(" ") };
    fill(ALLOWLIST, &[("Container", CONTAINER), ("AllowlistName", ALLOWLIST_NAME), ("entries", &entries), ("shown", &shown)])
}

const ENROLL: &str = r#"c={{Directory}}
echo "> cscli console enroll --name {{name}}"
out=$(docker exec {{Container}} cscli console enroll --overwrite --name {{quotedName}} {{clean}} 2>&1) && status=0 || status=$?
echo "$out" | sed 's/^/  /' | grep -v '^ *$'
if [ $status -ne 0 ] || echo "$out" | grep -qiE 'level=(error|fatal)|could not enroll|not valid|invalid'; then
  why=$(echo "$out" | grep -iE 'error|fatal|not valid|invalid' | tail -1 | sed -E 's/.*(Error|error|fatal)[:=] *//; s/^"//; s/"$//')
  fail "The CrowdSec console did not take the key (${why:-see above}). Copy it again from app.crowdsec.net (Security Engines → Enroll): a key can expire."
fi
sed -i 's/^ENROLLED=.*/ENROLLED=yes/' "$c/.env"
# Restarting CrowdSec makes it send its enrollment request to the console now.
docker restart {{Container}} >/dev/null
for i in $(seq 1 30); do docker exec {{Container}} cscli lapi status >/dev/null 2>&1 && break; sleep 2; done
echo "  Enrolled: accept \"{{name}}\" on {{ConsoleUrl}}""#;

/// Enrolls the engine in the CrowdSec console. The owner of the account then accepts it on app.crowdsec.net.
pub async fn enroll(server: &dyn Server, key: &str, name: &str, log: Log<'_>) -> Result<()> {
    let Some(clean) = key_from(key) else {
        fail!("That is not an enrollment key.");
    };
    let body = fill(
        ENROLL,
        &[
            ("Directory", directory()),
            ("Container", CONTAINER),
            ("quotedName", &shell_quote(name)),
            ("clean", &clean),
            ("ConsoleUrl", CONSOLE_URL),
            ("name", name),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Enrolling in the CrowdSec console failed", None).await
}

const MIDDLEWARE_SCRIPT: &str = r#"key=$(sed -n 's/^BOUNCER_KEY=//p' {{Directory}}/.env)
[ -n "$key" ] || fail "CrowdSec's bouncer key is missing in {{Directory}}/.env: run this addon again."
cfline=
if [ {{cloudflare}} = yes ]; then
  load_cloudflare_ranges
  cfline="          forwardedHeadersTrustedIPs: [$(echo "$ranges" | sed 's/.*/"&"/' | paste -sd, -)]"
fi
umask 077
cat > {{MiddlewareFile}} <<EOF
# CrowdSec's bouncer on every route (written by All-in-1 Manager, Addons → CrowdSec; Tools → CrowdSec).
http:
  middlewares:
    {{Middleware}}:
      plugin:
        {{Plugin}}:
          enabled: true
          logLevel: INFO
          crowdsecMode: stream
          updateIntervalSeconds: 60
          updateMaxFailure: -1
          streamStartupBlock: false
          crowdsecLapiScheme: http
          crowdsecLapiHost: "127.0.0.1:{{port}}"
          crowdsecLapiKey: "$key"
          remediationHeadersCustomName: X-Crowdsec-Remediation
          clientTrustedIPs: [{{trusted}}]
$cfline
EOF
umask 022
sed -i '/^$/d' {{MiddlewareFile}}
echo "> {{MiddlewareFile}}{{note}}""#;

// The bouncer middleware: Traefik's dynamic configuration, read again without a restart. Stream mode keeps the ban
// list in memory (no question to CrowdSec per request). When CrowdSec cannot be reached the hotel stays reachable
// (updateMaxFailure -1), and Traefik does not wait for CrowdSec when it starts (streamStartupBlock false).
fn middleware_script(port: i64, whitelist: &[String], cloudflare: bool) -> String {
    let trusted = std::iter::once("127.0.0.1/32")
        .chain(whitelist.iter().map(String::as_str))
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fill(
        MIDDLEWARE_SCRIPT,
        &[
            ("Directory", directory()),
            ("cloudflare", if cloudflare { "yes" } else { "no" }),
            ("MiddlewareFile", &middleware_file()),
            ("Middleware", MIDDLEWARE),
            ("Plugin", PLUGIN),
            ("port", &port.to_string()),
            ("trusted", &trusted),
            ("note", if cloudflare { ": visitors from X-Forwarded-For past Cloudflare's ranges" } else { "" }),
        ],
    )
}

const PROBE: &str = r#"probe() {
  if [ {{https}} = yes ]; then
    curl -sk -D - -o /dev/null --max-time 15 --resolve '{{Address}}:443:127.0.0.1' 'https://{{Address}}/' 2>/dev/null
  else
    curl -s -D - -o /dev/null --max-time 15 -H 'Host: {{Address}}' http://127.0.0.1/ 2>/dev/null
  fi | tr -d '\r' | awk 'NR == 1 {c = $2} tolower($1) == "server:" {s = $2} END {print (c ? c : "000") (s ? " " s : "")}'
}"#;

// The HTTP code and Server header of the hotel's start page through local Traefik ("200 nginx"): a broken route is
// Traefik's own 404, without NGINX's header.
fn probe_function(site: &WebAccessSettings) -> String {
    fill(PROBE, &[("https", if site.https { "yes" } else { "no" }), ("Address", &site.address)])
}

const CONFIGURE_TRAEFIK: &str = r#"t={{TraefikDirectory}}
[ -f "$t/traefik.yml" ] || fail "Traefik is not set up in $t. Run the Web access step first."
port=$(sed -n 's/^LAPI_PORT=//p' {{Directory}}/.env)
{{probe}}
before=$(probe)
echo "> The hotel through Traefik now: HTTP $before"
for f in traefik.yml compose.yaml; do cp -p "$t/$f" "$t/$f.before-crowdsec"; done
had_middleware=$([ -f {{MiddlewareFile}} ] && echo yes || true)
[ -n "$had_middleware" ] && cp -p {{MiddlewareFile}} "$t/crowdsec.yml.before"

{{middleware}}
{{staticConfig}}
{{compose}}

restore() {
  echo "  Putting the previous Traefik configuration back ..."
  for f in traefik.yml compose.yaml; do mv -f "$t/$f.before-crowdsec" "$t/$f"; done
  if [ -n "$had_middleware" ]; then mv -f "$t/crowdsec.yml.before" {{MiddlewareFile}}; else rm -f {{MiddlewareFile}}; fi
  compose_in "$t" up -d --force-recreate >/dev/null 2>&1
}
# From here on a failure puts the previous configuration back first.
fail() { restore; echo "ERROR: $* The previous Traefik configuration is back."; exit 1; }
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "> Restarting Traefik with the bouncer (players are disconnected for a moment)"
compose_in "$t" up -d --force-recreate 2>&1 | grep -v '^ *$'
{{waitForTraefik}}
# Traefik downloads the plugin (plugins.traefik.io) when it starts.
sleep 5
after=
for i in $(seq 1 15); do after=$(probe); [ "$after" = "$before" ] && break; sleep 2; done
problem=$(docker logs --since "$started" traefik 2>&1 | grep -iE 'plugin|crowdsec' | grep -iE 'error|fail|unable|disabled' | tail -3)
if [ -n "$problem" ] || [ "$after" != "$before" ]; then
  [ -n "$problem" ] && echo "$problem" | sed 's/^/  /'
  echo "$problem" | grep -q 'unable to download plugin' &&
    echo "  Traefik could not download the plugin from plugins.traefik.io: the server needs to reach it (internet, DNS, an outgoing firewall)."
  echo "  The hotel answers HTTP $after (before: $before)"
  fail "Traefik does not work with the CrowdSec plugin (see above), so the hotel runs as before."
fi
rm -f "$t/traefik.yml.before-crowdsec" "$t/compose.yaml.before-crowdsec" "$t/crowdsec.yml.before"
echo "  The hotel answers HTTP $after, as before: the bouncer is in front of every route""#;

/// Puts the bouncer in front of every route: the middleware, Traefik's static configuration with the plugin and the
/// access log, and a new Traefik container. When the site stops answering as before, the previous configuration
/// comes back.
pub async fn configure_traefik(server: &dyn Server, site: &WebAccessSettings, whitelist: &[String], log: Log<'_>) -> Result<()> {
    let middleware = middleware_script(0, whitelist, site.cloudflare).replace("127.0.0.1:0", "127.0.0.1:$port");
    // The texts of other modules go in last, so nothing in them is taken for a placeholder of this script.
    let body = fill(
        CONFIGURE_TRAEFIK,
        &[
            ("TraefikDirectory", web_access::traefik_directory()),
            ("Directory", directory()),
            ("MiddlewareFile", &middleware_file()),
            ("probe", &probe_function(site)),
            ("middleware", &middleware),
            ("waitForTraefik", &web_access::wait_for_traefik_script()),
            ("staticConfig", &web_access::static_config_script(site)),
            ("compose", &web_access::compose_script()),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Adding the CrowdSec bouncer to Traefik failed", None).await
}

const TEST: &str = r#"{{probe}}
probe >/dev/null
echo "> Waiting for the bouncer to fetch the ban list (once a minute) ..."
pulled=
for i in $(seq 1 45); do
  pulled=$(docker exec {{Container}} cscli bouncers list -o json 2>/dev/null | python3 -c 'import json, sys
for b in json.load(sys.stdin) or []:
    if b.get("name") == "{{BouncerName}}" and b.get("last_pull"): print(b["last_pull"])' 2>/dev/null)
  [ -n "$pulled" ] && break
  sleep 2
done
if [ -n "$pulled" ]; then echo "  The bouncer fetched the ban list at $pulled"; echo "BOUNCER=yes"
else echo "  The bouncer has not fetched the ban list yet: see Debug → Traefik for crowdsec lines"; fi
probe >/dev/null
sleep 3
# "│ file:/var/log/traefik/access.log │ 12 │ 12 │ - │ - │": the first number is the lines read.
read=$(docker exec {{Container}} cscli metrics show acquisition 2>/dev/null | grep 'access.log' | sed 's/.*access\.log//' | grep -oE '[0-9]+' | head -1)
echo "  CrowdSec read ${read:-0} line(s) of {{AccessLog}} so far"
[ "${read:-0}" -gt 0 ] 2>/dev/null && echo "LOGREAD=yes"
true"#;

/// Waits until the bouncer has pulled the ban list, and checks that CrowdSec reads Traefik's access log.
pub async fn test(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<CrowdSecResult> {
    let body = fill(
        TEST,
        &[("Container", CONTAINER), ("BouncerName", BOUNCER_NAME), ("AccessLog", access_log()), ("probe", &probe_function(site))],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Testing CrowdSec failed", &["BOUNCER", "LOGREAD"], None).await?;
    Ok(CrowdSecResult { bouncer_connected: values.yes("BOUNCER"), log_read: values.yes("LOGREAD") })
}

const SAVE_WHITELIST: &str = r#"[ "$(docker inspect -f '{{GoRunning}}' {{Container}} 2>/dev/null)" = true ] || fail "CrowdSec does not run: run Addons → CrowdSec first."
{{allowlist}}
sed -i 's#^WHITELIST=.*#WHITELIST={{whitelist}}#' {{Directory}}/.env
if [ -f {{MiddlewareFile}} ]; then
{{middleware}}
fi"#;

/// A new whitelist: CrowdSec's allowlist and the bouncer's trusted addresses (Traefik reads that without a restart).
pub async fn save_whitelist(server: &dyn Server, site: &WebAccessSettings, port: i64, whitelist: &[String], log: Log<'_>) -> Result<()> {
    let body = fill(
        SAVE_WHITELIST,
        &[
            ("Container", CONTAINER),
            ("whitelist", &whitelist.join(" ")),
            ("Directory", directory()),
            ("MiddlewareFile", &middleware_file()),
            ("allowlist", &allowlist_script(whitelist)),
            ("middleware", &middleware_script(port, whitelist, site.cloudflare)),
            ("GoRunning", GO_RUNNING),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Saving CrowdSec's whitelist failed", None).await
}

const UNBAN: &str = r#"echo "> cscli decisions delete {{flag}} {{value}}"
docker exec {{Container}} cscli decisions delete {{flag}} {{quoted}} 2>&1 | sed 's/^/  /' | grep -v '^ *$' || true
echo "  {{value}} can visit the hotel again (the bouncer knows within a minute)""#;

/// Lifts a ban (an address or a range).
pub async fn unban(server: &dyn Server, value: &str, log: Log<'_>) -> Result<()> {
    if let Some(problem) = fail2ban::entry_problem(value) {
        fail!("{problem}");
    }
    let flag = if value.contains('/') { "-r" } else { "-i" };
    let body = fill(UNBAN, &[("flag", flag), ("Container", CONTAINER), ("quoted", &shell_quote(value)), ("value", value)]);
    script::run(server, &script::with_prelude(&body), log, "Unbanning failed", None).await
}

const UPDATE_RULES: &str = r#"echo "> cscli hub update and upgrade (the detection rules)"
docker exec {{Container}} cscli hub update 2>&1 | grep -viE '^ *$|level=info' | sed 's/^/  /' || true
out=$(docker exec {{Container}} cscli hub upgrade 2>&1) || { echo "$out" | tail -5 | sed 's/^/  /'; fail "CrowdSec's hub could not be reached: the rules stay as they were."; }
changed=$(echo "$out" | grep -ciE 'upgrad(ed|ing)|updated|installed' || true)
if [ "${changed:-0}" -gt 0 ]; then
  echo "$out" | grep -iE 'upgrad(ed|ing)|updated|installed' | head -20 | sed 's/^/  /'
  echo "  Restarting CrowdSec to load the new rules"
  docker restart {{Container}} >/dev/null
  for i in $(seq 1 45); do docker exec {{Container}} cscli lapi status >/dev/null 2>&1 && break; sleep 2; done
  docker exec {{Container}} cscli lapi status >/dev/null 2>&1 || fail "CrowdSec does not start again after the rule update (see: docker logs crowdsec)."
else
  echo "  The rules were up to date"
fi
# The bouncer plugin comes with the installer: Addons → CrowdSec puts the version this installer knows in Traefik.
plugin=$(grep -A3 '{{Plugin}}:' {{TraefikDirectory}}/traefik.yml 2>/dev/null | sed -n 's/^ *version: *//p' | head -1)
if [ -n "$plugin" ] && [ "$plugin" != "{{PluginVersion}}" ]; then
  echo "  Traefik's CrowdSec plugin is $plugin; this installer has {{PluginVersion}}: run Addons → CrowdSec again to update it"
elif [ -n "$plugin" ]; then
  echo "  Traefik's CrowdSec plugin: $plugin (the newest this installer knows)"
fi"#;

/// Pulls the newest image and recreates the container when it changed, then brings the detection rules (the hub's
/// collections, parsers and scenarios) up to date. The bans, the enrollment and the bouncer stay; while CrowdSec
/// restarts the bouncer keeps its ban list and the hotel stays reachable.
pub async fn update(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let ready = format!("docker exec {CONTAINER} cscli lapi status >/dev/null 2>&1");
    server_tools::update_image(server, "CrowdSec", directory(), CONTAINER, VERSION_COMMAND, &ready, log).await?;
    let body = fill(
        UPDATE_RULES,
        &[
            ("Container", CONTAINER),
            ("Plugin", PLUGIN),
            ("TraefikDirectory", web_access::traefik_directory()),
            ("PluginVersion", PLUGIN_VERSION),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Updating CrowdSec's rules failed", None).await
}

const REMOVE_TRAEFIK: &str = r#"t={{TraefikDirectory}}
if [ -f {{MiddlewareFile}} ]; then
  rm -f {{MiddlewareFile}}
  echo "> {{MiddlewareFile}} deleted"
  {{staticConfig}}
  echo "> Restarting Traefik without the bouncer (players are disconnected for a moment)"
  compose_in "$t" up -d --force-recreate 2>&1 | grep -v '^ *$'
  {{waitForTraefik}}
fi"#;

const REMOVE: &str = r#"c={{Directory}}
{{traefik}}
if [ -f "$c/compose.yaml" ]; then
  echo "> docker compose down (CrowdSec)"
  compose_in "$c" down 2>&1 | grep -v '^ *$' || true
fi
docker rm -f {{Container}} >/dev/null 2>&1 || true
rm -f /etc/logrotate.d/traefik-access
if [ {{deleteData}} = yes ]; then
  rm -rf "$c"
  docker image rm {{Image}} >/dev/null 2>&1 || true
  echo "  $c deleted"
else
  echo "  $c kept (installing again uses its data and enrollment)"
fi"#;

/// Takes the bouncer out of Traefik, then stops CrowdSec; its data only when asked.
pub async fn remove(server: &dyn Server, site: Option<&WebAccessSettings>, delete_data: bool, log: Log<'_>) -> Result<()> {
    let traefik = match site {
        None => String::new(),
        Some(site) => fill(
            REMOVE_TRAEFIK,
            &[
                ("TraefikDirectory", web_access::traefik_directory()),
                ("MiddlewareFile", &middleware_file()),
                ("waitForTraefik", &web_access::wait_for_traefik_script()),
                ("staticConfig", &web_access::static_config_script(site)),
            ],
        ),
    };
    let body = fill(
        REMOVE,
        &[
            ("Directory", directory()),
            ("Container", CONTAINER),
            ("deleteData", if delete_data { "yes" } else { "no" }),
            ("Image", IMAGE),
            ("traefik", &traefik),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Removing CrowdSec failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> CrowdSecState {
        state_from(
            &ScriptValues::parse(
                "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\ndocker=yes\ntraefik=true\ncs_LAPI_PORT=8081\ncs_ENROLLED=yes\ncs_INSTANCE=hotel\n\
cs_WHITELIST=203.0.113.5 198.51.100.0/24\ninstalled=yes\nbouncer=yes\nrunning=true\nversion=v1.6.4\nbanned=192.0.2.1 192.0.2.2\ndns=104.21.1.1\ndnscf=no\n",
            ),
            Some("203.0.113.5".into()),
        )
    }

    #[test]
    fn parses_the_state() {
        let s = state();
        assert!(s.is_complete() && s.docker_running && s.traefik_running);
        assert_eq!(s.port, 8081);
        assert_eq!(s.instance_name.as_deref(), Some("hotel"));
        assert_eq!(s.whitelist, vec!["203.0.113.5".to_string(), "198.51.100.0/24".to_string()]);
        assert_eq!(s.banned.len(), 2);
        assert_eq!(s.dns_behind_cloudflare, Some(false));
        assert_eq!(problem(&s), None);
        let empty = state_from(&ScriptValues::parse("traefik=false\nrunning=\n"), None);
        assert!(!empty.traefik_running && !empty.is_complete());
        assert_eq!(empty.dns_behind_cloudflare, None);
        assert_eq!(empty.port, 0);
    }

    #[test]
    fn problems() {
        let s = state();
        assert!(problem(&CrowdSecState { site: None, ..s.clone() }).unwrap().starts_with("Run the Web access step first"));
        assert!(problem(&CrowdSecState { docker_running: false, ..s.clone() }).unwrap().starts_with("Docker does not run"));
        assert!(problem(&CrowdSecState { traefik_running: false, ..s.clone() }).unwrap().starts_with("Traefik does not run"));
        let cf = problem(&CrowdSecState { dns_behind_cloudflare: Some(true), ..s.clone() }).unwrap();
        assert!(cf.starts_with("hotel.example.com points to Cloudflare") && cf.contains("addresses instead of the visitors'"));
    }

    #[test]
    fn keys_and_names() {
        assert_eq!(key_from("sudo cscli console enroll -e context cm1abcdefghijklmnop").as_deref(), Some("cm1abcdefghijklmnop"));
        assert_eq!(key_from("  'cm1abcdefghijklmnop'\r\n").as_deref(), Some("cm1abcdefghijklmnop"));
        assert_eq!(key_from("short"), None);
        assert_eq!(key_from("-e"), None);
        assert_eq!(key_problem("  ", true), None);
        assert!(key_problem("", false).is_some());
        assert!(key_problem("nope", true).is_some());
        assert_eq!(name_problem("my-hotel_1.eu"), None);
        assert_eq!(name_problem("hotel\n"), None);
        assert!(name_problem("").is_some());
        assert!(name_problem("my hotel").is_some());
        assert!(name_problem(&"a".repeat(65)).is_some());
    }
}
