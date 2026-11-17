//! Folders the user picks: a place outside the private folder where the plugin may save new data files (an export,
//! a downloaded catalogue). Needs `"api": "1.1"` in the manifest, and no permission: the user picks the folder each
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
//! - a path that leads through a link is refused;
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

#[cfg(test)]
mod tests {
    use super::*;

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
