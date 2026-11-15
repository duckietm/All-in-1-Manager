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
