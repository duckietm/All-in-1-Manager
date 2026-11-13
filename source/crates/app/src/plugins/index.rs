//! The official signed plugin index: fetched from [`OFFICIAL_INDEX_URL`] (and its `.sig`), verified with
//! [`OFFICIAL_INDEX_KEYS`], checked for rollback against the highest `sequence` ever accepted, and cached in
//! configuration/plugins/index/ (`index.json`, `index.json.sig`, `state.json`). The cache is verified again every time
//! it is loaded, so a key removed from the program invalidates it.
//!
//! Developer mode only: the settings `plugin_index_url` and `plugin_index_test_keys` replace the official url and
//! keys (see [`feed_for`]); that index is cached apart, in configuration/plugins/index/test/, so it never mixes with
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

/// Which index a state belongs to: the official one, or the developer-mode test one (never "Verified by the
/// official index", and never served while developer mode is off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexSource {
    #[default]
    Official,
    Test,
}

/// What the Manager knows of the index.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexState {
    pub source: IndexSource,
    /// The verified index (from this run's check, else from the cache); never one that is a rollback.
    pub index: Option<Index>,
    pub freshness: Option<Freshness>,
    /// Why the last check failed (English, already passed through `display_safe`; fixed texts such as
    /// [`NOT_SET_UP`] stay equal to their constant, so `t()` translates them). The index above is kept.
    pub error: Option<String>,
    /// When the index was last fetched and accepted in this run.
    pub checked: Option<DateTime<Utc>>,
}

/// Longest error text kept in [`IndexState::error`].
const MAX_ERROR_CHARS: usize = 300;

/// Error text that may carry server or index supplied parts, made safe to show.
fn safe(text: &str) -> String {
    aio_plugin_host::display_safe(text, MAX_ERROR_CHARS)
}

/// The index that may be used to install or update: only a [`Freshness::Current`] one, and a test one only while
/// developer mode is on.
pub fn installable(state: &IndexState, developer_mode: bool) -> Option<&Index> {
    let allowed = match state.source {
        IndexSource::Official => true,
        IndexSource::Test => developer_mode,
    };
    state.index.as_ref().filter(|_| allowed && state.freshness == Some(Freshness::Current))
}

/// What may be shown: a test state is dropped (an empty official state instead) while developer mode is off.
fn served(state: &IndexState, developer_mode: bool) -> IndexState {
    if state.source == IndexSource::Test && !developer_mode {
        IndexState::default()
    } else {
        state.clone()
    }
}

/// Where an index comes from, the keys it must be signed with and where it is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFeed {
    pub kind: IndexSource,
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
pub fn feed_for(settings: &InstallerSettings, official_keys: &[(&str, &str)], plugins_root: &Path) -> Result<IndexFeed, String> {
    let cache = plugins_root.join("index");
    if let Some(url) = test_url(settings) {
        return test_source(url, &settings.plugin_index_test_keys, cache.join("test"));
    }
    if official_keys.is_empty() {
        return Err(NOT_SET_UP.to_string());
    }
    Ok(IndexFeed {
        kind: IndexSource::Official,
        index_url: OFFICIAL_INDEX_URL.to_string(),
        sig_url: format!("{OFFICIAL_INDEX_URL}.sig"),
        package_prefix: OFFICIAL_PACKAGE_PREFIX.to_string(),
        keys: official_keys.iter().map(|(id, key)| (id.to_string(), key.to_string())).collect(),
        cache,
    })
}

/// The test url, when the settings choose the test index (developer mode on and a url set).
fn test_url(settings: &InstallerSettings) -> Option<&str> {
    settings.plugin_index_url.as_deref().map(str::trim).filter(|u| settings.developer_mode && !u.is_empty())
}

/// Which index the settings choose, also when it cannot be fetched.
fn source_kind(settings: &InstallerSettings) -> IndexSource {
    if test_url(settings).is_some() {
        IndexSource::Test
    } else {
        IndexSource::Official
    }
}

/// The state when the settings' index cannot be used at all (`error` from [`feed_for`]).
fn unusable(settings: &InstallerSettings, error: &str) -> IndexState {
    IndexState { source: source_kind(settings), error: Some(safe(error)), ..IndexState::default() }
}

/// The developer-mode test index: https anywhere, or http on 127.0.0.1 / localhost; no query, fragment or login.
fn test_source(url: &str, keys: &[String], cache: PathBuf) -> Result<IndexFeed, String> {
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
    Ok(IndexFeed { kind: IndexSource::Test, sig_url: format!("{index_url}.sig"), index_url, package_prefix, keys, cache })
}

