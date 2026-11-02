//! Live figures for the Server status dashboard, read from /proc in one short command per sample, and the versions
//! of the hotel's clones and Docker services. (ServerStatus.cs)

use std::collections::HashMap;

use aio_core::layout::AtomCmsPresence;
use aio_core::{InstallerSettings, ServerLayout};
use chrono::{DateTime, Utc};

use crate::script::fill;
use crate::server::{shell_quote, Result, Server};
use crate::{atom_cms, crowdsec, emulator, gitea, octane};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskUsage {
    pub mount: String,
    pub device: String,
    pub total: i64,
    pub used: i64,
    pub available: i64,
}

impl DiskUsage {
    pub fn percent(&self) -> f64 {
        if self.total > 0 {
            self.used as f64 * 100.0 / self.total as f64
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedInUser {
    pub user: String,
    pub terminal: String,
    pub since: String,
    pub from: String,
}

/// One reading of the server. CPU and network are counters, so compare two samples for a rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSample {
    pub at: DateTime<Utc>,
    pub cpu_total: i64,
    pub cpu_idle: i64,
    pub cores: i32,
    pub mem_total: i64,
    pub mem_available: i64,
    pub swap_total: i64,
    pub swap_free: i64,
    pub received_bytes: i64,
    pub sent_bytes: i64,
    pub load: String,
    pub uptime_seconds: i64,
    pub disks: Vec<DiskUsage>,
    pub users: Vec<LoggedInUser>,
    pub players_online: Option<i32>,
    pub emulator_running: bool,
}

impl StatusSample {
    pub fn mem_used(&self) -> i64 {
        self.mem_total - self.mem_available
    }

    /// CPU busy percentage between `previous` and this sample.
    pub fn cpu_percent(&self, previous: &StatusSample) -> f64 {
        let total = self.cpu_total - previous.cpu_total;
        let idle = self.cpu_idle - previous.cpu_idle;
        if total > 0 {
            ((total - idle) as f64 * 100.0 / total as f64).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    /// Bytes per second (in, out) between `previous` and this sample.
    pub fn bytes_per_second(&self, previous: &StatusSample) -> (f64, f64) {
        let seconds = seconds_between(previous.at, self.at);
        if seconds <= 0.0 {
            return (0.0, 0.0);
        }
        (
            ((self.received_bytes - previous.received_bytes) as f64 / seconds).max(0.0),
            ((self.sent_bytes - previous.sent_bytes) as f64 / seconds).max(0.0),
        )
    }
}

/// `to - from` in seconds, negative when `to` is earlier (C#'s TimeSpan.TotalSeconds).
pub(crate) fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_microseconds().map(|us| us as f64 / 1_000_000.0).unwrap_or(0.0)
}

/// A Docker service of the hotel (Traefik, Gitea, CrowdSec): the version that runs, and whether Docker Hub has a newer
/// image for its tag, or a newer image is downloaded but not running yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageStatus {
    pub name: String,
    pub container: String,
    pub image: String,
    pub version: Option<String>,
    pub running: bool,
    pub newer_on_hub: bool,
    pub not_restarted: bool,
    pub newest_version: Option<String>,
    pub problem: Option<String>,
}

impl ImageStatus {
    pub fn has_update(&self) -> bool {
        self.newer_on_hub || self.not_restarted
    }
}

/// The git state of one hotel repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryVersion {
    pub name: String,
    pub directory: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub date: Option<String>,
    pub subject: Option<String>,
    pub build: Option<String>,
}

/// What a "git pull" would bring in for one clone: the commits it is behind, and the newest one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryUpdate {
    pub name: String,
    pub directory: String,
    pub branch: Option<String>,
    pub target: Option<String>,
    pub behind: i32,
    pub newest: Option<String>,
}

impl RepositoryUpdate {
    pub fn has_update(&self) -> bool {
        self.behind > 0
    }

