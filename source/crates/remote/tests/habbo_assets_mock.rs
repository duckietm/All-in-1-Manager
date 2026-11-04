//! Get Assets against a fake Habbo: a small HTTP server with the same paths as Habbo's hotels and CDN, so every
//! download runs end to end and the files land where /var/www/gamedata wants them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aio_remote::habbo_assets::{Asset, Downloader, Options, Urls};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const HAB: &[u8] = b"HAB\0\x01\0\x01\0\0\0\0\0\0\0\0\0\0\0\0\0";
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 13, 10, 26, 10, 1, 2];
const GIF: &[u8] = b"GIF89a....";
const MP3: &[u8] = b"ID3\x04\0\0\0\0\0\0";

/// Serves `files` (path -> body) on 127.0.0.1; anything else is a 404. Returns the base URL.
async fn serve(files: HashMap<String, Vec<u8>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(files);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let files = files.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&request);
                let path = line.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
                let (status, body) = match files.get(&path) {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    base
}

fn fake_habbo() -> HashMap<String, Vec<u8>> {
    let mut f: HashMap<String, Vec<u8>> = HashMap::new();
    let mut add = |path: &str, body: &[u8]| {
        f.insert(path.to_string(), body.to_vec());
    };
    add(
        "/vars",
        b"flash.client.url=//images.habbo.com/gordon/REL1/\nlanding.view.background=${image.library.url}reception/bg.png,${image.library.url}web_promo_small/promo.png\n",
    );
    add("/texts", b"badge_name_ADM=Admin\nbadge_name_X_HHUK=skip\nquests.welcome.Step1.title=Hi\nhello=Hello world\n");
    add("/texts2", b"badge_name_ADM=Admin\nbadge_name_NL1=Dutch\n");
    add("/productdata", br#"[["chair","Chair &amp; Co","A seat"]]"#);
    add(
        "/furnidata",
        br#"<furnidata><roomitemtypes>
            <furnitype id="1" classname="chair"><revision>3</revision><name>Chair</name></furnitype>
            <furnitype id="2" classname="chair*2"><revision>5</revision><name>Chair</name></furnitype>
            </roomitemtypes><wallitemtypes>
            <furnitype id="3" classname="poster"><revision>7</revision><name>Poster</name></furnitype>
            </wallitemtypes></furnidata>"#,
    );
    add("/figuredata", br#"<figuredata><colors/><sets><settype type="hd" paletteid="1"><set id="1" sellable="1"/></settype></sets></figuredata>"#);
    add("/img/gordon/REL1/figuremap.xml", br#"<map><lib id="hh_body" revision="1"><part id="1" type="bd"/></lib><lib id="hh_gone"/></map>"#);
    add("/img/gordon/REL1/hh_body.hab", HAB);
    add("/img/gordon/REL1/effectmap.xml", br#"<map><effect id="1" lib="Dance1" type="dance" revision="2"/></map>"#);
    add("/img/gordon/REL1/HabboAvatarActions.xml", br#"<actions><action id="Lay" state="lay"/></actions>"#);
    add("/img/gordon/REL1/Dance1.hab", HAB);
    add("/img/gordon/REL1/bear.hab", HAB);
    add("/img/gordon/REL1/dog.hab", HAB);
    add("/img/dcr/hof_furni/5/chair.hab", HAB);
    add("/img/dcr/hof_furni/5/chair.swf", b"CWS\x0a1234567");
    add("/img/gordon/REL1/bear.swf", b"FWS\x0a1234567");
    add("/img/dcr/hof_furni/3/chair_icon.png", PNG);
    add("/img/dcr/hof_furni/5/chair_2_icon.png", PNG);
    // An error page answered with 200: never saved as an asset.
    add("/img/dcr/hof_furni/7/poster.hab", b"<html>oops</html>");
    for n in [1, 2, 3, 5] {
        add(&format!("/img/c_images/catalogue/icon_{n}.png"), PNG);
    }
    for n in [1, 2] {
        add(&format!("/img/dcr/hof_furni/mp3/sound_machine_sample_{n}.mp3"), MP3);
    }
    add("/img/c_images/Quests/welcome.png", PNG);
    add("/img/c_images/Quests/welcome_step1.png", PNG);
    add("/img/c_images/reception/bg.png", PNG);
    add("/img/c_images/web_promo_small/promo.png", PNG);
    add("/img/c_images/album1584/ADM.gif", GIF);
    add("/img/c_images/album1584/NL1.gif", GIF);
    f
}

fn exists(root: &Path, path: &str) -> bool {
    root.join("gamedata").join(path).is_file()
}

#[tokio::test]
async fn every_download_lands_in_the_gamedata_layout() {
    let base = serve(fake_habbo()).await;
    let urls = Urls {
        variables: format!("{base}/vars"),
        texts: format!("{base}/texts"),
        productdata: format!("{base}/productdata"),
        furnidata: format!("{base}/furnidata"),
        figuredata: format!("{base}/figuredata"),
        badge_texts: vec![format!("{base}/texts"), format!("{base}/texts2"), format!("{base}/missing")],
        reception_variables: format!("{base}/vars"),
        images: format!("{base}/img"),
    };
    let root = tempfile::tempdir().unwrap();
    let icons = root.path().join("my icons");
    std::fs::create_dir_all(&icons).unwrap();
    std::fs::write(icons.join("fx_icon_5.png"), PNG).unwrap();
    std::fs::write(icons.join("fx_icon_bad.png"), PNG).unwrap();
    let options = Options { not_sellable: true, effect_icons_source: Some(icons.display().to_string()), also_swf: true };
    let lines = Mutex::new(Vec::<String>::new());
    let log = |line: &str| lines.lock().unwrap().push(line.to_string());
    let downloader = Downloader::new(root.path(), urls, options, &log, CancellationToken::new()).unwrap();

    let mut summaries = HashMap::new();
    for asset in Asset::ALL {
        let summary = downloader.run(asset).await.unwrap_or_else(|e| panic!("{asset:?}: {e}"));
        summaries.insert(asset, summary);
    }
    let r = root.path();
    for path in [
        "c_images/album1584/ADM.gif",
        "c_images/album1584/NL1.gif",
        "config/FigureData.json",
        "config/FigureMap.json",
        "clothes/hh_body.hab",
        "config/EffectMap.json",
        "config/HabboAvatarActions.json",
        "effect/Dance1.hab",
        "config/FurnitureData.json",
        "furniture/chair.hab",
        "icons/chair_icon.png",
        "icons/chair_2_icon.png",
        "c_images/catalogue/icon_5.png",
        "sounds/sound_machine_sample_2.mp3",
        "config/ProductData.json",
        "c_images/Quests/welcome_step1.png",
        "c_images/reception/bg.png",
        "c_images/web_promo_small/promo.png",
        "config/ExternalTexts.json",
        "pets/bear.hab",
        "c_images/fx_icons/fx_icon_5.png",
    ] {
        assert!(exists(r, path), "{path} is missing; log:\n{}", lines.lock().unwrap().join("\n"));
    }
    assert!(!exists(r, "furniture/poster.hab"), "an error page is not a .hab");
    // With also_swf the .swf goes to the asset tools, never into gamedata.
    let tools = r.join("Assets_tools/Original_SWF_Files");
    assert!(tools.join("furni/chair.swf").is_file() && tools.join("pets/bear.swf").is_file());
    assert!(!r.join("gamedata/furniture/chair.swf").exists());
    assert!(!exists(r, "clothes/hh_gone.hab"));
    assert!(r.join("downloads/ExternalVariables.json").is_file());
    assert!(!r.join("gamedata/config/ExternalVariables.json").exists(), "the variables stay on this computer");
    assert_eq!(std::fs::read_dir(r.join("gamedata/c_images/fx_icons")).unwrap().count(), 1);
    // No .part files left behind, no .swf or .xml saved.
    let mut stack = vec![r.join("gamedata")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                stack.push(entry.path());
            } else {
                assert!(!name.ends_with(".part") && !name.ends_with(".swf") && !name.ends_with(".xml"), "{name}");
            }
        }
    }

    let figuredata: serde_json::Value = serde_json::from_slice(&std::fs::read(r.join("gamedata/config/FigureData.json")).unwrap()).unwrap();
    assert_eq!(figuredata["setTypes"][0]["sets"][0]["sellable"], false, "not_sellable");
    let products: serde_json::Value = serde_json::from_slice(&std::fs::read(r.join("gamedata/config/ProductData.json")).unwrap()).unwrap();
    assert_eq!(products["productdata"]["product"][0]["name"], "Chair & Co");
    assert_eq!(summaries[&Asset::CatalogueIcons], "4 new catalogue icons, 0 already there");
    assert_eq!(summaries[&Asset::Furnidata], "FurnitureData.json with 3 furniture");
    assert!(summaries[&Asset::Furniture].starts_with("1 new furniture .hab, 0 already there, 1 not on Habbo"), "{}", summaries[&Asset::Furniture]);
    assert!(summaries[&Asset::Pets].starts_with("2 new pet .hab"), "{}", summaries[&Asset::Pets]);

    // A second run downloads nothing again.
    assert_eq!(downloader.run(Asset::Badges).await.unwrap(), "0 new badges, 2 already there");
    assert_eq!(downloader.run(Asset::CatalogueIcons).await.unwrap(), "0 new catalogue icons, 4 already there");
}

#[tokio::test]
async fn effect_icons_from_app_asar_unpacked() {
    // An app.asar whose bundle Electron keeps outside it, in app.asar.unpacked/client/generated/.
    let root = tempfile::tempdir().unwrap();
    let json = br#"{"files":{"client":{"files":{"generated":{"files":{"habbo-inventory-com.hab":{"size":10,"unpacked":true}}}}}}}"#;
    let mut asar = 4u32.to_le_bytes().to_vec();
    asar.extend_from_slice(&((json.len() + 8) as u32).to_le_bytes());
    asar.extend_from_slice(&((json.len() + 4) as u32).to_le_bytes());
    asar.extend_from_slice(&(json.len() as u32).to_le_bytes());
    asar.extend_from_slice(json);
    let app = root.path().join("resources");
    std::fs::create_dir_all(app.join("app.asar.unpacked/client/generated")).unwrap();
    std::fs::write(app.join("app.asar"), &asar).unwrap();
    // The unpacked "bundle" here is a folder-search hit by name; a real one is a .hab, an icon PNG is read as is.
    std::fs::write(app.join("app.asar.unpacked/client/generated/fx_icon_9.png"), PNG).unwrap();
    let options = Options { effect_icons_source: Some(app.join("app.asar").display().to_string()), ..Options::default() };
    let log = |_: &str| {};
    let downloader = Downloader::new(root.path(), Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]), options, &log, CancellationToken::new()).unwrap();
    assert_eq!(downloader.run(Asset::EffectIcons).await.unwrap(), "1 effect icons written, 0 already up to date");
    assert!(root.path().join("gamedata/c_images/fx_icons/fx_icon_9.png").is_file());
}

#[tokio::test]
async fn a_retro_in_any_format_ends_up_as_hab() {
    let files = vec![("x.json".to_string(), b"{}".to_vec())];
    let nitro = aio_assets::nitro::write(&files);
    let hab = aio_assets::hab::write("lamp", &files);
    let mut f: HashMap<String, Vec<u8>> = HashMap::new();
    f.insert(
        "/gamedata/config/FurnitureData.json".into(),
        br#"{"roomitemtypes":{"furnitype":[{"classname":"chair"},{"classname":"chair*2"},{"classname":"lamp"},{"classname":"plant"}]},"wallitemtypes":{"furnitype":[{"classname":"poster"}]}}"#.to_vec(),
    );
    f.insert("/gamedata/furniture/chair.nitro".into(), nitro.clone());
    f.insert("/gamedata/furniture/lamp.hab".into(), hab.clone());
    f.insert("/gamedata/furniture/plant.swf".into(), b"CWS\x0a1234".to_vec());
    f.insert("/gamedata/icons/chair_2_icon.png".into(), PNG.to_vec());
    f.insert("/gamedata/config/FigureMap.json".into(), br#"{"libraries":[{"id":"hh_shirt"},{"id":"hh_human_fx"}]}"#.to_vec());
    f.insert("/gamedata/config/FigureData.json".into(), br#"{"palettes":[]}"#.to_vec());
    f.insert("/gamedata/clothes/hh_shirt.nitro".into(), nitro.clone());
    let base = serve(f).await;
    let urls = aio_remote::habbo_assets::retro::RetroUrls {
        furnidata: format!("{base}/gamedata/config/FurnitureData.json"),
        furniture: format!("{base}/gamedata/furniture/%libname%.nitro"),
        furniture_icons: format!("{base}/gamedata/icons/"),
        clothes: format!("{base}/gamedata/clothes"),
        figuredata: format!("{base}/gamedata/config/FigureData.json"),
        figuremap: format!("{base}/gamedata/config/FigureMap.json"),
        format: String::new(),
    };
    let root = tempfile::tempdir().unwrap();
    let log = |_: &str| {};
    let downloader = Downloader::new(root.path(), Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]), Options::default(), &log, CancellationToken::new()).unwrap();
    let furniture = downloader.retro_furniture(&urls).await.unwrap();
    assert!(furniture.contains("2 new furniture .hab"), "{furniture}");
    assert!(furniture.contains("1 not found on the retro"), "poster: {furniture}");
    assert!(furniture.contains("1 .swf could not be converted and are kept in source/"), "a damaged .swf: {furniture}");
    let g = root.path().join("gamedata");
    let chair = std::fs::read(g.join("furniture/chair.hab")).unwrap();
    assert_eq!(aio_assets::hab::read(&chair).unwrap(), files, "the .nitro is repacked as .hab");
    assert_eq!(std::fs::read(g.join("furniture/lamp.hab")).unwrap(), hab, "a .hab is kept as it is");
    assert!(root.path().join("source/furniture/plant.swf").is_file());
    assert!(g.join("icons/chair_2_icon.png").is_file());
    assert!(g.join("config/FurnitureData.json").is_file());

    let clothes = downloader.retro_clothes(&urls).await.unwrap();
    assert!(clothes.starts_with("FigureData.json and FigureMap.json; 1 new clothes .hab"), "{clothes}");
    assert!(g.join("clothes/hh_shirt.hab").is_file() && !g.join("clothes/hh_human_fx.hab").exists());
    // A second run finds everything, also the kept .swf.
    assert!(downloader.retro_furniture(&urls).await.unwrap().contains("0 new furniture .hab, 3 already there"));

    // With .nitro chosen in the profile only .nitro is asked for: the lamp (only a .hab there) is not found.
    let fresh_root = tempfile::tempdir().unwrap();
    let only_nitro = Downloader::new(fresh_root.path(), Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]), Options::default(), &log, CancellationToken::new()).unwrap();
    let summary = only_nitro.retro_furniture(&aio_remote::habbo_assets::retro::RetroUrls { format: "nitro".into(), ..urls.clone() }).await.unwrap();
    assert!(summary.contains("1 new furniture .hab") && summary.contains("3 not found on the retro"), "{summary}");
    assert!(fresh_root.path().join("gamedata/furniture/chair.hab").is_file() && !fresh_root.path().join("gamedata/furniture/lamp.hab").exists());
}

