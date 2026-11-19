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
/// Most URLs one `http-get-many` (API 1.1) takes.
pub const MAX_MANY_URLS: usize = 8;
/// Most bytes of the bodies one `http-get-many` hands over, together.
pub const MAX_MANY_BYTES: usize = 32 * 1024 * 1024;
/// The refusal of a body of `http-get-many` that does not fit in what is left of [`MAX_MANY_BYTES`].
pub const BUDGET_EXCEEDED: &str = "permission denied: response budget exceeded";

/// The bytes left for the bodies of one `http-get-many`, shared by its fetches. A fetch reserves each piece of a body
/// before keeping it ([`BodyBudget::reserve`]), so the bodies held at once stay within the budget plus the piece each
/// fetch is looking at; a fetch that fails gives its reservation back ([`BodyBudget::release`]).
#[derive(Debug)]
pub struct BodyBudget {
    left: std::sync::atomic::AtomicUsize,
}

impl BodyBudget {
    pub fn new(bytes: usize) -> Self {
        Self { left: std::sync::atomic::AtomicUsize::new(bytes) }
    }

    /// Takes `bytes` from the budget, or nothing and [`BUDGET_EXCEEDED`] when fewer are left.
    pub fn reserve(&self, bytes: usize) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        self.left
            .try_update(Ordering::AcqRel, Ordering::Acquire, |left| left.checked_sub(bytes))
            .map(|_| ())
            .map_err(|_| BUDGET_EXCEEDED.to_string())
    }

    /// Gives back `bytes` reserved before (a failed fetch, a body shorter than announced).
    pub fn release(&self, bytes: usize) {
        self.left.fetch_add(bytes, std::sync::atomic::Ordering::AcqRel);
    }

    /// The bytes left.
    pub fn left(&self) -> usize {
        self.left.load(std::sync::atomic::Ordering::Acquire)
    }
}
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
/// Most hosts refused in one session that are remembered; once full, other hosts are refused without asking.
pub const MAX_REFUSED_HOSTS: usize = 256;
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

/// Names that are never public: asked for under `http:ask`, they would reach this computer or the local network.
const LOCAL_SUFFIXES: &[&str] = &[".localhost", ".local", ".lan", ".internal", ".home.arpa"];

/// `host` lower case without a trailing dot: how `http:ask` stores and compares hosts.
pub fn normalise_host(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// A host `http:ask` may ask the user about: a valid `http:<host>` name that is public, so a plugin cannot reach this
/// computer or the local network by asking. The URL parser the fetch uses (WHATWG, the `url` crate) decides what is
/// an address: it must read the host as a domain, not as IPv4 (which includes hex and short forms such as
/// `0x7f.0x1` or `127.1`) or IPv6. Also refused: `localhost` and `*.localhost`, single-label names, and names ending
/// in `.local`, `.lan`, `.internal` or `.home.arpa` (a trailing dot ignored, any case). Declared `http:<host>`
/// permissions keep their own rules. The addresses a name resolves to are checked when connecting (by the app; see
/// [`is_local_address`]).
pub fn askable_host(host: &str) -> bool {
    if !valid_host(host) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(&format!("http://{host}/")) else { return false };
    let Some(url::Host::Domain(domain)) = parsed.host() else { return false };
    let name = normalise_host(domain);
    let Some((_, last)) = name.rsplit_once('.') else { return false };
    let local = name == "localhost" || LOCAL_SUFFIXES.iter().any(|s| name.ends_with(s));
    !last.is_empty() && !local
}

/// An address an asked host must not lead to: loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7), link-local
/// (169.254/16, fe80::/10), site-local (fec0::/10), unspecified, CGNAT (100.64/10), "this network" (0/8), IETF protocol
/// assignments (192.0.0/24), benchmarking (198.18/15), reserved (240/4, broadcast included) and multicast; local-use
/// NAT64 (64:ff9b:1::/48) as a whole; and the IPv4-mapped, IPv4-translated (::ffff:0:0/96), IPv4-compatible, NAT64
/// (64:ff9b::/96) and 6to4 (2002::/16) forms of any of these IPv4 ones.
pub fn is_local_address(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    fn v4(ip: Ipv4Addr) -> bool {
        let [a, b, ..] = ip.octets();
        ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_unspecified()
            || ip.is_broadcast()
            || ip.is_multicast()
            || a == 0
            || (a == 100 && (64..128).contains(&b))
            || (a == 192 && b == 0 && ip.octets()[2] == 0)
            || (a == 198 && (b == 18 || b == 19))
            || a >= 240
    }
    match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => {
            let s = ip.segments();
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                return true;
            }
            // Unique local, link-local, site-local (deprecated, still routed locally), local-use NAT64.
            if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 || s[..3] == [0x64, 0xff9b, 1] {
                return true;
            }
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return v4(mapped);
            }
            let ipv4 = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
            // IPv4-compatible (::a.b.c.d), IPv4-translated (::ffff:0:a.b.c.d) and NAT64 (64:ff9b::a.b.c.d) carry an
            // IPv4 address in the low 32 bits.
            if s[..6] == [0; 6] || s[..6] == [0, 0, 0, 0, 0xffff, 0] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return v4(ipv4(s[6], s[7]));
            }
            // 6to4 (2002:a.b.c.d::/48) carries it right after the prefix.
            if s[0] == 0x2002 {
                return v4(ipv4(s[1], s[2]));
            }
            false
        }
    }
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
/// Hosts are kept lower case and must pass [`askable_host`]. One per plugin and session, shared by its instances.
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
            let h = normalise_host(&h);
            if askable_host(&h) && !hosts.contains(&h) && hosts.len() < MAX_ASKED_HOSTS {
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
        let host = normalise_host(host);
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
        let host = normalise_host(host);
        if !askable_host(&host) {
            return Err(format!("'{}' is not a public host name", sanitise(&host)));
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

    /// Remembers `host` as refused, for this session (not past [`MAX_REFUSED_HOSTS`]: see
    /// [`AskedHosts::refused_full`]).
    pub fn refuse(&self, host: &str) {
        let host = normalise_host(host);
        let mut asked = self.lock();
        if !asked.refused.contains(&host) && asked.refused.len() < MAX_REFUSED_HOSTS {
            asked.refused.push(host);
        }
    }

    /// [`MAX_REFUSED_HOSTS`] hosts were refused this session: any other host is refused without asking.
    pub fn refused_full(&self) -> bool {
        self.lock().refused.len() >= MAX_REFUSED_HOSTS
    }

    /// Remembers `hosts` as the allowed ones (an update keeps the user's answers; invalid or surplus ones are dropped).
    pub fn restore(&self, hosts: &[String]) -> Result<(), String> {
        let mut kept: Vec<String> = Vec::new();
        for h in hosts.iter().map(|h| normalise_host(h)) {
            if askable_host(&h) && !kept.contains(&h) && kept.len() < MAX_ASKED_HOSTS {
                kept.push(h);
            }
        }
        let mut asked = self.lock();
        self.write(&mut asked, kept)
    }

    /// Forgets the answer for `host`: the plugin asks again next time.
    pub fn forget(&self, host: &str) -> Result<(), String> {
        let host = normalise_host(host);
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