    /// The clone is on another branch than the one chosen in the settings, so updating switches it over first.
    /// Compared without case, because one repository writes the branch as "dev" and another as "Dev".
    pub fn switches_branch(&self) -> bool {
        match &self.target {
            Some(target) if !target.is_empty() => {
                !self.branch.as_deref().is_some_and(|branch| branch.to_lowercase() == target.to_lowercase())
            }
            _ => false,
        }
    }

    /// The branch the commits were counted against.
    pub fn counted(&self) -> Option<&str> {
        self.target.as_deref().or(self.branch.as_deref())
    }
}

// Network totals cover the server's own interfaces, not loopback or Docker's bridges.
const SAMPLE_SCRIPT: &str = r#"echo "cpu $(head -1 /proc/stat)"
echo "cores $(nproc)"
awk '/^(MemTotal|MemAvailable|SwapTotal|SwapFree):/ {print "mem", $1, $2}' /proc/meminfo
awk 'NR > 2 { sub(/^ */, ""); split($0, a, /[: ]+/); if (a[1] != "lo" && a[1] !~ /^(docker|veth|br-)/) { rx += a[2]; tx += a[10] } } END { printf "net %.0f %.0f\n", rx, tx }' /proc/net/dev
echo "load $(cut -d' ' -f1-3 /proc/loadavg)"
echo "uptime $(cut -d' ' -f1 /proc/uptime)"
df -P -B1 -x tmpfs -x devtmpfs -x overlay -x squashfs -x efivarfs 2>/dev/null | awk 'NR > 1 { print "disk", $6, $1, $2, $3, $4 }'
who 2>/dev/null | awk '{ from = ""; if (match($0, /\(.*\)/)) from = substr($0, RSTART + 1, RLENGTH - 2); print "user", $1, $2, $3 "_" $4, from }'
db=$(grep -sE '^db.database=' {{Directory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
[ -n "$db" ] && echo "players $(mariadb -N "$db" -e "SELECT COUNT(*) FROM users WHERE online = '1'" 2>/dev/null)"
if [ -d /run/systemd/system ]; then systemctl is-active --quiet emulator && echo "emulator yes"; else pgrep -f jar-with-dependencies >/dev/null && echo "emulator yes"; fi
true"#;

/// The command [`sample`] runs.
pub fn sample_script() -> String {
    fill(SAMPLE_SCRIPT, &[("Directory", &emulator::directory())])
}

pub async fn sample(server: &dyn Server) -> Result<StatusSample> {
    let result = server.run_as_root(&sample_script()).await?;
    Ok(parse_sample(&result.output, Utc::now()))
}

/// The sample script's output as a sample taken at `at`.
pub fn parse_sample(output: &str, at: DateTime<Utc>) -> StatusSample {
    let mut s = StatusSample {
        at,
        cpu_total: 0,
        cpu_idle: 0,
        cores: 1,
        mem_total: 0,
        mem_available: 0,
        swap_total: 0,
        swap_free: 0,
        received_bytes: 0,
        sent_bytes: 0,
        load: String::new(),
        uptime_seconds: 0,
        disks: Vec::new(),
        users: Vec::new(),
        players_online: None,
        emulator_running: false,
    };
    for line in output.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split(' ').filter(|p| !p.is_empty()).collect();
        match f[0] {
            // cpu cpu user nice system idle iowait irq softirq steal
            "cpu" if f.len() >= 6 => {
                let ticks: Vec<i64> = f.iter().skip(2).take(8).map(|t| long(t)).collect();
                s.cpu_total = ticks.iter().sum();
                s.cpu_idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
            }
            "cores" if f.len() > 1 => s.cores = (long(f[1]) as i32).max(1),
            "mem" if f.len() > 2 => {
                let kb = long(f[2]).wrapping_mul(1024);
                match f[1] {
                    "MemTotal:" => s.mem_total = kb,
                    "MemAvailable:" => s.mem_available = kb,
                    "SwapTotal:" => s.swap_total = kb,
                    "SwapFree:" => s.swap_free = kb,
                    _ => {}
                }
            }
            "net" if f.len() > 2 => {
                s.received_bytes = long(f[1]);
                s.sent_bytes = long(f[2]);
            }
            "load" => s.load = f[1..].join(" "),
            "uptime" if f.len() > 1 => s.uptime_seconds = f[1].parse::<f64>().map(|up| up as i64).unwrap_or(0),
            "disk" if f.len() > 5 => s.disks.push(DiskUsage {
                mount: f[1].into(),
                device: f[2].into(),
                total: long(f[3]),
                used: long(f[4]),
                available: long(f[5]),
            }),
            "user" if f.len() > 3 => s.users.push(LoggedInUser {
                user: f[1].into(),
                terminal: f[2].into(),
                since: f[3].replace('_', " "),
                from: f.get(4).map(|p| p.to_string()).unwrap_or_default(),
            }),
            "players" if f.len() > 1 => {
                if let Ok(online) = f[1].parse::<i32>() {
                    s.players_online = Some(online);
                }
            }
            "emulator" => s.emulator_running = true,
            _ => {}
        }
    }
    s
}

/// The hotel's clones, in the order the dashboard and the Tools page show them, with the GitHub repository each one
/// comes from. AtomCMS has none: its step offers no branch choice, so it follows its own branch.
fn repositories() -> Vec<(&'static str, String, Option<&'static str>)> {
    let mut list = vec![
        ("Emulator (Polaris)", emulator::repository(), Some("duckietm/Polaris-Emulator")),
        ("Octane", octane::octane(), Some("duckietm/Octane")),
        ("Octane-Renderer", octane::renderer(), Some("duckietm/Octane-Renderer")),
    ];
    // Last, so the indexes of the others stay the same without it. Left out when Settings say it is not installed.
    if ServerLayout::current().atom_cms_installed != AtomCmsPresence::NotInstalled {
        list.push(("AtomCMS", atom_cms::directory(), None));
    }
    list
}

/// The branch chosen on the Settings page for a clone folder, so the Tools updates follow the same choice as the
/// clone steps. None when that clone has no choice and keeps the branch it is on.
pub fn chosen_branch(directory: &str) -> Option<String> {
    chosen_branch_with(&InstallerSettings::load(), directory)
}

/// [`chosen_branch`] with these settings.
pub fn chosen_branch_with(settings: &InstallerSettings, directory: &str) -> Option<String> {
    repositories().into_iter().find(|r| r.1 == directory).and_then(|r| r.2).map(|github| settings.branch_for(github))
}

const VERSION_BLOCK: &str = r#"d={{Directory}}
if [ -d "$d/.git" ]; then
  echo "repo{{i}} $(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD 2>/dev/null)|$(git -c safe.directory="$d" -C "$d" log -1 --format='%h|%cd|%s' --date=format:'%Y-%m-%d %H:%M' 2>/dev/null)"
fi"#;

const VERSION_BUILDS: &str = r#"
jar=$(ls -t {{Directory}}/target/Polaris-*-jar-with-dependencies.jar {{Directory}}/target/Habbo-*-jar-with-dependencies.jar 2>/dev/null | head -1)
[ -n "$jar" ] && echo "jar $(basename "$jar" -jar-with-dependencies.jar)"
[ -f {{Octane}}/package.json ] && echo "octane $(grep -m1 '"version"' {{Octane}}/package.json | cut -d'"' -f4)"
[ -f {{Renderer}}/package.json ] && echo "renderer $(grep -m1 '"version"' {{Renderer}}/package.json | cut -d'"' -f4)"
true"#;

/// The command [`versions`] runs.
pub fn versions_script() -> String {
    let blocks: Vec<String> = repositories()
        .iter()
        .enumerate()
        .map(|(i, r)| fill(VERSION_BLOCK, &[("Directory", &r.1), ("i", &i.to_string())]))
        .collect();
    blocks.join("\n")
        + &fill(
            VERSION_BUILDS,
            &[("Directory", &emulator::directory()), ("Octane", &octane::octane()), ("Renderer", &octane::renderer())],
        )
}

/// Branch, commit and date of the emulator, Octane and Octane-Renderer clones, plus the emulator's JAR version.
pub async fn versions(server: &dyn Server) -> Result<Vec<RepositoryVersion>> {
    let result = server.run_as_root(&versions_script()).await?;
    Ok(parse_versions(&result.output))
}

pub fn parse_versions(output: &str) -> Vec<RepositoryVersion> {
    let lines = key_values(output);
    let value = |key: &str| lines.get(key).cloned();
    let builds = [value("jar"), value("octane").map(|o| format!("v{o}")), value("renderer").map(|r| format!("v{r}")), None];
    let mut versions = Vec::new();
    for (i, (name, directory, _)) in repositories().into_iter().enumerate() {
        let Some(info) = value(&format!("repo{i}")) else { continue };
        let parts: Vec<&str> = info.splitn(4, '|').collect();
        versions.push(RepositoryVersion {
            name: name.into(),
            directory,
            branch: part(&parts, 0),
            commit: part(&parts, 1),
            date: part(&parts, 2),
            subject: part(&parts, 3),
            build: builds.get(i).cloned().flatten(),
        });
    }
    versions
}

const UPDATE_BLOCK: &str = r#"d={{Directory}}
if [ -d "$d/.git" ]; then
  timeout 30 git -c safe.directory="$d" -C "$d" fetch --quiet --prune origin 2>/dev/null
  want={{want}}
  # The chosen branch as the remote really writes it ("dev" there, "Dev" in another repository).
  ref=$([ -n "$want" ] && git -c safe.directory="$d" -C "$d" for-each-ref --format='%(refname:lstrip=3)' refs/remotes/origin | grep -ix "$want" | head -1)
  # Without a choice, or when the remote has no such branch, count against the branch it follows now.
  if [ -n "$ref" ]; then target="origin/$ref"; else target='@{u}'; fi
  echo "repo{{i}} $(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD 2>/dev/null)|$ref|$(git -c safe.directory="$d" -C "$d" rev-list --count HEAD..$target 2>/dev/null || echo 0)|$(git -c safe.directory="$d" -C "$d" log -1 --format='%h|%cd|%s' --date=format:'%Y-%m-%d %H:%M' $target 2>/dev/null)"
fi"#;

/// The command [`updates`] runs with these settings.
pub fn updates_script(settings: &InstallerSettings) -> String {
    let blocks: Vec<String> = repositories()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let want = shell_quote(&r.2.map(|github| settings.branch_for(github)).unwrap_or_default());
            fill(UPDATE_BLOCK, &[("Directory", &r.1), ("i", &i.to_string()), ("want", &want)])
        })
        .collect();
    blocks.join("\n") + "\ntrue\n"
}

