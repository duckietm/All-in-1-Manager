//! Docker Engine, NGINX on 127.0.0.1:85, Traefik in /opt/traefik, the Polaris whitelist and the tests through
//! Traefik. (WebAccessInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use crate::script::ScriptValues;
use crate::server::{Result, Server};

/// How the hotel is reached from the internet. With a CMS (AtomCMS, EpicNext, or your own CMS in /var/www/<custom_cms>)
/// the CMS is the site and Octane moves to /client. `cms_port` is the CMS' own NGINX site on 127.0.0.1 (85-95); 0 for
/// an AtomCMS set up before that, which the hotel site on port 85 still serves itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebAccessSettings {
    pub address: String,
    pub https: bool,
    pub cloudflare: bool,
    pub atom_cms: bool,
    pub custom_cms: Option<String>,
    pub cms_port: i64,
    pub epic_next: bool,
}

impl WebAccessSettings {
    pub fn new(address: &str, https: bool, cloudflare: bool) -> Self {
        Self { address: address.into(), https, cloudflare, ..Default::default() }
    }

    pub fn scheme(&self) -> &'static str {
        if self.https {
            "https"
        } else {
            "http"
        }
    }

    pub fn has_cms(&self) -> bool {
        self.atom_cms || self.epic_next || self.custom_cms.is_some()
    }

    /// The CMS has its own NGINX site; Traefik sends it everything except /client, /gamedata, /custom and Polaris.
    pub fn cms_on_own_port(&self) -> bool {
        self.has_cms() && self.cms_port > 0
    }

    /// "AtomCMS", "EpicNext CMS", the custom CMS' folder name, or None.
    pub fn cms_name(&self) -> Option<String> {
        if self.atom_cms {
            Some("AtomCMS".into())
        } else if self.epic_next {
            Some("EpicNext CMS".into())
        } else {
            self.custom_cms.clone()
        }
    }

    /// Where Octane is served, "/" or "/client/".
    pub fn client_path(&self) -> &'static str {
        if self.has_cms() {
            "/client/"
        } else {
            "/"
        }
    }

    pub fn client_url(&self) -> String {
        format!("{}{}", self.url(), self.client_path())
    }

    pub fn url(&self) -> String {
        if self.address.contains(':') {
            format!("{}://[{}]", self.scheme(), self.address)
        } else {
            format!("{}://{}", self.scheme(), self.address)
        }
    }

    /// For a bare IP address Traefik answers for every host name.
    pub fn is_ip_address(&self) -> bool {
        self.address.parse::<std::net::IpAddr>().is_ok()
    }

    /// The second name of a hotel on a domain of its own: www.example.com for example.com, and example.com for
    /// www.example.com. None for a subdomain (test.example.com), a name without a dot (localhost) and an IP address:
    /// those are the only name. Traefik redirects the second name to the address, so cookies, Polaris' websocket
    /// whitelist and the CMS' own URL keep one name.
    pub fn www_alias(&self) -> Option<String> {
        if self.is_ip_address() {
            return None;
        }
        let address = self.address.trim_end_matches('.').to_lowercase();
        let (www, base) = match address.strip_prefix("www.") {
            Some(base) => (true, base.to_string()),
            None => (false, address.clone()),
        };
        if !is_registrable_domain(&base) {
            return None;
        }
        Some(if www { base } else { format!("www.{base}") })
    }

    /// Reads /opt/traefik/hotel.env as printed with a conf_ prefix. None before the web access step.
    pub fn from_hotel_env(values: &ScriptValues) -> Option<Self> {
        let address = values.get("conf_ADDRESS")?;
        let cms = values.get("conf_CMS");
        Some(Self {
            address: address.to_string(),
            https: values.get("conf_SCHEME") == Some("https"),
            cloudflare: values.yes("conf_CLOUDFLARE"),
            atom_cms: cms == Some("atom"),
            custom_cms: if cms == Some("custom") { values.string("conf_CMS_NAME") } else { None },
            cms_port: values.int("conf_CMS_PORT"),
            epic_next: cms == Some("epicnext"),
        })
    }
}

// Public suffixes with two parts that hotels use (example.co.uk is a domain of its own, like example.com).
const TWO_PART_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "me.uk", "ltd.uk", "plc.uk", "net.uk", "com.au", "net.au", "org.au", "co.nz", "net.nz", "org.nz", "com.br",
    "net.br", "org.br", "com.tr", "net.tr", "org.tr", "co.za", "com.mx", "com.ar", "co.jp", "com.cn", "com.sg", "com.my", "co.in",
    "co.id", "com.ph", "co.kr", "co.il", "com.pl", "com.pt", "com.es", "co.it", "com.ua", "com.ru",
];

// Names that never get a www twin: on the own computer or the local network.
const LOCAL_SUFFIXES: &[&str] = &["localhost", "local", "lan", "home", "internal", "test", "invalid", "example"];

/// example.com, example.co.uk: the name a registrar sells, not a subdomain of it, a name without a dot or a local one.
fn is_registrable_domain(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) || LOCAL_SUFFIXES.contains(labels.last().unwrap()) {
        return false;
    }
    let two_part = labels.len() >= 2 && TWO_PART_SUFFIXES.contains(&labels[labels.len() - 2..].join(".").as_str());
    labels.len() == if two_part { 3 } else { 2 }
}

pub const TRAEFIK_DIRECTORY: &str = "/opt/traefik";
pub const NGINX_PORT: &str = crate::hotel_site::PORT;
pub const POLARIS_PORT: &str = "2096";
pub const SITE_FILE: &str = crate::hotel_site::FILE;

/// Waits until the traefik container runs and listens on port 80; fails with its log otherwise. (WaitForTraefikScript)
pub fn wait_for_traefik_script() -> String {
    fill(WAIT_FOR_TRAEFIK, &[("GoRunning", GO_RUNNING)])
}

/// Writes $t/traefik.yml, with the CrowdSec parts while the CrowdSec addon's middleware file exists. Only Cloudflare,
/// when used, may set X-Forwarded-For; otherwise Traefik writes the visitor's address itself, so it cannot be faked.
/// (StaticConfigScript)
pub fn static_config_script(settings: &WebAccessSettings) -> String {
    fill(
        STATIC_CONFIG,
        &[
            ("cloudflare", if settings.cloudflare { "yes" } else { "no" }),
            ("MiddlewareFile", &crate::crowdsec::middleware_file()),
            ("withCrowdSec", &static_yaml(settings, true)),
            ("withoutCrowdSec", &static_yaml(settings, false)),
        ],
    )
}

