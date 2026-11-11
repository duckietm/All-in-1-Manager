//! Gitea in Docker behind Traefik: DNS check, compose, route, admin and test. (GiteaInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub const CONTAINER: &str = "gitea";
pub const IMAGE: &str = "gitea/gitea:1";

// ---------------------------------------------------------------------------------------------------------------
//
// Gitea (gitea.com) in Docker behind the hotel's Traefik, on its own domain such as git.example.com, following
// docs.gitea.com/administration/reverse-proxies. SQLite, its SSH server unpublished (clone over https), its web port on
// 127.0.0.1 on the first free port from 85 to 95, and a Let's Encrypt certificate through Traefik.

use std::sync::OnceLock;

use regex::Regex;

use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::{self, WebAccessSettings};
use crate::{local_ports, server_tools};

/// What the Gitea addon asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiteaSettings {
    pub domain: String,
    pub https: bool,
    pub cloudflare: bool,
    pub admin_user: String,
    pub admin_email: String,
}

impl GiteaSettings {
    pub fn url(&self) -> String {
        format!("{}://{}", if self.https { "https" } else { "http" }, self.domain)
    }
}

/// What the server has for Gitea before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiteaState {
    pub site: Option<WebAccessSettings>,
    pub docker_running: bool,
    pub traefik_running: bool,
    pub domain: Option<String>,
    pub cloudflare: Option<bool>,
    pub port: i64,
    pub running: bool,
    pub version: Option<String>,
}

impl GiteaState {
    pub fn installed(&self) -> bool {
        self.domain.is_some() && self.running
    }
}

/// The Gitea addon's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiteaResult {
    pub port: i64,
    pub admin_password: Option<String>,
    pub admin_existed: bool,
    pub certificate_valid: bool,
}

/// Gitea's folder (Settings → Server layout).
pub fn directory() -> &'static str {
    &aio_core::ServerLayout::current().gitea
}
/// `web_access::traefik_directory()` + "/dynamic/gitea.yml".
pub fn route_file() -> &'static str {
    static FILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FILE.get_or_init(|| format!("{}/dynamic/gitea.yml", web_access::traefik_directory()))
}

// Names Gitea keeps for itself (models/user: reservedUsernames), the ones someone would pick.
const RESERVED: [&str; 15] =
    ["admin", "api", "assets", "avatar", "explore", "ghost", "issues", "login", "new", "notifications", "org", "pulls", "repo", "search", "user"];

// The C# pattern starts with the lookahead (?=.{4,253}$), which the regex crate lacks: the length is checked apart.
fn domain_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^([A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?\\.)+[A-Za-z]{2,63}$").unwrap())
}

fn user_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[A-Za-z0-9][A-Za-z0-9._-]{0,39}$").unwrap())
}

fn email_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$").unwrap())
}

pub fn domain_problem(domain: &str, site: Option<&WebAccessSettings>) -> Option<String> {
    let length = domain.chars().count();
    if !(4..=253).contains(&length) || !domain_pattern().is_match(domain) {
        return Some("Enter a domain name such as git.example.com (letters, digits, '-' and dots).".into());
    }
    if let Some(site) = site.filter(|s| domain.eq_ignore_ascii_case(&s.address)) {
        return Some(format!("{domain} is the hotel's address: Gitea needs a name of its own, such as git.{}.", site.address));
    }
    None
}

pub fn user_problem(user: &str) -> Option<String> {
    if !user_pattern().is_match(user) {
        Some("The admin name: letters, digits, '-', '_' and '.', starting with a letter or digit, at most 40.".into())
    } else if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(user)) {
        Some(format!("Gitea keeps the name \"{user}\" for itself: choose another, such as gitadmin."))
    } else {
        None
    }
}

pub fn email_problem(email: &str) -> Option<String> {
    if email_pattern().is_match(email) {
        None
    } else {
        Some("Enter the admin's e-mail address.".into())
    }
}

/// git.example.com for a hotel on example.com or www.example.com; nothing for a hotel on an IP address.
pub fn suggested_domain(site: Option<&WebAccessSettings>) -> String {
    match site {
        Some(site) if !site.is_ip_address() => format!("git.{}", site.address.strip_prefix("www.").unwrap_or(&site.address)),
        _ => String::new(),
    }
}

