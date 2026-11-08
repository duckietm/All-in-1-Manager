//! The catalogue explorer: the hotel catalogue as the game shows it (root pages as tabs, the page tree with its
//! icons, the furni of a page as a grid with an info box), in two windows side by side. Furni are dragged like files:
//! onto another window, a page in a tree, or before another furni (to reorder); with Ctrl held they are copied. Every
//! drop is saved at once (one transaction, `catalog_admin::place_offers`) and can be undone; **Publish** makes the
//! emulator load the catalogue again. The icons come from the downloaded assets (Habbo and the Nitro retro profiles).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::catalog_admin::{self, FieldKind, Furni, FurniColumn, Offer, Page, PageUndo, PlaceUndo};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

/// The game's catalogue colours: the same in the light and the dark theme, as in the client.
mod game {
    use eframe::egui::Color32;
    pub const TITLE: Color32 = Color32::from_rgb(30, 114, 149);
    pub const TITLE_DARK: Color32 = Color32::from_rgb(21, 85, 112);
    pub const FRAME: Color32 = Color32::from_rgb(40, 40, 40);
    pub const BODY: Color32 = Color32::from_rgb(234, 234, 234);
    pub const TAB: Color32 = Color32::from_rgb(200, 210, 218);
    pub const TAB_HOVER: Color32 = Color32::from_rgb(218, 226, 232);
    pub const NAV: Color32 = Color32::WHITE;
    pub const NAV_HOVER: Color32 = Color32::from_rgb(222, 236, 245);
    pub const PAGE_BAR: Color32 = Color32::from_rgb(213, 226, 236);
    pub const TILE: Color32 = Color32::from_rgb(205, 211, 217);
    pub const TILE_EDGE: Color32 = Color32::from_rgb(182, 190, 197);
    pub const TILE_SELECTED: Color32 = Color32::from_rgb(222, 238, 249);
    pub const TEXT: Color32 = Color32::from_rgb(30, 30, 30);
    pub const MUTED: Color32 = Color32::from_rgb(110, 116, 122);
    pub const HIDDEN: Color32 = Color32::from_rgb(150, 150, 150);
    pub const CREDITS: Color32 = Color32::from_rgb(245, 197, 24);
    pub const DUCKETS: Color32 = Color32::from_rgb(196, 112, 220);
    pub const DIAMONDS: Color32 = Color32::from_rgb(91, 192, 235);
    pub const DROP: Color32 = Color32::from_rgb(255, 140, 0);
}

const TILE: f32 = 58.0;
/// The window's tab of the furni no offer sells (not a page id: those are positive).
const UNOFFERED: i64 = i64::MIN;
/// The most unoffered furni shown at once (the search narrows it).
const UNOFFERED_SHOWN: usize = 400;
const NAV_WIDTH: f32 = 200.0;
const ROW: f32 = 24.0;

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// One catalogue window.
#[derive(Default, Clone)]
struct Pane {
    /// The root page whose tab is open.
    root: Option<i64>,
    page: Option<i64>,
    expanded: HashSet<i64>,
    /// The offers selected (ids), and the one a Shift-click counts from.
    selected: Vec<i64>,
    anchor: Option<i64>,
    search: String,
    /// The Without offer tab: the furni selected (items_base ids) and the search.
    furni_selected: Vec<i64>,
    furni_search: String,
}

#[derive(Default)]
struct State {
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    prompt: Option<Prompt>,
    /// The furni editor (right-click on a furni), and the items_base columns once read.
    editor: Option<Editor>,
    /// The image picker over the editor (icon_image, page_headline ...), the files of the server's
    /// c_images/catalogue (None until read), and the files fetched from it for their preview (None: could not be).
    picker: Option<Picker>,
    /// The HTML editor of a page text (page_text1 ...).
    html: Option<HtmlEditor>,
    /// The page builder (a new page with an offer per furni of a furni line).
    builder: Option<Builder>,
    /// The price a furni dragged from Without offer gets: credits, points, points type.
    new_price: (i64, i64, i64),
    /// The furni search of the offer editor (to add furni to a bundle): the text and what it found.
    furni_search: String,
    furni_results: Vec<Furni>,
    catalogue_files: Option<Vec<String>>,
    listing: bool,
    fetched: HashMap<String, Option<Arc<Vec<u8>>>>,
    fetching: HashSet<String>,
    /// The columns of each table edited so far (items_base, catalog_items).
    columns: HashMap<&'static str, Vec<FurniColumn>>,
    login_visible: bool,
    database: String,
    subtitle: String,
    pages: Vec<Page>,
    /// Each page's offers, in the order the catalogue shows them; pages being read.
    offers: HashMap<i64, Vec<Offer>>,
    loading: HashSet<i64>,
    /// The furni of the offers (by items_base id), and the ids asked for already.
    furni: HashMap<i64, Furni>,
    furni_asked: HashSet<i64>,
    panes: [Pane; 2],
    two: bool,
    busy: bool,
    unpublished: bool,
    undo: Option<Undo>,
    /// The furni no offer sells (None until the Without offer tab is opened), and whether they are being read.
    unoffered: Option<Vec<Furni>>,
    unoffered_loading: bool,
    status: Status,
    sample: bool,
}

impl State {
    fn close(&mut self, shared: &Shared) {
        self.generation += 1;
        if let Some(session) = self.session.take() {
            shared.spawn(async move {
                if let Ok(session) = Arc::try_unwrap(session) {
                    session.close().await;
                }
            });
        }
        *self = State { generation: self.generation, two: true, new_price: NEW_PRICE, ..State::default() };
    }

    fn page(&self, id: i64) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// Root pages: parent -1 (or a parent that is not there), in the catalogue's order.
    fn roots(&self) -> Vec<&Page> {
        let ids: HashSet<i64> = self.pages.iter().map(|p| p.id).collect();
        let mut roots: Vec<&Page> = self.pages.iter().filter(|p| p.parent_id <= 0 || !ids.contains(&p.parent_id)).collect();
        roots.sort_by_key(|p| (p.order_num, p.id));
        roots
    }

    fn children(&self, id: i64) -> Vec<&Page> {
        let mut children: Vec<&Page> = self.pages.iter().filter(|p| p.parent_id == id && p.id != id).collect();
        children.sort_by_key(|p| (p.order_num, p.id));
        children
    }

    /// The path of captions down to `id`.
    fn path(&self, id: i64) -> Vec<String> {
        let mut path = Vec::new();
        let mut at = Some(id);
        while let Some(page) = at.and_then(|i| self.page(i)) {
            path.insert(0, page.caption.clone());
            at = (page.parent_id > 0 && page.parent_id != page.id && path.len() < 12).then_some(page.parent_id);
        }
        path
    }

    /// The root page `id` sits under.
    fn root_of(&self, id: i64) -> i64 {
        let mut at = id;
        for _ in 0..12 {
            match self.page(at) {
                Some(page) if page.parent_id > 0 && self.page(page.parent_id).is_some() => at = page.parent_id,
                _ => break,
            }
        }
        at
    }
}

/// One furni's items_base row being edited: each column's value as loaded and as typed (None / ticked: NULL).
struct Editor {
    /// items_base (a furni) or catalog_items (an offer).
    table: &'static str,
    id: i64,
    /// The furni whose icon the window shows, and the page whose offers are read again after a save.
    furni: Option<i64>,
    page: Option<i64>,
    /// Read from the database (None until it is).
    original: Option<Vec<Option<String>>>,
    values: Vec<String>,
    nulls: Vec<bool>,
    saving: bool,
    status: Status,
}

impl Editor {
    /// The columns whose value differs from what was loaded.
    fn changes(&self, columns: &[FurniColumn]) -> Vec<(String, Option<String>)> {
        let Some(original) = &self.original else { return Vec::new() };
        columns
            .iter()
            .enumerate()
            .filter(|(i, c)| c.name != "id" && *i < self.values.len())
            .filter_map(|(i, c)| {
                let now = if self.nulls[i] { None } else { Some(self.values[i].trim().to_string()) };
                (now != original[i]).then(|| (c.name.clone(), now))
            })
            .collect()
    }
}

/// The picker of one editor field: a page icon (icon_image: its number) or a catalogue image (page_headline ...: its
/// name without .gif, as the client adds that).
struct Picker {
    column: usize,
    icon: bool,
    search: String,
}

/// What a new offer costs at first: 3 credits, as on the Catalogue page.
const NEW_PRICE: (i64, i64, i64) = (3, 0, 0);

/// The page builder: under which page, its name, the class name prefix, only furni without an offer, the price, and
/// the furni found (None until Find furni).
struct Builder {
    parent: i64,
    caption: String,
    prefix: String,
    only_new: bool,
    price: (i64, i64, i64),
    found: Option<Vec<Furni>>,
}

/// The HTML editor of one page text: the text as typed and the selection (characters) the toolbar wraps.
struct HtmlEditor {
    column: usize,
    text: String,
    selection: (usize, usize),
}

/// The catalog_pages columns that hold a catalogue image, and whether it is the page icon.
fn image_column(name: &str) -> Option<bool> {
    match name {
        "icon_image" => Some(true),
        "page_headline" | "page_teaser" | "page_special" => Some(false),
        _ => None,
    }
}

/// The number of a page icon file (icon_12.png -> 12).
fn icon_number(file: &str) -> Option<i64> {
    file.strip_prefix("icon_")?.strip_suffix(".png")?.parse().ok()
}

/// What a picked file writes in the field: the icon's number, or the image's name (without .gif: the client adds it).
fn picked_value(file: &str, icon: bool) -> String {
    if icon {
        return icon_number(file).map(|n| n.to_string()).unwrap_or_default();
    }
    file.strip_suffix(".gif").unwrap_or(file).to_string()
}

/// The files a field's value can mean, in the order the client tries them.
fn value_files(value: &str, icon: bool) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if icon {
        return vec![format!("icon_{value}.png")];
    }
    if value.contains('.') {
        vec![value.to_string()]
    } else {
        vec![format!("{value}.gif"), format!("{value}.png")]
    }
}

/// What the last change did, to put it back.
enum Undo {
    Offers(PlaceUndo),
    Page(PageUndo),
}

/// Furni without an offer being dragged (items_base ids).
#[derive(Clone)]
struct DragFurni(Vec<i64>);

/// A page being dragged in a tree.
#[derive(Clone)]
struct DragPage(i64);

/// The offers being dragged, from which window.
#[derive(Clone)]
struct DragOffers {
    ids: Vec<i64>,
    from: usize,
}

enum Action {
    Login,
    Refresh,
    LoadOffers(i64),
    FurniNames(Vec<i64>),
    Place { ids: Vec<i64>, page: i64, before: Option<i64>, copy: bool },
    PlaceFurni { ids: Vec<i64>, page: i64, before: Option<i64> },
    MovePage { id: i64, parent: i64, before: Option<i64> },
    LoadUnoffered,
    EditRow { table: &'static str, id: i64, furni: Option<i64>, page: Option<i64> },
    SaveRow,
    ListCatalogueFiles,
    NewPage(i64),
    DeletePage(i64),
    DeleteOffers { ids: Vec<i64>, page: i64 },
    SortByName(i64),
    OpenBuilder(i64),
    BuilderFind,
    BuilderBuild,
    SearchFurni(String),
    FetchFiles(Vec<String>),
    Undo,
    Publish,
}

/// The icons on this computer: the furni icons (`icons/<class>_icon.png`) of Habbo's download and the retro profiles,
/// and the catalogue's page icons (`c_images/catalogue/icon_<n>.png`). Read when first shown, at most a few per frame.
#[derive(Default)]
struct Images {
    icon_folders: Vec<PathBuf>,
    catalogue_folders: Vec<PathBuf>,
    loaded: HashMap<String, Option<egui::TextureHandle>>,
    read_this_frame: usize,
    /// The files of the local c_images/catalogue folders (read once, for the picker).
    local_catalogue: Option<Vec<String>>,
}

impl Images {
    fn new() -> Self {
        let settings = aio_core::InstallerSettings::load();
        let assets = settings.assets_folder();
        let mut gamedata = vec![aio_remote::habbo_assets::gamedata_folder(&assets)];
        gamedata.extend(settings.retro_profiles().iter().map(|p| aio_remote::habbo_assets::gamedata_folder(&aio_remote::habbo_assets::retro::custom_folder(&assets, p))));
        Images {
            icon_folders: gamedata.iter().map(|g| g.join("icons")).collect(),
            catalogue_folders: gamedata.iter().map(|g| g.join("c_images").join("catalogue")).collect(),
            ..Images::default()
        }
    }

    fn read(&mut self, ctx: &egui::Context, key: String, paths: Vec<PathBuf>) -> Option<egui::TextureHandle> {
        if let Some(found) = self.loaded.get(&key) {
            return found.clone();
        }
        if self.read_this_frame >= 24 {
            ctx.request_repaint();
            return None;
        }
        self.read_this_frame += 1;
        let texture = paths.iter().find_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
            // Pixel art: no smoothing when it is drawn larger.
            Some(ctx.load_texture(&key, color, egui::TextureOptions::NEAREST))
        });
        self.loaded.insert(key, texture.clone());
        texture
    }

    /// A furni's icon by its items_base name ("rare_dragonlamp*4" -> rare_dragonlamp_4_icon.png).
    fn furni(&mut self, ctx: &egui::Context, item_name: &str) -> Option<egui::TextureHandle> {
        let file = format!("{}_icon.png", item_name.trim().replace('*', "_"));
        let paths = self.icon_folders.iter().flat_map(|f| [f.join(&file), f.join(file.to_lowercase())]).collect();
        self.read(ctx, format!("furni:{file}"), paths)
    }

    /// The image files of the local c_images/catalogue folders.
    fn local_catalogue(&mut self) -> Vec<String> {
        if self.local_catalogue.is_none() {
            let mut files: Vec<String> = self
                .catalogue_folders
                .iter()
                .filter_map(|f| std::fs::read_dir(f).ok())
                .flat_map(|entries| entries.flatten().filter_map(|e| e.file_name().into_string().ok()))
                .filter(|n| n.to_lowercase().ends_with(".png") || n.to_lowercase().ends_with(".gif"))
                .collect();
            files.sort();
            files.dedup();
            self.local_catalogue = Some(files);
        }
        self.local_catalogue.clone().unwrap_or_default()
    }

    /// A file of c_images/catalogue: from this computer, else from what was fetched from the server. The second value
    /// is true when it still has to be fetched.
    fn catalogue_file(&mut self, ctx: &egui::Context, file: &str, fetched: &HashMap<String, Option<Arc<Vec<u8>>>>) -> (Option<egui::TextureHandle>, bool) {
        let paths: Vec<PathBuf> = self.catalogue_folders.iter().map(|f| f.join(file)).collect();
        if paths.iter().any(|p| p.is_file()) {
            return (self.read(ctx, format!("page:{file}"), paths), false);
        }
        match fetched.get(file) {
            None => (None, true),
            Some(None) => (None, false),
            Some(Some(bytes)) => {
                let key = format!("server:{file}");
                if let Some(found) = self.loaded.get(&key) {
                    return (found.clone(), false);
                }
                let texture = image::load_from_memory(bytes).ok().map(|image| {
                    let image = image.to_rgba8();
                    let color = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw());
                    ctx.load_texture(&key, color, egui::TextureOptions::NEAREST)
                });
                self.loaded.insert(key, texture.clone());
                (texture, false)
            }
        }
    }

    /// A page icon by its number (icon_image).
    fn page(&mut self, ctx: &egui::Context, number: i64) -> Option<egui::TextureHandle> {
        let file = format!("icon_{number}.png");
        let paths = self.catalogue_folders.iter().map(|f| f.join(&file)).collect();
        self.read(ctx, format!("page:{file}"), paths)
    }
}

