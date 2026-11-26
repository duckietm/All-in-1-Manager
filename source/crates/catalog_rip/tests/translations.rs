//! Dictionaries for the Manager's page texts from rips of habbo.nl and habbo.it.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aio_catalog_rip::profiles::builtin_profiles;
use aio_catalog_rip::recording::{Recorder, SessionInfo};
use aio_catalog_rip::translations::{build, latest_official_rips, write};
use aio_catalog_rip::wire::Packet;
use aio_catalog_rip::HotelProfile;
use support::*;

fn record(folder: &Path, host: &str, index: Packet, pages: Vec<Packet>) {
    let recorder = Recorder::new(folder, true);
    recorder.index("NORMAL", &index.body);
    for page in &pages {
        let id = i32::from_be_bytes([page.body[0], page.body[1], page.body[2], page.body[3]]);
        recorder.page("NORMAL", id, &page.body);
    }
    recorder.session(&SessionInfo {
        format: 1,
        host: host.into(),
        port: 1,
        hotel_version: "v".into(),
        client_type: "FLASH".into(),
        profile: HotelProfile {
            builtin: false,
            ..builtin_profiles().remove(0)
        },
        headers: None,
        modes: vec!["NORMAL".into()],
        failed: BTreeMap::new(),
        complete: true,
        ripped_at: String::new(),
    });
}

fn dutch(root: &Path) -> PathBuf {
    let folder = root.join("habbo.nl_2026-11-01_10-00-00");
    let pages = vec![page_answer(1, "NORMAL").unwrap(), page_answer(3, "NORMAL").unwrap()];
    record(
        &folder,
        "game-nl.habbo.com",
        index_packet(INDEX, "NORMAL", &normal_roots(), true, false),
        pages,
    );
    folder
}

fn italian(root: &Path) -> PathBuf {
    let folder = root.join("habbo.it_2026-11-01_11-00-00");
    let mut roots = normal_roots();
    roots[0].caption = "Prima pagina";
    roots[1].caption = "Arredamento";
    roots[1].children[0].caption = "Negozio 3x3";
    // A page that only this hotel has under that id.
    roots[2].name = "it_only_page";
    roots[2].caption = "Solo in Italia";
    let page1 = page_packet(
        PAGE,
        1,
        "NORMAL",
        "frontpage4",
        &["catalog_frontpage_headline_shop_IT", "front_teaser"],
        &["Riscatta qui un codice:", "Secondo testo", "Testo di anteprima"],
        &[],
        None,
        true,
    );
    let page3 = page_packet(
        PAGE,
        3,
        "NORMAL",
        "default_3x3",
        &["catalog_header_roombuilder", "cr_infopic3", "catalog_special_txtbg1"],
        &["È un \"negozio\"\nriga 2", "Dettagli \\ barra", "Anteprima"],
        &[],
        None,
        true,
    );
    record(
        &folder,
        "game-it.habbo.com",
        index_packet(INDEX, "NORMAL", &roots, true, false),
        vec![page1, page3],
    );
    folder
}

#[test]
fn builds_dictionaries_from_two_hotels() {
    let dir = tempfile::tempdir().unwrap();
    let nl = dutch(dir.path());
    let it = italian(dir.path());
    let retro = dir.path().join("game.retro.net_2026-11-01_12-00-00");
    record(
        &retro,
        "game.retro.net",
        index_packet(INDEX, "NORMAL", &normal_roots(), true, false),
        vec![],
    );

    let t = build(&[it.clone(), retro.clone(), nl.clone()]).unwrap();
    assert_eq!(t.source_folder, nl);
    assert_eq!(t.skipped, vec![retro]);
    assert_eq!(t.languages.len(), 1);
    let it_texts = &t.languages[0];
    assert_eq!(it_texts.language, "it");
    assert_eq!(it_texts.folder, it);
    // Pages 1-7 are in both; page 6 has another name in habbo.it.
    assert_eq!((it_texts.pages_paired, it_texts.pages_mismatched), (6, 1));
    let d = &it_texts.dictionary;
    assert_eq!(d["Front Page"], "Prima pagina");
    assert_eq!(d["Furni"], "Arredamento");
    assert_eq!(d["Shop 3x3"], "Negozio 3x3");
    assert_eq!(d["Redeem a voucher code here:"], "Riscatta qui un codice:");
    assert_eq!(d["It's a \"shop\"\nline 2"], "È un \"negozio\"\nriga 2");
    assert_eq!(d["Details \\ backslash"], "Dettagli \\ barra");
    assert!(!d.contains_key("Slow page"), "the mismatched page is left out");
    // Same text in both hotels (not translated there) is kept as it is.
    assert_eq!(d["Rentables"], "Rentables");

    // Filling the Manager's dictionary: its own translations stay.
    let existing = dir.path().join("texts");
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::write(existing.join("it.json"), r#"{"Furni": "Mobili", "Front Page": ""}"#).unwrap();
    let out = dir.path().join("out");
    let files = write(&t, &out, Some(&existing)).unwrap();
    assert!(files.iter().any(|f| f.ends_with("translations_report.json")));
    let merged: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(out.join("it.json")).unwrap()).unwrap();
    assert_eq!(merged["Furni"], "Mobili");
    assert_eq!(merged["Front Page"], "Prima pagina");
    let added: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(out.join("it.added.json")).unwrap()).unwrap();
    assert!(added.contains_key("Front Page") && !added.contains_key("Furni"));
    let text = std::fs::read_to_string(out.join("it.json")).unwrap();
    assert!(text.starts_with("{\n  \""), "the Manager's format: {text}");

    // The newest rip per official hotel, the retro left out.
    let latest = latest_official_rips(dir.path());
    assert_eq!(latest, vec![nl.clone(), it.clone()].into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>());

    let (all, files) = aio_catalog_rip::translations::dictionaries_from_rips(dir.path(), &dir.path().join("out2"), None).unwrap();
    assert_eq!(all.languages.len(), 1);
    assert_eq!(files.len(), 2, "it.json and the report: {files:?}");

    let err = build(&[it]).unwrap_err().to_string();
    assert!(err.contains("habbo.nl"), "{err}");
}
