//! `--dry-run`: the login connects to a recorder instead of a server, and every command a step would send is shown
//! in that step's log (prefixed with "[dry run]") instead of being run. Nothing reaches a server.

use std::sync::{Arc, Mutex};

use aio_remote::recorder::Recorder;
use aio_remote::{CommandResult, ServerLogin};

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Where the recorded commands go: the log of the step that runs now.
pub fn set_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    *SINK.lock().unwrap() = Some(Box::new(sink));
}

fn write(text: &str) {
    if let Some(sink) = SINK.lock().unwrap().as_ref() {
        sink(text);
    }
}

/// The dry-run "server" for this login: Ubuntu 24.04, root, every command answers exit code 0 with no output.
pub fn connect(login: ServerLogin) -> Arc<Recorder> {
    let recorder = Recorder::dry_run(login, |kind, command| {
        write(&format!("[dry run] {kind}:"));
        for line in command.lines() {
            write(&format!("[dry run]   {line}"));
        }
    })
    .with_responder(|_, command, _| {
        // The few answers the login and the checks need to look sensible.
        match command {
            "id -un" => CommandResult::ok("root\n"),
            "id -u" => CommandResult::ok("0\n"),
            _ => CommandResult::default(),
        }
    });
    Arc::new(recorder)
}