pub struct CatalogExplorerView {
    state: Arc<Mutex<State>>,
    images: Images,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl Ctl {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn with<R>(&self, generation: u64, change: impl FnOnce(&mut State) -> R) -> Option<R> {
        let result = {
            let mut state = self.lock();
            (state.generation == generation).then(|| change(&mut state))
        };
        self.shared.ctx.request_repaint();
        result
    }

    fn session(&self) -> Option<(u64, Arc<SqlSession>, String)> {
        let state = self.lock();
        state.session.clone().map(|s| (state.generation, s, state.database.clone()))
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, task: impl FnOnce(Ctl) -> F) {
        self.shared.spawn(task(self.clone()));
    }

    /// Finds the hotel database and logs in to MariaDB like the SQL Editor (the saved account, or asks).
    async fn open(self, ask_first: bool) {
        let Some(server) = self.shared.session() else { return };
        let generation = {
            let mut state = self.lock();
            if state.opening {
                return;
            }
            state.opening = true;
            state.generation
        };
        self.open_inner(server, ask_first, generation).await;
        self.with(generation, |s| s.opening = false);
    }

    async fn open_inner(&self, server: Arc<dyn Server>, ask_first: bool, generation: u64) {
        if server.is_dry_run() {
            self.with(generation, |s| s.status = Status { text: t("Dry run: there is no MariaDB to connect to.").into(), error: true });
            return;
        }
        self.with(generation, |s| s.subtitle = t("Looking for the hotel database ...").into());
        let database = match catalog_admin::hotel_database(&*server).await {
            Ok(database) => database,
            Err(error) => {
                self.with(generation, |s| s.status = Status { text: error.to_string(), error: true });
                return;
            }
        };
        let host = server.login().host.clone();
        let saved = aio_core::config_dir::load_database_credentials(&host);
        let mut login = Login {
            user: saved.as_ref().map(|s| s.root_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into()),
            password: saved.as_ref().map(|s| s.root_password.clone()).unwrap_or_default(),
            port: saved.as_ref().map(|s| s.port).filter(|p| *p > 0).unwrap_or(3306),
        };
        let mut problem = saved.is_none().then(|| t("No saved MariaDB password for this server on this computer: enter an account.").to_string());
        let mut ask = ask_first || saved.is_none();
        let session = loop {
            if ask {
                let (prompt, answer) = Prompt::new(&host, &login, problem.take());
                if self.with(generation, |s| s.prompt = Some(prompt)).is_none() {
                    return;
                }
                match answer.await.ok().flatten() {
                    Some(entered) => login = entered,
                    None => {
                        self.with(generation, |s| {
                            s.login_visible = true;
                            s.subtitle = t("Not logged in to MariaDB. Click Log in to MariaDB.").into();
                        });
                        return;
                    }
                }
            }
            self.with(generation, |s| s.subtitle = tf("Connecting to MariaDB as {user} ...", &[("user", &login.user)]));
            match SqlSession::open(&*server, &login.user, &login.password, login.port).await {
                Ok(session) => break session,
                Err(error) => {
                    let message = error.to_string();
                    problem = Some(if text::is_access_denied(&message) {
                        tf("MariaDB refused {user}: {message}", &[("user", &login.user), ("message", &message)])
                    } else {
                        tf("Could not reach MariaDB on 127.0.0.1:{port} of the server: {message}", &[("port", &login.port.to_string()), ("message", &message)])
                    });
                    ask = true;
                }
            }
        };
        let session = Arc::new(session);
        let subtitle = tf("The catalogue of {database} on {host}, through the SSH tunnel.", &[("database", &database), ("host", &host)]);
        let kept = self.with(generation, |s| {
            s.session = Some(session.clone());
            s.database = database.clone();
            s.login_visible = false;
            s.subtitle = subtitle;
        });
        if kept.is_none() {
            if let Ok(session) = Arc::try_unwrap(session) {
                session.close().await;
            }
            return;
        }
        drop(session);
        self.load_pages().await;
    }

    async fn load_pages(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_pages(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(pages) => {
                    s.pages = pages;
                    s.offers.clear();
                    s.loading.clear();
                    let roots: Vec<i64> = s.roots().iter().map(|p| p.id).collect();
                    for (i, pane) in s.panes.iter_mut().enumerate() {
                        if pane.root.is_none_or(|r| r != UNOFFERED && !roots.contains(&r)) {
                            pane.root = roots.get(i.min(roots.len().saturating_sub(1))).copied();
                            pane.page = pane.root;
                        }
                    }
                    s.status = Status { text: tf("{count} catalogue pages.", &[("count", &s.pages.len().to_string())]), error: false };
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
    }

    async fn load_offers(&self, page: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let loaded = catalog_admin::load_offers(&session, &database, page).await;
        self.with(generation, |s| {
            s.loading.remove(&page);
            match loaded {
                Ok(offers) => {
                    s.offers.insert(page, offers);
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
    }

    async fn furni_names(&self, ids: Vec<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        if let Ok(found) = catalog_admin::furni_by_ids(&session, &database, &ids).await {
            self.with(generation, |s| {
                for furni in found {
                    s.furni.insert(furni.id, furni);
                }
            });
        }
    }

    async fn place(self, ids: Vec<i64>, page: i64, before: Option<i64>, copy: bool) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let result = catalog_admin::place_offers(&session, &database, &ids, page, before, copy).await;
        let caption = self.lock().page(page).map(|p| p.caption.clone()).unwrap_or_default();
        let pages = match &result {
            Ok(undo) => undo.pages.clone(),
            Err(_) => vec![page],
        };
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(undo) => {
                    let count = ids.len().to_string();
                    s.status = Status {
                        text: if copy {
                            tf("{count} furni copied to {page}. Publish to show it in the hotel.", &[("count", &count), ("page", &caption)])
                        } else {
                            tf("{count} furni moved to {page}. Publish to show it in the hotel.", &[("count", &count), ("page", &caption)])
                        },
                        error: false,
                    };
                    s.undo = Some(Undo::Offers(undo));
                    s.unpublished = true;
                    for pane in s.panes.iter_mut() {
                        pane.selected.clear();
                    }
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
        for page in pages {
            self.load_offers(page).await;
        }
    }

    async fn undo(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(undo) = self.lock().undo.take() else { return };
        self.with(generation, |s| s.busy = true);
        let result = match &undo {
            Undo::Offers(undo) => catalog_admin::undo_place(&session, &database, undo).await,
            Undo::Page(undo) => catalog_admin::undo_page_move(&session, &database, undo).await,
        };
        self.with(generation, |s| {
            s.busy = false;
            s.status = match result {
                Ok(_) => Status { text: t("The last drop is undone.").into(), error: false },
                Err(error) => Status { text: error.to_string(), error: true },
            };
        });
        match undo {
            Undo::Offers(undo) => {
                if !undo.inserted.is_empty() {
                    self.with(generation, |s| s.unoffered = None);
                }
                for page in undo.pages {
                    self.load_offers(page).await;
                }
            }
            Undo::Page(_) => self.load_pages().await,
        }
    }

    /// Furni without an offer dropped on a page: an offer each.
    async fn place_furni(self, ids: Vec<i64>, page: i64, before: Option<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        let (furni, caption) = {
            let s = self.lock();
            let furni: Vec<Furni> = ids.iter().filter_map(|id| s.unoffered.iter().flatten().find(|f| f.id == *id).cloned()).collect();
            (furni, s.page(page).map(|p| p.caption.clone()).unwrap_or_default())
        };
        if furni.is_empty() {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let (credits, points, points_type) = self.lock().new_price;
        let result = catalog_admin::place_new_furni(&session, &database, &furni, page, before, credits, points, points_type).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(undo) => {
                    s.status = Status {
                        text: tf("{count} furni got an offer on {page} (3 credits: set the price on the Catalogue page). Publish to show it in the hotel.", &[("count", &furni.len().to_string()), ("page", &caption)]),
                        error: false,
                    };
                    s.undo = Some(Undo::Offers(undo));
                    s.unpublished = true;
                    if let Some(list) = s.unoffered.as_mut() {
                        list.retain(|f| !ids.contains(&f.id));
                    }
                    for pane in s.panes.iter_mut() {
                        pane.furni_selected.clear();
                    }
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
        self.load_offers(page).await;
    }

    async fn move_page(self, id: i64, parent: i64, before: Option<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        let (caption, under) = {
            let s = self.lock();
            (s.page(id).map(|p| p.caption.clone()).unwrap_or_default(), s.page(parent).map(|p| p.caption.clone()).unwrap_or_default())
        };
        self.with(generation, |s| s.busy = true);
        let result = catalog_admin::move_page(&session, &database, id, parent, before).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(undo) => {
                    s.status = Status { text: tf("{page} is now under {parent}. Publish to show it in the hotel.", &[("page", &caption), ("parent", &under)]), error: false };
                    s.undo = Some(Undo::Page(undo));
                    s.unpublished = true;
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
        // The windows stay where they were: load_pages only resets a tab that is gone.
        self.load_pages().await;
    }

    async fn load_unoffered(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        let furni = catalog_admin::load_sellable_furni(&session, &database).await;
        let offers = catalog_admin::load_all_offers(&session, &database).await;
        self.with(generation, |s| {
            s.unoffered_loading = false;
            match (furni, offers) {
                (Ok(furni), Ok(offers)) => {
                    let mut list = catalog_admin::unoffered_furni(&furni, &offers);
                    list.sort_by(|a, b| a.item_name.to_lowercase().cmp(&b.item_name.to_lowercase()));
                    s.unoffered = Some(list);
                }
                (Err(error), _) | (_, Err(error)) => s.status = Status { text: error.to_string(), error: true },
            }
        });
    }

    /// Opens the editor of row `id` of `table`: the table's columns (read once) and the row.
    async fn edit_row(self, table: &'static str, id: i64, furni: Option<i64>, page: Option<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| {
            s.editor = Some(Editor { table, id, furni, page, original: None, values: Vec::new(), nulls: Vec::new(), saving: false, status: Status::default() });
        });
        let cached = self.lock().columns.get(table).cloned();
        let columns = match cached {
            Some(columns) => Ok(columns),
            None => catalog_admin::table_columns(&session, &database, table).await,
        };
        let row = catalog_admin::load_row(&session, &database, table, id).await;
        self.with(generation, |s| {
            let Some(editor) = s.editor.as_mut().filter(|e| e.id == id && e.table == table) else { return };
            match (columns, row) {
                (Ok(columns), Ok(row)) => {
                    let original: Vec<Option<String>> = columns.iter().map(|c| row.iter().find(|(name, _)| *name == c.name).and_then(|(_, v)| v.clone())).collect();
                    editor.values = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
                    editor.nulls = original.iter().map(Option::is_none).collect();
                    editor.original = Some(original);
                    s.columns.insert(table, columns);
                }
                (Err(error), _) | (_, Err(error)) => editor.status = Status { text: error.to_string(), error: true },
            }
        });
    }

    async fn save_row(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (table, id, page, changes, cleaned) = {
            let mut s = self.lock();
            let Some(table) = s.editor.as_ref().map(|e| e.table) else { return };
            let columns = s.columns.get(table).cloned().unwrap_or_default();
            let Some(editor) = s.editor.as_mut() else { return };
            let mut changes = editor.changes(&columns);
            // Page texts keep only the HTML Octane and the gameserver both show, however they were typed.
            let mut cleaned: Vec<String> = Vec::new();
            if table == "catalog_pages" {
                for (name, value) in changes.iter_mut() {
                    if let (true, Some(text)) = (catalog_admin::PAGE_TEXT_COLUMNS.contains(&name.as_str()), value.as_mut()) {
                        let safe = catalog_admin::safe_page_html(text);
                        if safe != *text {
                            cleaned.push(name.clone());
                            *text = safe;
                        }
                    }
                }
            }
            if let Some(problem) = changes.iter().find_map(|(name, value)| {
                let column = columns.iter().find(|c| c.name == *name)?;
                value.as_deref().and_then(|v| catalog_admin::field_problem(column, v))
            }) {
                editor.status = Status { text: problem, error: true };
                return;
            }
            if changes.is_empty() {
                editor.status = Status { text: t("Nothing changed.").into(), error: false };
                return;
            }
            editor.saving = true;
            (editor.table, editor.id, editor.page, changes, cleaned)
        };
        let result = catalog_admin::save_row(&session, &database, table, id, &changes).await;
        let ok = result.is_ok();
        self.with(generation, |s| {
            if result.is_ok() && table != "items_base" {
                s.unpublished = true;
            }
            let Some(editor) = s.editor.as_mut() else { return };
            editor.saving = false;
            editor.status = match result {
                Ok(_) if table == "catalog_items" => {
                    Status { text: tf("Saved {count} values of the offer. Publish to show it in the hotel.", &[("count", &changes.len().to_string())]), error: false }
                }
                Ok(_) if table == "catalog_pages" => {
                    let mut text = tf("Saved {count} values of the page. Publish to show it in the hotel.", &[("count", &changes.len().to_string())]);
                    if !cleaned.is_empty() {
                        text = format!("{text} {}", tf("HTML the client does not show was taken out of {columns}.", &[("columns", &cleaned.join(", "))]));
                    }
                    Status { text, error: false }
                }
                Ok(_) => Status {
                    text: tf("Saved {count} values. The emulator reads items_base when it starts: restart it (Tools), or :update_items in the hotel.", &[("count", &changes.len().to_string())]),
                    error: false,
                },
                Err(error) => Status { text: error.to_string(), error: true },
            };
        });
        if ok {
            // The new values as the database has them, and the windows: the names of a furni, the page of an offer.
            let row = catalog_admin::load_row(&session, &database, table, id).await;
            let names = if table == "items_base" { catalog_admin::furni_by_ids(&session, &database, &[id]).await.ok() } else { None };
            let moved_to = row.as_ref().ok().and_then(|r| r.iter().find(|(n, _)| n == "page_id")).and_then(|(_, v)| v.as_deref()?.trim().parse::<i64>().ok());
            self.with(generation, |s| {
                let columns = s.columns.get(table).cloned().unwrap_or_default();
                if let (Some(editor), Ok(row)) = (s.editor.as_mut().filter(|e| e.id == id && e.table == table), row) {
                    let original: Vec<Option<String>> = columns.iter().map(|c| row.iter().find(|(name, _)| *name == c.name).and_then(|(_, v)| v.clone())).collect();
                    editor.values = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
                    editor.nulls = original.iter().map(Option::is_none).collect();
                    editor.original = Some(original);
                }
                for furni in names.into_iter().flatten() {
                    if let Some(list) = s.unoffered.as_mut() {
                        if let Some(entry) = list.iter_mut().find(|f| f.id == furni.id) {
                            *entry = furni.clone();
                        }
                    }
                    s.furni.insert(furni.id, furni);
                }
            });
            if table == "catalog_items" {
                for page in [page, moved_to].into_iter().flatten().collect::<HashSet<i64>>() {
                    self.load_offers(page).await;
                }
            }
            if table == "catalog_pages" {
                // The tree: its caption, icon, parent, order, visible ... (the windows stay where they were).
                self.load_pages().await;
            }
        }
    }

    /// The image files of the server's gamedata/c_images/catalogue, for the picker.
    async fn list_catalogue_files(self) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let folder = format!("{}/c_images/catalogue", aio_remote::octane::gamedata());
        let command = format!(
            "find {} -maxdepth 1 -type f \\( -iname '*.gif' -o -iname '*.png' \\) -printf '%f\\n' 2>/dev/null | sort; true",
            aio_remote::server::shell_quote(&folder)
        );
        let result = server.run_as_root(&command).await;
        self.with(generation, |s| {
            s.listing = false;
            s.catalogue_files = Some(match result {
                Ok(result) => result.output.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains('/')).map(str::to_string).collect(),
                Err(error) => {
                    if let Some(editor) = s.editor.as_mut() {
                        editor.status = Status { text: error.to_string(), error: true };
                    }
                    Vec::new()
                }
            });
        });
    }

    /// Fetches these files of the server's c_images/catalogue for their preview, one after the other.
    async fn fetch_files(self, files: Vec<String>) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let folder = format!("{}/c_images/catalogue", aio_remote::octane::gamedata());
        let temp = std::env::temp_dir().join(format!("all-in-1-catalogue-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp);
        for file in files {
            let local = temp.join(&file);
            let bytes = match server.download(&format!("{folder}/{file}"), &local, &|_| {}).await {
                Ok(()) => std::fs::read(&local).ok().map(Arc::new),
                Err(_) => None,
            };
            let _ = std::fs::remove_file(&local);
            if self.with(generation, |s| {
                s.fetching.remove(&file);
                s.fetched.insert(file.clone(), bytes);
            })
            .is_none()
            {
                return;
            }
        }
    }

    /// A new page under `parent` (hidden until Visible is ticked), then its editor to name it.
    async fn new_page(self, parent: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let page = catalog_admin::new_page(&self.lock().pages, parent);
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_page(&session, &database, &page).await;
        self.with(generation, |s| s.busy = false);
        match saved {
            Ok(id) => {
                self.with(generation, |s| {
                    s.unpublished = true;
                    s.status = Status { text: t("New page made, hidden until you tick visible: give it a name in the editor.").into(), error: false };
                    for pane in s.panes.iter_mut() {
                        pane.expanded.insert(parent);
                    }
                });
                self.load_pages().await;
                self.edit_row("catalog_pages", id, None, None).await;
            }
            Err(error) => {
                self.with(generation, |s| s.status = Status { text: error.to_string(), error: true });
            }
        }
    }

    /// Asks first, then deletes the page with its offers (never one with pages under it or sold limited offers).
    async fn delete_page(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let offers = match catalog_admin::load_offers(&session, &database, id).await {
            Ok(offers) => offers,
            Err(error) => {
                self.with(generation, |s| s.status = Status { text: error.to_string(), error: true });
                return;
            }
        };
        let (problem, page) = {
            let s = self.lock();
            (catalog_admin::page_delete_problem(&s.pages, &offers, id), s.page(id).cloned())
        };
        let Some(page) = page else { return };
        if let Some(problem) = problem {
            self.with(generation, |s| s.status = Status { text: problem, error: true });
            return;
        }
        let message = tf("The page {name} and its {count} offers are deleted from the database. This cannot be undone.", &[("name", &page.caption), ("count", &offers.len().to_string())]);
        let options = vec![Choice::new(t("Delete the page"), t("Delete it and its offers."), true).primary(), Choice::new(t("Cancel"), t("Keep the page."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &page.caption)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_page(&session, &database, id).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.undo = None;
                    s.status = Status { text: tf("Page {name} deleted. Publish to take it out of the hotel.", &[("name", &page.caption)]), error: false };
                    // A window on the page goes to the page above it.
                    for pane in s.panes.iter_mut() {
                        if pane.page == Some(id) {
                            pane.page = Some(page.parent_id).filter(|p| *p > 0).or(pane.root);
                            pane.selected.clear();
                        }
                    }
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
        self.load_pages().await;
    }

    /// Asks first, then deletes these offers of `page` (never one with limited sales: its record of who has which
    /// number stays).
    async fn delete_offers(self, ids: Vec<i64>, page: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let offers: Vec<Offer> = self.lock().offers.get(&page).map(|o| o.iter().filter(|o| ids.contains(&o.id)).cloned().collect()).unwrap_or_default();
        if offers.is_empty() {
            return;
        }
        if let Some(problem) = offers.iter().find_map(catalog_admin::offer_delete_problem) {
            self.with(generation, |s| s.status = Status { text: problem, error: true });
            return;
        }
        let heading = if offers.len() == 1 { tf("Delete {name}?", &[("name", &offers[0].catalog_name)]) } else { tf("Delete {count} offers?", &[("count", &offers.len().to_string())]) };
        let options = vec![Choice::new(t("Delete"), t("Take them off the page for good."), true).primary(), Choice::new(t("Cancel"), t("Keep them."), false)];
        let message = t("The offers are deleted from the database. Furni players already bought stays theirs. This cannot be undone.");
        if !self.shared.dialogs.choose(t("Are you sure?"), &heading, message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let ids: Vec<i64> = offers.iter().map(|o| o.id).collect();
        let deleted = catalog_admin::delete_offers(&session, &database, &ids).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.undo = None;
                    s.unoffered = None;
                    for pane in s.panes.iter_mut() {
                        pane.selected.retain(|id| !ids.contains(id));
                    }
                    s.status = Status { text: tf("{count} offers deleted. Publish to take them out of the hotel.", &[("count", &ids.len().to_string())]), error: false };
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
        self.load_offers(page).await;
    }

    /// Puts the page's furni in the order of their names (Undo puts the old order back).
    async fn sort_by_name(self, page: i64) {
        let Some(offers) = self.lock().offers.get(&page).cloned() else { return };
        let order: Vec<i64> = catalog_admin::sorted_by_name(&offers).into_iter().map(|(id, _)| id).collect();
        if order.len() > 1 {
            let ctl = self.clone();
            self.place(order, page, None, false).await;
            let generation = ctl.lock().generation;
            ctl.with(generation, |s| s.status = Status { text: t("The furni of the page are in the order of their names. Publish to show it in the hotel.").into(), error: false });
        }
    }

    async fn builder_find(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((prefix, only_new)) = self.lock().builder.as_ref().map(|b| (b.prefix.trim().to_string(), b.only_new)) else { return };
        self.with(generation, |s| s.busy = true);
        let furni = catalog_admin::furni_by_prefix(&session, &database, &prefix).await;
        let offers = if only_new { catalog_admin::load_all_offers(&session, &database).await.map(Some) } else { Ok(None) };
        self.with(generation, |s| {
            s.busy = false;
            match (furni, offers) {
                (Ok(furni), Ok(offers)) => {
                    let found = match offers {
                        Some(offers) => catalog_admin::unoffered_furni(&furni, &offers),
                        None => furni,
                    };
                    if let Some(builder) = s.builder.as_mut() {
                        builder.found = Some(found);
                    }
                }
                (Err(error), _) | (_, Err(error)) => s.status = Status { text: error.to_string(), error: true },
            }
        });
    }

    /// Adds the page, then an offer for each furni found on it, and opens it in the first window.
    async fn builder_build(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (page, found, price) = {
            let s = self.lock();
            let Some(builder) = s.builder.as_ref() else { return };
            let Some(found) = builder.found.clone().filter(|f| !f.is_empty()) else { return };
            let mut page = catalog_admin::new_page(&s.pages, builder.parent);
            page.caption = builder.caption.trim().to_string();
            (page, found, builder.price)
        };
        if let Some(problem) = catalog_admin::page_problem(&page, &self.lock().pages) {
            self.with(generation, |s| s.status = Status { text: problem, error: true });
            return;
        }
        self.with(generation, |s| s.busy = true);
        let page_id = match catalog_admin::save_page(&session, &database, &page).await {
            Ok(id) => id,
            Err(error) => {
                self.with(generation, |s| {
                    s.busy = false;
                    s.status = Status { text: error.to_string(), error: true };
                });
                return;
            }
        };
        let offers = catalog_admin::offers_for(&found, page_id, &[], price.0, price.1, price.2);
        let added = catalog_admin::add_offers(&session, &database, &offers).await;
        self.with(generation, |s| {
            s.busy = false;
            s.unpublished = true;
            s.unoffered = None;
            match added {
                Ok(count) => {
                    s.builder = None;
                    s.status = Status {
                        text: tf("Page {page} made with {count} offers. It is hidden until you tick Visible; publish to put it in the hotel.", &[("page", &page.caption), ("count", &count.to_string())]),
                        error: false,
                    };
                    s.panes[0].page = Some(page_id);
                    s.panes[0].expanded.insert(page.parent_id);
                }
                Err(error) => s.status = Status { text: tf("The page was made, but its offers were not: {error}", &[("error", &error.to_string())]), error: true },
            }
        });
        self.load_pages().await;
        let root = self.lock().root_of(page_id);
        self.with(generation, |s| s.panes[0].root = Some(root));
    }

    async fn search_furni(&self, text: String) {
        let Some((generation, session, database)) = self.session() else { return };
        match catalog_admin::search_furni(&session, &database, &text).await {
            Ok(found) => {
                self.with(generation, |s| {
                    for furni in &found {
                        s.furni.insert(furni.id, furni.clone());
                    }
                    s.furni_results = found;
                });
            }
            Err(error) => {
                self.with(generation, |s| s.status = Status { text: error.to_string(), error: true });
            }
        }
    }

    async fn publish(self) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        self.with(generation, |s| {
            s.busy = true;
            s.status = Status { text: t("Asking the emulator to load the catalogue again ...").into(), error: false };
        });
        let result = catalog_admin::reload_catalog(&*server).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.unpublished = false;
                    s.status = Status { text: t("Published: the emulator loaded the catalogue again and the players online were told to refresh it.").into(), error: false };
                }
                Err(error) => s.status = Status { text: error.to_string(), error: true },
            }
        });
    }
}

impl CatalogExplorerView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State { two: true, new_price: NEW_PRICE, ..State::default() })), images: Images::new() }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    fn connect_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = server_id(&server);
        {
            let mut state = self.state.lock().unwrap();
            if state.session_for == Some(id) {
                return;
            }
            state.close(shared);
            state.session_for = Some(id);
            // The screenshot of the page shows a sample catalogue: the dry run has no database.
            if server.is_dry_run() && std::env::var_os("AIO_SCREENSHOT").is_some() {
                sample(&mut state);
                return;
            }
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

impl View for CatalogExplorerView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connect_if_needed(shared);
        self.images.read_this_frame = 0;
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock().unwrap();
            let s = &mut *guard;
            editor_window(&ctx, palette, s, &mut self.images, &mut actions);
            picker_window(&ctx, palette, s, &mut self.images, &mut actions);
            html_window(&ctx, palette, s);
            builder_window(&ctx, palette, s, &mut actions);
            // The Delete key deletes the furni selected in a window (asks first).
            if s.editor.is_none() && s.builder.is_none() && !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                let count = if s.two { 2 } else { 1 };
                if let Some(pane) = s.panes.iter().take(count).find(|p| !p.selected.is_empty() && p.page.is_some()) {
                    let page = pane.page.unwrap_or_default();
                    actions.push(Action::DeleteOffers { ids: pane.selected.clone(), page });
                }
            }
            if let Some(prompt) = s.prompt.as_mut() {
                if prompt.show(&ctx, palette) {
                    s.prompt = None;
                }
            }
            header(ui, palette, s, shared, &mut actions);
            ui.add_space(8.0);
            if !s.pages.is_empty() {
                let count = if s.two { 2 } else { 1 };
                let gap = 12.0;
                let width = ((ui.available_width() - gap * (count as f32 - 1.0)) / count as f32).max(420.0);
                let height = (ui.available_height() - 20.0).max(380.0);
                ui.horizontal_top(|ui| {
                    for index in 0..count {
                        if index > 0 {
                            ui.add_space(gap - ui.spacing().item_spacing.x);
                        }
                        ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                            window(ui, s, &mut self.images, index, width, height, &mut actions);
                        });
                    }
                });
                drag_label(&ctx, s, &mut self.images);
                // What the windows need: their page's offers and the furni of those offers.
                for pane in s.panes.iter().take(count) {
                    if let Some(page) = pane.page {
                        if !s.offers.contains_key(&page) && !s.loading.contains(&page) && !s.sample {
                            actions.push(Action::LoadOffers(page));
                        }
                    }
                }
                for action in &actions {
                    if let Action::LoadOffers(page) = action {
                        s.loading.insert(*page);
                    }
                }
                let mut missing: Vec<i64> = Vec::new();
                for pane in s.panes.iter().take(count) {
                    for offer in pane.page.and_then(|p| s.offers.get(&p)).into_iter().flatten() {
                        if let Some(id) = first_furni(offer) {
                            if !s.furni.contains_key(&id) && !s.furni_asked.contains(&id) && !missing.contains(&id) {
                                missing.push(id);
                            }
                        }
                    }
                }
                if !missing.is_empty() && !s.sample {
                    s.furni_asked.extend(missing.iter().copied());
                    actions.push(Action::FurniNames(missing));
                }
            }
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move { c.load_pages().await }),
                Action::LoadOffers(page) => ctl.spawn(move |c| async move { c.load_offers(page).await }),
                Action::FurniNames(ids) => ctl.spawn(move |c| async move { c.furni_names(ids).await }),
                Action::Place { ids, page, before, copy } => {
                    if self.state.lock().unwrap().sample {
                        sample_place(&mut self.state.lock().unwrap(), &ids, page, before, copy);
                    } else {
                        ctl.spawn(move |c| c.place(ids, page, before, copy));
                    }
                }
                Action::PlaceFurni { ids, page, before } => ctl.spawn(move |c| c.place_furni(ids, page, before)),
                Action::MovePage { id, parent, before } => ctl.spawn(move |c| c.move_page(id, parent, before)),
                Action::LoadUnoffered => ctl.spawn(|c| async move { c.load_unoffered().await }),
                Action::EditRow { table, id, furni, .. } if self.state.lock().unwrap().sample => sample_editor(&mut self.state.lock().unwrap(), table, id, furni),
                Action::EditRow { table, id, furni, page } => ctl.spawn(move |c| c.edit_row(table, id, furni, page)),
                Action::SaveRow => ctl.spawn(|c| c.save_row()),
                Action::ListCatalogueFiles => ctl.spawn(|c| c.list_catalogue_files()),
                Action::NewPage(parent) => ctl.spawn(move |c| c.new_page(parent)),
                Action::DeletePage(id) => ctl.spawn(move |c| c.delete_page(id)),
                Action::DeleteOffers { ids, page } => ctl.spawn(move |c| c.delete_offers(ids, page)),
                Action::SortByName(page) => ctl.spawn(move |c| c.sort_by_name(page)),
                Action::OpenBuilder(parent) => {
                    let mut s = self.state.lock().unwrap();
                    s.builder = Some(Builder { parent, caption: String::new(), prefix: String::new(), only_new: true, price: s.new_price, found: None });
                }
                Action::BuilderFind => ctl.spawn(|c| async move { c.builder_find().await }),
                Action::BuilderBuild => ctl.spawn(|c| c.builder_build()),
                Action::SearchFurni(text) => ctl.spawn(move |c| async move { c.search_furni(text).await }),
                Action::FetchFiles(files) => ctl.spawn(move |c| c.fetch_files(files)),
                Action::Undo => ctl.spawn(|c| c.undo()),
                Action::Publish => ctl.spawn(|c| c.publish()),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.state.lock().unwrap().close(shared);
    }
}

