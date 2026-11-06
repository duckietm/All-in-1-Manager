//! What ran on which server: every install step and tool keeps its log in
//! `configuration/history/<server>/<yyyyMMdd-HHmmss>-<title>.log`, with a short header (title, kind, outcome, when,
//! the result line) above the log lines. The History page lists them, newest first; the oldest go once a server has
//! more than [`KEPT_PER_SERVER`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime};

use crate::config_dir;

pub const KEPT_PER_SERVER: usize = 300;
const MAGIC: &str = "# All-in-1 Manager history";
/// The header of runs saved before the rename (All-in-1 Installer): still read.
const OLD_MAGIC: &str = "# All-in-1 Installer history";
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Warning,
    Failed,
    Aborted,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Warning => "warning",
            Outcome::Failed => "failed",
            Outcome::Aborted => "aborted",
        }
    }

    pub fn parse(text: &str) -> Outcome {
        match text {
            "warning" => Outcome::Warning,
            "failed" => Outcome::Failed,
            "aborted" => Outcome::Aborted,
            _ => Outcome::Done,
        }
    }
}

/// One run, from the header of its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub server: String,
    pub title: String,
    /// "step" (Install New Server, an addon) or "tool".
    pub kind: String,
    pub outcome: Outcome,
    pub started: NaiveDateTime,
    pub finished: NaiveDateTime,
    /// The result line (heading and message), when the run had one.
    pub result: String,
}

/// What to save of a run.
pub struct Record<'a> {
    pub server: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub outcome: Outcome,
    pub started: DateTime<Local>,
    pub finished: DateTime<Local>,
    pub result: &'a str,
    pub lines: &'a [String],
}

fn root() -> PathBuf {
    config_dir::directory().join("history")
}

/// A file-name part from a title: letters and digits, the rest '-'.
fn slug(title: &str) -> String {
    let mut slug: String = title.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    slug.chars().take(40).collect()
}

/// One line: the header must not be broken by a line break in a title or result.
fn single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// Saves a run; returns the file.
pub fn save(record: &Record) -> std::io::Result<PathBuf> {
    save_in(&root(), record)
}

pub fn save_in(root: &Path, record: &Record) -> std::io::Result<PathBuf> {
    let folder = root.join(config_dir::safe_name(record.server));
    std::fs::create_dir_all(&folder)?;
    config_dir::restrict_to_owner(&folder, true);
    let stamp = record.started.format("%Y%m%d-%H%M%S");
    let mut path = folder.join(format!("{stamp}-{}.log", slug(record.title)));
    let mut i = 2;
    while path.exists() {
        path = folder.join(format!("{stamp}-{}-{i}.log", slug(record.title)));
        i += 1;
    }
    let mut text = String::new();
    text.push_str(MAGIC);
    text.push('\n');
    for (key, value) in [
        ("title", single_line(record.title)),
        ("kind", record.kind.to_string()),
        ("server", single_line(record.server)),
        ("outcome", record.outcome.as_str().to_string()),
        ("started", record.started.format(TIME_FORMAT).to_string()),
        ("finished", record.finished.format(TIME_FORMAT).to_string()),
        ("result", single_line(record.result)),
    ] {
        text.push_str(&format!("# {key}: {value}\n"));
    }
    text.push('\n');
    for line in record.lines {
        text.push_str(line);
        text.push('\n');
    }
    // The logs can hold host names and paths of the server: owner-only, like the rest of the configuration.
    config_dir::write_private(&path, &text)?;
    prune(&folder, KEPT_PER_SERVER);
    Ok(path)
}

/// Removes the oldest runs of a server beyond `keep`.
fn prune(folder: &Path, keep: usize) {
    let mut files = log_files(folder);
    if files.len() <= keep {
        return;
    }
    files.sort();
    let excess = files.len() - keep;
    for path in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

fn log_files(folder: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(folder)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "log")).collect())
        .unwrap_or_default()
}

