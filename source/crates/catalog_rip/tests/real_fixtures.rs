//! Real rips kept in tests/fixtures/<name>/packets/ (see tests/fixtures/README.md): every
//! recorded packet must still parse, and the Manager import file must still be accepted.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aio_catalog_rip::manager::manager_sql;
use aio_catalog_rip::recording;

fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("packets").join(recording::SESSION_FILE).is_file())
        .collect();
    found.sort();
    found
}

#[test]
fn every_recorded_rip_still_reads() {
    let all = fixtures();
    if all.is_empty() {
        println!("no real rips in tests/fixtures yet (see its README.md)");
        return;
    }
    for folder in all {
        check(&folder);
    }
}

/// The checks on one recorded rip.
fn check(folder: &Path) {
    {
        let name = folder.display();
        let recording = recording::read(folder).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let catalog = recording.catalog();
        assert!(!catalog.modes.is_empty(), "{name}: no catalogue index");
        let pages: usize = catalog.modes.iter().map(|m| m.pages.len()).sum();
        assert!(pages > 0 || !recording.session.complete, "{name}: no pages");
        // Pages that parsed when they were recorded must still parse: a page file is only
        // written for a page that parsed, or under its key when it did not (listed below).
        for (mode, id, error) in &recording.unreadable {
            println!("{name}: {mode} page {id} is unreadable: {error}");
        }
        let text = manager_sql(&catalog, None).text;
        aio_remote::catalog_transfer::target(&BTreeMap::new(), &text)
            .unwrap_or_else(|e| panic!("{name}: catalog_manager.sql refused: {e}"));
        println!(
            "{name}: {} catalogues, {pages} pages, {} unreadable",
            catalog.modes.len(),
            recording.unreadable.len()
        );
    }
}

#[test]
fn the_check_works_on_a_recording() {
    use aio_catalog_rip::recording::{Recorder, SessionInfo};
    use support::*;
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new(dir.path(), true);
    recorder.index("NORMAL", &index_packet(INDEX, "NORMAL", &normal_roots(), true, false).body);
    for id in [1, 2, 3] {
        recorder.page("NORMAL", id, &page_answer(id, "NORMAL").unwrap().body);
    }
    recorder.session(&SessionInfo {
        format: 1,
        host: "game-nl.habbo.com".into(),
        port: 1,
        hotel_version: "v".into(),
        client_type: "FLASH".into(),
        profile: aio_catalog_rip::HotelProfile {
            builtin: false,
            ..aio_catalog_rip::profiles::builtin_profiles().remove(0)
        },
        headers: None,
        modes: vec!["NORMAL".into()],
        failed: BTreeMap::new(),
        complete: true,
        ripped_at: String::new(),
    });
    check(dir.path());
}
