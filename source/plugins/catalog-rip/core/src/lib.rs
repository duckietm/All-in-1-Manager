//! Catalogue ripper core: the pure, I/O-free part of the built-in `aio-catalog-rip` crate.
//!
//! Everything here takes bytes or strings in and returns bytes or strings out: no files, no
//! sockets, no clock, no threads. It builds natively (tests) and for `wasm32-wasip2`. The
//! plugin supplies the I/O (the G-Earth connection as a [`transport::Transport`], HTTP, picked
//! folders) and the current time (a [`Timestamp`]); [`gearth::GEarth`] speaks G-Earth's
//! extension protocol over that transport, synchronously.
//!
//! [`job::Job`] is the rip itself, stepped by the plugin: the built-in's `rip` as a state
//! machine over the plugin's I/O ([`job::JobIo`]); [`output`] and [`images`] are what it
//! writes and downloads, [`recording`] the raw packets it keeps (to go on with a stopped rip or
//! write it again), [`diff`] the changes since an earlier rip, [`gearth_log`] a rip from a
//! G-Earth Packet Logger export; [`source::Source`] reads what an earlier rip left.
//!
//! Not in the core: `gearth_install` (the Manager's G-Earth tool), `translations`, `http`.

pub mod conn;
pub mod diff;
pub mod export;
pub mod extras;
pub mod furnidata;
pub mod gearth;
pub mod gearth_log;
pub mod images;
pub mod job;
pub mod learn;
pub mod manager;
pub mod missing;
pub mod names;
pub mod output;
pub mod packets;
pub mod productdata;
pub mod profiles;
pub mod recording;
pub mod site_config;
pub mod source;
pub mod time;
pub mod transport;
pub mod wire;

use std::path::PathBuf;

pub use conn::{ConnectionInfo, PacketInfo};
pub use export::ItemsBase;
pub use diff::CatalogDiff;
pub use job::{
    Clock, Failure, Fetch, FetchError, ImportLog, Job, JobIo, Phase, ProfileChoice, Progress,
    RipOptions, Sink, Step, Summary,
};
pub use manager::{ExistingCatalogue, ManagerMode};
pub use missing::MissingFurni;
pub use packets::Headers;
pub use profiles::{
    load_profiles, profiles, save_profile, DetectedBy, HeaderMap, HotelKind, HotelProfile,
};
pub use source::{MemSource, Source, SourceEntry};
pub use time::Timestamp;

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

/// The rip was stopped by the user. Find it with
/// `error.is::<Cancelled>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The catalogue rip was cancelled.")]
pub struct Cancelled;

/// The rip stopped early (hotel disconnected, cancelled, ...) after writing what it had read
/// (the message of [`job::Failure`]); [`job::Job::resume`] with its folder goes on.
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

