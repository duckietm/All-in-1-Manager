//! Catalogue ripper: reads a hotel's catalogue through a G-Earth extension connection.
//!
//! The user runs G-Earth and logs into the hotel with their own client: official Habbo (Flash/
//! AIR client) or a retro (Flash or Nitro client). [`rip`] connects to G-Earth as a network
//! extension (127.0.0.1, port 9092 upwards), picks a [`HotelProfile`] for the connection (its
//! header ids; official Habbo uses the ids G-Earth reports; a hotel nothing covers has its ids
//! learned from the user's client), asks the session for the catalogue index (NORMAL and
//! BUILDERS_CLUB) and every page, one request at a time, and saves
//! `<output>/<hotel>_<timestamp>/`: catalog.json, catalog_pages.sql + catalog_items.sql for
//! Arcturus, catalog_manager.sql for the All-in-1 Manager's catalogue import, README.txt, the
//! page images and icons, and the raw packets (`packets/`, for [`RipOptions::resume_from`] and
//! [`reexport`]). Every intercepted game packet is let through unchanged at once; the only
//! packets sent are GetCatalogIndex and GetCatalogPage.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use aio_catalog_rip::*;
//! let options = RipOptions { output_folder: "Catalog".into(), ..RipOptions::default() };
//! let profiles = profiles(std::path::Path::new("catalog_profiles"));
//! let log = |line: &str| println!("{line}");
//! let ask = |question: String| -> futures::future::BoxFuture<'static, bool> {
//!     println!("{question}");
//!     Box::pin(async { false })
//! };
//! let progress = |p: RipProgress| println!("{}/{}", p.done, p.total);
//! let event = |e: &RipEvent| println!("{e:?}");
//! let hooks = RipHooks { event: Some(&event), ..RipHooks::new(&log, &ask, &progress) };
//! let cancel = tokio_util::sync::CancellationToken::new();
//! let result = rip_with(&options, ProfileChoice::Detect(profiles), &hooks, &cancel).await?;
//! println!("{} pages in {}", result.pages, result.folder.display());
//! # Ok(()) }
//! ```

pub mod diff;
pub mod export;
pub mod extras;
pub mod furnidata;
pub mod gearth;
pub mod gearth_install;
pub mod http;
pub mod images;
pub mod gearth_log;
pub mod learn;
pub mod manager;
pub mod missing;
pub mod packets;
pub mod productdata;
pub mod profiles;
pub mod recording;
pub mod rip;
pub mod site_config;
pub mod translations;
pub mod wire;

use std::path::PathBuf;

use futures::future::BoxFuture;

pub use export::ItemsBase;
pub use manager::{ExistingCatalogue, ManagerMode};
pub use packets::Headers;
pub use profiles::{
    load_profiles, profiles, save_profile, DetectedBy, HeaderMap, HotelKind, HotelProfile,
};
pub use diff::CatalogDiff;
pub use missing::MissingFurni;
pub use rip::{reexport, rip, rip_every, rip_with, ProfileChoice, RipOptions, RipResult};

/// A log line (the same shape as `aio_remote::server::Log`).
pub type Log<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A yes/no question to the user (e.g. "download G-Earth?"); resolves to the answer.
pub type Ask<'a> = &'a (dyn Fn(String) -> BoxFuture<'static, bool> + Send + Sync);

/// Called once with `done: 0` when the page list is known, then after every page.
pub type Progress<'a> = &'a (dyn Fn(RipProgress) + Send + Sync);

/// Receives every [`RipEvent`].
pub type EventSink<'a> = &'a (dyn Fn(&RipEvent) + Send + Sync);

/// Pages done out of the total, with the label of the last page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RipProgress {
    pub done: usize,
    pub total: usize,
    pub label: String,
}

