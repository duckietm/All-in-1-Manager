//! Picked folders (API 1.1): a folder the user chose in a dialog, where a plugin may create new files.
//!
//! The plugin never sees the folder's path. It names files with the private-file path rules
//! ([`crate::runtime::check_relative`]) and may:
//!
//! * create a data file ([`DATA_EXTENSIONS`]; no hidden names) that does not exist yet (and the subfolders it needs),
//!   with create-new semantics, so a file that appears in the meantime is never overwritten
//!   (`permission denied: <path> already exists`);
//! * rewrite a file it created through the same handle (and only that), through a temporary file renamed over it, so
//!   a failed write never leaves a cut-off file;
//! * append to a file it created through the same handle (or create it, like a first write): the file is opened
//!   without following a link and must still be a plain file. A failed append may leave part of the data at the
//!   end; the handle then counts the file's real size;
//! * hand such a file (at most [`crate::runtime::MAX_FILE_READ`]) to the Manager's import preview.
//!
//! Every existing component below the picked root is looked at without following it ([`std::fs::symlink_metadata`]);
//! a symlink, a junction or any other reparse point is refused, so a write cannot be led out of the folder. The root
//! is canonical when picked and must still canonicalise to itself at every use (it was not replaced by a link). A
//! root inside a folder of the Manager (its configuration, the plugin's own folder) is refused when picked, and a
//! write that would land in one (a root above it) is refused too: [`MANAGER_FOLDER`]. Too broad a root (a drive's
//! root, the user's home folder itself, a startup folder or anything inside one, see [`UserFolders`]) is refused when
//! picked: [`OWN_FOLDER`]. Every refusal starts with `permission denied:`, like every other refusal of the API.
//!
//! Limits per handle ([`FolderLimits`]): [`MAX_FOLDER_FILES`] files, [`MAX_FOLDER_BYTES`] in total (the files' current
//! sizes), each file at most [`MAX_FOLDER_FILE`]; past them [`LIMIT_REACHED`].

use std::collections::HashMap;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::runtime::{check_relative, io_text, read_limited};

/// Files one folder handle may create.
pub const MAX_FOLDER_FILES: usize = 100_000;
/// Total size of the files one folder handle created (their current sizes).
pub const MAX_FOLDER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Largest file written into a picked folder.
pub const MAX_FOLDER_FILE: u64 = 256 * 1024 * 1024;
/// The refusal past a limit of [`FolderLimits`].
pub const LIMIT_REACHED: &str = "permission denied: folder limit reached";
/// The refusal of a folder that is (or leads into) one of the Manager's own folders.
pub const MANAGER_FOLDER: &str = "permission denied: this folder belongs to the Manager";
/// The refusal of too broad a root: a drive's root, the home folder itself, a startup folder.
pub const OWN_FOLDER: &str = "permission denied: pick a folder of its own for this";
/// The file types a plugin may write into a picked folder (the last extension, any letter case): data, never
/// programs, libraries, scripts or shortcuts that a broad pick could plant.
pub const DATA_EXTENSIONS: &[&str] = &[
    "json", "sql", "txt", "md", "csv", "tsv", "xml", "html", "png", "gif", "jpg", "jpeg", "webp", "bmp", "svg", "zip",
    "nitro", "swf", "bin", "log",
];

/// The user's folders a picked root may not be: the home folder itself, and the startup folders (programs started at
/// login) or anything inside them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserFolders {
    pub home: Option<PathBuf>,
    pub autostart: Vec<PathBuf>,
}