/// Writes $t/compose.yaml. With the host network Traefik reaches NGINX, Polaris (which trusts 127.0.0.1 as the proxy
/// that sets X-Forwarded-For) and CrowdSec on 127.0.0.1. The access log folder and the plugin storage are for
/// CrowdSec. (ComposeScript)
pub fn compose_script() -> String {
    fill(COMPOSE, &[("AccessLogDirectory", crate::crowdsec::ACCESS_LOG_DIRECTORY)])
}

/// Polaris reads X-Forwarded-For from right to left and skips only the proxies it trusts. Behind Cloudflare the
/// header is "visitor, Cloudflare edge" (Traefik adds the edge it saw), so Cloudflare's ranges must be trusted too, or
/// every player gets a Cloudflare address. Without Cloudflare nothing extra is trusted (127.0.0.1 always is).
/// Cloudflare's ranges, comma separated, behind Cloudflare; "" otherwise. (PolarisTrustedProxiesAsync)
pub async fn polaris_trusted_proxies(server: &dyn Server, settings: &WebAccessSettings) -> Result<String> {
    if settings.cloudflare {
        Ok(cloudflare_ranges(server).await?.join(","))
    } else {
        Ok(String::new())
    }
}

// ---------------------------------------------------------------------------------------------------------------

use crate::script::{self, fill};
use crate::server::Log;

/// Docker, Traefik and NGINX state before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAccessState {
    pub docker_version: Option<String>,
    pub docker_from_docker: bool,
    pub conflicting_packages: Vec<String>,
    pub docker_running: bool,
    pub traefik_running: bool,
    pub site_enabled: bool,
    pub configured: Option<WebAccessSettings>,
    pub port80_process: Option<String>,
    pub port443_process: Option<String>,
    pub emulator_running: bool,
}

