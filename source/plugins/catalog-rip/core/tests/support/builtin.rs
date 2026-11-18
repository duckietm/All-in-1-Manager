//! The built-in ripper's recording format, copied from its `recording.rs`
//! (`origin/feature/catalog-ripper:source/crates/catalog_rip/src/recording.rs`: `Recorder`,
//! `SessionInfo`, `mode_part`, and `images::safe_file_name`) apart from the core's
//! `recording.rs`, so the tests see a drift: what the built-in writes into `packets/` for given
//! packets, as (name, bytes).

use std::collections::BTreeMap;

use catalog_rip_core::packets::Headers;
use catalog_rip_core::profiles::HotelProfile;
use serde::{Deserialize, Serialize};

/// The built-in's `packets/session.json`.
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
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub failed: BTreeMap<String, Vec<i32>>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub ripped_at: String,
}

/// The built-in's `images::safe_file_name`.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

fn mode_part(mode: &str) -> String {
    safe_file_name(&mode.to_ascii_uppercase())
}

/// The built-in's `Recorder`, into memory: `packets/<name>` -> bytes (a later write replaces).
#[derive(Debug, Default, Clone)]
pub struct Recorder {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Recorder {
    fn write(&mut self, name: &str, data: &[u8]) {
        self.files.insert(format!("packets/{name}"), data.to_vec());
    }

    pub fn index(&mut self, mode: &str, body: &[u8]) {
        self.write(&format!("index_{}.bin", mode_part(mode)), body);
    }

    pub fn page(&mut self, mode: &str, page_id: i32, body: &[u8]) {
        self.write(&format!("page_{}_{page_id}.bin", mode_part(mode)), body);
    }

    pub fn extra(&mut self, key: &str, body: &[u8]) {
        self.write(&format!("extra_{}.bin", safe_file_name(key)), body);
    }

    pub fn session(&mut self, info: &SessionInfo) {
        if let Ok(text) = serde_json::to_string_pretty(info) {
            self.write("session.json", text.as_bytes());
        }
    }
}

/// The built-in README's PACKETS section.
pub const PACKETS_SECTION: &str = "\nPACKETS\n  packets/ holds the raw catalogue packets: the Manager can go on with a stopped rip or\n  write these files again from them without the hotel.\n";