impl UserFolders {
    /// The folders of the user running the Manager: `home_dir`, and the Windows Startup folders (the user's and all
    /// users'), `$XDG_CONFIG_HOME/autostart` and `~/.config/autostart`, and `~/Library/LaunchAgents` on macOS.
    pub fn of_this_user() -> Self {
        let home = std::env::home_dir();
        let mut autostart = Vec::new();
        #[cfg(windows)]
        {
            const STARTUP: &str = r"Microsoft\Windows\Start Menu\Programs\Startup";
            for var in ["APPDATA", "ProgramData"] {
                if let Some(base) = std::env::var_os(var) {
                    autostart.push(PathBuf::from(base).join(STARTUP));
                }
            }
        }
        #[cfg(unix)]
        {
            if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|c| !c.is_empty()) {
                autostart.push(PathBuf::from(config).join("autostart"));
            }
            autostart.extend(home.as_ref().map(|h| h.join(".config/autostart")));
            #[cfg(target_os = "macos")]
            autostart.extend(home.as_ref().map(|h| h.join("Library/LaunchAgents")));
        }
        Self { home, autostart }
    }
}

/// A path made only of data files: no hidden segment (`.config`, `.bashrc`), and a last extension in
/// [`DATA_EXTENSIONS`].
fn check_data_file(path: &str, segments: &[String]) -> Result<(), String> {
    if segments.iter().any(|s| s.starts_with('.')) {
        return Err(format!("permission denied: {path} is hidden (a name starts with .)"));
    }
    let name = segments.last().map(String::as_str).unwrap_or("");
    let data = name.rsplit_once('.').is_some_and(|(_, ext)| DATA_EXTENSIONS.iter().any(|d| d.eq_ignore_ascii_case(ext)));
    if !data {
        return Err(format!("permission denied: {path} is not a data file"));
    }
    Ok(())
}
/// Shortest time between two `reveal`s of one handle (each opens a file-manager window).
pub const REVEAL_SPACING: Duration = Duration::from_secs(2);

/// What one folder handle may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderLimits {
    pub files: usize,
    pub bytes: u64,
    pub file: u64,
}

impl Default for FolderLimits {
    fn default() -> Self {
        Self { files: MAX_FOLDER_FILES, bytes: MAX_FOLDER_BYTES, file: MAX_FOLDER_FILE }
    }
}

/// A folder the user picked. The guest gets a handle; the path stays here.
pub struct PickedFolder {
    /// Canonical.
    root: PathBuf,
    /// The Manager's folders, canonical (those that exist).
    manager: Vec<PathBuf>,
    limits: FolderLimits,
    /// The files this handle created (the plugin's relative path, as written) and their current sizes.
    written: HashMap<String, u64>,
    total: u64,
    last_reveal: Option<Instant>,
}

/// A write checked against the handle's state, run off the async workers ([`WriteJob::run`]).
pub(crate) struct WriteJob {
    root: PathBuf,
    manager: Vec<PathBuf>,
    rel: String,
    segments: Vec<String>,
    /// The path was created through this handle: it may be rewritten.
    own: bool,
}

impl PickedFolder {
    /// The folder the user picked at `picked`, unless it lies inside one of `manager` (the Manager's folders) or is
    /// too broad a root: a drive's root, `user`'s home folder itself, one of its startup folders or inside one.
    pub(crate) fn open(picked: &Path, manager: &[PathBuf], user: &UserFolders, limits: FolderLimits) -> Result<Self, String> {
        let root = fs::canonicalize(picked).map_err(|e| io_text(&e))?;
        if !fs::metadata(&root).map_err(|e| io_text(&e))?.is_dir() {
            return Err("not a folder".into());
        }
        let manager: Vec<PathBuf> = manager.iter().filter_map(|m| fs::canonicalize(m).ok()).collect();
        if manager.iter().any(|m| root.starts_with(m)) {
            return Err(MANAGER_FOLDER.into());
        }
        let real = |p: &PathBuf| fs::canonicalize(p).ok();
        let is_home = user.home.as_ref().and_then(real).is_some_and(|h| h == root);
        let in_autostart = user.autostart.iter().filter_map(real).any(|a| root.starts_with(a));
        if root.parent().is_none() || is_home || in_autostart {
            return Err(OWN_FOLDER.into());
        }
        Ok(Self { root, manager, limits, written: HashMap::new(), total: 0, last_reveal: None })
    }