/// What happened, for an interface that wants more than log lines. Every event also has a log
/// line.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RipEvent {
    GearthConnected {
        port: u16,
    },
    HotelConnected {
        host: String,
        client_type: String,
        hotel_version: String,
    },
    ProfileChosen {
        name: String,
        kind: HotelKind,
        /// `None`: the profile was given, or none matched (G-Earth's packet info alone).
        detected_by: Option<DetectedBy>,
    },
    /// No source has every id: watching the client while the user opens the catalogue.
    LearningHeaders {
        missing: Vec<&'static str>,
    },
    /// The ids were learned from the client, and saved as a profile when a profiles folder is set.
    HeadersLearned {
        headers: Headers,
        saved_to: Option<PathBuf>,
    },
    HeadersResolved {
        headers: Headers,
        /// "override", "profile", "G-Earth" or "learned", per id.
        sources: [&'static str; 4],
    },
    /// A stopped rip goes on: what the recording already had.
    Resumed {
        folder: PathBuf,
        indexes: usize,
        pages: usize,
    },
    IndexRead {
        mode: String,
        pages: usize,
    },
    ModeSkipped {
        mode: String,
        reason: String,
    },
    PageDone {
        mode: String,
        page_id: i32,
        ok: bool,
    },
    /// The time between two requests changed (pages went unanswered, or answers came back).
    DelayChanged {
        delay_ms: u64,
    },
    /// Compared with an earlier rip of the same hotel.
    CatalogChanged {
        since: PathBuf,
        summary: String,
        added: usize,
        changed: usize,
        removed: usize,
    },
    Warning(String),
    Finished {
        folder: PathBuf,
        complete: bool,
    },
}

/// Everything the rip reports to and asks of the caller.
#[derive(Clone, Copy)]
pub struct RipHooks<'a> {
    pub log: Log<'a>,
    pub ask: Ask<'a>,
    pub progress: Progress<'a>,
    pub event: Option<EventSink<'a>>,
    /// The hotel's database: items_base, and the catalogue ids for the Manager export modes.
    pub items: Option<&'a dyn ItemsSource>,
}

impl<'a> RipHooks<'a> {
    pub fn new(log: Log<'a>, ask: Ask<'a>, progress: Progress<'a>) -> Self {
        Self {
            log,
            ask,
            progress,
            event: None,
            items: None,
        }
    }

    pub fn emit(&self, event: RipEvent) {
        if let Some(sink) = self.event {
            sink(&event);
        }
    }

    /// A warning: a log line and a [`RipEvent::Warning`].
    pub fn warn(&self, text: &str) {
        (self.log)(&format!("Warning: {text}"));
        self.emit(RipEvent::Warning(text.to_string()));
    }
}

/// The rip was stopped with the [`tokio_util::sync::CancellationToken`]. Find it with
/// `error.is::<Cancelled>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The catalogue rip was cancelled.")]
pub struct Cancelled;

/// The rip stopped early (hotel disconnected, cancelled, ...) after writing what it had read:
/// the outer context of the returned error. Find it with
/// `error.downcast_ref::<PartialOutput>()`; [`RipOptions::resume_from`] with its folder goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialOutput {
    pub folder: PathBuf,
    pub reason: String,
}

impl std::fmt::Display for PartialOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\nThe catalogue read so far was saved in {}.",
            self.reason,
            self.folder.display()
        )
    }
}

/// One items_base row, to turn product class ids into item ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemsBaseRow {
    pub id: u32,
    pub sprite_id: i32,
    /// s, i, e, b, ... as in items_base.type.
    pub item_type: String,
    pub item_name: String,
    /// effect_id_male (0 when the column is missing).
    pub effect_id: i32,
}

/// Where the rip reads the hotel's database from (the app backs it with its SQL session).
#[async_trait::async_trait]
pub trait ItemsSource: Send + Sync {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>>;

    /// The page and offer ids already in the catalogue, per catalog_type (NORMAL / BUILDER), for
    /// [`ManagerMode::ReplaceRipped`] and [`ManagerMode::Add`]. `None`: not known.
    async fn existing_catalogue(&self) -> anyhow::Result<Option<ExistingCatalogue>> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl ItemsSource for Vec<ItemsBaseRow> {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        Ok(self.clone())
    }
}

/// An [`ItemsSource`] from an async closure.
pub struct ItemsFn<F>(pub F);

#[async_trait::async_trait]
impl<F> ItemsSource for ItemsFn<F>
where
    F: Fn() -> BoxFuture<'static, anyhow::Result<Vec<ItemsBaseRow>>> + Send + Sync,
{
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        (self.0)().await
    }
}