impl IndexFeed {
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
pub fn load_cache_from(source: &IndexFeed, now: DateTime<Utc>) -> IndexState {
    let dir = &source.cache;
    let empty = IndexState { source: source.kind, ..IndexState::default() };
    let index_path = dir.join("index.json");
    let sig_path = dir.join("index.json.sig");
    let Ok(meta) = std::fs::metadata(&index_path) else {
        return empty;
    };
    let invalid = |why: &str| IndexState { error: Some(safe(&format!("The saved plugin index is not valid: {why}"))), ..empty.clone() };
    if meta.len() > MAX_INDEX_BYTES as u64 || std::fs::metadata(&sig_path).is_ok_and(|m| m.len() > MAX_SIG_BYTES as u64) {
        return invalid("it is too large");
    }
    let (Ok(bytes), Ok(sig)) = (std::fs::read(&index_path), std::fs::read_to_string(&sig_path)) else {
        return invalid("it cannot be read");
    };
    match verify_and_parse(&bytes, &sig, &source.key_refs(), &source.package_prefix) {
        Ok((index, _)) => match check_fresh(&index, stored_sequence(dir), now) {
            Freshness::Rollback => IndexState { error: Some(ROLLBACK.to_string()), ..empty },
            fresh => IndexState { index: Some(index), freshness: Some(fresh), ..empty },
        },
        Err(e) => invalid(&format!("{e:#}")),
    }
}

/// Fetches, verifies and checks the index of `source`; on success caches it and the new sequence. On any failure
/// the cached index stays and `error` says why.
pub async fn refresh_from(client: &reqwest::Client, source: &IndexFeed, now: DateTime<Utc>) -> IndexState {
    let cached = load_cache_from(source, now);
    match fetch_verified(client, source, now, cached.index.as_ref()).await {
        Ok(state) => state,
        Err(error) => IndexState { error: Some(safe(&error)), ..cached },
    }
}

async fn fetch_verified(client: &reqwest::Client, source: &IndexFeed, now: DateTime<Utc>, cached: Option<&Index>) -> Result<IndexState, String> {
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
    let error = write_cache(&source.cache, &index_bytes, &sig, sequence).err().map(|e| safe(&format!("The plugin index could not be saved: {e}")));
    Ok(IndexState { source: source.kind, index: Some(index), freshness: Some(freshness), error, checked: Some(now) })
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

/// Writes the three cache files, each to a temporary file then renamed over the old one. `state.json` (the new
/// sequence) goes first: a crash or a failure after it leaves the sequence raised and, at worst, a cache older than
/// it (refused as a rollback until the next fetch) or one that no longer verifies; never a lower floor.
fn write_cache(dir: &Path, index: &[u8], sig: &str, sequence: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let state = serde_json::to_vec(&Stored { sequence }).map_err(std::io::Error::other)?;
    write_atomic(&dir.join("state.json"), &state)?;
    write_atomic(&dir.join("index.json"), index)?;
    write_atomic(&dir.join("index.json.sig"), sig.as_bytes())
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

/// The cached index of the source `settings` choose, verified again with that source's keys.
fn load_for(settings: &InstallerSettings, official_keys: &[(&str, &str)], plugins_root: &Path, now: DateTime<Utc>) -> IndexState {
    match feed_for(settings, official_keys, plugins_root) {
        Ok(feed) => load_cache_from(&feed, now),
        Err(e) => unusable(settings, &e),
    }
}

/// [`refresh_from`] the source the settings choose; [`NOT_SET_UP`] (and no request) when there is none.
async fn refresh_in(
    client: &reqwest::Client,
    settings: &InstallerSettings,
    official_keys: &[(&str, &str)],
    plugins_root: &Path,
    now: DateTime<Utc>,
) -> IndexState {
    match feed_for(settings, official_keys, plugins_root) {
        Ok(feed) => refresh_from(client, &feed, now).await,
        Err(e) => unusable(settings, &e),
    }
}

/// The index state and the developer settings it was loaded for. The developer settings held here are the ones that
/// count: [`IndexStore::settings_changed`] is the only way to change them, and a test state is never served while
/// they say developer mode is off.
pub struct IndexStore {
    root: PathBuf,
    official_keys: Vec<(String, String)>,
    inner: Mutex<Shared>,
}

struct Shared {
    state: IndexState,
    settings: InstallerSettings,
}

impl IndexStore {
    /// A store loaded from the cache that `settings` choose; `root` is configuration/plugins/.
    pub fn new(root: PathBuf, official_keys: &[(&str, &str)], settings: &InstallerSettings, now: DateTime<Utc>) -> IndexStore {
        let official_keys: Vec<(String, String)> = official_keys.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let store = IndexStore { root, official_keys, inner: Mutex::new(Shared { state: IndexState::default(), settings: settings.clone() }) };
        let state = load_for(settings, &store.keys(), &store.root, now);
        store.inner.lock().state = state;
        store
    }

    fn keys(&self) -> Vec<(&str, &str)> {
        self.official_keys.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }

    /// The developer settings changed (developer mode, the test url or keys): the state in memory is dropped and the
    /// cache of the index the new settings choose is loaded instead.
    pub fn settings_changed(&self, settings: &InstallerSettings, now: DateTime<Utc>) {
        let state = load_for(settings, &self.keys(), &self.root, now);
        let mut inner = self.inner.lock();
        inner.settings = settings.clone();
        inner.state = state;
    }

    /// What the Manager knows of the index now; never a test state while developer mode is off.
    pub fn state(&self) -> IndexState {
        let inner = self.inner.lock();
        served(&inner.state, inner.settings.developer_mode)
    }

    /// The index to install and update from: see [`installable`].
    pub fn for_install(&self) -> Option<Index> {
        let inner = self.inner.lock();
        installable(&inner.state, inner.settings.developer_mode).cloned()
    }

    /// Fetches the index of the settings held here and keeps the result, unless the settings changed meanwhile.
    pub async fn refresh(&self, client: &reqwest::Client, now: DateTime<Utc>) -> IndexState {
        let settings = self.inner.lock().settings.clone();
        let state = refresh_in(client, &settings, &self.keys(), &self.root, now).await;
        let mut inner = self.inner.lock();
        if source_kind(&inner.settings) == state.source && inner.settings == settings {
            inner.state = state;
        }
        served(&inner.state, inner.settings.developer_mode)
    }
}

/// The process-wide store, loaded from the cache the saved settings choose the first time it is needed.
fn store() -> &'static IndexStore {
    static STORE: OnceLock<IndexStore> = OnceLock::new();
    STORE.get_or_init(|| IndexStore::new(super::packs::root(), OFFICIAL_INDEX_KEYS, &InstallerSettings::load(), Utc::now()))
}

static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

fn repaint() {
    if let Some(repaint) = REPAINT.get() {
        repaint();
    }
}

/// Called after every [`refresh`] (the window asks egui to repaint). Set once; later calls are ignored.
pub fn set_repaint(repaint: impl Fn() + Send + Sync + 'static) {
    let _ = REPAINT.set(Box::new(repaint));
}

/// What the Manager knows of the index now; never a test index while developer mode is off.
pub fn index_state() -> IndexState {
    store().state()
}

/// The index to install and update from: an official one that is [`Freshness::Current`], or a current test one
/// while developer mode is on. `None` otherwise.
pub fn index_for_install() -> Option<Index> {
    store().for_install()
}

/// The developer settings were changed (developer mode switched, or the test url or keys edited): call with the new
/// settings, saved or not. The index state follows them at once.
pub fn developer_settings_changed(settings: &InstallerSettings) {
    store().settings_changed(settings, Utc::now());
    repaint();
}

/// The cached index of the source the current settings choose, verified again with that source's keys.
pub fn load_cache() -> IndexState {
    let settings = store().inner.lock().settings.clone();
    served(&load_for(&settings, OFFICIAL_INDEX_KEYS, &super::packs::root(), Utc::now()), settings.developer_mode)
}

/// Fetches and checks the index the settings choose (use [`index_client`]), updates [`index_state`] and asks for a
/// repaint. Never touches the network when the index is not set up.
pub async fn refresh(client: &reqwest::Client) -> IndexState {
    let state = store().refresh(client, Utc::now()).await;
    repaint();
    state
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
    async fn accepted(server: &Server, root: &Path) -> IndexFeed {
        let source = feed_for(&dev_settings(server), &[], root).unwrap();
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
        let source = feed_for(&dev_settings(&server), &[], root.path()).unwrap();
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
        let without_key = IndexFeed { keys: vec![], ..source.clone() };
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
        let gone = IndexFeed { index_url: "http://127.0.0.1:1/index.json".into(), sig_url: "http://127.0.0.1:1/index.json.sig".into(), ..source.clone() };
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
        assert_eq!(feed_for(&settings, &[], root.path()), Err(NOT_SET_UP.to_string()));
        let state = refresh_in(&trap(&server), &settings, &[], root.path(), now()).await;
        assert_eq!(state, IndexState { error: Some(NOT_SET_UP.to_string()), ..IndexState::default() });
        assert_eq!(server.hits(), 0, "nothing is fetched");
        assert!(!root.path().join("index").exists());

        // With a key, the official urls, prefix and cache.
        let key = public(&SECRET);
        let (id, b64) = key.split_once(':').unwrap();
        let official = feed_for(&settings, &[(id, b64)], root.path()).unwrap();
        assert_eq!(official.index_url, OFFICIAL_INDEX_URL);
        assert_eq!(official.sig_url, format!("{OFFICIAL_INDEX_URL}.sig"));
        assert_eq!(official.package_prefix, OFFICIAL_PACKAGE_PREFIX);
        assert_eq!(official.keys, vec![(id.to_string(), b64.to_string())]);
        assert_eq!(official.cache, root.path().join("index"));
    }

    /// An official cache under `root`, signed with `secret`, of sequence `sequence`.
    fn official_cache(root: &Path, secret: &[u8; 32], sequence: u64) {
        let index = serde_json::to_vec(&json!({
            "schema": 1, "sequence": sequence, "generated": "2026-11-13T10:00:00Z", "expires": "2026-12-13T10:00:00Z",
            "plugins": [], "revoked": []
        }))
        .unwrap();
        let dir = root.join("index");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), &index).unwrap();
        std::fs::write(dir.join("index.json.sig"), aio_plugin_index::sign(&index, secret)).unwrap();
        std::fs::write(dir.join("state.json"), format!("{{\"sequence\":{sequence}}}")).unwrap();
    }

