//! The rip: connect to G-Earth, wait for the hotel connection, pick the hotel profile (or learn
//! the header ids from the client), read the catalogue indexes and pages, write the output
//! folder. Also [`reexport`]: the output again from a rip's recorded packets.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::diff::{self, CatalogDiff};
use crate::export::{self, ItemsBase, RippedCatalog};
use crate::extras;
use crate::furnidata::{self, Furnidata};
use crate::missing::{self, MissingFurni};
use crate::productdata::{self, Productdata};
use crate::gearth::{self, ConnectionInfo, Event, ExtensionInfo, Session};
use crate::images;
use crate::learn::Learner;
use crate::manager::{self, ManagerExport, ManagerMode, ManagerStats};
use crate::packets::{self, CatalogIndex, CatalogPage, Headers};
use crate::profiles::{self, DetectedBy, HeaderMap, HotelKind, HotelProfile};
use crate::recording::{self, Recorder, SessionInfo};
use crate::{
    Ask, Cancelled, ItemsSource, Log, PartialOutput, Progress, RipEvent, RipHooks, RipProgress,
};

/// The ripper's settings (the app keeps them; every field has a default).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RipOptions {
    /// The folder the `<hotel>_<timestamp>` output folder is made in.
    pub output_folder: PathBuf,
    /// Time between two requests to the server, in ms (at least 50).
    pub delay_ms: u64,
    /// How long to wait for an answer before retrying / giving up, in ms (at least 200).
    pub timeout_ms: u64,
    /// Header ids that win over the profile's and G-Earth's.
    pub header_overrides: HeaderMap,
    pub download_images: bool,
    /// Rip the normal catalogue.
    pub normal: bool,
    /// Rip the Builders Club catalogue (when the hotel profile says the hotel has one).
    pub builders_club: bool,
    /// G-Earth extension ports to try, in order (G-Earth binds the first free one from 9092).
    pub ports: Vec<u16>,
    /// Where G-Earth is (or gets downloaded to, after asking) when it is not running. `None`:
    /// never offer to start or download it.
    pub gearth_folder: Option<PathBuf>,
    /// How long to wait for G-Earth to be running, in ms.
    pub gearth_wait_ms: u64,
    /// How long to wait for the hotel connection, the login and a first answer, in ms.
    pub hotel_wait_ms: u64,
    /// While waiting for a first answer, how often to ask again, in ms.
    pub resend_every_ms: u64,
    /// Catalogue image URL (…/c_images/catalogue); wins over the profile's.
    pub catalog_url: Option<String>,
    /// Page icon URL prefix (`<prefix><n>.png`); wins over the profile's.
    pub icon_url: Option<String>,
    /// Keep the raw packets in `<folder>/packets/` (needed to resume and re-export).
    pub record_packets: bool,
    /// Go on with a stopped rip: its folder (the [`PartialOutput`] folder). Only the pages it
    /// lacks are asked; the output is written into that folder again.
    pub resume_from: Option<PathBuf>,
    /// Match products by class name through the source hotel's furnidata.
    pub use_furnidata: bool,
    /// The source hotel's furnidata (URL or file); wins over the profile's. Official Habbo
    /// hotels need none.
    pub furnidata_url: Option<String>,
    /// How catalog_manager.sql changes the hotel's catalogue.
    pub manager_mode: ManagerMode,
    /// When no source has every header id, learn them from the client while the user opens the
    /// catalogue (instead of stopping).
    pub learn_headers: bool,
    /// Where learned ids are saved as a hotel profile (the folder [`profiles::profiles`] reads).
    pub profiles_folder: Option<PathBuf>,
    /// Slow down when pages go unanswered (and speed up again when they answer).
    pub adaptive_delay: bool,
    /// The slowest the adaptive delay goes, in ms.
    pub max_delay_ms: u64,
    /// The source hotel's productdata (URL or file): product names for the reports. Official
    /// Habbo hotels need none.
    pub productdata_url: Option<String>,
    /// Read only the catalogue indexes (the page trees), no pages: for choosing branches, then
    /// [`RipOptions::only_pages`] with [`RipOptions::resume_from`] set to this rip's folder.
    pub index_only: bool,
    /// Rip only these branches: per catalogue (NORMAL / BUILDERS_CLUB), page ids whose pages and
    /// every page below them are asked; a catalogue not listed is not ripped. `None`: everything.
    pub only_pages: Option<BTreeMap<String, Vec<i32>>>,
    /// Compare with the newest earlier rip of the same hotel in the output folder (when it has
    /// recorded packets): catalog_changes.json and catalog_manager_changes.sql.
    pub diff_with_previous: bool,
    /// Compare with this earlier rip folder instead.
    pub diff_against: Option<PathBuf>,
    /// The changes file also deletes the pages and offers the hotel removed.
    pub diff_delete_removed: bool,
    /// Also read the club offers, the bundle discount ruleset and the gift wrapping
    /// configuration (catalog_extras.json).
    pub rip_extras: bool,
    /// Where the client's configuration is (Nitro site / config file, or Flash
    /// external_variables): the image URLs are read from it. Wins over the profile's.
    pub site_config_url: Option<String>,
    /// What G-Earth shows in its Extensions tab.
    #[serde(skip)]
    pub extension: ExtensionInfo,
}

impl Default for RipOptions {
    fn default() -> Self {
        Self {
            output_folder: PathBuf::from("Catalog"),
            delay_ms: 400,
            timeout_ms: 10_000,
            header_overrides: HeaderMap::default(),
            download_images: true,
            normal: true,
            builders_club: true,
            ports: gearth::default_ports(),
            gearth_folder: None,
            gearth_wait_ms: 5 * 60 * 1000,
            hotel_wait_ms: 10 * 60 * 1000,
            resend_every_ms: 20_000,
            catalog_url: None,
            icon_url: None,
            record_packets: true,
            resume_from: None,
            use_furnidata: true,
            furnidata_url: None,
            manager_mode: ManagerMode::Replace,
            learn_headers: true,
            profiles_folder: None,
            adaptive_delay: true,
            max_delay_ms: 5_000,
            productdata_url: None,
            index_only: false,
            only_pages: None,
            diff_with_previous: true,
            diff_against: None,
            diff_delete_removed: false,
            rip_extras: true,
            site_config_url: None,
            extension: ExtensionInfo::default(),
        }
    }
}

const DEFAULT_CATALOG_URL: &str = "https://images.habbo.com/c_images/catalogue";
const DEFAULT_ICON_URL: &str = "https://images.habbo.com/c_images/catalogue/icon_";
/// Answered requests in a row before the adaptive delay steps back down.
const SPEED_UP_AFTER: usize = 15;

/// The options as durations.
#[derive(Debug, Clone, Copy)]
struct Timing {
    delay: Duration,
    max_delay: Duration,
    response_timeout: Duration,
    gearth_wait: Duration,
    hotel_wait: Duration,
    resend_every: Duration,
}

impl Timing {
    fn of(options: &RipOptions) -> Self {
        let delay = Duration::from_millis(options.delay_ms.max(50));
        Self {
            delay,
            max_delay: Duration::from_millis(options.max_delay_ms).max(delay),
            response_timeout: Duration::from_millis(options.timeout_ms.max(200)),
            gearth_wait: Duration::from_millis(options.gearth_wait_ms),
            hotel_wait: Duration::from_millis(options.hotel_wait_ms),
            resend_every: Duration::from_millis(options.resend_every_ms.max(100)),
        }
    }
}

/// Which hotel profile to use.
#[derive(Debug, Clone)]
// Not boxed: callers build `Fixed(profile)` directly.
#[allow(clippy::large_enum_variant)]
pub enum ProfileChoice {
    /// Pick from these (e.g. [`profiles::profiles`]) by the connection's host, revision and
    /// client type; when none fits, G-Earth's packet info alone (or learned ids).
    Detect(Vec<HotelProfile>),
    /// Use this one.
    Fixed(HotelProfile),
}

