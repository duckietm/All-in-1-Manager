//! Read-only insight into the emulator, from per-thread CPU of its JVM in /proc and the slow-wired warnings in its log.
//! The wired engine runs on WiredTickCoordinator, WiredTickShard-N and WiredTickHeavy-N. (EmulatorMonitor.cs)

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::emulator;
use crate::script::fill;
use crate::server::{Result, Server};
use crate::server_status::seconds_between;

/// One thread of the emulator's JVM with its CPU ticks so far and its /proc state letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmThread {
    pub id: i32,
    pub name: String,
    pub state: char,
    pub ticks: i64,
}

/// A reading of the emulator process. Ticks are cumulative, so compare two samples to get a rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorSample {
    pub at: DateTime<Utc>,
    pub pid: i32,
    pub ticks_per_second: i32,
    pub cores: i32,
    pub machine_ticks: i64,
    pub process_ticks: i64,
    pub rss_bytes: i64,
    pub max_heap_bytes: i64,
    pub started_seconds_ago: i64,
    pub threads: Vec<JvmThread>,
}

impl EmulatorSample {
    pub fn running(&self) -> bool {
        self.pid > 0
    }

    /// CPU use of the whole process as a percentage of the server and of one core.
    pub fn cpu(&self, previous: &EmulatorSample) -> (f64, f64) {
        let machine = self.machine_ticks - previous.machine_ticks;
        let process = self.process_ticks - previous.process_ticks;
        let seconds = seconds_between(previous.at, self.at);
        let server = if machine > 0 { (process as f64 * 100.0 / machine as f64).clamp(0.0, 100.0) } else { 0.0 };
        let core = if seconds > 0.0 { (process as f64 * 100.0 / (seconds * self.ticks_per_second as f64)).max(0.0) } else { 0.0 };
        (server, core)
    }

    /// Percent of one core per thread id, since `previous`.
    pub fn thread_cpu(&self, previous: &EmulatorSample) -> HashMap<i32, f64> {
        let seconds = seconds_between(previous.at, self.at);
        let before: HashMap<i32, i64> = previous.threads.iter().map(|t| (t.id, t.ticks)).collect();
        self.threads
            .iter()
            .map(|thread| {
                let delta = before.get(&thread.id).map(|old| thread.ticks - old).unwrap_or(0);
                let cpu = if seconds > 0.0 { (delta as f64 * 100.0 / (seconds * self.ticks_per_second as f64)).max(0.0) } else { 0.0 };
                (thread.id, cpu)
            })
            .collect()
    }
}

/// A room in the emulator's "Slow wired task" warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowWiredRoom {
    pub room_id: i32,
    pub name: Option<String>,
    pub count: i32,
    pub slowest_ms: i32,
    pub total_ms: i32,
    pub last_seen: String,
    pub shards: String,
}

/// What the emulator log says about wired since the emulator started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredLogSummary {
    pub rooms: Vec<SlowWiredRoom>,
    pub recent: Vec<String>,
    pub slow_tasks: i32,
    pub dropped_warnings: i32,
}

/// The group a thread belongs to. Matches prefixes because /proc cuts names to 15 characters, such as
/// WiredTickCoordi, when jcmd gave no full names.
pub fn group(thread: &str) -> &'static str {
    if thread.starts_with("WiredTick") {
        "Wired"
    } else if thread.starts_with("HabExec") {
        "Game logic (HabExec)"
    } else if thread.starts_with("Game") || thread.starts_with("Ws") {
        "Game connections"
    } else if thread.starts_with("Polaris-JDBC") || thread.contains("Hikari") {
        "Database"
    } else if thread.starts_with("GC ") || thread.starts_with("G1 ") || thread == "VM Thread" {
        "Garbage collector"
    } else if thread.starts_with("C1 ") || thread.starts_with("C2 ") {
        "JIT compiler"
    } else if thread.starts_with("AsyncAppender") {
        "Logging"
    } else if thread.starts_with("RCON") {
        "RCON"
    } else {
        "Other"
    }
}

