//! What the install steps need from the connection to the server. [`crate::ssh::SshServer`] is the real SSH login;
//! [`crate::recorder::Recorder`] records the commands instead of running them (dry-run mode and the parity tests).

use std::path::Path;

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio_util::sync::CancellationToken;

/// A failure the user should read: the message says what went wrong and, where possible, how to fix it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ServerError(pub String);

/// Returns early with a [`ServerError`], like the C# installer's `throw new ServerConnectionException(...)`.
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => { return Err($crate::server::ServerError(format!($($arg)*)).into()) };
}

pub type Result<T> = anyhow::Result<T>;

/// Lines for the step's log.
pub type Log<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A password login, or a key login when `private_keys` is set. The addons log in as root with the saved keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLogin {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub private_keys: Vec<String>,
    pub key_passphrase: String,
}

impl ServerLogin {
    pub fn password(host: &str, port: u16, user: &str, password: &str) -> Self {
        Self { host: host.into(), port, user: user.into(), password: password.into(), private_keys: Vec::new(), key_passphrase: String::new() }
    }

    pub fn key(host: &str, port: u16, user: &str, keys: Vec<String>, passphrase: &str) -> Self {
        Self { host: host.into(), port, user: user.into(), password: String::new(), private_keys: keys, key_passphrase: passphrase.into() }
    }

    pub fn uses_key(&self) -> bool {
        !self.private_keys.is_empty()
    }

    /// Parses "host", "host:port" or "[ipv6]:port". The port defaults to 22.
    pub fn parse_host(text: &str) -> (String, u16) {
        let text = text.trim();
        if text.starts_with('[') {
            if let Some(close) = text.find(']').filter(|&c| c > 0) {
                let host = &text[1..close];
                let rest = &text[close + 1..];
                let port = rest.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(22);
                return (host.to_string(), port);
            }
        }
        // A single colon is host:port; more colons is a bare IPv6 address.
        if let Some(colon) = text.rfind(':').filter(|&c| c > 0) {
            if text.find(':') == Some(colon) {
                if let Ok(port) = text[colon + 1..].parse() {
                    return (text[..colon].to_string(), port);
                }
            }
        }
        (text.to_string(), 22)
    }
}

/// From /etc/os-release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    pub id: String,
    pub version_id: String,
    pub pretty_name: String,
}

impl OsInfo {
    pub fn is_supported(&self) -> bool {
        self.id == "ubuntu" || self.id == "debian"
    }

    pub fn parse(text: &str) -> Self {
        let mut values = std::collections::HashMap::new();
        for line in text.split('\n') {
            if let Some(equals) = line.find('=').filter(|&e| e > 0) {
                values.insert(line[..equals].trim().to_string(), line[equals + 1..].trim().trim_matches('"').to_string());
            }
        }
        let id = values.get("ID").cloned().unwrap_or_else(|| "unknown".into()).to_lowercase();
        OsInfo {
            version_id: values.get("VERSION_ID").cloned().unwrap_or_default(),
            pretty_name: values.get("PRETTY_NAME").cloned().unwrap_or_else(|| id.clone()),
            id,
        }
    }
}

/// The stages of logging in, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectStage {
    Login,
    OperatingSystem,
    Root,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    pub exit_code: i32,
    pub output: String,
    pub error: String,
}

impl CommandResult {
    pub fn ok(output: &str) -> Self {
        Self { exit_code: 0, output: output.into(), error: String::new() }
    }

    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

/// How root commands run: directly, or through sudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    None,
    Root,
    SudoNoPassword,
    SudoPassword,
}

