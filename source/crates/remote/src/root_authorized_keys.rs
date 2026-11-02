//! Root's authorized_keys: read, install a key (replace or add), sshd's PermitRootLogin, and a test login with a key.
//! (RootAuthorizedKeys.cs)

use aio_core::keys::{self, AuthorizedKey};

use crate::fail;
use crate::server::{shell_quote, Result, Server};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizedKeysMode {
    Replace,
    Add,
}

pub const FILE_PATH: &str = "/root/.ssh/authorized_keys";

/// Empty when the file does not exist.
pub async fn read(server: &dyn Server) -> Result<Vec<AuthorizedKey>> {
    let result = server.run_as_root(&format!("if [ -f {FILE_PATH} ]; then cat {FILE_PATH}; fi")).await?;
    if !result.succeeded() {
        fail!("Could not read {FILE_PATH}: {}", result.error.trim());
    }
    Ok(keys::parse_authorized_keys(&result.output))
}

/// The script that writes the key; prints the backup path (or an empty line).
fn install_script(key: &AuthorizedKey, mode: AuthorizedKeysMode) -> String {
    let line = shell_quote(&key.line());
    let write = match mode {
        AuthorizedKeysMode::Replace => format!("printf '%s\\n' {line} > \"$f\""),
        // Append only if absent, adding a newline first if the file lacks a trailing one.
        AuthorizedKeysMode::Add => format!(
            "if ! grep -qF -- {} \"$f\" 2>/dev/null; then if [ -s \"$f\" ] && [ -n \"$(tail -c1 \"$f\")\" ]; then echo >> \"$f\"; fi; printf '%s\\n' {line} >> \"$f\"; fi",
            shell_quote(&key.base64)
        ),
    };
    let lines = [
        "set -e".to_string(),
        "umask 077".to_string(),
        format!("f={FILE_PATH}"),
        "install -d -m 700 -o root -g root /root/.ssh".to_string(),
        "backup=".to_string(),
        "if [ -s \"$f\" ]; then backup=$(mktemp \"$f.bak-$(date +%Y%m%d-%H%M%S)-XXXX\"); cp -p \"$f\" \"$backup\"; fi".to_string(),
        write,
        "chmod 600 \"$f\"".to_string(),
        "chown root:root \"$f\"".to_string(),
        // In case SELinux is enabled.
        "if command -v restorecon >/dev/null 2>&1; then restorecon -R /root/.ssh || true; fi".to_string(),
        "echo \"$backup\"".to_string(),
    ];
    lines.join("\n") + "\n"
}

/// Backs up the current file first so a replace can be undone. Returns the backup path, if any.
pub async fn install(server: &dyn Server, key: &AuthorizedKey, mode: AuthorizedKeysMode) -> Result<Option<String>> {
    let result = server.run_as_root(&install_script(key, mode)).await?;
    if !result.succeeded() {
        fail!("Could not write {FILE_PATH}: {}", first_line(&result.error));
    }
    let backup = result.output.trim();
    Ok(if backup.is_empty() { None } else { Some(backup.to_string()) })
}

/// The effective sshd PermitRootLogin value, or None if unknown.
pub async fn permit_root_login(server: &dyn Server) -> Result<Option<String>> {
    let result = server.run_as_root("sshd -T 2>/dev/null | awk '$1==\"permitrootlogin\"{print $2}'").await?;
    let value = result.output.trim();
    Ok(if value.is_empty() { None } else { Some(value.to_string()) })
}

/// Logs in as root with the key in a second session that trusts only this session's host key. None on success,
/// else the reason ("the server refused the key", "logged in as 'x' instead of root", or the connection error).
pub async fn test_key_login(server: &dyn Server, private_key_pem: &str, passphrase: &str) -> Option<String> {
    server.test_key_login(private_key_pem, passphrase).await
}

fn first_line(text: &str) -> String {
    text.split('\n').map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_or_unknown() {
        assert_eq!(first_line("\n  \n mkdir: denied \nmore"), "mkdir: denied");
        assert_eq!(first_line(" \n"), "unknown error");
    }

    #[test]
    fn install_script_ends_with_a_newline() {
        let key = AuthorizedKey::new("ssh-ed25519", "AAAA", "it's me");
        let script = install_script(&key, AuthorizedKeysMode::Replace);
        assert!(script.starts_with("set -e\numask 077\n") && script.ends_with("echo \"$backup\"\n"));
        assert!(script.contains("printf '%s\\n' 'ssh-ed25519 AAAA it'\\''s me' > \"$f\""));
    }
}