/// Fetches every clone and reports what a "git pull" would bring in. The fetch only fills the remote branches: no file
/// on the server is changed, and the hotel keeps running.
pub async fn updates(server: &dyn Server) -> Result<Vec<RepositoryUpdate>> {
    updates_with(server, &InstallerSettings::load()).await
}

/// [`updates`] with the branch choices of these settings.
pub async fn updates_with(server: &dyn Server, settings: &InstallerSettings) -> Result<Vec<RepositoryUpdate>> {
    let result = server.run_as_root(&updates_script(settings)).await?;
    Ok(parse_updates(&result.output))
}

pub fn parse_updates(output: &str) -> Vec<RepositoryUpdate> {
    let lines = key_values(output);
    let mut updates = Vec::new();
    for (i, (name, directory, _)) in repositories().into_iter().enumerate() {
        let Some(info) = lines.get(&format!("repo{i}")) else { continue };
        // branch|target|behind|hash|date|subject, where the last three are missing when nothing is tracked.
        let parts: Vec<&str> = info.splitn(6, '|').collect();
        let newest: Vec<String> = [part(&parts, 3), part(&parts, 4), part(&parts, 5)].into_iter().flatten().collect();
        let newest = newest.join("  ");
        updates.push(RepositoryUpdate {
            name: name.into(),
            directory,
            branch: part(&parts, 0),
            target: part(&parts, 1),
            behind: part(&parts, 2).and_then(|b| b.trim().parse().ok()).unwrap_or(0),
            newest: (!newest.is_empty()).then_some(newest),
        });
    }
    updates
}

