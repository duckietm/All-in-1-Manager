//! Downloads the images a rip refers to: page images from the catalogue image URL
//! (`<name>.gif`, else `<name>.png`), page icons from the icon URL (`icon_<n>.png`) and the
//! frontpage promo images (relative to c_images).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::export::RippedCatalog;
use crate::Log;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageStats {
    pub downloaded: usize,
    pub missing: usize,
    pub failed: usize,
    pub missing_names: Vec<String>,
}

impl ImageStats {
    pub fn describe(&self) -> String {
        let mut text = format!(
            "{} downloaded, {} not on the server",
            self.downloaded, self.missing
        );
        if self.failed > 0 {
            text.push_str(&format!(", {} failed", self.failed));
        }
        text
    }
}

/// A file name that is safe on every platform.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

struct Job {
    label: String,
    urls: Vec<String>,
    dest_dir: PathBuf,
    stem: String,
}

fn has_extension(name: &str) -> bool {
    let last = name.rsplit('/').next().unwrap_or(name);
    last.rsplit_once('.').is_some_and(|(_, ext)| {
        (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// The page image names, icon numbers and promo images of a rip.
pub fn collect(catalog: &RippedCatalog) -> (BTreeSet<String>, BTreeSet<i32>, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    let mut icons = BTreeSet::new();
    let mut promos = BTreeSet::new();
    for mode in &catalog.modes {
        for (node, _, _) in mode.index.walk() {
            if node.icon > 0 {
                icons.insert(node.icon);
            }
        }
        for page in mode.pages.values() {
            for image in &page.images {
                // frontpage_featured joins several with ';'.
                for name in image.split(';').map(str::trim).filter(|n| !n.is_empty()) {
                    names.insert(name.to_string());
                }
            }
            for item in &page.frontpage_items {
                if !item.promo_image.trim().is_empty() {
                    promos.insert(item.promo_image.trim().to_string());
                }
            }
        }
    }
    (names, icons, promos)
}

/// Habbo's current image host. Old settings point the catalogue URL at habboo-a.akamaihd.net,
/// which no longer serves anything, so every image is also tried here (retros mostly keep
/// Habbo's image names, so this also fills the gaps of a retro's own image host).
const CURRENT_CATALOGUE: &str = "https://images.habbo.com/c_images/catalogue";

fn jobs(catalog: &RippedCatalog, out_dir: &Path, catalog_url: &str, icon_url: &str) -> Vec<Job> {
    let (names, icons, promos) = collect(catalog);
    let base = catalog_url.trim().trim_end_matches('/');
    let c_images = base.strip_suffix("/catalogue").unwrap_or(base);
    let images = out_dir.join("images");
    let mut jobs = Vec::new();
    let mut bases = vec![base.to_string()];
    if !base.eq_ignore_ascii_case(CURRENT_CATALOGUE) {
        bases.push(CURRENT_CATALOGUE.to_string());
    }
    for name in names {
        let urls = bases
            .iter()
            .flat_map(|base| {
                if has_extension(&name) {
                    vec![format!("{base}/{name}")]
                } else {
                    vec![format!("{base}/{name}.gif"), format!("{base}/{name}.png")]
                }
            })
            .collect();
        let stem = safe_file_name(&name);
        jobs.push(Job {
            label: name,
            urls,
            dest_dir: images.clone(),
            stem,
        });
    }
    for icon in icons {
        jobs.push(Job {
            label: format!("icon_{icon}"),
            urls: vec![format!("{}{icon}.png", icon_url.trim())],
            dest_dir: images.join("icons"),
            stem: format!("icon_{icon}"),
        });
    }
    for promo in promos {
        let urls = if promo.starts_with("http://") || promo.starts_with("https://") {
            vec![promo.clone()]
        } else {
            let current = CURRENT_CATALOGUE.strip_suffix("/catalogue").unwrap_or(CURRENT_CATALOGUE);
            let mut urls = vec![format!("{c_images}/{}", promo.trim_start_matches('/'))];
            if !c_images.eq_ignore_ascii_case(current) {
                urls.push(format!("{current}/{}", promo.trim_start_matches('/')));
            }
            urls
        };
        let file = promo.rsplit('/').next().unwrap_or(&promo);
        jobs.push(Job {
            label: promo.clone(),
            urls,
            dest_dir: images.join("frontpage"),
            stem: safe_file_name(file),
        });
    }
    jobs
}

fn extension_of(url: &str) -> Option<&str> {
    let last = url.rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    Some(ext)
}

/// Downloads every image (8 at a time). Files already there are kept. Stops early (with what
/// it has) when `cancel` fires.
pub async fn download(
    catalog: &RippedCatalog,
    out_dir: &Path,
    catalog_url: &str,
    icon_url: &str,
    log: Log<'_>,
    cancel: &CancellationToken,
) -> ImageStats {
    let jobs = jobs(catalog, out_dir, catalog_url, icon_url);
    let results = futures::stream::iter(jobs)
        .map(|job| async move {
            let _ = std::fs::create_dir_all(&job.dest_dir);
            for url in &job.urls {
                let dest = if has_extension(&job.stem) {
                    job.dest_dir.join(&job.stem)
                } else {
                    job.dest_dir.join(format!(
                        "{}.{}",
                        job.stem,
                        extension_of(url).unwrap_or("png")
                    ))
                };
                if dest.is_file() {
                    return (job.label, Ok(true));
                }
                match crate::http::try_get_bytes(url).await {
                    Ok(Some(bytes)) => {
                        return match std::fs::write(&dest, bytes) {
                            Ok(()) => (job.label, Ok(true)),
                            Err(e) => (job.label, Err(format!("{}: {e}", dest.display()))),
                        };
                    }
                    Ok(None) => continue,
                    // A dead host: try the next URL before giving up on the image.
                    Err(e) if url != job.urls.last().unwrap() => {
                        let _ = e;
                        continue;
                    }
                    Err(e) => return (job.label, Err(format!("{e:#}"))),
                }
            }
            (job.label, Ok(false))
        })
        .buffer_unordered(8)
        .take_until(cancel.cancelled())
        .collect::<Vec<_>>()
        .await;
    let mut stats = ImageStats::default();
    for (label, result) in results {
        match result {
            Ok(true) => stats.downloaded += 1,
            Ok(false) => {
                stats.missing += 1;
                stats.missing_names.push(label);
            }
            Err(e) => {
                stats.failed += 1;
                log(&format!("Image {label}: {e}"));
            }
        }
    }
    stats.missing_names.sort();
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            safe_file_name("catalog_header/../x?.gif"),
            "catalog_header_.._x_.gif"
        );
        assert!(has_extension("catalogue/feature_cata_vert.png"));
        assert!(!has_extension("catalog_frontpage_headline_shop_EN"));
        assert!(!has_extension("ctlg_pic_v1.0_hc"));
        assert_eq!(
            extension_of("http://x/c_images/catalogue/a.gif"),
            Some("gif")
        );
    }
}