/// A front page (layout frontpage, frontpage4, frontpage_featured ...): it shows promotions, never furni, so
/// nothing can be dropped on it.
fn is_front_page(page: &Page) -> bool {
    page.layout.trim().to_lowercase().starts_with("frontpage")
}

/// What was dropped on `response`, when it is a `P`. egui's `dnd_release_payload` takes the payload before it checks
/// its type, so asking for the wrong type first would throw a dragged page or furni away: look first.
fn released<P: std::any::Any + Send + Sync>(response: &egui::Response) -> Option<Arc<P>> {
    if egui::DragAndDrop::has_payload_of_type::<P>(&response.ctx) {
        response.dnd_release_payload::<P>()
    } else {
        None
    }
}

/// The furni an offer shows: its first items_base id.
fn first_furni(offer: &Offer) -> Option<i64> {
    catalog_admin::item_ids(&offer.item_ids).and_then(|ids| ids.first().copied())
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            let ready = (s.session.is_some() || s.sample) && !s.busy;
            let publish = if s.unpublished { t("Publish changes") } else { t("Publish") };
            if ui.add_enabled(ready && !s.sample, widgets::primary_button(publish)).on_hover_text(t("The emulator loads the catalogue again and tells the players online to refresh it.")).clicked() {
                actions.push(Action::Publish);
            }
            if ui.add_enabled(ready && s.undo.is_some(), widgets::secondary_button(t("Undo"))).on_hover_text(t("Puts the last drop back.")).clicked() {
                actions.push(Action::Undo);
            }
            if ui.add_enabled(ready && !s.sample, widgets::secondary_button(t("Refresh"))).clicked() {
                actions.push(Action::Refresh);
            }
            let label = if s.two { t("One window") } else { t("Two windows") };
            if ui.add(widgets::secondary_button(label)).clicked() {
                s.two = !s.two;
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    let text = if s.status.text.is_empty() {
        t("Drag furni onto the other window, a page, or before another furni; hold Ctrl to copy. Ctrl- or Shift-click selects more.").to_string()
    } else {
        s.status.text.clone()
    };
    let color = if s.status.error { palette.error_text } else { palette.muted };
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
}

/// One catalogue window, drawn like the game's.
fn window(ui: &mut egui::Ui, s: &mut State, images: &mut Images, index: usize, width: f32, height: f32, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    egui::Frame::new()
        .fill(game::BODY)
        .stroke(Stroke::new(1.0_f32, game::FRAME))
        .corner_radius(CornerRadius::same(8))
        .show(ui, |ui| {
            ui.set_width(width - 2.0);
            ui.set_height(height - 2.0);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            title_bar(ui, s, index, width - 2.0);
            tabs(ui, &ctx, s, images, index, actions);
            ui.horizontal_top(|ui| {
                let body_height = ui.available_height();
                ui.allocate_ui_with_layout(egui::vec2(NAV_WIDTH, body_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                    navigation(ui, &ctx, s, images, index, body_height, actions);
                });
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), body_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                    content(ui, &ctx, s, images, index, actions);
                });
            });
        });
}

