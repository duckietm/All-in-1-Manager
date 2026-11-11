//! Fail2Ban: sshd and NGINX jails, whitelist and bans. (Fail2BanInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

/// What is wrong with a whitelist entry (an IP address or range), or None. (EntryProblem)
///
/// An IPv4 or IPv6 address, or a network such as 192.168.1.0/24: None when valid, else the reason.
pub fn entry_problem(entry: &str) -> Option<String> {
    let parts: Vec<&str> = entry.split('/').collect();
    // .NET's IPAddress also takes short forms such as "10.1" (10.0.0.1): fail2ban does not, so IPv4 needs all four
    // parts.
    let short_ipv4 = !parts[0].contains(':') && parts[0].split('.').count() != 4;
    let address = if parts.len() > 2 || short_ipv4 { None } else { dotnet_ip::parse(parts[0]) };
    let Some(address) = address.filter(|_| !parts[0].contains('%')) else {
        return Some(format!("\"{entry}\" is not an IP address or a network such as 192.168.1.0/24."));
    };
    if parts.len() == 2 {
        let max = if address.is_ipv6() { 128 } else { 32 };
        if !dotnet_ip::parse_int(parts[1]).is_some_and(|prefix| (0..=max).contains(&prefix)) {
            return Some(format!("\"{entry}\": the part after / must be a number from 0 to {max}."));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------

use crate::fail;
use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};

/// How fail2ban protects SSH: how many failed logins within how long ban an address, for how long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail2BanSettings {
    pub max_retry: i32,
    pub find_time_minutes: i32,
    pub ban_time_minutes: i32,
    pub increment: bool,
    pub whitelist: Vec<String>,
}

/// Fail2ban on the server before a step or tool runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail2BanState {
    pub installed: bool,
    pub version: Option<String>,
    pub running: bool,
    pub configured: bool,
    pub settings: Option<Fail2BanSettings>,
    pub ssh_ports: Vec<i32>,
    pub banned: Vec<String>,
    pub total_banned: i64,
    pub client_ip: Option<String>,
}

impl Fail2BanState {
    pub fn is_complete(&self) -> bool {
        self.installed && self.running && self.configured
    }
}

pub const CONFIG_FILE: &str = "/etc/fail2ban/jail.d/all-in-1.local";
/// fail2ban's own log (logtarget in fail2ban.conf on Debian and Ubuntu): Found, Ban and Unban lines.
pub const LOG_FILE: &str = "/var/log/fail2ban.log";
pub const JAIL: &str = "sshd";
pub const MAX_WHITELIST_ENTRIES: usize = 100;

pub fn defaults() -> Fail2BanSettings {
    Fail2BanSettings { max_retry: 5, find_time_minutes: 10, ban_time_minutes: 60, increment: true, whitelist: Vec::new() }
}

/// The whitelist from text: one entry per line (or separated by spaces or commas), without duplicates. The second
/// value is the problem, if any (the entries are those read up to it).
pub fn parse_whitelist(text: &str) -> (Vec<String>, Option<String>) {
    let mut entries: Vec<String> = Vec::new();
    for line in text.split(['\n', '\r']) {
        // "# ..." is a comment, also after an entry.
        let content = line.split('#').next().unwrap_or("");
        for entry in content.split([',', ' ', '\t']).filter(|e| !e.is_empty()) {
            if let Some(problem) = entry_problem(entry) {
                return (entries, Some(problem));
            }
            if !entries.iter().any(|e| e.to_uppercase() == entry.to_uppercase()) {
                entries.push(entry.to_string());
            }
        }
    }
    if entries.len() > MAX_WHITELIST_ENTRIES {
        let problem = format!("At most {MAX_WHITELIST_ENTRIES} whitelist entries.");
        (entries, Some(problem))
    } else {
        (entries, None)
    }
}

pub fn settings_problem(settings: &Fail2BanSettings) -> Option<String> {
    if !(1..=100).contains(&settings.max_retry) {
        Some("Failed logins before a ban: a number from 1 to 100.".into())
    } else if !(1..=10080).contains(&settings.find_time_minutes) {
        Some("Counted within: from 1 minute to 7 days (10080 minutes).".into())
    } else if !(1..=525600).contains(&settings.ban_time_minutes) {
        Some("Ban time: from 1 minute to 365 days (525600 minutes).".into())
    } else {
        None
    }
}

