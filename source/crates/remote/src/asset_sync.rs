//! Copies the assets the All-in-1 downloader (github.com/duckietm/all-in-1-converter) put in its Habbo_Default folder
//! to /var/www/gamedata, in the layout Octane's configuration uses. Only new and changed files are sent, as one tar
//! stream per folder over the SSH session; nothing on the server is deleted. (AssetSync.cs)
//!
//! [`tar_stream`] is the tar writer of every upload (this module, emulator_assets, gamedata_files).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::script;
use crate::server::{shell_quote, Log, Progress, Result, Server};

/// A folder of the All-in-1 downloader and where its files go in /var/www/gamedata.
/// `source` is relative to the downloader's folder, with '/'; `target` relative to /var/www/gamedata. `only`: file
/// names to take (None takes every file matching `pattern`). `add_only`: only add missing files and leave existing
/// ones alone unless asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMapping {
    pub name: String,
    pub source: String,
    pub target: String,
    pub pattern: String,
    pub only: Option<Vec<String>>,
    pub add_only: bool,
}

impl AssetMapping {
    fn new(name: &str, source: &str, target: &str, pattern: &str) -> Self {
        Self { name: name.into(), source: source.into(), target: target.into(), pattern: pattern.into(), only: None, add_only: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub name: String,
    pub path: PathBuf,
    pub size: i64,
    /// Unix seconds.
    pub modified: i64,
}

/// Files one folder needs to upload. A file counts as changed when its size differs or the local copy is newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPlan {
    pub mapping: AssetMapping,
    pub source_path: PathBuf,
    pub local_count: usize,
    pub local_bytes: i64,
    pub new: Vec<LocalFile>,
    pub changed: Vec<LocalFile>,
}

impl AssetPlan {
    pub fn upload(&self) -> impl Iterator<Item = &LocalFile> {
        self.new.iter().chain(self.changed.iter())
    }

    pub fn upload_bytes(&self) -> i64 {
        self.upload().map(|f| f.size).sum()
    }

    pub fn upload_count(&self) -> usize {
        self.new.len() + self.changed.len()
    }
}

pub const DEFAULT_FOLDER: &str = "Habbo_Default";

/// Config files Octane loads from gamedata/config.
pub const CONFIG_FILES: [&str; 7] =
    ["FurnitureData.json", "ProductData.json", "FigureData.json", "FigureMap.json", "EffectMap.json", "HabboAvatarActions.json", "ExternalTexts.json"];

pub fn mappings() -> Vec<AssetMapping> {
    vec![
        AssetMapping::new("Furniture", "Habbo_Default/hof_furni/hab", "furniture", "*.hab"),
        AssetMapping::new("Furniture icons", "Habbo_Default/hof_furni/icons", "icons", "*.png"),
        AssetMapping::new("Clothes", "Habbo_Default/clothes/hab", "clothes", "*.hab"),
        AssetMapping::new("Effects", "Habbo_Default/effects/hab", "effect", "*.hab"),
        AssetMapping::new("Pets", "Habbo_Default/pets/hab", "pets", "*.hab"),
        AssetMapping::new("Badges", "Habbo_Default/badges", "c_images/album1584", "*"),
        AssetMapping::new("Catalogue icons", "Habbo_Default/icons", "c_images/catalogue", "*.png"),
        AssetMapping::new("Effect icons", "Habbo_Default/fx_icons", "c_images/fx_icons", "*.png"),
        AssetMapping::new("Quest images", "Habbo_Default/quests", "c_images/Quests", "*"),
        AssetMapping::new("Hotel view", "Habbo_Default/reception", "c_images/reception", "*"),
        AssetMapping::new("Promo images", "Habbo_Default/reception/web_promo_small", "c_images/web_promo_small", "*"),
        AssetMapping::new("Sounds", "Habbo_Default/mp3", "sounds", "*.mp3"),
        // Only added when missing, because the emulator edits ExternalTexts.json for custom badges and hotels edit the others.
        AssetMapping {
            only: Some(CONFIG_FILES.iter().map(|s| s.to_string()).collect()),
            add_only: true,
            ..AssetMapping::new("Game data (config JSON)", "Habbo_Default/files/json", "config", "*.json")
        },
    ]
}

/// The folder holding Habbo_Default for the downloader chosen in Settings, or None.
pub fn downloader_folder(executable: Option<&str>) -> Option<PathBuf> {
    let executable = executable.filter(|e| !e.trim().is_empty())?;
    let path = Path::new(executable);
    let folder = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    folder.join(DEFAULT_FOLDER).is_dir().then_some(folder)
}

/// A `Directory.EnumerateFiles` search pattern: `*` and `?`, case-insensitive on Windows like its file system.
pub(crate) fn matches_pattern(pattern: &str, name: &str) -> bool {
    fn fold(text: &str) -> Vec<char> {
        if cfg!(windows) { text.chars().flat_map(char::to_lowercase).collect() } else { text.chars().collect() }
    }
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    let pattern = if pattern == "*.*" { "*" } else { pattern };
    go(&fold(pattern), &fold(name))
}

/// Unix seconds of a file's last write (C#'s ToUnixTimeSeconds).
pub(crate) fn unix_seconds(time: std::time::SystemTime) -> i64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
    }
}

