//! Reading, installing and removing `.aiop` packages. `inspect` writes nothing.

use crate::manifest::{valid_id, Manifest};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_PACKAGE: usize = 32 * 1024 * 1024;
pub const MAX_WASM: u64 = 24 * 1024 * 1024;
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_ICON: u64 = 2 * 1024 * 1024;
const MAX_I18N: u64 = 512 * 1024;
const MAX_ENTRIES: usize = 64;

/// A validated package, held in memory.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub wasm: Vec<u8>,
    pub icon: Option<Vec<u8>>,
    /// language code to file content
    pub i18n: BTreeMap<String, Vec<u8>>,
}

fn read_limited(entry: &mut impl Read, limit: u64, name: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // Read one byte past the limit: the zip header may lie about the size.
    entry.take(limit + 1).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        bail!("{name} is too large");
    }
    Ok(out)
}

fn lang_of(name: &str) -> Option<&str> {
    let lang = name.strip_prefix("i18n/")?.strip_suffix(".json")?;
    let ok = (1..=16).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(lang)
}

/// Reads and validates a package. Writes nothing. Any unsafe or unexpected entry fails the whole package.
pub fn inspect(aiop: &[u8], reserved_ids: &[&str]) -> Result<Package> {
    if aiop.len() > MAX_PACKAGE {
        bail!("The package is larger than 32 MiB");
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(aiop)).context("The package is not a zip")?;
    if archive.len() > MAX_ENTRIES {
        bail!("The package has too many files");
    }
    let (mut manifest, mut wasm, mut icon) = (None, None, None);
    let mut i18n = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() || name.starts_with('/') || name.contains('\\') || name.split('/').any(|p| p == "..") {
            bail!("Unsafe path in the package: {name}");
        }
        if entry.is_dir() || entry.is_symlink() {
            bail!("Unexpected entry in the package: {name}");
        }
        let (limit, slot) = match name.as_str() {
            "manifest.json" => (MAX_MANIFEST, 0),
            "plugin.wasm" => (MAX_WASM, 1),
            "icon.png" => (MAX_ICON, 2),
            n if lang_of(n).is_some() => (MAX_I18N, 3),
            _ => bail!("Unexpected file in the package: {name}"),
        };
        if entry.size() > limit {
            bail!("{name} is too large");
        }
        let data = read_limited(&mut entry, limit, &name)?;
        let duplicate = match slot {
            0 => manifest.replace(data).is_some(),
            1 => wasm.replace(data).is_some(),
            2 => icon.replace(data).is_some(),
            _ => i18n.insert(lang_of(&name).unwrap_or_default().to_string(), data).is_some(),
        };
        if duplicate {
            bail!("Duplicate file in the package: {name}");
        }
    }
    let manifest = Manifest::parse(&manifest.context("manifest.json is missing")?)?;
    let wasm = wasm.context("plugin.wasm is missing")?;
    if wasm.is_empty() {
        bail!("plugin.wasm is empty");
    }
    if reserved_ids.contains(&manifest.id.as_str()) {
        bail!("The id {} belongs to a built-in plugin", manifest.id);
    }
    Ok(Package { manifest, wasm, icon, i18n })
}