const INSPECT: &str = r#"echo "version=$(fail2ban-client --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)"
echo "installed=$(command -v fail2ban-client >/dev/null 2>&1 && echo yes)"
echo "running=$(fail2ban-client ping 2>/dev/null | grep -q pong && echo yes)"
f={{ConfigFile}}
if [ -f "$f" ]; then
  echo "configured=yes"
  # [DEFAULT] holds the installer's values.
  get() { awk -F'=' -v k="$1" '$1 ~ "^[[:space:]]*"k"[[:space:]]*$" {sub(/^[[:space:]]+/, "", $2); sub(/[[:space:]]+$/, "", $2); print $2; exit}' "$f"; }
  echo "ignoreip=$(get ignoreip)"
  echo "maxretry=$(get maxretry)"
  echo "findtime=$(get findtime)"
  echo "bantime=$(get bantime)"
  echo "increment=$(get bantime.increment)"
fi
echo "ssh=$( (ss -Hltnp 2>/dev/null | grep -E '"sshd"|ssh.socket' | awk '{print $4}' | sed 's/.*://'; sshd -T 2>/dev/null | awk '$1=="port" {print $2}') | sort -un | tr '\n' ' ')"
if fail2ban-client status {{Jail}} >/tmp/.f2b.$$ 2>/dev/null; then
  echo "banned=$(sed -n 's/.*Banned IP list:[[:space:]]*//p' /tmp/.f2b.$$)"
  echo "total=$(sed -n 's/.*Total banned:[[:space:]]*//p' /tmp/.f2b.$$)"
fi
rm -f /tmp/.f2b.$$"#;

/// The address this computer reaches the server from ($SSH_CLIENT), when it is a valid whitelist entry.
pub(crate) async fn client_ip(server: &dyn Server) -> Option<String> {
    // Not known: the whitelist simply starts without it.
    let output = server.run("echo \"client=${SSH_CLIENT%% *}\"", None).await.ok()?.output;
    ScriptValues::parse(&output).string("client").filter(|ip| entry_problem(ip).is_none())
}

pub async fn inspect(server: &dyn Server) -> Result<Fail2BanState> {
    let script = fill(INSPECT, &[("ConfigFile", CONFIG_FILE), ("Jail", JAIL)]);
    let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
    // The address this computer reaches the server from: always whitelisted unless removed by hand.
    let client_ip = client_ip(server).await;
    Ok(state_from(&values, client_ip))
}

fn state_from(values: &ScriptValues, client_ip: Option<String>) -> Fail2BanState {
    let defaults = defaults();
    let settings = values.yes("configured").then(|| Fail2BanSettings {
        max_retry: match values.int("maxretry") {
            retry if retry > 0 => retry as i32,
            _ => defaults.max_retry,
        },
        find_time_minutes: minutes(values.get("findtime")).unwrap_or(defaults.find_time_minutes),
        ban_time_minutes: minutes(values.get("bantime")).unwrap_or(defaults.ban_time_minutes),
        increment: values.get("increment") != Some("false"),
        whitelist: values
            .or_empty("ignoreip")
            .split(' ')
            .filter(|e| !e.is_empty() && *e != "127.0.0.1/8" && *e != "::1")
            .map(str::to_string)
            .collect(),
    });
    Fail2BanState {
        installed: values.yes("installed"),
        version: values.string("version"),
        running: values.yes("running"),
        configured: values.yes("configured"),
        settings,
        ssh_ports: crate::harden::ports_from(values.get("ssh")),
        banned: values.or_empty("banned").split(' ').filter(|b| !b.is_empty()).map(str::to_string).collect(),
        total_banned: values.int("total"),
        client_ip,
    }
}

