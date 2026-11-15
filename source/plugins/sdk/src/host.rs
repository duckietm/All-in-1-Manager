//! The Manager itself: the plugin log, the status line, dialogs and the files the user picks.
//!
//! None of these needs a permission. A dialog (`ask`, [`prompt_text`], the file pickers) waits for the user, and
//! that time does not count against the call's time limit; one call (one [`render`](crate::Plugin::render) or
//! [`on_event`](crate::Plugin::on_event)) may open at most 20 dialogs. Past that, every further dialog gets the
//! cancel answer (`false` or `None`) without being shown, and the plugin log says so once.
//!
//! These calls work only inside the Manager. Off `wasm32` (a native `cargo test`), [`log`] and [`status`] write
//! to stderr; the other calls panic.

use crate::bindings::aio::plugin::host;
use crate::error::{Error, Result};

/// How important a [`log`] line is.
///
/// New levels may be added in a later 1.x version: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Level {
    /// Detail for the plugin's developer.
    Debug,
    /// Normal progress.
    Info,
    /// Something went wrong but the plugin carries on.
    Warn,
    /// Something failed.
    Error,
}

/// Appends `text` to the plugin log (shown on the plugin's page in the Manager).
///
/// The Manager keeps at most 4 KiB of one entry (the rest is cut) and at most 1,000 entries and 256 KiB per call
/// (status texts included); past that it writes "log truncated by the host" once and drops the rest. The plugin's
/// stdout and stderr also end up in the log, one entry per line.
pub fn log(level: Level, text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::bindings::aio::plugin::types as wit;
        let level = match level {
            Level::Debug => wit::LogLevel::Debug,
            Level::Info => wit::LogLevel::Info,
            Level::Warn => wit::LogLevel::Warn,
            Level::Error => wit::LogLevel::Error,
        };
        host::log(level, text);
    }
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("[{level:?}] {text}");
}

/// Sets the Manager's status line to `text` (it is also logged, and counts towards the log limits of [`log`]).
pub fn status(text: &str) {
    #[cfg(target_arch = "wasm32")]
    host::status(text);
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("[status] {text}");
}

/// Asks the Manager to call [`Plugin::tick`](crate::Plugin::tick) once more after the next render: the next step of
/// a long job. Call it in [`on_event`](crate::Plugin::on_event) to start the job and in each `tick` that leaves work
/// to do; the request holds only for the call that makes it. Steps run only while the plugin's page is open
/// (leaving the page, Stop or Reload ends the job), and events the user sends meanwhile come first.
///
/// Needs `"api": "1.1"` in the manifest.
pub fn keep_working() {
    host::keep_working();
}

/// Asks the user a yes/no question and waits for the answer: `true` for yes. `false` past the dialog limit.
pub fn ask(question: &str) -> bool {
    host::ask(question)
}

/// Asks the user for a line of text in a dialog titled `title`, with `label` before the field and `default`
/// already typed in. `None` if the user cancelled (or past the dialog limit).
pub fn prompt_text(title: &str, label: &str, default: &str) -> Option<String> {
    host::prompt_text(title, label, default)
}

/// Shows an open-file dialog. `extensions` are without dots (`["sql", "txt"]`, or a `Vec<String>`); an empty list
/// allows every file (write it with its type, for example `[] as [&str; 0]`). `None` if the user cancelled (or
/// past the dialog limit).
///
/// The file can only be [read](PickedFile::read) (or handed to [`open_import_preview`]).
///
/// ```no_run
/// use aio_plugin_sdk::host;
///
/// if let Some(file) = host::pick_open_file("Import a catalogue", ["sql"]) {
///     let bytes = file.read();
///     # let _ = bytes;
/// }
/// let any = host::pick_open_file("Any file", [] as [&str; 0]);
/// # let _ = any;
/// ```
pub fn pick_open_file(title: &str, extensions: impl IntoIterator<Item = impl AsRef<str>>) -> Option<PickedFile> {
    let file = host::pick_open_file(title, &owned(extensions))?;
    Some(PickedFile { file, mode: Mode::Open })
}

/// Shows a save-file dialog with `suggested_name` filled in. `extensions` are without dots; an empty list allows
/// every file (write it with its type, for example `[] as [&str; 0]`). `None` if the user cancelled (or past the
/// dialog limit).
///
/// The file can only be [written](PickedFile::write).
pub fn pick_save_file(
    title: &str,
    suggested_name: &str,
    extensions: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<PickedFile> {
    let file = host::pick_save_file(title, suggested_name, &owned(extensions))?;
    Some(PickedFile { file, mode: Mode::Save })
}

/// Hands a file picked with [`pick_open_file`] to the Manager's import preview, where the user reviews it before
/// anything is changed. Files of at most 32 MiB. A file picked for saving is refused
/// ([`Denied`](crate::ErrorKind::Denied)).
pub fn open_import_preview(file: &PickedFile) -> Result<()> {
    file.check(Mode::Open)?;
    crate::error::host_result(host::open_import_preview(&file.file))
}

fn owned(list: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    list.into_iter().map(|s| s.as_ref().to_string()).collect()
}

/// What a [`PickedFile`] was picked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Open,
    Save,
}

impl Mode {
    /// The host's refusal when a file picked for `self` is used for the other thing.
    fn refusal(self) -> &'static str {
        match self {
            Mode::Save => "a file picked for saving cannot be read",
            Mode::Open => "a file picked for opening cannot be written",
        }
    }
}

/// A file the user picked in a dialog ([`pick_open_file`], [`pick_save_file`]). The plugin never sees its path,
/// only its [name](PickedFile::name) and content.
///
/// A file picked for opening is read-only and a file picked for saving is write-only: the other call returns
/// [`ErrorKind::Denied`](crate::ErrorKind::Denied). The handle is released when the value is dropped.
#[derive(Debug)]
pub struct PickedFile {
    file: host::PickedFile,
    mode: Mode,
}

impl PickedFile {
    /// The file name, without folders.
    pub fn name(&self) -> String {
        self.file.name()
    }

    /// `true` for a file from [`pick_save_file`], `false` for one from [`pick_open_file`].
    pub fn is_for_saving(&self) -> bool {
        self.mode == Mode::Save
    }

    /// The whole content of a file picked for opening (at most 32 MiB).
    pub fn read(&self) -> Result<Vec<u8>> {
        self.check(Mode::Open)?;
        crate::error::host_result(self.file.read())
    }

    /// Replaces the content of a file picked for saving with `data`.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        self.check(Mode::Save)?;
        crate::error::host_result(self.file.write(data))
    }

    fn check(&self, wanted: Mode) -> Result<()> {
        if self.mode == wanted {
            Ok(())
        } else {
            Err(Error::new(crate::ErrorKind::Denied, self.mode.refusal()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::ErrorKind;

    #[test]
    fn wrong_mode_refusals_match_the_host_and_are_denied() {
        // The texts the host returns for the same misuse (runtime.rs `HostPickedFile`).
        assert_eq!(Mode::Save.refusal(), "a file picked for saving cannot be read");
        assert_eq!(Mode::Open.refusal(), "a file picked for opening cannot be written");
        for mode in [Mode::Open, Mode::Save] {
            assert_eq!(Error::from_host(mode.refusal().to_string()).kind(), ErrorKind::Denied);
        }
    }

    #[test]
    fn log_and_status_work_natively() {
        log(Level::Warn, "a warning");
        status("busy");
    }
}