#[tokio::test]
async fn a_flash_retro_with_furnidata_xml() {
    let files = vec![("x.json".to_string(), b"{}".to_vec())];
    let hab = aio_assets::hab::write("x", &files);
    let mut f: HashMap<String, Vec<u8>> = HashMap::new();
    f.insert(
        "/gamedata/furnidata_xml/1".into(),
        br#"<?xml version="1.0" encoding="UTF-8"?><furnidata><roomitemtypes><furnitype id="13" classname="shelves_norja"><revision>61856</revision><name>Boekenkast</name></furnitype><furnitype id="14" classname="shelves_norja*2"><revision>61856</revision><name>Boekenkast</name></furnitype></roomitemtypes><wallitemtypes><furnitype id="20" classname="poster"><revision>0</revision><name>Poster</name></furnitype></wallitemtypes></furnidata>"#.to_vec(),
    );
    f.insert("/dcr/hof_furni/61856/shelves_norja.hab".into(), hab.clone());
    f.insert("/dcr/hof_furni/61856/shelves_norja_2_icon.png".into(), PNG.to_vec());
    f.insert("/dcr/hof_furni/poster.hab".into(), hab.clone());
    f.insert("/dcr/hof_furni/poster_icon.png".into(), PNG.to_vec());
    f.insert("/gordon/REL/figuremap.xml".into(), br#"<map><lib id="hh_shirt" revision="3"><part id="1" type="ch"/></lib></map>"#.to_vec());
    f.insert("/gamedata/figuredata/1".into(), br#"<figuredata><colors/><sets/></figuredata>"#.to_vec());
    f.insert("/gordon/REL/hh_shirt.hab".into(), hab.clone());
    let base = serve(f).await;
    let urls = aio_remote::habbo_assets::retro::RetroUrls {
        furnidata: format!("{base}/gamedata/furnidata_xml/1"),
        furniture: format!("{base}/dcr/hof_furni"),
        furniture_icons: format!("{base}/dcr/hof_furni"),
        clothes: format!("{base}/gordon/REL"),
        figuredata: format!("{base}/gamedata/figuredata/1"),
        figuremap: format!("{base}/gordon/REL/figuremap.xml"),
        format: String::new(),
    };
    let root = tempfile::tempdir().unwrap();
    let log = |_: &str| {};
    let downloader = Downloader::new(root.path(), Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]), Options::default(), &log, CancellationToken::new()).unwrap();
    let furniture = downloader.retro_furniture(&urls).await.unwrap();
    assert!(furniture.starts_with("FurnitureData.json (from the furnidata XML) with 3 furniture; 2 new furniture .hab"), "{furniture}");
    assert!(furniture.ends_with("2 new icons, 0 already there, 1 not found on the retro"), "{furniture}");
    let g = root.path().join("gamedata");
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(g.join("config/FurnitureData.json")).unwrap()).unwrap();
    assert_eq!(data["roomitemtypes"]["furnitype"][0]["classname"], "shelves_norja", "the XML is saved as FurnitureData.json");
    assert!(g.join("furniture/shelves_norja.hab").is_file() && g.join("furniture/poster.hab").is_file());
    assert!(g.join("icons/shelves_norja_2_icon.png").is_file() && g.join("icons/poster_icon.png").is_file());

    let clothes = downloader.retro_clothes(&urls).await.unwrap();
    assert!(clothes.starts_with("FigureData.json and FigureMap.json; 1 new clothes .hab"), "{clothes}");
    let map: serde_json::Value = serde_json::from_slice(&std::fs::read(g.join("config/FigureMap.json")).unwrap()).unwrap();
    assert_eq!(map["libraries"][0]["id"], "hh_shirt", "figuremap.xml saved as FigureMap.json");
    let figure: serde_json::Value = serde_json::from_slice(&std::fs::read(g.join("config/FigureData.json")).unwrap()).unwrap();
    assert!(figure.get("palettes").is_some(), "figuredata XML saved as FigureData.json");
}

