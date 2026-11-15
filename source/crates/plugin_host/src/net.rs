//! Permission checks of `http-get` and `tcp-connect`, and the hosts the user allowed under `http:ask`.
//!
//! `check_http` accepts only a deliberately narrow URL grammar, so that no URL parser downstream can
//! read a different host out of it than this check did:
//!
//! `http[s]://<host>[:<port>][/<path>][?<query>][#<fragment>]`, printable ASCII only (no spaces, no
//! `\`), `<host>` made of `A-Z a-z 0-9 . -` (no userinfo, no `%`, no IPv6 brackets), `<port>` the
//! scheme's default port only (the permission grammar `http:<host>` names no port). The host must equal
//! a declared `http:<host>` exactly, ignoring case (no subdomains), or, with `http:ask` (API 1.1), be one the
//! user allowed ([`AskedHosts`]; the runtime asks for it). The URL handed on is rebuilt from the checked parts:
//! scheme and host lower case, the default port and the fragment dropped.
//!
//! Not covered here (the app's HTTP client, Task 8): redirects must not leave the checked host, a non-2xx
//! status is returned as `Err("HTTP <code>")`, and the body read stops past [`MAX_HTTP_BODY`].

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::permissions::{valid_host, Permission};
use crate::runtime::sanitise;

/// Largest HTTP response body handed to a plugin.
pub const MAX_HTTP_BODY: usize = 32 * 1024 * 1024;
/// Longest URL accepted.
pub const MAX_URL: usize = 2_048;
/// Local TCP connections one plugin instance may hold open at once.
pub const MAX_TCP_CONNECTIONS: usize = 4;
/// Most bytes one `recv` returns.
pub const MAX_TCP_RECV: usize = 1024 * 1024;
/// Longest wait of one `recv-timeout` (milliseconds); a larger value is clamped to it.
pub const MAX_RECV_TIMEOUT_MS: u32 = 30_000;
/// Most hosts `http:ask` remembers for one plugin.
pub const MAX_ASKED_HOSTS: usize = 64;
/// Most hosts refused in one session that are remembered (the oldest is forgotten first, and asked for again).
const MAX_REFUSED_HOSTS: usize = 256;
/// Largest remembered-hosts file read (a bigger one counts as empty).
const MAX_HOSTS_FILE: u64 = 64 * 1024;

pub(crate) fn deny(reason: impl std::fmt::Display) -> String {
    format!("permission denied: {reason}")
}

fn shown(url: &str) -> String {
    sanitise(&url.chars().take(200).collect::<String>())
}

/// A URL that passed the grammar of [`check_http`], before its host is compared with the permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedUrl {
    /// Rebuilt from the checked parts (scheme and host lower case, no default port, no fragment).
    pub url: String,
    /// Lower case.
    pub host: String,
}

/// The URL grammar of [`check_http`] alone: the host is not compared with any permission.
pub fn parse_http(url: &str) -> Result<CheckedUrl, String> {
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
    let tail = tail.split('#').next().unwrap_or("");
    Ok(CheckedUrl { url: format!("{scheme}://{host}{tail}"), host })
}

/// `host` is declared with `http:<host>`, ignoring case.
pub fn declared_host(host: &str, granted: &[Permission]) -> bool {
    granted.iter().any(|p| matches!(p, Permission::Http(h) if h.eq_ignore_ascii_case(host)))
}

/// The refusal of a host that is not declared (and not allowed under `http:ask`).
pub fn undeclared(host: &str) -> String {
    deny(format!("host '{host}' is not declared with http:{host}"))
}

/// Checks `url` against the plugin's `http:<host>` permissions; returns the URL to fetch. (`http:ask` is the
/// runtime's: it asks the user about a host this refuses as not declared.)
pub fn check_http(url: &str, granted: &[Permission]) -> Result<String, String> {
    let checked = parse_http(url)?;
    if !declared_host(&checked.host, granted) {
        return Err(undeclared(&checked.host));
    }
    Ok(checked.url)
}

/// Checks a `tcp-connect` to `127.0.0.1:<port>` against the plugin's `tcp:localhost:<port>` and
/// `tcp:localhost:<a>-<b>` permissions.
pub fn check_tcp(port: u16, granted: &[Permission]) -> Result<(), String> {
    let covers = |p: &Permission| match p {
        Permission::TcpLocal(q) => *q == port,
        Permission::TcpLocalRange(a, b) => (*a..=*b).contains(&port),
        _ => false,
    };
    if port != 0 && granted.iter().any(covers) {
        Ok(())
    } else {
        Err(deny(format!("port {port} is not declared with tcp:localhost:{port}")))
    }
}

/// What the user answered for one plugin under `http:ask`: the hosts allowed (at most [`MAX_ASKED_HOSTS`],
/// remembered in a file of the Manager's, or in memory only) and the hosts refused (in memory: for this session).
/// Hosts are kept lower case and must pass [`valid_host`]. One per plugin and session, shared by its instances.
#[derive(Debug)]
pub struct AskedHosts {
    /// The remembered-hosts file; `None` remembers in memory only (a developer plugin).
    file: Option<PathBuf>,
    inner: Mutex<Asked>,
}