    /// The folder's name (its last path component; empty for a drive's root).
    pub fn name(&self) -> String {
        self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Checks a write of `len` bytes to `path` against the path rules and the limits; nothing is touched yet.
    pub(crate) fn check_write(&self, path: &str, len: u64) -> Result<WriteJob, String> {
        let segments: Vec<String> = check_relative(path)?.into_iter().map(str::to_string).collect();
        check_data_file(path, &segments)?;
        let old = self.written.get(path).copied();
        let new_file = old.is_none();
        if len > self.limits.file
            || (new_file && self.written.len() >= self.limits.files)
            || self.total.saturating_sub(old.unwrap_or(0)).saturating_add(len) > self.limits.bytes
        {
            return Err(LIMIT_REACHED.into());
        }
        Ok(WriteJob { root: self.root.clone(), manager: self.manager.clone(), rel: path.to_string(), segments, own: !new_file })
    }

    /// Records a write that [`WriteJob::run`] completed.
    pub(crate) fn record(&mut self, path: &str, len: u64) {
        let old = self.written.insert(path.to_string(), len).unwrap_or(0);
        self.total = self.total.saturating_sub(old).saturating_add(len);
    }

    /// Checks an append of `len` bytes to `path` against the path rules and the limits (the file's size after the
    /// append, the handle's total, a new file's count); nothing is touched yet.
    pub(crate) fn check_append(&self, path: &str, len: u64) -> Result<WriteJob, String> {
        let segments: Vec<String> = check_relative(path)?.into_iter().map(str::to_string).collect();
        check_data_file(path, &segments)?;
        let old = self.written.get(path).copied();
        if old.unwrap_or(0).saturating_add(len) > self.limits.file
            || (old.is_none() && self.written.len() >= self.limits.files)
            || self.total.saturating_add(len) > self.limits.bytes
        {
            return Err(LIMIT_REACHED.into());
        }
        Ok(WriteJob { root: self.root.clone(), manager: self.manager.clone(), rel: path.to_string(), segments, own: old.is_some() })
    }

    /// Records an append of `len` bytes: what [`WriteJob::append`] did, or how it failed.
    pub(crate) fn record_append(&mut self, path: &str, len: u64, outcome: &Result<Appended, AppendFailed>) {
        match outcome {
            Ok(Appended::Created) => self.record(path, len),
            Ok(Appended::Extended) => {
                let old = self.written.get(path).copied().unwrap_or(0);
                self.record(path, old.saturating_add(len));
            }
            Err(AppendFailed::Refused(_)) => {}
            // Part of the data may be in the handle's file: count its real size, or (unknown) as if all of it were.
            // Never makes a file the handle's own (a stopped append of a file it did not have yet).
            Err(AppendFailed::Partial { size, .. }) => {
                if let Some(&old) = self.written.get(path) {
                    self.record(path, size.unwrap_or(old.saturating_add(len)));
                }
            }
        }
    }

    /// Checks a read of `path` for the import preview: only a file this handle wrote; nothing is touched yet.
    pub(crate) fn read_job(&self, path: &str) -> Result<ReadJob, String> {
        let segments: Vec<String> = check_relative(path)?.into_iter().map(str::to_string).collect();
        if !self.written.contains_key(path) {
            return Err(format!("permission denied: {path} was not written through this folder"));
        }
        Ok(ReadJob { root: self.root.clone(), rel: path.to_string(), segments })
    }

    /// The folder to show in the file manager, at most once per [`REVEAL_SPACING`].
    pub(crate) fn reveal_path(&mut self) -> Result<PathBuf, String> {
        if self.last_reveal.is_some_and(|t| t.elapsed() < REVEAL_SPACING) {
            return Err(format!("permission denied: the folder was shown less than {} s ago", REVEAL_SPACING.as_secs()));
        }
        check_root(&self.root)?;
        // Still a folder: a file put in its place would be opened, not shown.
        if !fs::symlink_metadata(&self.root).map_err(|e| io_text(&e))?.is_dir() {
            return Err("permission denied: the folder is no longer a folder".into());
        }
        self.last_reveal = Some(Instant::now());
        Ok(self.root.clone())
    }
}

/// A checked read of a file a handle wrote ([`PickedFolder::read_job`]), run off the async workers.
#[derive(Debug)]
pub(crate) struct ReadJob {
    root: PathBuf,
    rel: String,
    segments: Vec<String>,
}

impl ReadJob {
    /// The file's name and bytes (at most the read limit). Blocking file I/O.
    pub(crate) fn run(self) -> Result<(String, Vec<u8>), String> {
        match walk(&self.root, &self.segments, &self.rel)? {
            Some(meta) if meta.is_file() => {}
            Some(_) => return Err("not a file".into()),
            None => return Err("file not found".into()),
        }
        let bytes = read_limited(&join(&self.root, &self.segments))?;
        Ok((self.segments.last().cloned().unwrap_or_default(), bytes))
    }
}

impl WriteJob {
    /// Creates the file (or rewrites this handle's own file) with `data`. Blocking file I/O.
    pub(crate) fn run(self, data: &[u8]) -> Result<(), String> {
        let exists = || format!("permission denied: {} already exists", self.rel);
        let target = join(&self.root, &self.segments);
        let rewrite = match walk(&self.root, &self.segments, &self.rel)? {
            Some(meta) if self.own && meta.is_file() => true,
            Some(_) => return Err(exists()),
            None => false,
        };
        // Never a folder of the Manager, also when the root lies above one.
        self.check_manager(&target)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_text(&e))?;
        }
        // Again now that the folders exist: none of them may be a link made in the meantime.
        let again = walk(&self.root, &self.segments, &self.rel)?;
        self.check_manager(&target)?;
        if rewrite {
            if !again.is_some_and(|m| m.is_file()) {
                return Err(exists());
            }
            return replace(&target, data);
        }
        if again.is_some() {
            return Err(exists());
        }
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(exists()),
            Err(e) => return Err(io_text(&e)),
        };
        if let Err(e) = file.write_all(data) {
            drop(file);
            let _ = fs::remove_file(&target);
            return Err(io_text(&e));
        }
        Ok(())
    }

    /// Appends `data` to this handle's own file, or creates the file like a first [`run`](WriteJob::run) when it is
    /// not there (also when the plugin's file was removed in the meantime). Blocking file I/O.
    ///
    /// The existing file is opened without following a link (Windows: `FILE_FLAG_OPEN_REPARSE_POINT`; Unix:
    /// `O_NOFOLLOW | O_NONBLOCK`, so a FIFO or device put in its place fails or is refused instead of blocking). The
    /// opened handle must show a plain file with a single name (no hard link to a file elsewhere), and the same file
    /// as looked at just before. A file removed before the open is created like a first write. A failed write may
    /// leave part of `data` at the end: [`AppendFailed::Partial`] with the file's size read again.
    pub(crate) fn append(self, data: &[u8]) -> Result<Appended, AppendFailed> {
        let exists = || AppendFailed::Refused(format!("permission denied: {} already exists", self.rel));
        let link = || AppendFailed::Refused(format!("permission denied: {} leads through a link", self.rel));
        let refused = AppendFailed::Refused;
        let target = join(&self.root, &self.segments);
        match walk(&self.root, &self.segments, &self.rel).map_err(refused)? {
            Some(meta) if self.own && meta.is_file() => {}
            Some(_) => return Err(exists()),
            // Created like a first write: create-new, so a file that appears in the meantime is not touched.
            None => return self.create(data),
        }
        self.check_manager(&target).map_err(refused)?;
        // Right before opening: still a plain file of this handle, no link on the way.
        let seen = match walk(&self.root, &self.segments, &self.rel).map_err(refused)? {
            Some(meta) if meta.is_file() => meta,
            Some(_) => return Err(exists()),
            None => return self.create(data),
        };
        let before = match identity_before(&target, &seen) {
            Ok(id) => id,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return self.create(data),
            Err(e) => return Err(refused(io_text(&e))),
        };
        #[cfg(test)]
        tests::before_open(&target);
        let mut file = match open_for_append(&target) {
            Ok(file) => file,
            // Removed since it was looked at: a new file, by the first-write rules.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return self.create(data),
            Err(e) if is_link_error(&e) => return Err(link()),
            Err(e) => return Err(refused(io_text(&e))),
        };
        let opened = handle_info(&file).map_err(|e| refused(io_text(&e)))?;
        if !opened.plain {
            return Err(exists());
        }
        if opened.id != before {
            return Err(link());
        }
        if opened.links != 1 {
            return Err(AppendFailed::Refused(format!("permission denied: {} has other names (hard links)", self.rel)));
        }
        if let Err(e) = file.write_all(data) {
            // Read again: the totals count what is really in the file.
            let size = file.metadata().ok().map(|m| m.len());
            return Err(AppendFailed::Partial { error: io_text(&e), size });
        }
        Ok(Appended::Extended)
    }

    /// The append of a file that is not there: a first write (create-new; a failure leaves no file).
    fn create(self, data: &[u8]) -> Result<Appended, AppendFailed> {
        WriteJob { own: false, ..self }.run(data).map(|()| Appended::Created).map_err(AppendFailed::Refused)
    }

    /// Refuses `target` when its deepest existing folder (canonical, so in its real letter case) is inside one of the
    /// Manager's folders.
    fn check_manager(&self, target: &Path) -> Result<(), String> {
        let existing = target.ancestors().find(|p| fs::symlink_metadata(p).is_ok()).unwrap_or(&self.root);
        let real = fs::canonicalize(existing).map_err(|e| io_text(&e))?;
        if self.manager.iter().any(|m| real.starts_with(m)) {
            return Err(MANAGER_FOLDER.into());
        }
        Ok(())
    }
}