/// Readable name of a wired thread.
pub fn wired_name(thread: &str) -> String {
    if thread.starts_with("WiredTickCoordi") {
        "Coordinator".into()
    } else if let Some(rest) = thread.strip_prefix("WiredTickShard-") {
        if rest.is_empty() { "Shard".into() } else { format!("Shard {rest}") }
    } else if let Some(rest) = thread.strip_prefix("WiredTickHeavy-") {
        if rest.is_empty() { "Heavy rooms".into() } else { format!("Heavy rooms {rest}") }
    } else {
        thread.to_string()
    }
}

pub const SAMPLE_SCRIPT: &str = r#"pid=$(pgrep -u emulator -x java | head -1)
echo "clk $(getconf CLK_TCK)"
echo "cores $(nproc)"
echo "machine $(head -1 /proc/stat | awk '{ s = 0; for (i = 2; i <= 9; i++) s += $i; print s }')"
if [ -n "$pid" ] && [ -d /proc/$pid ]; then
  echo "pid $pid"
  # comm can hold spaces and brackets, so count fields from after the last ')'.
  awk '{ sub(/.*\) /, ""); print "proc", $12 + $13, $20 }' /proc/$pid/stat
  echo "rss $(awk '/^VmRSS:/ {print $2 * 1024}' /proc/$pid/status)"
  echo "xmx $(tr '\0' '\n' < /proc/$pid/cmdline | sed -n 's/^-Xmx\([0-9]*\)\([mMgG]\)$/\1 \2/p')"
  echo "boot $(awk '{print int($1)}' /proc/uptime)"
  for t in /proc/$pid/task/*; do
    s=$(cat "$t/stat" 2>/dev/null) || continue
    rest=${s##*) }
    set -- $rest
    echo "t ${t##*/} $1 $(( ${12} + ${13} )) $(cat "$t/comm" 2>/dev/null)"
  done
fi
true"#;

pub async fn sample(server: &dyn Server) -> Result<EmulatorSample> {
    let result = server.run_as_root(SAMPLE_SCRIPT).await?;
    Ok(parse_sample(&result.output, Utc::now()))
}

/// The sample script's output as a sample taken at `at`.
pub fn parse_sample(output: &str, at: DateTime<Utc>) -> EmulatorSample {
    let (mut pid, mut clk, mut cores) = (0i32, 100i32, 1i32);
    let (mut machine, mut process, mut rss, mut max_heap, mut start_ticks, mut boot) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let mut threads = Vec::new();
    for line in output.split('\n').filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.trim().split(' ').filter(|p| !p.is_empty()).collect();
        if f.len() < 2 {
            continue;
        }
        match f[0] {
            "clk" => clk = (long(f[1]) as i32).max(1),
            "cores" => cores = (long(f[1]) as i32).max(1),
            "machine" => machine = long(f[1]),
            "pid" => pid = long(f[1]) as i32,
            "proc" if f.len() > 2 => {
                process = long(f[1]);
                start_ticks = long(f[2]);
            }
            "rss" => rss = long(f[1]),
            "xmx" if f.len() > 2 => max_heap = long(f[1]).wrapping_mul(if matches!(f[2], "g" | "G") { 1 << 30 } else { 1 << 20 }),
            "boot" => boot = long(f[1]),
            "t" if f.len() > 4 => threads.push(JvmThread {
                id: long(f[1]) as i32,
                name: f[4..].join(" "),
                state: f[2].chars().next().unwrap_or(' '),
                ticks: long(f[3]),
            }),
            _ => {}
        }
    }
    let started_ago = if pid > 0 && boot > 0 { (boot - start_ticks / clk as i64).max(0) } else { 0 };
    EmulatorSample {
        at,
        pid,
        ticks_per_second: clk,
        cores,
        machine_ticks: machine,
        process_ticks: process,
        rss_bytes: rss,
        max_heap_bytes: max_heap,
        started_seconds_ago: started_ago,
        threads,
    }
}

