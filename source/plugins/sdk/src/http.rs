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
/// add up to at most [`MAX_MANY_BYTES`], reserved while they are read: a URL whose body does not fit in what is left
/// gets `permission denied: response budget exceeded` ([`Denied`](crate::ErrorKind::Denied)) and the others go on.
/// More than [`MAX_MANY`] URLs: nothing is fetched (the SDK does not even ask the Manager) and every result is
/// `permission denied: at most 8 URLs per call`, so the results still line up with `urls`.
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
    if urls.len() > MAX_MANY {
        return one_per_url(Vec::new(), urls.len());
    }
    let owned: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
    one_per_url(host::http_get_many(&owned), urls.len())
}

/// The Manager's results as one per URL: as they are when there is one per URL; otherwise (the Manager refuses a call
/// of more than [`MAX_MANY`] URLs with a single entry) its refusal, or the SDK's own, for every URL.
fn one_per_url(results: Vec<std::result::Result<Vec<u8>, String>>, urls: usize) -> Vec<Result<Vec<u8>>> {
    if results.len() == urls {
        return results.into_iter().map(host_result).collect();
    }
    let refusal = results
        .into_iter()
        .find_map(|r| r.err())
        .unwrap_or_else(|| format!("permission denied: at most {MAX_MANY} URLs per call"));
    (0..urls).map(|_| Err(crate::Error::from_host(refusal.clone()))).collect()
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

    #[test]
    fn more_than_8_urls_give_one_refusal_per_url() {
        // The Manager's single-entry refusal is repeated for every URL, so results zip with the URLs.
        let host = vec![Err("permission denied: at most 8 URLs per call".to_string())];
        let results = one_per_url(host, 20);
        assert_eq!(results.len(), 20);
        for r in &results {
            let e = r.as_ref().unwrap_err();
            assert_eq!((e.kind(), e.message()), (crate::ErrorKind::Denied, "permission denied: at most 8 URLs per call"));
        }
        // The SDK's own refusal (the Manager is not asked) reads the same.
        let results = one_per_url(Vec::new(), 9);
        assert_eq!(results.len(), 9);
        assert!(results.iter().all(|r| r.as_ref().unwrap_err().message() == "permission denied: at most 8 URLs per call"));
        // One result per URL passes through as it is.
        let results = one_per_url(vec![Ok(b"a".to_vec()), Err("HTTP 404".into())], 2);
        assert_eq!(results[0].as_deref().unwrap(), b"a");
        assert_eq!(results[1].as_ref().unwrap_err().message(), "HTTP 404");
    }
}