/// The Docker services the dashboard checks: Traefik always, Gitea and CrowdSec when they are installed.
fn services() -> [(&'static str, &'static str, &'static str, String); 3] {
    [
        ("Traefik", "traefik", "traefik:v3", "docker exec traefik traefik version 2>/dev/null | awk '/^Version/ {print $2}'".to_string()),
        ("Gitea", gitea::CONTAINER, gitea::IMAGE, format!("docker exec {} gitea --version 2>/dev/null | awk '{{print $3}}'", gitea::CONTAINER)),
        ("CrowdSec", crowdsec::CONTAINER, crowdsec::IMAGE, crowdsec::VERSION_COMMAND.to_string()),
    ]
}

// Go template braces are filled in like the C# installer's constants, so they stay out of the placeholders.
const GO_IMAGE_ID: &str = "{{.Image}}";
const GO_ID: &str = "{{.Id}}";
const GO_RUNNING: &str = "{{.State.Running}}";
const GO_DIGESTS: &str = "{{range .RepoDigests}}{{println .}}{{end}}";
const GO_MANIFEST: &str = "{{.Manifest.Digest}}";
const GO_CONFIG: &str = "{{json .Image}}";

const IMAGE_BLOCK: &str = r#"if docker inspect {{Container}} >/dev/null 2>&1; then
  running=$(docker inspect -f '{{GoRunning}}' {{Container}} 2>/dev/null)
  version=$({{VersionCommand}})
  # A newer image downloaded, but the container still runs the old one.
  current=$(docker inspect -f '{{GoImageId}}' {{Container}} 2>/dev/null)
  downloaded=$(docker image inspect -f '{{GoId}}' {{Image}} 2>/dev/null)
  restart=$([ -n "$downloaded" ] && [ "$current" != "$downloaded" ] && echo yes)
  # The digest Docker Hub has for the tag now, against the one this server pulled.
  remote=$(timeout 30 docker buildx imagetools inspect {{Image}} --format '{{GoManifest}}' 2>/dev/null)
  mine=$(docker image inspect -f '{{GoDigests}}' {{Image}} 2>/dev/null | sed -n 's/.*@//p' | head -1)
  newest=
  if [ -z "$remote" ]; then
    newer=; problem="Docker Hub did not answer"
  else
    problem=
    newer=$([ "$remote" != "$mine" ] && echo yes)
    [ -n "$newer" ] && newest=$(timeout 30 docker buildx imagetools inspect {{Image}} --format '{{GoConfig}}' 2>/dev/null \
      | grep -oE '"org.opencontainers.image.version": *"[^"]+"' | head -1 | cut -d'"' -f4)
  fi
  echo "image{{i}} $running|$version|$newer|$restart|$newest|$problem"
fi"#;

/// The command [`images`] runs.
pub fn images_script() -> String {
    let blocks: Vec<String> = services()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            // The service values first; the Go templates last, so their braces are never read as placeholders.
            fill(
                IMAGE_BLOCK,
                &[
                    ("Container", m.1),
                    ("Image", m.2),
                    ("VersionCommand", &m.3),
                    ("i", &i.to_string()),
                    ("GoRunning", GO_RUNNING),
                    ("GoImageId", GO_IMAGE_ID),
                    ("GoId", GO_ID),
                    ("GoManifest", GO_MANIFEST),
                    ("GoDigests", GO_DIGESTS),
                    ("GoConfig", GO_CONFIG),
                ],
            )
        })
        .collect();
    format!("command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1 || exit 0\n{}\ntrue\n", blocks.join("\n"))
}

