//! The ufw firewall, Polaris' ws.whitelist / ws.ip.header, the real-IP setup and the sysctl SYN flood settings.
//! (HardenInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use crate::web_access::WebAccessSettings;

/// The hotel's address without "www." (Polaris drops it itself).
pub fn whitelist_host(site: &WebAccessSettings) -> String {
    if site.address.len() >= 4 && site.address.is_char_boundary(4) && site.address[..4].eq_ignore_ascii_case("www.") {
        site.address[4..].to_string()
    } else {
        site.address.clone()
    }
}

// ---------------------------------------------------------------------------------------------------------------

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server};
use crate::{emulator, hotel_site, web_access};

/// Hardening state before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardenState {
    pub site: Option<WebAccessSettings>,
    pub ufw_active: bool,
    pub deny_incoming: bool,
    pub ssh_ports: Vec<i32>,
    pub maria_db_open: bool,
    pub sysctl_configured: bool,
    pub whitelist: Option<String>,
    pub ip_header: Option<String>,
    pub firewall_log_separate: bool,
    pub trusted_proxies: Option<String>,
    pub nginx_real_ip_cloudflare: bool,
}

impl HardenState {
    /// Polaris strips "www." itself, so the bare address is enough.
    pub fn expected_whitelist(&self) -> Option<String> {
        self.site.as_ref().map(whitelist_host)
    }

    pub fn is_complete(&self) -> bool {
        self.site.is_some()
            && self.ufw_active
            && self.deny_incoming
            && !self.maria_db_open
            && self.sysctl_configured
            && self.whitelist == self.expected_whitelist()
            && self.ip_header.as_deref() == Some("X-Forwarded-For")
            && self.firewall_log_separate
            && self.visitor_ips_readable()
    }

    /// Behind Cloudflare, Polaris and NGINX must trust Cloudflare's ranges to see the visitor instead of Cloudflare.
    pub fn visitor_ips_readable(&self) -> bool {
        !self.site.as_ref().is_some_and(|s| s.cloudflare)
            || (self.trusted_proxies.as_deref().is_some_and(|t| !t.is_empty()) && self.nginx_real_ip_cloudflare)
    }
}

/// Once present, other steps stop opening ports in ufw. ufw_allow in the prelude checks the same path.
pub const MARKER: &str = "/etc/ufw/all-in-1-hardened";

/// rsyslog's rule that writes the firewall's messages to /var/log/ufw.log.
pub const UFW_RSYSLOG: &str = "/etc/rsyslog.d/20-ufw.conf";
pub const UFW_LOG: &str = "/var/log/ufw.log";

// ufw logging on, and "& stop" in rsyslog's ufw rule, so blocked connections only go to /var/log/ufw.log instead of
// also filling kern.log and syslog. Restarts rsyslog. Without rsyslog the messages stay in the journal.
const FIREWALL_LOG_SCRIPT: &str = r#"echo "> Firewall log: {{UfwLog}}"
if ufw status verbose 2>/dev/null | grep -q '^Logging: off'; then ufw logging low >/dev/null; echo "  ufw logging switched on (low)"; fi
f={{UfwRsyslog}}
if [ -f "$f" ]; then
  if grep -qE '^[[:space:]]*#[[:space:]]*&[[:space:]]*stop' "$f"; then
    sed -i -E 's/^[[:space:]]*#[[:space:]]*(&[[:space:]]*stop)/\1/' "$f"
    echo "  $f: '& stop' switched on: the firewall's messages go only to {{UfwLog}}, not also to kern.log and syslog"
  elif grep -qE '^[[:space:]]*&[[:space:]]*stop' "$f"; then
    echo "  $f: '& stop' is already on"
  else
    printf '& stop\n' >> "$f"
    echo "  $f: '& stop' added"
  fi
  if { [ -d /run/systemd/system ] && systemctl restart rsyslog; } || service rsyslog restart >/dev/null 2>&1; then
    echo "  rsyslog restarted"
  else
    echo "  rsyslog could not be restarted: run 'sudo service rsyslog restart' yourself"
  fi
