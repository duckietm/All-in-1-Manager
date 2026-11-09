//! The catalogue manager: what the Catalogue explorer does not do - the front page slots, limited series, vouchers,
//! checks (duplicate offers, merging pages) and catalogue backups - in the hotel database through the SSH session (the
//! SQL Editor's tunnel and MariaDB login). Pages and offers are edited in the Catalogue explorer.
//! **Publish** makes the running emulator load the catalogue again (RCON `updatecatalog`), which tells the players
//! online to refresh it. The data and its rules live in `aio_remote::catalog_admin`.

use std::sync::{Arc, Mutex};

mod front_page;

use aio_remote::catalog_admin::{self, CatalogBackup, Catalogue, Featured, LimitedAction, LimitedSeries, Offer, Page, Voucher, VoucherUse};
use aio_remote::catalog_health::{self, Kind};
use aio_remote::catalog_journal;
use aio_remote::catalog_transfer;
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};
use front_page::{FrontHeader, FrontImages, FrontState};

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
    FrontPage,
    Limited,
    Vouchers,
    Checks,
    History,
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

/// The preview of an SQL import (Octane's Catalog Studio format).
struct Transfer {
    file: String,
    current: std::collections::BTreeMap<catalog_journal::Entity, serde_json::Value>,
    changes: Vec<catalog_journal::Change>,
}

#[derive(Default)]
struct State {
    tab: Tab,
    /// The front page slots as loaded, and as edited (None until the tab is opened).
    featured: Option<Vec<FeaturedRow>>,
    featured_saved: Vec<FeaturedRow>,
    /// The graphical front page: the tile edited, the image picker, the server's images and the page's header.
    front: FrontState,
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
    /// The Checks tab: every offer (None until it is run) and the duplicate offers ticked for deleting.
    all_offers: Option<Vec<Offer>>,
    /// The health check of both catalogues (None until it is run).
    health: Option<Vec<catalog_health::Problem>>,
    duplicates_ticked: std::collections::HashSet<i64>,
    /// The Backups tab: the backups on the server (None until it is opened) and the label for a new one.
    backups: Option<Vec<CatalogBackup>>,
    /// The History tab: the latest operations (None until it is opened; Some(None) when the database has no history).
    history: Option<Option<Vec<catalog_journal::Operation>>>,
    backup_label: String,
    /// An SQL file being imported: its name, the catalogue as it was at the preview, and what it changes.
    transfer: Option<Transfer>,
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

}

