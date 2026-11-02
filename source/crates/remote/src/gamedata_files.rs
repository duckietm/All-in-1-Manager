//! The Asset Explorer's server side, limited to /var/www/gamedata. Paths are checked twice. Here, names may not
//! contain ".." or separators. In every script, realpath follows symlinks and must stay inside gamedata, so neither a
//! crafted name nor a symlink reaches anything else. Uploads get owner www-data, folders 2775 and files 664.
//! (GamedataFiles.cs)

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::asset_sync::tar_stream::{self, TarItem};
use crate::script::fill;
use crate::server::{shell_quote, CommandResult, Progress, Result, Server, ServerError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_directory: bool,
    pub is_link: bool,
    pub size: i64,
    pub modified: DateTime<Local>,
}

pub fn root() -> String {
    crate::octane::gamedata()
}

// "inside PATH" prints the real path, or fails when it resolves outside gamedata, symlinks included.
const GUARD: &str = r#"set -e
# Called in $(...), so the message goes to stderr and set -e stops the script on failure.
inside() {
  r=$(realpath -m -- "$1") || { echo "ERROR: invalid path" >&2; exit 3; }
  case "$r" in
    {{Root}}|{{Root}}/*) printf '%s' "$r" ;;
    *) echo "ERROR: that path is outside {{Root}}" >&2; exit 3 ;;
  esac
}
# The web server and the emulator, both in group www-data, may read and change uploaded files.
fix_rights() {
  chown -R www-data:www-data -- "$1"
  find "$1" -type d -exec chmod 2775 {} +
  find "$1" -type f -exec chmod 664 {} +
}
"#;

fn guard() -> String {
    fill(GUARD, &[("Root", &root())])
}

/// Maps "a/b" to "/var/www/gamedata/a/b" and fails for anything that would leave gamedata.
pub fn absolute(relative: &str) -> Result<String> {
    let mut parts = Vec::new();
    for part in relative.split(['/', '\\']).filter(|p| !p.is_empty()) {
        if part == "." {
            continue;
        }
        if part == ".." {
            return Err(ServerError("Paths outside gamedata are not allowed.".into()).into());
        }
        parts.push(part);
    }
    Ok(if parts.is_empty() { root() } else { format!("{}/{}", root(), parts.join("/")) })
}

/// Returns why a single file or folder name is not allowed, or None when it is fine.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("Enter a name.");
    }
    if name == "." || name == ".." {
        return Some("That name is not allowed.");
    }
    if name.contains(['/', '\\']) {
        return Some("A name cannot contain / or \\.");
    }
    if name.chars().any(char::is_control) {
        return Some("A name cannot contain control characters.");
    }
    if name.len() > 255 {
        return Some("That name is too long.");
    }
    None
}

fn q(text: &str) -> String {
    shell_quote(text)
}

fn check_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<()> {
    for name in names {
        if let Some(problem) = name_problem(name) {
            return Err(ServerError(format!("{problem} ({name})")).into());
        }
    }
    Ok(())
}

// Prefers the script's own "ERROR: ..." line over everything it printed.
fn error_of(result: &CommandResult) -> String {
    let all = format!("{}\n{}", result.output, result.error);
    all.split('\n')
        .map(str::trim)
        .filter(|l| l.starts_with("ERROR: "))
        .next_back()
        .map(|l| l[7..].to_string())
        .unwrap_or_else(|| format!("{}{}", result.output, result.error).trim().to_string())
}

async fn run(server: &dyn Server, script: &str, failure: &str) -> Result<CommandResult> {
    let result = server.run_as_root(&(guard() + script)).await?;
    if !result.succeeded() {
        crate::fail!("{failure}: {}", error_of(&result));
    }
    Ok(result)
}

const LIST: &str = r#"d=$(inside {{dir}})
[ -d "$d" ] || { echo "ERROR: not a folder"; exit 3; }
cd "$d" && find . -mindepth 1 -maxdepth 1 -printf '%y\t%s\t%T@\t%f\n'"#;

pub async fn list(server: &dyn Server, relative: &str) -> Result<Vec<RemoteEntry>> {
    let result = run(server, &fill(LIST, &[("dir", &q(&absolute(relative)?))]), "Reading the folder failed").await?;
    Ok(parse_list(&result.output))
}

fn parse_list(output: &str) -> Vec<RemoteEntry> {
    let mut entries = Vec::new();
    for line in output.split('\n') {
        let f: Vec<&str> = line.splitn(4, '\t').collect();
        if f.len() < 4 || f[3].is_empty() || f[3].chars().any(char::is_control) {
            continue;
        }
        let size = f[1].parse::<i64>().unwrap_or(0);
        let seconds = f[2].trim().parse::<f64>().unwrap_or(0.0);
        let modified = DateTime::from_timestamp_millis((seconds * 1000.0) as i64).unwrap_or_default().with_timezone(&Local);
        entries.push(RemoteEntry { name: f[3].to_string(), is_directory: f[0] == "d", is_link: f[0] == "l", size, modified });
    }
    entries
}

const CREATE_FOLDER: &str = r#"p=$(inside {{path}})
[ ! -e "$p" ] || { echo "ERROR: "{{name}}" already exists"; exit 3; }
install -d -m 2775 -o www-data -g www-data "$p""#;

pub async fn create_folder(server: &dyn Server, relative: &str, name: &str) -> Result<()> {
    check_names([name])?;
    let path = q(&format!("{}/{name}", absolute(relative)?));
    run(server, &fill(CREATE_FOLDER, &[("path", &path), ("name", &q(name))]), "Creating the folder failed").await?;
    Ok(())
}

const RENAME: &str = r#"a=$(inside {{a}})
b=$(inside {{b}})
[ "$a" != "{{Root}}" ] || { echo "ERROR: gamedata itself cannot be renamed"; exit 3; }
[ ! -e "$b" ] || { echo "ERROR: "{{to}}" already exists"; exit 3; }
mv -- "$a" "$b""#;

pub async fn rename(server: &dyn Server, relative: &str, from: &str, to: &str) -> Result<()> {
    check_names([from, to])?;
    let dir = absolute(relative)?;
    let script = fill(
        RENAME,
        &[("Root", &root()), ("a", &q(&format!("{dir}/{from}"))), ("b", &q(&format!("{dir}/{to}"))), ("to", &q(to))],
    );
    run(server, &script, "Renaming failed").await?;
    Ok(())
}

const DELETE: &str = r#"p=$(inside {{path}})
[ "$p" != "{{Root}}" ] || { echo "ERROR: gamedata itself cannot be deleted"; exit 3; }
rm -rf -- "$p""#;

pub async fn delete(server: &dyn Server, relative: &str, names: &[String]) -> Result<()> {
    check_names(names.iter().map(String::as_str))?;
    let dir = absolute(relative)?;
    let script = names
        .iter()
        .map(|name| fill(DELETE, &[("Root", &root()), ("path", &q(&format!("{dir}/{name}")))]))
        .collect::<Vec<_>>()
        .join("\n");
    run(server, &script, "Deleting failed").await?;
    Ok(())
}

/// The files and their sizes under the given entries of a folder, recursing into folders, for a download.
pub async fn files_under(server: &dyn Server, relative: &str, names: &[String]) -> Result<Vec<(String, i64)>> {
    check_names(names.iter().map(String::as_str))?;
    let dir = absolute(relative)?;
    let finds = names
        .iter()
        .map(|n| format!("p=$(inside {}); find {} -type f -printf '%s\\t%p\\n'", q(&format!("{dir}/{n}")), q(&format!("./{n}"))))
        .collect::<Vec<_>>()
        .join("\n");
    let script = format!("d=$(inside {})\ncd \"$d\"\n{finds}", q(&dir));
    let result = run(server, &script, "Reading the files failed").await?;
    Ok(parse_files(&result.output))
}

fn parse_files(output: &str) -> Vec<(String, i64)> {
    let mut files = Vec::new();
    for line in output.split('\n') {
        let f: Vec<&str> = line.splitn(2, '\t').collect();
        if f.len() != 2 {
            continue;
        }
        let Ok(size) = f[0].parse::<i64>() else { continue };
        if f[1].chars().any(char::is_control) || f[1].split('/').any(|p| p == "..") {
            continue;
        }
        files.push((f[1].strip_prefix("./").unwrap_or(f[1]).to_string(), size));
    }
    files
}

/// The last part of a path, ignoring a trailing separator (C#'s GetFileName(TrimEndingDirectorySeparator(p))).
fn top_name(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', '\\']);
    let trimmed = if trimmed.is_empty() { text.as_ref() } else { trimmed };
    Path::new(trimmed).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Symlinks and other reparse points are left out, like .NET's AttributesToSkip = ReparsePoint.
fn is_reparse_point(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return true };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// The tar entries of the local files and folders: a folder, then everything below it, breadth first like .NET's
/// recursive enumeration.
fn tar_items(local_paths: &[PathBuf]) -> Vec<TarItem> {
    let mut items = Vec::new();
    for path in local_paths {
        let top = top_name(path);
        if path.is_dir() {
            items.push(TarItem::directory(&format!("{top}/")));
            let mut pending = VecDeque::from([path.clone()]);
            while let Some(directory) = pending.pop_front() {
                // Unreadable folders are skipped (IgnoreInaccessible).
                let Ok(entries) = std::fs::read_dir(&directory) else { continue };
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if is_reparse_point(&entry_path) {
                        continue;
                    }
                    let Ok(relative) = entry_path.strip_prefix(path) else { continue };
                    let name = format!("{top}/{}", relative.to_string_lossy().replace('\\', "/"));
                    let is_dir = entry_path.is_dir();
                    if is_dir {
                        pending.push_back(entry_path.clone());
                    }
                    if name.chars().any(char::is_control) {
                        continue;
                    }
                    if is_dir {
                        items.push(TarItem::directory(&format!("{name}/")));
                    } else {
                        items.push(TarItem::file(&entry_path, &name));
                    }
                }
            }
        } else {
            items.push(TarItem::file(path, &top));
        }
    }
    items
}

const UPLOAD: &str = r#"set -e
d=$(inside {{target}})
[ -d "$d" ] || { echo "ERROR: the target is not a folder"; exit 3; }
tar -x -f - -C "$d" --no-same-owner --no-same-permissions {{mode}}
{{fixes}}
echo OK"#;

/// Uploads local files and folders into a gamedata folder as one tar stream. Existing files are replaced unless
/// `skip_existing` is set. Afterwards every uploaded item gets the gamedata rights. `progress` gets the bytes sent.
pub async fn upload(server: &dyn Server, local_paths: &[PathBuf], relative: &str, skip_existing: bool, progress: Progress<'_>) -> Result<()> {
    let tops: Vec<String> = local_paths.iter().map(|p| top_name(p)).collect();
    if tops.iter().any(|t| name_problem(t).is_some()) {
        crate::fail!("A file name cannot be uploaded (control characters or too long).");
    }
    let target = absolute(relative)?;
    let fixes = tops
        .iter()
        .map(|t| format!("p=$(inside {}); [ ! -e \"$p\" ] || fix_rights \"$p\"", q(&format!("{target}/{t}"))))
        .collect::<Vec<_>>()
        .join("\n");
    let script = guard()
        + &fill(
            UPLOAD,
            &[("target", &q(&target)), ("mode", if skip_existing { "--skip-old-files" } else { "--overwrite" }), ("fixes", &fixes)],
        );
    let items = tar_items(local_paths);
    let result = tar_stream::upload(server, &script, &items, progress).await?;
    if !result.succeeded() || !result.output.contains("OK") {
        crate::fail!("Uploading failed: {}", error_of(&result));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let root = root();
        assert_eq!(absolute("").unwrap(), root);
        assert_eq!(absolute("/a/./b\\c/").unwrap(), format!("{root}/a/b/c"));
        assert_eq!(absolute("a/../b").unwrap_err().to_string(), "Paths outside gamedata are not allowed.");
    }

    #[test]
    fn names() {
        assert_eq!(name_problem(" "), Some("Enter a name."));
        assert_eq!(name_problem(".."), Some("That name is not allowed."));
        assert_eq!(name_problem("a/b"), Some("A name cannot contain / or \\."));
        assert_eq!(name_problem("a\u{1}"), Some("A name cannot contain control characters."));
        assert_eq!(name_problem(&"é".repeat(128)), Some("That name is too long."));
        assert_eq!(name_problem("badge.gif"), None);
        assert_eq!(check_names(["ok", "a/b"]).unwrap_err().to_string(), "A name cannot contain / or \\. (a/b)");
    }

    #[test]
    fn errors() {
        let result = CommandResult { exit_code: 3, output: "x\nERROR: first\n".into(), error: "ERROR: that path is outside /g\n".into() };
        assert_eq!(error_of(&result), "that path is outside /g");
        let result = CommandResult { exit_code: 1, output: " out ".into(), error: "err\n".into() };
        assert_eq!(error_of(&result), "out err");
    }

    #[test]
    fn parsing() {
        let entries = parse_list("d\t4096\t1700000000.5\tfolder\nf\t12\tx\tfile name\nl\t1\t0\tlink\nbad\nf\t1\t1\t\n");
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_directory && !entries[0].is_link);
        assert_eq!(entries[0].modified.timestamp_millis(), 1_700_000_000_500);
        assert_eq!(entries[1].name, "file name");
        assert!(entries[2].is_link);
        let files = parse_files("12\t./a/b.png\nx\t./c\n3\t./../d\n4\tplain\n");
        assert_eq!(files, vec![("a/b.png".to_string(), 12), ("plain".to_string(), 4)]);
    }

    #[test]
    fn upload_entries() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("pack");
        std::fs::create_dir_all(top.join("sub/deeper")).unwrap();
        std::fs::write(top.join("a.png"), "a").unwrap();
        std::fs::write(top.join("sub/b.png"), "bb").unwrap();
        std::fs::write(top.join("sub/deeper/c.png"), "ccc").unwrap();
        let single = dir.path().join("single.gif");
        std::fs::write(&single, "s").unwrap();
        let mut slashed = top.clone().into_os_string();
        slashed.push("/");
        let items = tar_items(&[PathBuf::from(slashed), single.clone()]);
        let names: Vec<String> = items
            .iter()
            .map(|i| match i {
                TarItem::File { name, .. } => name.clone(),
                TarItem::Directory { name } => name.clone(),
            })
            .collect();
        let mut sorted_tail = names[1..5].to_vec();
        sorted_tail.sort();
        assert_eq!(names[0], "pack/");
        assert_eq!(sorted_tail, ["pack/a.png", "pack/sub/", "pack/sub/b.png", "pack/sub/deeper/"]);
        assert_eq!(names[5], "pack/sub/deeper/c.png");
        assert_eq!(names[6], "single.gif");
        assert!(matches!(&items[6], TarItem::File { path, .. } if *path == single));
    }

    #[tokio::test]
    async fn upload_tar() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("pack");
        std::fs::create_dir_all(&top).unwrap();
        std::fs::write(top.join("a.png"), vec![1u8; 700]).unwrap();
        let bytes = tar_stream::to_bytes(&tar_items(&[top])).await;
        let mut archive = tar::Archive::new(&bytes[..]);
        let list: Vec<(String, u64, u32, tar::EntryType)> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.path().unwrap().to_string_lossy().into_owned(), e.header().size().unwrap(), e.header().mode().unwrap(), e.header().entry_type())
            })
            .collect();
        assert_eq!(list[0].0.trim_end_matches('/'), "pack");
        assert_eq!((list[0].2, list[0].3), (0o755, tar::EntryType::Directory));
        assert_eq!((list[1].0.as_str(), list[1].1), ("pack/a.png", 700));
        assert_eq!(bytes.len(), 3 * 512 + (3 + 2) * 512 + 2 * 512);
    }
}
