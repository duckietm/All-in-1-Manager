//! The health check: one short read-only script that collects what says whether the hotel is well (the services, the
//! hotel answering, the disk and memory, the certificate, the last database backup, pending updates, a reboot, the
//! firewall and the clock). It changes nothing on the server; the window turns the facts into green, yellow and red
//! lines with a fix next to the problems.

use crate::script::ScriptValues;
use crate::server::{Result, Server, ServerError};
use crate::{emulator, server_tools, web_access};

/// A systemd service the hotel uses: not installed, running, or installed but not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Missing,
    Running,
    Stopped,
}

impl Service {
    fn parse(value: Option<&str>) -> Service {
        match value {
            Some("active") => Service::Running,
            Some("") | None | Some("missing") => Service::Missing,
            Some(_) => Service::Stopped,
        }
    }
}

/// What the script found. `None` means it could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub emulator: Service,
    pub emulator_port: bool,
    pub mariadb: Service,
    pub nginx: Service,
    pub php_fpm: Service,
    pub docker: Service,
    /// The Traefik container: None when web access is not set up.
    pub traefik_running: Option<bool>,
    pub fail2ban: Service,
    pub crowdsec_running: Option<bool>,
    /// The HTTP status of the hotel site on 127.0.0.1:85; None without the hotel site.
    pub hotel_status: Option<u16>,
    pub disk_used_percent: Option<u8>,
    pub disk_free_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub swap_used_percent: Option<u8>,
    pub load: Option<f64>,
    pub cores: Option<u32>,
    /// The hotel's https address, and the days its certificate is still valid.
    pub https_address: Option<String>,
    pub certificate_days: Option<i64>,
    /// Days since the newest database backup in the backup folder; None when there is none.
    pub backup_age_days: Option<i64>,
    pub updates: Option<u32>,
    pub security_updates: Option<u32>,
    pub reboot_required: bool,
    pub ufw_installed: bool,
    pub ufw_active: bool,
    pub clock_synchronized: Option<bool>,
}

const SCRIPT: &str = r#"set +e
unit() {
  if [ -d /run/systemd/system ] && systemctl list-unit-files "$1.service" 2>/dev/null | grep -q "^$1\.service"; then
    systemctl is-active "$1" 2>/dev/null || true
  else
    echo missing
  fi
}
echo "emulator=$(unit emulator)"
echo "emulator_port=$(ss -Hltn 'sport = :3000' 2>/dev/null | grep -q . && echo yes)"
echo "mariadb=$(unit mariadb)"
echo "nginx=$(unit nginx)"
echo "php=$(unit php8.5-fpm)"
echo "docker=$(unit docker)"
if command -v docker >/dev/null 2>&1; then
  echo "traefik=$(docker inspect -f '{{.State.Running}}' traefik 2>/dev/null)"
  echo "crowdsec=$(docker inspect -f '{{.State.Running}}' crowdsec 2>/dev/null)"
fi
echo "fail2ban=$(unit fail2ban)"
if [ -e {{HotelSiteFile}} ]; then
  echo "hotel=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 http://127.0.0.1:{{HotelPort}}/ 2>/dev/null)"
fi
df -P / 2>/dev/null | awk 'NR==2 {gsub("%", "", $5); print "disk_used=" $5; print "disk_free=" $4 * 1024}'
awk '/^MemTotal:/ {print "mem_total=" $2 * 1024} /^MemAvailable:/ {print "mem_available=" $2 * 1024}
     /^SwapTotal:/ {st=$2} /^SwapFree:/ {sf=$2} END {if (st > 0) print "swap_used=" int((st - sf) * 100 / st)}' /proc/meminfo
echo "load=$(cut -d' ' -f1 /proc/loadavg)"
echo "cores=$(nproc 2>/dev/null)"
env={{TraefikDirectory}}/hotel.env
if [ -f "$env" ] && grep -q '^SCHEME=https' "$env"; then
  address=$(sed -n 's/^ADDRESS=//p' "$env")
  echo "https=$address"
  end=$(echo | timeout 10 openssl s_client -connect 127.0.0.1:443 -servername "$address" 2>/dev/null | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)
  [ -n "$end" ] && echo "cert_days=$(( ($(date -d "$end" +%s) - $(date +%s)) / 86400 ))"
fi
newest=$(find {{Backups}} -maxdepth 1 -name '*.sql.gz' -printf '%T@\n' 2>/dev/null | sort -n | tail -1 | cut -d. -f1)
[ -n "$newest" ] && echo "backup_age=$(( ($(date +%s) - newest) / 86400 ))"
if command -v apt-get >/dev/null 2>&1; then
  pending=$(apt-get -s -o Debug::NoLocking=1 upgrade 2>/dev/null | grep '^Inst ')
  echo "updates=$(printf '%s\n' "$pending" | grep -c '^Inst ')"
  echo "security=$(printf '%s\n' "$pending" | grep -ci 'security')"
fi
echo "reboot=$([ -f /var/run/reboot-required ] && echo yes)"
echo "ufw_installed=$(command -v ufw >/dev/null 2>&1 && echo yes)"
echo "ufw=$(ufw status 2>/dev/null | grep -q 'Status: active' && echo yes)"
echo "ntp=$(timedatectl show -p NTPSynchronized --value 2>/dev/null)"
true"#;