pub struct CatalogManagerView {
    state: Arc<Mutex<State>>,
    images: FrontImages,
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
        self.load_pages().await;
        let tab = self.lock().tab;
        match tab {
            Tab::FrontPage => self.load_featured().await,
            Tab::Limited => self.load_limited().await,
            Tab::Vouchers => self.load_vouchers(None).await,
            Tab::Checks => self.load_checks().await,
            Tab::History => self.load_history().await,
            Tab::Backups => self.load_backups().await,
        }
    }

    async fn load_pages(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_pages(&session, &database, Catalogue::Normal).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(pages) => {
                    s.status = Status::ok(tf("{count} pages.", &[("count", &pages.len().to_string())]));
                    s.pages = pages;
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    async fn load_featured(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = catalog_admin::load_featured(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(slots) => {
                    let mut rows: Vec<FeaturedRow> = slots.into_iter().map(FeaturedRow::new).collect();
                    rows.sort_by_key(|row| row.slot.slot_id);
                    s.featured_saved = rows.clone();
                    s.featured = Some(rows);
                    s.front.selected = None;
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_front_header(generation, &session, &database).await;
    }

    /// The front page's own row: its caption, icon, headline image and the voucher box's text (page_text1).
    async fn load_front_header(&self, generation: u64, session: &SqlSession, database: &str) {
        let page = front_page::front_page_of(&self.lock().pages).cloned();
        let Some(page) = page else {
            self.with(generation, |s| s.front.header = None);
            return;
        };
        let row = catalog_admin::load_row(session, database, "catalog_pages", page.id).await.unwrap_or_default();
        let value = |name: &str| row.iter().find(|(column, _)| column == name).and_then(|(_, v)| v.clone()).unwrap_or_default();
        let header = FrontHeader { caption: page.caption.clone(), icon: page.icon_image, headline: value("page_headline"), text1: value("page_text1") };
        self.with(generation, |s| s.front.header = Some(header));
    }

    /// The image files under the server's c_images, for the picker.
    async fn list_front_images(self) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let result = server.run_as_root(&front_page::list_command()).await;
        self.with(generation, |s| {
            s.front.listing = false;
            s.front.server_files = Some(match result {
                Ok(result) => result.output.lines().map(str::trim).filter_map(front_page::clean_path).collect(),
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    Vec::new()
                }
            });
        });
    }

    /// Fetches these files under the server's c_images for their preview, one after the other.
    async fn fetch_front_images(self, files: Vec<String>) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        let folder = format!("{}/c_images", aio_remote::octane::gamedata());
        for file in files {
            let bytes = match front_page::clean_path(&file) {
                Some(path) => server.read_file(&format!("{folder}/{path}"), 8 << 20).await.ok().flatten().map(Arc::new),
                None => None,
            };
            if self
                .with(generation, |s| {
                    s.front.fetching.remove(&file);
                    s.front.fetched.insert(file.clone(), bytes);
                })
                .is_none()
            {
                return;
            }
        }
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

    async fn load_checks(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let offers = catalog_admin::load_all_offers(&session, &database, Catalogue::Normal).await;
        let pages = catalog_admin::load_pages(&session, &database, Catalogue::Normal).await;
        let health = catalog_health::check(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match health {
                Ok(found) => s.health = Some(found),
                Err(error) => s.status = Status::error(error.to_string()),
            }
            match (offers, pages) {
                (Ok(offers), Ok(pages)) => {
                    s.pages = pages;
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
        let deleted = catalog_admin::delete_offers(&session, &database, Catalogue::Normal, &ids).await;
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
                    s.status = Status::ok(tf("Rolled back to {file} (the catalogue before is in {safety}). Publish to show it in the hotel.", &[("file", &file), ("safety", &safety)]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_backups().await;
        self.load_pages().await;
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

    /// The latest operations of the catalogue's shared history (Octane's Catalog Studio and this Manager).
    async fn load_history(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let history = if catalog_journal::has_history(&session, &database).await { catalog_journal::load_history(&session, &database, 150).await.map(Some) } else { Ok(None) };
        self.with(generation, |s| {
            s.busy = false;
            match history {
                Ok(history) => s.history = Some(history),
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Asks first, then undoes one operation of the history, when its pages and offers are still as it left them.
    async fn undo_operation(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let operation = self.lock().history.clone().flatten().and_then(|h| h.into_iter().find(|o| o.id == id));
        let Some(operation) = operation else { return };
        match catalog_journal::undo_problem(&session, &database, &operation).await {
            Ok(None) => {}
            Ok(Some(problem)) => {
                self.with(generation, |s| s.status = Status::error(tf("Operation #{id} cannot be undone: {problem}", &[("id", &id.to_string()), ("problem", &problem)])));
                return;
            }
            Err(error) => {
                self.with(generation, |s| s.status = Status::error(error.to_string()));
                return;
            }
        }
        let options = vec![Choice::new(t("Undo it"), t("Put every page and offer it changed back as it was before."), true).primary(), Choice::new(t("Cancel"), t("Leave it."), false)];
        let message = tf("{count} pages and offers go back to how they were before operation #{id}. The undo is recorded in the history too.", &[("count", &operation.entries.len().to_string()), ("id", &id.to_string())]);
        if !self.shared.dialogs.choose(t("Undo operation"), &operation.summary, &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let result = catalog_journal::undo(&session, &database, &operation).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Operation #{id} undone. Publish to show it in the hotel.", &[("id", &id.to_string())]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_history().await;
    }

    /// Saves the whole catalogue (both catalogues) as an SQL file in Octane's Catalog Studio format.
    async fn export_catalogue(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(file) = rfd::AsyncFileDialog::new().set_title(t("Export the catalogue")).add_filter(t("SQL files"), &["sql"]).set_file_name("catalog-studio.sql").save_file().await else {
            return;
        };
        self.with(generation, |s| s.busy = true);
        let text = catalog_transfer::export(&session, &database).await;
        let written = match text {
            Ok(text) => tokio::fs::write(file.path(), text).await.map_err(|e| e.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let name = file.path().display().to_string();
        self.with(generation, |s| {
            s.busy = false;
            s.status = match written {
                Ok(()) => Status::ok(tf("The catalogue is saved in {file}.", &[("file", &name)])),
                Err(error) => Status::error(error),
            };
        });
    }

    /// Reads an SQL file (Octane's Catalog Studio format) and shows what it would change; nothing is written yet.
    async fn import_catalogue(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(file) = rfd::AsyncFileDialog::new().set_title(t("Import a catalogue file")).add_filter(t("SQL files"), &["sql", "txt"]).add_filter(t("All files"), &["*"]).pick_file().await else {
            return;
        };
        let name = file.file_name();
        let text = match tokio::fs::read(file.path()).await {
            Ok(bytes) => String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes)).into_owned(),
            Err(error) => {
                self.with(generation, |s| s.status = Status::error(error.to_string()));
                return;
            }
        };
        self.with(generation, |s| {
            s.busy = true;
            s.transfer = None;
        });
        let preview = catalog_transfer::preview(&session, &database, &text).await;
        self.with(generation, |s| {
            s.busy = false;
            match preview {
                Ok((current, changes)) => {
                    s.status = Status::ok(tf("{file}: {count} changes. Check them, then Apply.", &[("file", &name), ("count", &changes.len().to_string())]));
                    s.transfer = Some(Transfer { file: name, current, changes });
                }
                Err(error) => s.status = Status::error(tf("{file} cannot be imported: {error}", &[("file", &name), ("error", &error.to_string())])),
            }
        });
    }

    /// Asks first, then writes the previewed import.
    async fn apply_import(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((count, file)) = self.lock().transfer.as_ref().map(|t| (t.changes.len(), t.file.clone())) else { return };
        let options = vec![Choice::new(t("Apply"), t("Write the changes to the hotel database."), true).primary(), Choice::new(t("Cancel"), t("Change nothing."), false)];
        let message = tf("{count} pages and offers are created, changed or deleted as {file} says, in one go. It is recorded in the history, so it can be undone there. Make a backup first if you may want the whole catalogue back.", &[("count", &count.to_string()), ("file", &file)]);
        if !self.shared.dialogs.choose(t("Import the catalogue"), &file, &message, false, options).await {
            return;
        }
        let Some(transfer) = self.lock().transfer.take() else { return };
        self.with(generation, |s| s.busy = true);
        let result = catalog_transfer::apply(&session, &database, &transfer.current, &transfer.changes).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.unpublished = true;
                    s.history = None;
                    s.status = Status::ok(tf("{count} changes of {file} are written. Publish to show them in the hotel.", &[("count", &count.to_string()), ("file", &file)]));
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    s.transfer = Some(transfer);
                }
            }
        });
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
        Self { state: Arc::new(Mutex::new(State::default())), images: FrontImages::new() }
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
            // The screenshot of the page shows a sample front page: the dry run has no database.
            if server.is_dry_run() && std::env::var_os("AIO_SCREENSHOT").is_some() {
                sample(&mut state);
                return;
            }
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

enum Action {
    Login,
    Refresh,
    LoadFeatured,
    SaveFeatured,
    ListFrontImages,
    FetchFrontImages(Vec<String>),
    LoadLimited,
    ChangeLimited(i64, LimitedAction),
    LoadVouchers,
    VoucherUses(i64),
    SaveVoucher,
    DeleteVoucher,
    GenerateVouchers,
    LoadChecks,
    DeleteDuplicates,
    LoadBackups,
    LoadHistory,
    ExportCatalogue,
    ImportCatalogue,
    ApplyImport,
    UndoOperation(i64),
    MakeBackup,
    RestoreBackup(String),
    DeleteBackup(String),
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
        self.images.new_frame();
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
                body(ui, palette, s, &mut self.images, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move { c.load_pages().await }),
                Action::LoadFeatured => ctl.spawn(|c| async move { c.load_featured().await }),
                Action::SaveFeatured => ctl.spawn(|c| c.save_featured()),
                Action::ListFrontImages => ctl.spawn(|c| c.list_front_images()),
                Action::FetchFrontImages(files) => ctl.spawn(move |c| c.fetch_front_images(files)),
                Action::LoadLimited => ctl.spawn(|c| async move { c.load_limited().await }),
                Action::ChangeLimited(offer_id, action) => ctl.spawn(move |c| c.change_limited(offer_id, action)),
                Action::LoadVouchers => ctl.spawn(|c| async move { c.load_vouchers(None).await }),
                Action::VoucherUses(id) => ctl.spawn(move |c| async move { c.load_voucher_uses(id).await }),
                Action::SaveVoucher => ctl.spawn(|c| c.save_voucher()),
                Action::DeleteVoucher => ctl.spawn(|c| c.delete_voucher()),
                Action::GenerateVouchers => ctl.spawn(|c| c.generate_vouchers()),
                Action::LoadChecks => ctl.spawn(|c| async move { c.load_checks().await }),
                Action::DeleteDuplicates => ctl.spawn(|c| c.delete_duplicates()),
                Action::LoadBackups => ctl.spawn(|c| async move { c.load_backups().await }),
                Action::LoadHistory => ctl.spawn(|c| async move { c.load_history().await }),
                Action::ExportCatalogue => ctl.spawn(|c| c.export_catalogue()),
                Action::ImportCatalogue => ctl.spawn(|c| c.import_catalogue()),
                Action::ApplyImport => ctl.spawn(|c| c.apply_import()),
                Action::UndoOperation(id) => ctl.spawn(move |c| c.undo_operation(id)),
                Action::MakeBackup => ctl.spawn(|c| c.make_backup()),
                Action::RestoreBackup(file) => ctl.spawn(move |c| c.restore_backup(file)),
                Action::DeleteBackup(file) => ctl.spawn(move |c| c.delete_backup(file)),
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
            if ui.add(widgets::secondary_button(t("Pages and offers →"))).on_hover_text(t("Pages and offers are edited in the Catalogue explorer: as the game shows them, with drag and drop.")).clicked() {
                shared.request(Request::Go(AppPage::CatalogExplorer));
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

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::FrontPage, t("Front page")), (Tab::Limited, t("Limited")), (Tab::Vouchers, t("Vouchers")), (Tab::Checks, t("Checks")), (Tab::History, t("History")), (Tab::Backups, t("Backups"))] {
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
                if tab == Tab::History && s.history.is_none() && s.session.is_some() {
                    actions.push(Action::LoadHistory);
                }
                if tab == Tab::Backups && s.backups.is_none() {
                    actions.push(Action::LoadBackups);
                }
            }
        }
    });
    ui.add_space(6.0);
    if s.tab == Tab::History {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-history").auto_shrink([false, false]).show(ui, |ui| history(ui, palette, s, actions));
        });
        return;
    }
    if s.tab == Tab::Backups {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("catalog-backups").auto_shrink([false, false]).show(ui, |ui| backups(ui, palette, s, actions));
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
    // The front page slots (the first tab). Pages and offers are edited in the Catalogue explorer.
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("catalog-front-page").auto_shrink([false, false]).show(ui, |ui| front_page::show(ui, palette, s, images, actions));
    });
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
    health(ui, palette, s.health.as_deref().unwrap_or(&[]), s.health.is_some());
    ui.add_space(12.0);
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
                    // Open it in the Catalogue explorer (its tree has a search on the name or id).
                    ui.label(RichText::new(page_name(offer.page_id)).size(11.5));
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
}

fn backups(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    transfer(ui, palette, s, actions);
    ui.add_space(14.0);
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

/// The screenshot's front page (`AIO_SCREENSHOT_PAGE=catalogmanager`): four tiles, the second selected.
fn sample(s: &mut State) {
    let page = |id: i64, caption: &str, save: &str, layout: &str| Page { id, parent_id: -1, caption: caption.into(), caption_save: save.into(), layout: layout.into(), icon_image: 1, visible: true, enabled: true, ..Page::default() };
    s.pages = vec![page(1, "Front page", "frontpage", "frontpage4"), page(2, "Furni", "furni", "default_3x3"), page(3, "HC", "hc_membership", "vip_buy"), page(4, "Clothes", "clothes", "default_3x3")];
    let slot = |slot_id: i64, caption: &str, image: &str, page_name: &str, expire_timestamp: i64| {
        FeaturedRow::new(Featured { slot_id, caption: caption.into(), image: image.into(), kind: "page_name".into(), expire_timestamp, page_name: page_name.into(), ..Featured::default() })
    };
    let rows = vec![
        slot(1, "New clothes", "catalogue/feature_cata_vert_rainbow.png", "clothes", -1),
        slot(2, "Become a member!", "catalogue/feature_cata_hort_hc.png", "hc_membership", -1),
        slot(3, "Give HC as a gift", "catalogue/feature_cata_hort_gift.png", "hc_membership", 1_600_000_000),
        slot(4, "Hotel furni", "catalogue/feature_cata_hort_furni.png", "furni", -1),
    ];
    s.featured_saved = rows.clone();
    s.featured = Some(rows);
    s.front.sample = true;
    s.front.header = Some(FrontHeader { caption: "Front page".into(), icon: 1, headline: "catalog_frontpage_headline_shop_EN".into(), text1: "Redeem a <b>voucher</b> code here:".into() });
    if std::env::var_os("AIO_SCREENSHOT_EDITOR").is_some() {
        s.front.selected = Some(2);
    }
    if std::env::var_os("AIO_SCREENSHOT_PICKER").is_some() {
        s.front.selected = Some(1);
        s.front.picker = Some(String::new());
    }
    s.subtitle = "The catalogue of hotel on example.org, through the SSH tunnel.".into();
}

/// What a health problem is, as a heading.
fn kind_title(kind: Kind) -> &'static str {
    match kind {
        Kind::OfferPageMissing => t("Offers on a page that does not exist"),
        Kind::OfferItemInvalid => t("Offers with wrong furni ids"),
        Kind::OfferItemMissing => t("Offers with furni that is not in items_base"),
        Kind::OfferSongMissing => t("Song offers without their song"),
        Kind::OfferPriceInvalid => t("Offers with a negative price or no amount"),
        Kind::OfferCurrencyUnknown => t("Offers in a currency the hotel does not use"),
        Kind::OfferLimitedBelowSales => t("Limited series smaller than what is sold"),
        Kind::PageParentMissing => t("Pages under a page that does not exist"),
        Kind::PageCycle => t("Pages in a loop"),
        Kind::PageLayoutUnknown => t("Pages with a layout the emulator does not know"),
        Kind::PageOrderTwice => t("Pages that share their order number"),
        Kind::PageIncludeInvalid => t("Pages with wrong includes"),
        Kind::PageIncludeMissing => t("Pages that include a page that does not exist"),
        Kind::PageUnreachable => t("Visible pages players never reach"),
    }
}

/// What is wrong with this one, in words.
fn problem_text(problem: &catalog_health::Problem) -> String {
    let v = problem.value.as_str();
    match problem.kind {
        Kind::OfferPageMissing => tf("Its page {page} does not exist.", &[("page", v)]),
        Kind::OfferItemInvalid if v.is_empty() => t("It has no furni ids.").into(),
        Kind::OfferItemInvalid => tf("\"{token}\" is not a furni id: write 123 or 123:2, separated by ;.", &[("token", v)]),
        Kind::OfferItemMissing => tf("Furni {id} is not in items_base.", &[("id", v)]),
        Kind::OfferSongMissing => t("A song offer needs a song id and the soundtrack code in its extra data.").into(),
        Kind::OfferPriceInvalid => t("Credits and points cannot be negative, and the amount is at least 1.").into(),
        Kind::OfferCurrencyUnknown => tf("Points type {type} is not a currency of this hotel.", &[("type", v)]),
        Kind::OfferLimitedBelowSales => {
            let (size, sold) = v.split_once('/').unwrap_or((v, ""));
            tf("Limited size {size}, but {sold} are sold.", &[("size", size), ("sold", sold)])
        }
        Kind::PageParentMissing => tf("Its parent page {page} does not exist.", &[("page", v)]),
        Kind::PageCycle => t("The page tree goes round in a loop here: give one of these pages another parent.").into(),
        Kind::PageLayoutUnknown => tf("Layout {layout}: the emulator skips the page.", &[("layout", v)]),
        Kind::PageOrderTwice => tf("Order number {order} is used more than once under the same parent.", &[("order", v)]),
        Kind::PageIncludeInvalid => tf("\"{token}\" in includes is not a page id.", &[("token", v)]),
        Kind::PageIncludeMissing => tf("It includes page {page}, which does not exist.", &[("page", v)]),
        Kind::PageUnreachable => tf("Page {page} above it is hidden.", &[("page", v)]),
    }
}

/// The health check: the problems the emulator's own catalogue check finds, in both catalogues.
fn health(ui: &mut egui::Ui, palette: &Palette, problems: &[catalog_health::Problem], ran: bool) {
    ui.label(RichText::new(tf("Health check ({count})", &[("count", &problems.len().to_string())])).size(14.0).strong());
    widgets::muted(
        ui,
        palette,
        t("What the emulator's catalogue check (Catalog Studio in Octane) finds, in the catalogue and the Builders Club warehouse. Fix them in the Catalogue explorer or the Builders Club explorer (right-click, Edit)."),
        12.0,
    );
    if !ran {
        widgets::muted(ui, palette, t("Loading ..."), 12.0);
        return;
    }
    if problems.is_empty() {
        ui.label(RichText::new(t("No problems found.")).size(12.5).color(palette.success_text));
        return;
    }
    let mut kinds: Vec<Kind> = problems.iter().map(|p| p.kind).collect();
    kinds.sort();
    kinds.dedup();
    for kind in kinds {
        let these: Vec<&catalog_health::Problem> = problems.iter().filter(|p| p.kind == kind).collect();
        egui::CollapsingHeader::new(RichText::new(format!("{} ({})", kind_title(kind), these.len())).size(13.0).strong())
            .id_salt(("catalog-health", kind.code()))
            .default_open(these.len() <= 8)
            .show(ui, |ui| {
                ui.label(RichText::new(kind.code()).size(11.0).monospace().color(palette.muted));
                for problem in these.iter().take(200) {
                    ui.horizontal_wrapped(|ui| {
                        if problem.cat == Catalogue::Builders {
                            ui.label(RichText::new("BC").size(11.0).strong().color(egui::Color32::from_rgb(226, 132, 10)));
                        }
                        let what = if kind.is_page() { t("Page") } else { t("Offer") };
                        ui.label(RichText::new(format!("{what} #{} {}", problem.id, problem.name)).size(12.5).strong());
                        ui.label(RichText::new(problem_text(problem)).size(12.5));
                    });
                }
                if these.len() > 200 {
                    widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(these.len() - 200).to_string())]), 12.0);
                }
            });
    }
}

