//! The catalogue manager: the hotel catalogue's pages as a tree, a page's settings and offers, and an offer's
//! settings, edited in the hotel database through the SSH session (the SQL Editor's tunnel and MariaDB login).
//! **Publish** makes the running emulator load the catalogue again (RCON `updatecatalog`), which tells the players
//! online to refresh it. The data and its rules live in `aio_remote::catalog_admin`.

use std::sync::{Arc, Mutex};

use aio_remote::catalog_admin::{self, CatalogBackup, Featured, Furni, LimitedAction, LimitedSeries, Offer, Page, TreeRow, Voucher, VoucherUse, CATALOG_MODES, FEATURED_TYPES, LAYOUTS};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{Fixed, Palette};
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

impl Status {
    fn ok(text: String) -> Self {
        Status { text, error: false }
    }

    fn error(text: String) -> Self {
        Status { text, error: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Pages,
    FrontPage,
    Limited,
    Vouchers,
    Checks,
    Unoffered,
    Backups,
}

/// A front page slot as edited: its expiry as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FeaturedRow {
    slot: Featured,
    expiry: String,
}

impl FeaturedRow {
    fn new(slot: Featured) -> Self {
        let expiry = catalog_admin::expiry_text(slot.expire_timestamp);
        FeaturedRow { slot, expiry }
    }
}

/// The page builder: a new page filled with an offer per furni whose class name starts with a prefix.
#[derive(Clone)]
struct Builder {
    page: Page,
    prefix: String,
    /// Only furni no offer sells yet.
    only_new: bool,
    credits: i64,
    points: i64,
    points_type: i64,
    /// The furni found for the prefix (None until Find furni).
    found: Option<Vec<Furni>>,
}

#[derive(Default)]
struct State {
    builder: Option<Builder>,
    tab: Tab,
    /// The front page slots as loaded, and as edited (None until the tab is opened).
    featured: Option<Vec<FeaturedRow>>,
    featured_saved: Vec<FeaturedRow>,
    /// The limited series (None until the tab is opened), the sizes typed per offer, and the search.
    limited: Option<Vec<LimitedSeries>>,
    limited_sizes: std::collections::HashMap<i64, i64>,
    limited_search: String,
    /// The vouchers (None until the tab is opened), the one shown as loaded and as edited, its uses, the search, and
    /// the code generator's prefix, length and count.
    vouchers: Option<Vec<Voucher>>,
    voucher: Option<Voucher>,
    voucher_edit: Option<Voucher>,
    voucher_uses: Vec<VoucherUse>,
    voucher_search: String,
    generate_prefix: String,
    generate_length: usize,
    generate_count: usize,
    /// The furni picker of the offer form: the search, its results, the names of furni ids seen, and the ids asked
    /// for already (so a missing id is not asked for every frame).
    furni_search: String,
    furni_results: Vec<Furni>,
    furni_names: std::collections::HashMap<i64, Furni>,
    furni_asked: std::collections::HashSet<i64>,
    /// The Checks tab: every offer (None until it is run) and the duplicate offers ticked for deleting.
    all_offers: Option<Vec<Offer>>,
    duplicates_ticked: std::collections::HashSet<i64>,
    /// The Unoffered tab: the furni no offer sells (None until it is run), the ones ticked, the search, and the page
    /// and price the new offers get.
    unoffered: Option<Vec<Furni>>,
    unoffered_ticked: std::collections::HashSet<i64>,
    unoffered_search: String,
    unoffered_page: i64,
    unoffered_credits: i64,
    unoffered_points: i64,
    unoffered_points_type: i64,
    /// The Backups tab: the backups on the server (None until it is opened) and the label for a new one.
    backups: Option<Vec<CatalogBackup>>,
    backup_label: String,
    /// Raised when the session closes: an older task changes nothing.
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    prompt: Option<Prompt>,
    login_visible: bool,
    database: String,
    subtitle: String,
    pages: Vec<Page>,
    rows: Vec<TreeRow>,
    search: String,
    /// The page shown, as loaded, and as edited.
    page: Option<Page>,
    page_edit: Option<Page>,
    offers: Vec<Offer>,
    offer: Option<Offer>,
    offer_edit: Option<Offer>,
    busy: bool,
    /// Saved since the last publish: the hotel still shows the old catalogue.
    unpublished: bool,
    status: Status,
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
        let generation = self.generation;
        *self = State { generation, ..State::default() };
    }

    fn set_pages(&mut self, pages: Vec<Page>) {
        self.rows = catalog_admin::tree(&pages);
        self.pages = pages;
    }
}

pub struct CatalogManagerView {
    state: Arc<Mutex<State>>,
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

