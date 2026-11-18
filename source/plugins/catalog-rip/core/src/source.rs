//! Reading files the job did not write: an earlier rip (its `packets/` to go on with it or
//! write it again, its catalog.json to compare with) and a G-Earth Packet Logger export.
//!
//! [`Source`] mirrors the plugin SDK's picked folder (`Folder::read` / `Folder::list`): paths
//! are relative to the folder the rips are made in, `/`-separated, like the [`crate::job::Sink`]
//! paths. A read returns at most [`MAX_READ`] bytes, so a big file is read in pieces and never
//! held whole.

use std::collections::BTreeMap;

/// The most a [`Source::read`] returns (and asks for).
pub const MAX_READ: u32 = 4 * 1024 * 1024;

/// The most entries a [`Source::list`] returns.
pub const MAX_LIST: usize = 10_000;

/// One entry of a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub name: String,
    pub is_folder: bool,
    /// Bytes (0 for a folder).
    pub size: u64,
}

/// Files the job may read.
pub trait Source {
    /// At most `max` bytes (`max` ≤ [`MAX_READ`]) of the file `path` from byte `offset`: fewer
    /// at the end of the file, none past it. A missing file is an error.
    fn read(&mut self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, String>;

    /// The entries of the folder `path` ("" for the top), sorted by name, from entry `start`:
    /// at most [`MAX_LIST`]; the next page starts where this one ended, a page past the last
    /// entry is empty. A missing folder is an error.
    fn list(&mut self, path: &str, start: u32) -> Result<Vec<SourceEntry>, String>;
}

/// Reads the whole file `path` (for the small ones: session.json, a recorded packet), in pieces
/// of [`MAX_READ`].
pub fn read_all(source: &mut dyn Source, path: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let piece = source.read(path, out.len() as u64, MAX_READ)?;
        if piece.is_empty() {
            return Ok(out);
        }
        out.extend_from_slice(&piece);
    }
}

/// Every entry of the folder `path`, page by page.
pub fn list_all(source: &mut dyn Source, path: &str) -> Result<Vec<SourceEntry>, String> {
    let mut out = Vec::new();
    loop {
        let page = source.list(path, out.len() as u32)?;
        if page.is_empty() {
            return Ok(out);
        }
        out.extend(page);
    }
}

/// `a/b` from a folder and a name (`b` alone at the top).
pub fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// Files in memory, by path (the folders are those the paths name). For tests, and for a caller
/// that has the files already.
#[derive(Debug, Clone, Default)]
pub struct MemSource {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Calls of [`Source::read`] and the most bytes one asked for (tests check the pieces).
    pub reads: usize,
    pub largest_read: u32,
}

impl MemSource {
    pub fn new() -> MemSource {
        MemSource::default()
    }

    /// `prefix/<path>` for every file of `files` (a sink's files, say).
    pub fn with_files<'a>(files: impl IntoIterator<Item = (&'a String, &'a Vec<u8>)>) -> MemSource {
        MemSource {
            files: files
                .into_iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ..MemSource::default()
        }
    }
}

impl Source for MemSource {
    fn read(&mut self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, String> {
        self.reads += 1;
        self.largest_read = self.largest_read.max(max);
        if max > MAX_READ {
            return Err(format!(
                "permission denied: reads are at most {MAX_READ} bytes"
            ));
        }
        let data = self.files.get(path).ok_or("file not found")?;
        let start = (offset.min(data.len() as u64)) as usize;
        let end = (start + max as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    fn list(&mut self, path: &str, start: u32) -> Result<Vec<SourceEntry>, String> {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut entries: BTreeMap<String, SourceEntry> = BTreeMap::new();
        for (file, data) in &self.files {
            let Some(rest) = file.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((folder, _)) => {
                    entries.entry(folder.to_string()).or_insert(SourceEntry {
                        name: folder.to_string(),
                        is_folder: true,
                        size: 0,
                    });
                }
                None => {
                    entries.insert(
                        rest.to_string(),
                        SourceEntry {
                            name: rest.to_string(),
                            is_folder: false,
                            size: data.len() as u64,
                        },
                    );
                }
            }
        }
        if entries.is_empty() && !path.is_empty() {
            return Err("file not found".into());
        }
        Ok(entries
            .into_values()
            .skip(start as usize)
            .take(MAX_LIST)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_pieces_and_lists_folders() {
        let mut s = MemSource::new();
        s.files.insert("a/x.bin".into(), vec![1, 2, 3]);
        s.files.insert("a/b/y.bin".into(), vec![4]);
        s.files.insert("z.txt".into(), vec![]);
        assert_eq!(s.read("a/x.bin", 1, 1).unwrap(), [2]);
        assert!(s.read("a/x.bin", 9, 4).unwrap().is_empty());
        assert!(s.read("nope", 0, 4).is_err());
        assert!(s.read("a/x.bin", 0, MAX_READ + 1).is_err());
        assert_eq!(read_all(&mut s, "a/x.bin").unwrap(), [1, 2, 3]);
        let top: Vec<(String, bool)> = list_all(&mut s, "")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.is_folder))
            .collect();
        assert_eq!(top, [("a".to_string(), true), ("z.txt".to_string(), false)]);
        let a = s.list("a", 0).unwrap();
        assert_eq!(a.len(), 2);
        assert!(s.list("a", 2).unwrap().is_empty());
        assert!(s.list("q", 0).is_err());
        assert_eq!(join("", "f"), "f");
        assert_eq!(join("d", "f"), "d/f");
    }
}