/// The command line that runs `command` as root, and what goes to stdin first (the sudo password, so `ps` never
/// shows it).
pub fn root_command_line(elevation: Elevation, password: &str, command: &str) -> Option<(String, Option<String>)> {
    let quoted = shell_quote(command);
    match elevation {
        Elevation::Root => Some((format!("bash -c {quoted}"), None)),
        Elevation::SudoNoPassword => Some((format!("sudo -n -H -- bash -c {quoted}"), None)),
        Elevation::SudoPassword => Some((format!("sudo -S -p '' -H -- bash -c {quoted}"), Some(format!("{password}\n")))),
        Elevation::None => None,
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Runs `command` in a process group of its own until the log is left, then stops the whole group, so a pipeline or
/// an if/else with tail in it stops too.
pub fn follow_script(command: &str) -> String {
    format!("setsid bash -c {} &\nfollower=$!\nwait $ALL_IN_1_WATCH\nkill -- -$follower 2>/dev/null\ntrue", shell_quote(command))
}

/// The first non-empty line of the texts.
pub fn first_line(texts: &[&str]) -> String {
    texts.iter().flat_map(|t| t.split('\n')).map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// An interactive shell on the server (the SSH terminal).
pub struct ShellChannel {
    /// Keys and pasted text for the shell.
    pub input: tokio::sync::mpsc::UnboundedSender<ShellInput>,
    /// What the shell prints; closed when the shell ends.
    pub output: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
}

pub enum ShellInput {
    Data(Vec<u8>),
    Resize { columns: u32, rows: u32 },
    Close,
}

/// Like `ssh -L`: a free port on 127.0.0.1 of this computer that leads to a host and port as the server sees it.
/// Dropping it closes the port.
pub struct PortForward {
    pub local_port: u16,
    pub stop: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for PortForward {
    fn drop(&mut self) {
        if let Some(task) = self.stop.take() {
            task.abort();
        }
    }
}

pub type Progress<'a> = &'a (dyn Fn(u64) + Send + Sync);

/// Receives the output lines of a command.
pub type OnLine<'a> = &'a mut (dyn for<'l> FnMut(&'l str) + Send);

/// The SSH session to the target server. Root commands go through sudo when the login user is not root.
#[async_trait]
pub trait Server: Send + Sync {
    fn login(&self) -> &ServerLogin;
    fn login_user(&self) -> String;
    fn host_key_fingerprint(&self) -> String;
    fn os(&self) -> Option<OsInfo>;
    fn is_connected(&self) -> bool;

    /// True when this is the dry-run recorder: nothing reaches a server.
    fn is_dry_run(&self) -> bool {
        false
    }

    /// Runs a command as the login user.
    async fn run(&self, command: &str, input: Option<&str>) -> Result<CommandResult>;

    async fn run_as_root(&self, command: &str) -> Result<CommandResult>;

    /// Streams stdout and stderr lines to `on_line` and returns the exit code. Pass secrets in `stdin`, never on the
    /// command line, where `ps` shows them.
    async fn run_as_root_streaming(&self, script: &str, on_line: OnLine<'_>, stdin: Option<&str>) -> Result<i32>;

    /// Runs a command that never ends by itself (tail -F, docker logs -f) as root and streams its lines until
    /// `cancel` fires or the connection drops. The script gets $ALL_IN_1_WATCH, a process that ends when the SSH
    /// channel closes: [`follow_script`] waits for it and then stops the command.
    async fn follow_as_root(&self, script: &str, on_line: OnLine<'_>, cancel: CancellationToken) -> Result<()>;

    /// Runs `script` as root with `input` as stdin, such as a tar archive for tar -x. Stdin closes at its end.
    async fn run_as_root_with_input(&self, script: &str, input: Box<dyn AsyncRead + Send + Unpin>) -> Result<CommandResult>;

    /// Copies a file from the server over SFTP. A file only root can read goes through a staging folder when the
    /// login user is not root. `progress` gets the bytes copied so far.
    async fn download(&self, remote_path: &str, local_path: &Path, progress: Progress<'_>) -> Result<()>;

    /// Downloads several files over one SFTP session and creates the local folders as needed. `progress` gets the
    /// bytes of all files copied so far.
    async fn download_files(&self, files: &[(String, std::path::PathBuf)], progress: Progress<'_>, cancel: CancellationToken) -> Result<()>;

    /// Reads a small file into memory, or returns None when it is larger than `max_bytes`.
    async fn read_file(&self, remote_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>>;

    /// Copies a file to the server over SFTP. Without a root login it goes to a staging folder first and root moves
    /// it into place (owned by root).
    async fn upload(&self, local_path: &Path, remote_path: &str, progress: Progress<'_>) -> Result<()>;

    /// An interactive xterm-256color shell of this size as the logged-in user.
    async fn open_shell(&self, columns: u32, rows: u32) -> Result<ShellChannel>;

    async fn forward_local_port(&self, host: &str, port: u16) -> Result<PortForward>;

    /// Logs in as root with this private key in a second session that trusts only this session's host key. None on
    /// success, else the reason. (RootAuthorizedKeys.TestKeyLoginAsync)
    async fn test_key_login(&self, private_key_pem: &str, passphrase: &str) -> Option<String>;

    async fn disconnect(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host() {
        assert_eq!(ServerLogin::parse_host(" 192.168.1.10 "), ("192.168.1.10".into(), 22));
        assert_eq!(ServerLogin::parse_host("hotel.example.com:2222"), ("hotel.example.com".into(), 2222));
        assert_eq!(ServerLogin::parse_host("[::1]:2222"), ("::1".into(), 2222));
        assert_eq!(ServerLogin::parse_host("[::1]"), ("::1".into(), 22));
        assert_eq!(ServerLogin::parse_host("fe80::1"), ("fe80::1".into(), 22));
        assert_eq!(ServerLogin::parse_host("host:abc"), ("host:abc".into(), 22));
    }

    #[test]
    fn root_command_lines() {
        assert_eq!(root_command_line(Elevation::Root, "", "id -un"), Some(("bash -c 'id -un'".into(), None)));
        assert_eq!(root_command_line(Elevation::SudoNoPassword, "", "echo 'a'").unwrap().0, "sudo -n -H -- bash -c 'echo '\\''a'\\'''");
        assert_eq!(
            root_command_line(Elevation::SudoPassword, "pw", "x"),
            Some(("sudo -S -p '' -H -- bash -c 'x'".into(), Some("pw\n".into())))
        );
        assert_eq!(root_command_line(Elevation::None, "", "x"), None);
    }

    #[test]
    fn os_release() {
        let os = OsInfo::parse("PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\n");
        assert_eq!(os, OsInfo { id: "ubuntu".into(), version_id: "24.04".into(), pretty_name: "Ubuntu 24.04.1 LTS".into() });
        assert!(os.is_supported());
        assert!(!OsInfo::parse("ID=fedora").is_supported());
    }

    #[test]
    fn follow_script_text() {
        assert_eq!(
            follow_script("tail -F /x"),
            "setsid bash -c 'tail -F /x' &\nfollower=$!\nwait $ALL_IN_1_WATCH\nkill -- -$follower 2>/dev/null\ntrue"
        );
    }
}