const THREAD_NAMES: &str = r#"java=$({{FindJava}})
[ -x "$java/bin/jcmd" ] || exit 0
timeout 10 runuser -u {{Account}} -- "$java/bin/jcmd" {{pid}} Thread.print 2>/dev/null |
  sed -nE 's/^"([^"]+)".* nid=([0-9]+) .*/\2 \1/p'
true"#;

/// The command [`thread_names`] runs.
pub fn thread_names_script(pid: i32) -> String {
    fill(THREAD_NAMES, &[("FindJava", emulator::FIND_JAVA), ("Account", &emulator::account()), ("pid", &pid.to_string())])
}

/// The JVM's own thread names by Linux thread id, from jcmd Thread.print run as the emulator account. Linux keeps only
/// 15 characters, so every WiredTickShard-N thread reads "WiredTickShard-" in /proc. Empty when jcmd is missing or
/// refuses.
pub async fn thread_names(server: &dyn Server, pid: i32) -> Result<HashMap<i32, String>> {
    let result = server.run_as_root(&thread_names_script(pid)).await?;
    Ok(parse_thread_names(&result.output))
}

pub fn parse_thread_names(output: &str) -> HashMap<i32, String> {
    let mut names = HashMap::new();
    for line in output.split('\n').filter(|l| !l.is_empty()) {
        if let Some(space) = line.find(' ').filter(|&s| s > 0) {
            if let Ok(tid) = line[..space].trim().parse::<i32>() {
                names.insert(tid, line[space + 1..].trim().to_string());
            }
        }
    }
    names
}

const WIRED_LOG: &str = r#"log={{LogDirectory}}/emulator.log
[ -f "$log" ] || exit 0
tmp=$(mktemp)
grep -E 'Slow wired task|wired tasks waiting' "$log" 2>/dev/null | tail -n 5000 > "$tmp" || true
awk '/Slow wired task/ {
       match($0, /shard=[0-9]+/); shard = substr($0, RSTART + 6, RLENGTH - 6)
       match($0, /room=[0-9]+/); room = substr($0, RSTART + 5, RLENGTH - 5)
       match($0, /took=[0-9]+/); took = substr($0, RSTART + 5, RLENGTH - 5) + 0
       n[room]++; sum[room] += took; if (took > max[room]) max[room] = took; last[room] = $1
       if (index(shards[room], "," shard ",") == 0) shards[room] = shards[room] "," shard ","
       total++ }
     /wired tasks waiting/ { dropped++ }
     END { for (r in n) { s = shards[r]; gsub(/,+/, ",", s); gsub(/^,|,$/, "", s); print "room", r, n[r], max[r], sum[r], last[r], s }
           print "totals", total + 0, dropped + 0 }' "$tmp"
tail -n 8 "$tmp" | sed 's/^/recent /'
rm -f "$tmp"
db=$(grep -sE '^db.database=' {{Directory}}/config.ini | cut -d= -f2 | tr -d '[:space:]')
ids=$(grep -oE 'room=[0-9]+' "$log" 2>/dev/null | cut -d= -f2 | sort -un | tail -n 200 | paste -sd, -)
[ -n "$db" ] && [ -n "$ids" ] && mariadb -N -B "$db" -e "SELECT id, name FROM rooms WHERE id IN ($ids)" 2>/dev/null | sed 's/^/name /'
true"#;

/// The command [`wired_log`] runs.
pub fn wired_log_script() -> String {
    fill(WIRED_LOG, &[("LogDirectory", &emulator::log_directory()), ("Directory", &emulator::directory())])
}

