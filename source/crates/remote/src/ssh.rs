//! The real SSH connection (russh): the login (password, keyboard-interactive / PAM, or an SSH key), the check of the
//! distribution, root through sudo, commands with live output, SFTP, port forwarding and the interactive shell.
//! (ServerConnection.cs)

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::known_hosts;
use crate::server::{
    first_line, root_command_line, shell_quote, CommandResult, ConnectStage, Elevation, Log, OnLine, OsInfo, PortForward, Progress,
    Result, Server, ServerError, ServerLogin, ShellChannel, ShellInput,
};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Accepts the server's host key and remembers its fingerprint; a second session trusts only that fingerprint.
struct Client {
    trusted: Option<String>,
    seen: Arc<std::sync::Mutex<String>>,
    connected: Arc<AtomicBool>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let key = key.public_key();
        let fingerprint = format!("{} {}", key.algorithm().as_str(), key.fingerprint(HashAlg::Sha256));
        *self.seen.lock().unwrap() = fingerprint.clone();
        Ok(self.trusted.as_ref().is_none_or(|t| *t == fingerprint))
    }

    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> std::result::Result<(), Self::Error> {
        self.connected.store(false, Ordering::SeqCst);
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(e) => Err(e),
        }
    }
}

/// The SSH session to the target server. Root commands go through sudo when the login user is not root.
pub struct SshServer {
    login: ServerLogin,
    handle: Arc<Handle<Client>>,
    connected: Arc<AtomicBool>,
    host_key: String,
    login_user: String,
    os: Option<OsInfo>,
    elevation: Elevation,
}

fn ssh_error(error: impl std::fmt::Display) -> anyhow::Error {
    ServerError(error.to_string()).into()
}