/// The History tab: every operation on pages and offers, by Octane's Catalog Studio and this Manager, with undo.
fn history(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("History"), 17.0));
        if ui.add_enabled(s.session.is_some() && !busy, small(t("Reload"))).clicked() {
            actions.push(Action::LoadHistory);
        }
    });
    widgets::muted(
        ui,
        palette,
        t("Every change to pages and offers, made in Octane's Catalog Studio or here (the explorers, Checks, Limited), newest first: the same history Octane shows. Undo puts what one operation changed back, when nothing changed those pages and offers since. Front page tiles, vouchers and rolled back backups are not part of it."),
        12.0,
    );
    ui.add_space(6.0);
    let Some(history) = &s.history else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    let Some(history) = history else {
        widgets::muted(ui, palette, t("This hotel's database has no catalogue history (catalog_manager_history): the emulator is older than Catalog Studio."), 12.5);
        return;
    };
    if history.is_empty() {
        widgets::muted(ui, palette, t("No changes recorded yet."), 12.5);
        return;
    }
    for operation in history {
        widgets::card_frame(palette).inner_margin(8).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("#{}", operation.revision)).size(12.5).monospace().color(palette.muted));
                ui.label(RichText::new(&operation.summary).size(13.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy && !operation.entries.is_empty(), small(t("Undo ..."))).clicked() {
                        actions.push(Action::UndoOperation(operation.id));
                    }
                });
            });
            let who = if operation.actor.is_empty() { t("no player (All-in-1 Manager or a script)").to_string() } else { operation.actor.clone() };
            ui.label(RichText::new(format!("{} · {} · {}", operation.created_at, who, operation.source)).size(11.5).color(palette.muted));
            egui::CollapsingHeader::new(RichText::new(tf("{count} pages and offers", &[("count", &operation.entries.len().to_string())])).size(12.0))
                .id_salt(("catalog-history-op", operation.id))
                .show(ui, |ui| {
                    for change in operation.entries.iter().take(100) {
                        ui.horizontal_wrapped(|ui| {
                            if change.entity.builders {
                                ui.label(RichText::new("BC").size(11.0).strong().color(egui::Color32::from_rgb(226, 132, 10)));
                            }
                            let what = if change.entity.page { t("Page") } else { t("Offer") };
                            ui.label(RichText::new(format!("{what} #{} {}", change.entity.id, change.name())).size(12.0).strong());
                            ui.label(RichText::new(&change.operation).size(11.0).monospace().color(palette.muted));
                            if change.operation == "UPDATE" || change.operation == "MOVE" {
                                ui.label(RichText::new(change.fields().join(", ")).size(11.5).color(palette.muted));
                            }
                        });
                    }
                    if operation.entries.len() > 100 {
                        widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(operation.entries.len() - 100).to_string())]), 11.5);
                    }
                });
        });
        ui.add_space(4.0);
    }
}

