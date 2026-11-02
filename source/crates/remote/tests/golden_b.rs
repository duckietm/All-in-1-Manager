//! Group B: emulator, emulator_assets, ui_texts, asset_sync, gamedata_files (tools/parity/Scenarios_B.cs).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use aio_core::DatabaseCredentials;
use aio_remote::recorder::Recorder;
use aio_remote::{asset_sync, emulator, emulator_assets, gamedata_files, ui_texts, CommandResult};
use common::{check, compare, log};

type Respond = Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>>;

fn output(exit: i32, text: &'static str) -> Respond {
    Some(Box::new(move |_: &str, _: &str, _: Option<&str>| CommandResult { exit_code: exit, output: text.into(), error: String::new() }))
}

fn by_kind(root: &'static str, stream: &'static str, input: &'static str, exit: i32) -> Respond {
    Some(Box::new(move |kind: &str, _: &str, _: Option<&str>| {
        let (exit_code, output) = match kind {
            "root" => (exit, root),
            "stream" => (exit, stream),
            "input" => (exit, input),
            _ => (0, ""),
        };
        CommandResult { exit_code, output: output.into(), error: String::new() }
    }))
}

fn write_file(path: &Path, size: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; size]).unwrap();
    std::fs::File::options().write(true).open(path).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1_704_164_645)).unwrap();
}

#[tokio::test]
async fn emulator_scripts() {
    compare("emulator_control_functions", &emulator::control_functions());
    compare("emulator_imager_settings_line", &emulator::imager_settings_line("https://hotel.example.com"));
    compare("emulator_imager_keys_sql", &emulator::imager_keys_sql());
    check("emulator_inspect", output(0, "repo=yes\nmemory=2048\nzones=Europe/Rome Etc/UTC\n"), |s| async move {
        let state = emulator::inspect(&*s).await?;
        assert_eq!(state.configured_memory_mb, Some(2048));
        assert_eq!(state.timezones, vec!["Europe/Rome", "Etc/UTC"]);
        Ok(())
    })
    .await;
    check("emulator_install_java", None, |s| async move { emulator::install_java(&*s, &log).await }).await;
    check("emulator_create_account", None, |s| async move { emulator::create_account(&*s, &log).await }).await;
    check("emulator_build", None, |s| async move { emulator::build(&*s, "/usr/lib/jvm/java-25-openjdk-amd64", &log).await }).await;
    check(
        "emulator_build_failed",
        output(1, "  compiling\nERROR: build.sh stopped with exit code 1 (see the Maven output above)."),
        |s| async move { emulator::build(&*s, "/usr/lib/jvm/java-25-openjdk-amd64", &log).await },
    )
    .await;

    // The random secrets on stdin are replaced, as the harness does.
    let recorder = Arc::new(Recorder::new());
    let mut credentials = DatabaseCredentials::new("127.0.0.1", 3306, "habbo", "root", "rootpw", "cms", "cmspw");
    credentials.emulator_password = "emu'pw".into();
    emulator::configure(&*recorder, &credentials, "Europe/Rome", 2048, "/usr/lib/jvm/java-25-openjdk-amd64", &log).await.unwrap();
    let secrets = regex::Regex::new("(master_key|jwt\\.secret)=[A-Za-z0-9]{64}").unwrap();
    compare("emulator_configure", &secrets.replace_all(&recorder.text(), "$1=<random>"));

    check("emulator_configure_imager", None, |s| async move {
        assert!(!emulator::configure_imager(&*s, "https://hotel.example.com", &log).await?);
        Ok(())
    })
    .await;
    check("emulator_set_settings", output(0, "  done\nSETTINGS=yes"), |s| async move {
        let settings = [("ws.whitelist".to_string(), "*.example.com".to_string()), ("a.key".to_string(), "it's".to_string())];
        assert!(emulator::set_settings(&*s, &settings, "test", &log, "").await?);
        Ok(())
    })
    .await;
    check("emulator_read_setting", output(0, "*.example.com\n"), |s| async move {
        assert_eq!(emulator::read_setting(&*s, "ws.whitelist").await?.as_deref(), Some("*.example.com"));
        Ok(())
    })
    .await;
    check("emulator_start", output(0, "ERRORS=2\nSYSTEMD=yes\nJAR=/x.jar"), |s| async move {
        let started = emulator::start(&*s, "*.example.com", &log).await?;
        assert_eq!(started, emulator::EmulatorStartResult { jar: "/x.jar".into(), systemd: true, errors: 2 });
        Ok(())
    })
    .await;
    check("emulator_start_failed", output(1, "  loading\n"), |s| async move { emulator::start(&*s, "*.example.com", &log).await }).await;
}

const ASSETS: &str = "== S0\nloading_icon.png\nwallet/coins.png\n.git/x\n== T0\nwallet/coins.png\n== S1\nbg.png\n== T1\nbg.png\n== S2\n== T2\n";

#[tokio::test]
async fn emulator_assets_scripts() {
    check("emulator_assets_inspect", by_kind(ASSETS, "", "", 0), |s| async move {
        let states = emulator_assets::inspect(&*s).await?;
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].from_server, vec!["loading_icon.png"]);
        assert_eq!(states[0].total, 2);
        assert_eq!(states[1].missing(), 0);
        Ok(())
    })
    .await;
    check("emulator_assets_install", by_kind(ASSETS, "  x\nADDED=1", "", 0), |s| async move {
        assert_eq!(emulator_assets::install(&*s, &log).await?, 1);
        Ok(())
    })
    .await;
    check("emulator_assets_install_nothing", None, |s| async move { emulator_assets::install(&*s, &log).await }).await;
}

