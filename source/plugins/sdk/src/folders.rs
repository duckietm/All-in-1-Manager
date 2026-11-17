//! Folders the user picks: a place outside the private folder where the plugin may save new data files (an export,
//! a downloaded catalogue) and read the data files already there (an earlier export to resume or compare with).
//! Needs `"api": "1.1"` in the manifest, and no permission, for writing as for reading: the user picks the folder each
//! time, and the plugin never sees its path, only its [name](Folder::name).
//!
//! The Manager's rules:
//! - paths follow the [private-file rules](crate::files): relative with `/`, at most 255 bytes and 8 levels, only
//!   `A-Z a-z 0-9 . _ -` in each name, no `..`, no drive and no Windows device name (an invalid path is
//!   [`ErrorKind::Other`](crate::ErrorKind::Other));
//! - only data files: the last extension (any letter case) is one of [`DATA_EXTENSIONS`], and no name in the path
//!   starts with `.`;
//! - a write creates a new file (and the folders it needs). A file that already exists and was not written through
//!   this [`Folder`] is never touched; the plugin may rewrite only its own files, and a rewrite replaces the file
//!   whole (a failed one leaves the old content);
//! - [`append`](Folder::append) extends a file this [`Folder`] created (or creates it like a first write), so a large
//!   file never has to be held whole in the plugin's memory. A failed append may leave part of the data at the end of
//!   the file;
//! - [`read`](Folder::read) reads any data file in the folder, not only the handle's own, at most [`MAX_READ`]
//!   bytes per call ([`read_all`](Folder::read_all) loops over it); a file with other names (a hard link) is refused;
//! - [`list`](Folder::list) shows a page of at most [`MAX_ENTRIES`] entries of a folder, sorted by name, from a given
//!   index ([`list_all`](Folder::list_all) pages through all of them); links, hidden names and names a path cannot
//!   name are left out;
//! - a path that leads through a link is refused, and nothing in a folder of the Manager is read or written;
//! - one [`Folder`] writes at most [`MAX_FILES`] files and [`MAX_BYTES`] in all, each file at most [`MAX_FILE`].
//!
//! Every refusal is [`ErrorKind::Denied`](crate::ErrorKind::Denied), with a message starting `permission denied:`.
//!
//! Some folders cannot be picked: a drive's root, the home folder itself, a startup folder (or inside one), and the
//! Manager's own install and configuration folders (or inside them). [`pick`] returns `None` for them, as for a
//! cancelled dialog, and the plugin log says why.
//!
//! ```no_run
//! use aio_plugin_sdk::folders;
//!
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! if let Some(folder) = folders::pick("Save the catalogue") {
//!     folder.write_str("catalogue/furni.json", "{}")?;
//!     folder.write("catalogue/icons/chair.png", &[0x89, b'P', b'N', b'G'])?;
//!     for line in ["INSERT 1;\n", "INSERT 2;\n"] {
//!         folder.append_str("catalogue/items.sql", line)?;
//!     }
//!     folder.reveal()?;
//! }
//! if let Some(folder) = folders::pick("Resume from an earlier rip") {
//!     for entry in folder.list_all("")? {
//!         if !entry.is_folder && entry.name.ends_with(".json") {
//!             let head = folder.read(&entry.name, 0, 1024)?;
//!             # let _ = head;
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use crate::bindings::aio::plugin::host;
use crate::error::{host_result, Result};

/// The file types a plugin may write into a picked folder (the last extension, in any letter case).
pub const DATA_EXTENSIONS: &[&str] = &[
    "json", "sql", "txt", "md", "csv", "tsv", "xml", "html", "png", "gif", "jpg", "jpeg", "webp", "bmp", "svg", "zip",
    "nitro", "swf", "bin", "log",
];

/// Most files one [`Folder`] writes.
pub const MAX_FILES: usize = 100_000;

/// Most bytes one [`Folder`] writes in all (the current sizes of the files it wrote).
pub const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Largest file written into a picked folder.
pub const MAX_FILE: u64 = 256 * 1024 * 1024;

/// Most bytes one [`Folder::read`] returns (4 MiB); a larger `max` is [`Denied`](crate::ErrorKind::Denied).
pub const MAX_READ: u32 = 4 * 1024 * 1024;

/// Most entries one [`Folder::list`] returns (one page).
pub const MAX_ENTRIES: usize = 10_000;

/// Most entries a folder may have to be listed at all; a folder with more is [`Denied`](crate::ErrorKind::Denied).
pub const MAX_LISTED: usize = 1_000_000;

/// An entry of a picked folder, from [`Folder::list`].
///
/// New fields may be added in a later 1.x version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Entry {
    /// The name, without folders.
    pub name: String,
    /// A folder (else a file).
    pub is_folder: bool,
    /// A file's size in bytes; 0 for a folder.
    pub size: u64,
}

