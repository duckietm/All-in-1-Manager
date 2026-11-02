//! Parity with the C# installer: a scenario runs against the recording server and what it sent must equal the
//! golden file `tests/golden/<name>.txt` that `tools/parity` wrote from the C# code.

#![allow(dead_code)]

use std::future::Future;
use std::path::PathBuf;

use aio_remote::recorder::Recorder;
use aio_remote::CommandResult;

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

/// Ignores what a step logs.
pub fn log(_: &str) {}

/// Runs `scenario` against a fresh recorder (answered by `respond`) and compares the record with the golden file.
/// An error is written as "!!! <message>", like the harness writes the C# exception.
pub async fn check<F, Fut, T>(name: &str, respond: Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>>, scenario: F)
where
    F: FnOnce(std::sync::Arc<Recorder>) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let recorder = Recorder::new();
    if let Some(respond) = respond {
        recorder.set_responder(respond);
    }
    let recorder = std::sync::Arc::new(recorder);
    if let Err(error) = scenario(recorder.clone()).await {
        recorder.note(&format!("!!! {error}"));
    }
    compare(name, &recorder.text());
}

/// Compares a text with the golden file and points at the first difference.
pub fn compare(name: &str, actual: &str) {
    let path = golden_dir().join(format!("{name}.txt"));
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (the golden files come from the C# harness tools/parity, in the git history up to commit 50dd986)", path.display()));
    if expected == actual {
        return;
    }
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/golden-actual");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join(format!("{name}.txt")), actual);
    let line = expected.lines().zip(actual.lines()).position(|(e, a)| e != a).unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    panic!(
        "{name}: differs from the C# installer at line {}\n  C#:   {:?}\n  Rust: {:?}\n(full output in target/golden-actual/{name}.txt)",
        line + 1,
        expected.lines().nth(line).unwrap_or("<end>"),
        actual.lines().nth(line).unwrap_or("<end>")
    );
}

/// A responder that answers commands containing `needle` with `output`, others with nothing.
pub fn answer(pairs: &'static [(&'static str, &'static str)]) -> Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>> {
    Some(Box::new(move |_kind: &str, command: &str, _stdin: Option<&str>| {
        pairs.iter().find(|(needle, _)| command.contains(needle)).map(|(_, output)| CommandResult::ok(output)).unwrap_or_default()
    }))
}
