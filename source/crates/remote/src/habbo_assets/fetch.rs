//! One GET of the downloader: with its own HTTP client, or through the system's curl (curl on Linux and macOS,
//! curl.exe on Windows 10 and later). Some retros sit behind Cloudflare's bot check, which looks at how the connection
//! is made and not only at the headers: curl gets through where the built-in client is held up.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::furniture_language::USER_AGENT;

/// The headers a browser sends with every request, so a CDN answers the downloader as it answers a browser.
pub const ACCEPT: &str = "*/*";
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9,nl;q=0.8";

/// What the server answered.
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
    /// Cloudflare's bot check answered instead of the file (403/503 with "cf-mitigated: challenge").
    pub challenge: bool,
}

impl Answer {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// How the downloader asks.
pub enum Transport {
    Client(reqwest::Client),
    Curl { program: PathBuf, headers: Vec<(String, String)>, counter: AtomicUsize },
}

/// The curl to run: curl.exe on Windows (in System32 since Windows 10 1803), curl elsewhere.
fn curl_program() -> PathBuf {
    PathBuf::from(if cfg!(windows) { "curl.exe" } else { "curl" })
}

impl Transport {
    pub fn client(headers: &[(String, String)]) -> Result<Self, String> {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static(ACCEPT));
        map.insert(reqwest::header::ACCEPT_LANGUAGE, reqwest::header::HeaderValue::from_static(ACCEPT_LANGUAGE));
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| format!("\"{name}\" is not a valid header name."))?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| format!("The value of header {name} is not valid."))?;
            map.insert(name, value);
        }
        // default_headers after user_agent: a User-Agent among the headers replaces the built-in one.
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(map)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Transport::Client(client))
    }

    /// The system's curl, checked once. Returns it and its version line (for the log).
    pub fn curl(headers: &[(String, String)]) -> Result<(Self, String), String> {
        for (name, value) in headers {
            if name.is_empty() || name.contains([' ', ':', '\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(format!("\"{name}\" is not a valid header."));
            }
        }
        let program = curl_program();
        let mut command = std::process::Command::new(&program);
        command.arg("--version");
        hide_window_std(&mut command);
        let output = command.output().map_err(|e| format!("curl was not found ({}: {e}). Install curl, or turn \"Download with curl\" off.", program.display()))?;
        let version = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("curl").to_string();
        Ok((Transport::Curl { program, headers: headers.to_vec(), counter: AtomicUsize::new(0) }, version))
    }

    pub async fn get(&self, url: &str) -> Result<Answer, String> {
        match self {
            Transport::Client(client) => {
                let response = client.get(url).send().await.map_err(|e| e.to_string())?;
                let status = response.status().as_u16();
                let challenge = response.headers().get("cf-mitigated").and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("challenge"));
                let body = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
                Ok(Answer { status, body, challenge })
            }
            Transport::Curl { program, headers, counter } => curl_get(program, headers, counter, url).await,
        }
    }
}

/// One curl run: the body and the response headers into temporary files, the status code on stdout ("-w"), which
/// works with every curl Windows has shipped.
async fn curl_get(program: &PathBuf, headers: &[(String, String)], counter: &AtomicUsize, url: &str) -> Result<Answer, String> {
    let folder = std::env::temp_dir().join("aio-curl");
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let n = counter.fetch_add(1, Ordering::Relaxed);
    let stem = format!("{}-{n}", std::process::id());
    let (body_path, head_path) = (folder.join(format!("{stem}.body")), folder.join(format!("{stem}.head")));
    let mut command = tokio::process::Command::new(program);
    command
        .args(["-sS", "-L", "--connect-timeout", "20", "--max-time", "180", "-w", "%{http_code}", "-A", USER_AGENT])
        .args(["-H", &format!("Accept: {ACCEPT}"), "-H", &format!("Accept-Language: {ACCEPT_LANGUAGE}")])
        .arg("-o")
        .arg(&body_path)
        .arg("-D")
        .arg(&head_path);
    for (name, value) in headers {
        // A -H User-Agent replaces the one -A gives.
        command.arg("-H").arg(format!("{name}: {value}"));
    }
    command.arg("--url").arg(url).stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command.output().await;
    let body = std::fs::read(&body_path).unwrap_or_default();
    let head = std::fs::read_to_string(&head_path).unwrap_or_default().to_ascii_lowercase();
    let _ = std::fs::remove_file(&body_path);
    let _ = std::fs::remove_file(&head_path);
    let output = output.map_err(|e| format!("curl: {e}"))?;
    let status: u16 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
    if status == 0 {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() { format!("curl ended with {}", output.status) } else { message });
    }
    let challenge = head.lines().any(|l| l.trim() == "cf-mitigated: challenge");
    Ok(Answer { status, body, challenge })
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// No console window flashing up for every curl on Windows.
fn hide_window_std(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
