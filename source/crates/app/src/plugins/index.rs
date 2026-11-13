//! The official signed plugin index: fetched from [`OFFICIAL_INDEX_URL`] (and its `.sig`), verified with
//! [`OFFICIAL_INDEX_KEYS`], checked for rollback against the highest `sequence` ever accepted, and cached in
//! configuration/plugins/index/ (`index.json`, `index.json.sig`, `state.json`). The cache is verified again every time
//! it is loaded, so a key removed from the program invalidates it.
//!
//! Developer mode only: the settings `plugin_index_url` and `plugin_index_test_keys` replace the official url and
//! keys (see [`source_for`]); that index is cached apart, in configuration/plugins/index/test/, so it never mixes with
//! the official cache nor raises the official sequence.

// The Plugins page (a later task of this milestone) calls `refresh`, `index_state` and `set_repaint`; remove then.
#![allow(dead_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use aio_core::InstallerSettings;
use aio_plugin_index::{check_fresh, verify_and_parse, Freshness, Index, MAX_INDEX_BYTES};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::i18n::n;

/// The official `index.json`; its signature is the same url with `.sig`.
pub const OFFICIAL_INDEX_URL: &str = "https://raw.githubusercontent.com/all-in-1-plugins/index/main/index.json";
/// Every package url of the official index starts with this (`<prefix><id>-<version>/<id>.aiop`).
pub const OFFICIAL_PACKAGE_PREFIX: &str = "https://github.com/all-in-1-plugins/index/releases/download/";
/// The keys the official index may be signed with: `(key id, base64 public key)`, at most two at once (rotation).
/// Empty until the owner generates the real key: then nothing is fetched and the index is "not set up yet".
pub const OFFICIAL_INDEX_KEYS: &[(&str, &str)] = &[];
/// The `.sig` file is two short lines.
pub const MAX_SIG_BYTES: usize = 4 * 1024;
/// Both downloads together.
const TIMEOUT: Duration = Duration::from_secs(30);

pub const NOT_SET_UP: &str = n("The official plugin index is not set up yet");
pub const ROLLBACK: &str = n("The index is older than one already seen");
const BAD_TEST_URL: &str = n("The index URL (testing) must be https, or http on 127.0.0.1 or localhost");
const BAD_TEST_KEY: &str = n("A test key is not in the form key-id:base64");
const NO_TEST_KEYS: &str = n("The test index has no keys");

/// What the Manager knows of the index.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexState {
    /// The verified index (from this run's check, else from the cache); never one that is a rollback.
    pub index: Option<Index>,
    pub freshness: Option<Freshness>,
    /// Why the last check failed (English; shown through `t()`), or [`NOT_SET_UP`]. The index above is kept.
    pub error: Option<String>,
    /// When the index was last fetched and accepted in this run.
    pub checked: Option<DateTime<Utc>>,
}

/// Where an index comes from, the keys it must be signed with and where it is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSource {
    pub index_url: String,
    pub sig_url: String,
    /// Always hard-coded ([`OFFICIAL_PACKAGE_PREFIX`]) or derived from the test url, never from index data.
    pub package_prefix: String,
    pub keys: Vec<(String, String)>,
    pub cache: PathBuf,
}

/// `state.json`: the highest sequence ever accepted from this source.
#[derive(Serialize, Deserialize)]
struct Stored {
    sequence: u64,
}

/// The source to use, `plugins_root` being configuration/plugins/. Developer mode with a `plugin_index_url` uses
/// that url, its `.sig`, the `plugin_index_test_keys` and the package prefix `<the url's directory>/packages/`
/// (a test index lists its packages next to itself), cached in `index/test/`. Otherwise the official index, cached
/// in `index/`. `Err` is the reason nothing can be fetched: [`NOT_SET_UP`] when there are no official keys.
pub fn source_for(settings: &InstallerSettings, official_keys: &[(&str, &str)], plugins_root: &Path) -> Result<IndexSource, String> {
    let cache = plugins_root.join("index");
    if let Some(url) = settings.plugin_index_url.as_deref().map(str::trim).filter(|u| settings.developer_mode && !u.is_empty()) {
        return test_source(url, &settings.plugin_index_test_keys, cache.join("test"));
    }
    if official_keys.is_empty() {
        return Err(NOT_SET_UP.to_string());
    }
    Ok(IndexSource {
        index_url: OFFICIAL_INDEX_URL.to_string(),
        sig_url: format!("{OFFICIAL_INDEX_URL}.sig"),
        package_prefix: OFFICIAL_PACKAGE_PREFIX.to_string(),
        keys: official_keys.iter().map(|(id, key)| (id.to_string(), key.to_string())).collect(),
        cache,
    })
}