const INSPECT: &str = r#"[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
echo "docker=$(docker info >/dev/null 2>&1 && echo yes)"
echo "traefik=$(docker inspect -f '{{.State.Running}}' traefik 2>/dev/null)"
[ -f {{Directory}}/.env ] && sed -n 's/^\(DOMAIN\|CLOUDFLARE\|PORT\)=/gitea_\1=/p' {{Directory}}/.env
echo "running=$(docker inspect -f '{{.State.Running}}' {{Container}} 2>/dev/null)"
echo "version=$(docker exec {{Container}} gitea --version 2>/dev/null | awk '{print $3}')""#;

pub async fn inspect(server: &dyn Server) -> Result<GiteaState> {
    let script = fill(INSPECT, &[("TraefikDirectory", web_access::traefik_directory()), ("Directory", directory()), ("Container", CONTAINER)]);
    let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
    Ok(GiteaState {
        site: WebAccessSettings::from_hotel_env(&values),
        docker_running: values.yes("docker"),
        traefik_running: values.get("traefik") == Some("true"),
        domain: values.string("gitea_DOMAIN"),
        cloudflare: values.get("gitea_CLOUDFLARE").map(|cf| cf == "yes"),
        port: values.int("gitea_PORT"),
        running: values.get("running") == Some("true"),
        version: values.string("version"),
    })
}