    /// Finds the hotel database, logs in to MariaDB like the SQL Editor (the saved root account, or asks), and
    /// loads the pages.
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
            self.with(generation, |s| s.status = Status::error(t("Dry run: there is no MariaDB to connect to.").into()));
            return;
        }
        self.with(generation, |s| s.subtitle = t("Looking for the hotel database ...").into());
        let database = match catalog_admin::hotel_database(&*server).await {
            Ok(database) => database,
            Err(error) => {
                self.with(generation, |s| s.status = Status::error(error.to_string()));
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
        self.load_pages(None).await;
        let tab = self.lock().tab;
        match tab {
            Tab::FrontPage => self.load_featured().await,
            Tab::Limited => self.load_limited().await,
            Tab::Vouchers => self.load_vouchers(None).await,
            Tab::Checks => self.load_checks().await,
            Tab::Unoffered => self.load_unoffered().await,
            Tab::Backups => self.load_backups().await,
            Tab::Pages => {}
        }
    }

    async fn load_pages(&self, select: Option<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_pages(&session, &database).await;
        let selected = self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(pages) => {
                    s.status = Status::ok(tf("{count} pages.", &[("count", &pages.len().to_string())]));
                    s.set_pages(pages);
                    select.or(s.page.as_ref().map(|p| p.id))
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    None
                }
            }
        });
        if let Some(Some(id)) = selected {
            self.select_page(id).await;
        }
    }

    async fn select_page(&self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let page = {
            let s = self.lock();
            s.pages.iter().find(|p| p.id == id).cloned()
        };
        let Some(page) = page else { return };
        self.with(generation, |s| {
            s.builder = None;
            s.page = Some(page.clone());
            s.page_edit = Some(page.clone());
            s.offers.clear();
            s.offer = None;
            s.offer_edit = None;
            s.busy = true;
        });
        let offers = catalog_admin::load_offers(&session, &database, id).await;
        self.with(generation, |s| {
            s.busy = false;
            if s.page.as_ref().map(|p| p.id) != Some(id) {
                return;
            }
            match offers {
                Ok(offers) => s.offers = offers,
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    async fn save_page(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(page) = self.lock().page_edit.clone() else { return };
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_page(&session, &database, &page).await;
        let id = self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(id) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Page {name} saved. Publish to show it in the hotel.", &[("name", &page.caption)]));
                    Some(id)
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    None
                }
            }
        });
        if let Some(Some(id)) = id {
            self.with(generation, |s| s.page = None);
            self.load_pages(Some(id)).await;
        }
    }

    async fn save_offer(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(offer) = self.lock().offer_edit.clone() else { return };
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_offer(&session, &database, &offer).await;
        let result = self.with(generation, |s| {
            s.busy = false;
            let id = match saved {
                Ok(id) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Offer {name} saved. Publish to show it in the hotel.", &[("name", &offer.catalog_name)]));
                    Some(id)
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    None
                }
            };
            (id, s.page.as_ref().map(|p| p.id))
        });
        if let Some((Some(offer_id), Some(page_id))) = result {
            self.select_page(page_id).await;
            self.with(generation, |s| {
                if let Some(found) = s.offers.iter().find(|o| o.id == offer_id).cloned() {
                    s.offer = Some(found.clone());
                    s.offer_edit = Some(found);
                }
            });
        }
    }

    /// Asks first, then deletes the page shown with its offers.
    async fn delete_page(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (page, problem, offers) = {
            let s = self.lock();
            let Some(page) = s.page.clone().filter(|p| p.id > 0) else { return };
            (page.clone(), catalog_admin::page_delete_problem(&s.pages, &s.offers, page.id), s.offers.len())
        };
        if let Some(problem) = problem {
            self.with(generation, |s| s.status = Status::error(problem));
            return;
        }
        let message = tf("The page {name} and its {count} offers are deleted from the database. This cannot be undone.", &[("name", &page.caption), ("count", &offers.to_string())]);
        let options = vec![Choice::new(t("Delete the page"), t("Delete it and its offers."), true).primary(), Choice::new(t("Cancel"), t("Keep the page."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &page.caption)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_page(&session, &database, page.id).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.page = None;
                    s.page_edit = None;
                    s.offers.clear();
                    s.offer = None;
                    s.offer_edit = None;
                    s.status = Status::ok(tf("Page {name} deleted. Publish to take it out of the hotel.", &[("name", &page.caption)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_pages(None).await;
    }

    /// Asks first, then deletes the offer shown.
    async fn delete_offer(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(offer) = self.lock().offer.clone().filter(|o| o.id > 0) else { return };
        if let Some(problem) = catalog_admin::offer_delete_problem(&offer) {
            self.with(generation, |s| s.status = Status::error(problem));
            return;
        }
        let options = vec![Choice::new(t("Delete the offer"), t("Take it off the page for good."), true).primary(), Choice::new(t("Cancel"), t("Keep the offer."), false)];
        let message = t("The offer is deleted from the database. Furni players already bought stays theirs. This cannot be undone.");
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &offer.catalog_name)]), message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_offer(&session, &database, offer.id).await;
        let page_id = self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Offer {name} deleted. Publish to take it out of the hotel.", &[("name", &offer.catalog_name)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
            s.page.as_ref().map(|p| p.id)
        });
        if let Some(Some(page_id)) = page_id {
            self.select_page(page_id).await;
        }
    }

    async fn load_featured(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_featured(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(slots) => {
                    let rows: Vec<FeaturedRow> = slots.into_iter().map(FeaturedRow::new).collect();
                    s.featured_saved = rows.clone();
                    s.featured = Some(rows);
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    async fn save_featured(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let slots: Option<Vec<Featured>> = self.lock().featured.as_ref().map(|rows| {
            rows.iter()
                .map(|row| Featured { expire_timestamp: catalog_admin::parse_expiry(&row.expiry).unwrap_or(-1), ..row.slot.clone() })
                .collect()
        });
        let Some(slots) = slots else { return };
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_featured(&session, &database, &slots).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(t("Front page saved. Publish to show it in the hotel.").into());
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_featured().await;
    }

    async fn load_limited(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_limited(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(series) => {
                    s.limited = Some(series);
                    s.limited_sizes.clear();
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Grows, ends or (after asking) removes a limited series with its sales.
    async fn change_limited(self, offer_id: i64, action: LimitedAction) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(series) = self.lock().limited.as_ref().and_then(|list| list.iter().find(|l| l.offer_id == offer_id).cloned()) else { return };
        if let Some(problem) = catalog_admin::limited_problem(&series, action) {
            self.with(generation, |s| s.status = Status::error(problem));
            return;
        }
        if action == LimitedAction::RemoveWithSales {
            let message = tf(
                "The series of {name} and the record of its {sold} sales are deleted for good; this cannot be undone. Furni players already have keeps its number.",
                &[("name", &series.catalog_name), ("sold", &series.sold.to_string())],
            );
            let options = vec![Choice::new(t("Remove with its sales"), t("End the series and delete its numbers."), true).primary(), Choice::new(t("Cancel"), t("Keep the series."), false)];
            if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Remove the series of {name}?", &[("name", &series.catalog_name)]), &message, false, options).await {
                return;
            }
        }
        self.with(generation, |s| s.busy = true);
        let changed = catalog_admin::change_limited(&session, &database, offer_id, action).await;
        self.with(generation, |s| {
            s.busy = false;
            match changed {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Series of {name} changed. Publish to put it in the hotel.", &[("name", &series.catalog_name)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_limited().await;
    }

    async fn load_vouchers(&self, select: Option<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_vouchers(&session, &database).await;
        let selected = self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(vouchers) => {
                    let found = select.and_then(|id| vouchers.iter().find(|v| v.id == id).cloned());
                    s.vouchers = Some(vouchers);
                    s.voucher = found.clone();
                    s.voucher_edit = found.clone();
                    s.voucher_uses.clear();
                    found.map(|v| v.id)
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    None
                }
            }
        });
        if let Some(Some(id)) = selected {
            self.load_voucher_uses(id).await;
        }
    }

    async fn load_voucher_uses(&self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let uses = catalog_admin::load_voucher_uses(&session, &database, id).await;
        self.with(generation, |s| {
            if s.voucher.as_ref().map(|v| v.id) != Some(id) {
                return;
            }
            match uses {
                Ok(uses) => s.voucher_uses = uses,
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    async fn save_voucher(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(voucher) = self.lock().voucher_edit.clone() else { return };
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_voucher(&session, &database, &voucher).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Voucher {code} saved. Publish so the hotel knows it.", &[("code", voucher.code.trim())]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        let id = if voucher.id > 0 { Some(voucher.id) } else { None };
        self.load_vouchers(id).await;
    }

    async fn delete_voucher(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(voucher) = self.lock().voucher.clone().filter(|v| v.id > 0) else { return };
        let options = vec![Choice::new(t("Delete the voucher"), t("The code stops working once you publish."), true).primary(), Choice::new(t("Cancel"), t("Keep the voucher."), false)];
        let message = tf("The voucher {code} and the record of its {used} uses are deleted. This cannot be undone.", &[("code", &voucher.code), ("used", &voucher.used.to_string())]);
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &voucher.code)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_voucher(&session, &database, voucher.id).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Voucher {code} deleted. Publish so the hotel forgets it.", &[("code", &voucher.code)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_vouchers(None).await;
    }

    /// Adds `count` vouchers with new random codes, each giving what the form shows.
    async fn generate_vouchers(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (template, prefix, length, count, taken) = {
            let s = self.lock();
            let Some(template) = s.voucher_edit.clone() else { return };
            let taken: Vec<String> = s.vouchers.iter().flatten().map(|v| v.code.clone()).collect();
            (template, s.generate_prefix.clone(), s.generate_length, s.generate_count, taken)
        };
        let codes = catalog_admin::random_voucher_codes(&prefix, length, count, &taken);
        let Some(codes) = codes else {
            self.with(generation, |s| s.status = Status::error(t("These codes cannot be made: keep at least 4 random characters, at most 10 in all and 500 codes.").into()));
            return;
        };
        self.with(generation, |s| s.busy = true);
        let added = catalog_admin::add_vouchers(&session, &database, &template, &codes).await;
        self.with(generation, |s| {
            s.busy = false;
            match added {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("{count} vouchers made, from {first} to {last}. Publish so the hotel knows them.", &[
                        ("count", &codes.len().to_string()),
                        ("first", codes.first().map(String::as_str).unwrap_or("")),
                        ("last", codes.last().map(String::as_str).unwrap_or("")),
                    ]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_vouchers(None).await;
    }

    async fn search_furni(&self, text: String) {
        let Some((generation, session, database)) = self.session() else { return };
        let found = catalog_admin::search_furni(&session, &database, &text).await;
        self.with(generation, |s| {
            if s.furni_search.trim() != text.trim() {
                return;
            }
            match found {
                Ok(list) => {
                    for furni in &list {
                        s.furni_names.insert(furni.id, furni.clone());
                    }
                    s.furni_results = list;
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    async fn furni_names(&self, ids: Vec<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        let found = catalog_admin::furni_by_ids(&session, &database, &ids).await;
        self.with(generation, |s| {
            if let Ok(list) = found {
                for furni in list {
                    s.furni_names.insert(furni.id, furni);
                }
            }
        });
    }

    async fn load_checks(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let offers = catalog_admin::load_all_offers(&session, &database).await;
        let pages = catalog_admin::load_pages(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match (offers, pages) {
                (Ok(offers), Ok(pages)) => {
                    s.set_pages(pages);
                    s.all_offers = Some(offers);
                    s.duplicates_ticked.clear();
                }
                (Err(error), _) | (_, Err(error)) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Asks first, then deletes the ticked duplicate offers (never one with limited sales).
    async fn delete_duplicates(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let ids: Vec<i64> = {
            let s = self.lock();
            let offers = s.all_offers.as_deref().unwrap_or(&[]);
            offers.iter().filter(|o| s.duplicates_ticked.contains(&o.id) && catalog_admin::offer_delete_problem(o).is_none()).map(|o| o.id).collect()
        };
        if ids.is_empty() {
            return;
        }
        let options = vec![Choice::new(t("Delete them"), t("Take the ticked offers off their pages for good."), true).primary(), Choice::new(t("Cancel"), t("Keep them."), false)];
        let message = tf("{count} offers are deleted from the database. Furni players already bought stays theirs. This cannot be undone.", &[("count", &ids.len().to_string())]);
        if !self.shared.dialogs.choose(t("Are you sure?"), t("Delete the ticked offers?"), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_offers(&session, &database, &ids).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(count) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("{count} offers deleted. Publish to take them out of the hotel.", &[("count", &count.to_string())]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_checks().await;
    }

    /// Numbers the offers of the page shown 1, 2, ... by name.
    async fn sort_offers(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (page_id, order) = {
            let s = self.lock();
            let Some(page) = s.page.as_ref().filter(|p| p.id > 0) else { return };
            (page.id, catalog_admin::sorted_by_name(&s.offers))
        };
        self.with(generation, |s| s.busy = true);
        let sorted = catalog_admin::set_offer_order(&session, &database, &order).await;
        self.with(generation, |s| {
            s.busy = false;
            match sorted {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(t("Offers sorted by name. Publish to show the new order in the hotel.").into());
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.select_page(page_id).await;
    }

    async fn load_unoffered(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let furni = catalog_admin::load_sellable_furni(&session, &database).await;
        let offers = catalog_admin::load_all_offers(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match (furni, offers) {
                (Ok(furni), Ok(offers)) => {
                    s.unoffered = Some(catalog_admin::unoffered_furni(&furni, &offers));
                    s.all_offers = Some(offers);
                    s.unoffered_ticked.clear();
                    if s.unoffered_credits == 0 && s.unoffered_points == 0 {
                        s.unoffered_credits = 3;
                    }
                }
                (Err(error), _) | (_, Err(error)) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Adds an offer for each ticked furni to the chosen page.
    async fn add_unoffered(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let (offers, page_name) = {
            let s = self.lock();
            let ticked: Vec<Furni> = s.unoffered.iter().flatten().filter(|f| s.unoffered_ticked.contains(&f.id)).cloned().collect();
            let Some(page) = s.pages.iter().find(|p| p.id == s.unoffered_page) else { return };
            let offers = catalog_admin::offers_for(&ticked, page.id, s.all_offers.as_deref().unwrap_or(&[]), s.unoffered_credits, s.unoffered_points, s.unoffered_points_type);
            (offers, page.caption.clone())
        };
        if offers.is_empty() {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let added = catalog_admin::add_offers(&session, &database, &offers).await;
        self.with(generation, |s| {
            s.busy = false;
            match added {
                Ok(count) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("{count} offers added to {page}. Publish to show them in the hotel.", &[("count", &count.to_string()), ("page", &page_name)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_unoffered().await;
    }

    async fn load_backups(&self) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let listed = catalog_admin::list_backups(&*server).await;
        self.with(generation, |s| match listed {
            Ok(list) => s.backups = Some(list),
            Err(error) => s.status = Status::error(error.to_string()),
        });
    }

    async fn make_backup(self) {
        let Some(server) = self.shared.session() else { return };
        let (generation, label) = {
            let s = self.lock();
            (s.generation, s.backup_label.clone())
        };
        self.with(generation, |s| {
            s.busy = true;
            s.status = Status::ok(t("Making a backup of the catalogue ...").into());
        });
        let made = catalog_admin::make_backup(&*server, &label).await;
        self.with(generation, |s| {
            s.busy = false;
            match made {
                Ok(file) => {
                    s.backup_label.clear();
                    s.status = Status::ok(tf("Backup {file} made.", &[("file", &file)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_backups().await;
    }

    /// Asks first, then puts the backup back (after a safety backup) and loads the pages again.
    async fn restore_backup(self, file: String) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let message = tf(
            "The catalogue pages, offers, front page and vouchers become what they were in {file}. A backup of the catalogue as it is now is made first, so this can be undone. Sold limited numbers and voucher uses stay as they are. Publish afterwards.",
            &[("file", &file)],
        );
        let options = vec![Choice::new(t("Roll back"), t("Put this backup back."), true).primary(), Choice::new(t("Cancel"), t("Keep the catalogue as it is."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), t("Roll the catalogue back?"), &message, false, options).await {
            return;
        }
        self.with(generation, |s| {
            s.busy = true;
            s.status = Status::ok(t("Rolling the catalogue back ...").into());
        });
        let restored = catalog_admin::restore_backup(&*server, &file).await;
        self.with(generation, |s| {
            s.busy = false;
            match restored {
                Ok(safety) => {
                    s.unpublished = true;
                    s.featured = None;
                    s.limited = None;
                    s.vouchers = None;
                    s.all_offers = None;
                    s.unoffered = None;
                    s.status = Status::ok(tf("Rolled back to {file} (the catalogue before is in {safety}). Publish to show it in the hotel.", &[("file", &file), ("safety", &safety)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_backups().await;
        self.load_pages(None).await;
    }

    async fn delete_backup(self, file: String) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let options = vec![Choice::new(t("Delete the backup"), t("Remove the file from the server."), true).primary(), Choice::new(t("Cancel"), t("Keep it."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &file)]), t("The backup file is deleted from the server."), false, options).await {
            return;
        }
        let deleted = catalog_admin::delete_backup(&*server, &file).await;
        self.with(generation, |s| {
            if let Err(error) = deleted {
                s.status = Status::error(error.to_string());
            }
        });
        self.load_backups().await;
    }

    async fn builder_find(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((prefix, only_new)) = self.lock().builder.as_ref().map(|b| (b.prefix.clone(), b.only_new)) else { return };
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
                (Err(error), _) | (_, Err(error)) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Adds the page, then an offer for each furni found on it.
    async fn builder_build(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(builder) = self.lock().builder.clone() else { return };
        let Some(found) = builder.found.clone().filter(|f| !f.is_empty()) else { return };
        self.with(generation, |s| s.busy = true);
        let page_id = match catalog_admin::save_page(&session, &database, &builder.page).await {
            Ok(id) => id,
            Err(error) => {
                self.with(generation, |s| {
                    s.busy = false;
                    s.status = Status::error(error.to_string());
                });
                return;
            }
        };
        let offers = catalog_admin::offers_for(&found, page_id, &[], builder.credits, builder.points, builder.points_type);
        let added = catalog_admin::add_offers(&session, &database, &offers).await;
        self.with(generation, |s| {
            s.busy = false;
            s.unpublished = true;
            match added {
                Ok(count) => {
                    s.builder = None;
                    s.status = Status::ok(tf("Page {page} made with {count} offers. It is hidden until you tick Visible; publish to put it in the hotel.", &[("page", &builder.page.caption), ("count", &count.to_string())]));
                }
                Err(error) => s.status = Status::error(tf("The page was made, but its offers were not: {error}", &[("error", &error.to_string())])),
            }
        });
        self.load_pages(Some(page_id)).await;
    }

    /// Asks first, then merges the pages into the first one.
    async fn merge_pages(self, target: i64, sources: Vec<i64>) {
        let Some((generation, session, database)) = self.session() else { return };
        let (problem, target_name) = {
            let s = self.lock();
            (catalog_admin::merge_problem(&s.pages, target, &sources), s.pages.iter().find(|p| p.id == target).map(|p| p.caption.clone()).unwrap_or_default())
        };
        if let Some(problem) = problem {
            self.with(generation, |s| s.status = Status::error(problem));
            return;
        }
        let others: Vec<String> = sources.iter().filter(|id| **id != target).map(|id| format!("#{id}")).collect();
        let message = tf(
            "The offers, sub-pages and front page tiles of {others} move to {page} #{id}, then those pages are deleted. Make a backup first if you may want them back.",
            &[("others", &others.join(", ")), ("page", &target_name), ("id", &target.to_string())],
        );
        let options = vec![Choice::new(t("Merge"), t("Move everything and delete the copies."), true).primary(), Choice::new(t("Cancel"), t("Keep the pages."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Merge into {page}?", &[("page", &target_name)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let merged = catalog_admin::merge_pages(&session, &database, target, &sources).await;
        self.with(generation, |s| {
            s.busy = false;
            match merged {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Merged into {page}. Publish to show it in the hotel.", &[("page", &target_name)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_checks().await;
    }

    async fn publish(self) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        self.with(generation, |s| {
            s.busy = true;
            s.status = Status::ok(t("Asking the emulator to load the catalogue again ...").into());
        });
        let result = catalog_admin::reload_catalog(&*server).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.unpublished = false;
                    s.status = Status::ok(t("Published: the emulator loaded the catalogue again and the players online were told to refresh it.").into());
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }
}

impl CatalogManagerView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())) }
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
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

enum Action {
    Login,
    Refresh,
    SelectPage(i64),
    SavePage,
    SaveOffer,
    DeletePage,
    DeleteOffer,
    LoadFeatured,
    SaveFeatured,
    LoadLimited,
    ChangeLimited(i64, LimitedAction),
    LoadVouchers,
    VoucherUses(i64),
    SaveVoucher,
    DeleteVoucher,
    GenerateVouchers,
    SearchFurni(String),
    FurniNames(Vec<i64>),
    LoadChecks,
    DeleteDuplicates,
    SortOffers,
    LoadUnoffered,
    AddUnoffered,
    LoadBackups,
    MakeBackup,
    RestoreBackup(String),
    DeleteBackup(String),
    BuilderFind,
    BuilderBuild,
    MergePages(i64, Vec<i64>),
    Publish,
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(10).corner_radius(10)
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

impl View for CatalogManagerView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connect_if_needed(shared);
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
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 18 }).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                header(ui, palette, s, shared, &mut actions);
                ui.add_space(10.0);
                body(ui, palette, s, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move { c.load_pages(None).await }),
                Action::SelectPage(id) => ctl.spawn(|c| async move { c.select_page(id).await }),
                Action::SavePage => ctl.spawn(|c| c.save_page()),
                Action::SaveOffer => ctl.spawn(|c| c.save_offer()),
                Action::DeletePage => ctl.spawn(|c| c.delete_page()),
                Action::DeleteOffer => ctl.spawn(|c| c.delete_offer()),
                Action::LoadFeatured => ctl.spawn(|c| async move { c.load_featured().await }),
                Action::SaveFeatured => ctl.spawn(|c| c.save_featured()),
                Action::LoadLimited => ctl.spawn(|c| async move { c.load_limited().await }),
                Action::ChangeLimited(offer_id, action) => ctl.spawn(move |c| c.change_limited(offer_id, action)),
                Action::LoadVouchers => ctl.spawn(|c| async move { c.load_vouchers(None).await }),
                Action::VoucherUses(id) => ctl.spawn(move |c| async move { c.load_voucher_uses(id).await }),
                Action::SaveVoucher => ctl.spawn(|c| c.save_voucher()),
                Action::DeleteVoucher => ctl.spawn(|c| c.delete_voucher()),
                Action::GenerateVouchers => ctl.spawn(|c| c.generate_vouchers()),
                Action::SearchFurni(text) => ctl.spawn(move |c| async move { c.search_furni(text).await }),
                Action::FurniNames(ids) => ctl.spawn(move |c| async move { c.furni_names(ids).await }),
                Action::LoadChecks => ctl.spawn(|c| async move { c.load_checks().await }),
                Action::DeleteDuplicates => ctl.spawn(|c| c.delete_duplicates()),
                Action::SortOffers => ctl.spawn(|c| c.sort_offers()),
                Action::LoadUnoffered => ctl.spawn(|c| async move { c.load_unoffered().await }),
                Action::AddUnoffered => ctl.spawn(|c| c.add_unoffered()),
                Action::LoadBackups => ctl.spawn(|c| async move { c.load_backups().await }),
                Action::MakeBackup => ctl.spawn(|c| c.make_backup()),
                Action::RestoreBackup(file) => ctl.spawn(move |c| c.restore_backup(file)),
                Action::DeleteBackup(file) => ctl.spawn(move |c| c.delete_backup(file)),
                Action::BuilderFind => ctl.spawn(|c| async move { c.builder_find().await }),
                Action::BuilderBuild => ctl.spawn(|c| c.builder_build()),
                Action::MergePages(target, sources) => ctl.spawn(move |c| c.merge_pages(target, sources)),
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

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            let ready = s.session.is_some() && !s.busy;
            let publish = if s.unpublished { t("Publish changes") } else { t("Publish") };
            if ui.add_enabled(ready, widgets::primary_button(publish)).on_hover_text(t("The emulator loads the catalogue again and tells the players online to refresh it.")).clicked() {
                actions.push(Action::Publish);
            }
            if ui.add_enabled(ready, small(t("Refresh"))).clicked() {
                actions.push(Action::Refresh);
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Catalogue")).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Pages, t("Pages")), (Tab::FrontPage, t("Front page")), (Tab::Limited, t("Limited")), (Tab::Vouchers, t("Vouchers")), (Tab::Checks, t("Checks")), (Tab::Unoffered, t("Unoffered")), (Tab::Backups, t("Backups"))] {
            if ui.selectable_label(s.tab == tab, RichText::new(label).size(15.0)).clicked() && s.tab != tab {
                s.tab = tab;
                if tab == Tab::FrontPage && s.featured.is_none() && s.session.is_some() {
                    actions.push(Action::LoadFeatured);
                }
                if tab == Tab::Limited && s.limited.is_none() && s.session.is_some() {
                    actions.push(Action::LoadLimited);
                }
                if tab == Tab::Vouchers && s.vouchers.is_none() && s.session.is_some() {
                    actions.push(Action::LoadVouchers);
                }
                if tab == Tab::Checks && s.all_offers.is_none() && s.session.is_some() {
                    actions.push(Action::LoadChecks);
                }
                if tab == Tab::Unoffered && s.unoffered.is_none() && s.session.is_some() {
                    actions.push(Action::LoadUnoffered);
                }
                if tab == Tab::Backups && s.backups.is_none() {
                    actions.push(Action::LoadBackups);
                }
            }
        }
    });
    ui.add_space(6.0);
    if s.tab == Tab::Backups {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-backups").auto_shrink([false, false]).show(ui, |ui| backups(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::Unoffered {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-unoffered").auto_shrink([false, false]).show(ui, |ui| unoffered(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::Checks {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-checks").auto_shrink([false, false]).show(ui, |ui| checks(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::Vouchers {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-vouchers").auto_shrink([false, false]).show(ui, |ui| vouchers(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::Limited {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-limited").auto_shrink([false, false]).show(ui, |ui| limited(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::FrontPage {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-front-page").auto_shrink([false, false]).show(ui, |ui| front_page(ui, palette, s, actions));
        });
        return;
    }
    egui::SidePanel::left("catalog-tree")
        .resizable(true)
        .default_width(300.0)
        .width_range(200.0..=600.0)
        .frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 6, top: 0, bottom: 0 }))
        .show_inside(ui, |ui| tree(ui, palette, s, actions));
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("catalog-editor").auto_shrink([false, false]).show(ui, |ui| editors(ui, palette, s, actions));
    });
}

fn tree(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let ready = s.session.is_some() && !s.busy;
        if ui.add_enabled(ready, small(t("New page"))).on_hover_text(t("A new page under the page shown, or at the top level.")).clicked() {
            let parent = s.page.as_ref().filter(|p| p.id > 0).map_or(-1, |p| p.id);
            let draft = catalog_admin::new_page(&s.pages, parent);
            s.page = Some(draft.clone());
            s.page_edit = Some(draft);
            s.offers.clear();
            s.offer = None;
            s.offer_edit = None;
            s.builder = None;
        }
        if ui.add_enabled(ready, small(t("Build page"))).on_hover_text(t("A new page with an offer for every furni of a furni line (class name prefix).")).clicked() {
            let parent = s.page.as_ref().filter(|p| p.id > 0).map_or(-1, |p| p.id);
            let page = catalog_admin::new_page(&s.pages, parent);
            s.builder = Some(Builder { page, prefix: String::new(), only_new: true, credits: 3, points: 0, points_type: 0, found: None });
        }
    });
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut s.search).hint_text(t("Search pages")).desired_width(f32::INFINITY));
    ui.add_space(6.0);
    let query = s.search.trim().to_lowercase();
    let selected = s.page.as_ref().map(|p| p.id);
    let mut select = None;
    egui::ScrollArea::both().id_salt("catalog-tree-scroll").auto_shrink([false, false]).show(ui, |ui| {
        for row in &s.rows {
            let page = &s.pages[row.index];
            let matches = query.is_empty() || page.caption.to_lowercase().contains(&query) || page.caption_save.to_lowercase().contains(&query) || page.id.to_string() == query;
            if !matches {
                continue;
            }
            let indent = if query.is_empty() { row.depth as f32 * 14.0 } else { 0.0 };
            let inner = ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.spacing_mut().item_spacing.x = 6.0;
                icons::show(ui, if row.depth == 0 { Icon::Folder } else { Icon::File }, 13.0, Fixed::brand());
                let color = if page.visible && page.enabled { palette.text } else { palette.subtle };
                ui.add(egui::Label::new(RichText::new(&page.caption).size(12.5).color(color)).selectable(false));
                ui.add(egui::Label::new(RichText::new(format!("#{}", page.id)).size(10.5).color(palette.subtle)).selectable(false));
            });
            let rect = inner.response.rect.expand(2.0);
            let response = ui.interact(rect, ui.id().with(("catalog-page", page.id)), egui::Sense::click());
            if selected == Some(page.id) || response.hovered() {
                let color = if selected == Some(page.id) { palette.selected } else { palette.hover };
                ui.painter().rect_filled(rect, 4.0, crate::gui::theme::with_alpha(color, 0x90));
            }
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                select = Some(page.id);
            }
        }
    });
    if let Some(id) = select {
        if selected != Some(id) {
            actions.push(Action::SelectPage(id));
        }
    }
}

/// A text field keeps its own width: in a grid, `desired_width` is capped at the column's width of the frame before, so
/// a field sharing a column with a small one (the slot number) stayed narrow.
const FIELD_HEIGHT: f32 = 26.0;

fn labelled(ui: &mut egui::Ui, palette: &Palette, label: &str) {
    ui.label(RichText::new(label).size(12.0).color(palette.muted));
}

fn number(ui: &mut egui::Ui, value: &mut i64, range: std::ops::RangeInclusive<i64>) {
    ui.add(egui::DragValue::new(value).range(range).speed(1));
}

fn editors(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    if s.builder.is_some() {
        builder(ui, palette, s, actions);
        return;
    }
    if s.page_edit.is_none() {
        ui.add_space(20.0);
        widgets::muted(ui, palette, t("Choose a page on the left."), 13.0);
        return;
    }
    let pages = s.pages.clone();
    let busy = s.busy;
    let is_new_page = s.page_edit.as_ref().is_some_and(|p| p.id <= 0);
    let page_changed = s.page_edit != s.page || is_new_page;
    let page_problem = s.page_edit.as_ref().and_then(|p| catalog_admin::page_problem(p, &pages));

    ui.label(widgets::title_text(if is_new_page { t("New page") } else { t("Page") }, 17.0));
    ui.add_space(4.0);
    if let Some(page) = s.page_edit.as_mut() {
        egui::Grid::new("catalog-page-form").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
            labelled(ui, palette, t("Name"));
            ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut page.caption));
            labelled(ui, palette, t("Internal name"));
            ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut page.caption_save));
            ui.end_row();

            labelled(ui, palette, t("Layout"));
            egui::ComboBox::from_id_salt("catalog-layout").selected_text(&page.layout).width(220.0).show_ui(ui, |ui| {
                for layout in LAYOUTS {
                    ui.selectable_value(&mut page.layout, layout.to_string(), *layout);
                }
            });
            labelled(ui, palette, t("Catalogue"));
            egui::ComboBox::from_id_salt("catalog-mode").selected_text(&page.catalog_mode).show_ui(ui, |ui| {
                for mode in CATALOG_MODES {
                    ui.selectable_value(&mut page.catalog_mode, mode.to_string(), *mode);
                }
            });
            ui.end_row();

            labelled(ui, palette, t("Parent page"));
            let parent_name = pages.iter().find(|p| p.id == page.parent_id).map(|p| format!("{} #{}", p.caption, p.id)).unwrap_or_else(|| t("(top level)").to_string());
            egui::ComboBox::from_id_salt("catalog-parent").selected_text(parent_name).width(220.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut page.parent_id, -1, t("(top level)"));
                for other in pages.iter().filter(|p| p.id != page.id && !catalog_admin::is_ancestor(&pages, page.id, p.id)) {
                    ui.selectable_value(&mut page.parent_id, other.id, format!("{} #{}", other.caption, other.id));
                }
            });
            labelled(ui, palette, t("Order"));
            number(ui, &mut page.order_num, -1000..=100_000);
            ui.end_row();

            labelled(ui, palette, t("Icon"));
            number(ui, &mut page.icon_image, 0..=100_000);
            labelled(ui, palette, t("Minimum rank"));
            number(ui, &mut page.min_rank, 0..=100);
            ui.end_row();

            labelled(ui, palette, t("Shown"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut page.visible, t("Visible"));
                ui.checkbox(&mut page.enabled, t("Enabled"));
                ui.checkbox(&mut page.club_only, t("Club only"));
            });
            ui.end_row();
        });
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(page_changed && page_problem.is_none() && !busy, widgets::primary_button(t("Save page"))).clicked() {
            actions.push(Action::SavePage);
        }
        if ui.add_enabled(page_changed && !busy, small(t("Undo changes"))).clicked() {
            if is_new_page {
                s.page = None;
                s.page_edit = None;
            } else {
                s.page_edit = s.page.clone();
            }
        }
        if !is_new_page && ui.add_enabled(!busy, small(t("Delete page"))).clicked() {
            actions.push(Action::DeletePage);
        }
        if let Some(problem) = &page_problem {
            ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
        } else if page_changed {
            ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.muted));
        }
    });

    if is_new_page || s.page_edit.is_none() {
        return;
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(&tf("Offers ({count})", &[("count", &s.offers.len().to_string())]), 17.0));
        if ui.add_enabled(!busy && s.offers.len() > 1, small(t("Sort by name"))).on_hover_text(t("Numbers the offers of this page 1, 2, ... in the order of their names.")).clicked() {
            actions.push(Action::SortOffers);
        }
        if ui.add_enabled(!busy, small(t("New offer"))).clicked() {
            if let Some(page) = s.page.as_ref() {
                let draft = catalog_admin::new_offer(&s.offers, page.id);
                s.offer = Some(draft.clone());
                s.offer_edit = Some(draft);
            }
        }
    });
    ui.add_space(4.0);
    offers_table(ui, palette, s);

    if s.offer_edit.is_some() {
        ui.add_space(14.0);
        offer_form(ui, palette, s, actions);
    }
}

fn offers_table(ui: &mut egui::Ui, palette: &Palette, s: &mut State) {
    if s.offers.is_empty() {
        widgets::muted(ui, palette, t("No offers on this page."), 12.5);
        return;
    }
    let selected = s.offer.as_ref().map(|o| o.id);
    let mut select = None;
    egui::Grid::new("catalog-offers").striped(true).num_columns(6).spacing([14.0, 4.0]).show(ui, |ui| {
        for heading in [t("Name"), t("Furni"), t("Credits"), t("Points"), t("Limited"), t("Order")] {
            ui.label(RichText::new(heading).size(11.5).color(palette.muted).strong());
        }
        ui.end_row();
        for offer in &s.offers {
            let name = if selected == Some(offer.id) { RichText::new(&offer.catalog_name).strong() } else { RichText::new(&offer.catalog_name) };
            if ui.add(egui::Label::new(name.size(12.5)).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                select = Some(offer.id);
            }
            ui.label(RichText::new(&offer.item_ids).size(12.0));
            ui.label(RichText::new(offer.cost_credits.to_string()).size(12.0));
            ui.label(RichText::new(if offer.cost_points > 0 { format!("{} ({})", offer.cost_points, offer.points_type) } else { "0".into() }).size(12.0));
            ui.label(RichText::new(if offer.limited_stack > 0 { format!("{} / {}", offer.limited_sells, offer.limited_stack) } else { "-".into() }).size(12.0));
            ui.label(RichText::new(offer.order_number.to_string()).size(12.0));
            ui.end_row();
        }
    });
    if let Some(id) = select {
        if let Some(offer) = s.offers.iter().find(|o| o.id == id).cloned() {
            s.offer = Some(offer.clone());
            s.offer_edit = Some(offer);
        }
    }
}

fn offer_form(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let is_new = s.offer_edit.as_ref().is_some_and(|o| o.id <= 0);
    let changed = s.offer_edit != s.offer || is_new;
    let problem = s.offer_edit.as_ref().and_then(catalog_admin::offer_problem);
    let pages = s.pages.clone();
    let Some(offer) = s.offer_edit.as_mut() else { return };

    let title = if is_new { t("New offer").to_string() } else { tf("Offer #{id}", &[("id", &offer.id.to_string())]) };
    ui.label(widgets::title_text(&title, 15.0));
    ui.add_space(4.0);
    egui::Grid::new("catalog-offer-form").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
        labelled(ui, palette, t("Name"));
        ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut offer.catalog_name));
        labelled(ui, palette, t("Furni ids"));
        ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut offer.item_ids)).on_hover_text(t("items_base ids: one id, or several as a bundle (100;200)."));
        ui.end_row();

        labelled(ui, palette, t("Credits"));
        number(ui, &mut offer.cost_credits, 0..=1_000_000_000);
        labelled(ui, palette, t("Points"));
        ui.horizontal(|ui| {
            number(ui, &mut offer.cost_points, 0..=1_000_000_000);
            ui.label(RichText::new(t("type")).size(12.0).color(palette.muted));
            number(ui, &mut offer.points_type, 0..=1000);
        });
        ui.end_row();

        labelled(ui, palette, t("Amount"));
        number(ui, &mut offer.amount, 1..=100_000);
        labelled(ui, palette, t("Limited size"));
        ui.horizontal(|ui| {
            number(ui, &mut offer.limited_stack, 0..=1_000_000);
            if offer.limited_stack > 0 {
                ui.label(RichText::new(tf("{sold} sold", &[("sold", &offer.limited_sells.to_string())])).size(12.0).color(palette.muted));
            }
        });
        ui.end_row();

        labelled(ui, palette, t("Order"));
        number(ui, &mut offer.order_number, -1000..=1_000_000);
        labelled(ui, palette, t("Offer id"));
        number(ui, &mut offer.offer_id, -1..=i32::MAX as i64);
        ui.end_row();

        labelled(ui, palette, t("Page"));
        let page_name = pages.iter().find(|p| p.id == offer.page_id).map(|p| format!("{} #{}", p.caption, p.id)).unwrap_or_else(|| format!("#{}", offer.page_id));
        egui::ComboBox::from_id_salt("catalog-offer-page").selected_text(page_name).width(220.0).show_ui(ui, |ui| {
            for page in &pages {
                ui.selectable_value(&mut offer.page_id, page.id, format!("{} #{}", page.caption, page.id));
            }
        });
        labelled(ui, palette, t("Extra data"));
        ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut offer.extradata));
        ui.end_row();

        labelled(ui, palette, t("Options"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut offer.club_only, t("Club only"));
            ui.checkbox(&mut offer.have_offer, t("Can be bought in bulk"));
        });
        ui.end_row();
    });
    furni_picker(ui, palette, &mut offer.item_ids, &mut s.furni_search, &s.furni_results, &s.furni_names, &mut s.furni_asked, busy, actions);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(changed && problem.is_none() && !busy, widgets::primary_button(t("Save offer"))).clicked() {
            actions.push(Action::SaveOffer);
        }
        if ui.add_enabled(changed && !busy, small(t("Undo changes"))).clicked() {
            if is_new {
                s.offer = None;
                s.offer_edit = None;
            } else {
                s.offer_edit = s.offer.clone();
            }
        }
        if !is_new && ui.add_enabled(!busy, small(t("Delete offer"))).clicked() {
            actions.push(Action::DeleteOffer);
        }
        if let Some(problem) = &problem {
            ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
        } else if changed {
            ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.muted));
        }
    });
}

fn front_page(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.label(widgets::title_text(t("Front page"), 17.0));
    widgets::muted(ui, palette, t("The promo tiles of the catalogue front page. Each opens a page (by internal name or id) or a product. The image is a path under the catalogue images (catalogue/...)."), 12.5);
    ui.add_space(8.0);
    let busy = s.busy;
    if ui.add_enabled(s.session.is_some() && !busy, small(t("Reload"))).clicked() {
        actions.push(Action::LoadFeatured);
    }
    ui.add_space(6.0);
    let saved = s.featured_saved.clone();
    let Some(rows) = s.featured.as_mut() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    let slots: Vec<Featured> = rows
        .iter()
        .map(|row| Featured { expire_timestamp: catalog_admin::parse_expiry(&row.expiry).unwrap_or(-2), ..row.slot.clone() })
        .collect();
    let bad_date = rows.iter().find(|row| catalog_admin::parse_expiry(&row.expiry).is_none()).map(|row| row.slot.slot_id);
    let problem = match bad_date {
        Some(id) => Some(tf("Slot {id}: the end date is not YYYY-MM-DD HH:MM.", &[("id", &id.to_string())])),
        None => catalog_admin::featured_problem(&slots),
    };
    let changed = *rows != saved;

    let mut remove = None;
    if rows.is_empty() {
        widgets::muted(ui, palette, t("No front page tiles yet."), 12.5);
    }
    for (index, row) in rows.iter_mut().enumerate() {
        widgets::card_frame(palette).inner_margin(8).corner_radius(8).show(ui, |ui| {
            egui::Grid::new(("catalog-featured", index)).num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                labelled(ui, palette, t("Slot"));
                number(ui, &mut row.slot.slot_id, 1..=99);
                labelled(ui, palette, t("Caption"));
                ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut row.slot.caption));
                ui.end_row();

                labelled(ui, palette, t("Image"));
                ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut row.slot.image).hint_text("catalogue/feature_cata/....png"));
                labelled(ui, palette, t("Opens"));
                egui::ComboBox::from_id_salt(("catalog-featured-type", index)).selected_text(&row.slot.kind).show_ui(ui, |ui| {
                    for kind in FEATURED_TYPES {
                        ui.selectable_value(&mut row.slot.kind, kind.to_string(), *kind);
                    }
                });
                ui.end_row();

                labelled(ui, palette, t("Target"));
                match row.slot.kind.as_str() {
                    "page_id" => number(ui, &mut row.slot.page_id, 0..=i32::MAX as i64),
                    "product_name" => {
                        ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut row.slot.product_name));
                    }
                    _ => {
                        ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut row.slot.page_name));
                    }
                }
                labelled(ui, palette, t("Ends"));
                ui.horizontal(|ui| {
                    ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut row.expiry).hint_text("YYYY-MM-DD HH:MM")).on_hover_text(t("Empty: it never ends. In the time zone of this computer."));
                    if ui.add_enabled(!busy, small(t("Remove"))).clicked() {
                        remove = Some(index);
                    }
                });
                ui.end_row();
            });
        });
        ui.add_space(6.0);
    }
    if let Some(index) = remove {
        rows.remove(index);
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy && rows.len() < catalog_admin::MAX_FEATURED_SLOTS, small(t("Add tile"))).clicked() {
            let slots: Vec<Featured> = rows.iter().map(|row| row.slot.clone()).collect();
            rows.push(FeaturedRow::new(catalog_admin::new_featured(&slots)));
        }
        if ui.add_enabled(changed && problem.is_none() && !busy, widgets::primary_button(t("Save front page"))).clicked() {
            actions.push(Action::SaveFeatured);
        }
        if ui.add_enabled(changed && !busy, small(t("Undo changes"))).clicked() {
            *rows = saved.clone();
        }
        if let Some(problem) = &problem {
            ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
        } else if changed {
            ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.muted));
        }
    });
}

