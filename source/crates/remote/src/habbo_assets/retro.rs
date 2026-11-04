//! Custom assets from a Nitro retro: the URLs of its renderer-config.json (furnidata.url, furni.asset.url,
//! furni.asset.icon.url, avatar.asset.url, avatar.figuredata.url, avatar.figuremap.url), downloaded into a folder of
//! their own laid out like gamedata, to upload by hand. A retro serves .hab, .nitro or .swf: every bundle ends up as
//! .hab; a .swf that cannot be converted yet stays in source/. (All-in-1 Converter: NitroFurniture.cs, NitroClothes.cs)

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

/// The URL with a timestamp, so a cache in front of the retro gives the newest file (as the converter does).
fn fresh(url: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{}{separator}t={}", url.trim(), chrono::Utc::now().timestamp())
}

const ORDER: [Format; 3] = [Format::Hab, Format::Nitro, Format::Swf];

/// What became of the bundles of one download.
#[derive(Default)]
struct BundleCounts {
    counts: Counts,
    /// .swf files kept in source/ (no SWF converter for them yet).
    swf_waiting: AtomicUsize,
    by_format: [AtomicUsize; 3],
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
            text += &format!("; {waiting} .swf kept in source/ for the SWF converter");
        }
        text
    }
}

impl Downloader<'_> {
    /// Bundles named `names` from `base`/<name>.<hab|nitro|swf> into gamedata/`folder`/<name>.hab. The format that
    /// worked last is tried first, so a retro that serves one format costs one request per bundle.
    async fn fetch_bundles(&self, what: &str, base: &str, folder: &str, names: Vec<String>, kind: AssetKind) -> Result<BundleCounts> {
        let result = BundleCounts::default();
        let preferred = AtomicU8::new(0);
        let total = names.len();
        let done = AtomicUsize::new(0);
        (self.log)(&format!("  {total} {what} to check"));
        let source = self.root.join("source").join(folder);
        stream::iter(names)
            .map(|name| {
                let (result, preferred, done, source) = (&result, &preferred, &done, &source);
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
                        let order = [ORDER[first], ORDER[(first + 1) % 3], ORDER[(first + 2) % 3]];
                        let mut found = false;
                        for format in order {
                            let url = format!("{base}/{name}.{}", format.extension());
                            let Ok(Some(data)) = self.bytes(&url).await else { continue };
                            if aio_assets::detect(&data) != Some(format) {
                                continue;
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
                                Err(_) if format == Format::Swf => {
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
                            result.counts.missing.fetch_add(1, Ordering::Relaxed);
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
        let text = self.text(&fresh(&urls.furnidata)).await?;
        let json: Value = serde_json::from_str(&text).map_err(|e| error(format!("{} is not JSON: {e}", urls.furnidata.trim())))?;
        let classnames: Vec<String> = ["roomitemtypes", "wallitemtypes"]
            .iter()
            .flat_map(|section| json[section]["furnitype"].as_array().cloned().unwrap_or_default())
            .filter_map(|f| f["classname"].as_str().map(str::to_string))
            .filter(|c| !c.is_empty())
            .collect();
        if classnames.is_empty() {
            return Err(error(format!("{} lists no furniture (roomitemtypes / wallitemtypes).", urls.furnidata.trim())));
        }
        self.save_raw("config/FurnitureData.json", &text)?;
        let mut libraries = BTreeSet::new();
        let names: Vec<String> = classnames
            .iter()
            .map(|c| c.split('*').next().unwrap_or("").to_string())
            .filter(|n| convert::safe_name(n) && libraries.insert(n.to_lowercase()))
            .collect();
        let bundles = self.fetch_bundles("furniture", &folder_url(&urls.furniture), "furniture", names, AssetKind::Furniture).await?;
        let mut summary = format!("FurnitureData.json with {} furniture; {}", classnames.len(), bundles.summary("furniture .hab"));
        if !urls.furniture_icons.trim().is_empty() {
            let base = folder_url(&urls.furniture_icons);
            let mut seen = BTreeSet::new();
            let items: Vec<Item> = classnames
                .iter()
                .map(|c| c.replacen('*', "_", 1))
                .filter(|icon| convert::safe_name(icon) && seen.insert(icon.clone()))
                .map(|icon| Item { url: format!("{base}/{icon}_icon.png"), path: format!("icons/{icon}_icon.png") })
                .collect();
            summary += &format!("; {}", self.fetch_many("furniture icons", items, Kind::Png).await?.summary("icons"));
        }
        Ok(summary)
    }

    /// The retro's FigureData.json, FigureMap.json and its clothes (as .hab); hh_human_fx and hh_pets are no clothes.
    pub async fn retro_clothes(&self, urls: &RetroUrls) -> Result<String> {
        if urls.figuremap.trim().is_empty() || urls.clothes.trim().is_empty() {
            return Err(error("Fill in FigureMap.json and the clothes folder of the retro first."));
        }
        let map = self.text(&fresh(&urls.figuremap)).await?;
        let json: Value = serde_json::from_str(&map).map_err(|e| error(format!("{} is not JSON: {e}", urls.figuremap.trim())))?;
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
            let data = self.text(&fresh(&urls.figuredata)).await?;
            serde_json::from_str::<Value>(&data).map_err(|e| error(format!("{} is not JSON: {e}", urls.figuredata.trim())))?;
            self.save_raw("config/FigureData.json", &data)?;
            summary += "FigureData.json and ";
        }
        let bundles = self.fetch_bundles("clothes", &folder_url(&urls.clothes), "clothes", names, AssetKind::Clothes).await?;
        Ok(format!("{summary}FigureMap.json; {}", bundles.summary("clothes .hab")))
    }
}

/// The retro's own folder below the assets folder.
pub fn custom_folder(assets_folder: &std::path::Path) -> PathBuf {
    assets_folder.join(CUSTOM)
}
