//! HTTP(S) requests to the hosts the plugin declares (permission `http:<host>`).
//!
//! The Manager's rules:
//! - only `http` and `https` URLs of at most 2,048 bytes, on the scheme's default port, without user information,
//!   to a host listed in the manifest; anything else is [`ErrorKind::Denied`](crate::ErrorKind::Denied);
//! - a redirect is followed only to the same host, on its default port and never from https to http; a redirect
//!   elsewhere is [`ErrorKind::Denied`](crate::ErrorKind::Denied);
//! - a status other than 2xx is an error with the message `HTTP <code>` (for example `HTTP 404`);
//! - the response body has at most 32 MiB, and the request has 60 s
//!   ([`ErrorKind::Timeout`](crate::ErrorKind::Timeout));
//! - [`get_many`] (API 1.1) fetches up to [`MAX_MANY`] URLs at once by the same rules; their bodies add up to at most
//!   [`MAX_MANY_BYTES`].

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

/// Most URLs one [`get_many`] takes.
pub const MAX_MANY: usize = 8;

/// Most bytes of the bodies one [`get_many`] returns, together (32 MiB).
pub const MAX_MANY_BYTES: usize = 32 * 1024 * 1024;

/// GETs up to [`MAX_MANY`] URLs at once and returns one result per URL, in the order of `urls` (API 1.1). Needs the
/// same permissions as [`get`].
///
/// Each URL is checked exactly like [`get`]. Under `http:ask`, the hosts not answered yet are asked about one at a
/// time, in order, before anything is fetched (each question is one of the call's 20 dialogs, and a host asked about
/// once is not asked again). Then the allowed URLs are fetched together, each with its own 60 s. The bodies of one call
/// add up to at most [`MAX_MANY_BYTES`]: the URL whose body does not fit, and every one not finished by then, get
/// `permission denied: response budget exceeded` ([`Denied`](crate::ErrorKind::Denied)). More than [`MAX_MANY`] URLs:
/// every result is `permission denied: at most 8 URLs per call` and nothing is fetched.
///
/// Needs `"api": "1.1"` in the manifest.
///
/// ```no_run
/// use aio_plugin_sdk::http;
///
/// let urls = ["https://images.example.com/a.png", "https://images.example.com/b.png"];
/// for (url, result) in urls.iter().zip(http::get_many(&urls)) {
///     match result {
///         Ok(body) => aio_plugin_sdk::host::status(&format!("{url}: {} bytes", body.len())),
///         Err(e) => aio_plugin_sdk::host::status(&format!("{url}: {e}")),
///     }
/// }
/// ```
pub fn get_many(urls: &[&str]) -> Vec<Result<Vec<u8>>> {
    let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
    host::http_get_many(&urls).into_iter().map(host_result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_limits_are_the_managers() {
        // net.rs `MAX_MANY_URLS`, `MAX_MANY_BYTES`.
        assert_eq!(MAX_MANY, 8);
        assert_eq!(MAX_MANY_BYTES, 33_554_432);
        let budget = crate::Error::from_host("permission denied: response budget exceeded".into());
        assert_eq!(budget.kind(), crate::ErrorKind::Denied);
    }
}
