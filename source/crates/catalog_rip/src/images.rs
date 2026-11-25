//! Downloads the images a rip refers to: page images from the catalogue image URL
//! (`<name>.gif`, else `<name>.png`), page icons from the icon URL (`icon_<n>.png`) and the
//! frontpage promo images (relative to c_images), plus the `th_*` thumbnails the client asks
//! for wallpaper, floor and landscape products. Images that are already in the owner's own
//! c_images/catalogue folder are not downloaded again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::export::RippedCatalog;
use crate::furnidata::Furnidata;
use crate::Log;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageStats {
    pub downloaded: usize,
    /// Found in the owner's own catalogue images folder: copied, not downloaded.
    pub already_have: usize,
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
        if self.already_have > 0 {
            text.push_str(&format!(", {} already in your folder (copied)", self.already_have));
        }
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
    /// Where the owner's own folder would have this image (copied into `dest_dir` when found).
    have: Vec<PathBuf>,
}

/// Where the images come from and what is already there.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    /// The source hotel's furnidata: class names tell which wall products are wallpaper, floor
    /// or landscape (their thumbnails). None: no thumbnails.
    pub furnidata: Option<&'a Furnidata>,
    /// `catalog.asset.url`: the folder of page images and `th_*` thumbnails.
    pub catalog_url: &'a str,
    pub icon_url: &'a str,
    /// The owner's own c_images/catalogue folder: images found there are copied, not downloaded.
    pub have_dir: Option<&'a Path>,
}

/// The thumbnail names (without extension) the Nitro catalogue asks `catalog.asset.url` for
/// (`Product.getIconUrl`): wall products whose furnidata class is `floor` (`th_floor_<extra>`),
/// `wallpaper` (`th_wall_<extra>`) or `landscape` (`th_landscape_<extra, first . as _>_001`);
/// products without extra data have none.
pub fn thumbnail_names(catalog: &RippedCatalog, furnidata: &Furnidata) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for mode in &catalog.modes {
        for page in mode.pages.values() {
            for product in page.offers.iter().flat_map(|o| &o.products) {
                if product.product_type != "i" {
                    continue;
                }
                let extra = product.extra_param.trim();
                if extra.is_empty() {
                    continue;
                }
                match furnidata.classname(product) {
                    Some("floor") => {
                        names.insert(format!("th_floor_{extra}"));
                    }
                    Some("wallpaper") => {
                        names.insert(format!("th_wall_{extra}"));
                    }
                    Some("landscape") => {
                        names.insert(format!("th_landscape_{}_001", extra.replacen('.', "_", 1)));
                    }
                    _ => {}
                }
            }
        }
    }
    names
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

