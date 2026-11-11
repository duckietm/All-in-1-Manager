//! The asset tools: a folder of SWF, NITRO and HAB files and what can be done with them, file by file: decompile a .swf
//! for debugging, convert a .swf or .nitro to .hab, decompile a .hab into its JSON and image to edit, and compile such
//! a folder into a .hab again. (All-in-1 Converter: Tools/SwfDecompiler.cs, NitroCompiler/BundleTools.cs,
//! NitroToHabConverter.cs, SWF_*_To_Nitro.cs)
//!
//! ```text
//! Assets_tools/
//!   Original_SWF_Files/<type>/      .swf to convert or decompile
//!   Original_NITRO_Files/<type>/    .nitro to convert
//!   converted/<type>/               the .hab of every SWF/NITRO to HAB
//!   Decompiled_SWF_Files/<type>/<name>/   images/, binaryData/, symbolClass/symbols.csv
//!   Decompiled_HAB_Files/<type>/<name>/   the files of a .hab (its JSON and image), to edit
//!   Compiled_HAB_Files/<type>/      the edited folders packed as .hab
//! ```

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::bundle::AssetKind;

pub const ORIGINAL_SWF: &str = "Original_SWF_Files";
pub const ORIGINAL_NITRO: &str = "Original_NITRO_Files";
pub const CONVERTED: &str = "converted";
pub const DECOMPILED_SWF: &str = "Decompiled_SWF_Files";
pub const DECOMPILED_HAB: &str = "Decompiled_HAB_Files";
pub const COMPILED_HAB: &str = "Compiled_HAB_Files";

/// The asset types, with their folder name below every tool folder.
pub const TYPES: [(AssetKind, &str); 4] = [(AssetKind::Furniture, "furni"), (AssetKind::Clothes, "clothes"), (AssetKind::Pets, "pets"), (AssetKind::Effects, "effects")];

pub fn type_folder(kind: AssetKind) -> &'static str {
    TYPES.iter().find(|(k, _)| *k == kind).map_or("furni", |(_, f)| f)
}

/// Every folder of the layout, created so the user sees where to put files.
pub fn create_layout(root: &Path) -> std::io::Result<()> {
    for top in [ORIGINAL_SWF, ORIGINAL_NITRO, CONVERTED, DECOMPILED_SWF, DECOMPILED_HAB, COMPILED_HAB] {
        for (_, folder) in TYPES {
            std::fs::create_dir_all(root.join(top).join(folder))?;
        }
    }
    Ok(())
}

/// What a run did: (done, skipped, failed) and the first errors.
#[derive(Debug, Default)]
pub struct Outcome {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

impl Outcome {
    pub fn summary(&self, what: &str) -> String {
        let mut text = format!("{} {what}", self.done);
        if self.skipped > 0 {
            text += &format!(", {} already there", self.skipped);
        }
        if self.failed > 0 {
            text += &format!(", {} failed", self.failed);
        }
        text
    }
}

/// The files with one of `extensions` directly in `folder`, sorted.
pub fn files(folder: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| extensions.iter().any(|e| x.eq_ignore_ascii_case(e))))
        .collect();
    files.sort();
    files
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    std::fs::write(&part, data)?;
    std::fs::rename(&part, path)?;
    Ok(())
}

/// Runs `work` on every file, on all CPU cores but one; `stopped` is asked between files. `progress` gets every
/// finished file's line ("name: ok" or the error).
pub fn run_all<T: Sync>(
    items: &[T],
    stopped: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(&str) + Sync),
    work: &(dyn Fn(&T) -> Result<Option<String>> + Sync),
) -> Outcome {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    let next = AtomicUsize::new(0);
    let outcome = Mutex::new(Outcome::default());
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).max(1)).min(items.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                if stopped() {
                    return;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { return };
                let result = work(item);
                let mut o = outcome.lock().unwrap();
                match result {
                    Ok(Some(line)) => {
                        o.done += 1;
                        drop(o);
                        progress(&line);
                    }
                    Ok(None) => o.skipped += 1,
                    Err(e) => {
                        o.failed += 1;
                        let line = format!("! {e}");
                        if o.errors.len() < 20 {
                            o.errors.push(line.clone());
                        }
                        drop(o);
                        progress(&line);
                    }
                }
            });
        }
    });
    outcome.into_inner().unwrap()
}

