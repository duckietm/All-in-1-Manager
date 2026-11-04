//! Custom assets from a Nitro retro: the URLs of its renderer-config.json (furnidata.url, furni.asset.url,
//! furni.asset.icon.url, avatar.asset.url, avatar.figuredata.url, avatar.figuremap.url), downloaded into a folder of
//! their own laid out like gamedata, to upload by hand. A retro serves .hab, .nitro or .swf: every bundle ends up as
//! .hab (a .nitro repacked, a .swf converted); a .swf that cannot be converted stays in source/. (All-in-1 Converter: NitroFurniture.cs, NitroClothes.cs)

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use aio_assets::{AssetKind, Format};
use futures::stream::{self, StreamExt};
use serde_json::Value;

use super::{convert, error, save, Counts, Downloader, Item, Kind, PARALLEL};
use crate::server::Result;

/// The folder below the assets folder for a retro's custom assets (its own gamedata/ and source/).
pub const CUSTOM: &str = "custom";

pub use aio_core::settings::RetroUrls;

/// A folder URL without a trailing slash, and without a "/%libname%..." file pattern after it.
pub fn folder_url(url: &str) -> String {
    let url = url.trim();
    let url = match url.find("%libname%") {
        Some(at) => &url[..at],
        None => url,
    };
    url.trim_end_matches('/').to_string()
}

impl Downloader<'_> {
    /// A JSON or XML file of the retro, fresh (see [`fresh`]); when the address with the timestamp fails, the address
    /// as given is tried once more.
    async fn fresh_text(&self, url: &str) -> Result<String> {
        // A file on this computer (saved from the browser, for example) instead of a link.
        if let Some(path) = local_file(url) {
            (self.log)(&format!("  reading {}", path.display()));
            let data = std::fs::read(&path).map_err(|e| error(format!("{}: {e}", path.display())))?;
            return Ok(String::from_utf8_lossy(data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&data)).into_owned());
        }
        let with_time = fresh(url);
        match self.text(&with_time).await {
            Err(_) if with_time != url.trim() => self.text(url.trim()).await,
            other => other,
        }
    }
}

/// The path when a JSON field holds a file on this computer instead of a link (a plain path or file:///...).
pub fn local_file(value: &str) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"');
    if value.starts_with("http://") || value.starts_with("https://") || value.is_empty() {
        return None;
    }
    let value = value.strip_prefix("file:///").or_else(|| value.strip_prefix("file://")).unwrap_or(value);
    Some(PathBuf::from(value))
}

/// Why an answer is not a bundle of `format`: its size, its first bytes, and what the reader says.
fn rejection(format: Format, data: &[u8]) -> String {
    let head: Vec<String> = data.iter().take(8).map(|b| format!("{b:02X}")).collect();
    let reason = match format {
        Format::Nitro => aio_assets::nitro::read(data).err().map(|e| e.to_string()),
        Format::Hab => aio_assets::hab::read(data).err().map(|e| e.to_string()),
        Format::Swf => (!aio_assets::bundle::is_swf(data)).then(|| "no FWS/CWS/ZWS header".to_string()),
    };
    let text = if data.first() == Some(&b'<') { " (an HTML page)" } else { "" };
    format!(
        "the answer is not a .{} ({} bytes, starts with {}{text}){}",
        format.extension(),
        data.len(),
        head.join(" "),
        reason.map(|r| format!(": {r}")).unwrap_or_default()
    )
}

/// XML (a Flash retro's or Habbo's furnidata, figuredata or figuremap) rather than JSON.
fn is_xml(text: &str) -> bool {
    text.trim_start_matches('\u{feff}').trim_start().starts_with('<')
}

/// The URL with a timestamp, so a cache in front of the retro gives the newest file (as the converter does).
/// An address that has a query already (a "?v=..." version of the retro's own) is used exactly as given: some CDNs
/// answer 404 to a query they do not know.
fn fresh(url: &str) -> String {
    let url = url.trim();
    if url.contains('?') {
        return url.to_string();
    }
    format!("{url}?t={}", chrono::Utc::now().timestamp())
}