/// Writes `data` to a new temporary file next to `target` (a hidden name a plugin cannot write itself), then renames
/// it over `target`: a failed write leaves `target` as it was, never cut off. The rename replaces the directory
/// entry and never follows a link put there in the meantime. The temporary file is removed on any failure.
fn replace(target: &Path, data: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().ok_or_else(|| "invalid path".to_string())?;
    for _ in 0..100 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".aio-tmp-{}-{n}", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_text(&e)),
        };
        let written = file.write_all(data).and_then(|()| file.sync_all());
        drop(file);
        let result = written.and_then(|()| fs::rename(&temp, target));
        if let Err(e) = result {
            let _ = fs::remove_file(&temp);
            return Err(io_text(&e));
        }
        return Ok(());
    }
    Err("no free temporary name".into())
}

/// What a successful [`WriteJob::append`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Appended {
    /// The file was not there: created with the data.
    Created,
    /// The handle's file was extended by the data.
    Extended,
}

/// How a [`WriteJob::append`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppendFailed {
    /// Refused or failed before any byte was written (or a new file, removed again).
    Refused(String),
    /// The write failed part way: the end of the file may hold part of the data. `size` is the file's size read
    /// again (`None`: it could not be read).
    Partial { error: String, size: Option<u64> },
}

impl AppendFailed {
    pub(crate) fn into_message(self) -> String {
        match self {
            AppendFailed::Refused(e) | AppendFailed::Partial { error: e, .. } => e,
        }
    }
}

