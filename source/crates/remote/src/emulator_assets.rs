//! Client images and group badge parts that the Habbo downloads lack. Polaris ships them in Assets/ of its repository
//! and the installer carries a copy next to its executable. Octane preloads images/loading_icon.png, clear_icon.png and
//! big_arrow.png. If one is missing the client fails to start and logs in with an empty SSO ticket, which the emulator
//! logs as "invalid SSO ticket". Missing files are copied from the server's clone first and the rest are uploaded.
//! Existing files are never replaced. (EmulatorAssetsInstaller.cs)

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

use crate::asset_sync::{matches_pattern, tar_stream};
use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server};

/// A folder of Assets/, found in the Polaris clone and next to the installer, and where it goes in /var/www/gamedata.
/// `source` is relative to Assets/, with '/'; `target` relative to /var/www/gamedata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorAssetFolder {
    pub name: String,
    pub source: String,
    pub target: String,
    pub pattern: String,
    pub recursive: bool,
    pub purpose: String,
}

/// Files one folder misses, split into those the server's Polaris clone has and those only the installer has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorAssetState {
    pub folder: EmulatorAssetFolder,
    pub total: usize,
    pub from_server: Vec<String>,
    pub from_installer: Vec<String>,
}

impl EmulatorAssetState {
    pub fn missing(&self) -> usize {
        self.from_server.len() + self.from_installer.len()
    }
}

fn folder(name: &str, source: &str, target: &str, pattern: &str, recursive: bool, purpose: &str) -> EmulatorAssetFolder {
    EmulatorAssetFolder {
        name: name.into(),
        source: source.into(),
        target: target.into(),
        pattern: pattern.into(),
        recursive,
        purpose: purpose.into(),
    }
}

fn fixed() -> Vec<EmulatorAssetFolder> {
    vec![
        folder(
            "Client images",
            "images",
            "images",
            "*",
            true,
            "loading_icon / clear_icon / big_arrow (Octane preloads them), wallet, navigator and catalogue extras",
        ),
        folder(
            "Hotel view",
            "images/reception",
            "c_images/reception",
            "*.png",
            false,
            "the hotel view's background, sun, drape and sides (ui-config's c_images/reception)",
        ),
        folder(
            "Group badge parts",
            "badgeparts",
            "c_images/groupbadge/badgeparts",
            "badgepart_*.png",
            false,
            "the parts of group badges, for Octane and the emulator's badge imager",
        ),
    ]
}

fn sorted_subdirectories(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The fixed folders, then every other top-level folder in the installer's Assets/, mapped to the same path in
/// gamedata. Adding a folder there is enough to have it uploaded.
pub fn folders() -> Vec<EmulatorAssetFolder> {
    folders_in(&local_assets())
}

/// Like [`folders`], for the Assets folder at `local`.
pub fn folders_in(local: &Path) -> Vec<EmulatorAssetFolder> {
    let mut folders = fixed();
    if !local.is_dir() {
        return folders;
    }
    let taken: HashSet<String> = folders.iter().map(|f| f.source.split('/').next().unwrap_or("").to_string()).collect();
    for name in sorted_subdirectories(local) {
        if taken.contains(&name) || name.starts_with('.') {
            continue;
        }
        let parts = sorted_subdirectories(&local.join(&name));
        let purpose = if parts.is_empty() { format!("Assets/{name} → gamedata/{name}") } else { parts.join(", ") };
        let title = if name == "c_images" { "Game images (c_images)".to_string() } else { name.clone() };
        folders.push(folder(&title, &name, &name, "*", true, &purpose));
    }
    folders
}

pub fn server_assets() -> String {
    format!("{}/Assets", crate::emulator::repository())
}

/// Assets/ next to the installer's executable.
pub fn local_assets() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)).unwrap_or_default().join("Assets")
}

fn server_source(f: &EmulatorAssetFolder) -> String {
    format!("{}/{}", server_assets(), f.source)
}

fn target(f: &EmulatorAssetFolder) -> String {
    format!("{}/{}", crate::octane::gamedata(), f.target)
}

fn local_source(local: &Path, f: &EmulatorAssetFolder) -> PathBuf {
    let mut path = local.to_path_buf();
    for part in f.source.split('/') {
        path.push(part);
    }
    path
}