// "600" (seconds, fail2ban's unit), "10m", "1h", "2d".
fn minutes(value: Option<&str>) -> Option<i32> {
    let value = value?.trim();
    let last = value.chars().last()?;
    let letter = last.is_alphabetic();
    let unit = if letter { last.to_lowercase().next().unwrap_or(last) } else { 's' };
    let number = dotnet_ip::parse_int(if letter { &value[..value.len() - last.len_utf8()] } else { value })?;
    match unit {
        's' => Some((number / 60).max(1)),
        'm' => Some(number),
        'h' => Some(number.wrapping_mul(60)),
        'd' => Some(number.wrapping_mul(1440)),
        'w' => Some(number.wrapping_mul(10080)),
        _ => None,
    }
}

const CONFIG: &str = r#"# Written by All-in-1 Manager (Addons -> Fail2Ban; the whitelist also in Tools -> Fail2Ban).
# Changes here are overwritten by the next run of the addon or the whitelist tool.

[DEFAULT]
# Never banned: this server itself and the whitelist.
ignoreip = {{ignore}}
# {{MaxRetry}} failed logins within {{FindTimeMinutes}} minutes ban an address for {{BanTimeMinutes}} minutes.
maxretry = {{MaxRetry}}
findtime = {{FindTimeMinutes}}m
bantime = {{BanTimeMinutes}}m
# Repeat offenders are banned longer each time (up to a week).
bantime.increment = {{Increment}}
bantime.maxtime = 1w

[{{Jail}}]
enabled = true
port = {{ports}}
# sshd logs to the systemd journal (Ubuntu 24.04+ and Debian 12+ have no auth.log by default).
backend = systemd"#;