#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Opens the existing file `target` for appending without following a link: on Windows a reparse point is opened
/// itself (`FILE_FLAG_OPEN_REPARSE_POINT`), so the caller sees it in the handle's attributes; on Unix `O_NOFOLLOW`
/// fails on a link and `O_NONBLOCK` keeps a FIFO from blocking the open. The sharing is the default one, the same as
/// `write`'s (others may read, write and delete). Never creates the file.
fn open_for_append(target: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(target)
}

/// The open failed because the last component is a link (`O_NOFOLLOW` on Unix).
fn is_link_error(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// What an open handle shows about its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HandleInfo {
    /// Unix: device and inode. Windows: volume serial number and file index.
    id: (u64, u64),
    /// The file's names (hard links).
    links: u64,
    /// A regular file, not a link or reparse point, not a device or FIFO.
    plain: bool,
}

#[cfg(unix)]
fn handle_info(file: &fs::File) -> io::Result<HandleInfo> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(HandleInfo { id: (meta.dev(), meta.ino()), links: meta.nlink(), plain: meta.file_type().is_file() })
}

#[cfg(windows)]
fn handle_info(file: &fs::File) -> io::Result<HandleInfo> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    let meta = file.metadata()?;
    // SAFETY: a plain C struct, all zeroes is valid; the handle is open for the call; the struct is written by it.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(HandleInfo {
        id: (u64::from(info.dwVolumeSerialNumber), index),
        links: u64::from(info.nNumberOfLinks),
        plain: meta.is_file() && !is_link(&meta),
    })
}