fn limited(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.label(widgets::title_text(t("Limited series"), 17.0));
    widgets::muted(ui, palette, t("Every limited offer with how many are sold. A series only grows; one with no sales can end, one with sales can be removed with them (furni already bought keeps its number)."), 12.5);
    ui.add_space(6.0);
    let busy = s.busy;
    ui.horizontal(|ui| {
        if ui.add_enabled(s.session.is_some() && !busy, small(t("Reload"))).clicked() {
            actions.push(Action::LoadLimited);
        }
        ui.add_sized([280.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.limited_search).hint_text(t("Search by name, page or offer id")));
    });
    ui.add_space(6.0);
    let Some(list) = s.limited.as_ref() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    let query = s.limited_search.trim().to_lowercase();
    let shown: Vec<&LimitedSeries> = list
        .iter()
        .filter(|l| query.is_empty() || l.catalog_name.to_lowercase().contains(&query) || l.page_caption.to_lowercase().contains(&query) || l.offer_id.to_string() == query)
        .collect();
    if shown.is_empty() {
        widgets::muted(ui, palette, t("No limited series."), 12.5);
        return;
    }
    let sizes = &mut s.limited_sizes;
    egui::Grid::new("catalog-limited-grid").striped(true).num_columns(5).spacing([14.0, 6.0]).show(ui, |ui| {
        for heading in [t("Offer"), t("Page"), t("Sold"), t("Size"), ""] {
            ui.label(RichText::new(heading).size(11.5).color(palette.muted).strong());
        }
        ui.end_row();
        for series in shown {
            ui.label(RichText::new(format!("{}  #{}", series.catalog_name, series.offer_id)).size(12.5));
            ui.label(RichText::new(if series.page_caption.is_empty() { format!("#{}", series.page_id) } else { series.page_caption.clone() }).size(12.0));
            ui.label(RichText::new(format!("{} / {}", series.sold, series.stack)).size(12.0));
            let size = sizes.entry(series.offer_id).or_insert(series.stack.max(1));
            ui.add(egui::DragValue::new(size).range(1..=catalog_admin::MAX_LIMITED_STACK).speed(1));
            let size = *size;
            ui.horizontal(|ui| {
                let grow = LimitedAction::Grow(size);
                if ui.add_enabled(!busy && catalog_admin::limited_problem(series, grow).is_none(), small(t("Grow"))).clicked() {
                    actions.push(Action::ChangeLimited(series.offer_id, grow));
                }
                if series.sold == 0 {
                    if ui.add_enabled(!busy && series.stack > 0, small(t("No longer limited"))).clicked() {
                        actions.push(Action::ChangeLimited(series.offer_id, LimitedAction::End));
                    }
                } else if ui.add_enabled(!busy, small(t("Remove with its sales"))).clicked() {
                    actions.push(Action::ChangeLimited(series.offer_id, LimitedAction::RemoveWithSales));
                }
            });
            ui.end_row();
        }
    });
}