fn title_bar(ui: &mut egui::Ui, s: &mut State, index: usize, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 30.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, game::TITLE);
    painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, game::TITLE_DARK));
    let path = s.panes[index].page.map(|p| s.path(p).join(" › ")).unwrap_or_default();
    painter.text(rect.left_center() + egui::vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, t("Catalogue"), egui::FontId::proportional(14.0), Color32::WHITE);
    let galley = painter.layout(path, egui::FontId::proportional(11.5), Color32::from_rgb(214, 234, 244), (rect.width() - 120.0).max(40.0));
    let at = egui::pos2(rect.right() - 10.0 - galley.size().x, rect.center().y - galley.size().y / 2.0);
    painter.galley(at, galley, Color32::WHITE);
}

/// The root pages as tabs.
fn tabs(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize, actions: &mut Vec<Action>) {
    let mut roots: Vec<(i64, String, i64, bool)> = s.roots().iter().map(|p| (p.id, p.caption.clone(), p.icon_image, p.visible)).collect();
    roots.push((UNOFFERED, t("Without offer").to_string(), -1, true));
    let mut chosen = None;
    egui::Frame::new().fill(game::TAB).inner_margin(egui::Margin { left: 6, right: 6, top: 6, bottom: 0 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::horizontal().id_salt(("catalog-tabs", index)).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (id, caption, icon, visible) in &roots {
                    let active = s.panes[index].root == Some(*id);
                    let font = egui::FontId::proportional(12.0);
                    let text_width = ui.painter().layout_no_wrap(caption.clone(), font.clone(), game::TEXT).size().x;
                    let size = egui::vec2(text_width + if *id == UNOFFERED { 18.0 } else { 34.0 }, 26.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    // A page dropped on a tab goes under that root page.
                    let page_drop = *id != UNOFFERED && response.dnd_hover_payload::<DragPage>().is_some();
                    let fill = if active || page_drop { game::BODY } else if response.hovered() { game::TAB_HOVER } else { game::TAB };
                    ui.painter().rect_filled(rect, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, fill);
                    if page_drop {
                        ui.painter().rect_stroke(rect, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, Stroke::new(2.0_f32, game::DROP), egui::StrokeKind::Inside);
                    }
                    let mut x = rect.left() + 9.0;
                    if *id != UNOFFERED {
                        if let Some(texture) = images.page(ctx, *icon) {
                            sharp_image(ui.painter(), &texture, rect.left_center() + egui::vec2(14.0, 0.0), egui::vec2(22.0, 22.0), 1);
                        }
                        x = rect.left() + 26.0;
                    }
                    let color = if *id == UNOFFERED { game::TITLE } else if *visible { game::TEXT } else { game::HIDDEN };
                    ui.painter().text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, caption, font, color);
                    if *id != UNOFFERED {
                        response.context_menu(|ui| page_menu(ui, s, *id, false, actions));
                    }
                    if let Some(page) = released::<DragPage>(&response) {
                        if *id != UNOFFERED {
                            actions.push(Action::MovePage { id: page.0, parent: *id, before: None });
                        }
                    } else if response.clicked() {
                        chosen = Some(*id);
                    }
                }
            });
        });
    });
    if let Some(id) = chosen {
        let pane = &mut s.panes[index];
        pane.root = Some(id);
        pane.page = (id != UNOFFERED).then_some(id);
        pane.selected.clear();
        pane.search.clear();
        if id == UNOFFERED && s.unoffered.is_none() && !s.unoffered_loading && !s.sample {
            s.unoffered_loading = true;
            actions.push(Action::LoadUnoffered);
        }
    }
}

/// Text fields as the game draws them: white with a grey edge and dark text, also in the dark theme.
fn light_fields(ui: &mut egui::Ui) {
    let visuals = ui.visuals_mut();
    visuals.extreme_bg_color = Color32::WHITE;
    visuals.override_text_color = Some(game::TEXT);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, game::TILE_EDGE);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, game::TITLE);
    visuals.selection.stroke = Stroke::new(1.0_f32, game::TITLE);
    visuals.text_cursor.stroke = Stroke::new(1.5_f32, game::TEXT);
}

/// Draws `texture` as the game does: each of its pixels on whole screen pixels (1:1, or `zoom` times when that fits),
/// on the screen's pixel grid, so the icons stay sharp at every display scale (125 %, 150 %, ...). An image larger
/// than `room` is made smaller to fit.
fn sharp_image(painter: &egui::Painter, texture: &egui::TextureHandle, center: egui::Pos2, room: egui::Vec2, zoom: u32) {
    let ppp = painter.ctx().pixels_per_point();
    let [w, h] = texture.size().map(|v| v.max(1) as f32);
    let fits = ((room.x * ppp) / w).min((room.y * ppp) / h);
    let factor = if fits >= 1.0 { fits.floor().clamp(1.0, zoom.max(1) as f32) } else { fits };
    let size = egui::vec2(w * factor / ppp, h * factor / ppp);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let min = egui::pos2(snap(center.x - size.x / 2.0), snap(center.y - size.y / 2.0));
    painter.image(texture.id(), egui::Rect::from_min_size(min, size), uv(), Color32::WHITE);
}

fn uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// Where a page dragged over a tree row lands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Before,
    Into,
    After,
}

/// The page tree of the open tab (or, with a search, every page that matches). Each row takes dropped furni, and
/// rows are dragged like folders: onto the middle of a page to go under it, onto its top or bottom edge to go before
/// or after it.
fn navigation(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize, height: f32, actions: &mut Vec<Action>) {
    egui::Frame::new().fill(game::NAV).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
        ui.set_width(NAV_WIDTH - 12.0);
        ui.set_height(height - 12.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        light_fields(ui);
        if s.panes[index].root == Some(UNOFFERED) {
            ui.add(egui::TextEdit::singleline(&mut s.panes[index].furni_search).hint_text(RichText::new(t("Search furni")).italics()).desired_width(NAV_WIDTH - 12.0));
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(t("Furni no offer sells yet. Drag them onto a page in the other window (or a page in its tree): each gets an offer at the price below there.")).size(11.5).color(game::MUTED)).wrap());
            ui.add_space(8.0);
            ui.label(RichText::new(t("Price of the new offers")).size(12.0).strong().color(game::TEXT));
            let price = &mut s.new_price;
            egui::Grid::new(("catalog-new-price", index)).num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new(t("Credits")).size(11.5).color(game::TEXT));
                ui.add(egui::DragValue::new(&mut price.0).range(0..=1_000_000_000));
                ui.end_row();
                ui.label(RichText::new(t("Points")).size(11.5).color(game::TEXT));
                ui.add(egui::DragValue::new(&mut price.1).range(0..=1_000_000_000));
                ui.end_row();
                ui.label(RichText::new(t("Points type")).size(11.5).color(game::TEXT)).on_hover_text(t("0 duckets, 5 diamonds; other numbers are the hotel's own points."));
                ui.add(egui::DragValue::new(&mut price.2).range(0..=1000));
                ui.end_row();
            });
            return;
        }
        ui.add(egui::TextEdit::singleline(&mut s.panes[index].search).hint_text(RichText::new(t("Search pages")).italics()).desired_width(NAV_WIDTH - 12.0));
        let search = s.panes[index].search.trim().to_lowercase();
        let mut rows: Vec<(i64, usize, bool)> = Vec::new();
        if search.is_empty() {
            if let Some(root) = s.panes[index].root {
                // depth first: the tab's pages, opened where expanded.
                let mut stack: Vec<(i64, usize)> = s.children(root).iter().rev().map(|p| (p.id, 0)).collect();
                while let Some((id, depth)) = stack.pop() {
                    let children: Vec<i64> = s.children(id).iter().map(|p| p.id).collect();
                    rows.push((id, depth, !children.is_empty()));
                    if s.panes[index].expanded.contains(&id) && depth < 10 {
                        stack.extend(children.iter().rev().map(|c| (*c, depth + 1)));
                    }
                }
            }
        } else {
            rows = s.pages.iter().filter(|p| p.caption.to_lowercase().contains(&search) || p.id.to_string() == search).map(|p| (p.id, 0, false)).collect();
        }
        egui::ScrollArea::vertical().id_salt(("catalog-nav", index)).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for (id, depth, has_children) in rows {
                let Some(page) = s.page(id).cloned() else { continue };
                let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::click_and_drag());
                let active = s.panes[index].page == Some(id);
                let locked = is_front_page(&page);
                let furni_over = !locked && (response.dnd_hover_payload::<DragOffers>().is_some() || response.dnd_hover_payload::<DragFurni>().is_some());
                // A page dragged over this row: before, into or after it (never into itself or its own pages).
                let page_zone = response.dnd_hover_payload::<DragPage>().and_then(|dragged| {
                    let y = ctx.pointer_latest_pos()?.y;
                    let zone = if y < rect.top() + rect.height() * 0.28 {
                        Zone::Before
                    } else if y > rect.bottom() - rect.height() * 0.28 {
                        Zone::After
                    } else {
                        Zone::Into
                    };
                    let parent = if zone == Zone::Into { id } else { page.parent_id };
                    let allowed = dragged.0 != id && catalog_admin::page_move_problem(&s.pages, dragged.0, parent).is_none() && !(zone == Zone::Into && locked);
                    allowed.then_some(zone)
                });
                let highlight = furni_over || page_zone == Some(Zone::Into);
                let fill = if highlight {
                    game::TILE_SELECTED
                } else if active {
                    game::TITLE
                } else if response.hovered() {
                    game::NAV_HOVER
                } else {
                    game::NAV
                };
                ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
                if highlight {
                    ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(2.0_f32, game::DROP), egui::StrokeKind::Inside);
                }
                response.context_menu(|ui| page_menu(ui, s, id, false, actions));
                match page_zone {
                    Some(Zone::Before) => {
                        ui.painter().line_segment([rect.left_top() + egui::vec2(depth as f32 * 12.0, 0.0), rect.right_top()], Stroke::new(3.0_f32, game::DROP));
                    }
                    Some(Zone::After) => {
                        ui.painter().line_segment([rect.left_bottom() + egui::vec2(depth as f32 * 12.0, 0.0), rect.right_bottom()], Stroke::new(3.0_f32, game::DROP));
                    }
                    _ => {}
                }
                let mut x = rect.left() + 4.0 + depth as f32 * 12.0;
                let text_color = if active && !highlight { Color32::WHITE } else if page.visible { game::TEXT } else { game::HIDDEN };
                if has_children {
                    let open = s.panes[index].expanded.contains(&id);
                    ui.painter().text(egui::pos2(x + 4.0, rect.center().y), egui::Align2::CENTER_CENTER, if open { "▾" } else { "▸" }, egui::FontId::proportional(12.0), text_color);
                }
                x += 12.0;
                if let Some(texture) = images.page(ctx, page.icon_image) {
                    sharp_image(ui.painter(), &texture, egui::pos2(x + 9.0, rect.center().y), egui::vec2(22.0, ROW), 1);
                }
                x += 24.0;
                let caption = if page.enabled { page.caption.clone() } else { format!("{} ({})", page.caption, t("off")) };
                let galley = ui.painter().layout(caption, egui::FontId::proportional(12.0), text_color, (rect.right() - x - 4.0).max(10.0));
                ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, text_color);
                if let (Some(payload), false) = (released::<DragOffers>(&response), locked) {
                    let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                    actions.push(Action::Place { ids: payload.ids.clone(), page: id, before: None, copy });
                } else if let (Some(payload), false) = (released::<DragFurni>(&response), locked) {
                    actions.push(Action::PlaceFurni { ids: payload.0.clone(), page: id, before: None });
                } else if let (Some(dragged), Some(zone)) = (released::<DragPage>(&response), page_zone) {
                    let (parent, before) = match zone {
                        Zone::Into => (id, None),
                        Zone::Before => (page.parent_id, Some(id)),
                        Zone::After => {
                            let siblings: Vec<i64> = s.children(page.parent_id).iter().map(|p| p.id).filter(|p| *p != dragged.0).collect();
                            let next = siblings.iter().position(|p| *p == id).and_then(|i| siblings.get(i + 1)).copied();
                            (page.parent_id, next)
                        }
                    };
                    if zone == Zone::Into {
                        s.panes[index].expanded.insert(id);
                    }
                    actions.push(Action::MovePage { id: dragged.0, parent, before });
                } else if response.drag_started() && !s.sample {
                    egui::DragAndDrop::set_payload(ctx, DragPage(id));
                } else if response.clicked() {
                    let root = s.root_of(id);
                    let pane = &mut s.panes[index];
                    if has_children && (pane.page == Some(id) || !pane.expanded.contains(&id)) {
                        if !pane.expanded.insert(id) && pane.page == Some(id) {
                            pane.expanded.remove(&id);
                        }
                    }
                    pane.page = Some(id);
                    pane.selected.clear();
                    if !search.is_empty() {
                        pane.root = Some(root);
                    }
                }
            }
        });
    });
}