/// Shows a folder dialog titled `title`. `None` if the user cancelled, picked a folder that cannot be picked (see the
/// [module](self)), or past the dialog limit (20 dialogs in one call, see [`host`](crate::host)). Waiting for the user
/// does not count against the call's time limit.
///
/// Needs `"api": "1.1"` in the manifest.
pub fn pick(title: &str) -> Option<Folder> {
    host::pick_folder(title).map(|folder| Folder { folder })
}

/// A folder the user picked with [`pick`]. The handle is released when the value is dropped; a new [`pick`] of the
/// same folder starts over (it cannot rewrite the files of an earlier handle).
#[derive(Debug)]
pub struct Folder {
    folder: host::PickedFolder,
}

impl Folder {
    /// The folder's name (its last path component), to show the user where the files went.
    pub fn name(&self) -> String {
        self.folder.name()
    }

    /// Creates the file `path` with `data` (and the folders it needs), or rewrites a file this handle created. See the
    /// [module](self) for the rules and limits.
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        host_result(self.folder.write(path, data))
    }

    /// Like [`write`](Folder::write), with `text` as UTF-8.
    pub fn write_str(&self, path: &str, text: &str) -> Result<()> {
        self.write(path, text.as_bytes())
    }

    /// Appends `data` to the file `path` that this handle created (with [`write`](Folder::write) or an earlier
    /// `append`), or creates it, with the folders it needs, when it does not exist yet. Write a large file in pieces
    /// with it instead of holding it whole in memory.
    ///
    /// The same rules as [`write`](Folder::write): a file that exists and was not created through this handle is
    /// [`Denied`](crate::ErrorKind::Denied) (`permission denied: <path> already exists`), and so is the plugin's file
    /// once it was replaced by a link or given another name (a hard link). The limits count the file's
    /// size after the append ([`MAX_FILE`]), the handle's total ([`MAX_BYTES`]) and, for a new file, [`MAX_FILES`].
    /// A failed append may leave part of `data` at the end of the file (a failed first one leaves no file); a later
    /// [`write`](Folder::write) replaces the file whole, and it stays this handle's own.
    pub fn append(&self, path: &str, data: &[u8]) -> Result<()> {
        host_result(self.folder.append(path, data))
    }

    /// Like [`append`](Folder::append), with `text` as UTF-8.
    pub fn append_str(&self, path: &str, text: &str) -> Result<()> {
        self.append(path, text.as_bytes())
    }

    /// Reads at most `max` bytes (at most [`MAX_READ`]) of the file `path` from byte `offset`: fewer at the end of the
    /// file, none at or past it. Any data file in the folder may be read, also one this handle did not write, by the
    /// same path rules and data-file types as [`write`](Folder::write). Refused
    /// ([`Denied`](crate::ErrorKind::Denied)): a `max` over [`MAX_READ`], another file type, a hidden name, a path
    /// through a link, a file with other names (a hard link), a file in a folder of the Manager. A missing file is
    /// [`NotFound`](crate::ErrorKind::NotFound). The time spent reading does not count against the call's time limit.
    ///
    /// Read a large file in pieces (and keep only what you need of each) instead of whole: the plugin has 64 MiB of
    /// memory in all.
    pub fn read(&self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>> {
        host_result(self.folder.read(path, offset, max))
    }

    /// The whole file `path`, read with [`read`](Folder::read) in pieces of [`MAX_READ`] until a piece comes back
    /// short. The same rules and errors as [`read`](Folder::read); the whole file must fit in the plugin's memory.
    pub fn read_all(&self, path: &str) -> Result<Vec<u8>> {
        read_all_with(|offset, max| self.read(path, offset, max))
    }

    /// A page of the entries of the folder `path` (`""` for the picked folder itself), sorted by name: at most
    /// [`MAX_ENTRIES`] from index `start`; the next page starts at `start` plus the length of this one, and a page past
    /// the last entry is empty. Files of every type with their sizes, and folders. Links (symlinks, junctions), hidden
    /// names (starting with `.`) and names a path cannot name (other characters than `A-Z a-z 0-9 . _ -`) are left
    /// out. Refused ([`Denied`](crate::ErrorKind::Denied)): a path through a link, a hidden name, a folder of the
    /// Manager, a folder with more than [`MAX_LISTED`] entries. A missing folder is
    /// [`NotFound`](crate::ErrorKind::NotFound).
    pub fn list(&self, path: &str, start: u32) -> Result<Vec<Entry>> {
        let entries = host_result(self.folder.list(path, start))?;
        Ok(entries.into_iter().map(|e| Entry { name: e.name, is_folder: e.is_folder, size: e.size }).collect())
    }

    /// Every entry of the folder `path`, page after page of [`list`](Folder::list) until a short one. The same rules
    /// and errors as [`list`](Folder::list).
    pub fn list_all(&self, path: &str) -> Result<Vec<Entry>> {
        list_all_with(|start| self.list(path, start))
    }

    /// Opens the folder in the system's file manager. At most once every 2 s per handle; a quicker repeat is
    /// [`Denied`](crate::ErrorKind::Denied).
    pub fn reveal(&self) -> Result<()> {
        host_result(self.folder.reveal())
    }

    /// Hands the file `path`, written through this handle and at most 32 MiB, to the Manager's import preview, where
    /// the user reviews it before anything is changed. Another file is [`Denied`](crate::ErrorKind::Denied).
    pub fn open_import_preview(&self, path: &str) -> Result<()> {
        host_result(self.folder.open_import_preview(path))
    }
}

