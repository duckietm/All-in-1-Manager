//! The raw packets of a rip, kept in `<rip folder>/packets/` (the built-in's `recording.rs`):
//! * `session.json`: the hotel connection, the profile and header ids, the catalogues, the pages
//!   without an answer and whether the rip finished;
//! * `index_<MODE>.bin` and `page_<MODE>_<id>.bin`: the body of every CatalogIndex and
//!   CatalogPage answer, as the hotel sent it; `extra_<key>.bin`: the answer of an extra message.
//!
//! The same files, names and bytes as the built-in, so a recording made by either is read by
//! either. They let a stopped rip go on ([`crate::job::Job::resume`]: only the missing pages are
//! asked), the output be written again without the hotel ([`crate::job::Job::reexport`]), and a
//! Packet Logger export become a rip ([`crate::job::Job::import_log`]).
//!
//! Unlike the built-in, the files are written through the job's [`Sink`] (`append` creates a
//! packet file, `write` replaces one that came again and rewrites `session.json`), and a
//! recording is read through a [`Source`] one file at a time ([`Loader`]), step by step.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::job::Sink;
use crate::names::safe_file_name;
use crate::packets::{self, CatalogIndex, CatalogPage, Headers};
use crate::profiles::HotelProfile;
use crate::source::{self, Source};

pub const PACKETS_DIR: &str = "packets";
pub const SESSION_FILE: &str = "session.json";
/// The `format` of session.json this version writes and reads.
pub const FORMAT: u32 = 1;

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

fn mode_part(mode: &str) -> String {
    safe_file_name(&mode.to_ascii_uppercase())
}

/// `index_<MODE>.bin`.
pub fn index_file(mode: &str) -> String {
    format!("index_{}.bin", mode_part(mode))
}

/// `page_<MODE>_<id>.bin`.
pub fn page_file(mode: &str, page_id: i32) -> String {
    format!("page_{}_{page_id}.bin", mode_part(mode))
}

/// `extra_<key>.bin`.
pub fn extra_file(key: &str) -> String {
    format!("extra_{}.bin", safe_file_name(key))
}

/// session.json's bytes (pretty JSON, as the built-in writes it).
pub fn session_json(info: &SessionInfo) -> Vec<u8> {
    serde_json::to_string_pretty(info)
        .unwrap_or_default()
        .into_bytes()
}

/// Writes the packets of a rip into `<folder>/packets/` through the sink (or nothing, when
/// recording is off, except what is asked with `always`).
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    dir: String,
    on: bool,
    /// The packet files written so far (a second answer replaces the file, as the built-in's
    /// `fs::write` does).
    written: HashSet<String>,
    /// Something was written into `packets/` (the README's PACKETS section).
    pub any: bool,
}

impl Recorder {
    pub fn new(folder: &str, on: bool) -> Recorder {
        Recorder {
            dir: format!("{folder}/{PACKETS_DIR}"),
            on,
            written: HashSet::new(),
            any: false,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The sink path of the packet file `name`.
    pub fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.dir)
    }

    /// Writes the packet file `name` (when on, or `always`). `Ok(Some(path))`: written.
    pub fn file(
        &mut self,
        sink: &mut dyn Sink,
        name: &str,
        data: &[u8],
        always: bool,
    ) -> Result<Option<String>, String> {
        if !(self.on || always) {
            return Ok(None);
        }
        let path = self.path(name);
        let result = if self.written.contains(&path) {
            sink.write(&path, data)
        } else {
            sink.append(&path, data)
        };
        result.map_err(|e| format!("{path}: {e}"))?;
        self.written.insert(path.clone());
        self.any = true;
        Ok(Some(path))
    }

    pub fn index(&mut self, sink: &mut dyn Sink, mode: &str, body: &[u8]) -> Result<(), String> {
        self.file(sink, &index_file(mode), body, false).map(|_| ())
    }

    /// Returns the file it wrote. `always`: also when recording is off (an unreadable page is
    /// kept so its layout can be worked out from real data).
    pub fn page(
        &mut self,
        sink: &mut dyn Sink,
        mode: &str,
        page_id: i32,
        body: &[u8],
        always: bool,
    ) -> Result<Option<String>, String> {
        self.file(sink, &page_file(mode, page_id), body, always)
    }

    /// The answer of an extra message (catalog_extras.json).
    pub fn extra(&mut self, sink: &mut dyn Sink, key: &str, body: &[u8]) -> Result<(), String> {
        self.file(sink, &extra_file(key), body, false).map(|_| ())
    }

    /// A file of an earlier recording, carried over: written unless this rip already wrote a
    /// newer one of that name.
    pub fn copy(&mut self, sink: &mut dyn Sink, name: &str, data: &[u8]) -> Result<(), String> {
        if self.written.contains(&self.path(name)) {
            return Ok(());
        }
        self.file(sink, name, data, true).map(|_| ())
    }