/// What a finished rip wrote.
#[derive(Debug, Clone)]
pub struct RipResult {
    /// The output folder.
    pub folder: PathBuf,
    pub files: Vec<PathBuf>,
    /// Output name of the hotel (habbo.nl, or the retro's host).
    pub hotel: String,
    pub host: String,
    pub client_type: String,
    pub hotel_version: String,
    pub profile: String,
    pub kind: HotelKind,
    pub headers: Headers,
    /// Pages read (over every catalogue).
    pub pages: usize,
    pub pages_failed: usize,
    /// Offers read.
    pub items: usize,
    pub products: usize,
    pub frontpage_items: usize,
    pub images: images::ImageStats,
    pub sql: export::SqlStats,
    pub manager: ManagerStats,
    pub complete: bool,
    pub elapsed: Duration,
    /// Class names in the source furnidata that was used (0: none).
    pub furnidata_names: usize,
    /// The header ids were learned from the client.
    pub learned_headers: bool,
    /// Pages that came from the recording of a stopped rip (not asked again).
    pub resumed_pages: usize,
    /// The time between requests at the end (the adaptive delay may have raised it).
    pub delay_ms: u64,
    /// Product names in the source productdata that was used (0: none).
    pub productdata_names: usize,
    /// The products the hotel's items_base lacks (empty without items_base).
    pub missing_furni: Vec<MissingFurni>,
    /// What changed since the earlier rip it was compared with.
    pub changes: Option<CatalogDiff>,
    /// The page trees, per catalogue (for choosing branches after an index-only rip).
    pub indexes: BTreeMap<String, CatalogIndex>,
}

/// The pages of `index` at or below one of `roots`.
pub fn pages_below(index: &CatalogIndex, roots: &[i32]) -> HashSet<i32> {
    fn rec(node: &crate::packets::IndexNode, inside: bool, roots: &[i32], out: &mut HashSet<i32>) {
        for child in &node.children {
            let here = inside || (child.page_id > 0 && roots.contains(&child.page_id));
            if here && child.page_id > 0 {
                out.insert(child.page_id);
            }
            rec(child, here, roots, out);
        }
    }
    let mut out = HashSet::new();
    rec(&index.root, false, roots, &mut out);
    out
}

/// `Catalog/<hotel>` name for a game host: game-nl.habbo.com -> habbo.nl, game-us -> habbo.com,
/// game-br -> habbo.com.br, game-tr -> habbo.com.tr, anything else the host itself.
pub fn hotel_name(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    let code = host
        .strip_prefix("game-")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(_, domain)| domain.starts_with("habbo."))
        .map(|(code, _)| code);
    let name = match code {
        Some("us") => "habbo.com".to_string(),
        Some("br") => "habbo.com.br".to_string(),
        Some("tr") => "habbo.com.tr".to_string(),
        Some(code) => format!("habbo.{code}"),
        None if host.is_empty() => "hotel".to_string(),
        None => host,
    };
    images::safe_file_name(&name)
}

fn progress_label(text: &str) -> String {
    text.replace(['%', '[', ']', '\r', '\n'], " ")
}