/// The open page: its title, the furni grid (drop targets) and the info box of the selected furni.
fn content(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize, actions: &mut Vec<Action>) {
    if s.panes[index].root == Some(UNOFFERED) {
        unoffered(ui, ctx, s, images, index, actions);
        return;
    }
    let Some(page_id) = s.panes[index].page else { return };
    let Some(page) = s.page(page_id).cloned() else { return };
    let offers = s.offers.get(&page_id).cloned();
    egui::Frame::new().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // The page bar: icon, caption, what it holds; right-click edits the page.
        let (bar, bar_response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
        bar_response.on_hover_text(t("Right-click: edit the page")).context_menu(|ui| page_menu(ui, s, page_id, true, actions));
        ui.painter().rect_filled(bar, CornerRadius::same(6), game::PAGE_BAR);
        if let Some(texture) = images.page(ctx, page.icon_image) {
            sharp_image(ui.painter(), &texture, bar.left_center() + egui::vec2(22.0, 0.0), egui::vec2(34.0, 34.0), 1);
        }
        ui.painter().text(bar.left_center() + egui::vec2(44.0, -7.0), egui::Align2::LEFT_CENTER, &page.caption, egui::FontId::proportional(15.0), game::TEXT);
        let mut facts = vec![page.layout.clone(), format!("id {}", page.id)];
        if !page.visible {
            facts.push(t("hidden").into());
        }
        if !page.enabled {
            facts.push(t("off").into());
        }
        if page.min_rank > 1 {
            facts.push(tf("rank {rank}+", &[("rank", &page.min_rank.to_string())]));
        }
        ui.painter().text(bar.left_center() + egui::vec2(44.0, 9.0), egui::Align2::LEFT_CENTER, facts.join(" · "), egui::FontId::proportional(11.0), game::MUTED);
        ui.add_space(8.0);

        if is_front_page(&page) {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
            ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0_f32, game::TILE_EDGE), egui::StrokeKind::Inside);
            let text = t("The front page shows the promotions of HOTEL → Catalogue (Front page tab), not furni: it is locked here. Open a page in the tree to drag furni.");
            let galley = ui.painter().layout(text.to_string(), egui::FontId::proportional(12.5), game::MUTED, rect.width() - 24.0);
            ui.painter().galley(egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0), galley, game::MUTED);
            return;
        }

        let Some(offers) = offers else {
            ui.label(RichText::new(t("Loading ...")).color(game::MUTED));
            return;
        };
        let info_height = 96.0;
        let grid_height = (ui.available_height() - info_height - 8.0).max(120.0);
        let width = ui.available_width();
        let (area, area_response) = ui.allocate_exact_size(egui::vec2(width, grid_height), Sense::hover());
        let mut grid = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let mut hovered_before: Option<(i64, egui::Rect)> = None;
        egui::ScrollArea::vertical().id_salt(("catalog-grid", index, page_id)).auto_shrink([false, false]).show(&mut grid, |ui| {
            let columns = ((width - 4.0) / (TILE + 6.0)).floor().max(1.0) as usize;
            if offers.is_empty() {
                ui.label(RichText::new(t("No furni on this page: drop some here.")).color(game::MUTED));
            }
            for row in offers.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for offer in row {
                        let (rect, response) = ui.allocate_exact_size(egui::vec2(TILE, TILE), Sense::click_and_drag());
                        let selected = s.panes[index].selected.contains(&offer.id);
                        let painter = ui.painter();
                        painter.rect_filled(rect, CornerRadius::same(6), if selected { game::TILE_SELECTED } else { game::TILE });
                        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(if selected { 2.0_f32 } else { 1.0 }, if selected { game::TITLE } else { game::TILE_EDGE }), egui::StrokeKind::Inside);
                        let furni = first_furni(offer).and_then(|id| s.furni.get(&id)).cloned();
                        draw_furni(painter, ctx, images, furni.as_ref(), offer, rect.shrink(4.0), 1);
                        if offer.limited_stack > 0 {
                            painter.text(rect.right_top() + egui::vec2(-4.0, 3.0), egui::Align2::RIGHT_TOP, "LTD", egui::FontId::proportional(9.0), game::DROP);
                        }
                        if catalog_admin::item_ids(&offer.item_ids).is_some_and(|ids| ids.len() > 1) {
                            painter.text(rect.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "+", egui::FontId::proportional(11.0), game::TITLE);
                        }
                        let response = response.on_hover_text(furni.as_ref().map(|f| f.public_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| offer.catalog_name.clone()));
                        let bundle: Vec<i64> = catalog_admin::item_ids(&offer.item_ids).unwrap_or_default();
                        response.context_menu(|ui| {
                            if ui.button(t("Edit offer (catalog_items) ...")).clicked() {
                                actions.push(Action::EditRow { table: "catalog_items", id: offer.id, furni: bundle.first().copied(), page: Some(page_id) });
                                ui.close_menu();
                            }
                            ui.separator();
                            for id in &bundle {
                                let name = s.furni.get(id).map(|f| if f.public_name.is_empty() { f.item_name.clone() } else { f.public_name.clone() }).unwrap_or_else(|| id.to_string());
                                let label = if bundle.len() > 1 { tf("Edit furni: {name}", &[("name", &name)]) } else { t("Edit furni (items_base) ...").to_string() };
                                if ui.button(label).clicked() {
                                    actions.push(Action::EditRow { table: "items_base", id: *id, furni: Some(*id), page: None });
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            // The selection when this furni is in it, else this one.
                            let selected = &s.panes[index].selected;
                            let ids: Vec<i64> = if selected.contains(&offer.id) { offers.iter().map(|o| o.id).filter(|id| selected.contains(id)).collect() } else { vec![offer.id] };
                            let label = if ids.len() > 1 { tf("Delete {count} offers ...", &[("count", &ids.len().to_string())]) } else { t("Delete offer ...").to_string() };
                            if ui.button(label).clicked() {
                                actions.push(Action::DeleteOffers { ids, page: page_id });
                                ui.close_menu();
                            }
                        });
                        if response.dnd_hover_payload::<DragOffers>().is_some() || response.dnd_hover_payload::<DragFurni>().is_some() {
                            hovered_before = Some((offer.id, rect));
                        }
                        if let Some(payload) = released::<DragFurni>(&response) {
                            actions.push(Action::PlaceFurni { ids: payload.0.clone(), page: page_id, before: Some(offer.id) });
                        }
                        if let Some(payload) = released::<DragOffers>(&response) {
                            let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                            // Dropped on itself (a click that moved a little): nothing changes.
                            if copy || !payload.ids.contains(&offer.id) {
                                actions.push(Action::Place { ids: payload.ids.clone(), page: page_id, before: Some(offer.id), copy });
                            }
                        } else if response.drag_started() {
                            let pane = &mut s.panes[index];
                            if !pane.selected.contains(&offer.id) {
                                pane.selected = vec![offer.id];
                                pane.anchor = Some(offer.id);
                            }
                            // In the page's order.
                            let ids: Vec<i64> = offers.iter().map(|o| o.id).filter(|id| pane.selected.contains(id)).collect();
                            egui::DragAndDrop::set_payload(ctx, DragOffers { ids, from: index });
                        } else if response.clicked() {
                            select(&mut s.panes[index], &offers, offer.id, ctx);
                        }
                    }
                });
                ui.add_space(6.0);
            }
        });
        // Where it lands: before the furni under the pointer, or at the end of the page.
        if let Some((_, rect)) = hovered_before {
            ui.painter().line_segment([rect.left_top() + egui::vec2(-3.5, 2.0), rect.left_bottom() + egui::vec2(-3.5, -2.0)], Stroke::new(3.0_f32, game::DROP));
        } else if area_response.dnd_hover_payload::<DragOffers>().is_some() || area_response.dnd_hover_payload::<DragFurni>().is_some() {
            ui.painter().rect_stroke(area, CornerRadius::same(6), Stroke::new(2.0_f32, game::DROP), egui::StrokeKind::Inside);
        }
        if hovered_before.is_none() {
            if let Some(payload) = released::<DragOffers>(&area_response) {
                let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                actions.push(Action::Place { ids: payload.ids.clone(), page: page_id, before: None, copy });
            }
            if let Some(payload) = released::<DragFurni>(&area_response) {
                actions.push(Action::PlaceFurni { ids: payload.0.clone(), page: page_id, before: None });
            }
        }
        ui.add_space(8.0);
        info_box(ui, ctx, s, images, index, &offers, info_height);
    });
}

/// The Without offer tab: the furni no offer sells, as tiles to drag onto a page.
fn unoffered(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize, actions: &mut Vec<Action>) {
    egui::Frame::new().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius::same(6), game::PAGE_BAR);
        ui.painter().text(bar.left_center() + egui::vec2(12.0, -7.0), egui::Align2::LEFT_CENTER, t("Without offer"), egui::FontId::proportional(15.0), game::TEXT);
        let Some(list) = s.unoffered.clone() else {
            ui.painter().text(bar.left_center() + egui::vec2(12.0, 9.0), egui::Align2::LEFT_CENTER, t("Reading the furni and the offers ..."), egui::FontId::proportional(11.0), game::MUTED);
            return;
        };
        let search = s.panes[index].furni_search.trim().to_lowercase();
        let shown: Vec<Furni> = list
            .iter()
            .filter(|f| search.is_empty() || f.item_name.to_lowercase().contains(&search) || f.public_name.to_lowercase().contains(&search) || f.id.to_string() == search)
            .take(UNOFFERED_SHOWN)
            .cloned()
            .collect();
        let facts = if shown.len() == UNOFFERED_SHOWN {
            tf("{count} furni without an offer; the first {shown} shown, search to narrow it.", &[("count", &list.len().to_string()), ("shown", &UNOFFERED_SHOWN.to_string())])
        } else {
            tf("{count} furni without an offer.", &[("count", &list.len().to_string())])
        };
        ui.painter().text(bar.left_center() + egui::vec2(12.0, 9.0), egui::Align2::LEFT_CENTER, facts, egui::FontId::proportional(11.0), game::MUTED);
        ui.add_space(8.0);
        let width = ui.available_width();
        egui::ScrollArea::vertical().id_salt(("catalog-unoffered", index)).auto_shrink([false, false]).show(ui, |ui| {
            let columns = ((width - 4.0) / (TILE + 6.0)).floor().max(1.0) as usize;
            for row in shown.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for furni in row {
                        let (rect, response) = ui.allocate_exact_size(egui::vec2(TILE, TILE), Sense::click_and_drag());
                        let selected = s.panes[index].furni_selected.contains(&furni.id);
                        let painter = ui.painter();
                        painter.rect_filled(rect, CornerRadius::same(6), if selected { game::TILE_SELECTED } else { game::TILE });
                        painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(if selected { 2.0_f32 } else { 1.0 }, if selected { game::TITLE } else { game::TILE_EDGE }), egui::StrokeKind::Inside);
                        let offer = Offer { catalog_name: furni.item_name.clone(), ..Offer::default() };
                        draw_furni(painter, ctx, images, Some(furni), &offer, rect.shrink(4.0), 1);
                        let name = if furni.public_name.is_empty() { furni.item_name.clone() } else { format!("{} ({})", furni.public_name, furni.item_name) };
                        let response = response.on_hover_text(name);
                        response.context_menu(|ui| {
                            if ui.button(t("Edit furni (items_base) ...")).clicked() {
                                actions.push(Action::EditRow { table: "items_base", id: furni.id, furni: Some(furni.id), page: None });
                                ui.close_menu();
                            }
                        });
                        let pane = &mut s.panes[index];
                        if response.drag_started() {
                            if !pane.furni_selected.contains(&furni.id) {
                                pane.furni_selected = vec![furni.id];
                            }
                            let ids: Vec<i64> = shown.iter().map(|f| f.id).filter(|id| pane.furni_selected.contains(id)).collect();
                            egui::DragAndDrop::set_payload(ctx, DragFurni(ids));
                        } else if response.clicked() {
                            let modifiers = ctx.input(|i| i.modifiers);
                            if modifiers.command || modifiers.ctrl || modifiers.shift {
                                if let Some(i) = pane.furni_selected.iter().position(|x| *x == furni.id) {
                                    pane.furni_selected.remove(i);
                                } else {
                                    pane.furni_selected.push(furni.id);
                                }
                            } else {
                                pane.furni_selected = vec![furni.id];
                            }
                        }
                    }
                });
                ui.add_space(6.0);
            }
        });
    });
}