/// [`Folder::list_all`] over `list(start)`: pages from index 0 until one is shorter than [`MAX_ENTRIES`].
fn list_all_with(mut list: impl FnMut(u32) -> Result<Vec<Entry>>) -> Result<Vec<Entry>> {
    let mut all = Vec::new();
    loop {
        let start = u32::try_from(all.len()).map_err(|_| crate::Error::denied("the folder has too many entries"))?;
        let page = list(start)?;
        let last = page.len() < MAX_ENTRIES;
        all.extend(page);
        if last {
            return Ok(all);
        }
    }
}

/// [`Folder::read_all`] over `read(offset, max)`: pieces of [`MAX_READ`] from offset 0 until one comes back short.
fn read_all_with(mut read: impl FnMut(u64, u32) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    let mut all = Vec::new();
    loop {
        let piece = read(all.len() as u64, MAX_READ)?;
        let last = piece.len() < MAX_READ as usize;
        all.extend_from_slice(&piece);
        if last {
            return Ok(all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, ErrorKind};

    /// A file of `len` bytes (byte `i` is `i % 251`) read as the host does: at most `max` bytes from `offset`.
    fn file(len: usize, calls: &mut Vec<(u64, u32)>) -> impl FnMut(u64, u32) -> Result<Vec<u8>> + '_ {
        move |offset, max| {
            calls.push((offset, max));
            let start = (offset as usize).min(len);
            let end = start.saturating_add(max as usize).min(len);
            Ok((start..end).map(|i| (i % 251) as u8).collect())
        }
    }

    #[test]
    fn read_all_reads_in_pieces_until_a_short_one() {
        let piece = MAX_READ as usize;
        for (len, expected_calls) in [(0, 1), (10, 1), (piece - 1, 1), (piece, 2), (piece + 1, 2), (2 * piece + 7, 3)] {
            let mut calls = Vec::new();
            let all = read_all_with(file(len, &mut calls)).unwrap();
            assert_eq!(all.len(), len);
            assert!(all.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
            assert_eq!(calls.len(), expected_calls, "{len} bytes");
            for (n, (offset, max)) in calls.iter().enumerate() {
                assert_eq!((*offset, *max), ((n * piece) as u64, MAX_READ), "never more than 4 MiB at once");
            }
        }
    }

    #[test]
    fn read_all_stops_at_the_first_error() {
        let mut calls = 0;
        let error = read_all_with(|offset, _| {
            calls += 1;
            if offset == 0 {
                Ok(vec![0; MAX_READ as usize])
            } else {
                Err(Error::from_host("permission denied: x.json has other names (hard links)".into()))
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Denied);
        assert_eq!(calls, 2);
    }

    #[test]
    fn list_all_pages_until_a_short_page() {
        let entry = |i: usize| Entry { name: format!("{i:06}.png"), is_folder: false, size: 0 };
        for (count, expected_calls) in [(0, 1), (5, 1), (MAX_ENTRIES, 2), (MAX_ENTRIES + 1, 2), (2 * MAX_ENTRIES + 3, 3)] {
            let mut starts = Vec::new();
            let all = list_all_with(|start| {
                starts.push(start);
                let from = (start as usize).min(count);
                Ok((from..(from + MAX_ENTRIES).min(count)).map(entry).collect())
            })
            .unwrap();
            assert_eq!(all, (0..count).map(entry).collect::<Vec<_>>(), "{count} entries");
            assert_eq!(starts.len(), expected_calls, "{count} entries");
            assert!(starts.iter().enumerate().all(|(n, s)| *s as usize == n * MAX_ENTRIES));
        }
    }

    #[test]
    fn read_and_list_limits_are_the_managers() {
        // folders.rs `MAX_FOLDER_READ`, `MAX_FOLDER_ENTRIES`.
        assert_eq!(MAX_READ, 4_194_304);
        assert_eq!(MAX_ENTRIES, 10_000);
        assert_eq!(MAX_LISTED, 1_000_000);
    }

    #[test]
    fn data_extensions_are_lower_case_and_distinct() {
        for (i, ext) in DATA_EXTENSIONS.iter().enumerate() {
            assert!(!ext.is_empty() && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{ext}");
            assert!(!DATA_EXTENSIONS[..i].contains(ext), "{ext} twice");
        }
    }

    #[test]
    fn limits_are_the_managers() {
        // folders.rs `MAX_FOLDER_FILES`, `MAX_FOLDER_BYTES`, `MAX_FOLDER_FILE`.
        assert_eq!(MAX_FILES, 100_000);
        assert_eq!(MAX_BYTES, 2_147_483_648);
        assert_eq!(MAX_FILE, 268_435_456);
    }
}
