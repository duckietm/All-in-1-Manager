//! The plugin's private data folder: files only this plugin can read and write, kept between sessions.
//!
//! Declare the `files` permission so the user sees that the plugin keeps files; the Manager lists it as always
//! allowed and does not require it.
//!
//! Paths are relative to the private folder and use `/` between folders (`"notes/today.txt"`); missing folders are
//! created by [`write()`]. The Manager's rules:
//! - a path has at most 255 bytes and 8 levels, only the characters `A-Z a-z 0-9 . _ -` in each name, and no empty,
//!   `.` or `..` names, no name ending in `.` and no Windows device name (`CON`, `NUL`, `COM1`, ...); anything else
//!   is refused as an invalid path ([`ErrorKind::Other`]);
//! - a path that leads out of the folder (through a link) is [`ErrorKind::Denied`];
//! - one file holds at most 32 MiB; the folder holds at most 64 MiB in all and at most 10,000 files and folders.
//!
//! A missing file is [`ErrorKind::NotFound`]. [`read_if_exists`] and [`read_to_string_if_exists`] turn it into
//! `None`; otherwise match on the kind:
//!
//! ```no_run
//! use aio_plugin_sdk::{files, ErrorKind};
//!
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! files::write_str("notes/today.txt", "buy plants")?;
//! assert_eq!(files::read_to_string("notes/today.txt")?, "buy plants");
//!
//! let settings = files::read_to_string_if_exists("settings.txt")?.unwrap_or_default();
//! # let _ = settings;
//! let count: u32 = match files::read_to_string("count.txt") {
//!     Ok(text) => text.trim().parse().unwrap_or(0),
//!     Err(e) if e.kind() == ErrorKind::NotFound => 0,
//!     Err(e) => return Err(e),
//! };
//! # let _ = count;
//! # Ok(())
//! # }
//! ```

use crate::bindings::aio::plugin::host;
use crate::error::{host_result, Error, ErrorKind, Result};

/// The whole content of the private file at `path`.
pub fn read(path: &str) -> Result<Vec<u8>> {
    host_result(host::file_read(path))
}

/// Creates or replaces the private file at `path` with `data`, creating missing folders.
pub fn write(path: &str, data: &[u8]) -> Result<()> {
    host_result(host::file_write(path, data))
}

/// The content of the private file at `path` as text; fails ([`ErrorKind::Other`]) if it is not UTF-8.
pub fn read_to_string(path: &str) -> Result<String> {
    utf8(path, read(path)?)
}

/// Creates or replaces the private file at `path` with `text` (UTF-8), creating missing folders.
pub fn write_str(path: &str, text: &str) -> Result<()> {
    write(path, text.as_bytes())
}

/// The whole content of the private file at `path`, or `None` if it does not exist.
pub fn read_if_exists(path: &str) -> Result<Option<Vec<u8>>> {
    if_exists(read(path))
}

/// The content of the private file at `path` as text, or `None` if it does not exist; fails
/// ([`ErrorKind::Other`]) if it is not UTF-8.
pub fn read_to_string_if_exists(path: &str) -> Result<Option<String>> {
    if_exists(read(path))?.map(|bytes| utf8(path, bytes)).transpose()
}

/// `Ok(None)` for [`ErrorKind::NotFound`].
fn if_exists<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn utf8(path: &str, bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Error::new(ErrorKind::Other, format!("the file '{path}' is not UTF-8 text")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_must_be_utf8() {
        assert_eq!(utf8("a.txt", b"hi".to_vec()).unwrap(), "hi");
        let error = utf8("a.txt", vec![0xff]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert_eq!(error.message(), "the file 'a.txt' is not UTF-8 text");
    }

    #[test]
    fn only_a_missing_file_becomes_none() {
        assert_eq!(if_exists(Ok(1)).unwrap(), Some(1));
        // The host's text for a missing file (runtime.rs `io_text`).
        assert_eq!(if_exists::<()>(Err(Error::from_host("file not found".into()))).unwrap(), None);
        let error = if_exists::<()>(Err(Error::from_host("file larger than 32 MiB".into()))).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
    }
}