/// Compares each service's image with Docker Hub's newest for the same tag (only the registry's index is read: nothing
/// is downloaded or changed), and the running container with the image downloaded last. (ImagesAsync)
pub async fn images(server: &dyn Server) -> Result<Vec<ImageStatus>> {
    let result = server.run_as_root(&images_script()).await?;
    Ok(parse_images(&result.output))
}

pub fn parse_images(output: &str) -> Vec<ImageStatus> {
    let lines = key_values(output);
    let mut list = Vec::new();
    for (i, (name, container, image, _)) in services().into_iter().enumerate() {
        let Some(info) = lines.get(&format!("image{i}")) else { continue };
        let parts: Vec<&str> = info.splitn(6, '|').collect();
        list.push(ImageStatus {
            name: name.into(),
            container: container.into(),
            image: image.into(),
            version: part(&parts, 1),
            running: part(&parts, 0).as_deref() == Some("true"),
            newer_on_hub: part(&parts, 2).as_deref() == Some("yes"),
            not_restarted: part(&parts, 3).as_deref() == Some("yes"),
            newest_version: part(&parts, 4),
            problem: part(&parts, 5),
        });
    }
    list
}

// "key value" lines; a key without a value is left out.
fn key_values(output: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in output.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
        if let Some((key, value)) = line.split_once(' ') {
            if !value.is_empty() {
                values.insert(key.to_string(), value.to_string());
            }
        }
    }
    values
}