fn scan(folder: &Path, mapping: &AssetMapping) -> Vec<LocalFile> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !matches_pattern(&mapping.pattern, &name) {
            continue;
        }
        if let Some(only) = &mapping.only {
            if !only.contains(&name) {
                continue;
            }
        }
        // Skip names that would not survive tar or a shell.
        if name.starts_with('.') || name.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
            continue;
        }
        let modified = metadata.modified().map(unix_seconds).unwrap_or(0);
        files.push(LocalFile { name, path: entry.path(), size: metadata.len() as i64, modified });
    }
    files
}

/// Where Deploy Assets copies from: a folder on this computer and its mappings to /var/www/gamedata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The folder shown in the window.
    pub label: PathBuf,
    /// What the mappings' `source` paths are relative to.
    pub base: PathBuf,
    pub mappings: Vec<AssetMapping>,
}

/// Get Assets' folder (`<assets>/gamedata`, laid out like the server's gamedata) as one mapping per folder that holds
/// files, each to the same folder on the server. config/ is only added when missing, like the downloader's JSON.
pub fn gamedata_mappings(assets_folder: &Path) -> Vec<AssetMapping> {
    let root = assets_folder.join(crate::habbo_assets::GAMEDATA);
    let mut folders = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(folder) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        let mut has_files = false;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => has_files = true,
                _ => {}
            }
        }
        if has_files && folder != root {
            let relative: Vec<String> =
                folder.strip_prefix(&root).unwrap_or(&folder).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            folders.push(relative.join("/"));
        }
    }
    folders.sort();
    folders
        .into_iter()
        .map(|target| AssetMapping {
            add_only: target == "config",
            ..AssetMapping::new(&target, &format!("{}/{target}", crate::habbo_assets::GAMEDATA), &target, "*")
        })
        .collect()
}

/// What Deploy Assets copies from: Get Assets' folder when it holds files, then the All-in-1 downloader's
/// Habbo_Default when Settings name it.
pub fn sources(assets_folder: &Path, downloader_executable: Option<&str>) -> Vec<Source> {
    let mut sources = Vec::new();
    let own = gamedata_mappings(assets_folder);
    if !own.is_empty() {
        sources.push(Source { label: assets_folder.join(crate::habbo_assets::GAMEDATA), base: assets_folder.to_path_buf(), mappings: own });
    }
    if let Some(folder) = downloader_folder(downloader_executable) {
        sources.push(Source { label: folder.join(DEFAULT_FOLDER), base: folder, mappings: mappings() });
    }
    sources
}

/// Compares every folder with the server using a single listing command (the All-in-1 downloader alone).
pub async fn plan(server: &dyn Server, downloader_folder: &Path, overwrite_config: bool) -> Result<Vec<AssetPlan>> {
    let source = Source { label: downloader_folder.join(DEFAULT_FOLDER), base: downloader_folder.to_path_buf(), mappings: mappings() };
    plan_sources(server, &[source], overwrite_config).await
}