#[cfg(not(any(unix, windows)))]
fn handle_info(file: &fs::File) -> io::Result<HandleInfo> {
    let meta = file.metadata()?;
    Ok(HandleInfo { id: (0, 0), links: 1, plain: meta.is_file() })
}

/// The identity of the file looked at (`seen`, without following links) just before the open for appending. Unix:
/// from `seen` itself. Windows: from a handle opened for no access at all, without following a reparse point.
fn identity_before(target: &Path, seen: &Metadata) -> io::Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = target;
        Ok((seen.dev(), seen.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = seen;
        let file = OpenOptions::new().access_mode(0).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(target)?;
        Ok(handle_info(&file)?.id)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, seen);
        Ok((0, 0))
    }
}

fn join(root: &Path, segments: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    path.extend(segments);
    path
}

/// The root still canonicalises to itself (it was not replaced by a link since it was picked).
fn check_root(root: &Path) -> Result<(), String> {
    match fs::canonicalize(root) {
        Ok(real) if real == root => Ok(()),
        Ok(_) => Err("permission denied: the folder was replaced by a link".into()),
        Err(e) => Err(io_text(&e)),
    }
}

/// A symlink, a junction or any other reparse point.
fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Looks at every existing component of `root/segments` without following it: a link is refused, a component before
/// the last must be a folder. The metadata of the last one, `None` when the path does not exist (yet).
fn walk(root: &Path, segments: &[String], rel: &str) -> Result<Option<Metadata>, String> {
    check_root(root)?;
    let mut path = root.to_path_buf();
    for (i, segment) in segments.iter().enumerate() {
        path.push(segment);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_text(&e)),
        };
        if is_link(&meta) {
            return Err(format!("permission denied: {rel} leads through a link"));
        }
        if i + 1 == segments.len() {
            return Ok(Some(meta));
        }
        if !meta.is_dir() {
            return Err(format!("permission denied: {rel} leads through a file"));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    type Hook = Box<dyn Fn(&Path)>;

    thread_local! {
        /// Runs in [`WriteJob::append`] right before the open, on the test's thread: a change made in the window.
        static BEFORE_OPEN: RefCell<Option<Hook>> = RefCell::new(None);
    }

    pub(super) fn before_open(target: &Path) {
        BEFORE_OPEN.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(target);
            }
        });
    }

    /// `folder` (an own file `own.txt` of 3 bytes) appended with `hook` run right before the open.
    fn append_with_hook(dir: &Path, hook: Hook) -> (PickedFolder, Result<Appended, AppendFailed>) {
        let mut folder = PickedFolder::open(dir, &[], &UserFolders::default(), FolderLimits::default()).unwrap();
        folder.check_write("own.txt", 3).unwrap().run(b"own").unwrap();
        folder.record("own.txt", 3);
        BEFORE_OPEN.with(|h| *h.borrow_mut() = Some(hook));
        let outcome = folder.check_append("own.txt", 4).unwrap().append(b"more");
        BEFORE_OPEN.with(|h| *h.borrow_mut() = None);
        folder.record_append("own.txt", 4, &outcome);
        (folder, outcome)
    }

    #[test]
    fn append_creates_a_file_removed_right_before_the_open() {
        let dir = tempfile::tempdir().unwrap();
        let (folder, outcome) = append_with_hook(dir.path(), Box::new(|target| fs::remove_file(target).unwrap()));
        assert_eq!(outcome, Ok(Appended::Created));
        assert_eq!(fs::read(dir.path().join("own.txt")).unwrap(), b"more");
        assert_eq!((folder.written["own.txt"], folder.total), (4, 4));
    }

    #[test]
    fn append_refuses_a_hard_link_swapped_in_right_before_the_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("out");
        fs::create_dir(&root).unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"victim").unwrap();
        let swap = victim.clone();
        let (folder, outcome) = append_with_hook(
            &root,
            Box::new(move |target| {
                fs::remove_file(target).unwrap();
                fs::hard_link(&swap, target).unwrap();
            }),
        );
        let error = outcome.unwrap_err().into_message();
        assert!(error.starts_with("permission denied:"), "{error}");
        assert_eq!(fs::read(&victim).unwrap(), b"victim");
        assert_eq!((folder.written["own.txt"], folder.total), (3, 3), "a refusal changes nothing");
    }

    #[test]
    fn a_stopped_append_of_a_new_file_does_not_make_it_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut folder = PickedFolder::open(dir.path(), &[], &UserFolders::default(), FolderLimits::default()).unwrap();
        // What the host records when the blocking task stopped (a join error).
        folder.record_append("new.txt", 10, &Err(AppendFailed::Partial { error: "stopped".into(), size: None }));
        assert!(folder.written.is_empty());
        assert_eq!(folder.total, 0);
        assert!(folder.read_job("new.txt").is_err());
    }

    #[test]
    fn reveal_refuses_a_root_that_is_no_longer_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let picked = dir.path().join("out");
        fs::create_dir(&picked).unwrap();
        let mut folder = PickedFolder::open(&picked, &[], &UserFolders::default(), FolderLimits::default()).unwrap();
        // Replaced by a file of the same name: the path still canonicalises to itself, but it is not the folder.
        fs::remove_dir(&picked).unwrap();
        fs::write(&picked, b"not a folder").unwrap();
        let error = folder.reveal_path().unwrap_err();
        assert!(error.starts_with("permission denied:"), "{error}");
        // A folder again: shown.
        fs::remove_file(&picked).unwrap();
        fs::create_dir(&picked).unwrap();
        assert_eq!(folder.reveal_path().unwrap(), fs::canonicalize(&picked).unwrap());
    }

    #[test]
    fn a_failed_append_counts_the_real_size() {
        let dir = tempfile::tempdir().unwrap();
        let limits = FolderLimits { files: 10, bytes: 100, file: 100 };
        let mut folder = PickedFolder::open(dir.path(), &[], &UserFolders::default(), limits).unwrap();
        folder.record_append("a.bin", 30, &Ok(Appended::Created));
        folder.record_append("a.bin", 20, &Ok(Appended::Extended));
        assert_eq!((folder.written["a.bin"], folder.total), (50, 50));
        // Refused before writing: nothing changes.
        folder.record_append("a.bin", 40, &Err(AppendFailed::Refused("x".into())));
        assert_eq!((folder.written["a.bin"], folder.total), (50, 50));
        // 7 of 40 bytes made it: the size read again counts.
        folder.record_append("a.bin", 40, &Err(AppendFailed::Partial { error: "disk full".into(), size: Some(57) }));
        assert_eq!((folder.written["a.bin"], folder.total), (57, 57));
        // The size could not be read: counted as if all of it were written.
        folder.record_append("a.bin", 10, &Err(AppendFailed::Partial { error: "x".into(), size: None }));
        assert_eq!((folder.written["a.bin"], folder.total), (67, 67));
        assert_eq!(folder.check_append("a.bin", 34).err().as_deref(), Some(LIMIT_REACHED));
        assert!(folder.check_append("a.bin", 33).is_ok());
    }

    #[test]
    fn read_job_reads_only_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut folder = PickedFolder::open(dir.path(), &[], &UserFolders::default(), FolderLimits::default()).unwrap();
        assert!(folder.read_job("a.json").unwrap_err().starts_with("permission denied:"));
        folder.check_write("a.json", 2).unwrap().run(b"{}").unwrap();
        folder.record("a.json", 2);
        assert_eq!(folder.read_job("a.json").unwrap().run().unwrap(), ("a.json".to_string(), b"{}".to_vec()));
    }
}
