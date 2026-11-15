//! Permission checks of `http-get` and `tcp-connect`.
//!
//! `check_http` accepts only a deliberately narrow URL grammar, so that no URL parser downstream can
//! read a different host out of it than this check did:
//!
//! `http[s]://<host>[:<port>][/<path>][?<query>][#<fragment>]`, printable ASCII only (no spaces, no
//! `\`), `<host>` made of `A-Z a-z 0-9 . -` (no userinfo, no `%`, no IPv6 brackets), `<port>` the
//! scheme's default port only (the permission grammar `http:<host>` names no port). The host must equal
//! a declared `http:<host>` exactly, ignoring case (no subdomains). The URL handed on is rebuilt from
//! the checked parts: scheme and host lower case, the default port and the fragment dropped.
//!
//! Not covered here (the app's HTTP client, Task 8): redirects must not leave the checked host, a non-2xx
//! status is returned as `Err("HTTP <code>")`, and the body read stops past [`MAX_HTTP_BODY`].

use crate::permissions::Permission;
use crate::runtime::sanitise;

/// Largest HTTP response body handed to a plugin.
pub const MAX_HTTP_BODY: usize = 32 * 1024 * 1024;
/// Longest URL accepted.
pub const MAX_URL: usize = 2_048;
/// Local TCP connections one plugin instance may hold open at once.
pub const MAX_TCP_CONNECTIONS: usize = 4;
/// Most bytes one `recv` returns.
pub const MAX_TCP_RECV: usize = 1024 * 1024;

fn deny(reason: impl std::fmt::Display) -> String {
    format!("permission denied: {reason}")
}

fn shown(url: &str) -> String {
    sanitise(&url.chars().take(200).collect::<String>())
}

/// Checks `url` against the plugin's `http:<host>` permissions; returns the URL to fetch.
pub fn check_http(url: &str, granted: &[Permission]) -> Result<String, String> {
    let bad = || deny(format!("'{}' is not an allowed http(s) URL", shown(url)));
    if url.is_empty() || url.len() > MAX_URL || !url.bytes().all(|b| b.is_ascii_graphic() && b != b'\\') {
        return Err(bad());
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return Err(deny(format!("only http and https URLs are allowed, not '{}'", shown(url)))),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return Err(deny(format!("a URL with user information is not allowed: '{}'", shown(url))));
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !host_ok {
        return Err(bad());
    }
    if let Some(p) = port {
        if p != default_port {
            return Err(deny(format!("'{}': only the default port of {scheme} is allowed", shown(url))));
        }
    }
    let host = host.to_ascii_lowercase();
    let declared = granted.iter().any(|p| matches!(p, Permission::Http(h) if h.eq_ignore_ascii_case(&host)));
    if !declared {
        return Err(deny(format!("host '{host}' is not declared with http:{host}")));
    }
    let tail = tail.split('#').next().unwrap_or("");
    Ok(format!("{scheme}://{host}{tail}"))
}

/// Checks a `tcp-connect` to `127.0.0.1:<port>` against the plugin's `tcp:localhost:<port>` permissions.
pub fn check_tcp(port: u16, granted: &[Permission]) -> Result<(), String> {
    if port != 0 && granted.iter().any(|p| matches!(p, Permission::TcpLocal(q) if *q == port)) {
        Ok(())
    } else {
        Err(deny(format!("port {port} is not declared with tcp:localhost:{port}")))
    }
}