const ORDER: [Format; 3] = [Format::Hab, Format::Nitro, Format::Swf];

/// What became of the bundles of one download.
#[derive(Default)]
struct BundleCounts {
    counts: Counts,
    /// .swf files kept in source/ because they could not be converted.
    swf_waiting: AtomicUsize,
    by_format: [AtomicUsize; 3],
    /// Answers that were not the format asked for (logged for the first few).
    rejected: AtomicUsize,
}

impl BundleCounts {
    fn summary(&self, what: &str) -> String {
        let mut text = self.counts.summary(what);
        let formats: Vec<String> = ORDER
            .iter()
            .zip(&self.by_format)
            .filter_map(|(format, n)| {
                let n = n.load(Ordering::Relaxed);
                (n > 0).then(|| format!("{n} as .{}", format.extension()))
            })
            .collect();
        if !formats.is_empty() {
            text += &format!(" (served {})", formats.join(", "));
        }
        let waiting = self.swf_waiting.load(Ordering::Relaxed);
        if waiting > 0 {
            text += &format!("; {waiting} .swf could not be converted and are kept in source/");
        }
        text
    }
}

impl Downloader<'_> {
    /// Bundles named `names` from `base`/<name>.<hab|nitro|swf> into gamedata/`folder`/<name>.hab. With a revision
    /// (from a furnidata XML) `base`/<revision>/<name>.<ext> is tried too, as Habbo's hof_furni serves them. The format
    /// and the path that worked last are tried first, so a retro costs one request per bundle.
    async fn fetch_bundles(&self, what: &str, base: &str, folder: &str, names: Vec<(String, Option<i64>)>, kind: AssetKind, only: Option<Format>) -> Result<BundleCounts> {
        let result = BundleCounts::default();
        let preferred = AtomicU8::new(0);
        let plain_first = std::sync::atomic::AtomicBool::new(false);
        let total = names.len();
        let done = AtomicUsize::new(0);
        (self.log)(&format!("  {total} {what} to check"));
        let source = self.root.join("source").join(folder);
        stream::iter(names)
            .map(|(name, revision)| {
                let (result, preferred, plain_first, done, source) = (&result, &preferred, &plain_first, &done, &source);
                async move {
                    if self.cancel.is_cancelled() {
                        return;
                    }
                    let target = self.path(&format!("{folder}/{name}.hab"));
                    let kept = source.join(format!("{name}.swf"));
                    if target.is_file() || kept.is_file() {
                        result.counts.existing.fetch_add(1, Ordering::Relaxed);
                    } else {
                        let first = preferred.load(Ordering::Relaxed) as usize;
                        // A format chosen in the profile is the only one tried; Auto tries all three, the last hit first.
                        let order: Vec<Format> = match only {
                            Some(format) => vec![format],
                            None => vec![ORDER[first], ORDER[(first + 1) % 3], ORDER[(first + 2) % 3]],
                        };
                        let mut stems = vec![name.clone()];
                        if let Some(revision) = revision {
                            let with_revision = format!("{revision}/{name}");
                            if plain_first.load(Ordering::Relaxed) {
                                stems.push(with_revision);
                            } else {
                                stems.insert(0, with_revision);
                            }
                        }
                        let mut found = false;
                        'formats: for &format in &order {
                            let mut hit = None;
                            for stem in &stems {
                                let url = format!("{base}/{stem}.{}", format.extension());
                                if let Ok(Some(data)) = self.bytes(&url).await {
                                    if aio_assets::detect(&data) == Some(format) {
                                        hit = Some((stem.contains('/'), data));
                                        break;
                                    }
                                    // The server answered, but not with this format: say why, for the first few.
                                    if result.rejected.fetch_add(1, Ordering::Relaxed) < 5 {
                                        (self.log)(&format!("  ! {stem}.{}: {}", format.extension(), rejection(format, &data)));
                                    }
                                }
                            }
                            let Some((used_revision, data)) = hit else { continue 'formats };
                            if revision.is_some() {
                                plain_first.store(!used_revision, Ordering::Relaxed);
                            }
                            found = true;
                            let index = ORDER.iter().position(|f| *f == format).unwrap_or(0);
                            preferred.store(index as u8, Ordering::Relaxed);
                            result.by_format[index].fetch_add(1, Ordering::Relaxed);
                            match aio_assets::to_hab(&name, kind, &data) {
                                Ok(hab) => {
                                    if save(&target, &hab).is_ok() {
                                        result.counts.new.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        result.counts.failed.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Err(e) if format == Format::Swf => {
                                    if result.swf_waiting.load(Ordering::Relaxed) < 10 {
                                        (self.log)(&format!("  ! {name}.swf: {e} (kept in source/)"));
                                    }
                                    if save(&kept, &data).is_ok() {
                                        result.swf_waiting.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        result.counts.failed.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Err(e) => {
                                    (self.log)(&format!("  ! {name}.{}: {e}", format.extension()));
                                    result.counts.failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            break;
                        }
                        if !found {
                            // For the first few: the address asked for and what the server answered.
                            if result.counts.missing.fetch_add(1, Ordering::Relaxed) < 3 {
                                let url = format!("{base}/{}.{}", stems[0], order[0].extension());
                                let answer = match self.client.get(&url).send().await {
                                    Ok(response) => format!("{} ({} bytes)", response.status(), response.content_length().map_or("?".into(), |n| n.to_string())),
                                    Err(e) => format!("no answer: {e}"),
                                };
                                (self.log)(&format!("  ! not found: {url} answered {answer}"));
                            }
                        }
                    }
                    let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if total >= 50 && done % (total / 10).max(1) == 0 && done < total {
                        (self.log)(&format!("  {} % ({done} of {total})", done * 100 / total));
                    }
                }
            })
            .buffer_unordered(PARALLEL)
            .collect::<Vec<()>>()
            .await;
        self.stopped()?;
        Ok(result)
    }

    fn save_raw(&self, below_gamedata: &str, text: &str) -> Result<()> {
        save(&self.path(below_gamedata), text.as_bytes()).map_err(|e| error(format!("Saving {below_gamedata} failed: {e}")))?;
        (self.log)(&format!("  config: {below_gamedata} saved"));
        Ok(())
    }

    /// The retro's FurnitureData.json, its furniture (as .hab) and the icons of every colour.
    pub async fn retro_furniture(&self, urls: &RetroUrls) -> Result<String> {
        if urls.furnidata.trim().is_empty() || urls.furniture.trim().is_empty() {
            return Err(error("Fill in FurnitureData.json and the furniture folder of the retro first."));
        }
        let text = self.fresh_text(&urls.furnidata).await?;
        // A Nitro retro serves FurnitureData.json; a Flash one (and Habbo) the furnidata XML, with a revision per furni.
        let (json, entries, xml): (Value, Vec<(String, Option<i64>)>, bool) = if is_xml(&text) {
            let json = crate::furniture_language::xml_to_json(&text)?;
            let entries = convert::furnidata_entries(&text)?.into_iter().map(|(c, r)| (c, (r > 0).then_some(r))).collect();
            (json, entries, true)
        } else {
            let json: Value = serde_json::from_str(&text).map_err(|e| error(format!("{} is neither JSON nor furnidata XML: {e}", urls.furnidata.trim())))?;
            let entries = ["roomitemtypes", "wallitemtypes"]
                .iter()
                .flat_map(|section| json[section]["furnitype"].as_array().cloned().unwrap_or_default())
                .filter_map(|f| f["classname"].as_str().map(|c| (c.to_string(), f["revision"].as_i64().filter(|r| *r > 0))))
                .collect();
            (json, entries, false)
        };
        let entries: Vec<(String, Option<i64>)> = entries.into_iter().filter(|(c, _)| !c.is_empty()).collect();
        if entries.is_empty() {
            return Err(error(format!("{} lists no furniture (roomitemtypes / wallitemtypes).", urls.furnidata.trim())));
        }
        if xml {
            let pretty = serde_json::to_string_pretty(&json).map_err(|e| error(e.to_string()))?;
            self.save_raw("config/FurnitureData.json", &pretty)?;
        } else {
            self.save_raw("config/FurnitureData.json", &text)?;
        }
        let classnames: Vec<String> = entries.iter().map(|(c, _)| c.clone()).collect();
        // One library per furni (its colours share it), at the highest revision.
        let mut libraries: std::collections::BTreeMap<String, (String, Option<i64>)> = std::collections::BTreeMap::new();
        for (classname, revision) in &entries {
            let name = classname.split('*').next().unwrap_or("").to_string();
            if !convert::safe_name(&name) {
                continue;
            }
            let slot = libraries.entry(name.to_lowercase()).or_insert((name, *revision));
            slot.1 = slot.1.max(*revision);
        }
        let names: Vec<(String, Option<i64>)> = libraries.into_values().collect();
        let bundles = self.fetch_bundles("furniture", &folder_url(&urls.furniture), "furniture", names, AssetKind::Furniture, Format::from_extension(&urls.format)).await?;
        let from = if xml { " (from the furnidata XML)" } else { "" };
        let mut summary = format!("FurnitureData.json{from} with {} furniture; {}", classnames.len(), bundles.summary("furniture .hab"));
        if !urls.furniture_icons.trim().is_empty() {
            let base = folder_url(&urls.furniture_icons);
            let mut seen = BTreeSet::new();
            let icons: Vec<(String, Option<i64>)> = entries
                .iter()
                .map(|(c, r)| (c.replacen('*', "_", 1), *r))
                .filter(|(icon, _)| convert::safe_name(icon) && seen.insert(icon.clone()))
                .collect();
            let item = |icon: &str, url: String| Item { url, path: format!("icons/{icon}_icon.png") };
            // With revisions (hof_furni) the icon is at <revision>/<icon>_icon.png; the plain folder is tried for the rest.
            let with_revision: Vec<Item> =
                icons.iter().filter_map(|(icon, r)| r.map(|r| item(icon, format!("{base}/{r}/{icon}_icon.png")))).collect();
            let found = if with_revision.is_empty() { 0 } else { self.fetch_many("furniture icons", with_revision, Kind::Png).await?.get().0 };
            let plain: Vec<Item> = icons.iter().map(|(icon, _)| item(icon, format!("{base}/{icon}_icon.png"))).collect();
            let counts = self.fetch_many("furniture icons", plain, Kind::Png).await?;
            counts.new.fetch_add(found, Ordering::Relaxed);
            counts.existing.fetch_sub(found.min(counts.existing.load(Ordering::Relaxed)), Ordering::Relaxed);
            summary += &format!("; {}", counts.summary("icons"));
        }
        Ok(summary.replace("not on Habbo", "not found on the retro"))
    }

    /// The retro's FigureData.json, FigureMap.json and its clothes (as .hab); hh_human_fx and hh_pets are no clothes.
    pub async fn retro_clothes(&self, urls: &RetroUrls) -> Result<String> {
        if urls.figuremap.trim().is_empty() || urls.clothes.trim().is_empty() {
            return Err(error("Fill in FigureMap.json and the clothes folder of the retro first."));
        }
        let mut map = self.fresh_text(&urls.figuremap).await?;
        // A Flash retro serves figuremap.xml and figuredata.xml: they become the JSON Octane loads.
        let json: Value = if is_xml(&map) {
            let (json, _) = convert::figuremap(&map)?;
            map = serde_json::to_string_pretty(&json).map_err(|e| error(e.to_string()))?;
            json
        } else {
            serde_json::from_str(&map).map_err(|e| error(format!("{} is neither JSON nor figuremap XML: {e}", urls.figuremap.trim())))?
        };
        let mut seen = BTreeSet::new();
        let names: Vec<String> = json["libraries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|l| l["id"].as_str().map(str::to_string))
            .filter(|id| id != "hh_human_fx" && id != "hh_pets" && convert::safe_name(id) && seen.insert(id.to_lowercase()))
            .collect();
        if names.is_empty() {
            return Err(error(format!("{} lists no libraries.", urls.figuremap.trim())));
        }
        self.save_raw("config/FigureMap.json", &map)?;
        let mut summary = String::new();
        if !urls.figuredata.trim().is_empty() {
            let mut data = self.fresh_text(&urls.figuredata).await?;
            if is_xml(&data) {
                data = serde_json::to_string_pretty(&convert::figuredata(&data, false)?).map_err(|e| error(e.to_string()))?;
            } else {
                serde_json::from_str::<Value>(&data).map_err(|e| error(format!("{} is neither JSON nor figuredata XML: {e}", urls.figuredata.trim())))?;
            }
            self.save_raw("config/FigureData.json", &data)?;
            summary += "FigureData.json and ";
        }
        let names = names.into_iter().map(|n| (n, None)).collect();
        let bundles = self.fetch_bundles("clothes", &folder_url(&urls.clothes), "clothes", names, AssetKind::Clothes, Format::from_extension(&urls.format)).await?;
        Ok(format!("{summary}FigureMap.json; {}", bundles.summary("clothes .hab")).replace("not on Habbo", "not found on the retro"))
    }
}

/// A retro profile's own folder below the assets folder: custom/<name>.
pub fn custom_folder(assets_folder: &std::path::Path, profile: &aio_core::settings::RetroProfile) -> PathBuf {
    assets_folder.join(CUSTOM).join(profile.folder_name())
}

/// The kind of asset each gamedata folder holds, for the .swf files kept in source/<folder>/.
pub const SOURCE_FOLDERS: [(&str, AssetKind); 4] =
    [("furniture", AssetKind::Furniture), ("clothes", AssetKind::Clothes), ("pets", AssetKind::Pets), ("effect", AssetKind::Effects)];

/// Converts the .swf files kept in `<custom>/source/<folder>/` into `<custom>/gamedata/<folder>/<name>.hab` (on this
/// computer, no download). A .swf whose .hab is there already is skipped; the .swf files stay. Returns the summary.
pub fn convert_sources(custom: &std::path::Path, log: crate::server::Log<'_>, cancel: &tokio_util::sync::CancellationToken) -> Result<String> {
    let (mut converted, mut skipped, mut failed) = (0, 0, 0);
    for (folder, kind) in SOURCE_FOLDERS {
        let source = custom.join("source").join(folder);
        let Ok(entries) = std::fs::read_dir(&source) else { continue };
        let mut files: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("swf"))).collect();
        files.sort();
        for path in files {
            if cancel.is_cancelled() {
                return Err(error("Stopped."));
            }
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let target = custom.join(super::GAMEDATA).join(folder).join(format!("{name}.hab"));
            if target.is_file() {
                skipped += 1;
                continue;
            }
            let result = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|data| aio_assets::to_hab(&name, kind, &data));
            match result.and_then(|hab| save(&target, &hab).map_err(anyhow::Error::from)) {
                Ok(()) => converted += 1,
                Err(e) => {
                    failed += 1;
                    if failed <= 10 {
                        log(&format!("  ! {folder}/{name}.swf: {e}"));
                    }
                }
            }
        }
    }
    if failed > 10 {
        log(&format!("  ! ... and {} more", failed - 10));
    }
    Ok(format!("{converted} .swf converted to .hab, {skipped} already converted, {failed} failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_with_its_own_version_is_kept() {
        let url = "https://images.leet.city/leet-asset-bundles/gamedata/leet_furni.json?v=94c2f8f6";
        assert_eq!(fresh(url), url);
        assert!(fresh(" https://x/FurnitureData.json ").starts_with("https://x/FurnitureData.json?t="));
        assert_eq!(folder_url("https://x/libraries/figure/%libname%.nitro?v=3"), "https://x/libraries/figure");
        assert_eq!(local_file("https://x/a.json"), None);
        assert_eq!(local_file(r#" "C:\Users\me\leet_furni.json" "#), Some(PathBuf::from(r"C:\Users\me\leet_furni.json")));
        assert_eq!(local_file("file:///home/me/a.json"), Some(PathBuf::from("home/me/a.json")));
    }
}
