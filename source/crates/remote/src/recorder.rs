//! A [`Server`] that runs nothing: it records every command and answers from a responder. The dry-run mode shows
//! these commands instead of running them, and the parity tests compare the record with the golden files the C#
//! installer wrote (`tools/parity`), in the same format:
//!
//! ```text
//! === <kind>
//! <command>
//! --- stdin
//! <stdin>
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::server::{CommandResult, OnLine, OsInfo, PortForward, Progress, Result, Server, ServerLogin, ShellChannel};

pub type Responder = dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync;

pub struct Recorder {
    login: ServerLogin,
    log: Mutex<String>,
    respond: Mutex<Arc<Responder>>,
    /// Called with every record, as it is written (dry-run shows them in the step's log).
    on_record: Option<Box<dyn Fn(&str, &str) + Send + Sync>>,
    pub os: OsInfo,
    pub login_user: String,
    dry_run: bool,
}

impl Recorder {
    /// Exit code 0 and no output for everything.
    pub fn new() -> Self {
        Self {
            login: ServerLogin::password("192.0.2.10", 22, "root", "secret"),
            log: Mutex::new(String::new()),
            respond: Mutex::new(Arc::new(|_: &str, _: &str, _: Option<&str>| CommandResult::default())),
            on_record: None,
            os: OsInfo { id: "ubuntu".into(), version_id: "24.04".into(), pretty_name: "Ubuntu 24.04 LTS".into() },
            login_user: "root".into(),
            dry_run: false,
        }
    }

    /// The dry-run server of the window: `on_record` gets (kind, command) for every command a step would send.
    pub fn dry_run(login: ServerLogin, on_record: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        Self { login_user: login.user.clone(), login, on_record: Some(Box::new(on_record)), dry_run: true, ..Self::new() }
    }

    pub fn with_responder(self, respond: impl Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync + 'static) -> Self {
        *self.respond.lock().unwrap() = Arc::new(respond);
        self
    }

    pub fn set_responder(&self, respond: impl Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync + 'static) {
        *self.respond.lock().unwrap() = Arc::new(respond);
    }

    /// What was recorded so far.
    pub fn text(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    /// Adds a line that is not a command (the parity harness writes exceptions as "!!! <type>: <message>").
    pub fn note(&self, line: &str) {
        let mut log = self.log.lock().unwrap();
        log.push_str(line);
        log.push('\n');
    }

    pub fn record(&self, kind: &str, command: &str, stdin: Option<&str>) -> CommandResult {
        let command = command.replace("\r\n", "\n");
        {
            let mut log = self.log.lock().unwrap();
            log.push_str("=== ");
            log.push_str(kind);
            log.push('\n');
            log.push_str(&command);
            if !command.ends_with('\n') {
                log.push('\n');
            }
            if let Some(stdin) = stdin {
                let stdin = stdin.replace("\r\n", "\n");
                log.push_str("--- stdin\n");
                log.push_str(&stdin);
                if !stdin.ends_with('\n') {
                    log.push('\n');
                }
            }
        }
        if let Some(on_record) = &self.on_record {
            on_record(kind, &command);
        }
        let respond = self.respond.lock().unwrap().clone();
        respond(kind, &command, stdin)
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Server for Recorder {
    fn login(&self) -> &ServerLogin {
        &self.login
    }

    fn login_user(&self) -> String {
        self.login_user.clone()
    }

    fn host_key_fingerprint(&self) -> String {
        "ssh-ed25519 SHA256:test".into()
    }

    fn os(&self) -> Option<OsInfo> {
        Some(self.os.clone())
    }

    fn is_connected(&self) -> bool {
        true
    }

    fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    async fn run(&self, command: &str, input: Option<&str>) -> Result<CommandResult> {
        Ok(self.record("run", command, input))
    }

    async fn run_as_root(&self, command: &str) -> Result<CommandResult> {
        Ok(self.record("root", command, None))
    }

    async fn run_as_root_streaming(&self, script: &str, on_line: OnLine<'_>, stdin: Option<&str>) -> Result<i32> {
        let result = self.record("stream", script, stdin);
        for line in result.output.split('\n').filter(|l| !l.is_empty()) {
            on_line(line);
        }
        Ok(result.exit_code)
    }

    async fn follow_as_root(&self, script: &str, _on_line: OnLine<'_>, _cancel: CancellationToken) -> Result<()> {
        self.record("follow", script, None);
        Ok(())
    }

    async fn run_as_root_with_input(&self, script: &str, mut input: Box<dyn AsyncRead + Send + Unpin>) -> Result<CommandResult> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await?;
        Ok(self.record("input", script, Some(&format!("<{} bytes>", bytes.len()))))
    }

    async fn download(&self, remote_path: &str, local_path: &Path, _progress: Progress<'_>) -> Result<()> {
        self.record("download", &format!("{remote_path} -> {}", local_path.display()), None);
        Ok(())
    }

    async fn download_files(&self, files: &[(String, std::path::PathBuf)], _progress: Progress<'_>, _cancel: CancellationToken) -> Result<()> {
        let list: Vec<String> = files.iter().map(|(remote, local)| format!("{remote} -> {}", local.display())).collect();
        self.record("download-files", &list.join("\n"), None);
        Ok(())
    }

    async fn read_file(&self, remote_path: &str, _max_bytes: u64) -> Result<Option<Vec<u8>>> {
        self.record("read-file", remote_path, None);
        Ok(Some(Vec::new()))
    }

    async fn upload(&self, local_path: &Path, remote_path: &str, _progress: Progress<'_>) -> Result<()> {
        self.record("upload", &format!("{} -> {remote_path}", local_path.display()), None);
        Ok(())
    }

    async fn open_shell(&self, _columns: u32, _rows: u32) -> Result<ShellChannel> {
        self.record("shell", "xterm-256color", None);
        let (input, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, output) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = output_tx.send(b"Dry run: no shell on a server.\r\n".to_vec());
            while input_rx.recv().await.is_some() {}
        });
        Ok(ShellChannel { input, output })
    }

    async fn forward_local_port(&self, host: &str, port: u16) -> Result<PortForward> {
        self.record("forward", &format!("{host}:{port}"), None);
        Ok(PortForward { local_port: 13306, stop: None })
    }

    async fn test_key_login(&self, _private_key_pem: &str, _passphrase: &str) -> Option<String> {
        self.record("key-login", "root", None);
        None
    }

    async fn disconnect(&self) {}
}
