//! The raw packets of a rip, kept in `<output folder>/packets/`:
//! * `session.json`: the hotel connection, the profile and header ids, the catalogues, the pages
//!   without an answer and whether the rip finished;
//! * `index_<MODE>.bin` and `page_<MODE>_<id>.bin`: the body of every CatalogIndex and
//!   CatalogPage answer, as the hotel sent it.
//!
//! They let a stopped rip continue where it was (only the missing pages are asked again), the
//! output be written again without the hotel (another export mode, a newer items_base or
//! furnidata: [`crate::reexport`]), and a real hotel's packets become test fixtures.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::export::{RippedCatalog, RippedMode};
use crate::packets::{self, CatalogIndex, CatalogPage, Headers};
use crate::profiles::HotelProfile;

pub const PACKETS_DIR: &str = "packets";
pub const SESSION_FILE: &str = "session.json";

/// `packets/session.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub format: u32,
    pub host: String,
    pub port: i32,
    pub hotel_version: String,
    pub client_type: String,
    pub profile: HotelProfile,
    #[serde(default)]
    pub headers: Option<Headers>,
    /// The catalogues whose index was read, in order.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Per catalogue, the pages that did not answer.
    #[serde(default)]
    pub failed: BTreeMap<String, Vec<i32>>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub ripped_at: String,
}

/// Writes the packets of a rip (or nothing, when recording is off).
#[derive(Debug, Clone)]
pub struct Recorder {
    dir: Option<PathBuf>,
}

fn mode_part(mode: &str) -> String {
    crate::images::safe_file_name(&mode.to_ascii_uppercase())
}

impl Recorder {
    pub fn new(output_folder: &Path, enabled: bool) -> Self {
        Self {
            dir: enabled.then(|| output_folder.join(PACKETS_DIR)),
        }
    }

    pub fn is_on(&self) -> bool {
        self.dir.is_some()
    }

    fn write(&self, name: &str, data: &[u8]) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(name);
        std::fs::write(&path, data).ok().map(|_| path)
    }

    pub fn index(&self, mode: &str, body: &[u8]) {
        self.write(&format!("index_{}.bin", mode_part(mode)), body);
    }

    /// Returns the file it wrote.
    pub fn page(&self, mode: &str, page_id: i32, body: &[u8]) -> Option<PathBuf> {
        self.write(&format!("page_{}_{page_id}.bin", mode_part(mode)), body)
    }

    /// The answer of an extra message (catalog_extras.json).
    pub fn extra(&self, key: &str, body: &[u8]) {
        self.write(&format!("extra_{}.bin", crate::images::safe_file_name(key)), body);
    }

    pub fn session(&self, info: &SessionInfo) {
        if let Ok(text) = serde_json::to_string_pretty(info) {
            self.write(SESSION_FILE, text.as_bytes());
        }
    }
}

/// A recording read back.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The output folder (the one that holds `packets/`).
    pub folder: PathBuf,
    pub session: SessionInfo,
    pub indexes: HashMap<String, CatalogIndex>,
    pub pages: HashMap<(String, i32), CatalogPage>,
    /// Recorded pages that cannot be read: (mode, page id, error).
    pub unreadable: Vec<(String, i32, String)>,
    /// The answers of the extra messages, by key.
    pub extras: BTreeMap<String, Vec<u8>>,
}

