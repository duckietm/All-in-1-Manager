//! The error every fallible SDK call returns.

use std::fmt;

/// A `Result` whose error is the SDK's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why an SDK call failed: a [`kind`](Error::kind) to act on and a [`message`](Error::message) to show.
///
/// It converts into `String` with `?`, so it can be returned from [`Plugin::on_event`](crate::Plugin::on_event)
/// directly:
///
/// ```
/// # use aio_plugin_sdk::{Error, ErrorKind};
/// fn handle() -> Result<(), String> {
///     let result: Result<(), Error> = Err(Error::new(ErrorKind::Other, "it failed"));
///     result?;
///     Ok(())
/// }
/// assert_eq!(handle(), Err("it failed".to_string()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

/// The broad reason for an [`Error`].
///
/// New kinds may be added in a later 1.x version: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The plugin is not allowed to do this: a permission is missing from its manifest, or the request is outside
    /// what the permission covers (another table, host, port or folder).
    Denied,
    /// The user was asked to approve the action (a script, database changes) and said no.
    Rejected,
    /// The action took longer than the Manager allows.
    Timeout,
    /// The file does not exist: a private file that was never written ([`files::read`](crate::files::read)), or a
    /// picked file that was removed after the user chose it.
    NotFound,
    /// Anything else: a network or file error, a page past the Manager's limits, a value the host refused.
    Other,
}

impl Error {
    /// An error of `kind` with `message`, for plugin code that wants to report its own failures the same way.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// The broad reason, to decide what to do (for example, explain which permission to grant on
    /// [`ErrorKind::Denied`]).
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The human-readable description.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// An error the Manager returned from a host call, with its kind read from the text:
    ///
    /// - `permission denied: ...` and `access denied...` (a missing permission, a path outside the private folder),
    ///   the wrong use of a picked file and a refused redirect to another host are [`ErrorKind::Denied`];
    /// - `the user rejected ...` is [`ErrorKind::Rejected`];
    /// - `<what> timed out after <n> s` is [`ErrorKind::Timeout`];
    /// - exactly `file not found` is [`ErrorKind::NotFound`];
    /// - anything else is [`ErrorKind::Other`].
    pub(crate) fn from_host(message: String) -> Self {
        Self { kind: host_kind(&message), message }
    }

    /// The error a host call would return, for a request the SDK refuses before making it.
    pub(crate) fn denied(reason: impl fmt::Display) -> Self {
        Self::new(ErrorKind::Denied, format!("{DENIED}: {reason}"))
    }
}

/// A host call's result with its error text turned into an [`Error`].
pub(crate) fn host_result<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(Error::from_host)
}

/// How the host starts every permission refusal (`plugin_host` runtime.rs, sql.rs, net.rs, approvals.rs).
const DENIED: &str = "permission denied";

/// Texts of the host that refuse a request without the `permission denied` prefix.
const DENIED_TEXTS: &[&str] = &[
    // An OS permission error or a path leading out of the private folder (runtime.rs `io_text`, `resolve_private`).
    "access denied",
    // A picked file used the wrong way (runtime.rs).
    "a file picked for saving cannot be read",
    "a file picked for opening cannot be written",
];

/// The host's whole text for a missing file (runtime.rs `io_text`).
const NOT_FOUND: &str = "file not found";

/// A redirect off the checked host, refused by the Manager's HTTP client (app plugins/services.rs); it ends the
/// request error's text.
const REDIRECT_REFUSED: &str = "a redirect to another host was refused";

fn host_kind(text: &str) -> ErrorKind {
    if text.starts_with(DENIED) || DENIED_TEXTS.iter().any(|t| text.starts_with(t)) || text.ends_with(REDIRECT_REFUSED) {
        ErrorKind::Denied
    } else if text.starts_with("the user rejected ") {
        ErrorKind::Rejected
    } else if is_timeout(text) {
        ErrorKind::Timeout
    } else if text == NOT_FOUND {
        ErrorKind::NotFound
    } else {
        ErrorKind::Other
    }
}

/// `<what> timed out after <seconds> s`, the host's text for a call past its time limit (runtime.rs `limited`).
fn is_timeout(text: &str) -> bool {
    let Some((_, tail)) = text.rsplit_once(" timed out after ") else {
        return false;
    };
    tail.strip_suffix(" s").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<Error> for String {
    /// The message, so `?` works in functions returning `Result<_, String>` such as
    /// [`Plugin::on_event`](crate::Plugin::on_event).
    fn from(error: Error) -> Self {
        error.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The texts below are copied verbatim from the host (`plugin_host` runtime.rs, sql.rs, net.rs, approvals.rs and
    /// the app's plugins/services.rs).
    #[test]
    fn error_kind_from_host_strings() {
        let denied = [
            "permission denied: running a script needs the 'ssh:root' permission",
            "permission denied: the script holds control or invisible characters (only tab and newline are allowed), \
             so it could not be shown exactly as it would run",
            "permission denied: an empty script",
            "permission denied: the script is larger than 64 KiB",
            "permission denied: table 'users' is not declared with sql:read",
            "permission denied: column 'users.password' is private (needs sql:read-private:users.password)",
            "permission denied: list the columns to read (there is no 'every column')",
            "permission denied: an update of 'users' needs at least one condition",
            "permission denied: a delete from 'users' needs at least one condition",
            "permission denied: more than 1000 operations in one batch",
            "permission denied: host 'example.com' is not declared with http:example.com",
            "permission denied: port 3000 is not declared with tcp:localhost:3000",
            "permission denied: at most 4 local connections may be open at once",
            "access denied: outside the private folder",
            "access denied",
            "a file picked for saving cannot be read",
            "a file picked for opening cannot be written",
            "error following redirect for url (https://example.com/a): a redirect to another host was refused",
        ];
        for text in denied {
            assert_eq!(Error::from_host(text.to_string()).kind(), ErrorKind::Denied, "{text}");
        }

        for text in ["the user rejected the script", "the user rejected the database changes"] {
            assert_eq!(Error::from_host(text.to_string()).kind(), ErrorKind::Rejected, "{text}");
        }

        let timeouts = [
            "the script timed out after 600 s",
            "the query timed out after 30 s",
            "the database changes timed out after 30 s",
            "the request timed out after 60 s",
            "connecting timed out after 5 s",
            "recv timed out after 30 s",
        ];
        for text in timeouts {
            assert_eq!(Error::from_host(text.to_string()).kind(), ErrorKind::Timeout, "{text}");
        }

        assert_eq!(Error::from_host("file not found".to_string()).kind(), ErrorKind::NotFound);

        let other = [
            "Not connected",
            "The server changed since the request",
            "Dry run: there is no database",
            "file larger than 32 MiB",
            "the private folder is full (64 MiB)",
            "the private folder is full (10000 files and folders)",
            "invalid path: use a relative path with / and the characters A-Z a-z 0-9 . _ -",
            "a folder has that name",
            "the response is larger than 32 MiB",
            "HTTP 404",
            "connecting to port 3000 failed (ConnectionRefused)",
            "the connection is closed",
            "max must be at least 1",
            "i/o error (Other)",
            // Look-alikes that are not the host's own refusals.
            "ssh: Permission denied (publickey)",
            "the user rejected",
            "the job timed out after a while",
            "file not found: notes.txt",
            "the file not found",
        ];
        for text in other {
            assert_eq!(Error::from_host(text.to_string()).kind(), ErrorKind::Other, "{text}");
        }

        let error = Error::from_host("the user rejected the script".to_string());
        assert_eq!(error.message(), "the user rejected the script");
    }
}