/// Reads a private key file the way the login does: OpenSSH, PEM (PKCS#1, PKCS#8) or PuTTY .ppk.
pub fn read_private_key(text: &str, passphrase: &str) -> std::result::Result<PrivateKey, KeyReadError> {
    let password = (!passphrase.is_empty()).then_some(passphrase);
    match russh::keys::decode_secret_key(text.trim(), password) {
        Ok(key) => Ok(key),
        Err(error) => {
            // ssh-key reads PPK v3; v2 with a password (what PuTTYgen saved for years, and the SSH key step) comes here.
            if text.trim_start().starts_with("PuTTY-User-Key-File-") {
                return match aio_core::keys::ppk_to_openssh(text, passphrase) {
                    Ok(pem) => russh::keys::decode_secret_key(&pem, None).map_err(|e| KeyReadError::Format(e.to_string())),
                    Err(aio_core::keys::PpkError::NeedsPassword) => Err(KeyReadError::NeedsPassword),
                    Err(aio_core::keys::PpkError::WrongPassword) => Err(KeyReadError::WrongPassword),
                    Err(aio_core::keys::PpkError::Invalid(why)) => Err(KeyReadError::Format(why)),
                };
            }
            Err(match error {
                russh::keys::Error::KeyIsEncrypted => KeyReadError::NeedsPassword,
                _ if password.is_some() => KeyReadError::WrongPassword,
                other => KeyReadError::Format(other.to_string()),
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyReadError {
    #[error("This key has a password: enter it.")]
    NeedsPassword,
    #[error("The key could not be opened: wrong key password?")]
    WrongPassword,
    #[error("This is not a private key the installer can use: {0}")]
    Format(String),
}

/// The public key of a private key file (OpenSSH, PEM or .ppk), as an authorized_keys entry. (SshKeys.PublicKeyOf)
pub fn public_key_of(private_key_text: &str, passphrase: &str, comment: &str) -> std::result::Result<aio_core::keys::AuthorizedKey, KeyReadError> {
    let key = read_private_key(&format!("{}\n", private_key_text.trim()), passphrase)?;
    let public = key.public_key();
    use russh::keys::PublicKeyBase64;
    Ok(aio_core::keys::AuthorizedKey::new(
        public.algorithm().as_str(),
        &public.public_key_base64(),
        &aio_core::keys::clean_comment(comment),
    ))
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

async fn open_session(host: &str, port: u16, trusted: Option<String>) -> Result<(Handle<Client>, Arc<AtomicBool>, String)> {
    let config = Arc::new(client::Config {
        // Installs run for minutes, and keep-alives stop routers and firewalls from dropping an idle-looking session.
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        inactivity_timeout: None,
        ..Default::default()
    });
    let connected = Arc::new(AtomicBool::new(true));
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let handler = Client { trusted, seen: seen.clone(), connected: connected.clone() };
    let address = (host.to_string(), port);
    let handle = tokio::time::timeout(TIMEOUT, client::connect(config, address, handler))
        .await
        .map_err(|_| ServerError(format!("Could not reach {host}:{port}: the connection timed out after 20 seconds")))?
        .map_err(|e| ServerError(format!("Could not reach {host}:{port}: {e}")))?;
    let fingerprint = seen.lock().unwrap().clone();
    Ok((handle, connected, fingerprint))
}

async fn authenticate_with_key(handle: &mut Handle<Client>, user: &str, key: PrivateKey) -> Result<bool> {
    let hash = if key.algorithm().is_rsa() { handle.best_supported_rsa_hash().await.ok().flatten().flatten() } else { None };
    let result = handle.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await.map_err(ssh_error)?;
    Ok(matches!(result, AuthResult::Success))
}

async fn authenticate_with_password(handle: &mut Handle<Client>, user: &str, password: &str) -> Result<bool> {
    if let Ok(AuthResult::Success) = handle.authenticate_password(user, password).await {
        return Ok(true);
    }
    // Ubuntu and Debian often only offer keyboard-interactive (PAM) for password logins.
    let mut response = handle.authenticate_keyboard_interactive_start(user, None).await.map_err(ssh_error)?;
    for _ in 0..5 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|_| password.to_string()).collect();
                response = handle.authenticate_keyboard_interactive_respond(answers).await.map_err(ssh_error)?;
            }
        }
    }
    Ok(false)
}

/// Runs a command on a session and collects its output. (SshCommand)
async fn exec(handle: &Handle<Client>, command: &str, input: Option<&[u8]>) -> Result<CommandResult> {
    // A Windows line ending would end up in the command ("set -e\r").
    let command = command.replace("\r\n", "\n");
    let mut channel = handle.channel_open_session().await.map_err(ssh_error)?;
    channel.exec(true, command.as_bytes()).await.map_err(ssh_error)?;
    if let Some(input) = input {
        channel.data(input).await.map_err(ssh_error)?;
    }
    channel.eof().await.map_err(ssh_error)?;
    let (mut output, mut error, mut code) = (Vec::new(), Vec::new(), None);
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => output.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => error.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
            _ => {}
        }
    }
    Ok(CommandResult {
        exit_code: code.unwrap_or(-1),
        output: String::from_utf8_lossy(&output).into_owned(),
        error: String::from_utf8_lossy(&error).into_owned(),
    })
}

/// Splits streamed bytes into lines (UTF-8, without the line ending), like StreamReader.ReadLine.
struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    fn push(&mut self, data: &[u8], on_line: &mut (dyn FnMut(&str) + Send)) {
        self.pending.extend_from_slice(data);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            on_line(text.trim_end_matches('\r'));
        }
    }

    fn finish(&mut self, on_line: &mut (dyn FnMut(&str) + Send)) {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            on_line(text.trim_end_matches('\r'));
        }
    }
}