fn vouchers(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.label(widgets::title_text(t("Vouchers"), 17.0));
    widgets::muted(ui, palette, t("Codes players type in the catalogue to get credits, points or an offer. Uses: how many times in all (0 without end); per player: how many times one player (-1 without end)."), 12.5);
    ui.add_space(6.0);
    let busy = s.busy;
    let ready = s.session.is_some() && !busy;
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, small(t("Reload"))).clicked() {
            actions.push(Action::LoadVouchers);
        }
        if ui.add_enabled(ready && s.vouchers.is_some(), small(t("New voucher"))).clicked() {
            let draft = Voucher { credits: 100, amount: 1, limit: 1, ..Voucher::default() };
            s.voucher = Some(draft.clone());
            s.voucher_edit = Some(draft);
            s.voucher_uses.clear();
        }
        ui.add_sized([240.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.voucher_search).hint_text(t("Search codes")));
    });
    ui.add_space(6.0);
    let Some(list) = s.vouchers.clone() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };

    if s.voucher_edit.is_some() {
        voucher_form(ui, palette, s, &list, actions);
        ui.add_space(12.0);
    }

    let query = s.voucher_search.trim().to_lowercase();
    let shown: Vec<&Voucher> = list.iter().filter(|v| query.is_empty() || v.code.to_lowercase().contains(&query)).collect();
    if shown.is_empty() {
        widgets::muted(ui, palette, t("No vouchers."), 12.5);
        return;
    }
    let selected = s.voucher.as_ref().map(|v| v.id);
    let mut select = None;
    egui::Grid::new("catalog-vouchers-grid").striped(true).num_columns(5).spacing([14.0, 4.0]).show(ui, |ui| {
        for heading in [t("Code"), t("Gives"), t("Used"), t("Per player"), "#"] {
            ui.label(RichText::new(heading).size(11.5).color(palette.muted).strong());
        }
        ui.end_row();
        for voucher in shown {
            let code = if selected == Some(voucher.id) { RichText::new(&voucher.code).strong() } else { RichText::new(&voucher.code) };
            if ui.add(egui::Label::new(code.size(12.5).monospace()).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                select = Some(voucher.id);
            }
            let mut gives = Vec::new();
            if voucher.credits > 0 {
                gives.push(tf("{n} credits", &[("n", &voucher.credits.to_string())]));
            }
            if voucher.points > 0 {
                gives.push(tf("{n} points ({type})", &[("n", &voucher.points.to_string()), ("type", &voucher.points_type.to_string())]));
            }
            if voucher.catalog_item_id > 0 {
                gives.push(tf("offer #{id}", &[("id", &voucher.catalog_item_id.to_string())]));
            }
            ui.label(RichText::new(gives.join(", ")).size(12.0));
            let total = if voucher.amount > 0 { voucher.amount.to_string() } else { "∞".into() };
            ui.label(RichText::new(format!("{} / {}", voucher.used, total)).size(12.0));
            ui.label(RichText::new(if voucher.limit > 0 { voucher.limit.to_string() } else { "∞".into() }).size(12.0));
            ui.label(RichText::new(voucher.id.to_string()).size(11.5).color(palette.subtle));
            ui.end_row();
        }
    });
    if let Some(id) = select {
        if let Some(voucher) = list.iter().find(|v| v.id == id).cloned() {
            s.voucher = Some(voucher.clone());
            s.voucher_edit = Some(voucher);
            s.voucher_uses.clear();
            actions.push(Action::VoucherUses(id));
        }
    }
}