/// The furni editor: every items_base column of one furni, the flags as ticks, the rest in the table's order.
fn editor_window(ctx: &egui::Context, palette: &Palette, s: &mut State, images: &mut Images, actions: &mut Vec<Action>) {
    let fetched = &s.fetched;
    let fetching = &s.fetching;
    let mut want: Vec<String> = Vec::new();
    let mut open_picker: Option<(usize, bool)> = None;
    let mut open_html: Option<usize> = None;
    let mut want_names: Vec<i64> = Vec::new();
    let furni_names = &s.furni;
    let furni_search = &mut s.furni_search;
    let furni_results = &s.furni_results;
    let Some(editor) = s.editor.as_mut() else { return };
    let columns = s.columns.get(editor.table).cloned().unwrap_or_default();
    let furni = editor.furni.and_then(|id| s.furni.get(&id).cloned().or_else(|| s.unoffered.iter().flatten().find(|f| f.id == id).cloned()));
    let offer_name = columns.iter().position(|c| c.name == "catalog_name").and_then(|i| editor.values.get(i).cloned());
    let mut close = false;
    let changed = editor.changes(&columns).len();
    egui::Modal::new(egui::Id::new("catalog-furni-editor")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(22, 18))).show(ctx, |ui| {
        ui.set_width(640.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(6), game::PAGE_BAR);
            let value = |name: &str| columns.iter().position(|c| c.name == name).and_then(|i| editor.values.get(i).cloned());
            if editor.table == "catalog_pages" {
                let icon = value("icon_image").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
                if let Some(texture) = images.page(ctx, icon) {
                    sharp_image(ui.painter(), &texture, rect.center(), rect.size() - egui::vec2(8.0, 8.0), 2);
                }
            } else {
                let offer = Offer { catalog_name: furni.as_ref().map(|f| f.item_name.clone()).unwrap_or_default(), ..Offer::default() };
                draw_furni(ui.painter(), ctx, images, furni.as_ref(), &offer, rect.shrink(4.0), 2);
            }
            ui.vertical(|ui| {
                let furni_name = furni.as_ref().map(|f| if f.public_name.is_empty() { f.item_name.clone() } else { f.public_name.clone() }).unwrap_or_default();
                if editor.table == "catalog_pages" {
                    let caption = value("caption").unwrap_or_default();
                    ui.label(RichText::new(caption).size(17.0).strong());
                    ui.label(RichText::new(format!("catalog_pages id {}", editor.id)).size(12.0).color(palette.muted));
                    ui.label(RichText::new(t("Every column of the page's catalog_pages row: caption, layout, icon, parent, order, rank, visible, texts and images ... Changed values are marked; Save writes only those.")).size(11.5).color(palette.muted));
                } else if editor.table == "catalog_items" {
                    ui.label(RichText::new(offer_name.clone().filter(|n| !n.is_empty()).unwrap_or(furni_name)).size(17.0).strong());
                    ui.label(RichText::new(format!("catalog_items id {}", editor.id)).size(12.0).color(palette.muted));
                    ui.label(RichText::new(t("Every column of the offer's catalog_items row: price, amount, page, order, limited series ... Changed values are marked; Save writes only those.")).size(11.5).color(palette.muted));
                } else {
                    ui.label(RichText::new(furni_name).size(17.0).strong());
                    ui.label(RichText::new(format!("{} · items_base id {}", furni.as_ref().map(|f| f.item_name.as_str()).unwrap_or(""), editor.id)).size(12.0).color(palette.muted));
                    ui.label(RichText::new(t("Every column of the furni's items_base row. Changed values are marked; Save writes only those.")).size(11.5).color(palette.muted));
                }
            });
        });
        ui.add_space(10.0);
        let Some(original) = editor.original.clone() else {
            if editor.status.text.is_empty() {
                ui.label(RichText::new(t("Reading the furni ...")).color(palette.muted));
            } else {
                ui.label(RichText::new(&editor.status.text).color(palette.error_text));
            }
            ui.add_space(8.0);
            if ui.add(widgets::secondary_button(t("Close"))).clicked() {
                close = true;
            }
            return;
        };
        let mark = |ui: &mut egui::Ui, i: usize, editor: &Editor| {
            let now = if editor.nulls[i] { None } else { Some(editor.values[i].trim().to_string()) };
            if now != original[i] {
                ui.label(RichText::new("●").size(10.0).color(palette.warning_text));
            }
        };
        egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.6).auto_shrink([false, true]).show(ui, |ui| {
            // The flags, as ticks in three columns.
            let flags: Vec<usize> = columns.iter().enumerate().filter(|(_, c)| c.kind() == FieldKind::Flag).map(|(i, _)| i).collect();
            if !flags.is_empty() {
                ui.label(RichText::new(t("Flags")).size(13.0).strong());
                egui::Grid::new("furni-flags").num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
                    for (n, i) in flags.iter().enumerate() {
                        let column = &columns[*i];
                        ui.horizontal(|ui| {
                            let mut on = editor.values[*i].trim() == "1";
                            if ui.checkbox(&mut on, &column.name).on_hover_text(hover(column)).changed() {
                                editor.values[*i] = if on { "1" } else { "0" }.into();
                                editor.nulls[*i] = false;
                            }
                            mark(ui, *i, editor);
                        });
                        if n % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(10.0);
            }
            ui.label(RichText::new(t("Values")).size(13.0).strong());
            egui::Grid::new("furni-values").num_columns(3).spacing([12.0, 6.0]).min_col_width(80.0).show(ui, |ui| {
                for (i, column) in columns.iter().enumerate() {
                    let kind = column.kind();
                    if kind == FieldKind::Flag {
                        continue;
                    }
                    ui.label(RichText::new(&column.name).size(12.5)).on_hover_text(hover(column));
                    ui.horizontal(|ui| {
                        let editable = column.name != "id";
                        ui.add_enabled_ui(editable && !editor.nulls[i], |ui| match &kind {
                            FieldKind::Choice(options) => {
                                egui::ComboBox::from_id_salt(("furni-choice", i)).selected_text(editor.values[i].clone()).width(220.0).show_ui(ui, |ui| {
                                    for option in options {
                                        ui.selectable_value(&mut editor.values[i], option.clone(), option);
                                    }
                                });
                            }
                            // A text column (page_text1 ...) can hold several lines.
                            FieldKind::Text { max: None } => {
                                ui.add(egui::TextEdit::multiline(&mut editor.values[i]).desired_rows(2).desired_width(300.0));
                            }
                            _ => {
                                let width = if matches!(kind, FieldKind::Text { max: Some(65..) }) { 300.0 } else { 140.0 };
                                ui.add(egui::TextEdit::singleline(&mut editor.values[i]).desired_width(width));
                            }
                        });
                        if editor.table == "catalog_pages" && catalog_admin::PAGE_TEXT_COLUMNS.contains(&column.name.as_str()) {
                            if ui.add_enabled(!editor.nulls[i], widgets::secondary_button(t("HTML ..."))).on_hover_text(t("A small HTML editor with a preview; only the HTML Octane and the gameserver both show is kept.")).clicked() {
                                open_html = Some(i);
                            }
                        }
                        if let (true, Some(icon)) = (editor.table == "catalog_pages", image_column(&column.name)) {
                            // The image the value means, small, and the picker.
                            let files = value_files(&editor.values[i], icon);
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(if icon { 26.0 } else { 90.0 }, 22.0), Sense::hover());
                            ui.painter().rect_filled(rect, CornerRadius::same(3), game::PAGE_BAR);
                            let mut texture = None;
                            for file in &files {
                                let (found, fetch) = images.catalogue_file(ctx, file, fetched);
                                if fetch && !fetching.contains(file) {
                                    want.push(file.clone());
                                }
                                if found.is_some() {
                                    texture = found;
                                    break;
                                }
                            }
                            if let Some(texture) = texture {
                                sharp_image(ui.painter(), &texture, rect.center(), rect.size(), 1);
                            }
                            if ui.add_enabled(!editor.nulls[i], widgets::secondary_button(t("Choose ..."))).clicked() {
                                open_picker = Some((i, icon));
                            }
                        }
                        if column.nullable && column.name != "id" {
                            ui.checkbox(&mut editor.nulls[i], "NULL");
                        }
                        mark(ui, i, editor);
                    });
                    let problem = (!editor.nulls[i]).then(|| catalog_admin::field_problem(column, &editor.values[i])).flatten();
                    match problem {
                        Some(problem) => ui.label(RichText::new(problem).size(11.0).color(palette.error_text)),
                        // A long enum type would make the window wide: cut, the whole type on hover.
                        None => {
                            let shown = if column.column_type.chars().count() > 28 { format!("{}…", column.column_type.chars().take(26).collect::<String>()) } else { column.column_type.clone() };
                            ui.label(RichText::new(shown).size(11.0).color(palette.muted)).on_hover_text(hover(column))
                        }
                    };
                    ui.end_row();
                }
            });
            // An offer's furni by name, to take one out of a bundle or add one found by name.
            if let (true, Some(col)) = (editor.table == "catalog_items", columns.iter().position(|c| c.name == "item_ids")) {
                ui.add_space(10.0);
                ui.label(RichText::new(t("Furni of this offer")).size(13.0).strong());
                let ids = catalog_admin::item_ids(&editor.values[col]).unwrap_or_default();
                for id in &ids {
                    ui.horizontal(|ui| {
                        let name = match furni_names.get(id) {
                            Some(f) => format!("{} ({})", if f.public_name.is_empty() { &f.item_name } else { &f.public_name }, f.item_name),
                            None => {
                                want_names.push(*id);
                                id.to_string()
                            }
                        };
                        ui.label(RichText::new(format!("{id}  ·  {name}")).size(12.5));
                        if ids.len() > 1 && ui.add(widgets::secondary_button(t("Remove"))).clicked() {
                            editor.values[col] = catalog_admin::remove_furni_id(&editor.values[col], *id);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    let response = ui.add(egui::TextEdit::singleline(furni_search).hint_text(RichText::new(t("Find furni by name, class name or id")).italics()).desired_width(260.0));
                    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add(widgets::secondary_button(t("Search"))).clicked() || enter {
                        actions.push(Action::SearchFurni(furni_search.trim().to_string()));
                    }
                });
                for furni in furni_results.iter().take(20) {
                    ui.horizontal(|ui| {
                        let name = if furni.public_name.is_empty() { furni.item_name.clone() } else { format!("{} ({})", furni.public_name, furni.item_name) };
                        ui.label(RichText::new(format!("{}  ·  {name}", furni.id)).size(12.0).color(palette.muted));
                        if !ids.contains(&furni.id) && ui.add(widgets::secondary_button(t("Add"))).clicked() {
                            editor.values[col] = catalog_admin::add_furni_id(&editor.values[col], furni.id);
                            editor.nulls[col] = false;
                        }
                    });
                }
            }
        });
        ui.add_space(10.0);
        if !editor.status.text.is_empty() {
            let color = if editor.status.error { palette.error_text } else { palette.muted };
            ui.add(egui::Label::new(RichText::new(&editor.status.text).size(12.0).color(color)).wrap());
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let label = if changed > 0 { tf("Save changes ({count})", &[("count", &changed.to_string())]) } else { t("Save").to_string() };
            if ui.add_enabled(changed > 0 && !editor.saving && !s.sample, widgets::primary_button(&label)).clicked() {
                actions.push(Action::SaveRow);
            }
            if ui.add_enabled(changed > 0 && !editor.saving, widgets::secondary_button(t("Undo changes"))).clicked() {
                editor.values = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
                editor.nulls = original.iter().map(Option::is_none).collect();
                editor.status = Status::default();
            }
            if ui.add(widgets::secondary_button(t("Close"))).clicked() {
                close = true;
            }
        });
    });
    if !want.is_empty() && !s.sample {
        s.fetching.extend(want.iter().cloned());
        actions.push(Action::FetchFiles(want));
    }
    want_names.retain(|id| !s.furni_asked.contains(id));
    want_names.dedup();
    if !want_names.is_empty() && !s.sample {
        s.furni_asked.extend(want_names.iter().copied());
        actions.push(Action::FurniNames(want_names));
    }
    if let Some(column) = open_html {
        let text = s.editor.as_ref().and_then(|e| e.values.get(column).cloned()).unwrap_or_default();
        s.html = Some(HtmlEditor { column, text, selection: (0, 0) });
    }
    if let Some((column, icon)) = open_picker {
        s.picker = Some(Picker { column, icon, search: String::new() });
        if s.catalogue_files.is_none() && !s.listing && !s.sample {
            s.listing = true;
            actions.push(Action::ListCatalogueFiles);
        }
    }
    if close {
        s.editor = None;
        s.picker = None;
        s.html = None;
    }
}

/// The image picker over the editor: the page icons (icon_image) or the catalogue images (page_headline ...) of this
/// computer and the server, with a search; a click writes the choice in the field.
fn picker_window(ctx: &egui::Context, palette: &Palette, s: &mut State, images: &mut Images, actions: &mut Vec<Action>) {
    let Some(picker) = s.picker.as_mut() else { return };
    let Some(editor) = s.editor.as_mut() else {
        s.picker = None;
        return;
    };
    let icon = picker.icon;
    // Every file of this computer and the server, once; icons for the icon, the other images for the rest.
    let mut files = images.local_catalogue();
    files.extend(s.catalogue_files.iter().flatten().cloned());
    files.sort();
    files.dedup();
    let search = picker.search.trim().to_lowercase();
    let mut shown: Vec<String> = files.into_iter().filter(|f| icon_number(f).is_some() == icon).filter(|f| search.is_empty() || f.to_lowercase().contains(&search)).collect();
    if icon {
        shown.sort_by_key(|f| icon_number(f).unwrap_or(0));
    }
    let current = editor.values.get(picker.column).cloned().unwrap_or_default();
    let mut chosen: Option<String> = None;
    let mut close = false;
    let mut want: Vec<String> = Vec::new();
    egui::Modal::new(egui::Id::new("catalog-image-picker")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(20, 16))).show(ctx, |ui| {
        ui.set_width(if icon { 560.0 } else { 720.0 });
        ui.label(RichText::new(if icon { t("Choose the page icon") } else { t("Choose the image") }).size(16.0).strong());
        ui.label(
            RichText::new(if icon {
                t("The page icons (c_images/catalogue/icon_<n>.png) of this computer and the server: a click writes the number in icon_image.")
            } else {
                t("The images in c_images/catalogue of this computer and the server: a click writes the name (without .gif, the client adds it).")
            })
            .size(11.5)
            .color(palette.muted),
        );
        ui.add_space(6.0);
        ui.add(egui::TextEdit::singleline(&mut picker.search).hint_text(RichText::new(t("Search")).italics()).desired_width(260.0));
        ui.add_space(6.0);
        if s.listing {
            ui.label(RichText::new(t("Reading the server's catalogue images ...")).size(12.0).color(palette.muted));
        }
        let (cell, label_height) = if icon { (egui::vec2(60.0, 44.0), 14.0) } else { (egui::vec2(228.0, 84.0), 16.0) };
        egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.55).auto_shrink([false, true]).show(ui, |ui| {
            if shown.is_empty() && !s.listing {
                ui.label(RichText::new(t("No images found.")).color(palette.muted));
            }
            let columns = ((ui.available_width() + 6.0) / (cell.x + 6.0)).floor().max(1.0) as usize;
            for row in shown.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for file in row {
                        let (rect, response) = ui.allocate_exact_size(cell + egui::vec2(0.0, label_height), Sense::click());
                        let selected = picked_value(file, icon) == current.trim();
                        let image_rect = egui::Rect::from_min_size(rect.min, cell);
                        ui.painter().rect_filled(image_rect, CornerRadius::same(5), if selected { game::TILE_SELECTED } else { game::PAGE_BAR });
                        ui.painter().rect_stroke(
                            image_rect,
                            CornerRadius::same(5),
                            Stroke::new(if selected { 2.0_f32 } else if response.hovered() { 1.5 } else { 1.0 }, if selected || response.hovered() { game::TITLE } else { game::TILE_EDGE }),
                            egui::StrokeKind::Inside,
                        );
                        // Previews only of what is on screen: the rest is fetched when scrolled to.
                        if ui.is_rect_visible(rect) {
                            let (texture, fetch) = images.catalogue_file(ctx, file, &s.fetched);
                            if fetch && !s.fetching.contains(file) && want.len() < 24 {
                                want.push(file.clone());
                            }
                            match texture {
                                Some(texture) => sharp_image(ui.painter(), &texture, image_rect.center(), image_rect.size() - egui::vec2(6.0, 6.0), 1),
                                None => {
                                    ui.painter().text(image_rect.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(14.0), game::MUTED);
                                }
                            }
                        }
                        let label = if icon { icon_number(file).map(|n| n.to_string()).unwrap_or_default() } else { picked_value(file, false) };
                        let galley = ui.painter().layout(label, egui::FontId::proportional(10.5), palette.muted, rect.width());
                        ui.painter().galley(egui::pos2(rect.center().x - galley.size().x / 2.0, image_rect.bottom() + 1.0), galley, palette.muted);
                        if response.on_hover_text(file).clicked() {
                            chosen = Some(picked_value(file, icon));
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                close = true;
            }
            ui.label(RichText::new(tf("{count} shown", &[("count", &shown.len().to_string())])).size(11.5).color(palette.muted));
        });
    });
    if let Some(value) = chosen {
        if let Some(slot) = editor.values.get_mut(picker.column) {
            *slot = value;
        }
        if let Some(null) = editor.nulls.get_mut(picker.column) {
            *null = false;
        }
        close = true;
    }
    if !want.is_empty() && !s.sample {
        s.fetching.extend(want.iter().cloned());
        actions.push(Action::FetchFiles(want));
    }
    if close {
        s.picker = None;
    }
}