fn jobs(catalog: &RippedCatalog, out_dir: &Path, sources: &Sources) -> Vec<Job> {
    let (catalog_url, icon_url) = (sources.catalog_url, sources.icon_url);
    let (names, icons, promos) = collect(catalog);
    // Looked up by their safe file names only: a name from the hotel never leaves the folder.
    let have_in = |names: &[String]| -> Vec<PathBuf> {
        sources
            .have_dir
            .map(|dir| names.iter().map(|n| dir.join(safe_file_name(n))).collect())
            .unwrap_or_default()
    };
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
        let have = if has_extension(&name) {
            have_in(std::slice::from_ref(&name))
        } else {
            have_in(&[format!("{name}.gif"), format!("{name}.png")])
        };
        jobs.push(Job {
            label: name,
            urls,
            dest_dir: images.clone(),
            stem,
            have,
        });
    }
    if let Some(furnidata) = sources.furnidata {
        for name in thumbnail_names(catalog, furnidata) {
            let file = format!("{name}.png");
            jobs.push(Job {
                label: name,
                urls: bases.iter().map(|base| format!("{base}/{file}")).collect(),
                dest_dir: images.clone(),
                stem: safe_file_name(&file),
                have: have_in(&[file]),
            });
        }
    }
    for icon in icons {
        jobs.push(Job {
            label: format!("icon_{icon}"),
            urls: vec![format!("{}{icon}.png", icon_url.trim())],
            dest_dir: images.join("icons"),
            stem: format!("icon_{icon}"),
            have: {
                let file = format!("icon_{icon}.png");
                let mut have = have_in(std::slice::from_ref(&file));
                have.extend(sources.have_dir.map(|dir| dir.join("icons").join(&file)));
                have
            },
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
        // Only by its path under c_images (a file name alone could be another image), and only
        // when every part of the path is a plain file name.
        let mut have = Vec::new();
        let relative = promo.trim_start_matches('/');
        let plain = !promo.starts_with("http://")
            && !promo.starts_with("https://")
            && relative.split('/').all(|part| !part.is_empty() && safe_file_name(part) == part);
        if let (Some(parent), true) = (sources.have_dir.and_then(Path::parent), plain) {
            have.push(relative.split('/').fold(parent.to_path_buf(), |path, part| path.join(part)));
        }
        jobs.push(Job {
            label: promo.clone(),
            urls,
            dest_dir: images.join("frontpage"),
            stem: safe_file_name(file),
            have,
        });
    }
    jobs
}

fn extension_of(url: &str) -> Option<&str> {
    let last = url.rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    Some(ext)
}

enum Got {
    Downloaded,
    Have,
    Missing,
}

/// Downloads every image (8 at a time). Files already there are kept; images the owner's own
/// folder has are copied from it instead of downloaded, so the rip folder stays complete. Stops early (with what it has) when `cancel` fires.
pub async fn download(
    catalog: &RippedCatalog,
    out_dir: &Path,
    sources: &Sources<'_>,
    log: Log<'_>,
    cancel: &CancellationToken,
) -> ImageStats {
    let jobs = jobs(catalog, out_dir, sources);
    let results = futures::stream::iter(jobs)
        .map(|job| async move {
            let _ = std::fs::create_dir_all(&job.dest_dir);
            if let Some(found) = job.have.iter().find(|path| path.is_file()) {
                let dest = if has_extension(&job.stem) {
                    job.dest_dir.join(&job.stem)
                } else {
                    let ext = found.extension().and_then(|e| e.to_str()).unwrap_or("png");
                    job.dest_dir.join(format!("{}.{ext}", job.stem))
                };
                if dest.is_file() {
                    return (job.label, Ok(Got::Have));
                }
                return match std::fs::copy(found, &dest) {
                    Ok(_) => (job.label, Ok(Got::Have)),
                    Err(e) => (job.label, Err(format!("{} -> {}: {e}", found.display(), dest.display()))),
                };
            }
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
                    return (job.label, Ok(Got::Downloaded));
                }
                match crate::http::try_get_bytes(url).await {
                    Ok(Some(bytes)) => {
                        return match std::fs::write(&dest, bytes) {
                            Ok(()) => (job.label, Ok(Got::Downloaded)),
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
            (job.label, Ok(Got::Missing))
        })
        .buffer_unordered(8)
        .take_until(cancel.cancelled())
        .collect::<Vec<_>>()
        .await;
    let mut stats = ImageStats::default();
    for (label, result) in results {
        match result {
            Ok(Got::Downloaded) => stats.downloaded += 1,
            Ok(Got::Have) => stats.already_have += 1,
            Ok(Got::Missing) => {
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

    use crate::export::RippedMode;
    use crate::packets::{CatalogIndex, CatalogPage, IndexNode, Offer, Product};
    use std::collections::HashMap;

    fn product(kind: &str, class_id: i32, extra: &str) -> Product {
        Product {
            product_type: kind.into(),
            class_id: Some(class_id),
            extra_param: extra.into(),
            count: 1,
            limited: false,
            limited_series_size: 0,
            limited_items_left: 0,
        }
    }

    fn catalog(products: Vec<Product>, images: Vec<String>, icon: i32) -> RippedCatalog {
        let offers = products
            .into_iter()
            .enumerate()
            .map(|(i, p)| Offer {
                offer_id: i as i32 + 1,
                localization_id: format!("o{i}"),
                rent: false,
                price_credits: 1,
                price_activity_points: 0,
                activity_point_type: 0,
                extra_int: None,
                giftable: true,
                products: vec![p],
                club_level: 0,
                bundle_purchase_allowed: true,
                is_pet: false,
                preview_image: String::new(),
                item_ids: None,
                have_offer: None,
            })
            .collect();
        let page = CatalogPage {
            page_id: 1,
            mode: "NORMAL".into(),
            layout: "default_3x3".into(),
            images,
            texts: vec![],
            offers,
            offer_id: -1,
            accept_season_currency_as_credits: None,
            frontpage_items: vec![],
            packet_layout: "classic".into(),
        };
        let node = |id, icon, children| IndexNode {
            visible: true,
            icon,
            page_id: id,
            parent_id: -1,
            name: String::new(),
            localization: String::new(),
            offer_ids: vec![],
            children,
        };
        RippedCatalog {
            hotel: "h".into(),
            host: "h".into(),
            hotel_version: String::new(),
            client_type: String::new(),
            ripped_at: String::new(),
            complete: true,
            modes: vec![RippedMode {
                mode: "NORMAL".into(),
                index: CatalogIndex {
                    root: node(-1, 0, vec![node(1, icon, vec![])]),
                    new_additions_available: false,
                    mode: "NORMAL".into(),
                    has_parent_ids: false,
                    tail: String::new(),
                },
                pages: HashMap::from([(1, page)]),
                failed: vec![],
            }],
        }
    }

    fn furnidata() -> Furnidata {
        Furnidata::parse(
            r#"{"roomitemtypes":{"furnitype":[{"id":1,"classname":"chair"}]},
                "wallitemtypes":{"furnitype":[
                  {"id":10,"classname":"floor"},{"id":11,"classname":"wallpaper"},
                  {"id":12,"classname":"landscape"},{"id":13,"classname":"poster"}]}}"#,
        )
        .unwrap()
    }

    fn thumbs_catalog() -> RippedCatalog {
        catalog(
            vec![
                product("i", 10, "101"),
                product("i", 11, "202"),
                product("i", 12, "1.1"),
                product("i", 12, "2.0.1"), // only the first dot, as the client does
                product("i", 10, ""),      // no extra data: no th_floor_.png
                product("i", 13, "40"), // a poster: no thumbnail
                product("s", 1, "x"),   // floor furni: its icon comes from the .nitro
            ],
            vec!["catalog_header".into()],
            5,
        )
    }

    #[test]
    fn thumbnails_are_named_like_the_client_asks() {
        // Product.getIconUrl: th_floor_<extra>, th_wall_<extra>,
        // th_landscape_<extra, first . -> _>_001; nothing for an empty extra.
        let names = thumbnail_names(&thumbs_catalog(), &furnidata());
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["th_floor_101", "th_landscape_1_1_001", "th_landscape_2_0.1_001", "th_wall_202"]
        );
    }

    fn sources<'a>(furnidata: Option<&'a Furnidata>, have: Option<&'a Path>, url: &'a str) -> Sources<'a> {
        Sources { furnidata, catalog_url: url, icon_url: "http://127.0.0.1:9/icon_", have_dir: have }
    }

    #[test]
    fn page_image_fallback_order_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let furni = furnidata();
        let jobs = jobs(
            &thumbs_catalog(),
            dir.path(),
            &sources(Some(&furni), None, "http://old.example/c_images/catalogue"),
        );
        let header = jobs.iter().find(|j| j.label == "catalog_header").unwrap();
        assert_eq!(
            header.urls,
            [
                "http://old.example/c_images/catalogue/catalog_header.gif",
                "http://old.example/c_images/catalogue/catalog_header.png",
                "https://images.habbo.com/c_images/catalogue/catalog_header.gif",
                "https://images.habbo.com/c_images/catalogue/catalog_header.png",
            ]
        );
        let thumb = jobs.iter().find(|j| j.label == "th_wall_202").unwrap();
        assert_eq!(thumb.urls[0], "http://old.example/c_images/catalogue/th_wall_202.png");
        assert_eq!(thumb.stem, "th_wall_202.png");
        assert!(jobs.iter().any(|j| j.label == "icon_5"));
        // No furnidata: no thumbnails.
        let none = super::jobs(&thumbs_catalog(), dir.path(), &sources(None, None, "http://x/c"));
        assert!(none.iter().all(|j| !j.label.starts_with("th_")));
    }

    #[tokio::test]
    async fn images_the_owner_already_has_are_copied_not_downloaded() {
        let out = tempfile::tempdir().unwrap();
        let have = tempfile::tempdir().unwrap();
        let furni = furnidata();
        std::fs::create_dir(have.path().join("icons")).unwrap();
        for name in [
            "catalog_header.gif", // a page image, as .gif
            "icons/icon_5.png",   // icons may sit in their own folder
            "th_floor_101.png",
            "th_wall_202.png",
            "th_landscape_1_1_001.png",
            "th_landscape_2_0.1_001.png",
        ] {
            std::fs::write(have.path().join(name), name).unwrap();
        }
        let log = |_: &str| {};
        // Everything is in the owner's folder: no request is made (the hosts would not be
        // reached); every image is copied into the rip folder.
        let stats = download(
            &thumbs_catalog(),
            out.path(),
            &sources(Some(&furni), Some(have.path()), "http://127.0.0.1:9/c_images/catalogue"),
            &log,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(stats.already_have, 6, "{stats:?}");
        assert_eq!((stats.downloaded, stats.missing, stats.failed), (0, 0, 0));
        let images = out.path().join("images");
        assert_eq!(std::fs::read_to_string(images.join("catalog_header.gif")).unwrap(), "catalog_header.gif");
        assert_eq!(std::fs::read_to_string(images.join("icons/icon_5.png")).unwrap(), "icons/icon_5.png");
        assert!(images.join("th_floor_101.png").is_file());
        assert!(images.join("th_landscape_2_0.1_001.png").is_file());
        assert!(stats.describe().contains("6 already in your folder (copied)"));
    }

    #[test]
    fn owner_folder_lookups_stay_inside_it() {
        let have = tempfile::tempdir().unwrap();
        let catalogue = have.path().join("catalogue");
        let mut catalog = catalog(vec![], vec!["../secret".into()], 0);
        let page = catalog.modes[0].pages.get_mut(&1).unwrap();
        for promo in ["catalogue/feature.png", "../outside.png", "other.png"] {
            page.frontpage_items.push(crate::packets::FrontPageItem {
                position: 0,
                name: String::new(),
                promo_image: promo.into(),
                item_type: 0,
                page_name: None,
                product_offer_id: None,
                product_code: None,
                expiration_seconds: 0,
            });
        }
        let jobs = jobs(&catalog, have.path(), &sources(None, Some(&catalogue), "http://x/c_images/catalogue"));
        let have_of = |label: &str| jobs.iter().find(|j| j.label == label).unwrap().have.clone();
        assert_eq!(have_of("../secret"), [catalogue.join("_secret.gif"), catalogue.join("_secret.png")]);
        // A promo by its c_images path only (no basename match); a path leaving the folder: none.
        assert_eq!(have_of("catalogue/feature.png"), [have.path().join("catalogue").join("feature.png")]);
        assert!(have_of("../outside.png").is_empty());
        assert_eq!(have_of("other.png"), [have.path().join("other.png")]);
    }
}