#[tokio::test]
async fn furnidata_from_a_file_with_the_bundles_from_the_retro() {
    let hab = aio_assets::hab::write("chair", &[("x.json".to_string(), b"{}".to_vec())]);
    let mut f: HashMap<String, Vec<u8>> = HashMap::new();
    f.insert("/libraries/furniture/chair.hab".into(), hab.clone());
    let base = serve(f).await;
    let root = tempfile::tempdir().unwrap();
    let json = root.path().join("leet_furni.json");
    std::fs::write(&json, br#"{"roomitemtypes":{"furnitype":[{"classname":"chair"}]},"wallitemtypes":{"furnitype":[]}}"#).unwrap();
    let urls = aio_remote::habbo_assets::retro::RetroUrls {
        furnidata: format!("\"{}\"", json.display()),
        furniture: format!("{base}/libraries/furniture/%libname%.nitro"),
        ..Default::default()
    };
    let log = |_: &str| {};
    let downloader = Downloader::new(root.path(), Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]), Options::default(), &log, CancellationToken::new()).unwrap();
    let summary = downloader.retro_furniture(&urls).await.unwrap();
    assert!(summary.contains("1 new furniture .hab"), "{summary}");
    assert!(root.path().join("gamedata/config/FurnitureData.json").is_file());
    assert_eq!(std::fs::read(root.path().join("gamedata/furniture/chair.hab")).unwrap(), hab);
}

#[tokio::test]
async fn a_stopped_run_stops() {
    let base = serve(fake_habbo()).await;
    let mut urls = Urls::habbo(aio_remote::furniture_language::LANGUAGES[0]);
    urls.texts = format!("{base}/texts");
    let root = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let log = |_: &str| {};
    let downloader = Downloader::new(root.path(), urls, Options::default(), &log, cancel).unwrap();
    assert!(downloader.run(Asset::Texts).await.is_err());
    assert!(!root.path().join("gamedata").exists());
}