fn voucher_form(ui: &mut egui::Ui, palette: &Palette, s: &mut State, list: &[Voucher], actions: &mut Vec<Action>) {
    let busy = s.busy;
    let is_new = s.voucher_edit.as_ref().is_some_and(|v| v.id <= 0);
    let changed = s.voucher_edit != s.voucher || is_new;
    let problem = s.voucher_edit.as_ref().and_then(|v| catalog_admin::voucher_problem(v, list));
    let Some(code) = s.voucher.as_ref().map(|v| v.code.clone()) else { return };
    let title = if is_new { t("New voucher").to_string() } else { tf("Voucher {code}", &[("code", &code)]) };
    widgets::card_frame(palette).inner_margin(10).corner_radius(8).show(ui, |ui| {
        ui.label(widgets::title_text(&title, 15.0));
        ui.add_space(4.0);
        let Some(voucher) = s.voucher_edit.as_mut() else { return };
        egui::Grid::new("catalog-voucher-form").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
            labelled(ui, palette, t("Code"));
            ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut voucher.code).char_limit(catalog_admin::MAX_VOUCHER_CODE));
            labelled(ui, palette, t("Credits"));
            number(ui, &mut voucher.credits, 0..=1_000_000_000);
            ui.end_row();

            labelled(ui, palette, t("Points"));
            ui.horizontal(|ui| {
                number(ui, &mut voucher.points, 0..=1_000_000_000);
                ui.label(RichText::new(t("type")).size(12.0).color(palette.muted));
                number(ui, &mut voucher.points_type, 0..=1000);
            });
            labelled(ui, palette, t("Offer id"));
            number(ui, &mut voucher.catalog_item_id, 0..=i32::MAX as i64);
            ui.end_row();

            labelled(ui, palette, t("Uses"));
            number(ui, &mut voucher.amount, 0..=10_000_000);
            labelled(ui, palette, t("Per player"));
            number(ui, &mut voucher.limit, -1..=10_000);
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(changed && problem.is_none() && !busy, widgets::primary_button(t("Save voucher"))).clicked() {
                actions.push(Action::SaveVoucher);
            }
            if ui.add_enabled(changed && !busy, small(t("Undo changes"))).clicked() {
                if is_new {
                    s.voucher = None;
                    s.voucher_edit = None;
                } else {
                    s.voucher_edit = s.voucher.clone();
                }
            }
            if !is_new && ui.add_enabled(!busy, small(t("Delete voucher"))).clicked() {
                actions.push(Action::DeleteVoucher);
            }
            if let Some(problem) = &problem {
                ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
            } else if changed {
                ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.muted));
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new(t("Make many codes that give the same")).size(12.5).strong());
        if s.generate_length == 0 {
            s.generate_length = 10;
            s.generate_count = 10;
        }
        ui.horizontal(|ui| {
            labelled(ui, palette, t("Prefix"));
            ui.add_sized([90.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.generate_prefix).char_limit(6));
            labelled(ui, palette, t("Length"));
            ui.add(egui::DragValue::new(&mut s.generate_length).range(4..=catalog_admin::MAX_VOUCHER_CODE));
            labelled(ui, palette, t("How many"));
            ui.add(egui::DragValue::new(&mut s.generate_count).range(1..=catalog_admin::MAX_GENERATED_VOUCHERS));
            let gives = s.voucher_edit.as_ref().map(|v| Voucher { code: "X".into(), id: -1, ..v.clone() });
            let can = gives.is_some_and(|v| catalog_admin::voucher_problem(&v, &[]).is_none());
            if ui.add_enabled(can && !busy, small(t("Make codes"))).on_hover_text(t("Adds new vouchers with random codes, each giving what the form above shows.")).clicked() {
                actions.push(Action::GenerateVouchers);
            }
        });

        if !is_new {
            ui.add_space(8.0);
            ui.label(RichText::new(tf("Used by ({count})", &[("count", &s.voucher_uses.len().to_string())])).size(12.5).strong());
            if s.voucher_uses.is_empty() {
                widgets::muted(ui, palette, t("Nobody used it yet."), 12.0);
            }
            for usage in s.voucher_uses.iter().take(100) {
                let name = if usage.username.is_empty() { format!("#{}", usage.user_id) } else { usage.username.clone() };
                let when = catalog_admin::expiry_text(usage.timestamp);
                ui.label(RichText::new(format!("{name}  ·  {when}")).size(12.0));
            }
        }
    });
}