fn config(settings: &Fail2BanSettings, ssh_ports: &[i32]) -> String {
    let ignore = ["127.0.0.1/8", "::1"].into_iter().chain(settings.whitelist.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
    let ports = if ssh_ports.is_empty() { "22".to_string() } else { ssh_ports.iter().map(i32::to_string).collect::<Vec<_>>().join(",") };
    fill(
        CONFIG,
        &[
            ("ignore", &ignore),
            ("MaxRetry", &settings.max_retry.to_string()),
            ("FindTimeMinutes", &settings.find_time_minutes.to_string()),
            ("BanTimeMinutes", &settings.ban_time_minutes.to_string()),
            ("Increment", if settings.increment { "true" } else { "false" }),
            ("Jail", JAIL),
            ("ports", &ports),
        ],
    )
}

const APPLY: &str = r#"f={{ConfigFile}}
install -d -m 755 /etc/fail2ban/jail.d
[ -f "$f" ] && cp -p "$f" "$f.bak"
cat > "$f" <<'CONF'
{{config}}
CONF
chmod 644 "$f"
# Without systemd there is no journal: the log file instead (auth.log, with rsyslog).
if [ ! -d /run/systemd/system ]; then
  sed -i 's/^backend = systemd$/backend = auto/' "$f"
  touch /var/log/auth.log
fi
if ! fail2ban-client -t >/tmp/.f2b-test 2>&1; then
  sed 's/^/  /' /tmp/.f2b-test | tail -20
  if [ -f "$f.bak" ]; then mv -f "$f.bak" "$f"; else rm -f "$f"; fi
  rm -f /tmp/.f2b-test
  fail "fail2ban refuses the configuration (see above); the previous one is kept."
fi
rm -f /tmp/.f2b-test "$f.bak"
echo "> $f"
echo "  ignoreip: {{whitelist}}"
echo "  {{MaxRetry}} failed logins within {{FindTimeMinutes}} min: banned for {{BanTimeMinutes}} min{{repeat}}"
if [ -d /run/systemd/system ]; then
  systemctl enable fail2ban >/dev/null 2>&1 || true
  systemctl restart fail2ban
else
  service fail2ban restart >/dev/null 2>&1 || fail2ban-client restart >/dev/null 2>&1 || fail2ban-client start
fi
for i in $(seq 1 20); do fail2ban-client ping 2>/dev/null | grep -q pong && break; sleep 1; done
fail2ban-client ping 2>/dev/null | grep -q pong || fail "fail2ban does not start: see journalctl -u fail2ban."
fail2ban-client status {{Jail}} | sed 's/^/  /'"#;

/// The config as a script part: written, checked with fail2ban-client -t (an invalid one is put back), and fail2ban
/// restarted.
fn apply_script(settings: &Fail2BanSettings, ssh_ports: &[i32]) -> String {
    let whitelist = if settings.whitelist.is_empty() { "(only this server)".to_string() } else { settings.whitelist.join(" ") };
    fill(
        APPLY,
        &[
            ("ConfigFile", CONFIG_FILE),
            ("config", &config(settings, ssh_ports)),
            ("whitelist", &whitelist),
            ("MaxRetry", &settings.max_retry.to_string()),
            ("FindTimeMinutes", &settings.find_time_minutes.to_string()),
            ("BanTimeMinutes", &settings.ban_time_minutes.to_string()),
            ("repeat", if settings.increment { ", longer for repeat offenders" } else { "" }),
            ("Jail", JAIL),
        ],
    )
}

const INSTALL: &str = r#"if ! command -v fail2ban-client >/dev/null 2>&1 || ! python3 -c 'import systemd.journal' >/dev/null 2>&1; then
  echo "> apt-get install fail2ban python3-systemd"
  apt_update >/dev/null
  DEBIAN_FRONTEND=noninteractive apt-get install -y fail2ban python3-systemd >/dev/null
fi
if [ ! -d /run/systemd/system ] && ! command -v rsyslogd >/dev/null 2>&1; then
  echo "> apt-get install rsyslog (no systemd journal: fail2ban reads /var/log/auth.log)"
  DEBIAN_FRONTEND=noninteractive apt-get install -y rsyslog >/dev/null
fi
[ -d /run/systemd/system ] || pgrep -x rsyslogd >/dev/null || service rsyslog start >/dev/null 2>&1 || rsyslogd 2>/dev/null || true
echo "  $(fail2ban-client --version 2>/dev/null | head -1)"
{{apply}}"#;

/// Installs fail2ban (and the journal reader it needs), writes the settings and starts it at boot.
pub async fn install(server: &dyn Server, settings: &Fail2BanSettings, ssh_ports: &[i32], log: Log<'_>) -> Result<()> {
    let body = fill(INSTALL, &[("apply", &apply_script(settings, ssh_ports))]);
    script::run(server, &script::with_prelude(&body), log, "Installing fail2ban failed", None).await
}

const SAVE: &str = r#"command -v fail2ban-client >/dev/null 2>&1 || fail "fail2ban is not installed: install it with Addons -> Fail2Ban first."
{{apply}}
{{unban}}"#;

/// Writes new settings (such as the whitelist) and restarts fail2ban; bans of whitelisted addresses are lifted.
pub async fn save(server: &dyn Server, settings: &Fail2BanSettings, ssh_ports: &[i32], log: Log<'_>) -> Result<()> {
    let unban = settings
        .whitelist
        .iter()
        .filter(|e| !e.contains('/'))
        .map(|ip| format!("[ \"$(fail2ban-client set {JAIL} unbanip {} 2>/dev/null)\" = 1 ] && echo \"  {ip} was banned: unbanned\" || true", shell_quote(ip)))
        .collect::<Vec<_>>()
        .join("\n");
    let body = fill(SAVE, &[("apply", &apply_script(settings, ssh_ports)), ("unban", &unban)]);
    script::run(server, &script::with_prelude(&body), log, "Saving the fail2ban settings failed", None).await
}

const UNBAN: &str = r#"echo "> fail2ban-client set {{Jail}} unbanip {{ip}}"
out=$(fail2ban-client set {{Jail}} unbanip {{quoted}} 2>&1) || fail "{{ip}} could not be unbanned: $out"
if [ "$out" = 1 ]; then echo "  {{ip}} can log in again"; else echo "  {{ip}} was not banned (any more)"; fi"#;

/// Lifts the ban of one address.
pub async fn unban(server: &dyn Server, ip: &str, log: Log<'_>) -> Result<()> {
    if ip.contains('/') {
        fail!("Unban one address, not a network.");
    }
    if let Some(problem) = entry_problem(ip) {
        fail!("{problem}");
    }
    let body = fill(UNBAN, &[("Jail", JAIL), ("quoted", &shell_quote(ip)), ("ip", ip)]);
    script::run(server, &script::with_prelude(&body), log, "Unbanning failed", None).await
}

const REMOVE: &str = r#"if [ -d /run/systemd/system ]; then systemctl disable --now fail2ban >/dev/null 2>&1 || true; else fail2ban-client stop >/dev/null 2>&1 || true; fi
echo "> fail2ban stopped: every ban is lifted"
rm -f {{ConfigFile}}
echo "  {{ConfigFile}} deleted"
DEBIAN_FRONTEND=noninteractive apt-get remove -y fail2ban >/dev/null && echo "  apt-get remove fail2ban"
true"#;

/// Stops fail2ban, takes it off at boot, removes the installer's settings and the package.
pub async fn remove(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(REMOVE, &[("ConfigFile", CONFIG_FILE)]);
    script::run(server, &script::with_prelude(&body), log, "Removing fail2ban failed", None).await
}

/// .NET's `IPAddress.TryParse` and `int.TryParse`, which [`entry_problem`] must match exactly.
mod dotnet_ip {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// `int.TryParse` (NumberStyles.Integer, invariant): optional white space around, an optional sign, ASCII digits.
    /// Trailing NUL characters are ignored, as .NET does.
    pub fn parse_int(text: &str) -> Option<i32> {
        let is_white = |c: char| matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ');
        let text = text.trim_end_matches('\0').trim_matches(is_white);
        let (negative, digits) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut value: i64 = 0;
        for b in digits.bytes() {
            value = value * 10 + i64::from(b - b'0');
            if value > i64::from(i32::MAX) + 1 {
                return None;
            }
        }
        let value = if negative { -value } else { value };
        i32::try_from(value).ok()
    }

    /// `IPAddress.TryParse`: IPv6 when the text has a ':', else IPv4 in .NET's lenient form (each part decimal, octal
    /// with a leading 0, or hex with 0x).
    pub fn parse(text: &str) -> Option<IpAddr> {
        if text.contains(':') {
            parse_ipv6(text).map(IpAddr::V6)
        } else {
            parse_ipv4(text).map(IpAddr::V4)
        }
    }

    /// IPv4AddressHelper.ParseNonCanonical, which must consume the whole text.
    fn parse_ipv4(text: &str) -> Option<Ipv4Addr> {
        let chars = text.as_bytes();
        let end = chars.len();
        let mut parts = [0u64; 4];
        let mut dot_count = 0usize;
        let mut current = 0usize;
        let mut value: u64 = 0;
        let mut at_least_one = false;
        while current < end {
            let mut base = 10u64;
            value = 0;
            if chars[current] == b'0' {
                base = 8;
                current += 1;
                at_least_one = true;
                if current < end && (chars[current] == b'x' || chars[current] == b'X') {
                    base = 16;
                    current += 1;
                    at_least_one = false;
                }
            }
            while current < end {
                let c = chars[current];
                let digit = match c {
                    b'0'..=b'9' if base != 8 => u64::from(c - b'0'),
                    b'0'..=b'7' => u64::from(c - b'0'),
                    b'a'..=b'f' if base == 16 => u64::from(c - b'a' + 10),
                    b'A'..=b'F' if base == 16 => u64::from(c - b'A' + 10),
                    _ => break,
                };
                value = value * base + digit;
                if value > 0xFFFF_FFFF {
                    return None;
                }
                at_least_one = true;
                current += 1;
            }
            if current < end && chars[current] == b'.' {
                if dot_count >= 3 || !at_least_one || value > 0xFF {
                    return None;
                }
                parts[dot_count] = value;
                dot_count += 1;
                at_least_one = false;
                current += 1;
                continue;
            }
            break;
        }
        // Empty trailing part, or a character that is not part of an address (a terminator such as '/' would end
        // the address early, which TryParse refuses too).
        if !at_least_one || current < end {
            return None;
        }
        parts[dot_count] = value;
        let address = match dot_count {
            0 => value,
            1 if parts[1] <= 0xFF_FFFF => (parts[0] << 24) | parts[1],
            2 if parts[2] <= 0xFFFF => (parts[0] << 24) | (parts[1] << 16) | parts[2],
            3 if parts[3] <= 0xFF => (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3],
            _ => return None,
        };
        Some(Ipv4Addr::from(address as u32))
    }

    /// .NET takes "[addr]" and "[addr]:port" as well as the bare address. The port is only checked for its
    /// characters (IPv6AddressHelper.IsValidStrict): "0x" and hex digits, or decimal digits, of any length.
    fn parse_ipv6(text: &str) -> Option<Ipv6Addr> {
        if let Some(rest) = text.strip_prefix('[') {
            let close = rest.find(']')?;
            let after = &rest[close + 1..];
            if !after.is_empty() {
                let port = after.strip_prefix(':')?;
                let valid = match port.strip_prefix("0x").filter(|_| after.len() >= 3) {
                    Some(hex) => hex.bytes().all(|b| b.is_ascii_hexdigit()),
                    None => port.bytes().all(|b| b.is_ascii_digit()),
                };
                if !valid {
                    return None;
                }
            }
            return rest[..close].parse().ok();
        }
        text.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_units() {
        assert_eq!(minutes(Some("600")), Some(10));
        assert_eq!(minutes(Some("30")), Some(1));
        assert_eq!(minutes(Some(" 10m ")), Some(10));
        assert_eq!(minutes(Some("1h")), Some(60));
        assert_eq!(minutes(Some("2D")), Some(2880));
        assert_eq!(minutes(Some("1w")), Some(10080));
        assert_eq!(minutes(Some("1y")), None);
        assert_eq!(minutes(Some("m")), None);
        assert_eq!(minutes(Some("abc")), None);
        assert_eq!(minutes(None), None);
    }

    #[test]
    fn parses_the_state() {
        let values = ScriptValues::parse(
            "version=1.0.2\ninstalled=yes\nrunning=yes\nconfigured=yes\nignoreip=127.0.0.1/8 ::1 203.0.113.5\nmaxretry=0\nfindtime=600\nbantime=2h\nincrement=false\nssh=22 2222 \nbanned=198.51.100.1 198.51.100.2\ntotal=7\n",
        );
        let state = state_from(&values, Some("203.0.113.5".into()));
        assert_eq!(
            state.settings,
            Some(Fail2BanSettings { max_retry: 5, find_time_minutes: 10, ban_time_minutes: 120, increment: false, whitelist: vec!["203.0.113.5".into()] })
        );
        assert_eq!(state.ssh_ports, vec![22, 2222]);
        assert_eq!(state.banned, vec!["198.51.100.1".to_string(), "198.51.100.2".to_string()]);
        assert_eq!(state.total_banned, 7);
        assert!(state.is_complete());
        let empty = state_from(&ScriptValues::parse("installed=yes"), None);
        assert_eq!(empty.settings, None);
        assert!(!empty.is_complete());
    }

    #[test]
    fn settings_problems() {
        assert_eq!(settings_problem(&defaults()), None);
        assert!(settings_problem(&Fail2BanSettings { max_retry: 0, ..defaults() }).unwrap().starts_with("Failed logins"));
        assert!(settings_problem(&Fail2BanSettings { find_time_minutes: 10081, ..defaults() }).unwrap().starts_with("Counted"));
        assert!(settings_problem(&Fail2BanSettings { ban_time_minutes: 0, ..defaults() }).unwrap().starts_with("Ban time"));
    }

    #[test]
    fn whitelist_parsing() {
        let (entries, problem) = parse_whitelist("203.0.113.5, 198.51.100.0/24 # office\r\n# a comment\n2001:db8::1\t203.0.113.5\n2001:DB8::1");
        assert_eq!(entries, vec!["203.0.113.5", "198.51.100.0/24", "2001:db8::1"]);
        assert_eq!(problem, None);
        let (entries, problem) = parse_whitelist("1.2.3.4 nope 5.6.7.8");
        assert_eq!(entries, vec!["1.2.3.4"]);
        assert_eq!(problem.as_deref(), Some("\"nope\" is not an IP address or a network such as 192.168.1.0/24."));
        let many: String = (0..101).map(|i| format!("10.0.{}.{}\n", i / 256, i % 256)).collect();
        assert_eq!(parse_whitelist(&many).1.as_deref(), Some("At most 100 whitelist entries."));
    }
}