#[derive(Debug, Default)]
struct Asked {
    /// The allowed hosts when there is no file.
    allowed: Vec<String>,
    refused: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct HostsFile {
    hosts: Vec<String>,
}

impl AskedHosts {
    /// Remembers in memory only.
    pub fn in_memory() -> Self {
        Self { file: None, inner: Mutex::default() }
    }

    /// Remembers the allowed hosts in `file`, written atomically and read again on every use (so deleting the file
    /// forgets them). A missing, oversized or invalid file holds no host.
    pub fn persisted(file: PathBuf) -> Self {
        Self { file: Some(file), inner: Mutex::default() }
    }

    fn lock(&self) -> MutexGuard<'_, Asked> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read(&self, asked: &Asked) -> Vec<String> {
        let Some(file) = &self.file else { return asked.allowed.clone() };
        if std::fs::metadata(file).map_or(true, |m| m.len() > MAX_HOSTS_FILE) {
            return Vec::new();
        }
        let Some(list) = std::fs::read(file).ok().and_then(|b| serde_json::from_slice::<HostsFile>(&b).ok()) else {
            return Vec::new();
        };
        let mut hosts: Vec<String> = Vec::new();
        for h in list.hosts {
            let h = h.to_ascii_lowercase();
            if valid_host(&h) && !hosts.contains(&h) && hosts.len() < MAX_ASKED_HOSTS {
                hosts.push(h);
            }
        }
        hosts
    }

    fn write(&self, asked: &mut Asked, hosts: Vec<String>) -> Result<(), String> {
        let Some(file) = &self.file else {
            asked.allowed = hosts;
            return Ok(());
        };
        let failed = |e: std::io::Error| format!("the list of allowed hosts could not be saved ({:?})", e.kind());
        if hosts.is_empty() {
            return match std::fs::remove_file(file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(e)),
                _ => Ok(()),
            };
        }
        let json = serde_json::to_vec_pretty(&HostsFile { hosts }).map_err(|e| e.to_string())?;
        let mut tmp = file.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, json).map_err(failed)?;
        std::fs::rename(&tmp, file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            failed(e)
        })
    }

    /// The allowed hosts, oldest first.
    pub fn allowed(&self) -> Vec<String> {
        let asked = self.lock();
        self.read(&asked)
    }

    /// `Some(true)` for an allowed host, `Some(false)` for one refused this session, `None` if not answered yet.
    pub fn answer(&self, host: &str) -> Option<bool> {
        let host = host.to_ascii_lowercase();
        let asked = self.lock();
        if self.read(&asked).contains(&host) {
            Some(true)
        } else if asked.refused.contains(&host) {
            Some(false)
        } else {
            None
        }
    }

    /// No other host can be allowed ([`MAX_ASKED_HOSTS`]).
    pub fn is_full(&self) -> bool {
        self.allowed().len() >= MAX_ASKED_HOSTS
    }

    /// Remembers `host` as allowed (and no longer refused).
    pub fn allow(&self, host: &str) -> Result<(), String> {
        let host = host.to_ascii_lowercase();
        if !valid_host(&host) {
            return Err(format!("'{}' is not a valid host", sanitise(&host)));
        }
        let mut asked = self.lock();
        asked.refused.retain(|h| *h != host);
        let mut hosts = self.read(&asked);
        if hosts.contains(&host) {
            return Ok(());
        }
        if hosts.len() >= MAX_ASKED_HOSTS {
            return Err(format!("at most {MAX_ASKED_HOSTS} hosts can be remembered"));
        }
        hosts.push(host);
        self.write(&mut asked, hosts)
    }

    /// Remembers `host` as refused, for this session.
    pub fn refuse(&self, host: &str) {
        let host = host.to_ascii_lowercase();
        let mut asked = self.lock();
        if !asked.refused.contains(&host) {
            if asked.refused.len() >= MAX_REFUSED_HOSTS {
                asked.refused.remove(0);
            }
            asked.refused.push(host);
        }
    }

    /// Forgets the answer for `host`: the plugin asks again next time.
    pub fn forget(&self, host: &str) -> Result<(), String> {
        let host = host.to_ascii_lowercase();
        let mut asked = self.lock();
        asked.refused.retain(|h| *h != host);
        let mut hosts = self.read(&asked);
        if !hosts.contains(&host) {
            return Ok(());
        }
        hosts.retain(|h| *h != host);
        self.write(&mut asked, hosts)
    }

    /// Forgets every answer (a reinstall, a revocation, a removal).
    pub fn clear(&self) -> Result<(), String> {
        let mut asked = self.lock();
        asked.refused.clear();
        self.write(&mut asked, Vec::new())
    }
}