#[tokio::test]
async fn ui_texts_scripts() {
    check("ui_texts_inspect", output(0, "LANGS= de en\nCURRENT=de\n"), |s| async move {
        let state = ui_texts::inspect(&*s).await?;
        assert_eq!(state.languages, vec!["de", "en"]);
        assert_eq!(state.current.as_deref(), Some("de"));
        Ok(())
    })
    .await;
    check("ui_texts_install", output(0, "BACKUP=/var/www/gamedata/config/UITexts.jsonc.bak-1"), |s| async move {
        ui_texts::install(&*s, "de", &log).await
    })
    .await;
    check("ui_texts_install_bad_code", None, |s| async move { ui_texts::install(&*s, "x;y", &log).await }).await;
}

#[tokio::test]
async fn asset_sync_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let downloader: PathBuf = dir.path().join("downloader");
    let hab = downloader.join("Habbo_Default").join("hof_furni").join("hab");
    let json = downloader.join("Habbo_Default").join("files").join("json");
    write_file(&hab.join("chair.hab"), 10);
    write_file(&hab.join("table.hab"), 600);
    write_file(&hab.join("notes.txt"), 3);
    write_file(&json.join("FurnitureData.json"), 50);
    write_file(&json.join("Other.json"), 5);
    const LISTING: &str = "== 0\nchair.hab\t10\t1704164645.0\n== 12\nFurnitureData.json\t5\t1.0\n";
    for overwrite in [false, true] {
        let name = if overwrite { "asset_sync_plan_upload_overwrite" } else { "asset_sync_plan_upload" };
        let folder = downloader.clone();
        check(name, by_kind(LISTING, "", "OK 2", 0), |s| async move {
            let plans = asset_sync::plan(&*s, &folder, overwrite).await?;
            assert_eq!(plans[0].local_count, 2);
            assert_eq!(plans[0].new.len(), 1);
            assert_eq!(plans[12].changed.len(), usize::from(overwrite));
            for plan in plans.iter().filter(|p| p.upload_count() > 0) {
                asset_sync::upload(&*s, plan, &|_| {}).await?;
            }
            Ok(())
        })
        .await;
    }
    let folder = downloader.clone();
    check("asset_sync_upload_failed", None, |s| async move {
        let plans = asset_sync::plan(&*s, &folder, false).await?;
        asset_sync::upload(&*s, &plans[0], &|_| {}).await
    })
    .await;
    check("asset_sync_free_space", by_kind("123456\n", "", "", 0), |s| async move {
        assert_eq!(asset_sync::free_space(&*s).await?, 123_456);
        Ok(())
    })
    .await;
    check("asset_sync_fix_permissions", None, |s| async move { asset_sync::fix_permissions(&*s, &log).await }).await;
}

#[tokio::test]
async fn gamedata_files_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("explorer");
    write_file(&local.join("pack").join("a.png"), 700);
    write_file(&local.join("pack").join("sub").join("b.png"), 20);
    write_file(&local.join("single.gif"), 1);

    check("gamedata_list", by_kind("d\t4096\t1700000000.5\tsub\nf\t12\t1700000000\ta.gif\n", "", "", 0), |s| async move {
        let entries = gamedata_files::list(&*s, "c_images/album1584").await?;
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_directory);
        Ok(())
    })
    .await;
    check("gamedata_list_failed", by_kind("ERROR: not a folder\n", "", "", 3), |s| async move { gamedata_files::list(&*s, "x").await }).await;
    check("gamedata_outside", None, |s| async move { gamedata_files::list(&*s, "a/../..").await }).await;
    check("gamedata_create_folder", None, |s| async move { gamedata_files::create_folder(&*s, "c_images", "new folder").await }).await;
    check("gamedata_create_folder_bad_name", None, |s| async move { gamedata_files::create_folder(&*s, "c_images", "a/b").await }).await;
    check("gamedata_rename", None, |s| async move { gamedata_files::rename(&*s, "", "old's", "new").await }).await;
    check("gamedata_delete", None, |s| async move { gamedata_files::delete(&*s, "icons", &["a.png".into(), "b dir".into()]).await }).await;
    check("gamedata_files_under", by_kind("5\t./a.png\n7\t./sub/b.png\n", "", "", 0), |s| async move {
        let files = gamedata_files::files_under(&*s, "icons", &["a.png".into(), "sub".into()]).await?;
        assert_eq!(files, vec![("a.png".to_string(), 5), ("sub/b.png".to_string(), 7)]);
        Ok(())
    })
    .await;
    let mut pack = local.join("pack").into_os_string();
    pack.push(std::path::MAIN_SEPARATOR_STR);
    let paths = vec![PathBuf::from(pack), local.join("single.gif")];
    check("gamedata_upload", by_kind("", "", "OK", 0), |s| async move {
        gamedata_files::upload(&*s, &paths, "c_images", false, &|_| {}).await
    })
    .await;
    let single = vec![local.join("single.gif")];
    check("gamedata_upload_skip", by_kind("", "", "ERROR: the target is not a folder", 3), |s| async move {
        gamedata_files::upload(&*s, &single, "", true, &|_| {}).await
    })
    .await;
}