const CHECK_DNS: &str = r#"d={{Domain}}
echo "> DNS: $d"
ips=$(getent ahostsv4 "$d" 2>/dev/null | awk '{print $1}' | sort -u | paste -sd' ' -)
[ -n "$ips" ] || fail "$d has no address yet. Add an A record for $d with this server's IP address in your DNS{{orange}}, wait a minute and try again."
echo "  $d points to $ips"
# In a subshell: without Cloudflare's list (no internet) the check below just has no ranges.
ranges=$(load_cloudflare_ranges >/dev/null 2>&1 && printf '%s' "$ranges") || ranges=
behind=$(IPS="$ips" RANGES="$ranges" python3 -c 'import ipaddress, os
nets = [ipaddress.ip_network(r) for r in os.environ["RANGES"].split()]
print("yes" if nets and all(any(ipaddress.ip_address(i) in n for n in nets) for i in os.environ["IPS"].split()) else "no")' 2>/dev/null)
if [ {{cloudflare}} = yes ]; then
  if [ "$behind" = yes ]; then echo "  Cloudflare's proxy is on (the address is Cloudflare's): good"
  elif [ -n "$ranges" ]; then echo "  WARNING: $d does not point to Cloudflare: turn its proxy on (orange cloud) in Cloudflare's DNS, or choose No for Cloudflare."; fi
else
  [ "$behind" = yes ] && echo "  WARNING: $d points to Cloudflare (proxy on), but Cloudflare is set to No: the visitors' addresses will be Cloudflare's."
  echo "  This server's addresses: $(hostname -I 2>/dev/null) (behind a router or NAT, the A record has the public one)"
fi
true"#;

/// The domain must point at the server before Let's Encrypt can give it a certificate. Fails without an address; warns
/// when Cloudflare's proxy is off although Cloudflare was chosen, or on although it was not. Runs on the server
/// (getent there, and the prelude's Cloudflare ranges).
pub async fn check_dns(server: &dyn Server, settings: &GiteaSettings, log: Log<'_>) -> Result<()> {
    let body = fill(
        CHECK_DNS,
        &[
            ("Domain", &settings.domain),
            ("orange", if settings.cloudflare { " (in Cloudflare: proxy on, the orange cloud)" } else { "" }),
            ("cloudflare", if settings.cloudflare { "yes" } else { "no" }),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Checking the domain failed", None).await
}

const INSTALL: &str = r#"set -o pipefail
g={{Directory}}
docker info >/dev/null 2>&1 || fail "Docker does not run. Run the Web access step first."
[ -d {{TraefikDirectory}}/dynamic ] || fail "Traefik is not set up. Run the Web access step first."
install -d -m 755 "$g"
install -d -m 750 -o 1000 -g 1000 "$g/data"

{{LocalPortsFunctions}}
self_container={{Container}}
pick_port "$(sed -n 's/^PORT=//p' "$g/.env" 2>/dev/null)"
echo "> Gitea on 127.0.0.1:$port"

# Gitea's own proxies: 127.0.0.1 and, once its network exists, the Docker network Traefik reaches it through.
trusted=$(sed -n 's/^TRUSTED=//p' "$g/.env" 2>/dev/null) || trusted=
write_env() {
  cat > "$g/.env" <<EOF
# Gitea (written by All-in-1 Manager, Addons → Gitea).
DOMAIN={{Domain}}
ROOT_URL={{Url}}/
CLOUDFLARE={{cloudflare}}
PORT=$port
TRUSTED=${trusted:-127.0.0.0/8,::1/128}
LIMIT={{limit}}
EOF
  chmod 600 "$g/.env"
}
write_env
echo "> $g/compose.yaml"
cat > "$g/compose.yaml" <<'EOF'
# Gitea for the hotel (written by All-in-1 Manager). Update: docker compose pull && docker compose up -d
# Its own network, not the host's: the image always starts an SSH server (port 22 inside), which must not
# meet the server's SSH. Only the web port is published, on 127.0.0.1, for Traefik.
services:
  gitea:
    image: {{Image}}
    container_name: {{Container}}
    restart: unless-stopped
    networks: [gitea]
    ports:
      - "127.0.0.1:${PORT}:3000"
    environment:
      - USER_UID=1000
      - USER_GID=1000
      - GITEA__server__DOMAIN=${DOMAIN}
      - GITEA__server__ROOT_URL=${ROOT_URL}
      - GITEA__server__HTTP_PORT=3000
      - GITEA__server__DISABLE_SSH=true
      - GITEA__server__START_SSH_SERVER=false
      - GITEA__database__DB_TYPE=sqlite3
      - GITEA__security__INSTALL_LOCK=true
      - GITEA__security__REVERSE_PROXY_TRUSTED_PROXIES=${TRUSTED}
      - GITEA__security__REVERSE_PROXY_LIMIT=${LIMIT}
      # No sign-ups: the admin makes the accounts. Visitors land on the repositories.
      - GITEA__service__DISABLE_REGISTRATION=true
      - GITEA__service__SHOW_REGISTRATION_BUTTON=false
      - GITEA__server__LANDING_PAGE=/explore/repos
    volumes:
      - ./data:/data
      - /etc/timezone:/etc/timezone:ro
      - /etc/localtime:/etc/localtime:ro
networks:
  gitea:
    name: gitea
EOF

echo "> docker compose pull ({{Image}})"
pulled=
for i in 1 2 3; do
  if docker compose -f "$g/compose.yaml" pull -q 2>"$g/pull.log"; then pulled=yes; break; fi
  tail -1 "$g/pull.log" | sed 's/^/  /'
  [ $i -lt 3 ] && { echo "  Trying again in $((i * 20)) seconds ..."; sleep $((i * 20)); }
done
rm -f "$g/pull.log"
if [ -z "$pulled" ]; then
  docker image inspect {{Image}} >/dev/null 2>&1 || fail "Could not download the Gitea image from Docker Hub (see above). Try again later."
  echo "  Docker Hub did not answer: using the {{Image}} image that is already on this server"
fi
echo "> docker compose up -d"
docker compose -f "$g/compose.yaml" up -d 2>&1 | grep -v '^ *$'
subnets=$(docker network inspect gitea -f '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null | xargs | tr ' ' ',')
if [ -n "$subnets" ] && [ "$trusted" != "127.0.0.0/8,::1/128,$subnets" ]; then
  trusted="127.0.0.0/8,::1/128,$subnets"
  write_env
  echo "  Gitea trusts the proxy headers from 127.0.0.1 and its Docker network ($subnets)"
  docker compose -f "$g/compose.yaml" up -d 2>&1 | grep -v '^ *$'
fi
for i in $(seq 1 60); do
  curl -fs --noproxy '*' --max-time 3 "http://127.0.0.1:$port/api/healthz" >/dev/null 2>&1 && break
  sleep 2
done
curl -fs --noproxy '*' --max-time 3 "http://127.0.0.1:$port/api/healthz" >/dev/null 2>&1 ||
  { docker logs --tail 20 {{Container}} 2>&1 | sed 's/^/  /'; fail "Gitea does not answer on 127.0.0.1:$port (see its log above)."; }
echo "  $(docker exec {{Container}} gitea --version 2>/dev/null | awk '{print "Gitea " $3}') answers"

echo "> {{RouteFile}}: {{Url}} to Gitea"
cat > {{RouteFile}} <<EOF
# Gitea (written by All-in-1 Manager, Addons → Gitea). Traefik picks it up without a restart.
http:
  routers:
    gitea:
      rule: "Host(\`{{Domain}}\`)"
      entryPoints: [{{entryPoint}}]
      service: gitea{{middleware}}{{tls}}{{middlewares}}
  services:
    gitea:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:$port"
EOF

# The admin, once: the first run creates it with a random password; later runs keep the users.
if docker exec -u git {{Container}} gitea admin user list --admin 2>/dev/null | awk 'NR > 1' | grep -q .; then
  echo "> Gitea has an admin already: kept"
  echo "ADMIN=existed"
else
  echo "> Creating the admin {{AdminUser}}"
  out=$(docker exec -u git {{Container}} gitea admin user create --admin --username {{AdminUserQuoted}} \
    --email {{AdminEmailQuoted}} --random-password --must-change-password=false 2>&1) ||
    { echo "$out" | sed 's/^/  /'; fail "Creating the admin failed (see above)."; }
  echo "  $(echo "$out" | grep -i 'successfully created' | tail -1)"
  echo "PASSWORD=$(echo "$out" | sed -n "s/.*generated random password is '\(.*\)'.*/\1/p")"
fi
echo "PORT=$port""#;

/// Behind Cloudflare, Traefik's X-Real-Ip is Cloudflare's edge: without it Gitea takes the visitor from
/// X-Forwarded-For ("visitor, edge"), two from the end.
const REAL_IP_MIDDLEWARE: &str = "\n  middlewares:\n    gitea-real-ip:\n      headers:\n        customRequestHeaders:\n          X-Real-Ip: \"\"";

/// Writes the compose file, starts Gitea on a free port, adds the Traefik route and creates the admin once.
pub async fn install(server: &dyn Server, settings: &GiteaSettings, log: Log<'_>) -> Result<GiteaResult> {
    let entry_point = if settings.https { "websecure" } else { "web" };
    let tls = if settings.https { "\n      tls:\n        certResolver: letsencrypt" } else { "" };
    let middleware = if settings.cloudflare { "\n      middlewares: [gitea-real-ip]" } else { "" };
    let middlewares = if settings.cloudflare { REAL_IP_MIDDLEWARE } else { "" };
    let url = settings.url();
    // The functions first: what they contain is not filled again.
    let body = fill(INSTALL, &[("LocalPortsFunctions", "\u{0}LOCAL_PORTS\u{0}")]);
    let body = fill(
        &body,
        &[
            ("Directory", directory()),
            ("TraefikDirectory", web_access::traefik_directory()),
            ("Container", CONTAINER),
            ("Domain", &settings.domain),
            ("Url", &url),
            ("cloudflare", if settings.cloudflare { "yes" } else { "no" }),
            ("limit", if settings.cloudflare { "2" } else { "1" }),
            ("Image", IMAGE),
            ("RouteFile", route_file()),
            ("entryPoint", entry_point),
            ("middleware", middleware),
            ("tls", tls),
            ("middlewares", middlewares),
            ("AdminUser", &settings.admin_user),
            ("AdminUserQuoted", &shell_quote(&settings.admin_user)),
            ("AdminEmailQuoted", &shell_quote(&settings.admin_email)),
        ],
    )
    .replace("\u{0}LOCAL_PORTS\u{0}", &local_ports::functions());
    let values =
        script::run_capturing(server, &script::with_prelude(&body), log, "Installing Gitea failed", &["PORT", "PASSWORD", "ADMIN"], None).await?;
    Ok(GiteaResult {
        port: values.int("PORT"),
        admin_password: values.string("PASSWORD"),
        admin_existed: values.get("ADMIN") == Some("existed"),
        certificate_valid: false,
    })
}

/// Pulls the newest gitea/gitea:1 and recreates the container when it changed; the data stays.
pub async fn update(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let version = format!("docker exec {CONTAINER} gitea --version 2>/dev/null | awk '{{print $3}}'");
    let ready = format!("curl -fs --noproxy '*' --max-time 3 \"http://127.0.0.1:$(sed -n 's/^PORT=//p' {}/.env)/api/healthz\" >/dev/null 2>&1", directory());
    server_tools::update_image(server, "Gitea", directory(), CONTAINER, &version, &ready, log).await
}

const REMOVE_CHECK: &str = r#"echo "> {{scheme}}://{{domain}}/ (through Traefik)"
# Traefik notices the removed route file and reloads a moment later (it waits about 2 seconds for more
# changes): until then it still has the route, without Gitea behind it (502).
for i in $(seq 1 20); do
  code=$(curl -s --noproxy '*' -k --max-time 5 --resolve "{{domain}}:{{port}}:127.0.0.1" -o /dev/null -w '%{http_code}' "{{scheme}}://{{domain}}/" || true)
  [ "$code" = 404 ] && break
  sleep 1
done
case "$code" in
  404) echo "  HTTP 404: Traefik has no route for it any more" ;;
  000|"") echo "  Nothing answers (Traefik does not run?)" ;;
  *) echo "  HTTP $code: something still answers for it (docker ps, and the files in {{TraefikDirectory}}/dynamic)" ;;
esac"#;

const REMOVE: &str = r#"g={{Directory}}
# The route first: while Gitea stops, visitors get Traefik's 404 instead of a 502.
if [ -f {{RouteFile}} ]; then
  rm -f {{RouteFile}}
  echo "> {{RouteFile}} removed: Traefik no longer answers for {{domain}}"
fi
if [ -f "$g/compose.yaml" ] && command -v docker >/dev/null 2>&1; then
  echo "> docker compose down (Gitea and its network)"
  docker compose -f "$g/compose.yaml" down --remove-orphans 2>&1 | grep -v '^ *$' | sed 's/^/  /' || true
fi
if docker inspect {{Container}} >/dev/null 2>&1; then docker rm -f {{Container}} >/dev/null && echo "  container {{Container}} removed"; fi
docker network rm gitea >/dev/null 2>&1 && echo "  network gitea removed" || true
if [ {{deleteData}} = yes ]; then
  if [ -d "$g" ]; then
    echo "> Deleting $g ($(du -sh "$g" 2>/dev/null | cut -f1)): the repositories, accounts and settings"
    rm -rf -- "$g"
  fi
  docker image rm {{Image}} >/dev/null 2>&1 && echo "  image {{Image}} removed" || true
elif [ -d "$g/data" ]; then
  echo "  $g stays (repositories, accounts, settings, $(du -sh "$g/data" 2>/dev/null | cut -f1)): installing Gitea again uses it"
fi
{{check}}"#;

/// Stops and removes the container, its network and the Traefik route. With `delete_data` also /opt/gitea
/// (repositories, accounts, settings) and the image; otherwise /opt/gitea stays for an install later. Checks through
/// Traefik that the domain no longer reaches Gitea.
pub async fn remove(server: &dyn Server, domain: Option<&str>, https: bool, delete_data: bool, log: Log<'_>) -> Result<()> {
    let port = if https { "443" } else { "80" };
    let scheme = if https { "https" } else { "http" };
    let check = match domain {
        None => String::new(),
        Some(domain) => fill(
            REMOVE_CHECK,
            &[("scheme", scheme), ("domain", domain), ("port", port), ("TraefikDirectory", web_access::traefik_directory())],
        ),
    };
    let body = fill(
        REMOVE,
        &[
            ("Directory", directory()),
            ("RouteFile", route_file()),
            ("domain", domain.unwrap_or("Gitea's domain")),
            ("Container", CONTAINER),
            ("deleteData", if delete_data { "yes" } else { "no" }),
            ("Image", IMAGE),
            ("check", &check),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Removing Gitea failed", None).await
}

const TEST: &str = r#"url={{Url}}
c() { curl -s --noproxy '*' --max-time 10 --resolve "{{Domain}}:{{port}}:127.0.0.1" "$@"; }
# Gitea's own answer, not only a certificate: Traefik may hold a certificate for the domain already (from an
# earlier install) before it has loaded the new route, which it does a moment after the file changes.
healthy() { echo "$1" | grep -q '"status": *"pass"'; }
echo "> $url/api/healthz through Traefik{{waiting}}"
cert=no; route=no; body=
for i in $(seq 1 60); do
  body=$(c "$url/api/healthz" 2>/dev/null || true)
  if healthy "$body"; then cert=yes; route=yes; break; fi
  {{insecure}}
  sleep 2
done
if [ $cert = yes ]; then
  echo "  Gitea is healthy after $((i * 2)) seconds{{valid}}"
elif [ $route = yes ]; then
  cert=no
  echo "  Gitea is healthy, but {{Domain}} has no valid certificate yet: check its A record, then docker logs traefik"
else
  fail "Traefik does not pass $url to Gitea: $(echo "${body:-no answer}" | head -c 200). See docker logs traefik and {{RouteFile}}."
fi
{{certYes}}
echo "CERT=$cert""#;

/// Through Traefik: waits up to 2 minutes for the certificate with https, then checks Gitea's health. Returns whether
/// the certificate is valid (always true over http).
pub async fn test(server: &dyn Server, settings: &GiteaSettings, log: Log<'_>) -> Result<bool> {
    let port = if settings.https { "443" } else { "80" };
    // With https: whether Traefik passes the route already, while the certificate is not valid yet.
    let insecure = if settings.https { "healthy \"$(c -k \"$url/api/healthz\" 2>/dev/null || true)\" && route=yes" } else { "" };
    let body = fill(
        TEST,
        &[
            ("Url", &settings.url()),
            ("Domain", &settings.domain),
            ("port", port),
            (
                "waiting",
                if settings.https { ", with a valid Let's Encrypt certificate (up to 2 minutes)" } else { " (up to 2 minutes)" },
            ),
            ("insecure", insecure),
            ("valid", if settings.https { ", and the certificate is valid" } else { "" }),
            ("RouteFile", route_file()),
            ("certYes", if settings.https { "" } else { "cert=yes" }),
        ],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Testing Gitea failed", &["CERT"], None).await?;
    Ok(values.yes("CERT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_file_is_in_traefik() {
        assert_eq!(route_file(), format!("{}/dynamic/gitea.yml", web_access::traefik_directory()));
    }

    #[test]
    fn domains() {
        assert_eq!(domain_problem("git.example.com", None), None);
        assert!(domain_problem("example", None).is_some());
        assert!(domain_problem("-a.example.com", None).is_some());
        assert!(domain_problem("a.b", None).is_some());
        let long = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        assert!(domain_problem(&long, None).is_some());
        let site = WebAccessSettings::new("Example.com", true, false);
        assert_eq!(
            domain_problem("example.com", Some(&site)),
            Some("example.com is the hotel's address: Gitea needs a name of its own, such as git.Example.com.".into())
        );
        assert_eq!(suggested_domain(Some(&WebAccessSettings::new("www.example.com", true, false))), "git.example.com");
        assert_eq!(suggested_domain(Some(&WebAccessSettings::new("192.0.2.10", false, false))), "");
        assert_eq!(suggested_domain(None), "");
    }

    #[test]
    fn users_and_emails() {
        assert_eq!(user_problem("gitadmin"), None);
        assert!(user_problem("Admin").unwrap().contains("keeps the name \"Admin\""));
        assert!(user_problem(".x").is_some());
        assert_eq!(email_problem("a@b.cd"), None);
        assert!(email_problem("a@b").is_some());
    }

    #[test]
    fn state() {
        let values = ScriptValues::parse("gitea_DOMAIN=git.example.com\ngitea_CLOUDFLARE=no\ngitea_PORT=86\nrunning=true");
        assert_eq!(values.get("gitea_CLOUDFLARE").map(|cf| cf == "yes"), Some(false));
        assert_eq!(values.int("gitea_PORT"), 86);
        let mut state = GiteaState {
            site: None,
            docker_running: true,
            traefik_running: true,
            domain: Some("git.example.com".into()),
            cloudflare: None,
            port: 86,
            running: true,
            version: None,
        };
        assert!(state.installed());
        state.running = false;
        assert!(!state.installed());
    }
}