    #[tokio::test]
    async fn turning_developer_mode_off_drops_test_index() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        official_cache(root.path(), &OTHER, 3);
        let official_key = public(&OTHER);
        let (id, b64) = official_key.split_once(':').unwrap();
        let keys = [(id, b64)];

        let dev = dev_settings(&server);
        let store = IndexStore::new(root.path().to_path_buf(), &keys, &dev, now());
        assert_eq!(store.state().source, IndexSource::Test);
        assert!(store.state().index.is_none(), "no test cache yet");
        let (bytes, sig) = signed(&server, 10);
        server.publish(&bytes, &sig);
        let state = store.refresh(&client(), now()).await;
        assert_eq!((state.source, state.index.as_ref().map(|i| i.sequence)), (IndexSource::Test, Some(10)));
        assert_eq!(store.for_install().map(|i| i.sequence), Some(10), "a test index installs in developer mode");

        // Developer mode off: the official cache at once, without a refresh.
        let off = InstallerSettings { developer_mode: false, ..dev.clone() };
        store.settings_changed(&off, now());
        let state = store.state();
        assert_eq!((state.source, state.index.as_ref().map(|i| i.sequence)), (IndexSource::Official, Some(3)));
        assert_eq!(store.for_install().map(|i| i.sequence), Some(3));

