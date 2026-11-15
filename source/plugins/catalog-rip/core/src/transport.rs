//! The byte stream the G-Earth client runs over. The core has no sockets: the plugin implements
//! this with the host's TCP connection (`tcp::Conn`), the native tests with a `TcpStream`.

/// A connected, ordered byte stream (a TCP connection to G-Earth).
pub trait Transport {
    /// Sends all of `data`.
    fn send(&mut self, data: &[u8]) -> Result<(), String>;

    /// At most `max` bytes, waiting at most `timeout_ms` for the first of them (0: only what
    /// is there already). `Ok(None)`: nothing arrived in time; `Ok(Some(empty))`: the peer
    /// closed the connection.
    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String>;
}

/// A transport with nothing behind it: every call fails. For a caller that already closed the
/// G-Earth connection but must still pass a transport (a job that is done with G-Earth never
/// touches it).
#[derive(Debug, Clone, Copy, Default)]
pub struct Closed;

impl Transport for Closed {
    fn send(&mut self, _data: &[u8]) -> Result<(), String> {
        Err("the connection to G-Earth is closed".into())
    }

    fn recv(&mut self, _max: usize, _timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        Err("the connection to G-Earth is closed".into())
    }
}