    /// session.json as it was (a recording written again unchanged).
    pub fn session_bytes(&mut self, sink: &mut dyn Sink, data: &[u8]) -> Result<(), String> {
        let path = self.path(SESSION_FILE);
        sink.write(&path, data)
            .map_err(|e| format!("{path}: {e}"))?;
        self.any = true;
        Ok(())
    }

    /// Writes (or rewrites) session.json.
    pub fn session(&mut self, sink: &mut dyn Sink, info: &SessionInfo) -> Result<(), String> {
        if !self.on {
            return Ok(());
        }
        let path = self.path(SESSION_FILE);
        sink.write(&path, &session_json(info))
            .map_err(|e| format!("{path}: {e}"))?;
        self.any = true;
        Ok(())
    }
}

/// A recording read back (the built-in's `Recording`).
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The rip folder that holds `packets/` (a [`Source`] path).
    pub folder: String,
    pub session: SessionInfo,
    /// session.json's bytes, as read.
    pub session_raw: Vec<u8>,
    pub indexes: HashMap<String, CatalogIndex>,
    pub pages: HashMap<(String, i32), CatalogPage>,
    /// Recorded pages that cannot be read: (mode, page id, error).
    pub unreadable: Vec<(String, i32, String)>,
    /// The answers of the extra messages, by key.
    pub extras: BTreeMap<String, Vec<u8>>,
    /// Every packet file as it was (name, bytes), to carry the recording over into the new rip
    /// folder's `packets/`.
    pub files: Vec<(String, Vec<u8>)>,
}

impl Recorded {
    /// An empty recording of `session` (filled with [`Recorded::add`]).
    pub fn new(folder: &str, session: SessionInfo) -> Recorded {
        Recorded {
            folder: folder.to_string(),
            session_raw: session_json(&session),
            session,
            indexes: HashMap::new(),
            pages: HashMap::new(),
            unreadable: Vec::new(),
            extras: BTreeMap::new(),
            files: Vec::new(),
        }
    }

    /// One packet file, as the built-in's `recording::read` takes it (other names are left
    /// alone, but kept for the copy).
    pub fn add(&mut self, name: &str, body: Vec<u8>) -> Result<(), String> {
        if let Some(stem) = name.strip_suffix(".bin") {
            if let Some(key) = stem.strip_prefix("extra_") {
                self.extras.insert(key.to_string(), body.clone());
            } else if let Some(mode) = stem.strip_prefix("index_") {
                let mut index = packets::parse_index(&body)
                    .map_err(|e| format!("reading {}/{PACKETS_DIR}/{name}: {e:#}", self.folder))?;
                index.mode = mode.to_string();
                self.indexes.insert(mode.to_string(), index);
            } else if let Some(rest) = stem.strip_prefix("page_") {
                if let Some((mode, id)) = rest.rsplit_once('_') {
                    if let Ok(id) = id.parse::<i32>() {
                        match packets::parse_page(&body) {
                            Ok(mut page) => {
                                page.mode = mode.to_string();
                                self.pages.insert((mode.to_string(), id), page);
                            }
                            Err(e) => {
                                self.unreadable
                                    .push((mode.to_string(), id, format!("{e:#}")))
                            }
                        }
                    }
                }
            }
        }
        self.files.push((name.to_string(), body));
        Ok(())
    }
}

/// Reads `<folder>/packets/` through a [`Source`], a few files per call of [`Loader::step`].
#[derive(Debug)]
pub struct Loader {
    folder: String,
    names: Option<Vec<String>>,
    listed: Vec<String>,
    list_done: bool,
    next: usize,
    out: Option<Recorded>,
}

