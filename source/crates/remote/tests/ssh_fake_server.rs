//! The SSH client against an in-process russh server: login, root commands, streaming with stdin, exit codes and a
//! large stdin upload.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aio_remote::server::{ConnectStage, Server, ServerLogin};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::io::AsyncReadExt;

#[derive(Clone, Default)]
struct Fake {
    commands: Arc<Mutex<HashMap<ChannelId, (String, Vec<u8>)>>>,
}

fn respond(command: &str, stdin: &[u8]) -> (Vec<u8>, u32) {
    let inner = command.strip_prefix("bash -c '").map(|c| c.trim_end_matches('\'').replace("'\\''", "'"));
    match (command, inner.as_deref()) {
        ("id -un", _) | (_, Some("id -un")) => (b"root\n".to_vec(), 0),
        ("id -u", _) => (b"0\n".to_vec(), 0),
        ("cat /etc/os-release", _) => (b"ID=ubuntu\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04 LTS\"\n".to_vec(), 0),
        (_, Some(script)) if script.contains("fail-me") => (b"line one\nERROR: boom\nlast line\n".to_vec(), 1),
        (_, Some(script)) if script.contains("echo-stdin") => (stdin.to_vec(), 0),
        (_, Some(script)) if script.contains("count-stdin") => (format!("got {} bytes\n", stdin.len()).into_bytes(), 0),
        _ => (format!("unknown: {command}\n").into_bytes(), 127),
    }
}

impl russh::server::Handler for Fake {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "root" && password == "secret" { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(&mut self, _channel: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _session: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.commands.lock().unwrap().insert(channel, (String::from_utf8_lossy(data).into_owned(), Vec::new()));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], _session: &mut Session) -> Result<(), Self::Error> {
        if let Some((_, stdin)) = self.commands.lock().unwrap().get_mut(&channel) {
            stdin.extend_from_slice(data);
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let Some((command, stdin)) = self.commands.lock().unwrap().remove(&channel) else { return Ok(()) };
        let (output, code) = respond(&command, &stdin);
        session.data(channel, output)?;
        session.exit_status_request(channel, code)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

async fn start() -> u16 {
    let pair = aio_core::keys::generate(aio_core::keys::SshKeyType::Ed25519, "host", "");
    let key = russh::keys::decode_secret_key(&pair.private_key_pem, None).unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { break };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, socket, Fake::default()).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

fn log(_: &str) {}

#[tokio::test]
async fn login_and_commands() {
    let port = start().await;
    let lines = Mutex::new(Vec::new());
    let server = aio_remote::ssh::connect(ServerLogin::password("127.0.0.1", port, "root", "secret"), &|l: &str| lines.lock().unwrap().push(l.to_string()), &|_: ConnectStage| {})
        .await
        .unwrap();
    assert_eq!(server.login_user(), "root");
    assert_eq!(server.os().unwrap().pretty_name, "Ubuntu 24.04 LTS");
    assert!(server.host_key_fingerprint().starts_with("ssh-ed25519 SHA256:"), "{}", server.host_key_fingerprint());
    let log_text = lines.lock().unwrap().join("\n");
    assert!(log_text.contains("Already root, no sudo needed."), "{log_text}");
    assert!(log_text.contains("Root access confirmed."), "{log_text}");

    assert_eq!(server.run_as_root("id -un").await.unwrap().output, "root\n");

    // A failing script: the last fail() message.
    let error = aio_remote::script::run(&*server, "fail-me", &log, "Doing it failed", None).await.unwrap_err();
    assert_eq!(error.to_string(), "Doing it failed: boom");

    // Streaming with stdin.
    let mut got = Vec::new();
    let code = server.run_as_root_streaming("echo-stdin", &mut |l: &str| got.push(l.to_string()), Some("hello\r\nworld")).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(got, vec!["hello", "world"]);

    // A large stdin (a tar archive) while the output is read.
    let data = vec![7u8; 3 * 1024 * 1024];
    let result = server.run_as_root_with_input("count-stdin", Box::new(std::io::Cursor::new(data))).await.unwrap();
    assert_eq!(result.output.trim(), format!("got {} bytes", 3 * 1024 * 1024));
    assert!(server.is_connected());
    server.disconnect().await;
    let _ = tokio::io::empty().read_u8().await;
}

#[tokio::test]
async fn wrong_password() {
    let port = start().await;
    let error = aio_remote::ssh::connect(ServerLogin::password("127.0.0.1", port, "root", "nope"), &log, &|_: ConnectStage| {}).await.err().unwrap();
    assert_eq!(error.to_string(), "Login refused for user 'root': wrong user name or password.");
}

#[tokio::test]
async fn unreachable() {
    let error = aio_remote::ssh::connect(ServerLogin::password("127.0.0.1", 1, "root", "x"), &log, &|_: ConnectStage| {}).await.err().unwrap();
    assert!(error.to_string().starts_with("Could not reach 127.0.0.1:1:"), "{error}");
}
