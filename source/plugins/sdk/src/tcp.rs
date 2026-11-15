//! Connections to local services on this computer (permission `tcp:localhost:<port>`).
//!
//! The Manager's rules:
//! - only `localhost` (127.0.0.1), only a port listed in the manifest, and at most 4 connections open at once;
//!   both refusals are [`ErrorKind::Denied`];
//! - connecting has 5 s and each [`recv`](Conn::recv) has 30 s ([`ErrorKind::Timeout`]);
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

/// Most bytes one [`Conn::recv`] returns (the Manager's limit).
pub const MAX_RECV: usize = 1024 * 1024;

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
}