/// Logs in, checks the distribution and verifies root access.
pub async fn connect(login: ServerLogin, log: Log<'_>, stage: &(dyn Fn(ConnectStage) + Send + Sync)) -> Result<Arc<SshServer>> {
    stage(ConnectStage::Login);
    let keys: Vec<String> = login.private_keys.iter().map(|p| file_name(p)).collect();
    log(&format!(
        "> Connecting to {}:{} as {}{}",
        login.host,
        login.port,
        login.user,
        if login.uses_key() { format!(" with the SSH key {} ...", keys.join(", ")) } else { " ...".into() }
    ));

    // The keys are opened before connecting, so a wrong key password is reported as such.
    let mut private_keys = Vec::new();
    if login.uses_key() {
        let mut problem = None;
        for path in &login.private_keys {
            let name = file_name(path);
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) => {
                    problem = Some(format!("The SSH key {name} could not be opened: {e}"));
                    continue;
                }
            };
            match read_private_key(&text, &login.key_passphrase) {
                Ok(key) => private_keys.push(key),
                Err(KeyReadError::NeedsPassword) => problem = Some(format!("The SSH key {name} has a password: enter it as the key password.")),
                Err(KeyReadError::WrongPassword) => problem = Some(format!("The SSH key {name} could not be opened: wrong key password?")),
                Err(KeyReadError::Format(why)) => {
                    problem = Some(if login.key_passphrase.is_empty() {
                        format!("The SSH key {name} could not be opened: {why}")
                    } else {
                        format!("The SSH key {name} could not be opened: wrong key password?")
                    })
                }
            }
        }
        if private_keys.is_empty() {
            return Err(ServerError(problem.unwrap_or_else(|| "No SSH key to log in with.".into())).into());
        }
    }

    // The key this server showed last time, so a different server on the same address is noticed.
    let remembered = known_hosts::trusted(&login.host, login.port);
    let (mut handle, connected, host_key) = open_session(&login.host, login.port, None).await?;
    if let Some(expected) = &remembered {
        if *expected != host_key {
            // Nothing has been sent yet: the login details go out only after this point.
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            return Err(ServerError(known_hosts::changed_message(&login.host, login.port, expected, &host_key)).into());
        }
    }
    let authenticated = if login.uses_key() {
        let mut ok = false;
        for key in private_keys {
            if authenticate_with_key(&mut handle, &login.user, key).await? {
                ok = true;
                break;
            }
        }
        ok
    } else {
        authenticate_with_password(&mut handle, &login.user, &login.password).await?
    };
    if !authenticated {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        return Err(ServerError(if login.uses_key() {
            format!(
                "The server refused the SSH key for '{}'. Is it still in /root/.ssh/authorized_keys, and does sshd allow root logins with a key (PermitRootLogin prohibit-password)?",
                login.user
            )
        } else {
            format!("Login refused for user '{}': wrong user name or password.", login.user)
        })
        .into());
    }
    match &remembered {
        Some(_) => log(&format!("  Connected. Host key {host_key} (the same as last time)")),
        None => {
            known_hosts::remember(&login.host, login.port, &host_key);
            log(&format!("  Connected. Host key {host_key} (remembered for the next login)"));
        }
    }

    let login_user = exec(&handle, "id -un", None).await?.output.trim().to_string();
    let uid = exec(&handle, "id -u", None).await?.output.trim().to_string();
    log(&format!("  Logged in as '{login_user}' (uid {uid})."));

    stage(ConnectStage::OperatingSystem);
    let os = OsInfo::parse(&exec(&handle, "cat /etc/os-release", None).await?.output);
    log(&format!("  Operating system: {}", os.pretty_name));
    if !os.is_supported() {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        return Err(ServerError(format!(
            "'{}' is not supported. This installer only supports Ubuntu Server and Debian.",
            os.pretty_name
        ))
        .into());
    }
    // Not a reason to stop: the hotel's own programs run, only some builds and tools fail later. Said now, instead of
    // halfway through a step.
    let cpu = exec(&handle, crate::cpu::SCRIPT, None).await?;
    if let Some(warning) = crate::cpu::warning(&crate::cpu::missing_from_output(&cpu.output)) {
        log(&warning);
    }

    stage(ConnectStage::Root);
    let mut server = SshServer { login, handle: Arc::new(handle), connected, host_key, login_user, os: Some(os), elevation: Elevation::None };
    if uid == "0" {
        server.elevation = Elevation::Root;
        log("  Already root, no sudo needed.");
    } else {
        server.elevate(log).await?;
    }

    let check = server.run_as_root("id -un").await?;
    if !check.succeeded() || check.output.trim() != "root" {
        return Err(ServerError(format!("Switching to root did not work: {}", first_line(&[&check.error, &check.output]))).into());
    }
    log("  Root access confirmed.");
    Ok(Arc::new(server))
}