fn manifest_json(m: &Manifest) -> Result<String> {
    let mut value = serde_json::json!({
        "id": m.id, "name": m.name, "version": m.version, "author": m.author,
        "description": m.description, "api": m.api,
        "permissions": m.permissions.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        "page_title": m.page_title,
    });
    if let Some(t) = &m.tile {
        value["tile"] = serde_json::json!({ "title": t.title, "description": t.description, "color": t.color });
    }
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Writes the new version's code into `dir`. No `data/`: the data is carried in later, by a rename.
fn write_all(pkg: &Package, dir: &Path) -> Result<()> {
    std::fs::write(dir.join("manifest.json"), manifest_json(&pkg.manifest)?)?;
    std::fs::write(dir.join("plugin.wasm"), &pkg.wasm)?;
    if let Some(icon) = &pkg.icon {
        std::fs::write(dir.join("icon.png"), icon)?;
    }
    if !pkg.i18n.is_empty() {
        std::fs::create_dir_all(dir.join("i18n"))?;
        for (lang, data) in &pkg.i18n {
            std::fs::write(dir.join("i18n").join(format!("{lang}.json")), data)?;
        }
    }
    Ok(())
}

/// Failure injection for the tests (exported only with the `test-hooks` feature).
mod fault {
    /// The steps of `install`, in order (see the crash table on [`super::install`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Step {
        Recover,
        Build,
        CarryData,
        MoveAside,
        PutInPlace,
        DropOld,
    }

    #[cfg(feature = "test-hooks")]
    impl Step {
        pub const ALL: [Step; 6] = [Step::Recover, Step::Build, Step::CarryData, Step::MoveAside, Step::PutInPlace, Step::DropOld];
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct Fault {
        /// This step fails with an I/O error; the normal rollback runs. (`Recover` cannot be failed this way.)
        pub fail: Option<Step>,
        /// The process dies right after this step: `install` returns at once, no rollback, nothing cleaned.
        pub crash_after: Option<Step>,
        /// During a rollback, moving the data back out of `.partial` fails.
        pub fail_data_restore: bool,
    }

    impl Fault {
        pub(super) fn fail(&self, step: Step) -> std::io::Result<()> {
            match self.fail == Some(step) {
                true => Err(std::io::Error::other(format!("injected failure at {step:?}"))),
                false => Ok(()),
            }
        }

        pub(super) fn crash(&self, step: Step) -> anyhow::Result<()> {
            match self.crash_after == Some(step) {
                true => anyhow::bail!("injected crash after {step:?}"),
                false => Ok(()),
            }
        }
    }
}

#[cfg(feature = "test-hooks")]
pub use fault::{Fault, Step};
use fault::{Fault as Inject, Step as S};

/// Installs (or upgrades) into `root/<id>`, keeping the plugin's private `data/` folder.
///
/// Three folders take part: live `<id>`, `.<id>.partial` (the new version being built) and `.<id>.old` (the
/// previous version moved aside). Data only ever moves by renaming a whole `data/` folder, which is atomic, so it
/// is never copied and never half-moved.
///
/// **Invariant.** At every instant the plugin data is complete in exactly one of `<id>/data`, `.<id>.partial/data`,
/// `.<id>.old/data`, and no folder is deleted while it holds a non-empty `data/`: every delete goes through
/// `discard`, which first removes `data/` with `remove_dir`, a call the OS refuses for a non-empty folder.
/// `Ok` is returned only when the data is in `<id>/data`.
///
/// Steps (`Step`) and what a crash right after each one leaves:
///
/// | step          | does                                            | data is in               | next `install` recovers by          |
/// |---------------|-------------------------------------------------|--------------------------|-------------------------------------|
/// | 0 Recover     | normalises leftovers (below)                    | live (or nowhere yet)    | -                                   |
/// | 1 Build       | writes the new code into `.partial` (no data)   | live                     | discarding `.partial` (no data)     |
/// | 2 CarryData   | renames `live/data` to `.partial/data`          | `.partial`               | renaming it back into live          |
/// | 3 MoveAside   | renames live to `.old`                          | `.partial`               | `.old` back to live, data back too  |
/// | 4 PutInPlace  | renames `.partial` to live                      | live (new version)       | discarding `.old` (no data)         |
/// | 5 DropOld     | discards `.old`                                 | live                     | nothing left to do                  |
///
/// A fresh install creates an empty `.partial/data` in step 2. Rollbacks keep the invariant: a failure in step
/// 1 or 2 discards `.partial` (the data never left live); in step 3 or 4 `.old` goes back to live, then the data
/// goes back from `.partial` to live, then `.partial` is discarded. If any rename back fails, the remaining
/// folders are left as they are: they are one of the crash states above and the next install recovers them.
/// A failure to drop `.old` (step 5) is ignored: it holds no data and the next install discards it.
///
/// **Recovery** (step 0) turns any leftover combination into "live folder (maybe missing), holding the only data":
/// 1. live missing and `.old` present: crash after step 3; rename `.old` to live.
/// 2. Among `.partial/data`, `live/data`, `.old/data` the non-empty ones hold data (an empty `data/` holds nothing
///    to lose). The first in that order wins: it is the most recent, since data only flows live -> `.partial`
///    and `.old` only held data in an aside made by older builds. The winner is renamed to `live/data` (creating
///    live if needed, and removing an empty `live/data` in the way). This algorithm never leaves two non-empty
///    copies; if some other cause did, each loser is renamed to `.<id>.data-from-<where>` (never deleted).
/// 3. `.partial` and `.old` are discarded; they hold no data now. A file in their place is an error, not deleted.
///
/// Every recovery action is a single rename or `discard`, so a crash inside recovery leaves another state of
/// this table, which the next install recovers in turn.
pub fn install(pkg: &Package, root: &Path) -> Result<PathBuf> {
    install_inner(pkg, root, Inject::default())
}

/// Test hook: `install` with failures or a crash injected.
#[cfg(feature = "test-hooks")]
pub fn install_with_fault(pkg: &Package, root: &Path, fault: Fault) -> Result<PathBuf> {
    install_inner(pkg, root, fault)
}

/// Test hook: behaves like `install` but fails the final rename, after the old version was moved aside.
#[cfg(feature = "test-hooks")]
pub fn install_failing_after_move_aside(pkg: &Package, root: &Path) -> Result<PathBuf> {
    install_inner(pkg, root, Inject { fail: Some(S::PutInPlace), ..Default::default() })
}

/// Test hook: like the above, and moving the data back to the restored version fails too.
#[cfg(feature = "test-hooks")]
pub fn install_failing_with_data_restore_failure(pkg: &Package, root: &Path) -> Result<PathBuf> {
    install_inner(pkg, root, Inject { fail: Some(S::PutInPlace), crash_after: None, fail_data_restore: true })
}

struct Slots {
    live: PathBuf,
    partial: PathBuf,
    old: PathBuf,
}

/// Deletes a leftover folder, but never one whose `data/` has anything in it: `data/` is removed first with
/// `remove_dir`, which fails on a non-empty folder, and then the folder holds only code.
fn discard(dir: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(dir) else { return Ok(()) };
    if !meta.is_dir() {
        bail!("{} is in the way (not a folder)", dir.display());
    }
    let data = dir.join("data");
    if std::fs::symlink_metadata(&data).is_ok() {
        std::fs::remove_dir(&data).with_context(|| format!("{} holds plugin data; it was kept", data.display()))?;
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("Could not remove {}", dir.display()))
}

/// A `data/` folder with something in it. Unreadable counts as non-empty (never treated as disposable).
fn holds_data(data: &Path) -> bool {
    data.is_dir() && std::fs::read_dir(data).map(|mut entries| entries.next().is_some()).unwrap_or(true)
}

/// Step 0 (see `install`): afterwards `.partial` and `.old` are gone and any data is in `live/data`.
fn recover(s: &Slots, root: &Path, id: &str) -> Result<()> {
    if !s.live.exists() && s.old.exists() {
        std::fs::rename(&s.old, &s.live).context("Could not recover the previous version")?;
    }
    let live_data = s.live.join("data");
    let found: Vec<(&str, PathBuf)> = [("partial", &s.partial), ("live", &s.live), ("old", &s.old)]
        .into_iter()
        .map(|(name, dir)| (name, dir.join("data")))
        .filter(|(_, data)| holds_data(data))
        .collect();
    if let Some(((_, winner), losers)) = found.split_first() {
        for (name, loser) in losers {
            let backup = backup_name(root, id, name)?;
            std::fs::rename(loser, &backup).with_context(|| format!("Could not keep {} aside", loser.display()))?;
        }
        if *winner != live_data {
            std::fs::create_dir_all(&s.live)?;
            if live_data.exists() {
                // Not a winner, so empty: `remove_dir` refuses anything else.
                std::fs::remove_dir(&live_data).with_context(|| format!("Could not clear {}", live_data.display()))?;
            }
            std::fs::rename(winner, &live_data).context("Could not recover the plugin data")?;
        }
    }
    discard(&s.partial)?;
    discard(&s.old)
}

/// Joins a step's error with the error of its rollback, if the rollback failed too.
fn with_rollback(error: anyhow::Error, rollback: Result<()>) -> anyhow::Error {
    match rollback {
        Ok(()) => error,
        Err(r) => anyhow::anyhow!("{error:#}. {r:#}"),
    }
}

fn install_inner(pkg: &Package, root: &Path, inject: Inject) -> Result<PathBuf> {
    let id = &pkg.manifest.id;
    if !valid_id(id) {
        bail!("Invalid plugin id: {id}");
    }
    std::fs::create_dir_all(root)?;
    let s = Slots { live: root.join(id), partial: root.join(format!(".{id}.partial")), old: root.join(format!(".{id}.old")) };
    let (live_data, new_data) = (s.live.join("data"), s.partial.join("data"));

    // 0 Recover.
    recover(&s, root, id)?;
    inject.crash(S::Recover)?;

    // 1 Build: the new code in `.partial`, no data there yet.
    std::fs::create_dir(&s.partial).with_context(|| format!("Could not create {}", s.partial.display()))?;
    let built = inject.fail(S::Build).map_err(anyhow::Error::from).and_then(|_| write_all(pkg, &s.partial));
    if let Err(error) = built {
        return Err(with_rollback(error, discard(&s.partial)));
    }
    inject.crash(S::Build)?;

    // 2 CarryData: one rename; on failure the data is still in live.
    let had_live = s.live.exists();
    let carried = inject.fail(S::CarryData).and_then(|_| match live_data.is_dir() {
        true => std::fs::rename(&live_data, &new_data),
        false => std::fs::create_dir(&new_data),
    });
    if let Err(error) = carried {
        let error = anyhow::Error::from(error).context("Could not carry the plugin data over");
        return Err(with_rollback(error, discard(&s.partial)));
    }
    inject.crash(S::CarryData)?;

    // Rollback of step 2, used once live is back in place: data back to live, then drop `.partial`.
    // If the data cannot move back, `.partial` is kept (it holds the data) and the next install recovers it.
    let give_back = || -> Result<()> {
        let back = match (inject.fail_data_restore, had_live) {
            (true, _) => Err(std::io::Error::other("injected failure")),
            (false, true) => std::fs::rename(&new_data, &live_data),
            (false, false) => Ok(()), // a fresh install: `.partial/data` is the empty folder made above
        };
        back.with_context(|| format!("Could not move the plugin data back; it is kept in {}", new_data.display()))?;
        let _ = discard(&s.partial); // holds no data now; a leftover is discarded by the next install
        Ok(())
    };

    // 3 MoveAside.
    if had_live {
        if let Err(error) = inject.fail(S::MoveAside).and_then(|_| std::fs::rename(&s.live, &s.old)) {
            let error = anyhow::Error::from(error).context(format!("Could not move {} aside", s.live.display()));
            return Err(with_rollback(error, give_back()));
        }
    }
    inject.crash(S::MoveAside)?;

    // 4 PutInPlace: the new version, data included, becomes live.
    if let Err(error) = inject.fail(S::PutInPlace).and_then(|_| std::fs::rename(&s.partial, &s.live)) {
        let error = anyhow::Error::from(error).context(format!("Could not put the plugin in {}", s.live.display()));
        if had_live {
            if let Err(back) = std::fs::rename(&s.old, &s.live) {
                // Crash state after step 3: the next install recovers it.
                let back = anyhow::Error::from(back).context("Could not restore the previous version; the next install recovers it");
                return Err(with_rollback(error, Err(back)));
            }
        }
        return Err(with_rollback(error, give_back()));
    }
    inject.crash(S::PutInPlace)?;

    // 5 DropOld: holds no data; a failure is harmless (the next install discards it).
    if had_live {
        let _ = inject.fail(S::DropOld).map_err(anyhow::Error::from).and_then(|_| discard(&s.old));
    }
    Ok(s.live)
}


/// Installs `pkg` as another plugin that only happens to share the installed one's id (another author, Ruling 22):
/// the installed plugin's private data is not handed over. Leftovers of an interrupted install are recovered first,
/// so every piece of the old data is in `<id>/data`; that folder is then renamed to
/// `.<id>.data-from-previous-author` (`-2`, `-3`, … if taken; never deleted, the admin can restore it by hand), and
/// the new version starts with an empty `data/`. Returns the plugin folder and where the old data went, if it had any.
pub fn install_without_data(pkg: &Package, root: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let id = &pkg.manifest.id;
    if !valid_id(id) {
        bail!("Invalid plugin id: {id}");
    }
    let s = Slots { live: root.join(id), partial: root.join(format!(".{id}.partial")), old: root.join(format!(".{id}.old")) };
    let data = s.live.join("data");
    let mut aside = None;
    if root.exists() {
        recover(&s, root, id)?;
        // An empty `data/` holds nothing of the old plugin: it can stay.
        if holds_data(&data) {
            let backup = backup_name(root, id, "previous-author")?;
            std::fs::rename(&data, &backup).context("Could not set the previous author's data aside")?;
            aside = Some(backup);
        }
    }
    Ok((install(pkg, root)?, aside))
}

/// A free `.<id>.data-from-<what>` name in `root` (`-2` … `-999` when taken).
fn backup_name(root: &Path, id: &str, what: &str) -> Result<PathBuf> {
    (1..1000)
        .map(|n| root.join(if n == 1 { format!(".{id}.data-from-{what}") } else { format!(".{id}.data-from-{what}-{n}") }))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .context("No free name to keep a second copy of the plugin data")
}

/// Removes everything of the plugin in `root`: its folder with its private data, and the leftovers of an interrupted
/// install (`.<id>.partial`, `.<id>.old`) and the data copies kept aside (`.<id>.data-from-*`), so nothing of it can
/// come back with a later install of the same id.
pub fn remove(id: &str, root: &Path) -> Result<()> {
    if !valid_id(id) {
        bail!("Invalid plugin id: {id}");
    }
    let mut targets = vec![root.join(id), root.join(format!(".{id}.partial")), root.join(format!(".{id}.old"))];
    let aside = format!(".{id}.data-from-");
    if let Ok(read) = std::fs::read_dir(root) {
        targets.extend(read.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&aside)).map(|e| e.path()));
    }
    for target in targets {
        let Ok(meta) = std::fs::symlink_metadata(&target) else { continue };
        let removed = if meta.is_dir() { std::fs::remove_dir_all(&target) } else { std::fs::remove_file(&target) };
        removed.with_context(|| format!("Could not remove {}", target.display()))?;
    }
    Ok(())
}

/// Manifests of the installed plugins; folders with a broken manifest and `.partial`/`.old` leftovers are skipped.
pub fn installed(root: &Path) -> Vec<Manifest> {
    let Ok(read) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Manifest> = read
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir())
        .filter_map(|e| {
            let manifest = Manifest::parse(&std::fs::read(e.path().join("manifest.json")).ok()?).ok()?;
            (e.file_name().to_string_lossy() == manifest.id.as_str()).then_some(manifest)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}