/// A .swf exported for debugging, like FFDEC's export: images/<id>_<symbol>.png, binaryData/<id>_<symbol>.bin (the
/// XML), symbolClass/symbols.csv. (ActionScript and sounds are not exported.)
pub fn decompile_swf(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let swf = crate::swf::read(data, true)?;
    let mut files = Vec::new();
    let csv: String = swf.symbols.iter().map(|(id, name)| format!("{id};\"{name}\"\n")).collect();
    files.push(("symbolClass/symbols.csv".to_string(), csv.into_bytes()));
    for binary in &swf.binaries {
        files.push((format!("binaryData/{}.bin", binary.export_name()), binary.data.clone()));
    }
    for bitmap in &swf.bitmaps {
        let mut png = Vec::new();
        bitmap.image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| anyhow!("{e}"))?;
        files.push((format!("images/{}.png", bitmap.export_name()), png));
    }
    Ok(files)
}

/// The files of a .hab or .nitro exactly as stored (BundleTools.DecompileAsync).
pub fn decompile_bundle(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if crate::hab::is_hab(data) {
        crate::hab::read(data)
    } else {
        crate::nitro::read(data)
    }
}

/// Writes `files` (relative paths) below `folder`, replacing what was there.
fn write_tree(folder: &Path, files: &[(String, Vec<u8>)]) -> Result<()> {
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    for (name, data) in files {
        let mut path = folder.to_path_buf();
        for part in name.split('/').filter(|p| !p.is_empty() && *p != "..") {
            path.push(part);
        }
        write(&path, data)?;
    }
    Ok(())
}

/// Decompile SWF files: every .swf of `source` into `output`/<name>/.
pub fn decompile_swf_folder(source: &Path, output: &Path, stopped: &(dyn Fn() -> bool + Sync), progress: &(dyn Fn(&str) + Sync)) -> Outcome {
    let items = files(source, &["swf"]);
    run_all(&items, stopped, progress, &|path| {
        let name = stem(path);
        let data = std::fs::read(path)?;
        let files = decompile_swf(&data).map_err(|e| anyhow!("{name}.swf: {e}"))?;
        write_tree(&output.join(&name), &files)?;
        Ok(Some(format!("{name}.swf: {} files", files.len())))
    })
}

/// SWF or NITRO to HAB: every .swf (or .nitro) of `source` into `output`/<name>.hab. A .hab that is there already is
/// skipped unless `overwrite`.
pub fn convert_folder(
    source: &Path,
    output: &Path,
    kind: AssetKind,
    extension: &str,
    overwrite: bool,
    stopped: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(&str) + Sync),
) -> Outcome {
    let items = files(source, &[extension]);
    run_all(&items, stopped, progress, &|path| {
        let name = stem(path);
        let target = output.join(format!("{name}.hab"));
        if target.is_file() && !overwrite {
            return Ok(None);
        }
        let data = std::fs::read(path)?;
        let hab = crate::bundle::to_hab(&name, kind, &data).map_err(|e| anyhow!("{name}.{extension}: {e}"))?;
        write(&target, &hab)?;
        Ok(Some(format!("{name}.{extension} → {name}.hab ({} KB)", hab.len().div_ceil(1024))))
    })
}

