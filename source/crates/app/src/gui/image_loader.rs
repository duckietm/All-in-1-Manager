//! Loads images (furni icons, catalogue images) in the background: reading and decoding happen on a few worker
//! threads, never in the frame, and the last asked comes first, so what is on screen now shows before what was
//! scrolled past. A frame asks with [`ImageLoader::get`] and draws a placeholder until the texture is there.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui;

/// Where an image comes from: the first of these files that exists, or bytes already in memory (fetched from the
/// server).
pub enum Source {
    Files(Vec<PathBuf>),
    Bytes(Arc<Vec<u8>>),
}

/// How far an image is.
#[derive(Clone, PartialEq)]
pub enum State {
    /// Not asked for yet.
    Unknown,
    Pending,
    Ready(egui::TextureHandle),
    /// No file of it exists, or it is not an image.
    Missing,
}

struct Job {
    key: String,
    source: Source,
}

struct Done {
    key: String,
    image: Option<egui::ColorImage>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    done: Vec<Done>,
    ctx: Option<egui::Context>,
}

/// The waiting jobs beyond which the oldest are dropped: they were asked for frames ago and are likely off screen
/// now; asked again, they come back.
const MAX_WAITING: usize = 256;
const WORKERS: usize = 4;

pub struct ImageLoader {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    loaded: HashMap<String, Option<egui::TextureHandle>>,
    pending: HashSet<String>,
}

impl Default for ImageLoader {
    fn default() -> Self {
        Self::new()
    }
}

fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw()))
}

fn work(job: Job) -> Option<egui::ColorImage> {
    match job.source {
        Source::Files(paths) => paths.iter().find_map(|path| std::fs::read(path).ok()).and_then(|bytes| decode(&bytes)),
        Source::Bytes(bytes) => decode(&bytes),
    }
}

impl ImageLoader {
    pub fn new() -> Self {
        let shared: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        for i in 0..WORKERS {
            let shared = shared.clone();
            let _ = std::thread::Builder::new().name(format!("image-loader-{i}")).spawn(move || loop {
                let job = {
                    let (lock, signal) = &*shared;
                    let mut queue = lock.lock().unwrap();
                    loop {
                        // Newest first: what is on screen now.
                        if let Some(job) = queue.jobs.pop_back() {
                            break job;
                        }
                        queue = signal.wait(queue).unwrap();
                    }
                };
                let key = job.key.clone();
                let image = work(job);
                let (lock, _) = &*shared;
                let mut queue = lock.lock().unwrap();
                queue.done.push(Done { key, image });
                if let Some(ctx) = &queue.ctx {
                    ctx.request_repaint();
                }
            });
        }
        ImageLoader { shared, loaded: HashMap::new(), pending: HashSet::new() }
    }

    /// Takes the images the workers finished into textures. Call once per frame, before drawing.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let (done, dropped) = {
            let (lock, _) = &*self.shared;
            let mut queue = lock.lock().unwrap();
            if queue.ctx.is_none() {
                queue.ctx = Some(ctx.clone());
            }
            let mut dropped = Vec::new();
            while queue.jobs.len() > MAX_WAITING {
                if let Some(job) = queue.jobs.pop_front() {
                    dropped.push(job.key);
                }
            }
            (std::mem::take(&mut queue.done), dropped)
        };
        for key in dropped {
            self.pending.remove(&key);
        }
        for Done { key, image } in done {
            self.pending.remove(&key);
            // Pixel art: no smoothing when it is drawn larger.
            let texture = image.map(|image| ctx.load_texture(&key, image, egui::TextureOptions::NEAREST));
            self.loaded.insert(key, texture);
        }
    }

    pub fn state(&self, key: &str) -> State {
        match self.loaded.get(key) {
            Some(Some(texture)) => State::Ready(texture.clone()),
            Some(None) => State::Missing,
            None if self.pending.contains(key) => State::Pending,
            None => State::Unknown,
        }
    }

    /// The texture of `key` when it is loaded; otherwise asks for it (once) from `source` and returns None.
    pub fn get(&mut self, key: &str, source: impl FnOnce() -> Source) -> Option<egui::TextureHandle> {
        match self.state(key) {
            State::Ready(texture) => Some(texture),
            State::Pending | State::Missing => None,
            State::Unknown => {
                self.request(key, source());
                None
            }
        }
    }

    fn request(&mut self, key: &str, source: Source) {
        self.pending.insert(key.to_string());
        let (lock, signal) = &*self.shared;
        lock.lock().unwrap().jobs.push_back(Job { key: key.to_string(), source });
        signal.notify_one();
    }

    /// Forgets every image (changed files are read again).
    pub fn clear(&mut self) {
        self.loaded.clear();
        self.pending.clear();
        self.shared.0.lock().unwrap().jobs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(loader: &mut ImageLoader, ctx: &egui::Context, key: &str) -> State {
        for _ in 0..200 {
            loader.poll(ctx);
            let state = loader.state(key);
            if state != State::Pending {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        loader.state(key)
    }

    #[test]
    fn images_load_in_the_background() {
        let folder = std::env::temp_dir().join(format!("aio-image-loader-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("dot.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255])).save(&file).unwrap();
        let ctx = egui::Context::default();
        let mut loader = ImageLoader::new();
        assert!(loader.get("dot", || Source::Files(vec![folder.join("missing.png"), file.clone()])).is_none(), "never in the frame");
        assert!(loader.state("dot") == State::Pending);
        match wait(&mut loader, &ctx, "dot") {
            State::Ready(texture) => assert_eq!(texture.size(), [3, 2]),
            _ => panic!("not loaded"),
        }
        assert!(loader.get("dot", || panic!("asked twice")).is_some());
        loader.get("none", || Source::Files(vec![folder.join("missing.png")]));
        assert!(wait(&mut loader, &ctx, "none") == State::Missing);
        loader.get("bytes", || Source::Bytes(Arc::new(b"not an image".to_vec())));
        assert!(wait(&mut loader, &ctx, "bytes") == State::Missing);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
