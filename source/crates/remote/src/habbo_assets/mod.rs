//! Get Assets: Habbo's own assets downloaded on this computer, straight into a folder laid out like the server's
//! /var/www/gamedata, so Deploy Assets can copy the folder as it is. Only .hab bundles, JSON and images are kept: what
//! Habbo still serves as XML or text (furnidata, figuredata, figuremap, effectmap, avatar actions, productdata, texts)
//! is converted to the JSON Octane loads, and no .swf or XML is saved. (All-in-1 Converter: Menu/HabboOriginalMenu.cs
//! and Download Classes/*.cs)
//!
//! The folder: `<root>/gamedata/` mirrors /var/www/gamedata (furniture/, icons/, clothes/, effect/, pets/, sounds/,
//! config/*.json, c_images/{album1584,catalogue,fx_icons,Quests,reception,...}); `<root>/downloads/` keeps what is
//! only for reading here (external_variables as JSON) and is never copied to the server.

pub mod convert;
pub use aio_assets::hab;
pub mod retro;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::furniture_language::{FurniLanguage, LANGUAGES, USER_AGENT};
use crate::server::{Log, Result, ServerError};

/// The folder below the assets folder that mirrors /var/www/gamedata.
pub const GAMEDATA: &str = "gamedata";
/// The folder below the assets folder for files that stay on this computer.
pub const DOWNLOADS: &str = "downloads";

/// Downloads at the same time from Habbo's CDN.
const PARALLEL: usize = 16;
/// Numbered files (catalogue icons, sound samples) end after this many missing numbers in a row.
const MISSES_IN_A_ROW: usize = 20;

/// The pet libraries (All-in-1 Converter: Pets.cs DefaultPets).
pub const PETS: [&str; 36] = [
    "bear", "bearbaby", "bunnydepressed", "bunnyeaster", "bunnyevil", "bunnylove", "cat", "chicken", "cow", "croco", "demonmonkey", "dog",
    "dragon", "dragondog", "fools", "frog", "gnome", "haloompa", "horse", "kittenbaby", "lion", "monkey", "monster", "monsterplant", "pig",
    "pigeonevil", "pigeongood", "pigletbaby", "pterosaur", "puppybaby", "rhino", "spider", "terrier", "terrierbaby", "turtle", "velociraptor",
];

/// One download of the Get Assets page, in the converter's menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Asset {
    Badges,
    Clothes,
    Effects,
    Furnidata,
    Furniture,
    CatalogueIcons,
    Mp3,
    ProductData,
    Quests,
    Reception,
    Texts,
    Variables,
    Pets,
    EffectIcons,
}

impl Asset {
    pub const ALL: [Asset; 14] = [
        Asset::Badges,
        Asset::Clothes,
        Asset::Effects,
        Asset::Furnidata,
        Asset::Furniture,
        Asset::CatalogueIcons,
        Asset::Mp3,
        Asset::ProductData,
        Asset::Quests,
        Asset::Reception,
        Asset::Texts,
        Asset::Variables,
        Asset::Pets,
        Asset::EffectIcons,
    ];

    /// What Download all runs: everything but the effect icons, which need the Habbo Classic app (and are added when
    /// a source for them is set).
    pub fn in_download_all(self) -> bool {
        self != Asset::EffectIcons
    }

    /// Where it ends up, below gamedata/ (or downloads/ for the variables).
    pub fn target(self) -> &'static str {
        match self {
            Asset::Badges => "c_images/album1584",
            Asset::Clothes => "clothes + config/FigureData.json, FigureMap.json",
            Asset::Effects => "effect + config/EffectMap.json, HabboAvatarActions.json",
            Asset::Furnidata => "config/FurnitureData.json",
            Asset::Furniture => "furniture + icons",
            Asset::CatalogueIcons => "c_images/catalogue",
            Asset::Mp3 => "sounds",
            Asset::ProductData => "config/ProductData.json",
            Asset::Quests => "c_images/Quests",
            Asset::Reception => "c_images/reception, web_promo_small, ...",
            Asset::Texts => "config/ExternalTexts.json",
            Asset::Variables => "downloads/ExternalVariables.json (stays on this computer)",
            Asset::Pets => "pets",
            Asset::EffectIcons => "c_images/fx_icons",
        }
    }
}

