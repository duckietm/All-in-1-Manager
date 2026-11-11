//! The HTTP client of the ripper (image downloads and the G-Earth download).

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// One shared client: rustls, gzip, a 30 s connect timeout.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("All-in-1-Manager/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

/// The body of `url`; `None` for 404/403/410 (the file is not there), an error for anything
/// else that is not a success (or when the host cannot be reached).
pub async fn try_get_bytes(url: &str) -> Result<Option<Vec<u8>>> {
    let response = client()
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    if matches!(status.as_u16(), 403 | 404 | 410) {
        return Ok(None);
    }
    if !status.is_success() {
        bail!("{url} answered {status}");
    }
    Ok(Some(response.bytes().await?.to_vec()))
}