fn find(directory: &str, f: &EmulatorAssetFolder, source_filter: bool) -> String {
    let q = shell_quote(directory);
    format!(
        "[ -d {q} ] && (cd {q} && find . {}-type f {}-printf '%P\\n')",
        if f.recursive { "" } else { "-maxdepth 1 " },
        if source_filter { format!("-name {} ! -iname 'README*' ", shell_quote(&f.pattern)) } else { String::new() }
    )
}

fn usable(name: &str) -> bool {
    !name.is_empty() && !name.split('/').any(|p| p.starts_with('.')) && !name.chars().any(char::is_control)
}

fn local_files(local: &Path, f: &EmulatorAssetFolder) -> Vec<String> {
    let root = local_source(local, f);
    let mut files = Vec::new();
    let mut pending = std::collections::VecDeque::from([root.clone()]);
    while let Some(directory) = pending.pop_front() {
        let Ok(entries) = std::fs::read_dir(&directory) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if f.recursive {
                    pending.push_back(path);
                }
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !matches_pattern(&f.pattern, &file_name) || file_name.to_ascii_uppercase().starts_with("README") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&root) else { continue };
            let name = relative.to_string_lossy().replace('\\', "/");
            if usable(&name) {
                files.push(name);
            }
        }
    }
    files
}

pub async fn inspect(server: &dyn Server) -> Result<Vec<EmulatorAssetState>> {
    inspect_in(server, &local_assets()).await
}

/// Like [`inspect`], for the Assets folder at `local` (the images packed into the program, extracted).
pub async fn inspect_in(server: &dyn Server, local: &Path) -> Result<Vec<EmulatorAssetState>> {
    let folders = folders_in(local);
    let mut script = folders
        .iter()
        .enumerate()
        .map(|(i, f)| format!("echo '== S{i}'; {}\necho '== T{i}'; {}", find(&server_source(f), f, true), find(&target(f), f, false)))
        .collect::<Vec<_>>()
        .join("\n");
    script.push_str("\ntrue");
    let result = server.run_as_root(&script).await?;
    let lists = parse_lists(&result.output);

    let empty = HashSet::new();
    let mut states = Vec::new();
    for (i, f) in folders.into_iter().enumerate() {
        let server_files = lists.get(&format!("S{i}")).unwrap_or(&empty);
        let target_files = lists.get(&format!("T{i}")).unwrap_or(&empty);
        let local_set: HashSet<String> = local_files(local, &f).into_iter().collect();
        let total = server_files.union(&local_set).count();
        let from_server: BTreeSet<&String> = server_files.iter().filter(|n| !target_files.contains(*n)).collect();
        let from_installer: BTreeSet<&String> =
            local_set.iter().filter(|n| !server_files.contains(*n) && !target_files.contains(*n)).collect();
        states.push(EmulatorAssetState {
            folder: f,
            total,
            from_server: from_server.into_iter().cloned().collect(),
            from_installer: from_installer.into_iter().cloned().collect(),
        });
    }
    Ok(states)
}

fn parse_lists(output: &str) -> HashMap<String, HashSet<String>> {
    let mut lists: HashMap<String, HashSet<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.split('\n') {
        let text = line.trim_end_matches('\r');
        if let Some(key) = text.strip_prefix("== ") {
            lists.insert(key.to_string(), HashSet::new());
            current = Some(key.to_string());
        } else if let Some(key) = &current {
            if usable(text) {
                lists.get_mut(key).unwrap().insert(text.to_string());
            }
        }
    }
    lists
}

const COPY: &str = r#"src={{src}}
dst={{dst}}
install -d -m 2775 -o www-data -g www-data "$dst"
n=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if [ ! -e "$dst/$f" ]; then
    install -d -m 2775 -o www-data -g www-data "$(dirname "$dst/$f")"
    install -m 664 -o www-data -g www-data "$src/$f" "$dst/$f"
    n=$((n + 1))
  fi
done <<'ALL_IN_1_FILES'
{{files}}
ALL_IN_1_FILES
echo "  {{name}}: $n copied from $src to $dst"
echo "ADDED=$n""#;

