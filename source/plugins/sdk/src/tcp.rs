//! Connections to local services on this computer (permission `tcp:localhost:<port>`, or
//! `tcp:localhost:<a>-<b>` for a range of at most 16 ports with `"api": "1.1"`).
//!
//! The Manager's rules:
//! - only `localhost` (127.0.0.1), only a port the manifest lists (alone or in a range), and at most 4 connections
//!   open at once; both refusals are [`ErrorKind::Denied`];
//! - connecting has 5 s and each [`recv`](Conn::recv) has 30 s ([`ErrorKind::Timeout`]);
//!   [`recv_timeout`](Conn::recv_timeout) waits as long as asked (at most 30 s) and returns `None` instead;
//! - one [`recv`](Conn::recv) returns at most 1 MiB.
//!
//! ```no_run
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! let mut conn = aio_plugin_sdk::tcp::connect(3000)?;
//! conn.send(b"ping\n")?;
//! let reply = conn.recv(1024)?;
//! # let _ = reply;
//! # Ok(())
//! # }
//! ```

use crate::bindings::aio::plugin::host;
use crate::error::{host_result, Error, ErrorKind, Result};
use std::time::Duration;

/// Most bytes one [`Conn::recv`] returns (the Manager's limit).
pub const MAX_RECV: usize = 1024 * 1024;

/// Longest wait of one [`Conn::recv_timeout`] (the Manager's limit); a longer one waits this long.
pub const MAX_RECV_TIMEOUT: Duration = Duration::from_secs(30);

/// Connects to `localhost:<port>`.
pub fn connect(port: u16) -> Result<Conn> {
    Ok(Conn { conn: host_result(host::tcp_connect(port))? })
}

/// An open local connection. Closed by [`close`](Conn::close) or when dropped.
#[derive(Debug)]
pub struct Conn {
    conn: host::TcpConnection,
}

impl Conn {
    /// Sends all of `data`.
    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        host_result(self.conn.send(data))
    }

    /// Waits for data and returns what is available, at most `max` bytes (and at most [`MAX_RECV`]). An empty
    /// result means the other side closed the connection. `max` must be at least 1.
    pub fn recv(&mut self, max: usize) -> Result<Vec<u8>> {
        let max = recv_max(max)?;
        host_result(self.conn.recv(max))
    }

    /// Like [`recv`](Conn::recv), but waits at most `timeout` (at most [`MAX_RECV_TIMEOUT`]; a longer one waits
    /// that long, a part of a millisecond counts as a whole one). `Ok(None)` means nothing arrived in time and the
    /// connection stays usable; `Ok(Some(empty))` means the other side closed the connection. A `timeout` of zero
    /// takes only what has already arrived, so a [`tick`](crate::Plugin::tick) can poll without waiting.
    ///
    /// Needs `"api": "1.1"` in the manifest.
    ///
    /// ```no_run
    /// # fn main() -> aio_plugin_sdk::Result<()> {
    /// use std::time::Duration;
    ///
    /// let mut conn = aio_plugin_sdk::tcp::connect(9092)?;
    /// match conn.recv_timeout(4096, Duration::from_millis(200))? {
    ///     None => {}                              // nothing yet: try again in the next tick
    ///     Some(bytes) if bytes.is_empty() => {}   // closed
    ///     Some(bytes) => { let _ = bytes; }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn recv_timeout(&mut self, max: usize, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let max = recv_max(max)?;
        host_result(self.conn.recv_timeout(max, timeout_ms(timeout)))
    }

    /// Closes the connection (the same as dropping it).
    pub fn close(self) {}
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.conn.close();
    }
}

/// `max` for the host: at least 1 (the host's own refusal otherwise), at most [`MAX_RECV`].
fn recv_max(max: usize) -> Result<u32> {
    if max == 0 {
        return Err(Error::new(ErrorKind::Other, "max must be at least 1"));
    }
    Ok(u32::try_from(max.min(MAX_RECV)).unwrap_or(u32::MAX))
}

/// `timeout` for the host: whole milliseconds rounded up, at most [`MAX_RECV_TIMEOUT`].
// `%`, not `is_multiple_of` (Rust 1.90): the SDK builds with Rust 1.88 (its `rust-version`).
#[allow(clippy::manual_is_multiple_of)]
fn timeout_ms(timeout: Duration) -> u32 {
    let timeout = timeout.min(MAX_RECV_TIMEOUT);
    let ms = timeout.as_millis() + u128::from(timeout.subsec_nanos() % 1_000_000 != 0);
    u32::try_from(ms).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recv_size_is_checked_and_capped() {
        // The host's text for `max == 0` (runtime.rs `HostTcpConnection::recv`).
        assert_eq!(recv_max(0).unwrap_err().message(), "max must be at least 1");
        assert_eq!(recv_max(1).unwrap(), 1);
        assert_eq!(recv_max(usize::MAX).unwrap(), 1024 * 1024);
    }

    #[test]
    fn recv_timeout_is_whole_milliseconds_up_to_30_s() {
        assert_eq!(timeout_ms(Duration::ZERO), 0);
        assert_eq!(timeout_ms(Duration::from_millis(250)), 250);
        // A part of a millisecond waits the whole millisecond, so a short wait never becomes "only what arrived".
        assert_eq!(timeout_ms(Duration::from_micros(1)), 1);
        assert_eq!(timeout_ms(Duration::from_micros(1500)), 2);
        // The host's clamp (net.rs `MAX_RECV_TIMEOUT_MS`).
        assert_eq!(timeout_ms(Duration::from_secs(30)), 30_000);
        assert_eq!(timeout_ms(Duration::from_secs(31)), 30_000);
        assert_eq!(timeout_ms(Duration::MAX), 30_000);
    }
}