/// The offer's furni by name, each with a remove button, and a search of items_base to add more.
#[allow(clippy::too_many_arguments)]
fn furni_picker(
    ui: &mut egui::Ui,
    palette: &Palette,
    item_ids: &mut String,
    search: &mut String,
    results: &[Furni],
    names: &std::collections::HashMap<i64, Furni>,
    asked: &mut std::collections::HashSet<i64>,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let ids: Vec<i64> = item_ids.split(';').filter_map(|p| p.split(':').next()?.trim().parse::<i64>().ok()).collect();
    let missing: Vec<i64> = ids.iter().copied().filter(|id| !names.contains_key(id) && !asked.contains(id)).collect();
    if !missing.is_empty() {
        asked.extend(missing.iter().copied());
        actions.push(Action::FurniNames(missing));
    }
    let describe = |furni: &Furni| {
        if furni.public_name.is_empty() { format!("{} #{}", furni.item_name, furni.id) } else { format!("{} ({}) #{}", furni.public_name, furni.item_name, furni.id) }
    };

    ui.add_space(6.0);
    labelled(ui, palette, t("Furni in this offer"));
    let mut remove = None;
    if ids.is_empty() {
        widgets::muted(ui, palette, t("None yet: search below and add one."), 12.0);
    }
    for id in &ids {
        ui.horizontal(|ui| {
            let text = names.get(id).map(describe).unwrap_or_else(|| tf("#{id} (not in items_base?)", &[("id", &id.to_string())]));
            ui.label(RichText::new(text).size(12.0));
            if ui.add_enabled(!busy, egui::Button::new(RichText::new("✕").size(11.0)).small()).on_hover_text(t("Take it out of the offer")).clicked() {
                remove = Some(*id);
            }
        });
    }
    if let Some(id) = remove {
        *item_ids = catalog_admin::remove_furni_id(item_ids, id);
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let response = ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(search).hint_text(t("Search furni by name, class name or id")));
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(!busy, small(t("Search"))).clicked() || enter) && !search.trim().is_empty() {
            actions.push(Action::SearchFurni(search.clone()));
        }
    });
    let mut add = None;
    egui::ScrollArea::vertical().id_salt("catalog-furni-results").max_height(180.0).show(ui, |ui| {
        for furni in results {
            ui.horizontal(|ui| {
                let already = ids.contains(&furni.id);
                if ui.add_enabled(!busy && !already, egui::Button::new(RichText::new(t("Add")).size(11.5)).small()).clicked() {
                    add = Some(furni.id);
                }
                ui.label(RichText::new(describe(furni)).size(12.0));
                ui.label(RichText::new(&furni.kind).size(11.0).color(palette.subtle));
            });
        }
    });
    if let Some(id) = add {
        *item_ids = catalog_admin::add_furni_id(item_ids, id);
    }
}