/// 12345 -> "12,345".
fn n0(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

async fn sleep_or_cancel(duration: Duration, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Err(Cancelled.into()),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

// ---------------------------------------------------------------- connecting

async fn connect_gearth(
    options: &RipOptions,
    timing: &Timing,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<Session> {
    let log = hooks.log;
    let ports = if options.ports.is_empty() {
        gearth::default_ports()
    } else {
        options.ports.clone()
    };
    if let Some(folder) = &options.gearth_folder {
        if !crate::gearth_install::is_running(&ports).await {
            crate::gearth_install::offer_to_start(folder, log, hooks.ask, cancel).await?;
        }
    }
    let deadline = Instant::now() + timing.gearth_wait;
    let mut last_note = Instant::now() - Duration::from_secs(3600);
    let range = match (ports.first(), ports.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}-{b}"),
        (Some(a), _) => a.to_string(),
        _ => String::new(),
    };
    loop {
        for &port in &ports {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            if let Ok(session) =
                Session::connect(port, &options.extension, Duration::from_secs(3)).await
            {
                log(&format!("Connected to G-Earth on port {port}."));
                hooks.emit(RipEvent::GearthConnected { port });
                return Ok(session);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            bail!(
                "G-Earth was not found on 127.0.0.1:{range} within {} s. Start G-Earth first (its Extensions tab shows the port; set the port in the ripper settings if it is another one).",
                timing.gearth_wait.as_secs()
            );
        }
        if now.duration_since(last_note) >= Duration::from_secs(15) {
            last_note = now;
            log(&format!(
                "Waiting for G-Earth on 127.0.0.1:{range} ... ({} s left)",
                deadline.saturating_duration_since(now).as_secs()
            ));
        }
        sleep_or_cancel(
            Duration::from_millis(200)
                .max(deadline.saturating_duration_since(now).min(Duration::from_secs(2))),
            cancel,
        )
        .await?;
    }
}

/// G-Earth let the extension go after accepting it, before any hotel connection: it was
/// restarted, or its client mode changed (in Nitro mode it lets extensions go at every Connect).
/// The rip connects again, as long as the time to wait for the hotel lasts.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct GearthDropped(String);

async fn wait_for_hotel(
    session: &mut Session,
    options: &RipOptions,
    timing: &Timing,
    log: Log<'_>,
    cancel: &CancellationToken,
) -> Result<ConnectionInfo> {
    let deadline = Instant::now() + timing.hotel_wait;
    let mut accepted = false;
    let mut last_note = Instant::now();
    loop {
        let now = Instant::now();
        if now >= deadline {
            bail!("No hotel connection through G-Earth within {} s. Connect G-Earth to the hotel, then start your client and log in.", timing.hotel_wait.as_secs());
        }
        if now.duration_since(last_note) >= Duration::from_secs(30) {
            last_note = now;
            log(&format!("Still waiting for the hotel connection ({} s left): connect G-Earth, then open your client and log in.", deadline.saturating_duration_since(now).as_secs()));
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_secs(5));
        let event = tokio::select! {
            _ = cancel.cancelled() => return Err(Cancelled.into()),
            event = tokio::time::timeout(wait, session.next_event()) => event,
        };
        match event {
            Err(_) => continue,
            Ok(None) if accepted => return Err(GearthDropped("the connection ended".into()).into()),
            Ok(None) => bail!("The connection to G-Earth ended."),
            Ok(Some(Event::Init { connected })) => {
                accepted = true;
                log(&format!(
                    "G-Earth accepted the extension (shown in its Extensions tab as \"{}\").",
                    options.extension.title
                ));
                if !connected {
                    log("G-Earth is not connected to a hotel yet: press Connect in G-Earth, then open your client and log in.");
                }
            }
            Ok(Some(Event::ConnectionStart(info))) => return Ok(info),
            Ok(Some(Event::ConnectionEnd)) => {
                log("The hotel connection ended; waiting for a new one.")
            }
            Ok(Some(Event::Closed(reason))) => {
                if accepted {
                    return Err(GearthDropped(reason).into());
                }
                bail!(
                    "G-Earth refused the extension ({reason}). G-Earth asks once whether to allow an unknown extension; \
                     if \"No\" was remembered, restart G-Earth and answer Yes when it asks about \"{}\".",
                    options.extension.title
                );
            }
            Ok(Some(Event::Packet(_))) => {}
        }
    }
}

fn fallback_profile(connection: &ConnectionInfo) -> HotelProfile {
    let kind = if connection.client_type.eq_ignore_ascii_case("NITRO") {
        HotelKind::NitroRetro
    } else {
        HotelKind::FlashRetro
    };
    HotelProfile {
        name: connection.host.clone(),
        kind,
        hosts: Vec::new(),
        revisions: Vec::new(),
        headers: HeaderMap::default(),
        catalog_url: None,
        icon_url: None,
        furnidata_url: None,
        productdata_url: None,
        site_config_url: None,
        extra_headers: BTreeMap::new(),
        builders_club: true,
        notes: String::new(),
        builtin: false,
        file: None,
    }
}

/// The profile for this connection.
fn choose_profile(
    choice: ProfileChoice,
    connection: &ConnectionInfo,
    hooks: &RipHooks<'_>,
) -> HotelProfile {
    let log = hooks.log;
    let (profile, detected_by) = match choice {
        ProfileChoice::Fixed(profile) => {
            log(&format!("Hotel profile: {} ({}).", profile.name, profile.kind.label()));
            (profile, None)
        }
        ProfileChoice::Detect(list) => match profiles::detect(&list, connection) {
            Some((profile, by)) => {
                let how = match by {
                    DetectedBy::Host => format!("host {}", connection.host),
                    DetectedBy::Revision => format!("client revision {}", connection.hotel_version),
                    DetectedBy::ClientType => format!("{} client", connection.client_type),
                };
                log(&format!(
                    "Hotel profile: {} ({}), picked by the {how}.",
                    profile.name,
                    profile.kind.label()
                ));
                (profile.clone(), Some(by))
            }
            None => {
                log(&format!(
                    "No hotel profile matches {} ({} client {}): using G-Earth's packet info.",
                    connection.host, connection.client_type, connection.hotel_version
                ));
                (fallback_profile(connection), None)
            }
        },
    };
    hooks.emit(RipEvent::ProfileChosen {
        name: profile.name.clone(),
        kind: profile.kind,
        detected_by,
    });
    profile
}

/// Learns the missing ids from the client while the user opens the catalogue and a page.
async fn learn_headers(
    session: &mut Session,
    known: [Option<u16>; 4],
    timing: &Timing,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<Learner> {
    let log = hooks.log;
    let mut learner = Learner::new(known);
    let missing = learner.missing();
    log(&format!(
        "No hotel profile and no G-Earth packet info has the id of {}.",
        missing.join(", ")
    ));
    log(&format!(
        "Open the catalogue in your client now and click any page: the ripper learns the ids from it (waiting up to {} s).",
        timing.hotel_wait.as_secs()
    ));
    hooks.emit(RipEvent::LearningHeaders { missing });
    session.watch_all(true);
    let deadline = Instant::now() + timing.hotel_wait;
    let outcome = loop {
        if learner.headers().is_some() {
            break Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(anyhow!(
                "The header ids could not be learned within {} s (still missing: {}). Open the catalogue in your client and click a page while the ripper waits, or set the ids in the hotel profile.",
                timing.hotel_wait.as_secs(),
                learner.missing().join(", ")
            ));
        }
        let event = tokio::select! {
            _ = cancel.cancelled() => break Err(Cancelled.into()),
            event = tokio::time::timeout(deadline - now, session.next_event()) => event,
        };
        match event {
            Err(_) => {}
            Ok(None) => break Err(anyhow!("the connection to G-Earth ended")),
            Ok(Some(Event::Packet(p))) => {
                if let Some(name) = learner.observe(p.to_server, &p.packet) {
                    log(&format!("Learned {name} = {}.", p.packet.header));
                }
            }
            Ok(Some(Event::ConnectionEnd)) => {
                break Err(anyhow!("the hotel connection ended (logged out or disconnected)"))
            }
            Ok(Some(Event::Closed(reason))) => break Err(anyhow!("{reason}")),
            Ok(Some(_)) => {}
        }
    };
    session.watch_all(false);
    outcome.map(|_| learner)
}

// ---------------------------------------------------------------- requesting

#[derive(Clone, Debug)]
enum Want {
    Index(String),
    Page(String, i32),
    Raw(u16),
    Nothing,
}

enum Pump {
    Got,
    ClientSent,
    Timeout,
}

struct Ripper<'a> {
    session: Session,
    headers: Headers,
    timing: Timing,
    adaptive: bool,
    /// The time between two requests now.
    delay: Duration,
    answered_in_a_row: usize,
    hooks: &'a RipHooks<'a>,
    cancel: &'a CancellationToken,
    recorder: Recorder,
    /// Writes pages that cannot be read even when recording is off.
    unreadable_recorder: Recorder,
    indexes: HashMap<String, CatalogIndex>,
    pages: HashMap<(String, i32), CatalogPage>,
    /// Pages that answered with something unreadable.
    unreadable: HashMap<(String, i32), String>,
    client_sent: HashSet<u16>,
    /// Request headers the server has answered at least once.
    answered: HashSet<u16>,
    last_send: Option<Instant>,
    /// The mode of the last index / page request, for answers that do not carry it.
    index_mode: String,
    page_mode: String,
    /// Answer headers kept as they come (the extra messages), and what came.
    raw_watch: HashSet<u16>,
    raw: HashMap<u16, Vec<u8>>,
}

impl Ripper<'_> {
    fn log(&self, text: &str) {
        (self.hooks.log)(text);
    }

    fn have(&self, want: &Want) -> bool {
        match want {
            Want::Index(mode) => self.indexes.contains_key(mode),
            Want::Page(mode, id) => {
                self.pages.contains_key(&(mode.clone(), *id))
                    || self.unreadable.contains_key(&(mode.clone(), *id))
            }
            Want::Raw(header) => self.raw.contains_key(header),
            Want::Nothing => false,
        }
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Packet(p) if !p.to_server && self.raw_watch.contains(&p.packet.header) => {
                self.raw.insert(p.packet.header, p.packet.body);
            }
            Event::Packet(p) if !p.to_server && p.packet.header == self.headers.index => {
                match packets::parse_index(&p.packet.body) {
                    Ok(mut index) => {
                        if index.mode.is_empty() {
                            index.mode = self.index_mode.clone();
                        }
                        let mode = index.mode.to_ascii_uppercase();
                        // An answer to the client's own request before any of ours: no mode.
                        if !mode.is_empty() {
                            self.recorder.index(&mode, &p.packet.body);
                            self.indexes.insert(mode, index);
                        }
                    }
                    Err(e) => self
                        .hooks
                        .warn(&format!("a catalogue index could not be read: {e:#}")),
                }
            }
            Event::Packet(p) if !p.to_server && p.packet.header == self.headers.page => {
                match packets::parse_page(&p.packet.body) {
                    Ok(mut page) => {
                        if page.mode.is_empty() {
                            page.mode = self.page_mode.clone();
                        }
                        let mode = page.mode.to_ascii_uppercase();
                        if !mode.is_empty() {
                            self.recorder.page(&mode, page.page_id, &p.packet.body);
                            self.pages.insert((mode, page.page_id), page);
                        }
                    }
                    Err(e) => {
                        if let Some((id, mut mode)) = packets::page_key(&p.packet.body) {
                            if mode.is_empty() {
                                mode = self.page_mode.clone();
                            }
                            let mode = mode.to_ascii_uppercase();
                            // Keep the raw packet so the layout can be worked out from real data.
                            let recorder = if self.recorder.is_on() {
                                &self.recorder
                            } else {
                                &self.unreadable_recorder
                            };
                            if let Some(file) = recorder.page(&mode, id, &p.packet.body) {
                                self.log(&format!("  raw page saved: {}", file.display()));
                            }
                            self.unreadable.insert((mode, id), format!("{e:#}"));
                        } else {
                            self.hooks
                                .warn(&format!("a catalogue page could not be read: {e:#}"));
                        }
                    }
                }
            }
            Event::Packet(p) if p.to_server => {
                self.client_sent.insert(p.packet.header);
            }
            Event::Packet(_) | Event::Init { .. } | Event::ConnectionStart(_) => {}
            Event::ConnectionEnd => {
                bail!("the hotel connection ended (logged out or disconnected; if the hotel kicks clients that ask too fast, raise the delay between requests)")
            }
            Event::Closed(reason) => bail!("{reason}"),
        }
        Ok(())
    }

    async fn pump(
        &mut self,
        deadline: Instant,
        want: &Want,
        client_header: Option<u16>,
    ) -> Result<Pump> {
        loop {
            if self.have(want) {
                return Ok(Pump::Got);
            }
            if let Some(h) = client_header {
                if self.client_sent.remove(&h) {
                    return Ok(Pump::ClientSent);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(Pump::Timeout);
            }
            let event = tokio::select! {
                _ = self.cancel.cancelled() => return Err(Cancelled.into()),
                event = tokio::time::timeout(deadline - now, self.session.next_event()) => event,
            };
            match event {
                Err(_) => return Ok(Pump::Timeout),
                Ok(None) => bail!("the connection to G-Earth ended"),
                Ok(Some(event)) => self.handle(event)?,
            }
        }
    }

    async fn send(&mut self, packet: &crate::wire::Packet) -> Result<()> {
        if let Some(last) = self.last_send {
            let next = last + self.delay;
            if next > Instant::now() {
                // Keep reading meanwhile (answers to the client's own requests are cached too).
                self.pump(next, &Want::Nothing, None).await?;
            }
        }
        if self.cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        self.last_send = Some(Instant::now());
        self.session.send_to_server(packet)
    }

    fn set_delay(&mut self, delay: Duration, why: &str) {
        if delay == self.delay {
            return;
        }
        self.delay = delay;
        self.log(&format!(
            "{why}: {} ms between requests now.",
            delay.as_millis()
        ));
        self.hooks.emit(RipEvent::DelayChanged {
            delay_ms: delay.as_millis() as u64,
        });
    }

    /// A request kind the server answers before went unanswered: maybe it is throttling.
    fn slow_down(&mut self) {
        self.answered_in_a_row = 0;
        if self.adaptive && self.delay < self.timing.max_delay {
            let slower = (self.delay * 2).min(self.timing.max_delay);
            self.set_delay(slower, "A request went unanswered, slowing down");
        }
    }

    fn answered_one(&mut self) {
        self.answered_in_a_row += 1;
        if self.adaptive
            && self.delay > self.timing.delay
            && self.answered_in_a_row >= SPEED_UP_AFTER
        {
            self.answered_in_a_row = 0;
            let faster = (self.delay / 2).max(self.timing.delay);
            self.set_delay(faster, "Answers come back again, speeding up");
        }
    }

    /// Sends a request and waits for its answer. The first time a request kind gets no answer,
    /// the user is told what to do and it is asked again until `hotel_wait` runs out; after
    /// that, a missing answer is retried once and then given up (`Ok(false)`).
    async fn request(
        &mut self,
        want: Want,
        packet: crate::wire::Packet,
        label: &str,
    ) -> Result<bool> {
        match &want {
            Want::Index(mode) => self.index_mode = mode.clone(),
            Want::Page(mode, _) => self.page_mode = mode.clone(),
            Want::Raw(_) | Want::Nothing => {}
        }
        if self.have(&want) {
            return Ok(true);
        }
        let header = packet.header;
        let timeout = self.timing.response_timeout;
        self.send(&packet).await?;
        if let Pump::Got = self.pump(Instant::now() + timeout, &want, None).await? {
            self.answered.insert(header);
            self.answered_one();
            return Ok(true);
        }
        if self.answered.contains(&header) {
            self.slow_down();
            self.log(&format!(
                "No answer for {label} within {:.1} s, asking once more.",
                timeout.as_secs_f64()
            ));
            self.send(&packet).await?;
            let got = matches!(
                self.pump(Instant::now() + timeout, &want, None).await?,
                Pump::Got
            );
            return Ok(got);
        }

        // Never answered yet: not logged in yet, wrong header ids, or G-Earth blocks the packet.
        let hotel_wait = self.timing.hotel_wait;
        let resend_every = self.timing.resend_every;
        self.log(&format!("The hotel did not answer the {label} request yet."));
        self.log("  - Make sure your client is logged in (in the hotel view or a room).");
        self.log("  - G-Earth only forwards packets it knows: if its packet info did not load, open the catalogue in");
        self.log("    your client once and click any page (or turn on Developer mode in G-Earth's Extra tab).");
        self.log("  - On a retro, check the header ids of the hotel profile.");
        self.log(&format!(
            "  Waiting up to {} s; the request is repeated every {} s.",
            hotel_wait.as_secs(),
            resend_every.as_secs()
        ));
        let end = Instant::now() + hotel_wait;
        self.client_sent.remove(&header);
        loop {
            let now = Instant::now();
            if now >= end {
                bail!(
                    "The hotel never answered the {label} request (waited {} s). On a retro, check the hotel profile's header ids.",
                    hotel_wait.as_secs()
                );
            }
            match self
                .pump((now + resend_every).min(end), &want, Some(header))
                .await?
            {
                Pump::Got => {
                    self.answered.insert(header);
                    return Ok(true);
                }
                Pump::ClientSent => {
                    // The client sent this request itself, so G-Earth lets it through now.
                    self.send(&packet).await?;
                    let got = matches!(
                        self.pump(Instant::now() + timeout, &want, None).await?,
                        Pump::Got
                    );
                    self.answered.insert(header);
                    return Ok(got);
                }
                Pump::Timeout => {
                    self.send(&packet).await?;
                    if let Pump::Got = self.pump(Instant::now() + timeout, &want, None).await? {
                        self.answered.insert(header);
                        return Ok(true);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- the operation

fn log_instructions(options: &RipOptions, timing: &Timing, log: Log<'_>) {
    log("Rip the catalogue through G-Earth");
    log("This reads the catalogue the way your own client does: G-Earth relays the requests through your");
    log("logged-in session. Nothing is bought or changed; your game traffic passes through unchanged.");
    log("  1. Start G-Earth AS ADMINISTRATOR (it edits the hosts file for the Flash/AIR client).");
    log("  2. With the client closed, press Connect in G-Earth; then open the client and log in.");
    log(&format!(
        "  3. G-Earth may ask whether to allow the extension \"{}\": answer Yes.",
        options.extension.title
    ));
    log("  4. Keep the catalogue window closed in your client while the rip runs.");
    log(&format!(
        "Requests are sent one at a time, every {} ms{}.",
        timing.delay.as_millis(),
        if options.adaptive_delay {
            format!(" (slower, up to {} ms, when the hotel stops answering)", timing.max_delay.as_millis())
        } else {
            String::new()
        }
    ));
}

/// The modes to rip, in order.
fn modes_to_rip(options: &RipOptions, profile: &HotelProfile, hooks: &RipHooks<'_>) -> Vec<String> {
    let mut modes = Vec::new();
    if options.normal {
        modes.push("NORMAL".to_string());
    }
    if options.builders_club {
        if profile.builders_club {
            modes.push("BUILDERS_CLUB".to_string());
        } else {
            let reason = format!("the profile \"{}\" has no Builders Club catalogue", profile.name);
            (hooks.log)(&format!("Only the normal catalogue is ripped: {reason}."));
            hooks.emit(RipEvent::ModeSkipped {
                mode: "BUILDERS_CLUB".into(),
                reason,
            });
        }
    }
    modes
}

/// Runs the whole rip (the callbacks one by one; see [`rip_with`]).
#[allow(clippy::too_many_arguments)]
pub async fn rip(
    options: &RipOptions,
    profile: ProfileChoice,
    items: Option<&dyn ItemsSource>,
    log: Log<'_>,
    ask: Ask<'_>,
    progress: Progress<'_>,
    cancel: &CancellationToken,
) -> Result<RipResult> {
    let hooks = RipHooks {
        items,
        ..RipHooks::new(log, ask, progress)
    };
    rip_with(options, profile, &hooks, cancel).await
}

/// The headers, where each came from, and whether any was learned.
async fn find_headers(
    session: &mut Session,
    connection: &ConnectionInfo,
    profile: &mut HotelProfile,
    options: &RipOptions,
    timing: &Timing,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<(Headers, [&'static str; 4], bool)> {
    let (partial, mut sources) =
        packets::resolve_partial(&connection.packets, &profile.headers, &options.header_overrides);
    if partial.iter().all(Option::is_some) || !options.learn_headers {
        let resolved = packets::resolve_headers(
            &connection.packets,
            &profile.headers,
            &options.header_overrides,
            &profile.name,
        )?;
        return Ok((resolved.headers, resolved.sources, false));
    }
    let learner = learn_headers(session, partial, timing, hooks, cancel).await?;
    let headers = learner.headers().context("learned ids incomplete")?;
    for (i, source) in sources.iter_mut().enumerate() {
        if partial[i].is_none() {
            *source = "learned";
        }
    }
    // Remember them: in the profile used from now on, and as a profile file.
    profile.headers = learner.known();
    if !profile.hosts.iter().any(|h| profiles::host_matches(h, &connection.host)) {
        profile.name = format!("{} (learned)", connection.host);
        profile.hosts = vec![connection.host.clone()];
        profile.builtin = false;
    }
    let saved_to = match &options.profiles_folder {
        Some(dir) => match profiles::save_profile(dir, profile) {
            Ok(path) => {
                (hooks.log)(&format!("The learned ids are saved as the hotel profile {}.", path.display()));
                Some(path)
            }
            Err(e) => {
                hooks.warn(&format!("the learned profile could not be saved: {e:#}"));
                None
            }
        },
        None => None,
    };
    hooks.emit(RipEvent::HeadersLearned { headers, saved_to });
    Ok((headers, sources, true))
}

/// Runs the whole rip.
pub async fn rip_with(
    options: &RipOptions,
    profile: ProfileChoice,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<RipResult> {
    let log = hooks.log;
    let timing = Timing::of(options);
    if !options.normal && !options.builders_club {
        bail!("Nothing to rip: turn on the normal and/or the Builders Club catalogue.");
    }
    let resumed = match &options.resume_from {
        Some(folder) => Some(recording::read(folder)?),
        None => None,
    };
    log_instructions(options, &timing, log);
    let reconnect_until = Instant::now() + timing.hotel_wait;
    let (mut session, connection) = loop {
        let mut session = connect_gearth(options, &timing, hooks, cancel).await?;
        match wait_for_hotel(&mut session, options, &timing, log, cancel).await {
            Ok(connection) => break (session, connection),
            Err(e) if e.is::<GearthDropped>() && Instant::now() < reconnect_until => {
                log(&format!(
                    "G-Earth let the extension go before the hotel connection ({e}; G-Earth restarted, or Connect in Nitro mode): connecting again ..."
                ));
                drop(session);
                sleep_or_cancel(Duration::from_millis(500), cancel).await?;
            }
            Err(e) => return Err(e),
        }
    };
    log(&format!(
        "Hotel connection: {} ({} client, {})",
        connection.host, connection.client_type, connection.hotel_version
    ));
    hooks.emit(RipEvent::HotelConnected {
        host: connection.host.clone(),
        client_type: connection.client_type.clone(),
        hotel_version: connection.hotel_version.clone(),
    });
    match connection.client_type.to_ascii_uppercase().as_str() {
        "SHOCKWAVE" => bail!("The Shockwave (Origins) client uses another protocol and has no catalogue pages to rip; use the Flash/AIR or Nitro client."),
        "UNITY" => hooks.warn("the Unity client is not tested; trying with the Flash structures."),
        _ => {}
    }
    if let Some(recording) = &resumed {
        if !recording.session.host.eq_ignore_ascii_case(&connection.host) {
            bail!(
                "{} was ripped from {}, but G-Earth is connected to {}: connect to the same hotel to go on with it.",
                recording.folder.display(),
                recording.session.host,
                connection.host
            );
        }
    }
    let mut profile = match (&resumed, profile) {
        // Go on with what worked: the recorded profile and ids.
        (Some(recording), ProfileChoice::Detect(_)) => {
            let mut profile = recording.session.profile.clone();
            if let Some(h) = recording.session.headers {
                profile.headers = HeaderMap::new(h.get_index, h.get_page, h.index, h.page);
            }
            log(&format!("Hotel profile: {} (from the stopped rip).", profile.name));
            hooks.emit(RipEvent::ProfileChosen {
                name: profile.name.clone(),
                kind: profile.kind,
                detected_by: None,
            });
            profile
        }
        (_, choice) => choose_profile(choice, &connection, hooks),
    };
    let (headers, sources, learned) = find_headers(
        &mut session,
        &connection,
        &mut profile,
        options,
        &timing,
        hooks,
        cancel,
    )
    .await?;
    log(&format!(
        "Header ids: GetCatalogIndex {} ({}), GetCatalogPage {} ({}), CatalogIndex {} ({}), CatalogPage {} ({})",
        headers.get_index, sources[0], headers.get_page, sources[1], headers.index, sources[2], headers.page, sources[3],
    ));
    hooks.emit(RipEvent::HeadersResolved { headers, sources });
    let mode_list = modes_to_rip(options, &profile, hooks);
    if mode_list.is_empty() {
        bail!("Nothing to rip: this hotel has no Builders Club catalogue and the normal one is turned off.");
    }
    session.watch(false, headers.index);
    session.watch(false, headers.page);
    session.watch(true, headers.get_index);
    session.watch(true, headers.get_page);
    session.console_log(&format!(
        "[green]{}: ripping the catalogue (read-only)",
        options.extension.title
    ));

    // The output folder now, so the packets are recorded as they come.
    let folder = match &resumed {
        Some(recording) => recording.folder.clone(),
        None => {
            let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
            options
                .output_folder
                .join(format!("{}_{stamp}", hotel_name(&connection.host)))
        }
    };
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let recorder = Recorder::new(&folder, options.record_packets || resumed.is_some());
    let mut session_info = SessionInfo {
        format: 1,
        host: connection.host.clone(),
        port: connection.port,
        hotel_version: connection.hotel_version.clone(),
        client_type: connection.client_type.clone(),
        profile: HotelProfile {
            builtin: false,
            file: None,
            ..profile.clone()
        },
        headers: Some(headers),
        modes: Vec::new(),
        failed: BTreeMap::new(),
        complete: false,
        ripped_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    recorder.session(&session_info);

    let started = Instant::now();
    let mut ripper = Ripper {
        session,
        headers,
        timing,
        adaptive: options.adaptive_delay,
        delay: timing.delay,
        answered_in_a_row: 0,
        hooks,
        cancel,
        recorder: recorder.clone(),
        unreadable_recorder: Recorder::new(&folder, true),
        indexes: HashMap::new(),
        pages: HashMap::new(),
        unreadable: HashMap::new(),
        client_sent: HashSet::new(),
        answered: HashSet::new(),
        last_send: None,
        index_mode: String::new(),
        page_mode: String::new(),
        raw_watch: HashSet::new(),
        raw: HashMap::new(),
    };
    let mut resumed_pages = 0;
    if let Some(recording) = &resumed {
        for mode in &mode_list {
            if let Some(index) = recording.indexes.get(mode) {
                ripper.indexes.insert(mode.clone(), index.clone());
            }
        }
        for (key, page) in &recording.pages {
            if mode_list.contains(&key.0) {
                ripper.pages.insert(key.clone(), page.clone());
                resumed_pages += 1;
            }
        }
        log(&format!(
            "Going on with {}: {} catalogue indexes and {} pages are already there; the rest is asked.",
            folder.display(),
            ripper.indexes.len(),
            n0(resumed_pages)
        ));
        hooks.emit(RipEvent::Resumed {
            folder: folder.clone(),
            indexes: ripper.indexes.len(),
            pages: resumed_pages,
        });
    }

    let mut aborted: Option<anyhow::Error> = None;
    let mut accepted: Vec<String> = Vec::new();
    for (i, mode) in mode_list.iter().enumerate() {
        let key = mode.to_ascii_uppercase();
        log(&format!("Requesting the {key} catalogue index ..."));
        match ripper
            .request(
                Want::Index(key.clone()),
                packets::get_catalog_index(headers.get_index, mode),
                &format!("{key} index"),
            )
            .await
        {
            Ok(true) => {
                let index = &ripper.indexes[&key];
                // A server without a Builders Club catalogue may answer with the normal tree.
                if i > 0
                    && accepted
                        .first()
                        .and_then(|first| ripper.indexes.get(first))
                        .is_some_and(|first| first.root == index.root)
                {
                    let reason = "the hotel sent the normal catalogue again".to_string();
                    log(&format!("{key}: {reason} (no separate {key} catalogue); skipped."));
                    hooks.emit(RipEvent::ModeSkipped { mode: key, reason });
                    continue;
                }
                let count = index.walk().len();
                log(&format!("{key}: {} pages in the index.", n0(count)));
                hooks.emit(RipEvent::IndexRead {
                    mode: key.clone(),
                    pages: count,
                });
                accepted.push(key);
            }
            Ok(false) if i == 0 => {
                aborted = Some(anyhow!("The hotel did not send the {key} catalogue index."));
                break;
            }
            Ok(false) => {
                let reason = "no index (this account or hotel may not have it)".to_string();
                log(&format!("{key}: {reason}; skipped."));
                hooks.emit(RipEvent::ModeSkipped { mode: key, reason });
            }
            Err(e) => {
                aborted = Some(e);
                break;
            }
        }
    }
    if accepted.is_empty() {
        return Err(aborted.unwrap_or_else(|| anyhow!("no catalogue index was received")));
    }
    session_info.modes = accepted.clone();
    recorder.session(&session_info);

    // Every requestable page (of the chosen branches), depth first, mode by mode.
    let mut jobs: Vec<(usize, i32, String)> = Vec::new();
    for (m, mode) in accepted.iter().enumerate() {
        if options.index_only {
            break;
        }
        let chosen = options.only_pages.as_ref().map(|only| {
            only.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(mode))
                .map(|(_, roots)| pages_below(&ripper.indexes[mode], roots))
                .unwrap_or_default()
        });
        let mut seen = HashSet::new();
        for (node, _, _) in ripper.indexes[mode].walk() {
            if chosen.as_ref().is_some_and(|c| !c.contains(&node.page_id)) {
                continue;
            }
            if node.page_id > 0 && seen.insert(node.page_id) {
                let prefix = if mode == "NORMAL" {
                    String::new()
                } else {
                    format!("{mode}: ")
                };
                let label = if node.localization.is_empty() {
                    node.name.clone()
                } else {
                    format!("{} ({})", node.localization, node.name)
                };
                jobs.push((m, node.page_id, progress_label(&format!("{prefix}{label}"))));
            }
        }
    }
    let total = jobs.len();
    (hooks.progress)(RipProgress {
        done: 0,
        total,
        label: String::new(),
    });
    let mut failed: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    if options.index_only {
        log("Only the catalogue indexes were asked (no pages): choose the branches to rip.");
    } else if aborted.is_none() {
        log(&format!("Requesting {} pages ...", n0(total)));
        // A later catalogue whose first pages all go unanswered is given up.
        let mut skip_mode: Option<usize> = None;
        let mut read_in_mode: HashMap<usize, usize> = HashMap::new();
        for (n, (m, page_id, label)) in jobs.iter().enumerate() {
            let mode = accepted[*m].clone();
            if skip_mode == Some(*m) {
                failed.entry(mode).or_default().push(*page_id);
                continue;
            }
            let key = (mode.clone(), *page_id);
            let already = ripper.pages.contains_key(&key);
            if !already {
                log(&format!("[{}/{total}] {label}", n + 1));
            }
            match ripper
                .request(
                    Want::Page(mode.clone(), *page_id),
                    packets::get_catalog_page(headers.get_page, *page_id, &mode),
                    &format!("page {page_id}"),
                )
                .await
            {
                Ok(true) => {
                    let ok = ripper.pages.contains_key(&key);
                    if ok {
                        *read_in_mode.entry(*m).or_default() += 1;
                    } else if let Some(error) = ripper.unreadable.get(&key) {
                        log(&format!("  page {page_id} could not be read: {error}"));
                        failed.entry(mode.clone()).or_default().push(*page_id);
                    }
                    hooks.emit(RipEvent::PageDone {
                        mode: mode.clone(),
                        page_id: *page_id,
                        ok,
                    });
                }
                Ok(false) => {
                    log(&format!("  page {page_id}: no answer, skipped."));
                    let list = failed.entry(mode.clone()).or_default();
                    list.push(*page_id);
                    hooks.emit(RipEvent::PageDone {
                        mode: mode.clone(),
                        page_id: *page_id,
                        ok: false,
                    });
                    if *m > 0 && !read_in_mode.contains_key(m) && list.len() >= 3 {
                        let reason = "its first pages got no answer".to_string();
                        log(&format!("{mode}: {reason}; the rest of the {mode} catalogue is skipped."));
                        hooks.emit(RipEvent::ModeSkipped {
                            mode: mode.clone(),
                            reason,
                        });
                        skip_mode = Some(*m);
                    }
                }
                Err(e) => {
                    aborted = Some(e);
                    break;
                }
            }
            (hooks.progress)(RipProgress {
                done: n + 1,
                total,
                label: label.clone(),
            });
        }
    }
    // The extra catalogue data: one try each, no waiting for a first answer.
    let mut extra_answers: BTreeMap<String, Vec<u8>> = resumed
        .as_ref()
        .map(|r| r.extras.clone())
        .unwrap_or_default();
    if aborted.is_none() && options.rip_extras && !options.index_only {
        for extra in extras::EXTRAS {
            if extra_answers.contains_key(extra.key) {
                continue;
            }
            let Some((ask_id, answer_id)) =
                extras::resolve(extra, &profile.extra_headers, &connection.packets)
            else {
                log(&format!("{}: no header ids for this hotel; skipped.", extra.answer));
                continue;
            };
            ripper.session.watch(false, answer_id);
            ripper.raw_watch.insert(answer_id);
            let attempt = async {
                ripper.send(&extras::request(extra, ask_id)).await?;
                ripper
                    .pump(Instant::now() + timing.response_timeout, &Want::Raw(answer_id), None)
                    .await
            };
            match attempt.await {
                Ok(Pump::Got) => {
                    let body = ripper.raw.remove(&answer_id).unwrap_or_default();
                    recorder.extra(extra.key, &body);
                    log(&format!("{}: read.", extra.answer));
                    extra_answers.insert(extra.key.to_string(), body);
                }
                Ok(_) => log(&format!("{}: no answer; skipped.", extra.answer)),
                Err(e) => {
                    aborted = Some(e);
                    break;
                }
            }
        }
    }
    let final_delay = ripper.delay;
    let indexes = std::mem::take(&mut ripper.indexes);
    let pages = std::mem::take(&mut ripper.pages);
    drop(ripper);
    let elapsed = started.elapsed();

    // A whole catalogue was read: no error, and not only the indexes or some branches.
    let whole = aborted.is_none() && !options.index_only && options.only_pages.is_none();
    session_info.failed = failed;
    session_info.complete = whole;
    session_info.ripped_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    recorder.session(&session_info);
    let catalog = recording::assemble(&session_info, &indexes, &pages, whole);
    let output = Output {
        options,
        profile: &profile,
        hooks,
        // A cancelled rip still writes its files, but skips the image downloads.
        cancel,
    };
    let mut result = output.write(&catalog, &folder, &extra_answers).await?;
    result.headers = headers;
    result.elapsed = elapsed;
    result.learned_headers = learned;
    result.resumed_pages = resumed_pages;
    result.delay_ms = final_delay.as_millis() as u64;
    log_summary(&result, log);
    hooks.emit(RipEvent::Finished {
        folder: result.folder.clone(),
        complete: result.complete,
    });
    if let Some(e) = aborted {
        let reason = if e.is::<Cancelled>() {
            "The catalogue rip was cancelled.".to_string()
        } else {
            format!("{e:#}")
        };
        return Err(e.context(PartialOutput {
            folder: result.folder.clone(),
            reason,
        }));
    }
    Ok(result)
}

/// Writes the output of a rip again from its recorded packets (`<folder>/packets/`), without
/// the hotel: with other settings (Manager export mode, images, furnidata) or a newer items_base.
pub async fn reexport(
    folder: &Path,
    options: &RipOptions,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<RipResult> {
    let recording = recording::read(folder)?;
    let catalog = recording.catalog();
    (hooks.log)(&format!(
        "Writing {} again from its recorded packets ({} catalogues, {} pages).",
        folder.display(),
        catalog.modes.len(),
        n0(recording.pages.len())
    ));
    if !recording.unreadable.is_empty() {
        hooks.warn(&format!(
            "{} recorded pages cannot be read and are left out.",
            recording.unreadable.len()
        ));
    }
    let output = Output {
        options,
        profile: &recording.session.profile,
        hooks,
        cancel,
    };
    let mut result = output.write(&catalog, folder, &recording.extras).await?;
    if let Some(headers) = recording.session.headers {
        result.headers = headers;
    }
    result.delay_ms = options.delay_ms;
    log_summary(&result, hooks.log);
    hooks.emit(RipEvent::Finished {
        folder: result.folder.clone(),
        complete: result.complete,
    });
    Ok(result)
}

struct Output<'a> {
    options: &'a RipOptions,
    profile: &'a HotelProfile,
    hooks: &'a RipHooks<'a>,
    cancel: &'a CancellationToken,
}

impl Output<'_> {
    async fn items_base(&self) -> (Option<ItemsBase>, String) {
        let log = self.hooks.log;
        let Some(source) = self.hooks.items else {
            return (None, "No items_base was given: item_ids are subqueries on items_base, resolved when the SQL is imported.".to_string());
        };
        log("Reading items_base ...");
        match source.items_base().await {
            Ok(rows) => {
                let mut items = ItemsBase::default();
                for row in &rows {
                    items.add(row.id, row.sprite_id, &row.item_type, &row.item_name, row.effect_id);
                }
                log(&format!("items_base: {} rows.", n0(rows.len())));
                (Some(items), "item_ids were matched against the hotel's items_base; products it lacked are looked up again on import (subqueries).".to_string())
            }
            Err(e) => {
                self.hooks.warn(&format!("items_base could not be read ({e:#}); item_ids are written as items_base subqueries resolved on import."));
                (None, format!("items_base could not be read while ripping ({e:#}): item_ids are subqueries on items_base, resolved when the SQL is imported."))
            }
        }
    }

    async fn furnidata(&self, host: &str) -> Option<Furnidata> {
        if !self.options.use_furnidata {
            return None;
        }
        let source = self
            .options
            .furnidata_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.profile.furnidata_url.clone().filter(|s| !s.trim().is_empty()))
            .or_else(|| {
                (self.profile.kind == HotelKind::Official)
                    .then(|| furnidata::official_url(host))
                    .flatten()
            })?;
        (self.hooks.log)(&format!("Reading the furnidata of {host} ({source}) ..."));
        match furnidata::load(&source).await {
            Ok(data) => {
                (self.hooks.log)(&format!(
                    "Furnidata: {} class names; products are matched by class name.",
                    n0(data.len())
                ));
                Some(data)
            }
            Err(e) => {
                self.hooks.warn(&format!(
                    "the furnidata could not be read ({e:#}); products are matched by sprite id."
                ));
                None
            }
        }
    }

    async fn productdata(&self, host: &str) -> Option<Productdata> {
        if !self.options.use_furnidata {
            return None;
        }
        let source = self
            .options
            .productdata_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.profile.productdata_url.clone().filter(|s| !s.trim().is_empty()))
            .or_else(|| {
                (self.profile.kind == HotelKind::Official)
                    .then(|| productdata::official_url(host))
                    .flatten()
            })?;
        match productdata::load(&source).await {
            Ok(data) => {
                (self.hooks.log)(&format!("Productdata: {} product names.", n0(data.len())));
                Some(data)
            }
            Err(e) => {
                self.hooks
                    .warn(&format!("the productdata could not be read ({e:#})."));
                None
            }
        }
    }

    /// The earlier rip to compare with, read from its recorded packets.
    fn previous(&self, catalog: &RippedCatalog, folder: &Path) -> Option<(PathBuf, RippedCatalog)> {
        let earlier = match &self.options.diff_against {
            Some(path) => Some(path.clone()),
            None if self.options.diff_with_previous => {
                diff::previous_rip(&self.options.output_folder, &catalog.host, Some(folder))
            }
            None => None,
        }?;
        match recording::read(&earlier) {
            Ok(recording) => Some((earlier, recording.catalog())),
            Err(e) => {
                self.hooks.warn(&format!(
                    "the earlier rip {} cannot be compared: {e:#}",
                    earlier.display()
                ));
                None
            }
        }
    }

    async fn existing(&self) -> Option<manager::ExistingCatalogue> {
        if self.options.manager_mode == ManagerMode::Replace {
            return None;
        }
        match self.hooks.items?.existing_catalogue().await {
            Ok(existing) => existing,
            Err(e) => {
                self.hooks
                    .warn(&format!("the hotel's catalogue ids could not be read: {e:#}"));
                None
            }
        }
    }

    /// The image URLs: the settings', else the client configuration's, else the profile's.
    async fn image_urls(&self) -> (String, String) {
        let mut catalog_url = self.profile.catalog_url.clone();
        let mut icon_url = self.profile.icon_url.clone();
        let config = self
            .options
            .site_config_url
            .clone()
            .or_else(|| self.profile.site_config_url.clone())
            .filter(|s| !s.trim().is_empty());
        if let (Some(source), true) = (config, self.options.download_images) {
            match crate::site_config::load(&source).await {
                Ok(urls) => {
                    (self.hooks.log)(&format!(
                        "Catalogue images from the client configuration: {} (icons {}<n>.png).",
                        urls.catalog_url, urls.icon_url
                    ));
                    catalog_url = Some(urls.catalog_url);
                    icon_url = Some(urls.icon_url);
                }
                Err(e) => self.hooks.warn(&format!(
                    "the client configuration could not be read ({e:#}); the image URLs of the profile are used."
                )),
            }
        }
        (
            self.options
                .catalog_url
                .clone()
                .or(catalog_url)
                .unwrap_or_else(|| DEFAULT_CATALOG_URL.into()),
            self.options
                .icon_url
                .clone()
                .or(icon_url)
                .unwrap_or_else(|| DEFAULT_ICON_URL.into()),
        )
    }

    async fn write(
        &self,
        catalog: &RippedCatalog,
        folder: &Path,
        extra_answers: &BTreeMap<String, Vec<u8>>,
    ) -> Result<RipResult> {
        let log = self.hooks.log;
        let options = self.options;
        std::fs::create_dir_all(folder)
            .with_context(|| format!("creating {}", folder.display()))?;
        let mut files = Vec::new();
        let mut write = |name: &str, text: &str| -> Result<()> {
            let path = folder.join(name);
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            files.push(path);
            Ok(())
        };

        let (items, database_note) = self.items_base().await;
        let furnidata = self.furnidata(&catalog.host).await;
        let products = self.productdata(&catalog.host).await;
        let mut json = export::catalog_json(catalog);
        if let Some(products) = &products {
            json["product_names"] = product_names(catalog, furnidata.as_ref(), products);
        }
        write("catalog.json", &serde_json::to_string_pretty(&json)?)?;
        let sql = export::arcturus_sql_with(catalog, items.as_ref(), furnidata.as_ref());
        write("catalog_pages.sql", &sql.pages)?;
        write("catalog_items.sql", &sql.items)?;
        if !sql.pages_bc.is_empty() {
            write("catalog_pages_bc.sql", &sql.pages_bc)?;
            write("catalog_items_bc.sql", &sql.items_bc)?;
        }
        let existing = self.existing().await;
        let manager = manager::manager_sql_with(
            catalog,
            &ManagerExport {
                items: items.as_ref(),
                furnidata: furnidata.as_ref(),
                mode: options.manager_mode,
                existing: existing.as_ref(),
            },
        );
        if !manager.stats.note.is_empty() {
            log(&format!("Manager import file: {}", manager.stats.note));
        }
        write("catalog_manager.sql", &manager.text)?;

        let missing_furni = missing::missing_furni(
            catalog,
            items.as_ref(),
            furnidata.as_ref(),
            products.as_ref(),
        );
        if !missing_furni.is_empty() {
            log(&format!(
                "{} products are not in the hotel's items_base: missing_furni.json{}.",
                n0(missing_furni.len()),
                if furnidata.is_some() { " and missing_furnidata.json (for the furni import)" } else { "" }
            ));
            write(
                "missing_furni.json",
                &serde_json::to_string_pretty(&serde_json::json!({
                    "count": missing_furni.len(),
                    "furni": missing_furni,
                }))?,
            )?;
            if let Some(data) = &furnidata {
                write(
                    "missing_furnidata.json",
                    &serde_json::to_string_pretty(&missing::missing_furnidata(&missing_furni, data))?,
                )?;
            }
        }

        let changes = match self.previous(catalog, folder) {
            Some((earlier, old)) => {
                let (mut report, text) = diff::diff(
                    &old,
                    catalog,
                    &ManagerExport {
                        items: items.as_ref(),
                        furnidata: furnidata.as_ref(),
                        ..ManagerExport::default()
                    },
                    options.diff_delete_removed,
                );
                report.since = Some(earlier.clone());
                log(&format!(
                    "Since the rip of {} ({}): {}.",
                    old.ripped_at,
                    earlier.display(),
                    report.summary()
                ));
                self.hooks.emit(RipEvent::CatalogChanged {
                    since: earlier,
                    summary: report.summary(),
                    added: report.added.len(),
                    changed: report.changed.len(),
                    removed: report.removed.len(),
                });
                write("catalog_changes.json", &serde_json::to_string_pretty(&report)?)?;
                write("catalog_manager_changes.sql", &text)?;
                Some(report)
            }
            None => None,
        };

        if !extra_answers.is_empty() {
            write(
                "catalog_extras.json",
                &serde_json::to_string_pretty(&extras::extras_json(extra_answers))?,
            )?;
        }
        let (catalog_url, icon_url) = self.image_urls().await;
        let (image_stats, images_text) = if !options.download_images {
            (images::ImageStats::default(), "not downloaded (turned off)".to_string())
        } else if self.cancel.is_cancelled() {
            (images::ImageStats::default(), "not downloaded (the rip was cancelled)".to_string())
        } else {
            log("Downloading page images and icons ...");
            let stats =
                images::download(catalog, folder, &catalog_url, &icon_url, log, self.cancel)
                    .await;
            let text = if self.cancel.is_cancelled() {
                format!("{} (stopped: cancelled)", stats.describe())
            } else {
                stats.describe()
            };
            (stats, text)
        };
        if image_stats.downloaded > 0 || folder.join("images").is_dir() {
            files.push(folder.join("images"));
        }

        let mut readme = export::readme(catalog, &sql.stats, &images_text, &database_note);
        readme.push_str(&manager::readme_section(&manager.stats, items.is_some()));
        readme.push_str(&format!(
            "\nHOTEL PROFILE\n  {} ({})\n",
            self.profile.name,
            self.profile.kind.label()
        ));
        if let Some(report) = &changes {
            readme.push_str(&format!(
                "\nCHANGES (catalog_changes.json, catalog_manager_changes.sql)\n  Since the rip of {}: {}.\n  catalog_manager_changes.sql brings a hotel that holds that earlier rip (imported with its ids)\n  up to date: it adds the new pages and offers and updates the changed ones{}.\n",
                report.since_ripped_at,
                report.summary(),
                if options.diff_delete_removed { ", and deletes the removed ones" } else { "; removed ones are kept" }
            ));
        }
        if !missing_furni.is_empty() {
            readme.push_str(&format!(
                "\nMISSING FURNI (missing_furni.json)\n  {} products are not in the hotel's items_base, so their offers are not in catalog_manager.sql.\n  {}\n",
                missing_furni.len(),
                if furnidata.is_some() {
                    "missing_furnidata.json lists them as a furnidata: give it to the Manager's furni import first."
                } else {
                    "With the source hotel's furnidata they would also be written as a furnidata for the furni import."
                }
            ));
        }
        if let Some(data) = &furnidata {
            readme.push_str(&format!(
                "\nFURNIDATA\n  {} class names of the source hotel: floor and wall products are matched by\n  class name (items_base.item_name), not by sprite id.\n",
                data.len()
            ));
        }
        if folder.join(recording::PACKETS_DIR).is_dir() {
            readme.push_str("\nPACKETS\n  packets/ holds the raw catalogue packets: the Manager can go on with a stopped rip or\n  write these files again from them without the hotel.\n");
        }
        let readme = if cfg!(windows) {
            readme.replace('\n', "\r\n")
        } else {
            readme
        };
        let readme_path = folder.join("README.txt");
        std::fs::write(&readme_path, readme)?;
        files.push(readme_path);

        let mut result = RipResult {
            folder: folder.to_path_buf(),
            files,
            hotel: catalog.hotel.clone(),
            host: catalog.host.clone(),
            client_type: catalog.client_type.clone(),
            hotel_version: catalog.hotel_version.clone(),
            profile: self.profile.name.clone(),
            kind: self.profile.kind,
            headers: Headers {
                get_index: 0,
                get_page: 0,
                index: 0,
                page: 0,
            },
            pages: 0,
            pages_failed: 0,
            items: 0,
            products: 0,
            frontpage_items: 0,
            images: image_stats,
            sql: sql.stats,
            manager: manager.stats,
            complete: catalog.complete,
            elapsed: Duration::ZERO,
            furnidata_names: furnidata.as_ref().map(Furnidata::len).unwrap_or(0),
            learned_headers: false,
            resumed_pages: 0,
            delay_ms: 0,
            productdata_names: products.as_ref().map(Productdata::len).unwrap_or(0),
            missing_furni,
            changes,
            indexes: catalog
                .modes
                .iter()
                .map(|m| (m.mode.clone(), m.index.clone()))
                .collect(),
        };
        for mode in &catalog.modes {
            result.pages += mode.pages.len();
            result.pages_failed += mode.failed.len();
            for page in mode.pages.values() {
                result.items += page.offers.len();
                result.products += page.offers.iter().map(|o| o.products.len()).sum::<usize>();
                result.frontpage_items += page.frontpage_items.len();
            }
        }
        Ok(result)
    }
}

/// `product_names` of catalog.json: the productdata name and description of every offer's
/// localization id and product class name that has one.
fn product_names(
    catalog: &RippedCatalog,
    furnidata: Option<&Furnidata>,
    products: &Productdata,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for mode in &catalog.modes {
        for page in mode.pages.values() {
            for offer in &page.offers {
                let mut codes = vec![offer.localization_id.clone()];
                codes.extend(
                    offer
                        .products
                        .iter()
                        .filter_map(|p| furnidata.and_then(|f| f.classname(p)))
                        .map(str::to_string),
                );
                for code in codes {
                    if let Some((name, description)) = products.get(&code) {
                        out.entry(code).or_insert_with(|| {
                            serde_json::json!({ "name": name, "description": description })
                        });
                    }
                }
            }
        }
    }
    serde_json::Value::Object(out)
}

/// Rips again and again, every `every`, until `cancel` fires: each rip is compared with the one
/// before ([`RipOptions::diff_with_previous`]), so a [`RipEvent::CatalogChanged`] says what the
/// hotel changed. A rip that fails (G-Earth closed, logged out) is reported and tried again at
/// the next turn. Returns the rips that finished.
pub async fn rip_every(
    options: &RipOptions,
    profile: ProfileChoice,
    hooks: &RipHooks<'_>,
    every: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<RipResult>> {
    let options = RipOptions {
        diff_with_previous: true,
        diff_against: None,
        resume_from: None,
        index_only: false,
        record_packets: true,
        ..options.clone()
    };
    let mut done = Vec::new();
    loop {
        match rip_with(&options, profile.clone(), hooks, cancel).await {
            Ok(result) => done.push(result),
            Err(e) if e.is::<Cancelled>() => return Ok(done),
            Err(e) => hooks.warn(&format!("this turn's rip failed: {e:#}")),
        }
        (hooks.log)(&format!("Next rip in {} min.", every.as_secs() / 60));
        if sleep_or_cancel(every, cancel).await.is_err() {
            return Ok(done);
        }
    }
}

fn log_summary(result: &RipResult, log: Log<'_>) {
    log("Catalogue rip summary");
    log(&format!("  Hotel:               {} ({}, profile {})", result.hotel, result.host, result.profile));
    log(&format!("  Time:                {:.1} min", result.elapsed.as_secs_f64() / 60.0));
    log(&format!("  Pages:               {}", n0(result.pages)));
    if result.resumed_pages > 0 {
        log(&format!("  From the stopped rip: {}", n0(result.resumed_pages)));
    }
    if result.pages_failed > 0 {
        log(&format!("  Pages without answer: {}", n0(result.pages_failed)));
    }
    log(&format!("  Offers:              {}", n0(result.items)));
    log(&format!("  Products:            {}", n0(result.products)));
    log(&format!("  Frontpage items:     {}", n0(result.frontpage_items)));
    log(&format!(
        "  Arcturus SQL:        {} catalog_pages, {} catalog_items",
        n0(result.sql.pages),
        n0(result.sql.items)
    ));
    log(&format!(
        "  Manager import:      {} pages, {} offers ({}){}",
        n0(result.manager.pages),
        n0(result.manager.offers),
        result.manager.mode.label(),
        if result.manager.offers_unresolved > 0 {
            format!(", {} left out: products not in items_base", n0(result.manager.offers_unresolved))
        } else {
            String::new()
        }
    ));
    log(&format!("  Images:              {}", result.images.describe()));
    let full = std::path::absolute(&result.folder).unwrap_or_else(|_| result.folder.clone());
    log(&format!("  Output folder:       {}", full.display()));
    if !result.complete {
        log("  INCOMPLETE: the rip stopped early (see README.txt); it can go on from this folder.");
    }
}

/// The newest rip folder under `root`.
pub fn latest_output(root: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotel_names() {
        assert_eq!(hotel_name("game-nl.habbo.com"), "habbo.nl");
        assert_eq!(hotel_name("game-us.habbo.com"), "habbo.com");
        assert_eq!(hotel_name("game-br.habbo.com"), "habbo.com.br");
        assert_eq!(hotel_name("game-tr.habbo.com"), "habbo.com.tr");
        assert_eq!(hotel_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(hotel_name("my:retro"), "my_retro");
        assert_eq!(hotel_name(""), "hotel");
    }

    #[test]
    fn labels_cannot_fake_progress() {
        assert_eq!(progress_label("50% off [new]"), "50  off  new ");
    }

    #[test]
    fn numbers() {
        assert_eq!(n0(0), "0");
        assert_eq!(n0(1234), "1,234");
        assert_eq!(n0(1234567), "1,234,567");
    }

    #[test]
    fn options_round_trip_as_json() {
        let options = RipOptions {
            header_overrides: HeaderMap::new(1, 2, 3, 4),
            gearth_folder: Some("Tools/G-Earth".into()),
            manager_mode: ManagerMode::Add,
            ..RipOptions::default()
        };
        let text = serde_json::to_string(&options).unwrap();
        let back: RipOptions = serde_json::from_str(&text).unwrap();
        assert_eq!(back, options);
        let partial: RipOptions = serde_json::from_str(r#"{"delay_ms": 900}"#).unwrap();
        assert_eq!((partial.delay_ms, partial.timeout_ms), (900, 10_000));
        assert!(partial.record_packets && partial.learn_headers && partial.adaptive_delay);
    }
}
