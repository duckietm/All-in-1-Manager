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
    TcpLocal(u16),
    Files,
}

fn ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn host(s: &str) -> bool {
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
                } else if let Some(h) = s.strip_prefix("http:") {
                    if !host(h) {
                        bail!("Invalid host in permission: {s}");
                    }
                    Permission::Http(h.into())
                } else if let Some(port) = s.strip_prefix("tcp:localhost:") {
                    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                        bail!("Invalid port in permission: {s}");
                    }
                    match port.parse::<u16>() {
                        Ok(p) if p > 0 => Permission::TcpLocal(p),
                        _ => bail!("Invalid port in permission: {s}"),
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
            Permission::TcpLocal(port) => words("Connect to a program on this computer, port {port} (for example G-Earth)", vec![("port", port.to_string())]),
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
            Permission::TcpLocal(p) => write!(f, "tcp:localhost:{p}"),
            Permission::Files => write!(f, "files"),
        }
    }
}