/// Compares every folder of every source with the server using a single listing command. A file an earlier source
/// already sends to the same place is not sent again by a later one.
pub async fn plan_sources(server: &dyn Server, sources: &[Source], overwrite_config: bool) -> Result<Vec<AssetPlan>> {
    let all: Vec<(&Path, &AssetMapping)> = sources.iter().flat_map(|s| s.mappings.iter().map(move |m| (s.base.as_path(), m))).collect();
    let gamedata = crate::octane::gamedata();
    let mut script: String = all
        .iter()
        .enumerate()
        .map(|(i, (_, m))| {
            format!(
                "echo '== {i}'; find {} -maxdepth 1 -type f -printf '%f\\t%s\\t%T@\\n' 2>/dev/null",
                shell_quote(&format!("{gamedata}/{}", m.target))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    script.push_str("\ntrue");
    let result = server.run_as_root(&script).await?;
    let remote = parse_listing(&result.output);

    let mut planned: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut plans = Vec::new();
    for (i, (base, mapping)) in all.into_iter().enumerate() {
        let mut source = base.to_path_buf();
        for part in mapping.source.split('/') {
            source.push(part);
        }
        let local = scan(&source, mapping);
        let empty = HashMap::new();
        let there = remote.get(&i).unwrap_or(&empty);
        let mut fresh = Vec::new();
        let mut changed = Vec::new();
        for file in &local {
            if planned.contains(&(mapping.target.clone(), file.name.clone())) {
                continue;
            }
            let chosen = match there.get(&file.name) {
                None => {
                    fresh.push(file.clone());
                    true
                }
                Some(&(size, modified)) => {
                    let differs = (!mapping.add_only || overwrite_config) && (size != file.size || file.modified as f64 > modified + 2.0);
                    if differs {
                        changed.push(file.clone());
                    }
                    differs
                }
            };
            if chosen {
                planned.insert((mapping.target.clone(), file.name.clone()));
            }
        }
        plans.push(AssetPlan {
            local_count: local.len(),
            local_bytes: local.iter().map(|f| f.size).sum(),
            mapping: mapping.clone(),
            source_path: source,
            new: fresh,
            changed,
        });
    }
    Ok(plans)
}

/// "== i" headers, then "name\tsize\tmtime" lines per folder.
fn parse_listing(output: &str) -> HashMap<usize, HashMap<String, (i64, f64)>> {
    let mut remote: HashMap<usize, HashMap<String, (i64, f64)>> = HashMap::new();
    let mut current: Option<usize> = None;
    for line in output.split('\n') {
        if let Some(index) = line.strip_prefix("== ") {
            current = index.trim().parse().ok();
            if let Some(i) = current {
                remote.insert(i, HashMap::new());
            }
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let (Some(i), true) = (current, f.len() >= 3) else { continue };
        let size = f[1].parse::<i64>().unwrap_or(-1);
        let modified = f[2].trim().parse::<f64>().unwrap_or(0.0);
        remote.entry(i).or_default().insert(f[0].to_string(), (size, modified));
    }
    remote
}

/// Free bytes on the file system holding /var/www/gamedata, or /var/www if that does not exist yet.
pub async fn free_space(server: &dyn Server) -> Result<i64> {
    let command = format!(
        "d={}; [ -d \"$d\" ] || d=/var/www; df -P -B1 \"$d\" | awk 'NR == 2 {{ print $4 }}'",
        crate::octane::gamedata()
    );
    let output = server.run_as_root(&command).await?.output;
    Ok(output.trim().parse().unwrap_or(i64::MAX))
}

const UPLOAD: &str = r#"set -e
install -d -m 2775 -o www-data -g www-data {{target}}
# Keep file dates for the next sync to compare. Owner and mode come from the server.
tar -x -f - -C {{target}} --no-same-owner --no-same-permissions
echo "OK $(find {{target}} -maxdepth 1 -type f | wc -l)""#;

/// Streams the plan's files as tar into the target folder. `progress` gets the bytes sent so far.
pub async fn upload(server: &dyn Server, plan: &AssetPlan, progress: Progress<'_>) -> Result<()> {
    let target = shell_quote(&format!("{}/{}", crate::octane::gamedata(), plan.mapping.target));
    let script = script::fill(UPLOAD, &[("target", &target)]);
    let items: Vec<tar_stream::TarItem> = plan.upload().map(|f| tar_stream::TarItem::file(&f.path, &f.name)).collect();
    let result = tar_stream::upload(server, &script, &items, progress).await?;
    if !result.succeeded() || !result.output.contains("OK ") {
        crate::fail!("Unpacking {} on the server failed: {}", plan.mapping.name, format!("{}{}", result.output, result.error).trim());
    }
    Ok(())
}

const FIX_PERMISSIONS: &str = r#"if [ -x {{PermissionsScript}} ]; then
  echo "> {{PermissionsScript}}"
  {{PermissionsScript}}
else
  echo "> chgrp -R www-data {{Gamedata}} (run the Octane step for the full permissions script)"
  chgrp -R www-data {{Gamedata}}
  chmod -R g+rwX,o+rX {{Gamedata}}
fi"#;

/// Runs the gamedata-permissions script from the Octane step, or a plain chgrp when it is missing.
pub async fn fix_permissions(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = script::fill(
        FIX_PERMISSIONS,
        &[("PermissionsScript", crate::octane::PERMISSIONS_SCRIPT), ("Gamedata", &crate::octane::gamedata())],
    );
    script::run(server, &script::with_prelude(&body), log, "Setting the permissions failed", None).await
}

/// The tar stream of the uploads, written like .NET's `TarWriter` in PAX format writes it: before every entry an
/// extended header ("./PaxHeaders.<pid>/.", mode 644) with `path` and `mtime` (seconds with 7 decimals), then a
/// ustar header with uid/gid 0, no user or group names, the name cut to 100 bytes and no prefix, and at the end two
/// empty blocks. Files keep their modification time; their mode is 755 on Windows (Windows files have none) and the
/// file's own mode elsewhere. Folder entries get mode 755 and the current time.
pub(crate) mod tar_stream {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

    use crate::server::{CommandResult, Progress, Result, Server};

    const BLOCK: usize = 512;
    /// The largest size the 11 octal digits of the size field hold.
    const MAX_SIZE: u64 = 0o77777777777;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum TarItem {
        File { path: PathBuf, name: String },
        Directory { name: String },
    }

    impl TarItem {
        pub fn file(path: &Path, name: &str) -> Self {
            TarItem::File { path: path.to_path_buf(), name: name.to_string() }
        }

        /// `name` ends with '/', as the C# code passes it.
        pub fn directory(name: &str) -> Self {
            TarItem::Directory { name: name.to_string() }
        }
    }

    fn octal(field: &mut [u8], value: u64) {
        let digits = field.len() - 1;
        let text = format!("{value:0digits$o}");
        let bytes = text.as_bytes();
        let bytes = &bytes[bytes.len().saturating_sub(digits)..];
        field[..digits].copy_from_slice(bytes);
        field[digits] = 0;
    }

    /// One ustar header block.
    fn header(name: &[u8], mode: u32, uid: u64, gid: u64, size: u64, mtime: Option<i64>, kind: u8) -> [u8; BLOCK] {
        let mut header = tar::Header::new_ustar();
        {
            let h = header.as_ustar_mut().expect("ustar header");
            let n = name.len().min(h.name.len());
            h.name[..n].copy_from_slice(&name[..n]);
            octal(&mut h.mode, u64::from(mode));
            octal(&mut h.uid, uid);
            octal(&mut h.gid, gid);
            octal(&mut h.size, size);
            if let Some(mtime) = mtime {
                octal(&mut h.mtime, mtime.max(0) as u64);
            }
            h.typeflag = [kind];
            h.cksum = [b' '; 8];
        }
        let mut bytes = [0u8; BLOCK];
        bytes.copy_from_slice(header.as_bytes());
        let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();
        // "%06o\0 ", as .NET writes the checksum.
        let text = format!("{sum:06o}");
        bytes[148..154].copy_from_slice(&text.as_bytes()[text.len() - 6..]);
        bytes[154] = 0;
        bytes[155] = b' ';
        bytes
    }

    /// "<length> key=value\n", the length counting itself.
    fn record(key: &str, value: &str) -> String {
        let rest = format!(" {key}={value}\n");
        let mut length = rest.len() + 1;
        while length.to_string().len() + rest.len() != length {
            length = length.to_string().len() + rest.len();
        }
        format!("{length}{rest}")
    }

    /// Seconds since 1970 with up to 7 decimals and no trailing zeros, as .NET writes the PAX mtime.
    pub(crate) fn pax_time(time: SystemTime) -> String {
        let (negative, duration) = match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => (false, d),
            Err(e) => (true, e.duration()),
        };
        let ticks = duration.subsec_nanos() / 100;
        let mut text = format!("{}{}", if negative { "-" } else { "" }, duration.as_secs());
        if ticks > 0 {
            let fraction = format!("{ticks:07}");
            text.push('.');
            text.push_str(fraction.trim_end_matches('0'));
        }
        text
    }

    fn whole_seconds(time: SystemTime) -> i64 {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
        }
    }

    async fn pad<W: AsyncWrite + Unpin>(out: &mut W, size: u64) -> io::Result<()> {
        let rest = (BLOCK - (size % BLOCK as u64) as usize) % BLOCK;
        if rest > 0 {
            out.write_all(&vec![0u8; rest]).await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_headers<W: AsyncWrite + Unpin>(
        out: &mut W,
        name: &str,
        mode: u32,
        uid: u64,
        gid: u64,
        size: u64,
        mtime: SystemTime,
        kind: u8,
    ) -> io::Result<()> {
        let data = record("path", name) + &record("mtime", &pax_time(mtime));
        let pax_name = format!("./PaxHeaders.{}/.", std::process::id());
        out.write_all(&header(pax_name.as_bytes(), 0o644, 0, 0, data.len() as u64, None, b'x')).await?;
        out.write_all(data.as_bytes()).await?;
        pad(out, data.len() as u64).await?;
        out.write_all(&header(name.as_bytes(), mode, uid, gid, size, Some(whole_seconds(mtime)), kind)).await
    }

    #[cfg(unix)]
    fn owner_and_mode(metadata: &std::fs::Metadata) -> (u32, u64, u64) {
        use std::os::unix::fs::MetadataExt;
        (metadata.mode() & 0o7777, u64::from(metadata.uid()), u64::from(metadata.gid()))
    }

    // Windows files don't have a mode: .NET uses 755.
    #[cfg(not(unix))]
    fn owner_and_mode(_metadata: &std::fs::Metadata) -> (u32, u64, u64) {
        (0o755, 0, 0)
    }

    /// Writes the archive. `progress` gets the bytes of the files written so far.
    pub async fn write<W: AsyncWrite + Unpin>(out: &mut W, items: &[TarItem], progress: &(dyn Fn(u64) + Send + Sync)) -> io::Result<()> {
        let mut sent = 0u64;
        for item in items {
            match item {
                TarItem::Directory { name } => {
                    write_headers(out, name, 0o755, 0, 0, 0, SystemTime::now(), b'5').await?;
                }
                TarItem::File { path, name } => {
                    let file = tokio::fs::File::open(path).await?;
                    let metadata = file.metadata().await?;
                    let size = metadata.len();
                    if size > MAX_SIZE {
                        return Err(io::Error::other(format!("{} is too large for the upload", path.display())));
                    }
                    let (mode, uid, gid) = owner_and_mode(&metadata);
                    let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    write_headers(out, name, mode, uid, gid, size, mtime, b'0').await?;
                    let copied = tokio::io::copy(&mut file.take(size), out).await?;
                    if copied != size {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{} changed while it was uploaded", path.display())));
                    }
                    pad(out, size).await?;
                    sent += size;
                    progress(sent);
                }
            }
        }
        out.write_all(&[0u8; BLOCK * 2]).await?;
        out.flush().await
    }

    /// Runs `script` as root with the archive of `items` as its stdin, streamed while it is written.
    pub async fn upload(server: &dyn Server, script: &str, items: &[TarItem], progress: Progress<'_>) -> Result<CommandResult> {
        let (reader, mut writer) = tokio::io::duplex(256 * 1024);
        let writing = async move {
            let result = write(&mut writer, items, progress).await;
            let _ = writer.shutdown().await;
            drop(writer);
            result
        };
        let (written, result) = tokio::join!(writing, server.run_as_root_with_input(script, Box::new(reader)));
        let result = result?;
        match written {
            // The server stopped reading: its own output says why.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(result),
            Err(e) => Err(e.into()),
            Ok(()) => Ok(result),
        }
    }

    /// Reads a whole archive into memory (tests).
    #[cfg(test)]
    pub async fn to_bytes(items: &[TarItem]) -> Vec<u8> {
        let mut bytes = Vec::new();
        write(&mut bytes, items, &|_| {}).await.unwrap();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::tar_stream::{self, TarItem};
    use super::*;
    use std::io::Read;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn set_mtime(path: &Path, time: SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
    }

    struct Entry {
        name: String,
        kind: tar::EntryType,
        size: u64,
        mtime: u64,
        mode: u32,
        pax_mtime: Option<String>,
        raw_name: Vec<u8>,
        data: Vec<u8>,
    }

    fn entries(bytes: &[u8]) -> Vec<Entry> {
        let mut archive = tar::Archive::new(bytes);
        let mut list = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let pax_mtime = entry.pax_extensions().unwrap().and_then(|p| {
                p.flatten().find(|e| e.key() == Ok("mtime")).map(|e| e.value().unwrap().to_string())
            });
            let header = entry.header().clone();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            list.push(Entry {
                name: entry.path().unwrap().to_string_lossy().replace('\\', "/").trim_end_matches('/').to_string(),
                kind: header.entry_type(),
                size: header.size().unwrap(),
                mtime: header.mtime().unwrap(),
                mode: header.mode().unwrap(),
                pax_mtime,
                raw_name: header.as_ustar().unwrap().name.iter().copied().take_while(|&b| b != 0).collect(),
                data,
            });
        }
        list
    }

    #[tokio::test]
    async fn tar_entries() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, "hello").unwrap();
        let time = UNIX_EPOCH + Duration::new(1_704_164_645, 123_456_700);
        set_mtime(&a, time);
        let b = dir.path().join("b.bin");
        std::fs::write(&b, vec![7u8; 600]).unwrap();
        set_mtime(&b, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let long = format!("{}/b.bin", "x".repeat(120));

        let items = vec![TarItem::file(&a, "sub/a.txt"), TarItem::directory("top/"), TarItem::file(&b, &long)];
        let bytes = tar_stream::to_bytes(&items).await;
        // Per entry an extended header block and its data block, the header, the data; two empty blocks at the end.
        assert_eq!(bytes.len(), (2 + 1 + 1) * 512 + (2 + 1) * 512 + (2 + 1 + 2) * 512 + 2 * 512);
        // .NET's extended header name and checksum layout.
        assert!(bytes[..100].starts_with(format!("./PaxHeaders.{}/.", std::process::id()).as_bytes()));
        assert_eq!(&bytes[100..108], b"0000644\0");
        assert_eq!(bytes[155], b' ');
        assert_eq!(bytes[154], 0);
        assert_eq!(&bytes[512..512 + 46], b"18 path=sub/a.txt\n28 mtime=1704164645.1234567\n");

        let list = entries(&bytes);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "sub/a.txt");
        assert_eq!(list[0].kind, tar::EntryType::Regular);
        assert_eq!(list[0].size, 5);
        assert_eq!(list[0].mtime, 1_704_164_645);
        assert_eq!(list[0].pax_mtime.as_deref(), Some("1704164645.1234567"));
        assert_eq!(list[0].data, b"hello");
        assert_eq!(list[1].name, "top");
        assert_eq!(list[1].kind, tar::EntryType::Directory);
        assert_eq!(list[1].mode, 0o755);
        assert_eq!(list[1].size, 0);
        assert_eq!(list[2].name, long);
        assert_eq!(list[2].raw_name.len(), 100);
        assert_eq!(list[2].size, 600);
        assert_eq!(list[2].mtime, 1_700_000_000);
        assert_eq!(list[2].pax_mtime.as_deref(), Some("1700000000"));
        if cfg!(windows) {
            assert_eq!(list[0].mode, 0o755);
            assert_eq!(list[2].mode, 0o755);
        }
    }

    #[test]
    fn get_assets_folder_maps_one_to_one() {
        let root = tempfile::tempdir().unwrap();
        let g = root.path().join("gamedata");
        for (folder, file) in [("furniture", "chair.hab"), ("config", "FurnitureData.json"), ("c_images/album1584", "ADM.gif"), ("c_images/reception/x", "bg.png")] {
            std::fs::create_dir_all(g.join(folder)).unwrap();
            std::fs::write(g.join(folder).join(file), b"1").unwrap();
        }
        std::fs::create_dir_all(g.join("empty")).unwrap();
        std::fs::create_dir_all(g.join(".hidden")).unwrap();
        std::fs::write(g.join(".hidden/a"), b"1").unwrap();
        let mappings = gamedata_mappings(root.path());
        let targets: Vec<(&str, &str, bool)> = mappings.iter().map(|m| (m.target.as_str(), m.source.as_str(), m.add_only)).collect();
        assert_eq!(
            targets,
            [
                ("c_images/album1584", "gamedata/c_images/album1584", false),
                ("c_images/reception/x", "gamedata/c_images/reception/x", false),
                ("config", "gamedata/config", true),
                ("furniture", "gamedata/furniture", false),
            ]
        );
        let sources = sources(root.path(), None);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].label, g);
        assert!(super::sources(&root.path().join("nothing"), None).is_empty());
    }

    #[test]
    fn pax_times() {
        assert_eq!(tar_stream::pax_time(UNIX_EPOCH + Duration::new(5, 100)), "5.0000001");
        assert_eq!(tar_stream::pax_time(UNIX_EPOCH + Duration::new(5, 500_000_000)), "5.5");
        assert_eq!(tar_stream::pax_time(UNIX_EPOCH + Duration::new(5, 99)), "5");
    }

    #[test]
    fn patterns() {
        assert!(matches_pattern("*.hab", "chair.hab"));
        assert!(!matches_pattern("*.hab", "chair.habx"));
        assert!(matches_pattern("badgepart_*.png", "badgepart_base_1.png"));
        assert!(!matches_pattern("badgepart_*.png", "x_badgepart_1.png"));
        assert!(matches_pattern("*", "README"));
        assert!(matches_pattern("a?c", "abc"));
    }

    #[test]
    fn listing() {
        let remote = parse_listing("== 0\na.hab\t10\t1704164645.5\nbad line\n== 3\nb\tx\ty\n== z\nc\t1\t2\n");
        assert_eq!(remote[&0]["a.hab"], (10, 1_704_164_645.5));
        assert_eq!(remote[&3]["b"], (-1, 0.0));
        assert_eq!(remote.len(), 2);
    }

    #[test]
    fn plans_count() {
        let plan = AssetPlan {
            mapping: mappings()[0].clone(),
            source_path: PathBuf::new(),
            local_count: 3,
            local_bytes: 30,
            new: vec![LocalFile { name: "a".into(), path: PathBuf::new(), size: 10, modified: 0 }],
            changed: vec![LocalFile { name: "b".into(), path: PathBuf::new(), size: 5, modified: 0 }],
        };
        assert_eq!(plan.upload_count(), 2);
        assert_eq!(plan.upload_bytes(), 15);
        assert_eq!(mappings().len(), 13);
        assert!(mappings()[12].add_only);
    }
}