/// Where everything comes from. [`Urls::habbo`] for a real hotel; tests point it at a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Urls {
    pub variables: String,
    pub texts: String,
    pub productdata: String,
    pub furnidata: String,
    pub figuredata: String,
    /// The external_flash_texts of every hotel: badges are collected from all of them.
    pub badge_texts: Vec<String>,
    /// The variables with the hotel view images (habbo.com's, as in the converter).
    pub reception_variables: String,
    /// images.habbo.com: dcr/hof_furni, gordon and c_images below it.
    pub images: String,
}

impl Urls {
    pub fn habbo(hotel: FurniLanguage) -> Self {
        let d = hotel.domain;
        Self {
            variables: format!("https://www.{d}/gamedata/external_variables/1"),
            texts: format!("https://{d}/gamedata/external_flash_texts/1"),
            productdata: format!("https://{d}/gamedata/productdata/1"),
            furnidata: format!("https://www.{d}/gamedata/furnidata_xml/1"),
            figuredata: format!("https://www.{d}/gamedata/figuredata/1"),
            badge_texts: LANGUAGES.iter().map(|h| format!("https://www.{}/gamedata/external_flash_texts/1", h.domain)).collect(),
            reception_variables: "https://www.habbo.com/gamedata/external_variables/1".into(),
            images: "https://images.habbo.com".into(),
        }
    }

    fn furniture(&self) -> String {
        format!("{}/dcr/hof_furni", self.images)
    }

    fn gordon(&self, release: &str) -> String {
        format!("{}/gordon/{release}", self.images)
    }

    fn c_images(&self) -> String {
        format!("{}/c_images", self.images)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Clothes: every set non-sellable, so every item can be worn (the converter's question).
    pub not_sellable: bool,
    /// Effect icons: HabboClassicWin.zip, its app.asar, habbo-inventory-com.hab, a folder with any of them or with
    /// fx_icon_<type>.png files, or a link to one of them. None: `<root>/import/effect_icons`.
    pub effect_icons_source: Option<String>,
    /// Furniture, clothes, effects and pets: also the .swf of every library, into the asset tools'
    /// Assets_tools/Original_SWF_Files/<type> (never into gamedata, so never to the server).
    pub also_swf: bool,
}

/// The asset tools' folder below the assets folder.
pub const ASSETS_TOOLS: &str = "Assets_tools";
/// An Item path starting with this goes below Assets_tools/Original_SWF_Files instead of gamedata.
const SWF_PREFIX: &str = "swf:";

/// How many files of one kind were saved, were there already, or Habbo does not have.
#[derive(Debug, Default)]
struct Counts {
    new: AtomicUsize,
    existing: AtomicUsize,
    missing: AtomicUsize,
    failed: AtomicUsize,
}

impl Counts {
    fn get(&self) -> (usize, usize, usize, usize) {
        (self.new.load(Ordering::Relaxed), self.existing.load(Ordering::Relaxed), self.missing.load(Ordering::Relaxed), self.failed.load(Ordering::Relaxed))
    }

    fn summary(&self, what: &str) -> String {
        let (new, existing, missing, failed) = self.get();
        let mut text = format!("{new} new {what}, {existing} already there");
        if missing > 0 {
            text += &format!(", {missing} not on Habbo");
        }
        if failed > 0 {
            text += &format!(", {failed} could not be saved");
        }
        text
    }
}

/// One file to fetch: the URL and the path below gamedata/ (or, with SWF_PREFIX, below Original_SWF_Files).
#[derive(Clone)]
struct Item {
    url: String,
    path: String,
}

pub fn gamedata_folder(root: &Path) -> PathBuf {
    root.join(GAMEDATA)
}

/// The kinds of file a download must be, so an error page of the CDN is never saved as an asset.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Hab,
    Png,
    Image,
    Mp3,
    Swf,
}

impl Kind {
    fn accepts(self, data: &[u8]) -> bool {
        match self {
            Kind::Hab => hab::is_hab(data),
            Kind::Png => hab::is_png(data),
            Kind::Image => hab::is_png(data) || data.starts_with(b"GIF8") || data.starts_with(&[0xFF, 0xD8, 0xFF]),
            Kind::Mp3 => data.starts_with(b"ID3") || (data.len() > 2 && data[0] == 0xFF && data[1] & 0xE0 == 0xE0),
            Kind::Swf => aio_assets::bundle::is_swf(data),
        }
    }
}

pub struct Downloader<'a> {
    root: PathBuf,
    urls: Urls,
    options: Options,
    client: reqwest::Client,
    log: Log<'a>,
    cancel: CancellationToken,
    release: tokio::sync::OnceCell<String>,
}