/// Decompile HAB files: every .hab (or .nitro) of `source` into `output`/<name>/ (its JSON and image).
pub fn decompile_hab_folder(source: &Path, output: &Path, stopped: &(dyn Fn() -> bool + Sync), progress: &(dyn Fn(&str) + Sync)) -> Outcome {
    let items = files(source, &["hab", "nitro"]);
    run_all(&items, stopped, progress, &|path| {
        let name = stem(path);
        let data = std::fs::read(path)?;
        let files = decompile_bundle(&data).map_err(|e| anyhow!("{}: {e}", path.file_name().unwrap_or_default().to_string_lossy()))?;
        let flat: Vec<(String, Vec<u8>)> =
            files.into_iter().map(|(n, d)| (n.rsplit(['/', '\\']).next().unwrap_or(&n).to_string(), d)).collect();
        write_tree(&output.join(&name), &flat)?;
        Ok(Some(format!("{name}: {}", flat.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "))))
    })
}

/// Compile HAB files: every folder of `source` (its files, as edited) into `output`/<folder name>.hab.
pub fn compile_hab_folder(source: &Path, output: &Path, stopped: &(dyn Fn() -> bool + Sync), progress: &(dyn Fn(&str) + Sync)) -> Outcome {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(source).map(|e| e.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    folders.sort();
    run_all(&folders, stopped, progress, &|folder| {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        entries.sort();
        let files: Vec<(String, Vec<u8>)> = entries
            .iter()
            .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .map(|p| Ok((p.file_name().unwrap_or_default().to_string_lossy().into_owned(), std::fs::read(p)?)))
            .collect::<Result<_>>()?;
        if files.is_empty() {
            return Err(anyhow!("{name}: the folder is empty"));
        }
        let hab = crate::hab::write(&name, &files);
        write(&output.join(format!("{name}.hab")), &hab)?;
        Ok(Some(format!("{name} → {name}.hab ({} files)", files.len())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompile_edit_compile_round_trip() {
        let root = tempfile::tempdir().unwrap();
        create_layout(root.path()).unwrap();
        let files = vec![("chair.json".to_string(), b"{\"name\":\"chair\"}".to_vec()), ("chair.png".to_string(), vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5])];
        let hab = crate::hab::write("chair", &files);
        let converted = root.path().join(CONVERTED).join("furni");
        std::fs::write(converted.join("chair.hab"), &hab).unwrap();
        // A .nitro converts to .hab with the same files; a second run skips it.
        std::fs::write(root.path().join(ORIGINAL_NITRO).join("furni/lamp.nitro"), crate::nitro::write(&files)).unwrap();
        let never = || false;
        let quiet = |_: &str| {};
        let o = convert_folder(&root.path().join(ORIGINAL_NITRO).join("furni"), &converted, AssetKind::Furniture, "nitro", false, &never, &quiet);
        assert_eq!((o.done, o.failed), (1, 0));
        assert_eq!(crate::hab::read(&std::fs::read(converted.join("lamp.hab")).unwrap()).unwrap(), files);
        assert_eq!(convert_folder(&root.path().join(ORIGINAL_NITRO).join("furni"), &converted, AssetKind::Furniture, "nitro", false, &never, &quiet).skipped, 1);

        let decompiled = root.path().join(DECOMPILED_HAB).join("furni");
        let o = decompile_hab_folder(&converted, &decompiled, &never, &quiet);
        assert_eq!(o.done, 2);
        assert_eq!(std::fs::read(decompiled.join("chair/chair.json")).unwrap(), files[0].1);
        // Edit the JSON, compile again.
        std::fs::write(decompiled.join("chair/chair.json"), b"{\"name\":\"edited\"}").unwrap();
        let compiled = root.path().join(COMPILED_HAB).join("furni");
        assert_eq!(compile_hab_folder(&decompiled, &compiled, &never, &quiet).done, 2);
        let back = crate::hab::read(&std::fs::read(compiled.join("chair.hab")).unwrap()).unwrap();
        assert_eq!(back[0], ("chair.json".to_string(), b"{\"name\":\"edited\"}".to_vec()));
        assert_eq!(back[1], files[1]);
    }

    #[test]
    fn a_damaged_file_fails_alone_and_stop_stops() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("in");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("bad.swf"), b"not a swf").unwrap();
        let lines = std::sync::Mutex::new(Vec::new());
        let log = |l: &str| lines.lock().unwrap().push(l.to_string());
        let o = decompile_swf_folder(&source, &root.path().join("out"), &|| false, &log);
        assert_eq!((o.done, o.failed), (0, 1));
        assert!(lines.lock().unwrap()[0].starts_with("! bad.swf:"));
        let o = decompile_swf_folder(&source, &root.path().join("out"), &|| true, &log);
        assert_eq!(o.done + o.failed, 0);
    }

    /// The converter's real pets: decompiled for debugging and converted to .hab, when its repository is next to this one.
    #[test]
    fn real_pets_through_the_tools_when_present() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../duckietm/all-in-1-converter/Compiled/SWFCompiler/import/pets");
        if !source.is_dir() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let o = decompile_swf_folder(&source, &root.path().join("swf"), &|| false, &|_| {});
        assert!(o.done > 30 && o.failed == 0, "{o:?}");
        assert!(root.path().join("swf/dog/symbolClass/symbols.csv").is_file());
        assert!(std::fs::read_dir(root.path().join("swf/dog/images")).unwrap().count() > 10);
        let o = convert_folder(&source, &root.path().join("hab"), AssetKind::Pets, "swf", false, &|| false, &|_| {});
        assert!(o.done > 30 && o.failed == 0, "{o:?}");
    }
}
