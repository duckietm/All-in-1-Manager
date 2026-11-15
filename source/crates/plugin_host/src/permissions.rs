//! Permission strings of a manifest and their grammar.

use anyhow::{bail, Result};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    Ssh,
    SshRoot,
    SqlRead(Vec<String>),
    SqlWrite(Vec<String>),
    /// `(table, column)`
    SqlReadPrivate(String, String),
    Http(String),
    /// `http:ask` (API 1.1): a request to another host asks the user first.
    HttpAsk,
    TcpLocal(u16),
    /// `tcp:localhost:<a>-<b>` (API 1.1): `a < b`, at most [`MAX_PORT_RANGE`] ports.
    TcpLocalRange(u16, u16),
    Files,
}

/// Most ports one `tcp:localhost:<a>-<b>` range may span.
pub const MAX_PORT_RANGE: u16 = 16;

impl Permission {
    /// Needs `"api": "1.1"` in the manifest (an older Manager does not know it).
    pub fn needs_api_1_1(&self) -> bool {
        matches!(self, Permission::HttpAsk | Permission::TcpLocalRange(..))
    }
}

/// A port number of a `tcp:localhost:` permission: plain digits, 1 to 65535.
fn port(s: &str) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u16>().ok().filter(|p| *p > 0)
}

fn ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A host name as `http:<host>` accepts it (the same rule holds for a host asked for under `http:ask`).
pub fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with(['.', '-'])
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn tables(list: &str) -> Result<Vec<String>> {
    let tables: Vec<String> = list.split(',').map(str::to_string).collect();
    if tables.iter().any(|t| !ident(t)) {
        bail!("Invalid table list in permission: {list}");
    }
    Ok(tables)
}

impl Permission {
    pub fn parse(s: &str) -> Result<Permission> {
        Ok(match s {
            "ssh" => Permission::Ssh,
            "ssh:root" => Permission::SshRoot,
            "files" => Permission::Files,
            _ => {
                if let Some(rest) = s.strip_prefix("sql:read-private:") {
                    let Some((table, column)) = rest.split_once('.') else { bail!("Invalid permission: {s}") };
                    if !ident(table) || !ident(column) {
                        bail!("Invalid permission: {s}");
                    }
                    Permission::SqlReadPrivate(table.into(), column.into())
                } else if let Some(list) = s.strip_prefix("sql:read:") {
                    Permission::SqlRead(tables(list)?)
                } else if let Some(list) = s.strip_prefix("sql:write:") {
                    Permission::SqlWrite(tables(list)?)
                } else if s == "http:ask" {
                    Permission::HttpAsk
                } else if let Some(h) = s.strip_prefix("http:") {
                    if !valid_host(h) {
                        bail!("Invalid host in permission: {s}");
                    }
                    Permission::Http(h.into())
                } else if let Some(ports) = s.strip_prefix("tcp:localhost:") {
                    match ports.split_once('-') {
                        None => match port(ports) {
                            Some(p) => Permission::TcpLocal(p),
                            None => bail!("Invalid port in permission: {s}"),
                        },
                        Some((a, b)) => match (port(a), port(b)) {
                            (Some(a), Some(b)) if a < b && b - a < MAX_PORT_RANGE => Permission::TcpLocalRange(a, b),
                            _ => bail!("Invalid port range in permission (the smaller port first, at most {MAX_PORT_RANGE} ports): {s}"),
                        },
                    }
                } else {
                    bail!("Unknown permission: {s}");
                }
            }
        })
    }
}

/// A permission in plain words: an English sentence with `{name}` placeholders, and the values that fill them.
/// The Manager translates `english` (it is the translation key) before filling it; the `aio-plugin` tool prints it
/// in English with [`PlainWords::english_text`]. One wording for both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainWords {
    pub english: &'static str,
    pub values: Vec<(&'static str, String)>,
}

impl PlainWords {
    /// `template` (the English sentence or its translation) with the placeholders filled in.
    pub fn fill(&self, template: &str) -> String {
        let mut text = template.to_string();
        for (name, value) in &self.values {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    /// The English sentence, filled in.
    pub fn english_text(&self) -> String {
        self.fill(self.english)
    }
}

impl Permission {
    /// One plain sentence for the permission. Every kind is matched, so a new permission cannot be forgotten.
    pub fn plain_words(&self) -> PlainWords {
        let words = |english, values| PlainWords { english, values };
        let tables_words = |tables: &Vec<String>, one, many| words(if tables.len() == 1 { one } else { many }, vec![("tables", tables.join(", "))]);
        match self {
            Permission::Ssh => words("Run commands on the hotel's server (you approve each script)", vec![]),
            Permission::SshRoot => words("Run commands as root on the hotel's server (you approve each script)", vec![]),
            Permission::SqlRead(tables) => tables_words(tables, "Read the table {tables} of the hotel database", "Read the tables {tables} of the hotel database"),
            Permission::SqlWrite(tables) => tables_words(
                tables,
                "Change the table {tables} of the hotel database (you approve each change)",
                "Change the tables {tables} of the hotel database (you approve each change)",
            ),
            Permission::SqlReadPrivate(table, column) => words(
                "Read the private column {column} of the table {table} (it can hold passwords, addresses or tokens)",
                vec![("table", table.clone()), ("column", column.clone())],
            ),
            Permission::Http(host) => words("Fetch pages from {host} on the internet", vec![("host", host.clone())]),
            Permission::HttpAsk => words("Ask you before fetching from any other internet host", vec![]),
            Permission::TcpLocal(port) => words("Connect to a program on this computer, port {port} (for example G-Earth)", vec![("port", port.to_string())]),
            Permission::TcpLocalRange(a, b) => words(
                "Connect to programs on this computer, ports {a}\u{2013}{b} (for example G-Earth)",
                vec![("a", a.to_string()), ("b", b.to_string())],
            ),
            Permission::Files => words("Keep its own files in a private folder (always allowed)", vec![]),
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Permission::Ssh => write!(f, "ssh"),
            Permission::SshRoot => write!(f, "ssh:root"),
            Permission::SqlRead(t) => write!(f, "sql:read:{}", t.join(",")),
            Permission::SqlWrite(t) => write!(f, "sql:write:{}", t.join(",")),
            Permission::SqlReadPrivate(t, c) => write!(f, "sql:read-private:{t}.{c}"),
            Permission::Http(h) => write!(f, "http:{h}"),
            Permission::HttpAsk => write!(f, "http:ask"),
            Permission::TcpLocal(p) => write!(f, "tcp:localhost:{p}"),
            Permission::TcpLocalRange(a, b) => write!(f, "tcp:localhost:{a}-{b}"),
            Permission::Files => write!(f, "files"),
        }
    }
}