else
  echo "  No $f (rsyslog is not installed): the firewall's messages are in the journal (journalctl -k | grep UFW)"
fi"#;

fn firewall_log_script() -> String {
    fill(FIREWALL_LOG_SCRIPT, &[("UfwLog", UFW_LOG), ("UfwRsyslog", UFW_RSYSLOG)])
}

pub const SYSCTL: [(&str, &str); 3] =
    [("net.ipv4.tcp_syncookies", "1"), ("net.ipv4.tcp_max_syn_backlog", "2048"), ("net.ipv4.tcp_synack_retries", "3")];

const INSPECT: &str = r#"[ -f {{TraefikDirectory}}/hotel.env ] && sed 's/^/conf_/' {{TraefikDirectory}}/hotel.env
status=$(ufw status verbose 2>/dev/null)
echo "active=$(echo "$status" | grep -q '^Status: active' && echo yes)"
echo "deny=$(echo "$status" | grep -q 'deny (incoming)' && echo yes)"
# sshd's listeners plus its configured ports. On Ubuntu 24.04+ systemd holds the socket through ssh.socket.
echo "ssh=$( (ss -Hltnp 2>/dev/null | grep -E '"sshd"|ssh.socket' | awk '{print $4}' | sed 's/.*://'; sshd -T 2>/dev/null | awk '$1=="port" {print $2}') | sort -un | tr '\n' ' ')"
echo "mariadb=$(echo "$status" | grep -qE '^3306[/ ].*ALLOW' && echo open)"
echo "sysctl=$({{sysctlCheck}} && echo yes)"
# The firewall's messages only in /var/log/ufw.log: "& stop" is on in rsyslog's ufw file (no file, no rsyslog: fine).
f={{UfwRsyslog}}
echo "ufwlog=$([ ! -f "$f" ] || grep -qE '^[[:space:]]*&[[:space:]]*stop' "$f" && echo yes)"
# NGINX trusts more than 127.0.0.1 (Cloudflare's ranges) in the hotel site's real-IP file.
echo "realipcf=$([ "$(grep -c '^set_real_ip_from' {{RealIpFile}} 2>/dev/null)" -gt 1 ] && grep -q 'include {{RealIpFile}}' {{HotelSiteFile}} 2>/dev/null && echo yes)"
db=$(grep -sE '^db.database=' {{EmulatorDirectory}}/config.ini | cut -d= -f2)
if [ -n "$db" ]; then
  echo "whitelist=$(mariadb -N "$db" -e "SELECT \`value\` FROM emulator_settings WHERE \`key\` = 'ws.whitelist'" 2>/dev/null)"
  echo "ipheader=$(mariadb -N "$db" -e "SELECT \`value\` FROM emulator_settings WHERE \`key\` = 'ws.ip.header'" 2>/dev/null)"
  echo "trusted=$(mariadb -N "$db" -e "SELECT \`value\` FROM emulator_settings WHERE \`key\` = 'ws.ip.header.trusted'" 2>/dev/null)"
fi"#;

/// The SSH ports from a space separated list; anything that is not a positive number is dropped.
pub(crate) fn ports_from(text: Option<&str>) -> Vec<i32> {
    text.unwrap_or("").split(' ').filter(|p| !p.is_empty()).map(|p| p.parse::<i32>().unwrap_or(0)).filter(|&n| n > 0).collect()
}

/// `ports` plus this session's port, without duplicates, in order, space separated.
fn port_list(server: &dyn Server, ports: &[i32]) -> String {
    let mut all: Vec<i32> = ports.to_vec();
    all.push(i32::from(server.login().port));
    all.sort_unstable();
    all.dedup();
    all.iter().map(i32::to_string).collect::<Vec<_>>().join(" ")
}

pub async fn inspect(server: &dyn Server) -> Result<HardenState> {
    let sysctl_check = SYSCTL
        .iter()
        .map(|(key, value)| format!("grep -Eq '^[[:space:]]*{key}[[:space:]]*=[[:space:]]*{value}[[:space:]]*$' /etc/sysctl.conf"))
        .collect::<Vec<_>>()
        .join(" && ");
    let emulator_directory = emulator::directory();
    let script = fill(
        INSPECT,
        &[
            ("TraefikDirectory", web_access::TRAEFIK_DIRECTORY),
            ("sysctlCheck", &sysctl_check),
            ("UfwRsyslog", UFW_RSYSLOG),
            ("RealIpFile", hotel_site::REAL_IP_FILE),
            ("HotelSiteFile", hotel_site::FILE),
            ("EmulatorDirectory", &emulator_directory),
        ],
    );
    let result = server.run_as_root(&script).await?;
    Ok(state_from(&ScriptValues::parse(&result.output)))
}

fn state_from(values: &ScriptValues) -> HardenState {
    HardenState {
        site: WebAccessSettings::from_hotel_env(values),
        ufw_active: values.yes("active"),
        deny_incoming: values.yes("deny"),
        ssh_ports: ports_from(values.get("ssh")),
        maria_db_open: values.get("mariadb") == Some("open"),
        sysctl_configured: values.yes("sysctl"),
        whitelist: values.string("whitelist"),
        ip_header: values.string("ipheader"),
        firewall_log_separate: values.yes("ufwlog"),
        trusted_proxies: values.string("trusted"),
        nginx_real_ip_cloudflare: values.yes("realipcf"),
    }
}

const CONFIGURE_FIREWALL: &str = r#"if ! command -v ufw >/dev/null 2>&1; then
  echo "> Installing ufw"
  apt_update >/dev/null
  apt-get install -y ufw 2>&1 | grep -v 'Reading database' || true
  command -v ufw >/dev/null 2>&1 || fail "ufw could not be installed (see above)."
fi
# ufw only takes IPv6 rules when IPv6 is on in /etc/default/ufw and on the server.
ipv6=yes
grep -qi '^IPV6=yes' /etc/default/ufw 2>/dev/null && [ -f /proc/net/if_inet6 ] || ipv6=

# Download Cloudflare's ranges before anything changes, so a failed download changes nothing.
ranges=
if [ {{cloudflare}} = yes ]; then
  load_cloudflare_ranges "the firewall was not changed"
  [ -n "$ipv6" ] || { ranges=$(echo "$ranges" | grep -v ':'); echo "  IPv6 is off: only the IPv4 ranges are used"; }
fi

echo "> ufw: starting from a clean rule set (ufw keeps the old rules as /etc/ufw/*.rules.<date>)"
ufw --force reset >/dev/null
ufw default deny incoming
ufw default allow outgoing
for port in {{ports}}; do
  ufw limit "$port/tcp" comment 'Rate limit for openssh server'
done

if [ -n "$ranges" ]; then
  echo "> HTTP/HTTPS only from Cloudflare"
  for range in $ranges; do
    ufw allow proto tcp from "$range" to any port 80,443 comment 'Cloudflare' >/dev/null
  done
  echo "  $(echo "$ranges" | grep -c .) Cloudflare ranges may reach ports 80 and 443"
else
  ufw allow 80/tcp comment 'HTTP (Traefik)'
  ufw allow 443/tcp comment 'HTTPS (Traefik)'
fi

echo "  MariaDB (3306) and the emulator's ports stay closed (use an SSH tunnel for HeidiSQL)"

echo "> ufw enable"
ufw --force enable
printf 'Firewall set up by the All-in-1 Manager harden step on %s.\nThe other steps no longer open ports; run the harden step again to change the rules.\n' "$(date)" > {{Marker}}
ufw status verbose | sed 's/^/  /' | grep -v '^  $' | grep -v 'Cloudflare' || true
[ -z "$ranges" ] || echo "  (+ $(ufw status | grep -c Cloudflare) Cloudflare rules for 80,443)"
{{FirewallLogScript}}"#;

/// Resets ufw, which backs up the old rules in /etc/ufw. Rate-limits SSH on every sshd port plus this session's,
/// and opens HTTP/HTTPS to everyone or only Cloudflare. MariaDB and the emulator ports stay closed.
pub async fn configure_firewall(server: &dyn Server, site: &WebAccessSettings, ssh_ports: &[i32], log: Log<'_>) -> Result<()> {
    let ports = port_list(server, ssh_ports);
    let body = fill(
        CONFIGURE_FIREWALL,
        &[
            ("cloudflare", if site.cloudflare { "yes" } else { "no" }),
            ("ports", &ports),
            ("Marker", MARKER),
            ("FirewallLogScript", &firewall_log_script()),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Setting up the firewall failed", None).await
}

const ENABLE_CHECK: &str = r#"command -v ufw >/dev/null 2>&1 && echo "ufw=yes"
[ -f {{Marker}} ] && echo "marker=yes"
echo "rules=$(ufw show added 2>/dev/null | grep -c '^ufw ')""#;

const ENABLE_FIREWALL: &str = r#"# Never lock out SSH. Every sshd port and this session's must be in the rules before ufw is switched on.
for port in {{ports}}; do
  if ! ufw show added | grep -Eq "^ufw (limit|allow) ($port|$port/tcp)( |$)"; then
    echo "> SSH port $port was not in the rules: adding it (rate limited)"
    ufw limit "$port/tcp" comment 'Rate limit for openssh server'
  fi
done
echo "> ufw enable (the rules of the harden step)"
ufw --force enable
ufw status verbose | sed 's/^/  /' | grep -v '^  $' | grep -v 'Cloudflare' || true
n=$(ufw status | grep -c Cloudflare); [ "$n" -eq 0 ] || echo "  (+ $n Cloudflare rules for 80,443)""#;

/// Tools → Enable firewall. If the harden step's rules exist they are switched on again, after making sure every
/// SSH port including this session's is still allowed. Otherwise the harden step's rule set is written.
/// Returns true when the full rule set was written.
pub async fn enable_firewall(server: &dyn Server, log: Log<'_>) -> Result<bool> {
    let state = inspect(server).await?;
    let check = fill(ENABLE_CHECK, &[("Marker", MARKER)]);
    let values = ScriptValues::parse(&server.run_as_root(&check).await?.output);
    if !values.yes("ufw") || !values.yes("marker") || values.int("rules") == 0 {
        log("> No firewall rules from the harden step yet: setting them up (the same rules as the Harden step)");
        let site = state.site.clone().unwrap_or_else(|| WebAccessSettings::new("", false, false));
        configure_firewall(server, &site, &state.ssh_ports, log).await?;
        return Ok(true);
    }

    let ports = port_list(server, &state.ssh_ports);
    let body = fill(ENABLE_FIREWALL, &[("ports", &ports)]);
    script::run(server, &script::with_prelude(&body), log, "Switching on the firewall failed", None).await?;
    Ok(false)
}

const DISABLE_FIREWALL: &str = r#"if ! command -v ufw >/dev/null 2>&1; then
  echo "  ufw is not installed: there is no firewall to switch off"
  exit 0
fi
echo "> ufw disable"
ufw disable
echo "  $(ufw show added 2>/dev/null | grep -c '^ufw ') rules are kept for Enable firewall"
ufw status | sed 's/^/  /'"#;

/// Tools → Disable firewall. The rules are kept, so Enable firewall switches the same rules on again.
pub async fn disable_firewall(server: &dyn Server, log: Log<'_>) -> Result<()> {
    script::run(server, &script::with_prelude(DISABLE_FIREWALL), log, "Switching off the firewall failed", None).await
}

const SYSCTL_LINE: &str = r#"if grep -Eq '^[[:space:]]*#?[[:space:]]*{{Key}}[[:space:]]*=' /etc/sysctl.conf; then
  sed -i -E 's|^[[:space:]]*#?[[:space:]]*{{Key}}[[:space:]]*=.*|{{Key}}={{Value}}|' /etc/sysctl.conf
else
  [ -n "$added" ] || printf '\n# SYN flood protection (All-in-1 Manager)\n' >> /etc/sysctl.conf
  added=yes
  echo '{{Key}}={{Value}}' >> /etc/sysctl.conf
fi
if sysctl -w '{{Key}}={{Value}}' >/dev/null 2>&1; then echo "  {{Key}} = $(sysctl -n {{Key}})"; else echo "  {{Key}}={{Value}} is in /etc/sysctl.conf (this machine does not allow changing it now)"; fi"#;

const CONFIGURE_SYSCTL: &str = r#"echo "> /etc/sysctl.conf"
touch /etc/sysctl.conf
cp /etc/sysctl.conf "/etc/sysctl.conf.bak-$(date +%Y%m%d-%H%M%S)"
added=
{{lines}}"#;

/// Replaces existing lines for these keys in /etc/sysctl.conf and applies them now.
pub async fn configure_sysctl(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let lines = SYSCTL.iter().map(|(key, value)| fill(SYSCTL_LINE, &[("Key", key), ("Value", value)])).collect::<Vec<_>>().join("\n");
    let body = fill(CONFIGURE_SYSCTL, &[("lines", &lines)]);
    script::run(server, &script::with_prelude(&body), log, "Changing the TCP settings failed", None).await
}

/// Restricts ws.whitelist to the hotel and trusts X-Forwarded-For, which Traefik sets; behind Cloudflare also
/// Cloudflare's ranges, so Polaris reads past the Cloudflare hop to the visitor.
pub async fn configure_polaris(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<bool> {
    let host = whitelist_host(site);
    let trusted = web_access::polaris_trusted_proxies(server, site).await?;
    let settings = [
        ("ws.whitelist".to_string(), host.clone()),
        ("websockets.whitelist".to_string(), host),
        ("ws.ip.header".to_string(), "X-Forwarded-For".to_string()),
        ("ws.ip.header.trusted".to_string(), trusted),
    ];
    emulator::set_settings(server, &settings, "websocket whitelist and visitor IP", log, "").await
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "conf_ADDRESS=www.hotel.example.com\nconf_SCHEME=https\nconf_CLOUDFLARE=yes\nactive=yes\ndeny=yes\n\
ssh=22 2222 abc 0 \nmariadb=\nsysctl=yes\nufwlog=yes\nrealipcf=yes\nwhitelist=hotel.example.com\nipheader=X-Forwarded-For\n\
trusted=173.245.48.0/20\n";

    #[test]
    fn parses_the_state() {
        let state = state_from(&ScriptValues::parse(OUTPUT));
        assert_eq!(state.ssh_ports, vec![22, 2222]);
        assert_eq!(state.expected_whitelist().as_deref(), Some("hotel.example.com"));
        assert!(state.site.as_ref().unwrap().cloudflare);
        assert!(state.visitor_ips_readable());
        assert!(state.is_complete());
    }

    #[test]
    fn incomplete_states() {
        let base = state_from(&ScriptValues::parse(OUTPUT));
        assert!(!HardenState { nginx_real_ip_cloudflare: false, ..base.clone() }.is_complete());
        assert!(!HardenState { trusted_proxies: None, ..base.clone() }.visitor_ips_readable());
        assert!(!HardenState { maria_db_open: true, ..base.clone() }.is_complete());
        assert!(!HardenState { ip_header: None, ..base.clone() }.is_complete());
        assert!(!HardenState { whitelist: Some("www.hotel.example.com".into()), ..base.clone() }.is_complete());
        let none = state_from(&ScriptValues::parse("active=yes"));
        assert_eq!(none.site, None);
        assert_eq!(none.expected_whitelist(), None);
        assert!(none.visitor_ips_readable());
        assert!(!none.is_complete());
        // Without Cloudflare the trusted proxies do not matter.
        let plain = state_from(&ScriptValues::parse(&OUTPUT.replace("conf_CLOUDFLARE=yes", "conf_CLOUDFLARE=no").replace("realipcf=yes", "")));
        assert!(plain.visitor_ips_readable());
        assert!(plain.is_complete());
    }
}
