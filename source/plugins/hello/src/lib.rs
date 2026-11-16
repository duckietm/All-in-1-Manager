//! "hello": the example All-in-1 Manager plugin, written on `aio-plugin-sdk`.
//!
//! A plugin never draws anything itself: `render` describes the page with the `Ui` builder,
//! `on_event` receives what the user did (a button click, an edited input) and the host calls
//! `render` again afterwards. This plugin only uses `host::log` and the private data folder
//! (`files`, permission `files`).
//!
//! The Cargo features `runaway`, `hog`, `sleeper`, `asker`, `scripter` and `ticker` are test-only variants
//! used by the host's sandbox tests; the default build has none of them.

use aio_plugin_sdk::{export_plugin, files, host, Event, Plugin, Ui};

/// The private file that keeps the counter between sessions.
const COUNT_FILE: &str = "count.txt";

/// How many times the user has greeted so far; a missing or unreadable file counts as 0.
fn read_count() -> u64 {
    files::read_to_string_if_exists(COUNT_FILE)
        .ok()
        .flatten()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

struct Hello {
    /// What the user typed into the `name` input.
    name: String,
    /// The `ticker` test variant's job.
    #[cfg(feature = "ticker")]
    job: ticker::Job,
}

/// The `ticker` test variant: a job that runs in ticks (see Cargo.toml).
#[cfg(feature = "ticker")]
mod ticker {
    use aio_plugin_sdk::{host, Event, Ui};

    #[derive(Default)]
    pub struct Job {
        /// Ticks still to run.
        left: u32,
        /// How long each tick takes.
        pace: std::time::Duration,
        /// The next tick never returns.
        spin: bool,
        /// Ticks run so far.
        ticked: u32,
        /// Ticks run when "cancel" arrived.
        cancelled: Option<u32>,
    }

    impl Job {
        pub fn render(&self, ui: &mut Ui) {
            ui.button("start", "Start");
            ui.button("slow", "Slow");
            ui.button("spin", "Spin");
            ui.button("cancel", "Cancel");
            ui.text(format!("Ticked {} times", self.ticked));
            if let Some(n) = self.cancelled {
                ui.text(format!("Cancelled after {n} ticks"));
            }
        }

        pub fn on_event(&mut self, event: &Event) {
            let Event::Clicked { id } = event else { return };
            match id.as_str() {
                "start" | "slow" => {
                    self.left = 3;
                    self.pace = std::time::Duration::from_millis(if id == "slow" { 2000 } else { 0 });
                    host::keep_working();
                }
                "spin" => {
                    self.spin = true;
                    host::keep_working();
                }
                "cancel" => {
                    self.left = 0;
                    self.cancelled = Some(self.ticked);
                }
                _ => {}
            }
        }

        pub fn tick(&mut self) {
            if self.spin {
                loop {
                    std::hint::black_box(());
                }
            }
            // Logged before the work, so a test knows a tick is under way.
            host::log(host::Level::Info, &format!("starting tick {}", self.ticked + 1));
            std::thread::sleep(self.pace);
            self.ticked += 1;
            self.left = self.left.saturating_sub(1);
            host::log(host::Level::Info, &format!("tick {}", self.ticked));
            if self.left > 0 {
                host::keep_working();
            }
        }
    }
}

impl Plugin for Hello {
    fn new() -> Self {
        Hello {
            name: String::new(),
            #[cfg(feature = "ticker")]
            job: ticker::Job::default(),
        }
    }

    // With the `runaway` feature everything after the loop is unreachable on purpose.
    #[cfg_attr(feature = "runaway", allow(unreachable_code, unused_variables))]
    fn render(&mut self, ui: &mut Ui) {
        // Test variants (see Cargo.toml). Compiled out of the default build.
        #[cfg(feature = "runaway")]
        loop {
            std::hint::black_box(());
        }
        #[cfg(feature = "sleeper")]
        std::thread::sleep(std::time::Duration::from_secs(3600));
        #[cfg(feature = "asker")]
        host::log(host::Level::Info, &format!("asked: {}", host::ask("Continue?")));
        #[cfg(feature = "scripter")]
        match aio_plugin_sdk::script::run("echo hi") {
            Ok(r) => host::log(host::Level::Info, &format!("script: {} {}", r.exit_code, r.output.trim_end())),
            Err(e) => host::log(host::Level::Error, &format!("script failed: {e}")),
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

        ui.heading("Hello");
        ui.input("name", "Name", self.name.clone());
        ui.primary_button("greet", "Greet");
        ui.text(format!("Greeted {} times", read_count()));
        #[cfg(feature = "ticker")]
        self.job.render(ui);
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        #[cfg(feature = "ticker")]
        self.job.on_event(&event);
        match event {
            Event::Changed { id, value } if id == "name" => self.name = value,
            Event::Clicked { id } if id == "greet" => {
                host::log(host::Level::Info, &format!("Hello {}", self.name));
                let count = read_count() + 1;
                files::write_str(COUNT_FILE, &count.to_string())?;
            }
            // Other events do not concern this plugin.
            _ => {}
        }
        Ok(())
    }

    #[cfg(feature = "ticker")]
    fn tick(&mut self) -> Result<(), String> {
        self.job.tick();
        Ok(())
    }
}

export_plugin!(Hello);