/// "Slow wired task: shard=, room=, took=ms" and "Room N has N wired tasks waiting" from the current emulator log, per
/// room, with the room names from the database.
pub async fn wired_log(server: &dyn Server) -> Result<WiredLogSummary> {
    let result = server.run_as_root(&wired_log_script()).await?;
    Ok(parse_wired_log(&result.output))
}

pub fn parse_wired_log(output: &str) -> WiredLogSummary {
    let mut names: HashMap<i32, String> = HashMap::new();
    // (id, count, max, sum, last, shards)
    let mut rows: Vec<(i32, i32, i32, i32, String, String)> = Vec::new();
    let mut recent = Vec::new();
    let (mut slow, mut dropped) = (0i32, 0i32);
    for line in output.split('\n').filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("recent ") {
            recent.push(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("name ") {
            if let Some((id, name)) = rest.split_once('\t') {
                if let Ok(id) = id.trim().parse::<i32>() {
                    names.insert(id, name.to_string());
                }
            }
            continue;
        }
        let f: Vec<&str> = line.split(' ').filter(|p| !p.is_empty()).collect();
        match f.first() {
            Some(&"room") if f.len() >= 6 => rows.push((
                long(f[1]) as i32,
                long(f[2]) as i32,
                long(f[3]) as i32,
                long(f[4]) as i32,
                f[5].to_string(),
                f.get(6).map(|s| s.to_string()).unwrap_or_default(),
            )),
            Some(&"totals") if f.len() >= 3 => {
                slow = long(f[1]) as i32;
                dropped = long(f[2]) as i32;
            }
            _ => {}
        }
    }
    // Most total time first (a stable sort, like LINQ's OrderByDescending).
    rows.sort_by(|a, b| b.3.cmp(&a.3));
    let rooms = rows
        .into_iter()
        .map(|(id, count, max, sum, last, shards)| SlowWiredRoom {
            room_id: id,
            name: names.get(&id).cloned(),
            count,
            slowest_ms: max,
            total_ms: sum,
            last_seen: last,
            shards,
        })
        .collect();
    WiredLogSummary { rooms, recent, slow_tasks: slow, dropped_warnings: dropped }
}

/// C#'s long.TryParse, or 0.
fn long(text: &str) -> i64 {
    text.trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
    }

    const SAMPLE: &str = "clk 100\n\
cores 4\n\
machine 2345678\n\
pid 4242\n\
proc 81234 1500000\n\
rss 1610612736\n\
xmx 2 g\n\
boot 20000\n\
t 4242 S 12 java\n\
t 4250 R 5400 WiredTickCoordi\n\
t 4251 S 2100 WiredTickShard-\n\
t 4260 S 700 C2 CompilerThre\n";

    #[test]
    fn parses_a_sample() {
        let s = parse_sample(SAMPLE, at(0));
        assert!(s.running());
        assert_eq!(s.pid, 4242);
        assert_eq!(s.ticks_per_second, 100);
        assert_eq!(s.cores, 4);
        assert_eq!(s.machine_ticks, 2345678);
        assert_eq!(s.process_ticks, 81234);
        assert_eq!(s.rss_bytes, 1610612736);
        assert_eq!(s.max_heap_bytes, 2 << 30);
        // Booted 20000 s ago, the process started 15000 s after boot.
        assert_eq!(s.started_seconds_ago, 5000);
        assert_eq!(s.threads.len(), 4);
        assert_eq!(s.threads[1], JvmThread { id: 4250, name: "WiredTickCoordi".into(), state: 'R', ticks: 5400 });
        assert_eq!(s.threads[3].name, "C2 CompilerThre");
    }

    #[test]
    fn not_running() {
        let s = parse_sample("clk 100\ncores 2\nmachine 99\n", at(0));
        assert!(!s.running());
        assert_eq!(s.started_seconds_ago, 0);
        assert_eq!(s.max_heap_bytes, 0);
        let s = parse_sample("xmx 512 m\nclk 0\n", at(0));
        assert_eq!(s.max_heap_bytes, 512 << 20);
        assert_eq!(s.ticks_per_second, 1);
    }

    #[test]
    fn cpu_between_samples() {
        let first = parse_sample(SAMPLE, at(0));
        let second = parse_sample(
            &SAMPLE.replace("machine 2345678", "machine 2346478").replace("proc 81234", "proc 81434").replace("t 4250 R 5400", "t 4250 R 5500"),
            at(2),
        );
        let (server, core) = second.cpu(&first);
        assert!((server - 25.0).abs() < 1e-9);
        assert!((core - 100.0).abs() < 1e-9);
        let threads = second.thread_cpu(&first);
        assert!((threads[&4250] - 50.0).abs() < 1e-9);
        assert_eq!(threads[&4251], 0.0);
        assert_eq!(first.cpu(&first), (0.0, 0.0));
    }

    #[test]
    fn groups_and_wired_names() {
        assert_eq!(group("WiredTickShard-3"), "Wired");
        assert_eq!(group("HabExec-1"), "Game logic (HabExec)");
        assert_eq!(group("GameServer-io"), "Game connections");
        assert_eq!(group("WsWorker"), "Game connections");
        assert_eq!(group("HikariPool-1 housekeeper"), "Database");
        assert_eq!(group("G1 Young RemSet"), "Garbage collector");
        assert_eq!(group("VM Thread"), "Garbage collector");
        assert_eq!(group("C2 CompilerThread0"), "JIT compiler");
        assert_eq!(group("AsyncAppender-Worker"), "Logging");
        assert_eq!(group("RCONServer"), "RCON");
        assert_eq!(group("main"), "Other");
        assert_eq!(wired_name("WiredTickCoordinator"), "Coordinator");
        assert_eq!(wired_name("WiredTickShard-"), "Shard");
        assert_eq!(wired_name("WiredTickShard-2"), "Shard 2");
        assert_eq!(wired_name("WiredTickHeavy-0"), "Heavy rooms 0");
        assert_eq!(wired_name("WiredTickHeavy-"), "Heavy rooms");
        assert_eq!(wired_name("Other"), "Other");
    }

    #[test]
    fn parses_thread_names() {
        let names = parse_thread_names("4250 WiredTickCoordinator\n4251 WiredTickShard-0 \nx bad\n 7\n");
        assert_eq!(names.len(), 2);
        assert_eq!(names[&4250], "WiredTickCoordinator");
        assert_eq!(names[&4251], "WiredTickShard-0");
    }

    #[test]
    fn parses_the_wired_log() {
        let output = "room 12 3 85 190 2026-10-31T09:12:01 0,2\n\
room 7 5 40 160 2026-10-31T09:10:44 1\n\
room 99 1 300 300 2026-10-31T09:13:00\n\
totals 9 2\n\
recent 2026-10-31T09:13:00 WARN Slow wired task: shard=1, room=99, took=300ms\n\
recent 2026-10-31T09:13:05 WARN Room 99 has 64 wired tasks waiting\n\
name 12\tThe Casino\n\
name 7\tWired lab\n";
        let summary = parse_wired_log(output);
        assert_eq!(summary.slow_tasks, 9);
        assert_eq!(summary.dropped_warnings, 2);
        assert_eq!(summary.recent.len(), 2);
        assert!(summary.recent[1].ends_with("wired tasks waiting"));
        let ids: Vec<i32> = summary.rooms.iter().map(|r| r.room_id).collect();
        assert_eq!(ids, [99, 12, 7]);
        assert_eq!(summary.rooms[0].name, None);
        assert_eq!(summary.rooms[0].shards, "");
        assert_eq!(
            summary.rooms[1],
            SlowWiredRoom {
                room_id: 12,
                name: Some("The Casino".into()),
                count: 3,
                slowest_ms: 85,
                total_ms: 190,
                last_seen: "2026-10-31T09:12:01".into(),
                shards: "0,2".into()
            }
        );
    }
}
