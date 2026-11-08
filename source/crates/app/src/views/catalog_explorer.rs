//! The catalogue explorer: the hotel catalogue as the game shows it (root pages as tabs, the page tree with its
//! icons, the furni of a page as a grid with an info box), in two windows side by side. Furni are dragged like files:
//! onto another window, a page in a tree, or before another furni (to reorder); with Ctrl held they are copied. Every
//! drop is saved at once (one transaction, `catalog_admin::place_offers`) and can be undone; **Publish** makes the
//! emulator load the catalogue again. The icons come from the downloaded assets (Habbo and the Nitro retro profiles).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::catalog_admin::{self, Furni, Offer, Page, PlaceUndo};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
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
}

#[derive(Default)]
struct State {
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    prompt: Option<Prompt>,
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
    undo: Option<PlaceUndo>,
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
        *self = State { generation: self.generation, two: true, ..State::default() };
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
                        if pane.root.is_none_or(|r| !roots.contains(&r)) {
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
                    s.undo = Some(undo);
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
        let result = catalog_admin::undo_place(&session, &database, &undo).await;
        self.with(generation, |s| {
            s.busy = false;
            s.status = match result {
                Ok(_) => Status { text: t("The last drop is undone.").into(), error: false },
                Err(error) => Status { text: error.to_string(), error: true },
            };
        });
        for page in undo.pages {
            self.load_offers(page).await;
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
        Self { state: Arc::new(Mutex::new(State { two: true, ..State::default() })), images: Images::new() }
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
            tabs(ui, &ctx, s, images, index);
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
fn tabs(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize) {
    let roots: Vec<(i64, String, i64, bool)> = s.roots().iter().map(|p| (p.id, p.caption.clone(), p.icon_image, p.visible)).collect();
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
                    let size = egui::vec2(text_width + 34.0, 26.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    let fill = if active { game::BODY } else if response.hovered() { game::TAB_HOVER } else { game::TAB };
                    ui.painter().rect_filled(rect, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, fill);
                    if let Some(texture) = images.page(ctx, *icon) {
                        ui.painter().image(texture.id(), egui::Rect::from_center_size(rect.left_center() + egui::vec2(14.0, 0.0), egui::vec2(18.0, 18.0)), uv(), Color32::WHITE);
                    }
                    let color = if *visible { game::TEXT } else { game::HIDDEN };
                    ui.painter().text(rect.left_center() + egui::vec2(26.0, 0.0), egui::Align2::LEFT_CENTER, caption, font, color);
                    if response.clicked() {
                        chosen = Some(*id);
                    }
                }
            });
        });
    });
    if let Some(id) = chosen {
        let pane = &mut s.panes[index];
        pane.root = Some(id);
        pane.page = Some(id);
        pane.selected.clear();
        pane.search.clear();
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

fn uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// The page tree of the open tab (or, with a search, every page that matches), each a drop target.
fn navigation(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut State, images: &mut Images, index: usize, height: f32, actions: &mut Vec<Action>) {
    egui::Frame::new().fill(game::NAV).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
        ui.set_width(NAV_WIDTH - 12.0);
        ui.set_height(height - 12.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        light_fields(ui);
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
                let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::click());
                let active = s.panes[index].page == Some(id);
                let dropping = response.dnd_hover_payload::<DragOffers>().is_some();
                let fill = if dropping {
                    game::TILE_SELECTED
                } else if active {
                    game::TITLE
                } else if response.hovered() {
                    game::NAV_HOVER
                } else {
                    game::NAV
                };
                ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
                if dropping {
                    ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(2.0_f32, game::DROP), egui::StrokeKind::Inside);
                }
                let mut x = rect.left() + 4.0 + depth as f32 * 12.0;
                let text_color = if active && !dropping { Color32::WHITE } else if page.visible { game::TEXT } else { game::HIDDEN };
                if has_children {
                    let open = s.panes[index].expanded.contains(&id);
                    ui.painter().text(egui::pos2(x + 4.0, rect.center().y), egui::Align2::CENTER_CENTER, if open { "▾" } else { "▸" }, egui::FontId::proportional(12.0), text_color);
                }
                x += 12.0;
                if let Some(texture) = images.page(ctx, page.icon_image) {
                    ui.painter().image(texture.id(), egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 9.0), egui::vec2(18.0, 18.0)), uv(), Color32::WHITE);
                }
                x += 22.0;
                let caption = if page.enabled { page.caption.clone() } else { format!("{} ({})", page.caption, t("off")) };
                let galley = ui.painter().layout(caption, egui::FontId::proportional(12.0), text_color, (rect.right() - x - 4.0).max(10.0));
                ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, text_color);
                if let Some(payload) = response.dnd_release_payload::<DragOffers>() {
                    let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                    actions.push(Action::Place { ids: payload.ids.clone(), page: id, before: None, copy });
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
    let Some(page_id) = s.panes[index].page else { return };
    let Some(page) = s.page(page_id).cloned() else { return };
    let offers = s.offers.get(&page_id).cloned();
    egui::Frame::new().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // The page bar: icon, caption, what it holds.
        let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius::same(6), game::PAGE_BAR);
        if let Some(texture) = images.page(ctx, page.icon_image) {
            ui.painter().image(texture.id(), egui::Rect::from_center_size(bar.left_center() + egui::vec2(22.0, 0.0), egui::vec2(28.0, 28.0)), uv(), Color32::WHITE);
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
                        draw_furni(painter, ctx, images, furni.as_ref(), offer, rect.shrink(6.0));
                        if offer.limited_stack > 0 {
                            painter.text(rect.right_top() + egui::vec2(-4.0, 3.0), egui::Align2::RIGHT_TOP, "LTD", egui::FontId::proportional(9.0), game::DROP);
                        }
                        if catalog_admin::item_ids(&offer.item_ids).is_some_and(|ids| ids.len() > 1) {
                            painter.text(rect.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "+", egui::FontId::proportional(11.0), game::TITLE);
                        }
                        let response = response.on_hover_text(furni.as_ref().map(|f| f.public_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| offer.catalog_name.clone()));
                        if response.dnd_hover_payload::<DragOffers>().is_some() {
                            hovered_before = Some((offer.id, rect));
                        }
                        if let Some(payload) = response.dnd_release_payload::<DragOffers>() {
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
        } else if area_response.dnd_hover_payload::<DragOffers>().is_some() {
            ui.painter().rect_stroke(area, CornerRadius::same(6), Stroke::new(2.0_f32, game::DROP), egui::StrokeKind::Inside);
        }
        if hovered_before.is_none() {
            if let Some(payload) = area_response.dnd_release_payload::<DragOffers>() {
                let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
                actions.push(Action::Place { ids: payload.ids.clone(), page: page_id, before: None, copy });
            }
        }
        ui.add_space(8.0);
        info_box(ui, ctx, s, images, index, &offers, info_height);
    });
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
fn draw_furni(painter: &egui::Painter, ctx: &egui::Context, images: &mut Images, furni: Option<&Furni>, offer: &Offer, rect: egui::Rect) {
    let name = furni.map(|f| f.item_name.as_str()).unwrap_or(offer.catalog_name.as_str());
    if let Some(texture) = images.furni(ctx, name) {
        let size = texture.size_vec2();
        let scale = (rect.width() / size.x).min(rect.height() / size.y).min(if size.x <= rect.width() / 2.0 { 2.0 } else { 1.0 });
        painter.image(texture.id(), egui::Rect::from_center_size(rect.center(), size * scale), uv(), Color32::WHITE);
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
    draw_furni(painter, ctx, images, furni, offer, preview.shrink(8.0));
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
    let Some(payload) = egui::DragAndDrop::payload::<DragOffers>(ctx) else { return };
    let Some(pointer) = ctx.pointer_latest_pos() else { return };
    let copy = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
    let first = payload.ids.first().and_then(|id| s.offers.values().flatten().find(|o| o.id == *id)).cloned();
    egui::Area::new(egui::Id::new("catalog-drag")).order(egui::Order::Tooltip).fixed_pos(pointer + egui::vec2(14.0, 10.0)).interactable(false).show(ctx, |ui| {
        egui::Frame::new().fill(Color32::WHITE).stroke(Stroke::new(1.0_f32, game::FRAME)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(4), game::TILE);
                if let Some(offer) = &first {
                    let furni = first_furni(offer).and_then(|id| s.furni.get(&id));
                    draw_furni(ui.painter(), ctx, images, furni, offer, rect.shrink(3.0));
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
        page(1, -1, "Front page", 1, 1),
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
    s.panes[1] = Pane { root: Some(2), page: Some(13), ..Pane::default() };
    s.two = true;
    s.sample = true;
    s.database = "habbo".into();
    s.subtitle = t("A sample catalogue: the dry run has no database.").into();
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