/// The developer-mode test index: https anywhere, or http on 127.0.0.1 / localhost; no query, fragment or login.
fn test_source(url: &str, keys: &[String], cache: PathBuf) -> Result<IndexSource, String> {
    let bad = || BAD_TEST_URL.to_string();
    let parsed = reqwest::Url::parse(url).map_err(|_| bad())?;
    let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
    let scheme_ok = parsed.scheme() == "https" || (parsed.scheme() == "http" && local);
    let plain = parsed.query().is_none() && parsed.fragment().is_none() && parsed.username().is_empty() && parsed.password().is_none();
    if !scheme_ok || !plain || parsed.host_str().is_none() || parsed.path().ends_with('/') {
        return Err(bad());
    }
    let package_prefix = parsed.join("packages/").map_err(|_| bad())?.to_string();
    let keys = keys
        .iter()
        .map(|k| {
            let (id, key) = k.trim().split_once(':').ok_or_else(|| BAD_TEST_KEY.to_string())?;
            let (id, key) = (id.trim(), key.trim());
            if id.is_empty() || key.is_empty() {
                return Err(BAD_TEST_KEY.to_string());
            }
            Ok((id.to_string(), key.to_string()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if keys.is_empty() {
        return Err(NO_TEST_KEYS.to_string());
    }
    let index_url = parsed.to_string();
    Ok(IndexSource { sig_url: format!("{index_url}.sig"), index_url, package_prefix, keys, cache })
}

impl IndexSource {
    fn key_refs(&self) -> Vec<(&str, &str)> {
        self.keys.iter().map(|(id, key)| (id.as_str(), key.as_str())).collect()
    }
}

/// The highest sequence accepted from the source cached in `dir`, if `state.json` is readable.
fn stored_sequence(dir: &Path) -> Option<u64> {
    let text = std::fs::read(dir.join("state.json")).ok()?;
    serde_json::from_slice::<Stored>(&text).ok().map(|s| s.sequence)
}

/// The cached index of `source`, verified again with its keys and checked against `state.json`.
pub fn load_cache_from(source: &IndexSource, now: DateTime<Utc>) -> IndexState {
    let dir = &source.cache;
    let index_path = dir.join("index.json");
    let Ok(meta) = std::fs::metadata(&index_path) else {
        return IndexState::default();
    };
    let invalid = |why: String| IndexState { error: Some(format!("The saved plugin index is not valid: {why}")), ..IndexState::default() };
    if meta.len() > MAX_INDEX_BYTES as u64 {
        return invalid("larger than 4 MiB".into());
    }
    let (Ok(bytes), Ok(sig)) = (std::fs::read(&index_path), std::fs::read_to_string(dir.join("index.json.sig"))) else {
        return invalid("it cannot be read".into());
    };
    match verify_and_parse(&bytes, &sig, &source.key_refs(), &source.package_prefix) {
        Ok((index, _)) => match check_fresh(&index, stored_sequence(dir), now) {
            Freshness::Rollback => IndexState { error: Some(ROLLBACK.to_string()), ..IndexState::default() },
            fresh => IndexState { index: Some(index), freshness: Some(fresh), error: None, checked: None },
        },
        Err(e) => invalid(format!("{e:#}")),
    }
}

/// Fetches, verifies and checks the index of `source`; on success caches it and the new sequence. On any failure
/// the cached index stays and `error` says why.
pub async fn refresh_from(client: &reqwest::Client, source: &IndexSource, now: DateTime<Utc>) -> IndexState {
    let cached = load_cache_from(source, now);
    match fetch_verified(client, source, now, cached.index.as_ref()).await {
        Ok(state) => state,
        Err(error) => IndexState { error: Some(error), ..cached },
    }
}

async fn fetch_verified(client: &reqwest::Client, source: &IndexSource, now: DateTime<Utc>, cached: Option<&Index>) -> Result<IndexState, String> {
    let fetch_both = async {
        let index = fetch_capped(client, &source.index_url, MAX_INDEX_BYTES).await?;
        let sig = fetch_capped(client, &source.sig_url, MAX_SIG_BYTES).await?;
        Ok::<_, String>((index, sig))
    };
    let (index_bytes, sig_bytes) = tokio::time::timeout(TIMEOUT, fetch_both)
        .await
        .map_err(|_| "The plugin index could not be downloaded: the request timed out".to_string())?
        .map_err(|e| format!("The plugin index could not be downloaded: {e}"))?;
    let sig = String::from_utf8(sig_bytes).map_err(|_| "The index signature file is not in the expected format".to_string())?;
    let (index, _) = verify_and_parse(&index_bytes, &sig, &source.key_refs(), &source.package_prefix).map_err(|e| format!("{e:#}"))?;
    // The highest sequence seen: state.json, or the verified cache if state.json was lost.
    let last = stored_sequence(&source.cache).into_iter().chain(cached.map(|i| i.sequence)).max();
    let freshness = check_fresh(&index, last, now);
    if freshness == Freshness::Rollback {
        return Err(ROLLBACK.to_string());
    }
    let sequence = last.map_or(index.sequence, |l| l.max(index.sequence));
    let error = write_cache(&source.cache, &index_bytes, &sig, sequence).err().map(|e| format!("The plugin index could not be saved: {e}"));
    Ok(IndexState { index: Some(index), freshness: Some(freshness), error, checked: Some(now) })
}

/// GET `url` into memory; a non-2xx status is `Err("HTTP <code>")` and a body past `cap` bytes is refused while
/// streaming.
async fn fetch_capped(client: &reqwest::Client, url: &str, cap: usize) -> Result<Vec<u8>, String> {
    let too_large = || format!("{url} is larger than {cap} bytes");
    let mut response = client.get(url).send().await.map_err(|e| error_text(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    if response.content_length().is_some_and(|n| n > cap as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| error_text(&e))? {
        if body.len() + chunk.len() > cap {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn error_text(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

/// Writes the three cache files, each to a temporary file then renamed over the old one; `state.json` last, so a
/// crash in between leaves at worst an index that no longer verifies (then refused) or a sequence not yet raised
/// (then [`fetch_verified`] still uses the cached index's sequence).
fn write_cache(dir: &Path, index: &[u8], sig: &str, sequence: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    write_atomic(&dir.join("index.json"), index)?;
    write_atomic(&dir.join("index.json.sig"), sig.as_bytes())?;
    let state = serde_json::to_vec(&Stored { sequence }).map_err(std::io::Error::other)?;
    write_atomic(&dir.join("state.json"), &state)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temp = path.with_file_name(name);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    let result = written.and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The client for the index: no redirects at all (raw.githubusercontent.com serves the files directly), 30 s per
/// request, the Manager's user agent.
pub fn index_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("All-in-1-Manager/{}", crate::update::CURRENT_VERSION))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .expect("HTTP client")
}

/// The process-wide state, first loaded from the cache.
fn shared() -> &'static Mutex<IndexState> {
    static STATE: OnceLock<Mutex<IndexState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(load_cache()))
}

static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Called after every [`refresh`] (the window asks egui to repaint). Set once; later calls are ignored.
pub fn set_repaint(repaint: impl Fn() + Send + Sync + 'static) {
    let _ = REPAINT.set(Box::new(repaint));
}

/// What the Manager knows of the index now.
pub fn index_state() -> IndexState {
    shared().lock().clone()
}

/// The cached index of the source the settings choose, verified again with that source's keys.
pub fn load_cache() -> IndexState {
    let settings = InstallerSettings::load();
    match source_for(&settings, OFFICIAL_INDEX_KEYS, &super::packs::root()) {
        Ok(source) => load_cache_from(&source, Utc::now()),
        Err(e) => IndexState { error: Some(e), ..IndexState::default() },
    }
}

/// Fetches and checks the index the settings choose (use [`index_client`]), updates [`index_state`] and asks for a
/// repaint. Never touches the network when the index is not set up.
pub async fn refresh(client: &reqwest::Client) -> IndexState {
    let settings = InstallerSettings::load();
    let state = refresh_in(client, &settings, OFFICIAL_INDEX_KEYS, &super::packs::root(), Utc::now()).await;
    *shared().lock() = state.clone();
    if let Some(repaint) = REPAINT.get() {
        repaint();
    }
    state
}

/// [`refresh_from`] the source the settings choose; [`NOT_SET_UP`] (and no request) when there is none.
async fn refresh_in(
    client: &reqwest::Client,
    settings: &InstallerSettings,
    official_keys: &[(&str, &str)],
    plugins_root: &Path,
    now: DateTime<Utc>,
) -> IndexState {
    match source_for(settings, official_keys, plugins_root) {
        Ok(source) => refresh_from(client, &source, now).await,
        Err(e) => IndexState { error: Some(e), ..IndexState::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const SECRET: [u8; 32] = [9; 32];
    const OTHER: [u8; 32] = [3; 32];

    /// status, extra headers, body
    type Reply = (u16, Vec<(String, String)>, Vec<u8>);

    /// A raw HTTP/1.1 responder on 127.0.0.1 that serves fixed replies by path and counts the requests.
    struct Server {
        addr: std::net::SocketAddr,
        routes: Arc<Mutex<HashMap<String, Reply>>>,
        hits: Arc<AtomicUsize>,
    }

    impl Server {
        async fn start() -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
            let hits = Arc::new(AtomicUsize::new(0));
            let (r, h) = (routes.clone(), hits.clone());
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let (routes, hits) = (r.clone(), h.clone());
                    tokio::spawn(async move {
                        let mut head = Vec::new();
                        let mut buf = [0u8; 1024];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        hits.fetch_add(1, Ordering::SeqCst);
                        let text = String::from_utf8_lossy(&head);
                        let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                        let (status, headers, body) = routes.lock().get(&path).cloned().unwrap_or((404, vec![], vec![]));
                        let mut reply = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                        for (k, v) in headers {
                            reply.push_str(&format!("{k}: {v}\r\n"));
                        }
                        reply.push_str("\r\n");
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.write_all(&body).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Server { addr, routes, hits }
        }

        fn url(&self) -> String {
            format!("http://{}/index.json", self.addr)
        }

        fn set(&self, path: &str, status: u16, body: Vec<u8>) {
            self.routes.lock().insert(path.to_string(), (status, vec![], body));
        }

        fn publish(&self, index: &[u8], sig: &str) {
            self.set("/index.json", 200, index.to_vec());
            self.set("/index.json.sig", 200, sig.as_bytes().to_vec());
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    fn public(secret: &[u8; 32]) -> String {
        let public = SigningKey::from_bytes(secret).verifying_key().to_bytes();
        format!("{}:{}", aio_plugin_index::key_id(&public), base64::engine::general_purpose::STANDARD.encode(public))
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn now() -> DateTime<Utc> {
        at("2026-11-20T00:00:00Z")
    }

    /// An index of sequence `sequence` with one plugin whose package is under the test server's `packages/`.
    fn index_json(server: &Server, sequence: u64, expires: &str) -> Vec<u8> {
        let prefix = format!("http://{}/packages/", server.addr);
        serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "sequence": sequence,
            "generated": "2026-11-13T10:00:00Z",
            "expires": expires,
            "plugins": [{
                "id": "hello",
                "name": "Hello",
                "version": "1.2.3",
                "author": "Someone",
                "description": "Says hello.",
                "api": "1.0",
                "permissions": ["ssh"],
                "size": 12345,
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "url": format!("{prefix}hello-1.2.3/hello.aiop"),
                "source": { "repo": "https://github.com/someone/hello", "commit": "0123456789abcdef0123456789abcdef01234567" }
            }],
            "revoked": []
        }))
        .unwrap()
    }

    fn signed(server: &Server, sequence: u64) -> (Vec<u8>, String) {
        let bytes = index_json(server, sequence, "2026-12-13T10:00:00Z");
        let sig = aio_plugin_index::sign(&bytes, &SECRET);
        (bytes, sig)
    }

    fn dev_settings(server: &Server) -> InstallerSettings {
        InstallerSettings {
            developer_mode: true,
            plugin_index_url: Some(server.url()),
            plugin_index_test_keys: vec![public(&SECRET)],
            ..InstallerSettings::default()
        }
    }

    fn client() -> reqwest::Client {
        index_client()
    }

    fn sequence_file(dir: &Path) -> u64 {
        let text = std::fs::read_to_string(dir.join("state.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["sequence"].as_u64().unwrap()
    }

    /// A test source over `server`, cached under `root`, after one accepted refresh of sequence 10.
    async fn accepted(server: &Server, root: &Path) -> IndexSource {
        let source = source_for(&dev_settings(server), &[], root).unwrap();
        let (bytes, sig) = signed(server, 10);
        server.publish(&bytes, &sig);
        let state = refresh_from(&client(), &source, now()).await;
        assert_eq!(state.error, None);
        assert_eq!(state.index.as_ref().map(|i| i.sequence), Some(10));
        source
    }

    #[tokio::test]
    async fn valid_index_is_cached() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let source = source_for(&dev_settings(&server), &[], root.path()).unwrap();
        assert_eq!(source.cache, root.path().join("index").join("test"), "a test index is cached apart");
        assert_eq!(source.package_prefix, format!("http://{}/packages/", server.addr));
        assert_eq!(source.sig_url, format!("{}.sig", server.url()));

        let (bytes, sig) = signed(&server, 10);
        server.publish(&bytes, &sig);
        let state = refresh_from(&client(), &source, now()).await;
        assert_eq!(state.error, None);
        assert_eq!(state.freshness, Some(Freshness::Current));
        assert_eq!(state.checked, Some(now()));
        assert_eq!(state.index.as_ref().unwrap().entry("hello").unwrap().version, "1.2.3");
        assert_eq!(std::fs::read(source.cache.join("index.json")).unwrap(), bytes);
        assert_eq!(std::fs::read_to_string(source.cache.join("index.json.sig")).unwrap(), sig);
        assert_eq!(sequence_file(&source.cache), 10);
        assert!(!root.path().join("index").join("index.json").exists(), "the official cache is untouched");
        let leftovers: Vec<_> = std::fs::read_dir(&source.cache).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 3, "no temporary files: {leftovers:?}");

        // An expired index is still cached (it can revoke), with its freshness.
        let expired = index_json(&server, 11, "2026-11-14T00:00:00Z");
        let expired_sig = aio_plugin_index::sign(&expired, &SECRET);
        server.publish(&expired, &expired_sig);
        let state = refresh_from(&client(), &source, now()).await;
        assert_eq!(state.error, None);
        assert_eq!(state.freshness, Some(Freshness::Expired));
        assert_eq!(std::fs::read(source.cache.join("index.json")).unwrap(), expired);
        assert_eq!(sequence_file(&source.cache), 11);
        assert_eq!(load_cache_from(&source, now()).freshness, Some(Freshness::Expired));
    }

    #[tokio::test]
    async fn bad_signature_keeps_cache() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let source = accepted(&server, root.path()).await;
        let cached = std::fs::read(source.cache.join("index.json")).unwrap();

        // Signed by a key that is not trusted.
        let newer = index_json(&server, 11, "2026-12-13T10:00:00Z");
        server.publish(&newer, &aio_plugin_index::sign(&newer, &OTHER));
        let state = refresh_from(&client(), &source, now()).await;
        assert!(state.error.as_deref().is_some_and(|e| e.contains("unknown key")), "{:?}", state.error);
        assert_eq!(state.index.as_ref().map(|i| i.sequence), Some(10), "the cached index stays");
        assert_eq!(state.freshness, Some(Freshness::Current));

        // Bytes changed after signing.
        let (good, sig) = signed(&server, 11);
        let mut tampered = good.clone();
        tampered.extend_from_slice(b" ");
        server.publish(&tampered, &sig);
        let state = refresh_from(&client(), &source, now()).await;
        assert!(state.error.as_deref().is_some_and(|e| e.contains("signature is not valid")), "{:?}", state.error);
        assert_eq!(state.index.as_ref().map(|i| i.sequence), Some(10));

        assert_eq!(std::fs::read(source.cache.join("index.json")).unwrap(), cached);
        assert_eq!(sequence_file(&source.cache), 10);

        // A key removed from the program invalidates the cache.
        let without_key = IndexSource { keys: vec![], ..source.clone() };
        let state = load_cache_from(&without_key, now());
        assert!(state.index.is_none() && state.error.is_some(), "{state:?}");
        // A cache whose bytes were changed is not used.
        std::fs::write(source.cache.join("index.json"), &tampered).unwrap();
        assert!(load_cache_from(&source, now()).index.is_none());
    }

    #[tokio::test]
    async fn cached_sequence_survives_restart() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let source = accepted(&server, root.path()).await;

        // A restart: the cache is loaded and verified again.
        let restarted = load_cache_from(&source, now());
        assert_eq!(restarted.error, None);
        assert_eq!(restarted.index.as_ref().map(|i| i.sequence), Some(10));
        assert_eq!(restarted.freshness, Some(Freshness::Current));

        // An older, validly signed index is refused and not cached.
        let (older, sig) = signed(&server, 9);
        server.publish(&older, &sig);
        let state = refresh_from(&client(), &source, now()).await;
        assert_eq!(state.error.as_deref(), Some(ROLLBACK));
        assert_eq!(state.index.as_ref().map(|i| i.sequence), Some(10));

        // Even with the cached index gone, state.json still knows 10.
        std::fs::remove_file(source.cache.join("index.json")).unwrap();
        std::fs::remove_file(source.cache.join("index.json.sig")).unwrap();
        let state = refresh_from(&client(), &source, now()).await;
        assert_eq!(state.error.as_deref(), Some(ROLLBACK));
        assert!(state.index.is_none());
        assert!(!source.cache.join("index.json").exists());
        assert_eq!(sequence_file(&source.cache), 10);

        // The same sequence again is fine.
        let (same, sig) = signed(&server, 10);
        server.publish(&same, &sig);
        assert_eq!(refresh_from(&client(), &source, now()).await.error, None);
    }

    #[tokio::test]
    async fn fetch_failure_keeps_cache() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let source = accepted(&server, root.path()).await;
        let cached = std::fs::read(source.cache.join("index.json")).unwrap();
        let keeps = |state: &IndexState| {
            assert!(state.error.is_some(), "{state:?}");
            assert_eq!(state.index.as_ref().map(|i| i.sequence), Some(10));
        };

        server.set("/index.json", 500, vec![]);
        let state = refresh_from(&client(), &source, now()).await;
        keeps(&state);
        assert!(state.error.as_deref().unwrap().contains("HTTP 500"), "{:?}", state.error);

        // Larger than 4 MiB.
        server.set("/index.json", 200, vec![b' '; MAX_INDEX_BYTES + 1]);
        keeps(&refresh_from(&client(), &source, now()).await);

        // A .sig larger than 4 KiB.
        let (bytes, _) = signed(&server, 11);
        server.publish(&bytes, &"A".repeat(MAX_SIG_BYTES + 1));
        keeps(&refresh_from(&client(), &source, now()).await);

        // Redirects are not followed, even to the same host.
        let (bytes, sig) = signed(&server, 11);
        server.publish(&bytes, &sig);
        server.set("/moved.json", 200, bytes.clone());
        server.routes.lock().insert("/index.json".into(), (302, vec![("Location".into(), "/moved.json".into())], vec![]));
        keeps(&refresh_from(&client(), &source, now()).await);

        // Nobody listening.
        let gone = IndexSource { index_url: "http://127.0.0.1:1/index.json".into(), sig_url: "http://127.0.0.1:1/index.json.sig".into(), ..source.clone() };
        keeps(&refresh_from(&client(), &gone, now()).await);

        assert_eq!(std::fs::read(source.cache.join("index.json")).unwrap(), cached);
        assert_eq!(sequence_file(&source.cache), 10);
    }

    /// A client whose every request goes to `server` (as a proxy), so any request at all is counted.
    fn trap(server: &Server) -> reqwest::Client {
        reqwest::Client::builder().proxy(reqwest::Proxy::all(format!("http://{}", server.addr)).unwrap()).build().unwrap()
    }

    #[tokio::test]
    async fn no_keys_means_not_set_up() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        assert!(OFFICIAL_INDEX_KEYS.len() <= 2);
        let settings = InstallerSettings::default();
        assert_eq!(source_for(&settings, &[], root.path()), Err(NOT_SET_UP.to_string()));
        let state = refresh_in(&trap(&server), &settings, &[], root.path(), now()).await;
        assert_eq!(state, IndexState { error: Some(NOT_SET_UP.to_string()), ..IndexState::default() });
        assert_eq!(server.hits(), 0, "nothing is fetched");
        assert!(!root.path().join("index").exists());

        // With a key, the official urls, prefix and cache.
        let key = public(&SECRET);
        let (id, b64) = key.split_once(':').unwrap();
        let official = source_for(&settings, &[(id, b64)], root.path()).unwrap();
        assert_eq!(official.index_url, OFFICIAL_INDEX_URL);
        assert_eq!(official.sig_url, format!("{OFFICIAL_INDEX_URL}.sig"));
        assert_eq!(official.package_prefix, OFFICIAL_PACKAGE_PREFIX);
        assert_eq!(official.keys, vec![(id.to_string(), b64.to_string())]);
        assert_eq!(official.cache, root.path().join("index"));
    }

    #[tokio::test]
    async fn test_url_ignored_without_developer_mode() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let settings = InstallerSettings { developer_mode: false, ..dev_settings(&server) };
        let (bytes, sig) = signed(&server, 10);
        server.publish(&bytes, &sig);

        assert_eq!(source_for(&settings, &[], root.path()), Err(NOT_SET_UP.to_string()));
        let state = refresh_in(&client(), &settings, &[], root.path(), now()).await;
        assert_eq!(state.error.as_deref(), Some(NOT_SET_UP));
        assert!(state.index.is_none());
        assert_eq!(server.hits(), 0, "the test url is never fetched");

        // With official keys, the official index is used, not the test one.
        let key = public(&OTHER);
        let (id, b64) = key.split_once(':').unwrap();
        let source = source_for(&settings, &[(id, b64)], root.path()).unwrap();
        assert_eq!(source.index_url, OFFICIAL_INDEX_URL);
        assert_eq!(source.keys, vec![(id.to_string(), b64.to_string())], "the test keys are not trusted");

        // In developer mode the test url must be https, or http on this computer.
        for bad in ["http://example.com/index.json", "ftp://127.0.0.1/index.json", "https://example.com/index.json?x=1", "not a url"] {
            let s = InstallerSettings { plugin_index_url: Some(bad.into()), ..dev_settings(&server) };
            assert!(source_for(&s, &[], root.path()).is_err(), "{bad}");
        }
        let https = InstallerSettings { plugin_index_url: Some("https://example.com/test/index.json".into()), ..dev_settings(&server) };
        assert_eq!(source_for(&https, &[], root.path()).unwrap().package_prefix, "https://example.com/test/packages/");
        let no_keys = InstallerSettings { plugin_index_test_keys: vec![], ..dev_settings(&server) };
        assert!(source_for(&no_keys, &[], root.path()).is_err());
        let bad_key = InstallerSettings { plugin_index_test_keys: vec!["nocolon".into()], ..dev_settings(&server) };
        assert!(source_for(&bad_key, &[], root.path()).is_err());
        assert_eq!(server.hits(), 0);
    }
}