/// Reads `<folder>/packets/` (the folder of a rip).
pub fn read(folder: &Path) -> Result<Recording> {
    let dir = folder.join(PACKETS_DIR);
    let session_path = dir.join(SESSION_FILE);
    let text = std::fs::read_to_string(&session_path).with_context(|| {
        format!(
            "{} has no recorded packets ({} is missing): it was ripped without recording, or is not a rip folder",
            folder.display(),
            session_path.display()
        )
    })?;
    let session: SessionInfo =
        serde_json::from_str(&text).with_context(|| format!("reading {}", session_path.display()))?;
    if session.format != 1 {
        bail!("{} is format {}; this version reads format 1", session_path.display(), session.format);
    }
    let mut recording = Recording {
        folder: folder.to_path_buf(),
        session,
        indexes: HashMap::new(),
        pages: HashMap::new(),
        unreadable: Vec::new(),
        extras: BTreeMap::new(),
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin"))
        .collect();
    files.sort();
    for file in files {
        let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let body = std::fs::read(&file)?;
        if let Some(key) = stem.strip_prefix("extra_") {
            recording.extras.insert(key.to_string(), body);
        } else if let Some(mode) = stem.strip_prefix("index_") {
            let mut index = packets::parse_index(&body)
                .with_context(|| format!("reading {}", file.display()))?;
            index.mode = mode.to_string();
            recording.indexes.insert(mode.to_string(), index);
        } else if let Some(rest) = stem.strip_prefix("page_") {
            let Some((mode, id)) = rest.rsplit_once('_') else {
                continue;
            };
            let Ok(id) = id.parse::<i32>() else { continue };
            match packets::parse_page(&body) {
                Ok(mut page) => {
                    page.mode = mode.to_string();
                    recording.pages.insert((mode.to_string(), id), page);
                }
                Err(e) => recording
                    .unreadable
                    .push((mode.to_string(), id, format!("{e:#}"))),
            }
        }
    }
    Ok(recording)
}

/// The catalogue as the rip writes it: per catalogue in `modes` that has an index, every page
/// of its tree that answered; `failed` lists the pages without an answer.
pub fn assemble(
    session: &SessionInfo,
    indexes: &HashMap<String, CatalogIndex>,
    pages: &HashMap<(String, i32), CatalogPage>,
    complete: bool,
) -> RippedCatalog {
    let mut modes = Vec::new();
    for mode in &session.modes {
        let Some(index) = indexes.get(mode) else { continue };
        let mut found = HashMap::new();
        for (node, _, _) in index.walk() {
            if node.page_id > 0 {
                if let Some(page) = pages.get(&(mode.clone(), node.page_id)) {
                    found.insert(node.page_id, page.clone());
                }
            }
        }
        modes.push(RippedMode {
            mode: mode.clone(),
            index: index.clone(),
            pages: found,
            failed: session.failed.get(mode).cloned().unwrap_or_default(),
        });
    }
    RippedCatalog {
        hotel: crate::rip::hotel_name(&session.host),
        host: session.host.clone(),
        hotel_version: session.hotel_version.clone(),
        client_type: session.client_type.clone(),
        ripped_at: session.ripped_at.clone(),
        complete,
        modes,
    }
}

impl Recording {
    /// The recorded catalogue.
    pub fn catalog(&self) -> RippedCatalog {
        assemble(&self.session, &self.indexes, &self.pages, self.session.complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::builtin_profiles;
    use crate::wire::Writer;

    #[test]
    fn writes_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::new(dir.path(), true);
        // root with one child page 5, no tail
        let index = Writer::new(1)
            .bool(true).i32(0).i32(-1).string("root").string("").i32(0).i32(1)
            .bool(true).i32(1).i32(5).string("shop").string("Shop").i32(0).i32(0)
            .packet();
        recorder.index("BUILDERS_CLUB", &index.body);
        let page = Writer::new(2)
            .i32(5).string("x").string("default_3x3").i32(0).i32(0).i32(0).i32(-1).bool(false)
            .packet();
        recorder.page("BUILDERS_CLUB", 5, &page.body);
        recorder.page("BUILDERS_CLUB", 6, &[0, 0, 0, 6, 9]);
        let session = SessionInfo {
            format: 1,
            host: "game.x.net".into(),
            port: 1,
            hotel_version: "v".into(),
            client_type: "FLASH".into(),
            profile: HotelProfile {
                builtin: false,
                ..builtin_profiles().remove(1)
            },
            headers: None,
            modes: vec!["BUILDERS_CLUB".into()],
            failed: BTreeMap::new(),
            complete: true,
            ripped_at: "now".into(),
        };
        recorder.session(&session);

        let back = read(dir.path()).unwrap();
        assert_eq!(back.session, session);
        assert_eq!(back.indexes["BUILDERS_CLUB"].mode, "BUILDERS_CLUB");
        assert_eq!(back.pages[&("BUILDERS_CLUB".to_string(), 5)].mode, "BUILDERS_CLUB");
        assert_eq!(back.unreadable.len(), 1);
        let catalog = back.catalog();
        assert_eq!(catalog.hotel, "game.x.net");
        assert_eq!(catalog.modes[0].pages.len(), 1);

        assert!(!Recorder::new(dir.path(), false).is_on());
        let empty = tempfile::tempdir().unwrap();
        assert!(read(empty.path()).unwrap_err().to_string().contains("no recorded packets"));
    }
}