impl Loader {
    pub fn new(folder: &str) -> Loader {
        Loader {
            folder: folder.trim_end_matches('/').to_string(),
            names: None,
            listed: Vec::new(),
            list_done: false,
            next: 0,
            out: None,
        }
    }

    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// Files read so far, and how many there are (0 until listed).
    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.names.as_ref().map_or(0, Vec::len))
    }

    fn dir(&self) -> String {
        source::join(&self.folder, PACKETS_DIR)
    }

    /// Reads on while `more()` says so (at least one read per call). `Ok(Some(_))`: all read.
    pub fn step(
        &mut self,
        source: &mut dyn Source,
        more: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Recorded>, String> {
        let dir = self.dir();
        loop {
            if self.out.is_none() {
                let path = format!("{dir}/{SESSION_FILE}");
                let text = source::read_all(source, &path).map_err(|e| {
                    format!(
                        "{} has no recorded packets ({path} is missing): it was ripped without recording, or is not a rip folder: {e}",
                        self.folder
                    )
                })?;
                let session: SessionInfo =
                    serde_json::from_slice(&text).map_err(|e| format!("reading {path}: {e}"))?;
                if session.format != FORMAT {
                    return Err(format!(
                        "{path} is format {}; this version reads format {FORMAT}",
                        session.format
                    ));
                }
                let mut recorded = Recorded::new(&self.folder, session);
                recorded.session_raw = text;
                self.out = Some(recorded);
            } else if !self.list_done {
                let page = source
                    .list(&dir, self.listed.len() as u32)
                    .map_err(|e| format!("listing {dir}: {e}"))?;
                if page.is_empty() {
                    self.list_done = true;
                    let mut names: Vec<String> = std::mem::take(&mut self.listed)
                        .into_iter()
                        .filter(|n| n.ends_with(".bin"))
                        .collect();
                    names.sort();
                    self.names = Some(names);
                } else {
                    // Folders too: the paging counts every entry.
                    self.listed.extend(page.into_iter().map(|e| {
                        if e.is_folder {
                            String::new()
                        } else {
                            e.name
                        }
                    }));
                }
            } else {
                let names = self.names.as_ref().expect("listed");
                let Some(name) = names.get(self.next).cloned() else {
                    return Ok(self.out.take());
                };
                self.next += 1;
                let body = source::read_all(source, &format!("{dir}/{name}"))
                    .map_err(|e| format!("reading {dir}/{name}: {e}"))?;
                self.out.as_mut().expect("session").add(&name, body)?;
            }
            if !more() {
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::builtin_profiles;
    use crate::source::MemSource;
    use crate::wire::Writer;

    #[derive(Default)]
    struct Files(BTreeMap<String, Vec<u8>>);

    impl Sink for Files {
        fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
            self.0.insert(path.into(), data.to_vec());
            Ok(())
        }
        fn append(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
            self.0
                .entry(path.into())
                .or_default()
                .extend_from_slice(data);
            Ok(())
        }
    }

    /// The built-in's `writes_and_reads_back`, through a sink and a source.
    #[test]
    fn writes_and_reads_back() {
        let mut sink = Files::default();
        let mut recorder = Recorder::new("rip", true);
        // root with one child page 5, no tail
        let index = Writer::new(1)
            .bool(true)
            .i32(0)
            .i32(-1)
            .string("root")
            .string("")
            .i32(0)
            .i32(1)
            .bool(true)
            .i32(1)
            .i32(5)
            .string("shop")
            .string("Shop")
            .i32(0)
            .i32(0)
            .packet();
        recorder
            .index(&mut sink, "BUILDERS_CLUB", &index.body)
            .unwrap();
        let page = Writer::new(2)
            .i32(5)
            .string("x")
            .string("default_3x3")
            .i32(0)
            .i32(0)
            .i32(0)
            .i32(-1)
            .bool(false)
            .packet();
        recorder
            .page(&mut sink, "BUILDERS_CLUB", 5, &page.body, false)
            .unwrap();
        recorder
            .page(&mut sink, "BUILDERS_CLUB", 6, &[0, 0, 0, 6, 9], false)
            .unwrap();
        // A second answer replaces the file.
        recorder
            .page(&mut sink, "BUILDERS_CLUB", 6, &[0, 0, 0, 6, 8], false)
            .unwrap();
        assert_eq!(
            sink.0["rip/packets/page_BUILDERS_CLUB_6.bin"],
            [0, 0, 0, 6, 8]
        );
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
        recorder.session(&mut sink, &session).unwrap();
        assert!(recorder.any);

        let mut source = MemSource::with_files(sink.0.iter());
        let mut loader = Loader::new("rip");
        let back = loop {
            if let Some(r) = loader.step(&mut source, &mut || false).unwrap() {
                break r;
            }
        };
        assert_eq!(back.session, session);
        assert_eq!(back.indexes["BUILDERS_CLUB"].mode, "BUILDERS_CLUB");
        assert_eq!(
            back.pages[&("BUILDERS_CLUB".to_string(), 5)].mode,
            "BUILDERS_CLUB"
        );
        assert_eq!(back.unreadable.len(), 1);
        assert_eq!(back.files.len(), 3);

        let mut off = Recorder::new("rip", false);
        assert!(!off.is_on());
        assert_eq!(off.page(&mut sink, "NORMAL", 1, &[1], false), Ok(None));
        assert!(off
            .page(&mut sink, "NORMAL", 1, &[1], true)
            .unwrap()
            .is_some());
        let mut empty = MemSource::new();
        let err = Loader::new("x").step(&mut empty, &mut || true).unwrap_err();
        assert!(err.contains("no recorded packets"), "{err}");
    }
}