/// The HTML editor of a page text: a toolbar that wraps the selection, the text, and a preview of what the client
/// shows. Use writes the safe HTML (catalog_admin::safe_page_html) in the field.
fn html_window(ctx: &egui::Context, palette: &Palette, s: &mut State) {
    let Some(html) = s.html.as_mut() else { return };
    let Some(editor) = s.editor.as_mut() else {
        s.html = None;
        return;
    };
    let name = s.columns.get(editor.table).and_then(|c| c.get(html.column)).map(|c| c.name.clone()).unwrap_or_default();
    let mut close = false;
    let mut apply = false;
    egui::Modal::new(egui::Id::new("catalog-html-editor")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(20, 16))).show(ctx, |ui| {
        ui.set_width(760.0);
        ui.label(RichText::new(tf("HTML of {column}", &[("column", &name)])).size(16.0).strong());
        ui.label(
            RichText::new(t("Only what Octane and the gameserver both show is kept: b, i, u, strong, em, br, p, div, span, a (http, https, mailto or #, opens in a new tab; or event: for a hotel link) and img (http or https); styles only color, background-color, font-weight, font-style, font-size, text-decoration and text-align."))
                .size(11.5)
                .color(palette.muted),
        );
        ui.add_space(8.0);
        // The toolbar: wraps the selection in a tag, or puts one in at the cursor.
        let mut wrap: Option<(String, String)> = None;
        ui.horizontal_wrapped(|ui| {
            let tool = |ui: &mut egui::Ui, label: RichText, tip: &str| ui.add(egui::Button::new(label).min_size(egui::vec2(30.0, 26.0))).on_hover_text(tip).clicked();
            if tool(ui, RichText::new("B").strong(), t("Bold")) {
                wrap = Some(("<b>".into(), "</b>".into()));
            }
            if tool(ui, RichText::new("I").italics(), t("Italic")) {
                wrap = Some(("<i>".into(), "</i>".into()));
            }
            if tool(ui, RichText::new("U").underline(), t("Underline")) {
                wrap = Some(("<u>".into(), "</u>".into()));
            }
            if tool(ui, RichText::new("↵"), t("Line break")) {
                wrap = Some(("<br>".into(), String::new()));
            }
            if tool(ui, RichText::new("¶"), t("Paragraph")) {
                wrap = Some(("<p>".into(), "</p>".into()));
            }
            if tool(ui, RichText::new("🔗"), t("Link (opens in a new tab)")) {
                wrap = Some(("<a href=\"https://\" target=\"_blank\">".into(), "</a>".into()));
            }
            if tool(ui, RichText::new("🏨"), t("Hotel link (event:, opens in the client: catalog/open/<page>, habbopages/<page> ...)")) {
                wrap = Some(("<a href=\"event:catalog/open/\">".into(), "</a>".into()));
            }
            if tool(ui, RichText::new("🖼"), t("Image (http or https)")) {
                wrap = Some(("<img src=\"https://\" alt=\"\">".into(), String::new()));
            }
            ui.add_space(8.0);
            for (color, hex) in [(Color32::from_rgb(214, 39, 40), "#d62728"), (Color32::from_rgb(31, 119, 180), "#1f77b4"), (Color32::from_rgb(44, 160, 44), "#2ca02c"), (Color32::from_rgb(255, 127, 14), "#ff7f0e"), (Color32::BLACK, "#000000")] {
                let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
                ui.painter().rect_filled(rect.shrink(2.0), CornerRadius::same(4), color);
                ui.painter().rect_stroke(rect.shrink(2.0), CornerRadius::same(4), Stroke::new(1.0_f32, palette.muted), egui::StrokeKind::Inside);
                if response.on_hover_text(tf("Text colour {color}", &[("color", hex)])).clicked() {
                    wrap = Some((format!("<span style=\"color: {hex}\">"), "</span>".into()));
                }
            }
        });
        if let Some((open, end)) = wrap {
            let chars: Vec<char> = html.text.chars().collect();
            let (a, b) = (html.selection.0.min(html.selection.1).min(chars.len()), html.selection.0.max(html.selection.1).min(chars.len()));
            let before: String = chars[..a].iter().collect();
            let inside: String = chars[a..b].iter().collect();
            let after: String = chars[b..].iter().collect();
            html.text = format!("{before}{open}{inside}{end}{after}");
            let at = a + open.chars().count() + inside.chars().count();
            html.selection = (at, at);
        }
        ui.add_space(6.0);
        let output = egui::TextEdit::multiline(&mut html.text).code_editor().desired_rows(8).desired_width(f32::INFINITY).show(ui);
        if let Some(range) = output.cursor_range {
            html.selection = (range.primary.ccursor.index, range.secondary.ccursor.index);
        }
        ui.add_space(8.0);
        let safe = catalog_admin::safe_page_html(&html.text);
        ui.label(RichText::new(t("Preview")).size(13.0).strong());
        egui::Frame::new().fill(game::NAV).stroke(Stroke::new(1.0_f32, game::TILE_EDGE)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt("catalog-html-preview").max_height(160.0).show(ui, |ui| {
                ui.label(html_preview(&safe));
            });
        });
        if safe.contains("href=\"event:") {
            ui.add_space(4.0);
            ui.label(
                RichText::new(t("Hotel links (event:) open in the client on the layouts single_bundle, info_loyalty, the info pages, guild_frontpage, info_illustration, trophies and builders_club_buy; on the others only their text shows."))
                    .size(11.5)
                    .color(palette.muted),
            );
        }
        if safe != html.text {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("Part of this HTML is not shown by the client (or not allowed): it is taken out when used.")).size(12.0).color(palette.warning_text));
                if ui.add(widgets::secondary_button(t("Clean up"))).on_hover_text(&safe).clicked() {
                    html.text = safe.clone();
                }
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(widgets::primary_button(t("Use"))).clicked() {
                apply = true;
            }
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                close = true;
            }
        });
    });
    if apply {
        let safe = catalog_admin::safe_page_html(&html.text);
        if let Some(value) = editor.values.get_mut(html.column) {
            *value = safe;
        }
        close = true;
    }
    if close {
        s.html = None;
    }
}

/// Safe page HTML as the client shows it, roughly: bold, italic, underline, colour, links, line breaks and paragraphs.
fn html_preview(html: &str) -> egui::text::LayoutJob {
    #[derive(Clone, Default)]
    struct Look {
        bold: bool,
        italic: bool,
        underline: bool,
        link: bool,
        color: Option<Color32>,
    }
    fn color(value: &str) -> Option<Color32> {
        let value = value.trim().to_lowercase();
        let hex = value.strip_prefix('#')?;
        let full = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect::<String>() } else { hex.to_string() };
        if full.len() != 6 {
            return None;
        }
        let n = u32::from_str_radix(&full, 16).ok()?;
        Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }
    fn decode(text: &str) -> String {
        text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&")
    }
    let mut job = egui::text::LayoutJob::default();
    let mut stack: Vec<(String, Look)> = Vec::new();
    let mut look = Look::default();
    let add = |job: &mut egui::text::LayoutJob, text: &str, look: &Look| {
        if text.is_empty() {
            return;
        }
        // Real bold: the bold UI font (Ubuntu Bold) the app installs as the "bold" family.
        let family = if look.bold { egui::FontFamily::Name("bold".into()) } else { egui::FontFamily::Proportional };
        let mut format = egui::TextFormat {
            font_id: egui::FontId::new(13.0, family),
            color: if look.link { game::TITLE } else { look.color.unwrap_or(game::TEXT) },
            italics: look.italic,
            ..Default::default()
        };
        if look.underline || look.link {
            format.underline = Stroke::new(1.0_f32, format.color);
        }
        job.append(&decode(text), 0.0, format);
    };
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let end = rest.find('>').unwrap_or(rest.len() - 1);
                let tag = &rest[1..end];
                rest = &rest[(end + 1).min(rest.len())..];
                let closing = tag.starts_with('/');
                let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
                if closing {
                    if let Some(at) = stack.iter().rposition(|(n, _)| *n == name) {
                        look = stack[at].1.clone();
                        stack.truncate(at);
                    }
                    if matches!(name.as_str(), "p" | "div") {
                        add(&mut job, "\n", &look);
                    }
                    continue;
                }
                match name.as_str() {
                    "br" => add(&mut job, "\n", &look),
                    "img" => add(&mut job, "[image]", &Look { italic: true, ..look.clone() }),
                    _ => {
                        stack.push((name.clone(), look.clone()));
                        match name.as_str() {
                            "b" | "strong" => look.bold = true,
                            "i" | "em" => look.italic = true,
                            "u" => look.underline = true,
                            "a" => look.link = true,
                            _ => {}
                        }
                        if let Some(style) = tag.split("style=\"").nth(1).and_then(|s| s.split('"').next()) {
                            for declaration in style.split(';') {
                                if let Some((property, value)) = declaration.split_once(':') {
                                    match property.trim() {
                                        "color" => look.color = color(value).or(look.color),
                                        "font-weight" if value.trim().starts_with("bold") || value.trim().parse::<u32>().is_ok_and(|w| w >= 600) => look.bold = true,
                                        "font-style" if value.trim() == "italic" => look.italic = true,
                                        "text-decoration" if value.contains("underline") => look.underline = true,
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(at) => {
                add(&mut job, &rest[..at], &look);
                rest = &rest[at..];
            }
            None => {
                add(&mut job, rest, &look);
                rest = "";
            }
        }
    }
    job
}

/// A column's type and comment, for its hover text.
fn hover(column: &FurniColumn) -> String {
    if column.comment.trim().is_empty() {
        column.column_type.clone()
    } else {
        format!("{}\n{}", column.column_type, column.comment)
    }
}

/// The right-click menu of a page (in a tree, on its tab, on its page bar).
fn page_menu(ui: &mut egui::Ui, s: &State, id: i64, on_bar: bool, actions: &mut Vec<Action>) {
    let mut pick = |ui: &mut egui::Ui, label: &str, action: Action| {
        if ui.button(label).clicked() {
            actions.push(action);
            ui.close_menu();
        }
    };
    pick(ui, t("Edit page (catalog_pages) ..."), Action::EditRow { table: "catalog_pages", id, furni: None, page: None });
    ui.separator();
    pick(ui, t("New page here"), Action::NewPage(id));
    pick(ui, t("Build page from a furni line ..."), Action::OpenBuilder(id));
    if on_bar && s.offers.get(&id).is_some_and(|o| o.len() > 1) {
        pick(ui, t("Sort the furni by name"), Action::SortByName(id));
    }
    ui.separator();
    pick(ui, t("Delete page ..."), Action::DeletePage(id));
}

/// The page builder: a new page under the chosen one with an offer for every floor and wall furni of a furni line
/// (class names that start with the prefix), at one price.
fn builder_window(ctx: &egui::Context, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let parent = s.builder.as_ref().map(|b| b.parent).and_then(|p| s.page(p)).map(|p| p.caption.clone()).unwrap_or_default();
    let Some(builder) = s.builder.as_mut() else { return };
    let mut close = false;
    egui::Modal::new(egui::Id::new("catalog-page-builder")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(22, 18))).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new(t("Build page")).size(17.0).strong());
        ui.label(RichText::new(tf("A new page under {page} with an offer for every floor and wall furni whose class name starts with the prefix (a furni line, like rare_dragonlamp).", &[("page", &parent)])).size(12.0).color(palette.muted));
        ui.add_space(10.0);
        egui::Grid::new("catalog-builder").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(t("Name"));
            ui.add(egui::TextEdit::singleline(&mut builder.caption).desired_width(260.0));
            ui.end_row();
            ui.label(t("Class name starts with"));
            ui.horizontal(|ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut builder.prefix).hint_text("rare_dragonlamp").desired_width(200.0));
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ready = builder.prefix.trim().chars().count() >= 3 && !busy;
                if (ui.add_enabled(ready, widgets::secondary_button(t("Find furni"))).clicked() || enter) && ready {
                    actions.push(Action::BuilderFind);
                }
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut builder.only_new, t("Only furni no offer sells yet"));
            ui.end_row();
            ui.label(t("Credits"));
            ui.add(egui::DragValue::new(&mut builder.price.0).range(0..=1_000_000_000));
            ui.end_row();
            ui.label(t("Points"));
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut builder.price.1).range(0..=1_000_000_000));
                ui.label(RichText::new(t("type")).size(12.0).color(palette.muted));
                ui.add(egui::DragValue::new(&mut builder.price.2).range(0..=1000));
            });
            ui.end_row();
        });
        ui.add_space(8.0);
        match &builder.found {
            None => {
                ui.label(RichText::new(t("Find furni first: the page gets an offer for each one found.")).size(12.0).color(palette.muted));
            }
            Some(found) => {
                ui.label(RichText::new(tf("{count} furni found.", &[("count", &found.len().to_string())])).size(12.5).strong());
                egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                    for furni in found {
                        ui.label(RichText::new(format!("{}  ·  {}", furni.item_name, furni.public_name)).size(12.0).color(palette.muted));
                    }
                });
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let ready = !busy && !builder.caption.trim().is_empty() && builder.found.as_ref().is_some_and(|f| !f.is_empty());
            if ui.add_enabled(ready, widgets::primary_button(t("Build page"))).clicked() {
                actions.push(Action::BuilderBuild);
            }
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                close = true;
            }
        });
    });
    if close {
        s.builder = None;
    }
}

/// Click selects one; Ctrl toggles; Shift selects from the last one clicked.
fn select(pane: &mut Pane, offers: &[Offer], id: i64, ctx: &egui::Context) {
    let modifiers = ctx.input(|i| i.modifiers);
    if modifiers.shift {
        let at = |x: i64| offers.iter().position(|o| o.id == x);
        if let (Some(from), Some(to)) = (pane.anchor.and_then(at), at(id)) {
            let (a, b) = if from <= to { (from, to) } else { (to, from) };
            pane.selected = offers[a..=b].iter().map(|o| o.id).collect();
            return;
        }
    }
    if modifiers.command || modifiers.ctrl {
        if let Some(i) = pane.selected.iter().position(|x| *x == id) {
            pane.selected.remove(i);
        } else {
            pane.selected.push(id);
        }
    } else {
        pane.selected = vec![id];
    }
    pane.anchor = Some(id);
}

/// The furni's icon centred in `rect` (at its own size, or smaller to fit), or its initials while there is none.
fn draw_furni(painter: &egui::Painter, ctx: &egui::Context, images: &mut Images, furni: Option<&Furni>, offer: &Offer, rect: egui::Rect, zoom: u32) {
    let name = furni.map(|f| f.item_name.as_str()).unwrap_or(offer.catalog_name.as_str());
    if let Some(texture) = images.furni(ctx, name) {
        sharp_image(painter, &texture, rect.center(), rect.size(), zoom);
    } else {
        let initials: String = name.split(['_', '*', ' ']).filter(|p| !p.is_empty()).take(2).filter_map(|p| p.chars().next()).collect::<String>().to_uppercase();
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, initials, egui::FontId::proportional(15.0), game::MUTED);
    }
}

/// The selected furni as the game's catalogue shows it: a large icon, the name and the price.
fn info_box(ui: &mut egui::Ui, ctx: &egui::Context, s: &State, images: &mut Images, index: usize, offers: &[Offer], height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0_f32, game::TILE_EDGE), egui::StrokeKind::Inside);
    let selected = &s.panes[index].selected;
    let Some(offer) = selected.last().and_then(|id| offers.iter().find(|o| o.id == *id)) else {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, t("Select a furni to see it here."), egui::FontId::proportional(12.0), game::MUTED);
        return;
    };
    let furni = first_furni(offer).and_then(|id| s.furni.get(&id));
    let preview = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), egui::vec2(height - 16.0, height - 16.0));
    painter.rect_filled(preview, CornerRadius::same(6), game::PAGE_BAR);
    draw_furni(painter, ctx, images, furni, offer, preview.shrink(6.0), 2);
    let x = preview.right() + 12.0;
    let name = furni.map(|f| f.public_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| offer.catalog_name.clone());
    painter.text(egui::pos2(x, rect.top() + 12.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(15.0), game::TEXT);
    let mut facts = vec![offer.catalog_name.clone(), format!("{} {}", t("offer"), offer.id)];
    if let Some(furni) = furni {
        facts.push(format!("{} ({})", furni.item_name, furni.id));
    }
    if selected.len() > 1 {
        facts.push(tf("{count} selected", &[("count", &selected.len().to_string())]));
    }
    painter.text(egui::pos2(x, rect.top() + 34.0), egui::Align2::LEFT_TOP, facts.join(" · "), egui::FontId::proportional(11.0), game::MUTED);
    // The price: credits, then the points in their currency.
    let mut px = x;
    let y = rect.bottom() - 22.0;
    let mut coin = |amount: i64, color: Color32, label: &str| {
        if amount <= 0 {
            return;
        }
        painter.circle_filled(egui::pos2(px + 7.0, y), 7.0, color);
        painter.circle_stroke(egui::pos2(px + 7.0, y), 7.0, Stroke::new(1.0_f32, Color32::from_black_alpha(90)));
        let text = format!("{amount} {label}");
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), game::TEXT);
        let w = galley.size().x;
        painter.galley(egui::pos2(px + 18.0, y - galley.size().y / 2.0), galley, game::TEXT);
        px += 30.0 + w;
    };
    coin(offer.cost_credits, game::CREDITS, t("credits"));
    let (color, label) = match offer.points_type {
        0 => (game::DUCKETS, t("duckets")),
        5 => (game::DIAMONDS, t("diamonds")),
        _ => (game::DIAMONDS, t("points")),
    };
    coin(offer.cost_points, color, label);
    if offer.cost_credits <= 0 && offer.cost_points <= 0 {
        painter.text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, t("Free"), egui::FontId::proportional(13.0), game::TEXT);
    }
    if offer.amount > 1 {
        painter.text(egui::pos2(rect.right() - 10.0, y), egui::Align2::RIGHT_CENTER, format!("× {}", offer.amount), egui::FontId::proportional(13.0), game::TEXT);
    }
}