impl WebAccessState {
    pub fn is_complete(&self) -> bool {
        self.docker_from_docker && self.docker_running && self.traefik_running && self.site_enabled && self.configured.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAccessResult {
    pub certificate_valid: bool,
    pub certificate_problem: Option<String>,
    pub api_answered: bool,
    pub web_socket_answered: bool,
}

/// "Docker Release (CE deb)" key from download.docker.com/linux/<distro>/gpg.
pub const DOCKER_KEY_FINGERPRINT: &str = "9DC858229FC7DD38854AE2D88D81803C0EBFCD88";

// Every key currently in Docker's gpg file. Any other key is refused.
const KNOWN_DOCKER_KEYS: [&str; 2] = [DOCKER_KEY_FINGERPRINT, "D3306A018370199E527AE7997EA0A9C3F273FCD8"];

/// From docs.docker.com/engine/install, "Uninstall old versions".
pub const CONFLICTING_PACKAGES: &str = "docker.io docker-compose docker-compose-v2 docker-doc podman-docker containerd runc";

// Go template braces would look like placeholders: filled in last.
const GO_RUNNING: &str = "{{.State.Running}}";

// Name of the process listening on the port, or empty when there is none.
fn port_owner(port: u16) -> String {
    format!(r#"ss -Hltnp 'sport = :{port}' 2>/dev/null | grep -oE 'users:\(\("[^"]+"' | head -1 | cut -d'"' -f2"#)
}

const INSPECT: &str = r#"echo "docker=$(dpkg-query -W -f='${db:Status-Abbrev}${Version}' docker-ce 2>/dev/null | awk '/^ii/{print substr($0,4)}')"
echo "dockerorg=$(apt-cache policy docker-ce 2>/dev/null | grep -q download.docker.com && echo yes)"
echo "conflicts=$(dpkg-query -W -f='${db:Status-Abbrev}${Package}\n' {{ConflictingPackages}} 2>/dev/null | awk '/^ii/{printf "%s ", substr($0,4)}')"
echo "dockerrunning=$(docker info >/dev/null 2>&1 && echo yes)"
echo "traefik=$(docker inspect -f '{{GoRunning}}' traefik 2>/dev/null)"
echo "site=$([ -L /etc/nginx/sites-enabled/hotel ] && grep -qs 'listen 127.0.0.1:{{NginxPort}}' {{SiteFile}} && echo yes)"
[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
echo "port80=$({{port80}})"
echo "port443=$({{port443}})"
echo "emulator=$(ss -Hltn 'sport = :{{PolarisPort}}' 2>/dev/null | grep -q . && echo yes)""#;

pub async fn inspect(server: &dyn Server) -> Result<WebAccessState> {
    let script = fill(
        INSPECT,
        &[
            ("ConflictingPackages", CONFLICTING_PACKAGES),
            ("NginxPort", NGINX_PORT),
            ("SiteFile", SITE_FILE),
            ("TraefikDirectory", TRAEFIK_DIRECTORY),
            ("port80", &port_owner(80)),
            ("port443", &port_owner(443)),
            ("PolarisPort", POLARIS_PORT),
            ("GoRunning", GO_RUNNING),
        ],
    );
    let result = server.run_as_root(&script).await?;
    Ok(parse_state(&ScriptValues::parse(&result.output)))
}

fn parse_state(values: &ScriptValues) -> WebAccessState {
    // "5:29.9.2-1~ubuntu.24.04~noble" -> "29.9.2"
    let docker = values.get("docker").map(|package| {
        let after = &package[package.find(':').map_or(0, |i| i + 1)..];
        after.split('-').next().unwrap_or("").to_string()
    });
    WebAccessState {
        docker_version: docker,
        docker_from_docker: values.yes("dockerorg"),
        conflicting_packages: values.or_empty("conflicts").split(' ').filter(|p| !p.is_empty()).map(str::to_string).collect(),
        docker_running: values.yes("dockerrunning"),
        traefik_running: values.get("traefik") == Some("true"),
        site_enabled: values.yes("site"),
        configured: WebAccessSettings::from_hotel_env(values),
        port80_process: values.string("port80"),
        port443_process: values.string("port443"),
        emulator_running: values.yes("emulator"),
    }
}

const INSTALL_DOCKER: &str = r#"case "$ID" in
  ubuntu|debian) distro=$ID ;;
  *) fail "Docker's repository is for Ubuntu and Debian; this server runs $ID." ;;
esac
conflicts=$(dpkg-query -W -f='${db:Status-Abbrev}${Package}\n' {{ConflictingPackages}} 2>/dev/null | awk '/^ii/{printf "%s ", substr($0,4)}')
if [ -n "$conflicts" ]; then
  echo "> Removing packages that conflict with Docker Engine: $conflicts"
  apt-get remove -y $conflicts
fi

echo "> Adding the Docker repository (download.docker.com/linux/$distro)"
apt_update >/dev/null
apt-get install -y ca-certificates curl gnupg
install -m 0755 -d /etc/apt/keyrings
tmp=$(mktemp)
curl -fsSL "https://download.docker.com/linux/$distro/gpg" -o "$tmp" || fail "Could not download Docker's signing key."
fingerprints=$(gpg --show-keys --with-colons "$tmp" 2>/dev/null | awk -F: '$1=="fpr" {print $10}')
echo "$fingerprints" | grep -qx '{{DockerKeyFingerprint}}' || { rm -f "$tmp"; fail "Docker's signing key does not have the published fingerprint {{DockerKeyFingerprint}}."; }
for fpr in $fingerprints; do
  echo "$fpr" | grep -Eqx '{{known}}' || { rm -f "$tmp"; fail "Docker's key file contains an unknown key $fpr."; }
done
echo "  key {{DockerKeyFingerprint}} checked"
install -m 0644 "$tmp" /etc/apt/keyrings/docker.asc
rm -f "$tmp"
# The format docs.docker.com uses now. An older docker.list would add the same repository twice.
rm -f /etc/apt/sources.list.d/docker.list
cat > /etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/$distro
Suites: $codename
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF
apt_update 2>&1 | grep -E 'docker|^(E|W):' || true
apt-cache policy docker-ce | grep -q download.docker.com || fail "Docker has no packages for $PRETTY_NAME ($codename) yet."

echo "> apt-get install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin"
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

echo "> Starting Docker"
if [ -d /run/systemd/system ]; then
  systemctl enable --now docker
elif ! docker info >/dev/null 2>&1; then
  service docker start || (setsid dockerd </dev/null >/var/log/dockerd.log 2>&1 &)
fi
for i in $(seq 1 30); do docker info >/dev/null 2>&1 && break; sleep 1; done
docker info >/dev/null 2>&1 || fail "Docker is installed, but the Docker daemon does not run (see journalctl -u docker)."
docker --version
docker compose version"#;

/// Follows docs.docker.com/engine/install/ubuntu and /debian, with a verified key.
pub async fn install_docker(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let known = KNOWN_DOCKER_KEYS.join("|");
    let body = fill(
        INSTALL_DOCKER,
        &[("ConflictingPackages", CONFLICTING_PACKAGES), ("DockerKeyFingerprint", DOCKER_KEY_FINGERPRINT), ("known", &known)],
    );
    script::run(server, &script::with_prelude(&body), log, "Installing Docker failed", None).await
}

/// Moves the hotel site to 127.0.0.1:85 and the default site off port 80, which Traefik needs.
pub async fn configure_nginx(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let script = script::with_prelude(&crate::hotel_site::write_script(settings, None));
    script::run(server, &script, log, "Configuring NGINX failed", None).await
}

const CONFIGURE_TRAEFIK: &str = r#"t={{TraefikDirectory}}
install -d -m 755 "$t" "$t/dynamic"
install -d -m 700 "$t/letsencrypt"

{{StaticConfigScript}}

{{RoutesScript}}

{{ComposeScript}}

echo "> docker compose pull (traefik:v3)"
pulled=
for i in 1 2 3; do
  if compose_in "$t" pull -q 2>"$t/pull.log"; then pulled=yes; break; fi
  tail -1 "$t/pull.log" | sed 's/^/  /'
  [ $i -lt 3 ] && { echo "  Trying again in $((i * 20)) seconds ..."; sleep $((i * 20)); }
done
rm -f "$t/pull.log"
if [ -z "$pulled" ]; then
  docker image inspect traefik:v3 >/dev/null 2>&1 || fail "Could not download the Traefik image from Docker Hub (see above). Try again later."
  echo "  Docker Hub did not answer: using the traefik:v3 image that is already on this server"
fi
echo "> docker compose up -d"
compose_in "$t" up -d --force-recreate 2>&1 | grep -v '^ *$'
{{WaitForTraefikScript}}

ufw_allow 80/tcp 443/tcp"#;

/// With Cloudflare, X-Forwarded-For is trusted only from Cloudflare's published ranges.
pub async fn configure_traefik(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let text = fill(CONFIGURE_TRAEFIK, &[("TraefikDirectory", TRAEFIK_DIRECTORY)]);
    let body = fill(
        &text,
        &[
            ("StaticConfigScript", &static_config_script(settings)),
            ("RoutesScript", &routes_script(settings)),
            ("ComposeScript", &compose_script()),
            ("WaitForTraefikScript", &wait_for_traefik_script()),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Setting up Traefik failed", None).await
}

const WAIT_FOR_TRAEFIK: &str = r#"for i in $(seq 1 20); do
  [ "$(docker inspect -f '{{GoRunning}}' traefik 2>/dev/null)" = true ] && ss -Hltn 'sport = :80' | grep -q . && break
  sleep 1
done
[ "$(docker inspect -f '{{GoRunning}}' traefik 2>/dev/null)" = true ] || { docker logs --tail 20 traefik 2>&1 | sed 's/^/  /'; fail "The Traefik container does not run."; }
ss -Hltn 'sport = :80' | grep -q . || { docker logs --tail 20 traefik 2>&1 | sed 's/^/  /'; fail "Traefik runs, but does not listen on port 80."; }
echo "  $(docker exec traefik traefik version 2>/dev/null | awk '/^Version/ {print "Traefik " $2}') is running""#;

// Traefik's static configuration. With the CrowdSec addon: the access log CrowdSec reads, the bouncer plugin, and the
// bouncer on both entry points (so on every route, Gitea's too). [TRUSTED] becomes Cloudflare's ranges.
fn static_yaml(settings: &WebAccessSettings, crowdsec: bool) -> String {
    use crate::crowdsec as cs;
    let middlewares = if crowdsec { format!("\n      middlewares: [{}@file]", cs::MIDDLEWARE) } else { String::new() };
    let crowdsec_http = if crowdsec { format!("\n    http:{middlewares}") } else { String::new() };
    let web = if settings.https {
        format!("\n    http:\n      redirections:\n        entryPoint:\n          to: websecure\n          scheme: https{middlewares}")
    } else {
        crowdsec_http.clone()
    };
    let websecure = if settings.https {
        format!("\n  websecure:\n    address: \":443\"\n    forwardedHeaders:\n      trustedIPs: [TRUSTED]{crowdsec_http}")
    } else {
        String::new()
    };
    let acme = if settings.https {
        "\ncertificatesResolvers:\n  letsencrypt:\n    acme:\n      storage: /letsencrypt/acme.json\n      httpChallenge:\n        entryPoint: web"
    } else {
        ""
    };
    let extra = if crowdsec {
        format!(
            "\n# CrowdSec (Addons → CrowdSec): it reads this log; the plugin blocks the addresses it bans.\naccessLog:\n  filePath: {}\nexperimental:\n  plugins:\n    {}:\n      moduleName: {}\n      version: {}",
            cs::ACCESS_LOG,
            cs::PLUGIN,
            cs::PLUGIN_MODULE,
            cs::PLUGIN_VERSION
        )
    } else {
        String::new()
    };
    format!(
        "# Traefik static configuration (written by All-in-1 Manager).\nentryPoints:\n  web:\n    address: \":80\"\n    forwardedHeaders:\n      trustedIPs: [TRUSTED]{web}{websecure}\nproviders:\n  file:\n    directory: /etc/traefik/dynamic\n    watch: true{acme}\nlog:\n  level: INFO\n  noColor: true{extra}"
    )
}

const STATIC_CONFIG: &str = r#"trusted=
if [ {{cloudflare}} = yes ]; then
  load_cloudflare_ranges
  trusted=$(echo "$ranges" | sed 's/.*/"&"/' | paste -sd, -)
fi
if [ -f {{MiddlewareFile}} ]; then
  echo "> $t/traefik.yml (with CrowdSec)"
  cat > "$t/traefik.yml" <<'EOF'
{{withCrowdSec}}
EOF
else
  echo "> $t/traefik.yml"
  cat > "$t/traefik.yml" <<'EOF'
{{withoutCrowdSec}}
EOF
fi
sed -i "s#\[TRUSTED\]#[$trusted]#" "$t/traefik.yml""#;

const COMPOSE: &str = r#"echo "> $t/compose.yaml"
install -d -m 755 {{AccessLogDirectory}} "$t/plugins-storage"
cat > "$t/compose.yaml" <<'EOF'
# Traefik for the hotel (written by All-in-1 Manager). Update: docker compose pull && docker compose up -d
services:
  traefik:
    image: traefik:v3
    container_name: traefik
    restart: unless-stopped
    network_mode: host
    volumes:
      - ./traefik.yml:/etc/traefik/traefik.yml:ro
      - ./dynamic:/etc/traefik/dynamic:ro
      - ./letsencrypt:/letsencrypt
      - ./plugins-storage:/plugins-storage
      - {{AccessLogDirectory}}:{{AccessLogDirectory}}
EOF"#;

/// Polaris' own /api paths. With a CMS only these go to Polaris because the CMS has its own /api.
pub const POLARIS_API_PATHS: [&str; 10] = ["auth", "authx", "badges", "cms", "emustats", "health", "housekeeping", "maintenance", "public", "wired"];

const ROUTES: &str = r#"echo "> $t/dynamic/hotel.yml: {{Url}}{{description}}"
cat > "$t/dynamic/hotel.yml" <<'EOF'
# The hotel's routes (written by All-in-1 Manager).
http:
  routers:
    polaris:
      rule: "{{host}}({{polaris}})"
      entryPoints: [{{entryPoint}}]
      service: polaris
      priority: 100{{tls}}
    hotel:
      rule: "{{hotelRule}}"
      entryPoints: [{{entryPoint}}]
      service: nginx
      priority: {{hotelPriority}}{{tls}}{{cmsRouter}}
  services:
    polaris:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:{{PolarisPort}}"
    nginx:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:{{NginxPort}}"{{cmsService}}
EOF

cat > "$t/hotel.env" <<'EOF'
ADDRESS={{Address}}
SCHEME={{Scheme}}
CLOUDFLARE={{cloudflare}}
CMS={{cms}}
CMS_NAME={{cmsName}}
CMS_PORT={{cmsPort}}
EOF{{www}}"#;

// The hotel's second name (www.example.com, or example.com for www.example.com): its own router that redirects to the
// address, with a certificate of its own, so a missing record can never cost the address its certificate. Only when the
// name has a DNS record that points where the address points (or to Cloudflare); otherwise the step says what to add.
const WWW_ROUTE: &str = r#"

www={{alias}}
main=$(getent ahostsv4 {{Address}} 2>/dev/null | awk '{print $1}' | sort -u)
second=$(getent ahostsv4 "$www" 2>/dev/null | awk '{print $1}' | sort -u)
if [ -n "$second" ] && { [ {{cloudflare}} = yes ] || [ -z "$main" ] || [ -n "$(printf '%s\n' $main $second | sort | uniq -d)" ]; }; then
  echo "> $t/dynamic/hotel-www.yml: $www → {{Url}}"
  cat > "$t/dynamic/hotel-www.yml" <<'EOF'
# {{alias}} leads to the hotel's address (written by All-in-1 Manager).
http:
  routers:
    hotel-www:
      rule: "Host(`{{alias}}`)"
      entryPoints: [{{entryPoint}}]
      middlewares: [hotel-www]
      service: noop@internal{{tls}}
  middlewares:
    hotel-www:
      redirectRegex:
        regex: "^[^:]+://[^/]+(.*)"
        replacement: "{{Url}}${1}"
        permanent: true
EOF
else
  rm -f "$t/dynamic/hotel-www.yml"
  if [ -z "$second" ]; then
    echo "  $www has no DNS record yet: add one (an A record with this server's IP{{cname}}{{proxied}}) and run this step again, so $www leads to the hotel too."
  else
    echo "  $www points to $(echo $second), not where {{Address}} points ($(echo $main)): change its DNS record and run this step again, so $www leads to the hotel too."
  fi
fi"#;

/// The part of the routes script for the hotel's second name: the redirect, or removing an old one.
fn www_route_script(settings: &WebAccessSettings, entry_point: &str, tls: &str) -> String {
    let Some(alias) = settings.www_alias() else {
        return "\nrm -f \"$t/dynamic/hotel-www.yml\"".into();
    };
    fill(
        WWW_ROUTE,
        &[
            ("alias", &alias),
            ("Address", &settings.address),
            ("Url", &settings.url()),
            ("cloudflare", if settings.cloudflare { "yes" } else { "no" }),
            ("entryPoint", entry_point),
            ("tls", tls),
            // A CNAME only works for www, not for the bare domain.
            ("cname", &if alias.starts_with("www.") { format!(", or a CNAME to {}", settings.address) } else { String::new() }),
            ("proxied", if settings.cloudflare { ", proxied in Cloudflare" } else { "" }),
        ],
    )
}

// Writes dynamic/hotel.yml and hotel.env in $t. Traefik picks up the routes without a restart.
fn routes_script(settings: &WebAccessSettings) -> String {
    let own = settings.cms_on_own_port();
    let entry_point = if settings.https { "websecure" } else { "web" };
    let host = if settings.is_ip_address() { String::new() } else { format!("Host(`{}`) && ", settings.address) };
    let site_rule = if settings.is_ip_address() { "PathPrefix(`/`)".to_string() } else { format!("Host(`{}`)", settings.address) };
    let tls = if settings.https { "\n      tls:\n        certResolver: letsencrypt" } else { "" };
    let polaris = if settings.has_cms() {
        format!("PathPrefix(`/ws`) || PathPrefix(`/nitro-sec`) || PathRegexp(`^/api/({})(/|$)`)", POLARIS_API_PATHS.join("|"))
    } else {
        "PathPrefix(`/ws`) || PathPrefix(`/api`)".to_string()
    };
    let cms_name = settings.cms_name().unwrap_or_default();
    let description = if own {
        format!(
            "/ws and Polaris' /api paths to Polaris, /client, /gamedata and /custom to the hotel site, the rest to {cms_name} on port {}",
            settings.cms_port
        )
    } else if settings.has_cms() {
        format!("/ws and Polaris' /api paths to Polaris, the rest ({cms_name}, /client) to NGINX")
    } else {
        "/ws and /api to Polaris, the rest to NGINX".to_string()
    };
    // With the CMS on its own port, the hotel site keeps Octane (/client) and the assets; the CMS gets the rest.
    let cms_router = if own {
        format!("\n    cms:\n      rule: \"{site_rule}\"\n      entryPoints: [{entry_point}]\n      service: cms\n      priority: 1{tls}")
    } else {
        String::new()
    };
    // Whole folders only: a CMS page such as /customize or /clients stays the CMS'. EpicNext has a page of its own on
    // /client (it loads Octane from /client/), so only /client/ goes to the hotel site there.
    let hotel_rule = if own {
        let folders = ["client", "gamedata", "custom"]
            .iter()
            .map(|f| if *f == "client" && settings.epic_next { "PathPrefix(`/client/`)".to_string() } else { format!("Path(`/{f}`) || PathPrefix(`/{f}/`)") })
            .collect::<Vec<_>>()
            .join(" || ");
        format!("{host}({folders})")
    } else {
        site_rule.clone()
    };
    let cms_service = if own {
        format!("\n    cms:\n      loadBalancer:\n        servers:\n          - url: \"http://127.0.0.1:{}\"", settings.cms_port)
    } else {
        String::new()
    };
    let cms = if settings.atom_cms {
        "atom"
    } else if settings.epic_next {
        "epicnext"
    } else if settings.custom_cms.is_some() {
        "custom"
    } else {
        "none"
    };
    fill(
        ROUTES,
        &[
            ("Url", &settings.url()),
            ("description", &description),
            ("host", &host),
            ("polaris", &polaris),
            ("entryPoint", entry_point),
            ("hotelRule", &hotel_rule),
            ("hotelPriority", if own { "50" } else { "1" }),
            ("tls", tls),
            ("cmsRouter", &cms_router),
            ("PolarisPort", POLARIS_PORT),
            ("NginxPort", NGINX_PORT),
            ("cmsService", &cms_service),
            ("Address", &settings.address),
            ("Scheme", settings.scheme()),
            ("cloudflare", if settings.cloudflare { "yes" } else { "no" }),
            ("cms", cms),
            ("cmsName", settings.custom_cms.as_deref().unwrap_or("")),
            ("cmsPort", &if own { settings.cms_port.to_string() } else { String::new() }),
            ("www", &www_route_script(settings, entry_point, tls)),
        ],
    )
}

/// Rewrites only the routes, for the CMS addon. Traefik itself keeps running.
pub async fn configure_routes(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let body = format!(
        "t={TRAEFIK_DIRECTORY}\n[ -d \"$t/dynamic\" ] || fail \"Traefik is not set up in $t. Run the Web access step first.\"\n{}",
        routes_script(settings)
    );
    script::run(server, &script::with_prelude(&body), log, "Changing the Traefik routes failed", None).await
}

const RELOAD_AFTER_REAL_IP: &str = r#"
nginx -t 2>&1 | sed 's/^/  /'
nginx -t >/dev/null 2>&1 || fail "nginx -t refuses the configuration (see above)."
if [ -d /run/systemd/system ]; then systemctl reload nginx; else nginx -s reload; fi
echo "  NGINX reloaded""#;

/// NGINX's trusted proxies ([`crate::hotel_site::REAL_IP_FILE`]): 127.0.0.1, and Cloudflare's ranges behind
/// Cloudflare. Points an older hotel site at the file, then checks and reloads NGINX (no connection is dropped).
pub async fn configure_nginx_real_ip(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let body = format!("{}{RELOAD_AFTER_REAL_IP}", crate::hotel_site::real_ip_script(settings));
    script::run(server, &script::with_prelude(&body), log, "Setting NGINX's visitor address failed", None).await
}

/// The weekly refresh of Cloudflare's ranges, at the place and with the name the hotel guides use.
pub const CLOUDFLARE_REFRESH_SCRIPT: &str = "/var/scripts/CF_Refresh.sh";
pub const CLOUDFLARE_REFRESH_CRON: &str = "/etc/cron.d/cf-refresh";

const CLOUDFLARE_REFRESH: &str = r##"#!/bin/bash
# Keeps Cloudflare's IP ranges current for the hotel (written by All-in-1 Manager; weekly from {{CloudflareRefreshCron}}).
# NGINX, Traefik and Polaris read the visitor's address from X-Forwarded-For past Cloudflare, instead of seeing Cloudflare's.
set -u
fail() { echo "ERROR: $*"; exit 1; }
v4=$(curl -fsS --max-time 20 https://www.cloudflare.com/ips-v4) || fail "Could not download Cloudflare's IPv4 ranges: nothing changed."
v6=$(curl -fsS --max-time 20 https://www.cloudflare.com/ips-v6) || fail "Could not download Cloudflare's IPv6 ranges: nothing changed."
ranges=$(printf '%s\n%s\n' "$v4" "$v6" | grep -E '^[0-9a-f.:]+/[0-9]+$')
[ "$(echo "$ranges" | grep -c .)" -ge 10 ] || fail "Cloudflare's ranges look wrong: nothing changed."
echo "$(date '+%F %T') $(echo "$ranges" | grep -c .) Cloudflare ranges"

# NGINX: the hotel site's trusted proxies. A reload keeps every connection.
realip={{RealIpFile}}
{ echo "# Proxies NGINX takes the visitor's address from (written by All-in-1 Manager): Traefik and Cloudflare."
  echo "set_real_ip_from 127.0.0.1;"
  echo "$ranges" | sed 's/.*/set_real_ip_from &;/'; } > "$realip.new"
if ! cmp -s "$realip.new" "$realip"; then
  cp "$realip" "$realip.old" 2>/dev/null || true
  mv "$realip.new" "$realip"
  if nginx -t >/dev/null 2>&1; then
    { [ -d /run/systemd/system ] && systemctl reload nginx; } || nginx -s reload
    echo "  NGINX: updated and reloaded"
  else
    [ -f "$realip.old" ] && mv "$realip.old" "$realip"
    echo "  NGINX: nginx -t refused the new list, the old one is back"
  fi
else
  rm -f "$realip.new"
  echo "  NGINX: up to date"
fi

# Traefik: the entry points trust X-Forwarded-For only from Cloudflare. A restart drops the players for a moment.
traefik={{TraefikDirectory}}/traefik.yml
list=$(echo "$ranges" | sed 's/.*/"&"/' | paste -sd, -)
if [ -f "$traefik" ]; then
  sed -E "s#^([[:space:]]*trustedIPs:).*#\1 [$list]#" "$traefik" > "$traefik.new"
  if ! cmp -s "$traefik.new" "$traefik"; then
    mv "$traefik.new" "$traefik"
    docker restart traefik >/dev/null && echo "  Traefik: updated and restarted"
  else
    rm -f "$traefik.new"
    echo "  Traefik: up to date"
  fi
fi

# CrowdSec's bouncer (Addons → CrowdSec) takes the visitor past the same ranges. Traefik reads it again by itself.
bouncer={{MiddlewareFile}}
if [ -f "$bouncer" ] && grep -q 'forwardedHeadersTrustedIPs:' "$bouncer"; then
  sed -E "s#^([[:space:]]*forwardedHeadersTrustedIPs:).*#\1 [$list]#" "$bouncer" > "$bouncer.new"
  if ! cmp -s "$bouncer.new" "$bouncer"; then
    cat "$bouncer.new" > "$bouncer"
    echo "  CrowdSec bouncer: updated"
  else
    echo "  CrowdSec bouncer: up to date"
  fi
  rm -f "$bouncer.new"
fi

# Polaris: the proxies it reads X-Forwarded-For past. Only stopped (and started again) when the list changed:
# it writes its settings back when it stops, so they cannot be changed while it runs.
d={{EmulatorDirectory}}
log={{EmulatorLogDirectory}}/emulator.log
db=$(grep -sE '^db.database=' "$d/config.ini" | cut -d= -f2)
if [ -n "$db" ]; then
  want=$(echo "$ranges" | paste -sd, -)
  current=$(mariadb -N "$db" -e "SELECT \`value\` FROM emulator_settings WHERE \`key\` = 'ws.ip.header.trusted'" 2>/dev/null)
  if [ -z "$current" ]; then
    echo "  Polaris: not set up for Cloudflare (Tools → Fix visitor IPs does that)"
  elif [ "$current" = "$want" ]; then
    echo "  Polaris: up to date"
  else
{{ControlFunctions}}
    was_running=
    if emulator_running; then was_running=yes; stop_emulator; fi
    mariadb "$db" -e "UPDATE emulator_settings SET \`value\` = '$want' WHERE \`key\` = 'ws.ip.header.trusted'"
    if [ -n "$was_running" ]; then start_emulator; wait_loaded; fi
    echo "  Polaris: updated${was_running:+ and restarted}"
  fi
fi"##;

/// The refresh script: downloads Cloudflare's ranges and gives them to NGINX (reloaded gracefully), Traefik and
/// Polaris. Traefik and Polaris only restart when the ranges changed, because a restart briefly drops the players;
/// Polaris only when it was set up for Cloudflare before (Web access, Harden, or Tools → Fix visitor IPs).
fn cloudflare_refresh_content() -> String {
    let text = fill(
        CLOUDFLARE_REFRESH,
        &[
            ("CloudflareRefreshCron", CLOUDFLARE_REFRESH_CRON),
            ("RealIpFile", crate::hotel_site::REAL_IP_FILE),
            ("TraefikDirectory", TRAEFIK_DIRECTORY),
            ("MiddlewareFile", &crate::crowdsec::middleware_file()),
            ("EmulatorDirectory", &crate::emulator::directory()),
            ("EmulatorLogDirectory", &crate::emulator::log_directory()),
        ],
    );
    fill(&text, &[("ControlFunctions", &crate::emulator::control_functions())])
}

const INSTALL_REFRESH: &str = r#"echo "> {{CloudflareRefreshScript}}: Cloudflare's ranges for NGINX, Traefik and Polaris, weekly from {{CloudflareRefreshCron}}"
install -d -m 755 /var/scripts
cat > {{CloudflareRefreshScript}} <<'REFRESH'
{{content}}
REFRESH
chmod 755 {{CloudflareRefreshScript}}
printf '# Cloudflare IP ranges for the hotel (All-in-1 Manager)\n0 4 * * 0 root {{CloudflareRefreshScript}} >> /var/log/cf-refresh.log 2>&1\n' > {{CloudflareRefreshCron}}
chmod 644 {{CloudflareRefreshCron}}
echo "  {{CloudflareRefreshCron}}: every Sunday at 04:00, output in /var/log/cf-refresh.log"
echo "> Running it now"
{{CloudflareRefreshScript}} | sed 's/^/  /'
[ "${PIPESTATUS[0]}" = 0 ] || fail "The refresh script failed (see above).""#;

const REMOVE_REFRESH: &str = r#"if grep -qsE 'All-in-1 (Manager|Installer)' {{CloudflareRefreshScript}}; then
  rm -f {{CloudflareRefreshScript}} {{CloudflareRefreshCron}}
  echo "  No Cloudflare: the weekly refresh of Cloudflare's ranges is removed"
fi"#;

/// Behind Cloudflare: writes [`CLOUDFLARE_REFRESH_SCRIPT`] and its weekly cron job (Sunday 04:00) and runs it once.
/// Without Cloudflare: removes both when this installer wrote them.
pub async fn install_cloudflare_refresh(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let names = [("CloudflareRefreshScript", CLOUDFLARE_REFRESH_SCRIPT), ("CloudflareRefreshCron", CLOUDFLARE_REFRESH_CRON)];
    let body = if settings.cloudflare {
        fill(&fill(INSTALL_REFRESH, &names), &[("content", &cloudflare_refresh_content())])
    } else {
        fill(REMOVE_REFRESH, &names)
    };
    script::run(server, &script::with_prelude(&body), log, "Setting up the Cloudflare refresh failed", None).await
}

const CLOUDFLARE_RANGES: &str = r#"load_cloudflare_ranges >/dev/null
echo "RANGES=$(echo "$ranges" | paste -sd, -)""#;

/// Cloudflare's published IP ranges (IPv4 and IPv6), downloaded on the server.
pub async fn cloudflare_ranges(server: &dyn Server) -> Result<Vec<String>> {
    let values = script::run_capturing(
        server,
        &script::with_prelude(CLOUDFLARE_RANGES),
        &|_: &str| {},
        "Downloading Cloudflare's IP ranges failed",
        &["RANGES"],
        None,
    )
    .await?;
    Ok(values.or_empty("RANGES").split(',').map(str::trim).filter(|r| !r.is_empty()).map(str::to_string).collect())
}

/// Adds the hotel to ws.whitelist (websocket and CORS origin) and reads the visitor IP from X-Forwarded-For, trusting
/// Cloudflare's ranges when the site uses Cloudflare. Returns false when the emulator is not installed.
pub async fn configure_polaris(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<bool> {
    let trusted = polaris_trusted_proxies(server, settings).await?;
    let host = crate::harden::whitelist_host(settings);
    let current = crate::emulator::read_setting(server, "ws.whitelist").await?.unwrap_or_default();
    let whitelist = merge_whitelist(&current, &host);
    let settings = [
        ("ws.whitelist".to_string(), whitelist.clone()),
        ("websockets.whitelist".to_string(), whitelist),
        ("ws.ip.header".to_string(), "X-Forwarded-For".to_string()),
        ("ws.ip.header.trusted".to_string(), trusted),
    ];
    crate::emulator::set_settings(server, &settings, "websocket whitelist and visitor IP", log, "").await
}

// The whitelist with the host added (compared ignoring case); "*" stays "*".
fn merge_whitelist(current: &str, host: &str) -> String {
    if current == "*" {
        return "*".into();
    }
    let mut hosts: Vec<String> = current.split(',').map(str::trim).filter(|h| !h.is_empty()).map(str::to_string).collect();
    if !hosts.iter().any(|h| h.to_lowercase() == host.to_lowercase()) {
        hosts.push(host.to_string());
    }
    hosts.join(",")
}

const TEST: &str = r#"url="{{Scheme}}://{{name}}"
# --noproxy because these requests are for this server itself, never for a proxy set in the environment.
c() { curl -s --noproxy '*' --max-time 10 --resolve "{{name}}:{{port}}:127.0.0.1" --resolve "{{name}}:80:127.0.0.1" "$@"; }
{{ipOverride}}

cert=no
if [ "{{Scheme}}" = https ]; then
  echo "> Waiting for the Let's Encrypt certificate for {{name}} (up to 2 minutes)"
  for i in $(seq 1 24); do
    if c -o /dev/null "$url/"; then cert=yes; echo "  certificate is valid after $((i * 5)) seconds"; break; fi
    if docker logs traefik 2>&1 | grep -qiE 'unable to obtain ACME certificate|acme: error'; then break; fi
    sleep 5
  done
  if [ $cert = no ]; then
    problem=$(docker logs traefik 2>&1 | grep -iE 'acme|certificate' | grep -iE 'error|unable' | tail -1 \
      | sed 's/.*error="//; s/" [A-Za-z][A-Za-z ]*=.*//; s/\\"/"/g' | cut -c1-300)
    # No apostrophe inside ${...:-...}, because bash would take it as an open quote.
    [ -n "$problem" ] || problem="no answer from Let's Encrypt yet"
    echo "CERTPROBLEM=$problem"
    echo "  No valid certificate yet: $problem"
  fi
  echo "CERT=$cert"
  k=-k
else
  k=
fi

echo "> $url/ (NGINX on port {{NginxPort}})"
code=$(c $k -o /dev/null -w '%{http_code}' "$url/" || true)
echo "  HTTP $code"
# AtomCMS answers 302 on a new install to send visitors to its installer.
case "$code" in 200|302|403) ;; *) fail "Traefik does not pass $url/ to NGINX (HTTP '$code'). See: docker logs traefik" ;; esac
if [ "{{Scheme}}" = https ]; then
  code=$(c -o /dev/null -w '%{http_code}' "http://{{name}}/" || true)
  case "$code" in
    301|302|307|308) echo "  http://{{name}}/ redirects to https:// (HTTP $code)" ;;
    *) echo "  http://{{name}}/ answers HTTP $code (expected a redirect to https://)" ;;
  esac
fi

echo "> $url/api/health (Polaris on port {{PolarisPort}})"
body=$(c $k -w ' HTTP %{http_code}' "$url/api/health" || true)
echo "  $body"
echo "$body" | grep -q 'HTTP 200' && echo "API=yes"

echo "> $url/ws (websocket upgrade, Origin {{origin}})"
status=$(c $k --http1.1 -o /dev/null -w '%{http_code}' --max-time 5 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: SGVsbG8sIGhvdGVsIHRlc3Qh' -H 'Origin: {{origin}}' "$url/ws" || true)
echo "  HTTP $status"
[ "$status" = 101 ] && echo "WS=yes"
true"#;

/// Checks the site, /api/health and a websocket upgrade through local Traefik. With https, waits up to 2 minutes for
/// the Let's Encrypt certificate.
pub async fn test(server: &dyn Server, settings: &WebAccessSettings, log: Log<'_>) -> Result<WebAccessResult> {
    let name = if settings.is_ip_address() { "hotel.invalid".to_string() } else { settings.address.clone() };
    let ip_override = if settings.is_ip_address() {
        format!("c() {{ curl -s --noproxy '*' --max-time 10 -k \"$@\"; }}; url=\"{}://127.0.0.1\"", settings.scheme())
    } else {
        String::new()
    };
    let body = fill(
        TEST,
        &[
            ("Scheme", settings.scheme()),
            ("name", &name),
            ("port", if settings.https { "443" } else { "80" }),
            ("ipOverride", &ip_override),
            ("NginxPort", NGINX_PORT),
            ("PolarisPort", POLARIS_PORT),
            ("origin", &settings.url()),
        ],
    );
    let result = script::run_capturing(
        server,
        &script::with_prelude(&body),
        log,
        "Testing the web access failed",
        &["CERT", "CERTPROBLEM", "API", "WS"],
        None,
    )
    .await?;
    Ok(WebAccessResult {
        certificate_valid: result.yes("CERT"),
        certificate_problem: result.string("CERTPROBLEM"),
        api_answered: result.yes("API"),
        web_socket_answered: result.yes("WS"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn www_alias_only_for_a_domain_of_its_own() {
        let alias = |address: &str| WebAccessSettings::new(address, true, false).www_alias();
        assert_eq!(alias("example.com").as_deref(), Some("www.example.com"));
        assert_eq!(alias("Example.COM").as_deref(), Some("www.example.com"));
        assert_eq!(alias("www.example.com").as_deref(), Some("example.com"));
        assert_eq!(alias("example.co.uk").as_deref(), Some("www.example.co.uk"));
        assert_eq!(alias("www.example.com.br").as_deref(), Some("example.com.br"));
        for only in ["test.example.com", "www.test.example.com", "hotel.example.co.uk", "localhost", "hotel.localhost", "hotel.local",
            "10.10.10.1", "2001:db8::1", "example"]
        {
            assert_eq!(alias(only), None, "{only}");
        }
    }

    #[test]
    fn www_route_redirects_to_the_address() {
        let script = www_route_script(&WebAccessSettings::new("example.com", true, true), "websecure", "\n      tls:\n        certResolver: letsencrypt");
        assert!(script.contains("www=www.example.com"));
        assert!(script.contains("rule: \"Host(`www.example.com`)\""));
        assert!(script.contains("replacement: \"https://example.com${1}\""));
        assert!(script.contains("certResolver: letsencrypt"));
        assert!(script.contains("[ yes = yes ]"), "Cloudflare: any record of www is fine");
        let plain = www_route_script(&WebAccessSettings::new("www.example.com", false, false), "web", "");
        assert!(plain.contains("Host(`example.com`)") && plain.contains("http://www.example.com${1}"));
        assert!(!plain.contains("CNAME"), "the bare domain cannot be a CNAME");
        assert_eq!(www_route_script(&WebAccessSettings::new("test.example.com", true, false), "websecure", ""), "\nrm -f \"$t/dynamic/hotel-www.yml\"");
    }

    #[test]
    fn state_and_whitelist() {
        let state = parse_state(&ScriptValues::parse(
            "docker=5:29.9.2-1~ubuntu.24.04~noble\ndockerorg=yes\nconflicts=docker.io runc \ndockerrunning=yes\ntraefik=true\nsite=yes\n\
             conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\nconf_CLOUDFLARE=yes\nconf_CMS=custom\nconf_CMS_NAME=mycms\nconf_CMS_PORT=87\n\
             port80=docker-proxy\nport443=\nemulator=yes\n",
        ));
        assert_eq!(state.docker_version.as_deref(), Some("29.9.2"));
        assert_eq!(state.conflicting_packages, vec!["docker.io", "runc"]);
        assert!(state.is_complete());
        let site = state.configured.unwrap();
        assert_eq!(site.custom_cms.as_deref(), Some("mycms"));
        assert_eq!(site.cms_port, 87);
        assert!(site.cloudflare && site.https);
        assert_eq!(state.port443_process, None);
        assert!(!parse_state(&ScriptValues::parse("docker=29.1-1\n")).is_complete());
        assert_eq!(parse_state(&ScriptValues::parse("docker=29.1-1\n")).docker_version.as_deref(), Some("29.1"));

        assert_eq!(merge_whitelist("*", "a.com"), "*");
        assert_eq!(merge_whitelist("", "a.com"), "a.com");
        assert_eq!(merge_whitelist(" b.com, A.com ,", "a.com"), "b.com,A.com");
        assert_eq!(merge_whitelist("b.com", "a.com"), "b.com,a.com");
    }
}