fn checks(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Checks"), 17.0));
        if ui.add_enabled(s.session.is_some() && !busy, small(t("Check again"))).clicked() {
            actions.push(Action::LoadChecks);
        }
    });
    let Some(offers) = s.all_offers.clone() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    let page_name = |id: i64| s.pages.iter().find(|p| p.id == id).map(|p| format!("{} #{}", p.caption, p.id)).unwrap_or_else(|| format!("#{id}"));

    ui.add_space(6.0);
    let page_groups = catalog_admin::duplicate_page_groups(&s.pages);
    ui.label(RichText::new(tf("Pages with the same internal name ({count})", &[("count", &page_groups.len().to_string())])).size(14.0).strong());
    widgets::muted(ui, palette, t("The client opens a page by its internal name: of these, links reach only one. Give each its own name."), 12.0);
    let mut open = None;
    for group in &page_groups {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&s.pages[group[0]].caption_save).size(12.5).monospace());
            let target = s.pages[group[0]].id;
            let ids: Vec<i64> = group.iter().map(|i| s.pages[*i].id).collect();
            if ui.add_enabled(!busy, small(&tf("Merge into #{id}", &[("id", &target.to_string())]))).on_hover_text(t("Move the offers, sub-pages and front page tiles of the others to the first, then delete the others.")).clicked() {
                actions.push(Action::MergePages(target, ids));
            }
            for index in group {
                let page = &s.pages[*index];
                if ui.link(format!("{} #{}", page.caption, page.id)).clicked() {
                    open = Some(page.id);
                }
            }
        });
    }
    if page_groups.is_empty() {
        widgets::muted(ui, palette, t("None."), 12.0);
    }

    ui.add_space(12.0);
    let groups = catalog_admin::duplicate_offer_groups(&offers);
    ui.label(RichText::new(tf("Offers that sell the same furni ({count})", &[("count", &groups.len().to_string())])).size(14.0).strong());
    widgets::muted(ui, palette, t("Tick the copies to delete; offers with sold limited numbers are kept. Same furni with other extra data (a poster, a trophy text) is not counted."), 12.0);
    let ticked_count = offers.iter().filter(|o| s.duplicates_ticked.contains(&o.id)).count();
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy && ticked_count > 0, widgets::primary_button(&tf("Delete ticked ({count})", &[("count", &ticked_count.to_string())]))).clicked() {
            actions.push(Action::DeleteDuplicates);
        }
        if ui.add_enabled(!busy && !groups.is_empty(), small(t("Tick all but the first of each"))).clicked() {
            s.duplicates_ticked = groups
                .iter()
                .flat_map(|g| g.iter().skip(1))
                .map(|i| &offers[*i])
                .filter(|o| catalog_admin::offer_delete_problem(o).is_none())
                .map(|o| o.id)
                .collect();
        }
        if ui.add_enabled(!busy && ticked_count > 0, small(t("Untick all"))).clicked() {
            s.duplicates_ticked.clear();
        }
    });
    if groups.is_empty() {
        widgets::muted(ui, palette, t("None."), 12.0);
    }
    for (number, group) in groups.iter().enumerate().take(300) {
        widgets::card_frame(palette).inner_margin(6).corner_radius(6).show(ui, |ui| {
            for index in group {
                let offer = &offers[*index];
                ui.horizontal(|ui| {
                    let mut ticked = s.duplicates_ticked.contains(&offer.id);
                    let can = catalog_admin::offer_delete_problem(offer).is_none();
                    if ui.add_enabled(can && !busy, egui::Checkbox::without_text(&mut ticked)).changed() {
                        if ticked {
                            s.duplicates_ticked.insert(offer.id);
                        } else {
                            s.duplicates_ticked.remove(&offer.id);
                        }
                    }
                    ui.label(RichText::new(format!("{}  #{}", offer.catalog_name, offer.id)).size(12.5));
                    ui.label(RichText::new(format!("[{}]", offer.item_ids)).size(11.5).color(palette.subtle));
                    if ui.link(page_name(offer.page_id)).clicked() {
                        open = Some(offer.page_id);
                    }
                    ui.label(RichText::new(tf("{n} credits", &[("n", &offer.cost_credits.to_string())])).size(11.5).color(palette.muted));
                    if !can {
                        ui.label(RichText::new(t("limited, sold")).size(11.5).color(palette.subtle));
                    }
                });
            }
        });
        if number + 1 == 300 {
            widgets::muted(ui, palette, t("Only the first 300 groups are shown."), 12.0);
        }
        ui.add_space(4.0);
    }
    if let Some(id) = open {
        s.tab = Tab::Pages;
        actions.push(Action::SelectPage(id));
    }
}