/// While dragging: the furni and how many, at the pointer, and whether a drop copies.
fn drag_label(ctx: &egui::Context, s: &State, images: &mut Images) {
    let Some(pointer) = ctx.pointer_latest_pos() else { return };
    let small = |text: String| {
        egui::Area::new(egui::Id::new("catalog-drag")).order(egui::Order::Tooltip).fixed_pos(pointer + egui::vec2(14.0, 10.0)).interactable(false).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::WHITE).stroke(Stroke::new(1.0_f32, game::FRAME)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.label(RichText::new(text).color(game::TEXT).size(12.0));
            });
        });
    };
    if let Some(page) = egui::DragAndDrop::payload::<DragPage>(ctx) {
        let caption = s.page(page.0).map(|p| p.caption.clone()).unwrap_or_default();
        small(tf("Page {page}: onto a page to put it under it, onto its edge to put it before or after it", &[("page", &caption)]));
        return;
    }
    if let Some(furni) = egui::DragAndDrop::payload::<DragFurni>(ctx) {
        small(tf("New offer: {count} furni", &[("count", &furni.0.len().to_string())]));
        return;
    }
    let Some(payload) = egui::DragAndDrop::payload::<DragOffers>(ctx) else { return };
    let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
    let first = payload.ids.first().and_then(|id| s.offers.values().flatten().find(|o| o.id == *id)).cloned();
    egui::Area::new(egui::Id::new("catalog-drag")).order(egui::Order::Tooltip).fixed_pos(pointer + egui::vec2(14.0, 10.0)).interactable(false).show(ctx, |ui| {
        egui::Frame::new().fill(Color32::WHITE).stroke(Stroke::new(1.0_f32, game::FRAME)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(4), game::TILE);
                if let Some(offer) = &first {
                    let furni = first_furni(offer).and_then(|id| s.furni.get(&id));
                    draw_furni(ui.painter(), ctx, images, furni, offer, rect.shrink(3.0), 1);
                }
                let what = if copy { t("Copy") } else { t("Move") };
                ui.label(RichText::new(format!("{what}: {}", tf("{count} furni", &[("count", &payload.ids.len().to_string())]))).color(game::TEXT).size(12.0));
                let _ = payload.from;
            });
        });
    });
}

/// A small catalogue for the page's screenshot (the dry run has no database).
fn sample(s: &mut State) {
    let page = |id: i64, parent_id: i64, caption: &str, icon: i64, order: i64| Page {
        id,
        parent_id,
        caption: caption.into(),
        caption_save: caption.to_lowercase(),
        layout: "default_3x3".into(),
        icon_image: icon,
        min_rank: 1,
        order_num: order,
        visible: true,
        enabled: true,
        catalog_mode: "NORMAL".into(),
        ..Page::default()
    };
    s.pages = vec![
        Page { layout: "frontpage4".into(), ..page(1, -1, "Front page", 1, 1) },
        page(2, -1, "Furni", 2, 2),
        page(3, -1, "Clothes", 3, 3),
        page(4, -1, "Pets", 4, 4),
        page(10, 2, "Rares", 21, 1),
        page(11, 2, "Area", 22, 2),
        page(12, 11, "Area Blue", 23, 1),
        page(13, 2, "Plastic", 24, 3),
        page(14, 2, "Mode", 25, 4),
        Page { visible: false, ..page(15, 2, "Staff only", 26, 5) },
    ];
    let names = [
        ("rare_dragonlamp*4", "Blue Dragon Lamp"),
        ("throne", "Throne"),
        ("rare_icecream*1", "Ice Cream Machine"),
        ("hc_crtn", "Curtain"),
        ("pillow*2", "Silk Pillow"),
        ("rare_fountain", "Fountain"),
        ("scifiport*3", "Sci-Fi Port"),
        ("rare_parasol*0", "Parasol"),
    ];
    let mut id = 100;
    for (page_id, take) in [(10, 8), (13, 5)] {
        let mut offers = Vec::new();
        for (i, (item, name)) in names.iter().take(take).enumerate() {
            id += 1;
            s.furni.insert(id, Furni { id, item_name: item.to_string(), public_name: name.to_string(), kind: "s".into(), sprite_id: id });
            offers.push(Offer {
                id,
                page_id,
                catalog_name: item.to_string(),
                item_ids: id.to_string(),
                cost_credits: 5 + i as i64 * 5,
                cost_points: if i % 3 == 0 { 10 } else { 0 },
                points_type: if i % 2 == 0 { 5 } else { 0 },
                amount: 1,
                limited_stack: if i == 1 { 100 } else { 0 },
                order_number: i as i64 + 1,
                offer_id: id,
                ..Offer::default()
            });
        }
        s.offers.insert(page_id, offers);
    }
    for page_id in [1, 2, 3, 4, 11, 12, 14, 15] {
        s.offers.entry(page_id).or_default();
    }
    s.panes[0] = Pane { root: Some(2), page: Some(10), expanded: [11].into_iter().collect(), selected: vec![102], anchor: Some(102), ..Pane::default() };
    s.panes[1] = Pane { root: Some(UNOFFERED), page: None, furni_selected: vec![902], ..Pane::default() };
    s.unoffered = Some(
        [("hc_crtn", "Curtain"), ("pillow*2", "Silk Pillow"), ("rare_fountain", "Fountain"), ("rare_parasol*0", "Parasol"), ("throne", "Throne")]
            .iter()
            .enumerate()
            .map(|(i, (item, name))| Furni { id: 900 + i as i64, item_name: item.to_string(), public_name: name.to_string(), kind: "s".into(), sprite_id: 900 + i as i64 })
            .collect(),
    );
    s.two = true;
    s.sample = true;
    s.database = "habbo".into();
    s.subtitle = t("A sample catalogue: the dry run has no database.").into();
    if std::env::var_os("AIO_SCREENSHOT_EDITOR").is_some() {
        let table = match std::env::var("AIO_SCREENSHOT_EDITOR").as_deref() {
            Ok("offer") => "catalog_items",
            Ok("page") => "catalog_pages",
            _ => "items_base",
        };
        sample_editor(s, table, 102, Some(102));
        if let (Ok(_), Some(editor)) = (std::env::var("AIO_SCREENSHOT_HTML"), s.editor.as_ref()) {
            if let Some(column) = s.columns.get(editor.table).and_then(|c| c.iter().position(|c| c.name == "page_text1")) {
                let text = "<p>The <b>rarest</b> furni of the hotel, <span style=\"color: #d62728; position: fixed\">only this week</span>!</p>\n<a href=\"https://hotel.example.com/rares\">See all rares</a><script>steal()</script>".to_string();
                s.html = Some(HtmlEditor { column, text, selection: (0, 0) });
            }
        }
        if let (Some(kind), Some(editor)) = (std::env::var("AIO_SCREENSHOT_PICKER").ok(), s.editor.as_ref()) {
            let name = if kind == "icon" { "icon_image" } else { "page_headline" };
            if let Some(column) = s.columns.get(editor.table).and_then(|c| c.iter().position(|c| c.name == name)) {
                s.picker = Some(Picker { column, icon: kind == "icon", search: String::new() });
            }
        }
    }
}

/// The editor on a sample furni (the columns of Arcturus' items_base), for the screenshot.
fn sample_editor(s: &mut State, table: &'static str, id: i64, furni: Option<i64>) {
    if table == "catalog_pages" {
        let columns: Vec<(&str, &str, &str)> = vec![
            ("id", "int(11)", "10"),
            ("parent_id", "int(11)", "2"),
            ("caption_save", "varchar(25)", "rares"),
            ("caption", "varchar(128)", "Rares"),
            ("page_layout", "enum('default_3x3','frontpage','frontpage4','spaces','trophies','pets','info_loyalty','soundmachine')", "default_3x3"),
            ("icon_color", "int(11)", "1"),
            ("icon_image", "int(11)", "21"),
            ("min_rank", "int(11)", "1"),
            ("order_num", "int(11)", "1"),
            ("visible", "enum('0','1')", "1"),
            ("enabled", "enum('0','1')", "1"),
            ("club_only", "enum('0','1')", "0"),
            ("vip_only", "enum('0','1')", "0"),
            ("page_headline", "varchar(1024)", "catalog_rares_headline1"),
            ("page_teaser", "varchar(64)", ""),
            ("page_special", "varchar(2048)", ""),
            ("page_text1", "text", "The rarest furni of the hotel."),
            ("page_text2", "text", ""),
            ("page_text_details", "text", ""),
            ("page_text_teaser", "text", ""),
            ("room_id", "int(11)", "0"),
            ("includes", "varchar(32)", ""),
        ];
        s.columns.insert(table, columns.iter().map(|(name, kind, _)| FurniColumn { name: name.to_string(), column_type: kind.to_string(), nullable: false, comment: String::new() }).collect());
        let original: Vec<Option<String>> = columns.iter().map(|(_, _, v)| Some(v.to_string())).collect();
        let values: Vec<String> = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
        s.editor = Some(Editor { table, id, furni, page: None, nulls: vec![false; values.len()], values, original: Some(original), saving: false, status: Status::default() });
        return;
    }
    if table == "catalog_items" {
        let columns: Vec<(&str, &str, &str)> = vec![
            ("id", "int(11)", "102"),
            ("item_ids", "varchar(666)", "102"),
            ("page_id", "int(11)", "10"),
            ("catalog_name", "varchar(100)", "throne"),
            ("cost_credits", "int(11)", "10"),
            ("cost_points", "int(11)", "0"),
            ("points_type", "int(11)", "0"),
            ("amount", "int(11)", "1"),
            ("limited_stack", "int(11)", "100"),
            ("limited_sells", "int(11)", "12"),
            ("order_number", "int(11)", "2"),
            ("offer_id", "int(11)", "102"),
            ("song_id", "int(11) unsigned", "0"),
            ("extradata", "varchar(500)", ""),
            ("have_offer", "enum('0','1')", "1"),
            ("club_only", "enum('0','1')", "0"),
        ];
        s.columns.insert(table, columns.iter().map(|(name, kind, _)| FurniColumn { name: name.to_string(), column_type: kind.to_string(), nullable: false, comment: String::new() }).collect());
        let original: Vec<Option<String>> = columns.iter().map(|(_, _, v)| Some(v.to_string())).collect();
        let values: Vec<String> = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
        s.editor = Some(Editor { table, id, furni, page: Some(10), nulls: vec![false; values.len()], values, original: Some(original), saving: false, status: Status::default() });
        return;
    }
    let columns: Vec<(&str, &str, &str)> = vec![
        ("id", "int(11)", "102"),
        ("sprite_id", "int(11)", "4906"),
        ("item_name", "varchar(70)", "throne"),
        ("public_name", "varchar(56)", "Throne"),
        ("width", "int(11)", "1"),
        ("length", "int(11)", "1"),
        ("stack_height", "double(4,2)", "1.6"),
        ("allow_stack", "enum('0','1')", "0"),
        ("allow_sit", "enum('0','1')", "1"),
        ("allow_lay", "enum('0','1')", "0"),
        ("allow_walk", "enum('0','1')", "0"),
        ("allow_gift", "enum('0','1')", "1"),
        ("allow_trade", "enum('0','1')", "1"),
        ("allow_recycle", "enum('0','1')", "0"),
        ("allow_marketplace_sell", "enum('0','1')", "1"),
        ("allow_inventory_stack", "enum('0','1')", "1"),
        ("type", "enum('s','i','e','h','v','r','b','p')", "s"),
        ("interaction_type", "varchar(500)", "default"),
        ("interaction_modes_count", "int(11)", "1"),
        ("vending_ids", "varchar(255)", "0"),
        ("multiheight", "varchar(50)", "0"),
        ("customparams", "varchar(256)", ""),
        ("effect_id_male", "int(11)", "0"),
        ("effect_id_female", "int(11)", "0"),
        ("clothing_on_walk", "varchar(255)", ""),
    ];
    s.columns.insert(table, columns.iter().map(|(name, kind, _)| FurniColumn { name: name.to_string(), column_type: kind.to_string(), nullable: false, comment: String::new() }).collect());
    let original: Vec<Option<String>> = columns.iter().map(|(_, _, v)| Some(v.to_string())).collect();
    let mut values: Vec<String> = original.iter().map(|v| v.clone().unwrap_or_default()).collect();
    values[6] = "1.5".into();
    s.editor = Some(Editor { table, id, furni, page: None, nulls: vec![false; values.len()], values, original: Some(original), saving: false, status: Status::default() });
}

/// The screenshot's drops, without a database.
fn sample_place(s: &mut State, ids: &[i64], page: i64, before: Option<i64>, copy: bool) {
    let all: Vec<Offer> = s.offers.values().flatten().cloned().collect();
    let target: Vec<i64> = s.offers.get(&page).map(|o| o.iter().map(|o| o.id).collect()).unwrap_or_default();
    let mut placed = Vec::new();
    for id in ids {
        if let Some(mut offer) = all.iter().find(|o| o.id == *id).cloned() {
            if copy {
                offer.id = all.iter().map(|o| o.id).max().unwrap_or(0) + 1 + placed.len() as i64;
            } else {
                for offers in s.offers.values_mut() {
                    offers.retain(|o| o.id != *id);
                }
            }
            offer.page_id = page;
            placed.push(offer);
        }
    }
    let order = catalog_admin::arranged(&target, &placed.iter().map(|o| o.id).collect::<Vec<_>>(), before);
    let mut pool: Vec<Offer> = s.offers.remove(&page).unwrap_or_default();
    pool.extend(placed);
    let offers = order.iter().filter_map(|id| pool.iter().find(|o| o.id == *id).cloned()).collect();
    s.offers.insert(page, offers);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with the pointer at `pos`, pressed or released, and a drop target over the whole screen that asks for
    /// offers first and then for a page, as a tree row does.
    fn frame(ctx: &egui::Context, pos: egui::Pos2, pressed: Option<bool>, start: Option<DragPage>) -> (Option<i64>, bool) {
        let mut input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        let mut got = (None, false);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.allocate_rect(ui.max_rect(), Sense::click_and_drag());
                if let Some(page) = start.clone() {
                    egui::DragAndDrop::set_payload(ctx, page);
                }
                let offers = released::<DragOffers>(&response).is_some();
                let page = released::<DragPage>(&response).map(|p| p.0);
                got = (page, offers);
            });
        });
        got
    }

    #[test]
    fn the_preview_shows_the_text_with_its_breaks() {
        let job = html_preview("<p>A <b>bold</b> &amp; <a href=\"#x\">link</a></p>B<br>C<img src=\"https://h/a.png\">");
        assert_eq!(job.text, "A bold & link\nB\nC[image]");
        assert!(job.sections.iter().any(|s| s.format.underline.width > 0.0), "the link is underlined");
    }

    #[test]
    fn picked_images_become_the_values_the_client_reads() {
        assert_eq!(icon_number("icon_12.png"), Some(12));
        assert_eq!(icon_number("icon_x.png"), None);
        assert_eq!(picked_value("icon_12.png", true), "12");
        assert_eq!(picked_value("catalog_rares_headline1.gif", false), "catalog_rares_headline1");
        assert_eq!(picked_value("teaser.png", false), "teaser.png", "the client adds .gif: a png keeps its ending");
        assert_eq!(value_files("12", true), ["icon_12.png"]);
        assert_eq!(value_files("catalog_rares_headline1", false), ["catalog_rares_headline1.gif", "catalog_rares_headline1.png"]);
        assert_eq!(value_files("teaser.png", false), ["teaser.png"]);
        assert!(value_files(" ", false).is_empty());
        assert_eq!(image_column("icon_image"), Some(true));
        assert_eq!(image_column("page_headline"), Some(false));
        assert_eq!(image_column("caption"), None);
    }

    #[test]
    fn a_dropped_page_is_not_thrown_away_by_an_offer_target() {
        let ctx = egui::Context::default();
        let at = egui::pos2(100.0, 100.0);
        frame(&ctx, at, None, None);
        frame(&ctx, at, Some(true), Some(DragPage(42)));
        let (page, offers) = frame(&ctx, at + egui::vec2(20.0, 20.0), Some(false), None);
        assert!(!offers, "the payload is a page, not offers");
        assert_eq!(page, Some(42), "the page arrives although the offers were asked for first");
    }
}
