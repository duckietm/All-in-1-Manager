//! "hello": the example All-in-1 Manager plugin.
//!
//! A plugin is a WebAssembly component for the `plugin` world of `aio:plugin@1.0.0`
//! (see `source/wit/aio-plugin/plugin.wit`). It never draws anything itself:
//!
//! * `render` returns a description of the page (a flat list of elements, containers point
//!   to their children by index);
//! * `on-event` receives what the user did (a button click, an edited input). The host calls
//!   `render` again afterwards.
//!
//! Everything that touches the outside world goes through the `host` imports. This plugin only
//! uses `log` and the private data folder (`file-read` / `file-write`, permission `files`).
//!
//! Plugin state lives in a `thread_local`: the component instance is single threaded and
//! keeps its memory between calls.

use std::cell::RefCell;

wit_bindgen::generate!({
    path: "../../wit/aio-plugin",
    world: "plugin",
});

use aio::plugin::host::{self, LogLevel};
use aio::plugin::types::{ButtonData, InputData, UiElement};

/// The private file that keeps the counter between sessions.
const COUNT_FILE: &str = "count.txt";

#[derive(Default)]
struct State {
    /// What the user typed into the `name` input.
    name: String,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// How many times the user has greeted so far; a missing or unreadable file counts as 0.
fn read_count() -> u64 {
    host::file_read(COUNT_FILE)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

struct Hello;

impl Guest for Hello {
    // With the `runaway` feature everything after the loop is unreachable on purpose.
    #[cfg_attr(feature = "runaway", allow(unreachable_code, unused_variables))]
    fn render() -> UiTree {
        // Test variants (see Cargo.toml). Compiled out of the default build.
        #[cfg(feature = "runaway")]
        loop {
            std::hint::black_box(());
        }
        #[cfg(feature = "sleeper")]
        std::thread::sleep(std::time::Duration::from_secs(3600));
        #[cfg(feature = "asker")]
        host::log(LogLevel::Info, &format!("asked: {}", host::ask("Continue?")));
        #[cfg(feature = "scripter")]
        match host::run_script("echo hi", false) {
            Ok(r) => host::log(LogLevel::Info, &format!("script: {} {}", r.exit_code, r.output.trim_end())),
            Err(e) => host::log(LogLevel::Error, &format!("script failed: {e}")),
        }
        #[cfg(feature = "hog")]
        {
            // Touch every page so the memory is really used, and keep it alive until the end.
            let mut hog = vec![0u8; 80 * 1024 * 1024];
            for i in (0..hog.len()).step_by(4096) {
                hog[i] = 1;
            }
            std::hint::black_box(&hog);
        }

        let name = STATE.with(|s| s.borrow().name.clone());
        let elements = vec![
            // 0: the top element, a column that lays out the others from top to bottom.
            UiElement::Column(vec![1, 2, 3, 4]),
            UiElement::Heading("Hello".to_string()),
            UiElement::Input(InputData { id: "name".to_string(), label: "Name".to_string(), value: name }),
            UiElement::Button(ButtonData { id: "greet".to_string(), label: "Greet".to_string(), primary: true }),
            UiElement::Text(format!("Greeted {} times", read_count())),
        ];
        UiTree { root: 0, elements }
    }

    fn on_event(event: UiEvent) -> Result<(), String> {
        match event {
            UiEvent::Changed(change) if change.id == "name" => {
                STATE.with(|s| s.borrow_mut().name = change.value);
            }
            UiEvent::Clicked(id) if id == "greet" => {
                let name = STATE.with(|s| s.borrow().name.clone());
                host::log(LogLevel::Info, &format!("Hello {name}"));
                let count = read_count() + 1;
                host::file_write(COUNT_FILE, count.to_string().as_bytes())?;
            }
            // Other events do not concern this plugin.
            _ => {}
        }
        Ok(())
    }
}

export!(Hello);