const UNOFFERED_SHOWN: usize = 500;

fn unoffered(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Unoffered furni"), 17.0));
        if ui.add_enabled(s.session.is_some() && !busy, small(t("Check again"))).clicked() {
            actions.push(Action::LoadUnoffered);
        }
    });
    widgets::muted(ui, palette, t("Floor and wall furni in items_base that no offer sells. Tick them and add an offer for each to a page."), 12.5);
    let Some(list) = s.unoffered.clone() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    ui.add_space(6.0);

    let query = s.unoffered_search.trim().to_lowercase();
    let shown: Vec<&Furni> = list
        .iter()
        .filter(|f| query.is_empty() || f.item_name.to_lowercase().contains(&query) || f.public_name.to_lowercase().contains(&query) || f.id.to_string() == query)
        .collect();
    let ticked = list.iter().filter(|f| s.unoffered_ticked.contains(&f.id)).count();

    egui::Grid::new("catalog-unoffered-form").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
        labelled(ui, palette, t("Add to page"));
        let page_name = s.pages.iter().find(|p| p.id == s.unoffered_page).map(|p| format!("{} #{}", p.caption, p.id)).unwrap_or_else(|| t("(choose a page)").to_string());
        egui::ComboBox::from_id_salt("catalog-unoffered-page").selected_text(page_name).width(260.0).show_ui(ui, |ui| {
            for page in &s.pages {
                ui.selectable_value(&mut s.unoffered_page, page.id, format!("{} #{}", page.caption, page.id));
            }
        });
        labelled(ui, palette, t("Credits"));
        number(ui, &mut s.unoffered_credits, 0..=1_000_000_000);
        ui.end_row();
        labelled(ui, palette, t("Points"));
        ui.horizontal(|ui| {
            number(ui, &mut s.unoffered_points, 0..=1_000_000_000);
            ui.label(RichText::new(t("type")).size(12.0).color(palette.muted));
            number(ui, &mut s.unoffered_points_type, 0..=1000);
        });
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let page_ok = s.pages.iter().any(|p| p.id == s.unoffered_page);
        if ui.add_enabled(!busy && ticked > 0 && page_ok, widgets::primary_button(&tf("Add {count} offers", &[("count", &ticked.to_string())]))).clicked() {
            actions.push(Action::AddUnoffered);
        }
        if ui.add_enabled(!busy && !shown.is_empty(), small(t("Tick all shown"))).clicked() {
            s.unoffered_ticked.extend(shown.iter().take(UNOFFERED_SHOWN).map(|f| f.id));
        }
        if ui.add_enabled(!busy && ticked > 0, small(t("Untick all"))).clicked() {
            s.unoffered_ticked.clear();
        }
        ui.add_sized([240.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.unoffered_search).hint_text(t("Search by name, class name or id")));
    });
    ui.add_space(4.0);
    ui.label(RichText::new(tf("{count} furni without an offer", &[("count", &list.len().to_string())])).size(12.5).strong());
    if shown.len() > UNOFFERED_SHOWN {
        widgets::muted(ui, palette, &tf("The first {count} are shown: search to narrow them down.", &[("count", &UNOFFERED_SHOWN.to_string())]), 12.0);
    }
    for furni in shown.iter().take(UNOFFERED_SHOWN) {
        ui.horizontal(|ui| {
            let mut on = s.unoffered_ticked.contains(&furni.id);
            if ui.add_enabled(!busy, egui::Checkbox::without_text(&mut on)).changed() {
                if on {
                    s.unoffered_ticked.insert(furni.id);
                } else {
                    s.unoffered_ticked.remove(&furni.id);
                }
            }
            ui.label(RichText::new(&furni.item_name).size(12.5));
            if !furni.public_name.is_empty() {
                ui.label(RichText::new(&furni.public_name).size(12.0).color(palette.muted));
            }
            ui.label(RichText::new(format!("#{} · {}", furni.id, furni.kind)).size(11.0).color(palette.subtle));
        });
    }
}

fn backups(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Backups"), 17.0));
        if ui.add_enabled(!busy, small(t("Reload"))).clicked() {
            actions.push(Action::LoadBackups);
        }
    });
    widgets::muted(
        ui,
        palette,
        &tf("Copies of the catalogue (pages, offers, front page, vouchers) on the server, in {folder}. Make one before a big change; Roll back puts one back after a backup of the catalogue as it is then. Sold limited numbers and voucher uses are never part of it.", &[("folder", &catalog_admin::backup_folder())]),
        12.5,
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_sized([240.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.backup_label).hint_text(t("Name (optional), e.g. before-summer-sale")).char_limit(40));
        if ui.add_enabled(!busy, widgets::primary_button(t("Make backup"))).clicked() {
            actions.push(Action::MakeBackup);
        }
    });
    ui.add_space(8.0);
    let Some(list) = s.backups.as_ref() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    if list.is_empty() {
        widgets::muted(ui, palette, t("No catalogue backups yet."), 12.5);
        return;
    }
    egui::Grid::new("catalog-backups-grid").striped(true).num_columns(4).spacing([14.0, 6.0]).show(ui, |ui| {
        for heading in [t("Backup"), t("Made"), t("Size"), ""] {
            ui.label(RichText::new(heading).size(11.5).color(palette.muted).strong());
        }
        ui.end_row();
        for backup in list {
            ui.label(RichText::new(&backup.file).size(12.0).monospace());
            ui.label(RichText::new(catalog_admin::expiry_text(backup.made)).size(12.0));
            ui.label(RichText::new(format!("{:.1} KB", backup.bytes as f64 / 1024.0)).size(12.0));
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, small(t("Roll back"))).clicked() {
                    actions.push(Action::RestoreBackup(backup.file.clone()));
                }
                if ui.add_enabled(!busy, small(t("Delete"))).clicked() {
                    actions.push(Action::DeleteBackup(backup.file.clone()));
                }
            });
            ui.end_row();
        }
    });
}

fn builder(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let pages = s.pages.clone();
    let Some(builder) = s.builder.as_mut() else { return };
    ui.label(widgets::title_text(t("Build page"), 17.0));
    widgets::muted(ui, palette, t("A new page with an offer for every floor and wall furni whose class name starts with the prefix (a furni line, like rare_dragonlamp)."), 12.5);
    ui.add_space(6.0);
    egui::Grid::new("catalog-builder").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
        labelled(ui, palette, t("Name"));
        ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut builder.page.caption));
        labelled(ui, palette, t("Internal name"));
        ui.add_sized([160.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut builder.page.caption_save));
        ui.end_row();

        labelled(ui, palette, t("Parent page"));
        let parent_name = pages.iter().find(|p| p.id == builder.page.parent_id).map(|p| format!("{} #{}", p.caption, p.id)).unwrap_or_else(|| t("(top level)").to_string());
        egui::ComboBox::from_id_salt("catalog-builder-parent").selected_text(parent_name).width(220.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut builder.page.parent_id, -1, t("(top level)"));
            for page in &pages {
                ui.selectable_value(&mut builder.page.parent_id, page.id, format!("{} #{}", page.caption, page.id));
            }
        });
        labelled(ui, palette, t("Layout"));
        egui::ComboBox::from_id_salt("catalog-builder-layout").selected_text(&builder.page.layout).width(160.0).show_ui(ui, |ui| {
            for layout in LAYOUTS {
                ui.selectable_value(&mut builder.page.layout, layout.to_string(), *layout);
            }
        });
        ui.end_row();

        labelled(ui, palette, t("Class name starts with"));
        ui.horizontal(|ui| {
            let response = ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut builder.prefix).hint_text("rare_dragonlamp"));
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!busy && builder.prefix.trim().chars().count() >= 3, small(t("Find furni"))).clicked() || enter) && builder.prefix.trim().chars().count() >= 3 {
                actions.push(Action::BuilderFind);
            }
        });
        ui.checkbox(&mut builder.only_new, t("Only furni no offer sells yet"));
        ui.end_row();

        labelled(ui, palette, t("Credits"));
        number(ui, &mut builder.credits, 0..=1_000_000_000);
        labelled(ui, palette, t("Points"));
        ui.horizontal(|ui| {
            number(ui, &mut builder.points, 0..=1_000_000_000);
            ui.label(RichText::new(t("type")).size(12.0).color(palette.muted));
            number(ui, &mut builder.points_type, 0..=1000);
        });
        ui.end_row();
    });
    ui.add_space(6.0);
    let problem = catalog_admin::page_problem(&builder.page, &pages);
    let count = builder.found.as_ref().map_or(0, Vec::len);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy && problem.is_none() && count > 0, widgets::primary_button(&tf("Build page with {count} offers", &[("count", &count.to_string())]))).clicked() {
            actions.push(Action::BuilderBuild);
        }
        if ui.add_enabled(!busy, small(t("Cancel"))).clicked() {
            s.builder = None;
        }
        if let Some(problem) = &problem {
            ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
        }
    });
    let Some(builder) = s.builder.as_ref() else { return };
    if let Some(found) = &builder.found {
        ui.add_space(6.0);
        if found.is_empty() {
            widgets::muted(ui, palette, t("No furni found for this prefix."), 12.5);
        }
        for furni in found.iter().take(300) {
            ui.label(RichText::new(format!("{}  #{}  {}", furni.item_name, furni.id, furni.public_name)).size(12.0));
        }
        if found.len() > 300 {
            widgets::muted(ui, palette, &tf("and {count} more", &[("count", &(found.len() - 300).to_string())]), 12.0);
        }
    }
}