fn part(parts: &[&str], i: usize) -> Option<String> {
    parts.get(i).filter(|p| !p.is_empty()).map(|p| p.to_string())
}

/// C#'s long.TryParse, or 0.
fn long(text: &str) -> i64 {
    text.trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SAMPLE: &str = "cpu cpu  4705 150 1120 16250 520 0 25 0 0 0\n\
cores 4\n\
mem MemTotal: 8148108\n\
mem MemAvailable: 5023412\n\
mem SwapTotal: 2097148\n\
mem SwapFree: 2097148\n\
net 123456789 98765432\n\
load 0.42 0.31 0.25\n\
uptime 864123.57\n\
disk / /dev/sda1 81032015872 21474836480 55985274880\n\
disk /boot/efi /dev/sda15 109422592 6291456 103131136\n\
user root pts/0 2026-10-31_09:12 203.0.113.7\n\
user simone tty1 2026-10-30_18:00 \n\
players 12\n\
emulator yes\n";

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn parses_a_sample() {
        let s = parse_sample(SAMPLE, at(0));
        assert_eq!(s.cpu_total, 4705 + 150 + 1120 + 16250 + 520 + 25);
        assert_eq!(s.cpu_idle, 16250 + 520);
        assert_eq!(s.cores, 4);
        assert_eq!(s.mem_total, 8148108 * 1024);
        assert_eq!(s.mem_available, 5023412 * 1024);
        assert_eq!(s.mem_used(), (8148108 - 5023412) * 1024);
        assert_eq!(s.swap_total, 2097148 * 1024);
        assert_eq!(s.swap_free, 2097148 * 1024);
        assert_eq!((s.received_bytes, s.sent_bytes), (123456789, 98765432));
        assert_eq!(s.load, "0.42 0.31 0.25");
        assert_eq!(s.uptime_seconds, 864123);
        assert_eq!(s.disks.len(), 2);
        assert_eq!(s.disks[0].mount, "/");
        assert_eq!(s.disks[0].device, "/dev/sda1");
        assert!((s.disks[0].percent() - 26.5).abs() < 0.1);
        assert_eq!(
            s.users,
            vec![
                LoggedInUser { user: "root".into(), terminal: "pts/0".into(), since: "2026-10-31 09:12".into(), from: "203.0.113.7".into() },
                LoggedInUser { user: "simone".into(), terminal: "tty1".into(), since: "2026-10-30 18:00".into(), from: "".into() },
            ]
        );
        assert_eq!(s.players_online, Some(12));
        assert!(s.emulator_running);
    }

    #[test]
    fn empty_and_partial_output() {
        let s = parse_sample("", at(0));
        assert_eq!(s.cores, 1);
        assert_eq!(s.players_online, None);
        assert!(!s.emulator_running);
        // No players value when the database query printed nothing; cores never below 1.
        let s = parse_sample("players\ncores 0\nload\n", at(0));
        assert_eq!(s.players_online, None);
        assert_eq!(s.cores, 1);
        assert_eq!(s.load, "");
    }

    #[test]
    fn rates_between_samples() {
        let first = parse_sample(SAMPLE, at(0));
        let second = parse_sample(
            &SAMPLE
                .replace("cpu cpu  4705 150 1120 16250 520", "cpu cpu  4805 150 1120 16550 520")
                .replace("net 123456789 98765432", "net 123466789 98770432"),
            at(2),
        );
        // 400 ticks, 300 idle: 25 % busy.
        assert!((second.cpu_percent(&first) - 25.0).abs() < 1e-9);
        assert_eq!(second.bytes_per_second(&first), (5000.0, 2500.0));
        assert_eq!(first.bytes_per_second(&second), (0.0, 0.0));
        assert_eq!(first.cpu_percent(&first), 0.0);
    }

    #[test]
    fn parses_versions() {
        let output = "repo0 main|a1b2c3d|2026-10-30 21:15|Fix wired|with a pipe\n\
repo1 dev|e4f5a6b|2026-10-29 10:00|Octane update\n\
repo3 \n\
jar Polaris-4.2.0\n\
octane 1.6.3\n";
        let versions = parse_versions(output);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].name, "Emulator (Polaris)");
        assert_eq!(versions[0].branch.as_deref(), Some("main"));
        assert_eq!(versions[0].commit.as_deref(), Some("a1b2c3d"));
        assert_eq!(versions[0].date.as_deref(), Some("2026-10-30 21:15"));
        assert_eq!(versions[0].subject.as_deref(), Some("Fix wired|with a pipe"));
        assert_eq!(versions[0].build.as_deref(), Some("Polaris-4.2.0"));
        assert_eq!(versions[1].name, "Octane");
        assert_eq!(versions[1].build.as_deref(), Some("v1.6.3"));
    }

    #[test]
    fn parses_updates() {
        let output = "repo0 main|Main|3|9f8e7d6|2026-10-31 08:00|Newest commit\n\
repo1 dev||0|\n\
repo2 HEAD||x|\n";
        let updates = parse_updates(output);
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].behind, 3);
        assert!(updates[0].has_update());
        assert_eq!(updates[0].target.as_deref(), Some("Main"));
        assert!(!updates[0].switches_branch());
        assert_eq!(updates[0].newest.as_deref(), Some("9f8e7d6  2026-10-31 08:00  Newest commit"));
        assert_eq!(updates[1].target, None);
        assert_eq!(updates[1].counted(), Some("dev"));
        assert_eq!(updates[1].newest, None);
        assert!(!updates[1].has_update());
        assert_eq!(updates[2].behind, 0);
        let switching = RepositoryUpdate { target: Some("dev".into()), ..updates[0].clone() };
        assert!(switching.switches_branch());
    }

    #[test]
    fn parses_images() {
        let output = "image0 true|3.5.3|yes||3.5.4|\n\
image2 false|1.6.3||||Docker Hub did not answer\n";
        let images = parse_images(output);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].name, "Traefik");
        assert!(images[0].running && images[0].newer_on_hub && !images[0].not_restarted);
        assert_eq!(images[0].version.as_deref(), Some("3.5.3"));
        assert_eq!(images[0].newest_version.as_deref(), Some("3.5.4"));
        assert_eq!(images[0].problem, None);
        assert!(images[0].has_update());
        assert_eq!(images[1].name, "CrowdSec");
        assert!(!images[1].running && !images[1].has_update());
        assert_eq!(images[1].problem.as_deref(), Some("Docker Hub did not answer"));
    }
}