/// Export and import of the whole catalogue as one SQL file, in the format of Octane's Catalog Studio.
fn transfer(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let ready = s.session.is_some() && !busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Export and import"), 17.0));
        if ui.add_enabled(ready, small(t("Export to a file ..."))).clicked() {
            actions.push(Action::ExportCatalogue);
        }
        if ui.add_enabled(ready, small(t("Import a file ..."))).clicked() {
            actions.push(Action::ImportCatalogue);
        }
    });
    widgets::muted(
        ui,
        palette,
        t("The pages and offers of the catalogue and the Builders Club warehouse as one SQL file, in the format of Octane's Catalog Studio: a file exported there can be imported here and the other way round. An import is never run as SQL: only INSERT, UPDATE and DELETE of catalog_pages and catalog_items with plain values are read, and you first see what would change."),
        12.0,
    );
    let mut cancel = false;
    if let Some(preview) = &s.transfer {
        ui.add_space(6.0);
        widgets::card_frame(palette).inner_margin(10).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let count = |op: &str| preview.changes.iter().filter(|c| c.operation == op).count();
            ui.label(RichText::new(&preview.file).size(13.5).strong());
            ui.label(
                RichText::new(tf(
                    "{created} new, {changed} changed, {moved} moved, {deleted} deleted.",
                    &[("created", &count("CREATE").to_string()), ("changed", &count("UPDATE").to_string()), ("moved", &count("MOVE").to_string()), ("deleted", &count("DELETE").to_string())],
                ))
                .size(12.5),
            );
            egui::ScrollArea::vertical().id_salt("catalog-import-preview").max_height(260.0).show(ui, |ui| {
                for change in preview.changes.iter().take(400) {
                    ui.horizontal_wrapped(|ui| {
                        if change.entity.builders {
                            ui.label(RichText::new("BC").size(11.0).strong().color(egui::Color32::from_rgb(226, 132, 10)));
                        }
                        let what = if change.entity.page { t("Page") } else { t("Offer") };
                        ui.label(RichText::new(format!("{what} #{} {}", change.entity.id, change.name())).size(12.0).strong());
                        ui.label(RichText::new(&change.operation).size(11.0).monospace().color(palette.muted));
                        if change.operation == "UPDATE" || change.operation == "MOVE" {
                            ui.label(RichText::new(change.fields().join(", ")).size(11.5).color(palette.muted));
                        }
                    });
                }
                if preview.changes.len() > 400 {
                    widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(preview.changes.len() - 400).to_string())]), 11.5);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = tf("Apply {count} changes", &[("count", &preview.changes.len().to_string())]);
                if ui.add_enabled(!busy && !preview.changes.is_empty(), widgets::primary_button(&label)).clicked() {
                    actions.push(Action::ApplyImport);
                }
                if ui.add_enabled(!busy, small(t("Cancel"))).clicked() {
                    cancel = true;
                }
                if preview.changes.is_empty() {
                    ui.label(RichText::new(t("The file changes nothing: the catalogue is already like this.")).size(12.0).color(palette.muted));
                }
            });
        });
    }
    if cancel {
        s.transfer = None;
    }
}