        // On again: the test cache comes back (verified again).
        store.settings_changed(&dev, now());
        assert_eq!(store.state().index.map(|i| i.sequence), Some(10));
        // The test keys changed: the test cache no longer verifies.
        let other_keys = InstallerSettings { plugin_index_test_keys: vec![public(&OTHER)], ..dev.clone() };
        store.settings_changed(&other_keys, now());
        assert!(store.state().index.is_none() && store.for_install().is_none());

        // A refresh started in developer mode and finished after it was turned off is not kept.
        store.settings_changed(&dev, now());
        let http = client();
        let refreshing = store.refresh(&http, now());
        tokio::pin!(refreshing);
        // Started: the settings are taken and the request is on its way.
        assert!(futures::poll!(&mut refreshing).is_pending());
        store.settings_changed(&off, now());
        let state = refreshing.await;
        assert_eq!(state.source, IndexSource::Official);
        assert_eq!(store.state().index.map(|i| i.sequence), Some(3));
    }

    #[tokio::test]
    async fn test_index_never_served_as_official() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let feed = feed_for(&dev_settings(&server), &[], root.path()).unwrap();
        assert_eq!(feed.kind, IndexSource::Test);
        let (bytes, sig) = signed(&server, 10);
        server.publish(&bytes, &sig);
        let test = refresh_from(&client(), &feed, now()).await;
        assert_eq!(test.source, IndexSource::Test);
        assert_eq!(load_cache_from(&feed, now()).source, IndexSource::Test);

        // Never shown nor installable with developer mode off; installable with it on.
        assert_eq!(served(&test, false), IndexState::default());
        assert!(installable(&test, false).is_none());
        assert_eq!(installable(&test, true).map(|i| i.sequence), Some(10));
        assert_eq!(served(&test, true), test);
        // A test state that sneaked into the store is not served once developer mode is off.
        let store = IndexStore::new(root.path().to_path_buf(), &[], &InstallerSettings::default(), now());
        store.inner.lock().state = test.clone();
        assert_eq!(store.state(), IndexState::default());
        assert!(store.for_install().is_none());

        // An official state: only a current one installs.
        let official = IndexState { source: IndexSource::Official, ..test.clone() };
        assert_eq!(installable(&official, false).map(|i| i.sequence), Some(10));
        let expired = IndexState { freshness: Some(Freshness::Expired), ..official.clone() };
        assert!(installable(&expired, true).is_none());
        // The official cache never reads the test one.
        let key = public(&SECRET);
        let (id, b64) = key.split_once(':').unwrap();
        let official_feed = feed_for(&InstallerSettings::default(), &[(id, b64)], root.path()).unwrap();
        assert_eq!(official_feed.kind, IndexSource::Official);
        assert!(load_cache_from(&official_feed, now()).index.is_none());

        // Errors are made safe to show.
        server.set("/index.json", 200, "\x1b[31m\u{202e}not json".as_bytes().to_vec());
        server.set("/index.json.sig", 200, b"bad\x07sig".to_vec());
        let state = refresh_from(&client(), &feed, now()).await;
        let error = state.error.unwrap();
        assert!(!error.chars().any(|c| c.is_control() || c == '\u{202e}'), "{error:?}");
        assert!(error.chars().count() <= MAX_ERROR_CHARS);
        let raw = format!("x\u{202e}\x1b[31m{}", "y".repeat(1000));
        let shown = safe(&raw);
        assert!(!shown.chars().any(|c| c.is_control() || c == '\u{202e}') && shown.chars().count() <= MAX_ERROR_CHARS, "{shown:?}");
        assert_eq!(safe(NOT_SET_UP), NOT_SET_UP, "fixed texts stay translatable");
    }

    #[tokio::test]
    async fn test_url_ignored_without_developer_mode() {
        let server = Server::start().await;
        let root = tempfile::tempdir().unwrap();
        let settings = InstallerSettings { developer_mode: false, ..dev_settings(&server) };
        let (bytes, sig) = signed(&server, 10);
        server.publish(&bytes, &sig);

        assert_eq!(feed_for(&settings, &[], root.path()), Err(NOT_SET_UP.to_string()));
        let state = refresh_in(&client(), &settings, &[], root.path(), now()).await;
        assert_eq!(state.error.as_deref(), Some(NOT_SET_UP));
        assert!(state.index.is_none());
        assert_eq!(server.hits(), 0, "the test url is never fetched");

        // With official keys, the official index is used, not the test one.
        let key = public(&OTHER);
        let (id, b64) = key.split_once(':').unwrap();
        let source = feed_for(&settings, &[(id, b64)], root.path()).unwrap();
        assert_eq!(source.index_url, OFFICIAL_INDEX_URL);
        assert_eq!(source.keys, vec![(id.to_string(), b64.to_string())], "the test keys are not trusted");

        // In developer mode the test url must be https, or http on this computer.
        for bad in ["http://example.com/index.json", "ftp://127.0.0.1/index.json", "https://example.com/index.json?x=1", "not a url"] {
            let s = InstallerSettings { plugin_index_url: Some(bad.into()), ..dev_settings(&server) };
            assert!(feed_for(&s, &[], root.path()).is_err(), "{bad}");
        }
        let https = InstallerSettings { plugin_index_url: Some("https://example.com/test/index.json".into()), ..dev_settings(&server) };
        assert_eq!(feed_for(&https, &[], root.path()).unwrap().package_prefix, "https://example.com/test/packages/");
        let no_keys = InstallerSettings { plugin_index_test_keys: vec![], ..dev_settings(&server) };
        assert!(feed_for(&no_keys, &[], root.path()).is_err());
        let bad_key = InstallerSettings { plugin_index_test_keys: vec!["nocolon".into()], ..dev_settings(&server) };
        assert!(feed_for(&bad_key, &[], root.path()).is_err());
        assert_eq!(server.hits(), 0);
    }
}