fn error(message: impl Into<String>) -> anyhow::Error {
    ServerError(message.into()).into()
}

/// Writes next to the target first, so a stopped run never leaves half a file that counts as downloaded.
fn save(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    std::fs::write(&part, data)?;
    std::fs::rename(&part, path)
}

impl<'a> Downloader<'a> {
    pub fn new(root: &Path, urls: Urls, options: Options, log: Log<'a>, cancel: CancellationToken) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| error(e.to_string()))?;
        Ok(Self { root: root.to_path_buf(), urls, options, client, log, cancel, release: tokio::sync::OnceCell::new() })
    }

    fn path(&self, below_gamedata: &str) -> PathBuf {
        let (mut path, below_gamedata) = match below_gamedata.strip_prefix(SWF_PREFIX) {
            Some(rest) => (self.root.join(ASSETS_TOOLS).join(aio_assets::workshop::ORIGINAL_SWF), rest),
            None => (gamedata_folder(&self.root), below_gamedata),
        };
        for part in below_gamedata.split('/') {
            path.push(part);
        }
        path
    }

    fn stopped(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(error("Stopped."));
        }
        Ok(())
    }

    async fn text(&self, url: &str) -> Result<String> {
        self.stopped()?;
        let response = self.client.get(url).send().await.map_err(|e| error(format!("Could not download {url}: {e}")))?;
        if !response.status().is_success() {
            return Err(error(format!("{url} answered {}", response.status())));
        }
        response.text().await.map_err(|e| error(format!("Could not download {url}: {e}")))
    }

    /// The body of a successful answer; None for a missing file. A network error is retried once.
    async fn bytes(&self, url: &str) -> std::result::Result<Option<Vec<u8>>, String> {
        let mut last = String::new();
        for _ in 0..2 {
            match self.client.get(url).send().await {
                Ok(response) if response.status().is_success() => match response.bytes().await {
                    Ok(body) => return Ok(Some(body.to_vec())),
                    Err(e) => last = e.to_string(),
                },
                Ok(response) if response.status().is_client_error() => return Ok(None),
                Ok(response) => last = response.status().to_string(),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    fn save_json(&self, below_gamedata: &str, value: &Value) -> Result<()> {
        let text = serde_json::to_string_pretty(value).map_err(|e| error(e.to_string()))?;
        save(&self.path(below_gamedata), text.as_bytes()).map_err(|e| error(format!("Saving {below_gamedata} failed: {e}")))?;
        (self.log)(&format!("  config: {below_gamedata} saved"));
        Ok(())
    }

    /// The release folder of Habbo's gordon assets, read once from the hotel's external_variables.
    async fn release(&self) -> Result<String> {
        self.release
            .get_or_try_init(|| async {
                let variables = self.text(&self.urls.variables).await?;
                let release = convert::release(&variables).ok_or_else(|| error("external_variables has no flash.client.url: the release is unknown."))?;
                (self.log)(&format!("  Habbo release {release}"));
                Ok::<String, anyhow::Error>(release)
            })
            .await
            .cloned()
    }

    /// Fetches the files that are not there yet, PARALLEL at a time, and logs the progress every 10 %.
    async fn fetch_many(&self, what: &str, items: Vec<Item>, kind: Kind) -> Result<Counts> {
        let counts = Counts::default();
        let total = items.len();
        let done = AtomicUsize::new(0);
        let reported = AtomicUsize::new(0);
        (self.log)(&format!("  {total} {what} to check"));
        let errors = std::sync::Mutex::new(Vec::<String>::new());
        stream::iter(items)
            .map(|item| {
                let (counts, done, reported, errors) = (&counts, &done, &reported, &errors);
                async move {
                    if self.cancel.is_cancelled() {
                        return;
                    }
                    let path = self.path(&item.path);
                    if path.is_file() {
                        counts.existing.fetch_add(1, Ordering::Relaxed);
                    } else {
                        match self.bytes(&item.url).await {
                            Ok(Some(data)) if kind.accepts(&data) => match save(&path, &data) {
                                Ok(()) => {
                                    counts.new.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(e) => {
                                    counts.failed.fetch_add(1, Ordering::Relaxed);
                                    errors.lock().unwrap().push(format!("{}: {e}", item.path));
                                }
                            },
                            Ok(_) => {
                                counts.missing.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                counts.failed.fetch_add(1, Ordering::Relaxed);
                                errors.lock().unwrap().push(format!("{}: {e}", item.url));
                            }
                        }
                    }
                    let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    let step = done * 10 / total.max(1);
                    if total >= 20 && step > reported.load(Ordering::Relaxed) && step < 10 {
                        reported.store(step, Ordering::Relaxed);
                        let (new, ..) = counts.get();
                        (self.log)(&format!("  {} % ({done} of {total}, {new} new)", step * 10));
                    }
                }
            })
            .buffer_unordered(PARALLEL)
            .collect::<Vec<()>>()
            .await;
        let errors = errors.into_inner().unwrap();
        for e in errors.iter().take(5) {
            (self.log)(&format!("  ! {e}"));
        }
        if errors.len() > 5 {
            (self.log)(&format!("  ! ... and {} more", errors.len() - 5));
        }
        self.stopped()?;
        Ok(counts)
    }

    /// Numbered files (icon_1.png, icon_2.png, ...) until MISSES_IN_A_ROW numbers in a row are missing; a file that is
    /// already there counts as found. Checked PARALLEL numbers at a time.
    async fn fetch_numbered(&self, what: &str, url: impl Fn(usize) -> String, path: impl Fn(usize) -> String, kind: Kind) -> Result<Counts> {
        let counts = Counts::default();
        let mut misses = 0;
        let mut next = 1;
        while misses < MISSES_IN_A_ROW {
            self.stopped()?;
            let batch: Vec<usize> = (next..next + PARALLEL).collect();
            next += PARALLEL;
            let found: Vec<(usize, bool)> = stream::iter(batch)
                .map(|n| {
                    let counts = &counts;
                    let (url, path) = (url(n), self.path(&path(n)));
                    async move {
                        if path.is_file() {
                            counts.existing.fetch_add(1, Ordering::Relaxed);
                            return (n, true);
                        }
                        match self.bytes(&url).await {
                            Ok(Some(data)) if kind.accepts(&data) => {
                                if save(&path, &data).is_ok() {
                                    counts.new.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    counts.failed.fetch_add(1, Ordering::Relaxed);
                                }
                                (n, true)
                            }
                            _ => (n, false),
                        }
                    }
                })
                .buffered(PARALLEL)
                .collect()
                .await;
            for (_, hit) in found {
                misses = if hit { 0 } else { misses + 1 };
                if misses >= MISSES_IN_A_ROW {
                    break;
                }
            }
            if next % 400 == 1 {
                let (new, existing, ..) = counts.get();
                (self.log)(&format!("  {} {what} so far ({new} new)", new + existing));
            }
        }
        Ok(counts)
    }

    /// Libraries (.hab into gamedata) and, with `also_swf`, their .swf into Original_SWF_Files/`swf_type`.
    async fn libraries(&self, what: &str, items: Vec<Item>, hab_what: &str, swf_type: &str) -> Result<String> {
        let swf: Vec<Item> = if self.options.also_swf {
            items
                .iter()
                .filter_map(|item| {
                    let url = item.url.strip_suffix(".hab")?;
                    let name = item.path.rsplit('/').next()?.strip_suffix(".hab")?;
                    Some(Item { url: format!("{url}.swf"), path: format!("{SWF_PREFIX}{swf_type}/{name}.swf") })
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut summary = self.fetch_many(what, items, Kind::Hab).await?.summary(hab_what);
        if !swf.is_empty() {
            summary += &format!("; {}", self.fetch_many(&format!("{what} (.swf)"), swf, Kind::Swf).await?.summary(&format!(".swf into {ASSETS_TOOLS}/{}/{swf_type}", aio_assets::workshop::ORIGINAL_SWF)));
        }
        Ok(summary)
    }

    /// Runs one download and returns its summary line.
    pub async fn run(&self, asset: Asset) -> Result<String> {
        match asset {
            Asset::Badges => self.badges().await,
            Asset::Clothes => self.clothes().await,
            Asset::Effects => self.effects().await,
            Asset::Furnidata => self.furnidata().await,
            Asset::Furniture => self.furniture().await,
            Asset::CatalogueIcons => self.catalogue_icons().await,
            Asset::Mp3 => self.mp3().await,
            Asset::ProductData => self.productdata().await,
            Asset::Quests => self.quests().await,
            Asset::Reception => self.reception().await,
            Asset::Texts => self.texts().await,
            Asset::Variables => self.variables().await,
            Asset::Pets => self.pets().await,
            Asset::EffectIcons => self.effect_icons().await,
        }
    }

    async fn badges(&self) -> Result<String> {
        // The hotels share most badges: the names are collected from all of them, each badge is fetched once.
        let mut names = BTreeSet::new();
        for url in &self.urls.badge_texts {
            match self.text(url).await {
                Ok(texts) => {
                    let found = convert::badge_names(&texts);
                    (self.log)(&format!("  {} badges in {url}", found.len()));
                    names.extend(found);
                }
                Err(e) => (self.log)(&format!("  ! {e}")),
            }
        }
        self.stopped()?;
        if names.is_empty() {
            return Err(error("No hotel answered with its texts: no badges to download."));
        }
        let base = format!("{}/album1584", self.urls.c_images());
        let items = names.into_iter().map(|n| Item { url: format!("{base}/{n}.gif"), path: format!("c_images/album1584/{n}.gif") }).collect();
        Ok(self.fetch_many("badges", items, Kind::Image).await?.summary("badges"))
    }

    async fn clothes(&self) -> Result<String> {
        let release = self.release().await?;
        let gordon = self.urls.gordon(&release);
        let figuredata = self.text(&self.urls.figuredata).await?;
        self.save_json("config/FigureData.json", &convert::figuredata(&figuredata, self.options.not_sellable)?)?;
        let figuremap = self.text(&format!("{gordon}/figuremap.xml")).await?;
        let (json, libraries) = convert::figuremap(&figuremap)?;
        self.save_json("config/FigureMap.json", &json)?;
        let items = libraries
            .into_iter()
            .filter(|l| convert::safe_name(l))
            .map(|l| Item { url: format!("{gordon}/{l}.hab"), path: format!("clothes/{l}.hab") })
            .collect();
        self.libraries("clothes libraries", items, "clothes .hab", "clothes").await
    }

    async fn effects(&self) -> Result<String> {
        let release = self.release().await?;
        let gordon = self.urls.gordon(&release);
        let effectmap = self.text(&format!("{gordon}/effectmap.xml")).await?;
        let (json, libraries) = convert::effectmap(&effectmap)?;
        self.save_json("config/EffectMap.json", &json)?;
        let actions = self.text(&format!("{gordon}/HabboAvatarActions.xml")).await?;
        self.save_json("config/HabboAvatarActions.json", &convert::avatar_actions(&actions)?)?;
        let items = libraries
            .into_iter()
            .filter(|l| convert::safe_name(l))
            .map(|l| Item { url: format!("{gordon}/{l}.hab"), path: format!("effect/{l}.hab") })
            .collect();
        self.libraries("effect libraries", items, "effect .hab", "effects").await
    }

    async fn furnidata(&self) -> Result<String> {
        let xml = self.text(&self.urls.furnidata).await?;
        let json = crate::furniture_language::xml_to_json(&xml)?;
        let count = ["roomitemtypes", "wallitemtypes"]
            .iter()
            .map(|s| json[s]["furnitype"].as_array().map_or(0, Vec::len))
            .sum::<usize>();
        self.save_json("config/FurnitureData.json", &json)?;
        Ok(format!("FurnitureData.json with {count} furniture"))
    }

    async fn furniture(&self) -> Result<String> {
        let xml = self.text(&self.urls.furnidata).await?;
        let entries = convert::furnidata_entries(&xml)?;
        let base = self.urls.furniture();
        // One library per furni (its colours share it, at the highest revision); one icon per colour.
        let mut libraries: BTreeMap<String, (String, i64)> = BTreeMap::new();
        for (classname, revision) in &entries {
            let name = classname.split('*').next().unwrap_or("");
            let slot = libraries.entry(name.to_lowercase()).or_insert((name.to_string(), *revision));
            slot.1 = slot.1.max(*revision);
        }
        let items: Vec<Item> = libraries
            .into_values()
            .filter(|(name, _)| convert::safe_name(name))
            .map(|(name, revision)| Item { url: format!("{base}/{revision}/{name}.hab"), path: format!("furniture/{name}.hab") })
            .collect();
        let bundles = self.libraries("furniture libraries", items, "furniture .hab", "furni").await?;
        let mut icons = BTreeSet::new();
        let items: Vec<Item> = entries
            .iter()
            .filter_map(|(classname, revision)| {
                let icon = classname.replacen('*', "_", 1);
                (convert::safe_name(&icon) && icons.insert(icon.clone()))
                    .then(|| Item { url: format!("{base}/{revision}/{icon}_icon.png"), path: format!("icons/{icon}_icon.png") })
            })
            .collect();
        let icons = self.fetch_many("furniture icons", items, Kind::Png).await?;
        Ok(format!("{bundles}; {}", icons.summary("icons")))
    }

    async fn catalogue_icons(&self) -> Result<String> {
        let base = format!("{}/catalogue", self.urls.c_images());
        let counts = self
            .fetch_numbered("catalogue icons", |n| format!("{base}/icon_{n}.png"), |n| format!("c_images/catalogue/icon_{n}.png"), Kind::Png)
            .await?;
        Ok(counts.summary("catalogue icons"))
    }

    async fn mp3(&self) -> Result<String> {
        let base = format!("{}/mp3", self.urls.furniture());
        let counts = self
            .fetch_numbered(
                "sound samples",
                |n| format!("{base}/sound_machine_sample_{n}.mp3"),
                |n| format!("sounds/sound_machine_sample_{n}.mp3"),
                Kind::Mp3,
            )
            .await?;
        Ok(counts.summary("sound samples"))
    }

    async fn productdata(&self) -> Result<String> {
        let text = self.text(&self.urls.productdata).await?;
        let json = convert::productdata(&text);
        let count = json["productdata"]["product"].as_array().map_or(0, Vec::len);
        if count == 0 {
            return Err(error(format!("{} has no products in it.", self.urls.productdata)));
        }
        self.save_json("config/ProductData.json", &json)?;
        Ok(format!("ProductData.json with {count} products"))
    }

    async fn quests(&self) -> Result<String> {
        let texts = self.text(&self.urls.texts).await?;
        let base = format!("{}/Quests", self.urls.c_images());
        let items =
            convert::quest_images(&texts).into_iter().map(|n| Item { url: format!("{base}/{n}"), path: format!("c_images/Quests/{n}") }).collect();
        Ok(self.fetch_many("quest images", items, Kind::Png).await?.summary("quest images"))
    }

    async fn reception(&self) -> Result<String> {
        let variables = self.text(&self.urls.reception_variables).await?;
        let base = self.urls.c_images();
        let items = convert::library_images(&variables)
            .into_iter()
            .map(|p| Item { url: format!("{base}/{p}"), path: format!("c_images/{p}") })
            .collect();
        Ok(self.fetch_many("hotel view images", items, Kind::Image).await?.summary("hotel view images"))
    }

    async fn texts(&self) -> Result<String> {
        let text = self.text(&self.urls.texts).await?;
        let json = convert::key_values(&text);
        let count = json.as_object().map_or(0, |m| m.len());
        self.save_json("config/ExternalTexts.json", &json)?;
        Ok(format!("ExternalTexts.json with {count} texts"))
    }

    async fn variables(&self) -> Result<String> {
        let text = self.text(&self.urls.variables).await?;
        let json = convert::key_values(&text);
        let count = json.as_object().map_or(0, |m| m.len());
        let path = self.root.join(DOWNLOADS).join("ExternalVariables.json");
        let pretty = serde_json::to_string_pretty(&json).map_err(|e| error(e.to_string()))?;
        save(&path, pretty.as_bytes()).map_err(|e| error(format!("Saving {} failed: {e}", path.display())))?;
        Ok(format!("ExternalVariables.json with {count} variables in {}", path.display()))
    }

    async fn pets(&self) -> Result<String> {
        let release = self.release().await?;
        let gordon = self.urls.gordon(&release);
        let items = PETS.iter().map(|p| Item { url: format!("{gordon}/{p}.hab"), path: format!("pets/{p}.hab") }).collect();
        self.libraries("pet libraries", items, "pet .hab", "pets").await
    }

    /// The effect icons from the Habbo Classic app (Habbo has no CDN link for them).
    async fn effect_icons(&self) -> Result<String> {
        let source = self.options.effect_icons_source.clone().filter(|s| !s.trim().is_empty());
        let default = self.root.join("import").join("effect_icons");
        let mut icons = BTreeMap::new();
        match source.as_deref().map(str::trim) {
            Some(link) if link.starts_with("http://") || link.starts_with("https://") => {
                (self.log)(&format!("  Downloading {link} (this can take a while: the app is large) ..."));
                let client = reqwest::Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(900)).build().map_err(|e| error(e.to_string()))?;
                let response = client.get(link).send().await.map_err(|e| error(format!("Could not download {link}: {e}")))?;
                if !response.status().is_success() {
                    return Err(error(format!("{link} answered {}", response.status())));
                }
                let data = response.bytes().await.map_err(|e| error(format!("Could not download {link}: {e}")))?;
                let name = link.rsplit('/').next().unwrap_or("download").to_string();
                icons = hab::icons_from(&name, &data)?;
            }
            Some(path) => read_icon_source(Path::new(path), &mut icons)?,
            None => {
                std::fs::create_dir_all(&default).ok();
                read_icon_source(&default, &mut icons)?;
            }
        }
        if icons.is_empty() {
            return Err(error(format!(
                "No effect icons found. Choose the app.asar of the Habbo Classic app (its resources/app.asar, with {} inside), or put it in {}.",
                hab::INVENTORY_BUNDLE,
                default.display()
            )));
        }
        let (mut written, mut unchanged) = (0, 0);
        for (kind, png) in &icons {
            let path = self.path(&format!("c_images/fx_icons/fx_icon_{kind}.png"));
            if std::fs::read(&path).is_ok_and(|old| &old == png) {
                unchanged += 1;
                continue;
            }
            save(&path, png).map_err(|e| error(format!("Saving {} failed: {e}", path.display())))?;
            written += 1;
        }
        Ok(format!("{written} effect icons written, {unchanged} already up to date"))
    }
}

/// The effect icons of a file or a folder (searched for the bundle, an .asar, HabboClassic*.zip and fx_icon_* files).
fn read_icon_source(path: &Path, icons: &mut BTreeMap<u32, Vec<u8>>) -> Result<()> {
    if path.is_file() {
        let data = std::fs::read(path).map_err(|e| error(format!("{}: {e}", path.display())))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match hab::icons_from(&name, &data) {
            Ok(found) => {
                for (kind, png) in found {
                    icons.entry(kind).or_insert(png);
                }
            }
            Err(e) => {
                // Electron keeps some files outside the archive, in app.asar.unpacked next to it.
                let mut unpacked = path.as_os_str().to_owned();
                unpacked.push(".unpacked");
                let unpacked = PathBuf::from(unpacked);
                if !(name.to_lowercase().ends_with(".asar") && unpacked.is_dir()) {
                    return Err(e);
                }
                read_icon_source(&unpacked, icons)?;
            }
        }
        return Ok(());
    }
    if !path.is_dir() {
        return Err(error(format!("{} does not exist.", path.display())));
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let file = entry.path();
            if file.is_dir() {
                stack.push(file);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let wanted = name == hab::INVENTORY_BUNDLE || name.ends_with(".asar") || (name.starts_with("habboclassic") && name.ends_with(".zip"))
                || name.starts_with("fx_icon_");
            if wanted {
                if let Ok(data) = std::fs::read(&file) {
                    if let Ok(found) = hab::icons_from(&name, &data) {
                        for (kind, png) in found {
                            icons.entry(kind).or_insert(png);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
