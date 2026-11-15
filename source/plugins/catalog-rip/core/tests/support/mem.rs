//! In-memory I/O for the rip job: a sink that keeps the files, a fetch that answers like the
//! built-in tests' `fake_cdn`, clocks, and a loop that steps a job to its end.

use std::collections::BTreeMap;
use std::time::Instant;

use catalog_rip_core::export::ItemsBase;
use catalog_rip_core::job::{Clock, Fetch, FetchError, Job, JobIo, Sink, Step};
use catalog_rip_core::manager::ExistingCatalogue;
use catalog_rip_core::transport::{Closed, Transport};

/// 2025-11-15 12:00:00 UTC: the time every test rip is made at.
pub const UNIX_SECS: i64 = 1_763_208_000;

/// The files a job wrote, by sink path.
#[derive(Default, Debug)]
pub struct MemSink {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Sink for MemSink {
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        assert!(!self.files.contains_key(path), "{path} written twice");
        self.files.insert(path.to_string(), data.to_vec());
        Ok(())
    }
}

impl MemSink {
    /// The files under `folder`, by path relative to it.
    pub fn under(&self, folder: &str) -> BTreeMap<String, Vec<u8>> {
        let prefix = format!("{folder}/");
        self.files
            .iter()
            .map(|(path, data)| {
                let rel = path
                    .strip_prefix(&prefix)
                    .unwrap_or_else(|| panic!("{path} is outside {folder}"));
                (rel.to_string(), data.clone())
            })
            .collect()
    }

    pub fn text(&self, path: &str) -> String {
        String::from_utf8(self.files[path].clone()).unwrap()
    }
}

/// Answers from a function; remembers what was asked.
pub struct MemFetch {
    pub answer: Box<dyn FnMut(&str) -> Result<Vec<u8>, FetchError>>,
    pub asked: Vec<String>,
}

impl Fetch for MemFetch {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.asked.push(url.to_string());
        (self.answer)(url)
    }
}

impl MemFetch {
    /// Nothing is anywhere.
    pub fn none() -> MemFetch {
        MemFetch {
            answer: Box::new(|_| Err(FetchError::NotFound)),
            asked: Vec::new(),
        }
    }
}

/// The fake CDN's host (catalogue images at `/c_images/catalogue`, icons at `/icons/icon_`).
pub const CDN: &str = "http://cdn.test";

/// The built-in tests' `fake_cdn` as a fetch: `<name>.png` exists, `.gif` does not, except for
/// the catalog_header images; icons exist; a name with "missing" in it does not. The body is
/// `image:<path>`. Other hosts (Habbo's image host) have nothing.
pub fn fake_cdn() -> MemFetch {
    MemFetch {
        answer: Box::new(|url| {
            let Some(path) = url.strip_prefix(CDN) else {
                return Err(FetchError::NotFound);
            };
            let found = (path.starts_with("/c_images/catalogue/catalog_header")
                && path.ends_with(".gif"))
                || (path.starts_with("/c_images/catalogue/")
                    && path.ends_with(".png")
                    && !path.contains("missing"))
                || path.starts_with("/icons/icon_");
            if found {
                Ok(format!("image:{path}").into_bytes())
            } else {
                Err(FetchError::NotFound)
            }
        }),
        asked: Vec::new(),
    }
}

/// Real milliseconds (for the TCP fake G-Earth), at [`UNIX_SECS`].
pub struct RealClock(pub Instant);

impl RealClock {
    pub fn new() -> RealClock {
        RealClock(Instant::now())
    }
}

impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    fn unix_secs(&self) -> i64 {
        UNIX_SECS
    }
}

/// What a run of a job left.
pub struct Run {
    pub end: Step,
    pub log: Vec<String>,
    pub events: Vec<catalog_rip_core::RipEvent>,
    pub steps: usize,
}

impl Run {
    pub fn log_text(&self) -> String {
        self.log.join("\n")
    }
}

/// Steps `job` to its end with `budget` per step. The G-Earth transport is closed (dropped,
/// through `close`) as soon as the job no longer uses it, as the plugin does. `each` sees the
/// job after every step (to cancel, or check something). `db`: the items_base and catalogue ids.
#[allow(clippy::too_many_arguments)]
pub fn run_job<T: Transport>(
    job: &mut Job,
    transport: T,
    close: impl FnOnce(T),
    fetch: &mut dyn Fetch,
    sink: &mut dyn Sink,
    clock: &dyn Clock,
    db: (Option<&ItemsBase>, Option<&ExistingCatalogue>),
    budget: impl Fn(usize) -> u32,
    mut each: impl FnMut(&mut Job, &Step),
) -> Run {
    let mut transport = Some(transport);
    let mut close = Some(close);
    let mut log = Vec::new();
    let mut events = Vec::new();
    let mut steps = 0;
    loop {
        let mut closed = Closed;
        let t: &mut dyn Transport = match transport.as_mut() {
            Some(t) => t,
            None => &mut closed,
        };
        let mut io = JobIo {
            transport: t,
            fetch,
            sink,
            clock,
            items: db.0,
            existing: db.1,
        };
        let step = job.step(&mut io, budget(steps));
        steps += 1;
        log.extend(job.take_log());
        events.extend(job.take_events());
        if !job.uses_gearth() {
            if let (Some(t), Some(close)) = (transport.take(), close.take()) {
                close(t);
            }
        }
        each(job, &step);
        if matches!(step, Step::Done(_) | Step::Failed(_)) {
            return Run {
                end: step,
                log,
                events,
                steps,
            };
        }
        assert!(steps < 1_000_000, "the job never ends");
    }
}