impl SshServer {
    async fn elevate(&mut self, log: Log<'_>) -> Result<()> {
        let user = self.login_user.clone();
        log(&format!("> '{user}' is not root, switching to root with sudo ..."));
        if !exec(&self.handle, "command -v sudo", None).await?.succeeded() {
            return Err(ServerError(
                "sudo is not installed on the server. Log in as root, or install sudo and add the user to the 'sudo' group.".into(),
            )
            .into());
        }
        if exec(&self.handle, "sudo -n true", None).await?.succeeded() {
            self.elevation = Elevation::SudoNoPassword;
            log("  sudo works without a password.");
            return Ok(());
        }
        if self.login.uses_key() {
            return Err(ServerError(format!(
                "'{user}' logged in with the SSH key, but sudo asks for a password. Log in as root with the key, allow sudo without a password for '{user}', or log in with the password instead."
            ))
            .into());
        }
        let input = format!("{}\n", self.login.password);
        let result = exec(&self.handle, "sudo -S -p '' -k id -u", Some(input.as_bytes())).await?;
        if !result.succeeded() || result.output.trim() != "0" {
            let reason = first_line(&[&result.error, &result.output]);
            return Err(ServerError(format!(
                "sudo refused '{user}'{} Make sure the user is in the 'sudo' group (usermod -aG sudo {user}).",
                if reason.is_empty() { ".".to_string() } else { format!(": {reason}") }
            ))
            .into());
        }
        self.elevation = Elevation::SudoPassword;
        log("  sudo accepted the password.");
        Ok(())
    }

    fn root_line(&self, command: &str) -> Result<(String, Option<String>)> {
        root_command_line(self.elevation, &self.login.password, command)
            .ok_or_else(|| ServerError("The connection has no root access yet.".into()).into())
    }

    /// SFTP runs as the login user. Without a root login, files in root-only folders (the database backups) go
    /// through a staging folder that only the login user can open.
    fn sftp_is_root(&self) -> bool {
        self.login_user == "root"
    }

    async fn sftp(&self) -> Result<russh_sftp::client::SftpSession> {
        let channel = self.handle.channel_open_session().await.map_err(ssh_error)?;
        channel.request_subsystem(true, "sftp").await.map_err(ssh_error)?;
        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(ssh_error)
    }

    async fn create_staging_folder(&self) -> Result<String> {
        let result = self
            .run_as_root(&format!(
                "d=$(mktemp -d /tmp/all-in-1.XXXXXX) && chown {} \"$d\" && chmod 700 \"$d\" && echo \"$d\"",
                shell_quote(&self.login_user)
            ))
            .await?;
        if !result.succeeded() {
            return Err(ServerError(format!("Could not create a staging folder on the server: {}", first_line(&[&result.error, &result.output]))).into());
        }
        Ok(result.output.trim().to_string())
    }

    async fn remove_staging_folder(&self, folder: &str) {
        let _ = self.run_as_root(&format!("rm -rf -- {}", shell_quote(folder))).await;
    }