const UPLOAD: &str = r#"set -e
install -d -m 2775 -o www-data -g www-data {{target}}
# --skip-old-files keeps files that are already there.
tar -x -f - -C {{target}} --no-same-owner --no-same-permissions --skip-old-files
find {{target}} -user root -exec chown www-data:www-data {} +
echo OK"#;

/// Adds the missing files without replacing any and returns how many were added.
pub async fn install(server: &dyn Server, log: Log<'_>) -> Result<i64> {
    install_in(server, &local_assets(), log).await
}

/// Like [`install`], for the Assets folder at `local`.
pub async fn install_in(server: &dyn Server, local: &Path, log: Log<'_>) -> Result<i64> {
    let local = local.to_path_buf();
    let states = inspect_in(server, &local).await?;
    let mut added = 0i64;

    // Copy what the server's Polaris clone has, so those files need no upload.
    let copies = states
        .iter()
        .filter(|s| !s.from_server.is_empty())
        .map(|s| {
            fill(
                COPY,
                &[
                    ("src", &shell_quote(&server_source(&s.folder))),
                    ("dst", &shell_quote(&target(&s.folder))),
                    ("name", &s.folder.name),
                    ("files", &s.from_server.join("\n")),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !copies.is_empty() {
        let copied = AtomicI64::new(0);
        let on_line = |line: &str| {
            if let Some(n) = line.trim().strip_prefix("ADDED=") {
                copied.fetch_add(n.parse::<i32>().map(i64::from).unwrap_or(0), Ordering::Relaxed);
            } else {
                log(line);
            }
        };
        script::run(server, &script::with_prelude(&copies), &on_line, "Copying the emulator's assets failed", None).await?;
        added += copied.load(Ordering::Relaxed);
    }

    // Upload the rest, including everything when there is no clone, from Assets/ next to the installer.
    for state in states.iter().filter(|s| !s.from_installer.is_empty()) {
        let target_path = target(&state.folder);
        let script = fill(UPLOAD, &[("target", &shell_quote(&target_path))]);
        let root = local_source(&local, &state.folder);
        let items: Vec<tar_stream::TarItem> = state
            .from_installer
            .iter()
            .map(|name| {
                let mut path = root.clone();
                for part in name.split('/') {
                    path.push(part);
                }
                tar_stream::TarItem::file(&path, name)
            })
            .collect();
        let result = tar_stream::upload(server, &script, &items, &|_| {}).await?;
        if !result.succeeded() || !result.output.contains("OK") {
            crate::fail!(
                "Uploading {} failed: {}",
                state.folder.name.to_lowercase(),
                format!("{}{}", result.output, result.error).trim()
            );
        }
        log(&format!("    {}: {} uploaded from the installer to {target_path}", state.folder.name, state.from_installer.len()));
        added += state.from_installer.len() as i64;
    }

    if added > 0 {
        let p = crate::octane::PERMISSIONS_SCRIPT;
        script::run(server, &format!("if [ -x {p} ]; then {p} >/dev/null; fi"), log, "Setting the permissions failed", None).await?;
    }
    if added == 0 {
        log("    Nothing missing.");
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_folders_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path();
        for path in ["images/sub/a.png", "images/README.md", "images/.hidden/x.png", "badgeparts/badgepart_1.png", "badgeparts/other.png", "c_images/b/c.gif", "extra/d.txt"] {
            let full = assets.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, "x").unwrap();
        }
        std::fs::create_dir_all(assets.join(".git")).unwrap();
        let folders = folders_in(assets);
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Client images", "Hotel view", "Group badge parts", "Game images (c_images)", "extra"]);
        assert_eq!(folders[3].purpose, "b");
        assert_eq!(folders[4].purpose, "Assets/extra → gamedata/extra");
        assert_eq!(local_files(assets, &folders[0]), vec!["sub/a.png".to_string()]);
        assert_eq!(local_files(assets, &folders[2]), vec!["badgepart_1.png".to_string()]);
        assert_eq!(local_files(assets, &folders[3]), vec!["b/c.gif".to_string()]);
    }

    #[test]
    fn lists() {
        let lists = parse_lists("junk\n== S0\na.png\r\n.x/b.png\n== T0\n\na.png\n");
        assert_eq!(lists["S0"].len(), 1);
        assert!(lists["T0"].contains("a.png"));
        assert!(usable("a/b.png") && !usable("a/.b") && !usable(""));
    }
}
