//! HTTP(S) requests to the hosts the plugin declares (permission `http:<host>`).
//!
//! The Manager's rules:
//! - only `http` and `https` URLs of at most 2,048 bytes, on the scheme's default port, without user information,
//!   to a host listed in the manifest; anything else is [`ErrorKind::Denied`](crate::ErrorKind::Denied);
//! - a redirect is followed only to the same host, on its default port and never from https to http; a redirect
//!   elsewhere is [`ErrorKind::Denied`](crate::ErrorKind::Denied);
//! - a status other than 2xx is an error with the message `HTTP <code>` (for example `HTTP 404`);
//! - the response body has at most 32 MiB, and the request has 60 s
//!   ([`ErrorKind::Timeout`](crate::ErrorKind::Timeout)).

use crate::bindings::aio::plugin::host;
use crate::error::{host_result, Result};

/// GETs `url` and returns the response body.
///
/// ```no_run
/// # fn main() -> aio_plugin_sdk::Result<()> {
/// let body = aio_plugin_sdk::http::get("https://example.com/data.json")?;
/// # let _ = body;
/// # Ok(())
/// # }
/// ```
pub fn get(url: &str) -> Result<Vec<u8>> {
    host_result(host::http_get(url))
}