/// The script, with this layout's backup folder and the hotel site's port.
pub fn script() -> String {
    SCRIPT
        .replace("{{HotelPort}}", web_access::NGINX_PORT)
        .replace("{{TraefikDirectory}}", web_access::traefik_directory())
        .replace("{{HotelSiteFile}}", crate::hotel_site::file())
        .replace("{{Backups}}", &server_tools::backup_directory())
}

pub fn parse(output: &str) -> Facts {
    let v = ScriptValues::parse(output);
    let number = |key: &str| v.get(key).and_then(|x| x.parse::<u64>().ok());
    let flag = |key: &str| v.get(key).map(|x| x == "true");
    Facts {
        emulator: Service::parse(v.get("emulator")),
        emulator_port: v.yes("emulator_port"),
        mariadb: Service::parse(v.get("mariadb")),
        nginx: Service::parse(v.get("nginx")),
        php_fpm: Service::parse(v.get("php")),
        docker: Service::parse(v.get("docker")),
        traefik_running: flag("traefik"),
        fail2ban: Service::parse(v.get("fail2ban")),
        crowdsec_running: flag("crowdsec"),
        hotel_status: v.get("hotel").and_then(|x| x.parse().ok()),
        disk_used_percent: v.get("disk_used").and_then(|x| x.parse().ok()),
        disk_free_bytes: number("disk_free"),
        memory_total_bytes: number("mem_total"),
        memory_available_bytes: number("mem_available"),
        swap_used_percent: v.get("swap_used").and_then(|x| x.parse().ok()),
        load: v.get("load").and_then(|x| x.parse().ok()),
        cores: v.get("cores").and_then(|x| x.parse().ok()),
        https_address: v.string("https"),
        certificate_days: v.get("cert_days").and_then(|x| x.parse().ok()),
        backup_age_days: v.get("backup_age").and_then(|x| x.parse().ok()),
        updates: v.get("updates").and_then(|x| x.parse().ok()),
        security_updates: v.get("security").and_then(|x| x.parse().ok()),
        reboot_required: v.yes("reboot"),
        ufw_installed: v.yes("ufw_installed"),
        ufw_active: v.yes("ufw"),
        clock_synchronized: v.get("ntp").map(|x| x == "yes"),
    }
}

/// Runs the check (read only).
pub async fn check(server: &dyn Server) -> Result<Facts> {
    let result = server.run_as_root(&script()).await?;
    if result.output.trim().is_empty() && !result.succeeded() {
        return Err(ServerError(format!("The health check could not run: {}", result.error.trim())).into());
    }
    Ok(parse(&result.output))
}

/// The emulator's folder, for messages.
pub fn emulator_directory() -> String {
    emulator::directory()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_server() {
        let facts = parse(
            "emulator=active\nemulator_port=yes\nmariadb=active\nnginx=active\nphp=active\ndocker=active\ntraefik=true\ncrowdsec=\n\
             fail2ban=missing\nhotel=200\ndisk_used=41\ndisk_free=52613349376\nmem_total=8331239424\nmem_available=5123345408\n\
             load=0.42\ncores=4\nhttps=hotel.example.com\ncert_days=63\nbackup_age=0\nupdates=3\nsecurity=1\nreboot=\n\
             ufw_installed=yes\nufw=yes\nntp=yes\n",
        );
        assert_eq!(facts.emulator, Service::Running);
        assert_eq!(facts.fail2ban, Service::Missing);
        assert_eq!(facts.traefik_running, Some(true));
        assert_eq!(facts.crowdsec_running, None);
        assert_eq!(facts.hotel_status, Some(200));
        assert_eq!(facts.disk_used_percent, Some(41));
        assert_eq!(facts.certificate_days, Some(63));
        assert_eq!(facts.backup_age_days, Some(0));
        assert_eq!(facts.security_updates, Some(1));
        assert!(!facts.reboot_required);
        assert_eq!(facts.clock_synchronized, Some(true));
        assert_eq!(facts.swap_used_percent, None);
    }

    #[test]
    fn problems_and_unknowns() {
        let facts = parse("emulator=failed\nmariadb=inactive\nhotel=502\nreboot=yes\nntp=no\n");
        assert_eq!(facts.emulator, Service::Stopped);
        assert_eq!(facts.mariadb, Service::Stopped);
        assert_eq!(facts.nginx, Service::Missing);
        assert_eq!(facts.hotel_status, Some(502));
        assert!(facts.reboot_required);
        assert_eq!(facts.clock_synchronized, Some(false));
        assert_eq!(facts.backup_age_days, None);
        assert_eq!(facts.certificate_days, None);
    }

    #[test]
    fn the_script_reads_only() {
        let script = script();
        // Nothing that writes, restarts or installs.
        for word in [" restart", " start ", "apt-get install", " rm ", "> /", "systemctl stop", "ufw enable"] {
            assert!(!script.contains(word), "{word}");
        }
        assert!(script.contains("/var/backups/all-in-1"));
    }
}