/// Reads the header of a run's file.
pub fn read_entry(path: &Path) -> Option<Entry> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines.next()?.ok()?;
    if first != MAGIC && first != OLD_MAGIC {
        return None;
    }
    let mut values = std::collections::HashMap::new();
    for line in lines {
        let line = line.ok()?;
        let Some(rest) = line.strip_prefix("# ") else { break };
        if let Some((key, value)) = rest.split_once(": ") {
            values.insert(key.to_string(), value.to_string());
        }
    }
    let time = |key: &str| values.get(key).and_then(|v| NaiveDateTime::parse_from_str(v, TIME_FORMAT).ok());
    Some(Entry {
        path: path.to_path_buf(),
        server: values.get("server").cloned().unwrap_or_default(),
        title: values.get("title").cloned().unwrap_or_default(),
        kind: values.get("kind").cloned().unwrap_or_default(),
        outcome: Outcome::parse(values.get("outcome").map(String::as_str).unwrap_or("")),
        started: time("started")?,
        finished: time("finished").or_else(|| time("started"))?,
        result: values.get("result").cloned().unwrap_or_default(),
    })
}

/// The log lines of a run (without the header).
pub fn read_log(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines();
    if !matches!(lines.next(), Some(MAGIC | OLD_MAGIC)) {
        return text.lines().map(str::to_string).collect();
    }
    // The header ends at the first empty line.
    lines.by_ref().take_while(|l| !l.is_empty()).for_each(drop);
    lines.map(str::to_string).collect()
}

/// Every run, newest first; only the server's when `server` is set.
pub fn list(server: Option<&str>) -> Vec<Entry> {
    list_in(&root(), server)
}

pub fn list_in(root: &Path, server: Option<&str>) -> Vec<Entry> {
    let folders: Vec<PathBuf> = match server {
        Some(server) => vec![root.join(config_dir::safe_name(server))],
        None => std::fs::read_dir(root).map(|e| e.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default(),
    };
    let mut entries: Vec<Entry> = folders.iter().flat_map(|f| log_files(f)).filter_map(|p| read_entry(&p)).collect();
    entries.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.path.cmp(&a.path)));
    entries
}

/// The servers with a history, as their runs name them.
pub fn servers() -> Vec<String> {
    let mut servers: Vec<String> = list(None).into_iter().map(|e| e.server).collect();
    servers.sort();
    servers.dedup();
    servers
}

/// The history folder (for the Open folder button).
pub fn folder() -> PathBuf {
    root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn record<'a>(server: &'a str, title: &'a str, minute: u32, lines: &'a [String]) -> Record<'a> {
        let at = Local.with_ymd_and_hms(2026, 11, 1, 17, minute, 0).unwrap();
        Record { server, kind: "tool", title, outcome: Outcome::Done, started: at, finished: at, result: "It ran\nwell", lines }
    }

    #[test]
    fn save_list_read_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec!["> Restarting".to_string(), String::new(), "  ok".to_string()];
        let first = save_in(dir.path(), &record("hotel.example.com", "Restart NGINX", 1, &lines)).unwrap();
        save_in(dir.path(), &record("hotel.example.com", "Update system", 2, &lines)).unwrap();
        save_in(dir.path(), &record("[::1]:2222", "Restart all", 3, &lines)).unwrap();

        let all = list_in(dir.path(), None);
        assert_eq!(all.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["Restart all", "Update system", "Restart NGINX"]);
        let one = list_in(dir.path(), Some("hotel.example.com"));
        assert_eq!(one.len(), 2);
        assert_eq!(one[1].result, "It ran well");
        assert_eq!(one[1].kind, "tool");
        // The log keeps its empty lines; the header is not part of it.
        assert_eq!(read_log(&first), lines);
        assert!(first.file_name().unwrap().to_string_lossy().starts_with("20261101-170100-restart-nginx"));

        let folder = dir.path().join("hotel.example.com");
        prune(&folder, 1);
        assert_eq!(list_in(dir.path(), Some("hotel.example.com")).iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["Update system"]);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Restart NGINX"), "restart-nginx");
        assert_eq!(slug("Emulator & wired"), "emulator-wired");
    }
}