    async fn copy_down(&self, sftp: &russh_sftp::client::SftpSession, remote: &str, local: &Path, done_before: u64, progress: Progress<'_>) -> Result<u64> {
        let mut file = sftp.open(remote).await.map_err(ssh_error)?;
        let mut output = tokio::fs::File::create(local).await?;
        let mut buffer = vec![0u8; 64 * 1024];
        let mut copied = 0u64;
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).await?;
            copied += read as u64;
            progress(done_before + copied);
        }
        output.flush().await?;
        Ok(copied)
    }
}

#[async_trait]
impl Server for SshServer {
    fn login(&self) -> &ServerLogin {
        &self.login
    }

    fn login_user(&self) -> String {
        self.login_user.clone()
    }

    fn host_key_fingerprint(&self) -> String {
        self.host_key.clone()
    }

    fn os(&self) -> Option<OsInfo> {
        self.os.clone()
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst) && !self.handle.is_closed()
    }

    async fn run(&self, command: &str, input: Option<&str>) -> Result<CommandResult> {
        exec(&self.handle, command, input.map(str::as_bytes)).await
    }

    async fn run_as_root(&self, command: &str) -> Result<CommandResult> {
        let (line, input) = self.root_line(command)?;
        exec(&self.handle, &line, input.as_deref().map(str::as_bytes)).await
    }

    async fn run_as_root_streaming(&self, script: &str, on_line: OnLine<'_>, stdin: Option<&str>) -> Result<i32> {
        let (line, mut input) = self.root_line(&format!("exec 2>&1\n{script}"))?;
        // sudo -S reads only the password line, so everything after it reaches the script.
        if let Some(stdin) = stdin {
            input = Some(input.unwrap_or_default() + stdin);
        }
        let mut channel = self.handle.channel_open_session().await.map_err(ssh_error)?;
        channel.exec(true, line.replace("\r\n", "\n").as_bytes()).await.map_err(ssh_error)?;
        if let Some(input) = input {
            channel.data(input.as_bytes()).await.map_err(ssh_error)?;
        }
        channel.eof().await.map_err(ssh_error)?;
        let mut lines = LineSplitter { pending: Vec::new() };
        let (mut error, mut code) = (Vec::new(), None);
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => lines.push(&data, on_line),
                ChannelMsg::ExtendedData { data, ext: 1 } => error.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                _ => {}
            }
        }
        lines.finish(on_line);
        // Only sudo itself can still write to stderr, since the script's stderr is redirected.
        for text in String::from_utf8_lossy(&error).split('\n').filter(|l| !l.is_empty()) {
            on_line(text.trim_end_matches('\r'));
        }
        Ok(code.unwrap_or(-1))
    }

    async fn follow_as_root(&self, script: &str, on_line: OnLine<'_>, cancel: CancellationToken) -> Result<()> {
        // A background job's stdin is /dev/null unless it is redirected, hence <&0. sudo -S only reads the password line.
        let (line, password) = self.root_line(&format!("exec 2>&1\ncat >/dev/null <&0 &\nALL_IN_1_WATCH=$!\n{script}"))?;
        let mut channel = self.handle.channel_open_session().await.map_err(ssh_error)?;
        channel.exec(true, line.replace("\r\n", "\n").as_bytes()).await.map_err(ssh_error)?;
        // Stdin stays open while following: closing it would end the watcher, and with it the command.
        if let Some(password) = password {
            channel.data(password.as_bytes()).await.map_err(ssh_error)?;
        }
        let mut lines = LineSplitter { pending: Vec::new() };
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    // Closing the channel ends the watcher on the server, which stops the command.
                    let _ = channel.eof().await;
                    let _ = channel.close().await;
                    break;
                }
                message = channel.wait() => match message {
                    Some(ChannelMsg::Data { data }) => lines.push(&data, on_line),
                    Some(ChannelMsg::ExtendedData { data, .. }) => lines.push(&data, on_line),
                    Some(_) => {}
                    None => break,
                }
            }
        }
        lines.finish(on_line);
        Ok(())
    }

    async fn run_as_root_with_input(&self, script: &str, mut input: Box<dyn AsyncRead + Send + Unpin>) -> Result<CommandResult> {
        let (line, password) = self.root_line(&format!("exec 2>&1\n{script}"))?;
        let mut channel = self.handle.channel_open_session().await.map_err(ssh_error)?;
        channel.exec(true, line.replace("\r\n", "\n").as_bytes()).await.map_err(ssh_error)?;
        let writer_channel = channel.make_writer();
        // Reading and writing at the same time: the server's window would otherwise stall a large archive.
        let write = async move {
            let mut writer = writer_channel;
            // sudo -S reads only the password line, so the rest reaches the script.
            if let Some(password) = password {
                writer.write_all(password.as_bytes()).await?;
            }
            tokio::io::copy(&mut input, &mut writer).await?;
            writer.shutdown().await?;
            anyhow::Ok(())
        };
        let read = async {
            let (mut output, mut error, mut code) = (Vec::new(), Vec::new(), None);
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => output.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, ext: 1 } => error.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                    _ => {}
                }
            }
            (output, error, code)
        };
        let (written, (output, error, code)) = tokio::join!(write, read);
        let result = CommandResult {
            exit_code: code.unwrap_or(-1),
            output: String::from_utf8_lossy(&output).into_owned(),
            error: String::from_utf8_lossy(&error).into_owned(),
        };
        if let Err(e) = written {
            // The script ended early (a failed check): its output says why.
            if result.succeeded() {
                return Err(e);
            }
        }
        Ok(result)
    }

    async fn download(&self, remote_path: &str, local_path: &Path, progress: Progress<'_>) -> Result<()> {
        let mut source = remote_path.to_string();
        let mut staging = None;
        let result = async {
            if !self.sftp_is_root() {
                let folder = self.create_staging_folder().await?;
                source = format!("{folder}/{}", file_name(remote_path));
                staging = Some(folder);
                let copy = self
                    .run_as_root(&format!(
                        "cp -- {} {} && chown {} {}",
                        shell_quote(remote_path),
                        shell_quote(&source),
                        shell_quote(&self.login_user),
                        shell_quote(&source)
                    ))
                    .await?;
                if !copy.succeeded() {
                    return Err(ServerError(format!("Could not read {remote_path}: {}", first_line(&[&copy.error, &copy.output]))).into());
                }
            }
            let sftp = self.sftp().await?;
            self.copy_down(&sftp, &source, local_path, 0, progress).await?;
            let _ = sftp.close().await;
            anyhow::Ok(())
        }
        .await;
        if let Some(folder) = staging {
            self.remove_staging_folder(&folder).await;
        }
        result
    }

    async fn download_files(&self, files: &[(String, std::path::PathBuf)], progress: Progress<'_>, cancel: CancellationToken) -> Result<()> {
        let sftp = self.sftp().await?;
        let mut done = 0u64;
        for (remote, local) in files {
            if cancel.is_cancelled() {
                return Err(ServerError("Cancelled.".into()).into());
            }
            if let Some(folder) = local.parent() {
                std::fs::create_dir_all(folder)?;
            }
            done += self.copy_down(&sftp, remote, local, done, progress).await?;
            progress(done);
        }
        let _ = sftp.close().await;
        Ok(())
    }

    async fn read_file(&self, remote_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        let sftp = self.sftp().await?;
        let size = sftp.metadata(remote_path).await.map_err(ssh_error)?.size.unwrap_or(0);
        if size > max_bytes {
            return Ok(None);
        }
        let mut file = sftp.open(remote_path).await.map_err(ssh_error)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        let _ = sftp.close().await;
        Ok(Some(bytes))
    }

    async fn upload(&self, local_path: &Path, remote_path: &str, progress: Progress<'_>) -> Result<()> {
        let mut target = remote_path.to_string();
        let mut staging = None;
        let result = async {
            if !self.sftp_is_root() {
                let folder = self.create_staging_folder().await?;
                target = format!("{folder}/{}", file_name(remote_path));
                staging = Some(folder);
            }
            let sftp = self.sftp().await?;
            let mut input = tokio::fs::File::open(local_path).await?;
            let mut file = sftp.create(target.clone()).await.map_err(ssh_error)?;
            let mut buffer = vec![0u8; 64 * 1024];
            let mut sent = 0u64;
            loop {
                let read = input.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read]).await?;
                sent += read as u64;
                progress(sent);
            }
            file.shutdown().await?;
            let _ = sftp.close().await;
            if staging.is_some() {
                let moved = self
                    .run_as_root(&format!("mv -f -- {} {} && chown root:root {}", shell_quote(&target), shell_quote(remote_path), shell_quote(remote_path)))
                    .await?;
                if !moved.succeeded() {
                    return Err(ServerError(format!("Could not write {remote_path}: {}", first_line(&[&moved.error, &moved.output]))).into());
                }
            }
            anyhow::Ok(())
        }
        .await;
        if let Some(folder) = staging {
            self.remove_staging_folder(&folder).await;
        }
        result
    }

    async fn open_shell(&self, columns: u32, rows: u32) -> Result<ShellChannel> {
        let mut channel = self.handle.channel_open_session().await.map_err(ssh_error)?;
        channel.request_pty(true, "xterm-256color", columns, rows, 0, 0, &[]).await.map_err(ssh_error)?;
        channel.request_shell(true).await.map_err(ssh_error)?;
        let (input, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<ShellInput>();
        let (output_tx, output) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    message = input_rx.recv() => match message {
                        Some(ShellInput::Data(bytes)) => { if channel.data(bytes.as_slice()).await.is_err() { break; } }
                        Some(ShellInput::Resize { columns, rows }) => { let _ = channel.window_change(columns, rows, 0, 0).await; }
                        Some(ShellInput::Close) | None => { let _ = channel.close().await; break; }
                    },
                    message = channel.wait() => match message {
                        Some(ChannelMsg::Data { data }) => { if output_tx.send(data.to_vec()).is_err() { break; } }
                        Some(ChannelMsg::ExtendedData { data, .. }) => { let _ = output_tx.send(data.to_vec()); }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
            }
        });
        Ok(ShellChannel { input, output })
    }

    async fn forward_local_port(&self, host: &str, port: u16) -> Result<PortForward> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_port = listener.local_addr()?.port();
        let handle = self.handle.clone();
        let host = host.to_string();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let handle = handle.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    let Ok(channel) = handle.channel_open_direct_tcpip(host, port as u32, peer.ip().to_string(), peer.port() as u32).await else {
                        return;
                    };
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
        });
        Ok(PortForward { local_port, stop: Some(task) })
    }

    async fn test_key_login(&self, private_key_pem: &str, passphrase: &str) -> Option<String> {
        let key = match read_private_key(private_key_pem, passphrase) {
            Ok(key) => key,
            Err(e) => return Some(e.to_string()),
        };
        // Trust only the host key of the existing connection.
        let (mut handle, _, _) = match open_session(&self.login.host, self.login.port, Some(self.host_key.clone())).await {
            Ok(session) => session,
            Err(e) => return Some(e.to_string()),
        };
        match authenticate_with_key(&mut handle, "root", key).await {
            Ok(true) => {}
            Ok(false) => return Some("the server refused the key".into()),
            Err(e) => return Some(e.to_string()),
        }
        let user = match exec(&handle, "id -un", None).await {
            Ok(result) => result.output.trim().to_string(),
            Err(e) => return Some(e.to_string()),
        };
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        if user == "root" {
            None
        } else {
            Some(format!("logged in as '{user}' instead of root"))
        }
    }

    async fn disconnect(&self) {
        self.connected.store(false, Ordering::SeqCst);
        let _ = self.handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}
