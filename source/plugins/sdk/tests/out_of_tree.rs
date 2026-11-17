//! A plugin crate outside this repository's workspaces, depending on the SDK by path the way a git dependency
//! does, builds into a component for wasm32-wasip2. It uses every SDK module, so this also checks that the whole
//! API is reachable from a plugin crate and that the SDK finds its WIT files relative to itself.
//!
//! Needs the `wasm32-wasip2` target and the guest workspace's dependencies in the local cargo cache (the build runs
//! `--offline`). The build output goes to `source/plugins/target/out-of-tree`, so later runs are incremental.

use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN: &str = r#"
use aio_plugin_sdk::host::{self, Level, PickedFile};
use aio_plugin_sdk::sql::{self, Op, Row, Select, Value};
use aio_plugin_sdk::ui::BannerKind;
use aio_plugin_sdk::{export_plugin, files, http, script, tcp, Error, ErrorKind, Event, Plugin, Ui};

#[derive(Default)]
struct Everything {
    lines: Vec<String>,
    file: Option<PickedFile>,
}

impl Everything {
    fn run(&mut self, id: &str) -> Result<(), Error> {
        match id {
            "log" => {
                host::log(Level::Info, "hello");
                host::status("busy");
            }
            "ask" => {
                if host::ask("Sure?") {
                    let name = host::prompt_text("Name", "Your name", "ann").unwrap_or_default();
                    self.lines.push(name);
                }
            }
            "open" => {
                let extensions: Vec<String> = vec!["sql".into(), "txt".into()];
                if let Some(file) = host::pick_open_file("Open", &extensions) {
                    self.lines.push(format!("{} bytes", file.read()?.len()));
                    host::open_import_preview(&file)?;
                    self.file = Some(file);
                }
            }
            "save" => {
                if let Some(file) = host::pick_save_file("Save", "out.txt", [] as [&str; 0]) {
                    file.write(b"data")?;
                    self.lines.push(file.name());
                }
                if let Some(file) = host::pick_save_file("Save", "out.sql", ["sql"]) {
                    file.write(b"--")?;
                }
                if let Some(bytes) = files::read_if_exists("maybe.bin")? {
                    self.lines.push(bytes.len().to_string());
                }
                let saved = files::read_to_string_if_exists("maybe.txt")?.unwrap_or_default();
                self.lines.push(saved);
            }
            "files" => {
                files::write_str("a/b.txt", "x")?;
                files::write("c.bin", &[1, 2])?;
                self.lines.push(files::read_to_string("a/b.txt")?);
                self.lines.push(files::read("c.bin")?.len().to_string());
            }
            "sql" => {
                let rows: Vec<Row> = Select::from("users").columns(["id", "name"]).filter("id", Op::Gt, 0).limit(5).run()?;
                for row in &rows {
                    let id: i64 = row.get("id")?;
                    let name: Option<String> = row.get("name")?;
                    let raw: Value = row.get("name")?;
                    self.lines.push(format!("{id} {name:?} {raw:?}"));
                }
                let n = sql::write([
                    sql::insert("t").value("a", 1).value("b", "x").into(),
                    sql::update("t").set("a", 2.5).filter("b", Op::Eq, "x").into(),
                    sql::delete("t").filter("a", Op::IsNull, Value::Null).into(),
                ])?;
                self.lines.push(n.to_string());
            }
            "net" => {
                self.lines.push(http::get("https://example.com/")?.len().to_string());
                let mut conn = tcp::connect(3000)?;
                conn.send(b"ping")?;
                self.lines.push(conn.recv(tcp::MAX_RECV)?.len().to_string());
                conn.close();
            }
            "script" => {
                let r = script::run("uptime\n")?;
                let root = script::run_as_root("id\n")?;
                self.lines.push(format!("{} {} {} {}", r.success(), r.exit_code, r.output, root.error));
            }
            _ => return Err(Error::new(ErrorKind::Other, "unknown button")),
        }
        Ok(())
    }
}

impl Plugin for Everything {
    fn new() -> Self {
        Self::default()
    }

    fn render(&mut self, ui: &mut Ui) {
        ui.heading("Everything");
        ui.banner(BannerKind::Info, "ready");
        ui.row(|ui| {
            for id in ["log", "ask", "open", "save", "files", "sql", "net", "script"] {
                ui.button(id, id);
            }
        });
        ui.log(self.lines.iter().cloned());
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        let id = event.id().to_string();
        match self.run(&id) {
            Err(e) if e.kind() == ErrorKind::Rejected => {
                self.lines.push(format!("rejected: {e}"));
                Ok(())
            }
            other => Ok(other?),
        }
    }
}

export_plugin!(Everything);
"#;

/// A fresh, empty folder in the system temp folder (outside the repository).
fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("aio-plugin-sdk-out-of-tree-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

/// The path as a TOML literal string (single quotes: no escapes, so Windows backslashes are kept).
fn toml_path(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''), "unexpected quote in {text}");
    format!("'{text}'")
}

#[test]
fn a_plugin_outside_the_repository_builds_for_wasm32_wasip2() {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guest_workspace = sdk.parent().unwrap();
    let dir = temp_dir();

    let manifest = format!(
        "[package]\nname = \"out-of-tree\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n\
         [lib]\ncrate-type = [\"cdylib\"]\n\n\
         [dependencies]\naio-plugin-sdk = {{ path = {} }}\n\n\
         # Not part of any workspace above the temp folder.\n[workspace]\n",
        toml_path(sdk)
    );
    std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/lib.rs"), PLUGIN).unwrap();
    // The guest workspace's versions, so the offline build finds every crate in the cache.
    std::fs::copy(guest_workspace.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();

    let target = guest_workspace.join("target").join("out-of-tree");
    let output = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args(["build", "--offline", "--target", "wasm32-wasip2"])
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("RUSTFLAGS")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the out-of-tree plugin did not build:\n{stderr}");

    let wasm = std::fs::read(target.join("wasm32-wasip2/debug/out_of_tree.wasm")).unwrap();
    // A component, not a core module: magic, then version 0x0d and layer 1.
    assert_eq!(wasm[..8], [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00]);

    let _ = std::fs::remove_dir_all(&dir);
}
