//! The Plugins page's "Available" tab: the plugins of the signed index (see [`crate::plugins::index`]), with a
//! search box, each with Install, "Update available" or "Installed", and the index's status line with "Check again".
//!
//! An install or an update runs download → [`external::ExternalPlugins::prepare_verified`] → the consent window
//! (with its green "Verified by the official index" line) → [`external::ExternalPlugins::commit`], which checks the
//! index again. The network and disk work runs off the window's thread; each entry shows how far it got.
//!
//! What the tab shows is built by pure functions ([`rows`], [`button`]), tested below; the drawing is not. Every
//! string the index supplies goes through `display_safe`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use aio_plugin_host::{display_safe, Manifest, Permission};
use aio_plugin_index::{Freshness, Index};
use chrono::{DateTime, Utc};
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{n, t, tf};
use crate::plugins::external;
use crate::plugins::index::{self, IndexSource, IndexState, NOT_SET_UP};

/// Install and Update wait for a current index.
pub const OUT_OF_DATE: &str = n("The index is out of date");

/// What an entry's button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// This plugin is installed and the index offers nothing newer for it.
    Installed,
    /// The index offers a newer version of the installed plugin (same author, not revoked).
    Update { version: String },
    Install,
    /// The version the index lists is revoked by the same index (reason already `display_safe`).
    Revoked { reason: String },
}

/// One entry of the Available tab, every text already safe to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// The plain-words sentence of each permission, and whether it is a sensitive one.
    pub permissions: Vec<(String, bool)>,
    pub repo: String,
    pub commit: String,
    pub action: Action,
    /// Why Install / Update cannot be used now (an index that is out of date).
    pub blocked: Option<&'static str>,
}

/// The plain-words sentence of a permission string of the index (its raw text, made safe, if it does not parse).
fn permission_line(raw: &str) -> (String, bool) {
    match Permission::parse(raw) {
        Ok(p) => (display_safe(&external::permission_text(&p), 300), external::is_sensitive(&p)),
        Err(_) => (display_safe(raw, 120), false),
    }
}

/// The entries of `index` matching `query` (in the name, author or description, ignoring case), in the index's order.
/// `installed` are the installed plugins (not developer folders); `can_install` is false when the index may not be
/// installed from (out of date).
pub fn rows(index: &Index, installed: &[Manifest], can_install: bool, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    index
        .plugins
        .iter()
        .map(|entry| {
            let action = match installed.iter().find(|m| m.id == entry.id) {
                Some(manifest) => match external::offered_update(index, manifest) {
                    Some(update) => Action::Update { version: display_safe(&update.version, 40) },
                    None => Action::Installed,
                },
                None => match index.revocation_for(&entry.id, &entry.version) {
                    Some(revoked) => Action::Revoked { reason: display_safe(&revoked.reason, 300) },
                    None => Action::Install,
                },
            };
            let blocked = (!can_install && matches!(action, Action::Install | Action::Update { .. })).then_some(OUT_OF_DATE);
            Row {
                id: entry.id.clone(),
                name: display_safe(&entry.name, 60),
                version: display_safe(&entry.version, 40),
                author: display_safe(&entry.author, 60),
                description: display_safe(&entry.description, 300),
                permissions: entry.permissions.iter().map(|p| permission_line(p)).collect(),
                repo: display_safe(&entry.source.repo, 200),
                commit: display_safe(&entry.source.commit, 64),
                action,
                blocked,
            }
        })
        .filter(|row| query.is_empty() || [&row.name, &row.author, &row.description].iter().any(|s| s.to_lowercase().contains(&query)))
        .collect()
}

/// How far an install or update from the index got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Downloading,
    Checking,
    /// Why it stopped (already safe to show).
    Failed(String),
}

impl Job {
    fn running(&self) -> bool {
        matches!(self, Job::Downloading | Job::Checking)
    }
}

/// What an entry's button looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    /// The main (green) button rather than a secondary one.
    pub primary: bool,
    pub enabled: bool,
    pub hint: Option<String>,
}

/// The button of `row`, given its job and whether anything else is running or waiting for consent (`busy`).
pub fn button(row: &Row, job: Option<&Job>, busy: bool) -> Button {
    let (label, primary) = match &row.action {
        Action::Installed => (t("Installed").to_string(), false),
        Action::Update { version } => (tf("Update available ({version})", &[("version", version)]), true),
        Action::Install => (t("Install").to_string(), true),
        Action::Revoked { .. } => (t("Revoked").to_string(), false),
    };
    let hint = match (&row.action, row.blocked) {
        (Action::Revoked { reason }, _) => Some(reason.clone()),
        (_, Some(blocked)) => Some(t(blocked).to_string()),
        _ => None,
    };
    let actionable = matches!(row.action, Action::Install | Action::Update { .. });
    let enabled = actionable && row.blocked.is_none() && !busy && !job.is_some_and(Job::running);
    Button { label, primary, enabled, hint }
}

/// "5 min ago", "3 h ago", "12 days ago".
pub fn age_text(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - then).num_minutes().max(0);
    if minutes < 1 {
        t("just now").to_string()
    } else if minutes < 60 {
        tf("{count} min ago", &[("count", &minutes.to_string())])
    } else if minutes < 48 * 60 {
        tf("{count} h ago", &[("count", &(minutes / 60).to_string())])
    } else {
        tf("{count} days ago", &[("count", &(minutes / (24 * 60)).to_string())])
    }
}

/// The error of the last check: its fixed text translated, the whole made safe (it is sanitised at the source too).
pub fn error_text(error: &str) -> String {
    display_safe(t(error), 300)
}

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static STATE: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The job of plugin `id`, for its card on the Installed tab too.
pub fn job(id: &str) -> Option<Job> {
    jobs().lock().get(id).cloned()
}

/// True while a download or a check runs, or a consent waits: one install from the index at a time.
pub fn busy() -> bool {
    jobs().lock().values().any(Job::running) || super::plugins::consent_open()
}

/// True while the index is being checked.
fn checking() -> &'static AtomicBool {
    static STATE: AtomicBool = AtomicBool::new(false);
    &STATE
}

/// Fetches the index again in the background ("Check again", and once at startup); its revocations are applied and
/// the window repaints when it is done. `cached_first` applies the revocations of the cached index before the fetch.
pub fn start_check(shared: &Shared, cached_first: bool) {
    if checking().swap(true, Ordering::AcqRel) {
        return;
    }
    shared.spawn(async move {
        if cached_first {
            // Loading the cache reads and verifies files: not on the window's thread.
            let _ = tokio::task::spawn_blocking(index::apply_cached_revocations).await;
        }
        index::refresh(&index::index_client()).await;
        checking().store(false, Ordering::Release);
    });
}

/// Installs or updates plugin `id` from the index: download, check, then the consent window. An update takes the
/// version [`external::ExternalPlugins::update_for`] offers.
pub fn start_install(shared: &Shared, id: &str) {
    if busy() {
        return;
    }
    let Some(signed) = index::signed_index_for_install() else {
        shared.set_status(t(OUT_OF_DATE));
        return;
    };
    let installed = external::plugins().list().iter().any(|i| !i.dev && i.manifest.id == id);
    let entry = if installed { external::plugins().update_for(id) } else { signed.index.entry(id).cloned() };
    let Some(entry) = entry else { return };
    jobs().lock().insert(id.to_string(), Job::Downloading);
    let (status, ctx, id) = (shared.clone(), shared.ctx.clone(), id.to_string());
    shared.spawn(async move {
        let fail = |error: String| {
            jobs().lock().insert(id.clone(), Job::Failed(error));
            status.set_status(&tf("{plugin} could not be installed.", &[("plugin", &display_safe(&entry.name, 80))]));
        };
        let client = external::package_client(signed.source);
        let bytes = match external::download(&client, &entry, signed.source).await {
            Ok(bytes) => bytes,
            Err(error) => return fail(display_safe(&format!("{error:#}"), 300)),
        };
        jobs().lock().insert(id.clone(), Job::Checking);
        ctx.request_repaint();
        let checked_entry = entry.clone();
        match tokio::task::spawn_blocking(move || external::plugins().prepare_verified(&signed, &checked_entry, bytes)).await {
            Ok(Ok(pending)) => {
                jobs().lock().remove(&id);
                if !super::plugins::ask_install(pending) {
                    status.set_status(t("Another plugin waits for your answer: try again after it."));
                }
            }
            Ok(Err(error)) => fail(display_safe(&format!("{error:#}"), 300)),
            Err(error) => fail(display_safe(&error.to_string(), 300)),
        }
    });
}

/// The state of a job under an entry: progress, or why it failed.
pub fn job_line(ui: &mut egui::Ui, palette: &Palette, job: Option<&Job>) {
    match job {
        Some(Job::Downloading) => {
            ui.label(RichText::new(t("Downloading…")).size(12.5).color(palette.muted));
            widgets::indeterminate_bar(ui, 6.0, palette.muted);
        }
        Some(Job::Checking) => {
            ui.label(RichText::new(t("Checking the package against the index…")).size(12.5).color(palette.muted));
            widgets::indeterminate_bar(ui, 6.0, palette.muted);
        }
        Some(Job::Failed(error)) => widgets::result_banner(ui, palette, ResultKind::Error, t("It could not be installed"), error),
        None => {}
    }
}

/// The Available tab.
#[derive(Default)]
pub struct PluginIndexView {
    query: String,
}

impl PluginIndexView {
    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let state = index::index_state();
        status_line(ui, palette, shared, &state);
        let Some(index) = &state.index else { return };
        ui.add_space(10.0);
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text(RichText::new(t("Search by name, author or description")).italics()).desired_width(320.0));
        ui.add_space(10.0);
        let installed: Vec<Manifest> = external::plugins().list().into_iter().filter(|i| !i.dev).map(|i| i.manifest).collect();
        let developer_mode = external::plugins().developer_enabled();
        let can_install = index::installable(&state, developer_mode).is_some();
        let list = rows(index, &installed, can_install, &self.query);
        if list.is_empty() {
            widgets::muted(ui, palette, if index.plugins.is_empty() { t("The index lists no plugin yet.") } else { t("No plugin matches the search.") }, 12.5);
        }
        let busy = busy();
        for row in list {
            entry_card(ui, palette, shared, &row, busy);
            ui.add_space(10.0);
        }
    }
}

fn status_line(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, state: &IndexState) {
    let not_set_up = state.index.is_none() && state.error.as_deref() == Some(NOT_SET_UP);
    if not_set_up {
        widgets::result_banner(ui, palette, ResultKind::Info, t(NOT_SET_UP), t("Plugins can still be installed from a file on the Installed tab."));
        return;
    }
    let now = Utc::now();
    ui.horizontal_wrapped(|ui| {
        match &state.index {
            Some(index) => {
                let text = tf("Index {sequence}, published {age}", &[("sequence", &index.sequence.to_string()), ("age", &age_text(index.generated, now))]);
                ui.label(RichText::new(text).size(12.5).color(palette.muted));
                if let Some(checked) = state.checked {
                    ui.label(RichText::new(tf("· checked {age}", &[("age", &age_text(checked, now))])).size(12.5).color(palette.muted));
                }
            }
            None => {
                ui.label(RichText::new(t("The plugin index has not been fetched yet.")).size(12.5).color(palette.muted));
            }
        }
        if state.source == IndexSource::Test {
            ui.label(RichText::new(t("Test index (developer mode)")).size(12.5).strong().color(palette.warning_text));
        }
        if checking().load(Ordering::Acquire) {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new(t("Checking…")).size(12.5).color(palette.muted));
        } else if ui.add(widgets::secondary_button(t("Check again"))).on_hover_text(t("Fetch the plugin index again")).clicked() {
            start_check(shared, false);
        }
    });
    if state.freshness == Some(Freshness::Expired) {
        ui.add_space(6.0);
        widgets::result_banner(
            ui,
            palette,
            ResultKind::Warning,
            t(OUT_OF_DATE),
            t("Nothing can be installed or updated from it until a newer index is fetched. What it revokes stays revoked."),
        );
    }
    if let Some(error) = &state.error {
        ui.add_space(6.0);
        widgets::result_banner(ui, palette, ResultKind::Error, t("The plugin index could not be checked"), &error_text(error));
    }
}

fn entry_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, row: &Row, busy: bool) {
    let job = job(&row.id);
    widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(widgets::title_text(&row.name, 16.0));
                    ui.label(RichText::new(format!("{} · {}", row.version, row.author)).size(12.5).color(palette.muted));
                });
                widgets::muted(ui, palette, &row.description, 12.5);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let look = button(row, job.as_ref(), busy);
                let widget = if look.primary { widgets::primary_button(&look.label) } else { widgets::secondary_button(&look.label) };
                let mut response = ui.add_enabled(look.enabled, widget);
                if let Some(hint) = &look.hint {
                    response = response.on_disabled_hover_text(hint.as_str());
                }
                if response.clicked() {
                    start_install(shared, &row.id);
                }
            });
        });
        if let Action::Revoked { reason } = &row.action {
            ui.label(RichText::new(tf("Revoked: {reason}", &[("reason", reason)])).size(12.5).color(palette.error_text));
        }
        job_line(ui, palette, job.as_ref());
        egui::CollapsingHeader::new(RichText::new(tf("Permissions ({count})", &[("count", &row.permissions.len().to_string())])).size(12.5))
            .id_salt(("index-permissions", &row.id))
            .show(ui, |ui| {
                if row.permissions.is_empty() {
                    widgets::muted(ui, palette, t("It asks for no permission."), 12.5);
                }
                for (text, sensitive) in &row.permissions {
                    let color = if *sensitive { palette.warning_text } else { palette.muted };
                    ui.label(RichText::new(format!("• {text}")).size(12.5).color(color));
                }
            });
        ui.label(RichText::new(tf("Source: {repo} at {commit}", &[("repo", &row.repo), ("commit", &row.commit)])).size(11.5).color(palette.subtle));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use aio_plugin_index::{Entry, Revoked, Source};

    fn entry(id: &str, version: &str, author: &str) -> Entry {
        Entry {
            id: id.into(),
            name: format!("{id} plugin"),
            version: version.into(),
            author: author.into(),
            description: "Does things".into(),
            api: "1.0".into(),
            permissions: vec!["ssh".into(), "sql:read-private:users.mail".into()],
            size: 10,
            sha256: "0".repeat(64),
            url: format!("{}{id}-{version}/{id}.aiop", index::OFFICIAL_PACKAGE_PREFIX),
            source: Source { repo: "https://github.com/someone/demo".into(), commit: "0123456789abcdef0123456789abcdef01234567".into() },
        }
    }

    fn index_of(plugins: Vec<Entry>, revoked: Vec<Revoked>) -> Index {
        Index {
            schema: 1,
            sequence: 3,
            generated: "2026-11-13T10:00:00Z".parse().unwrap(),
            expires: "2026-12-13T10:00:00Z".parse().unwrap(),
            plugins,
            revoked,
        }
    }

    fn manifest(id: &str, version: &str, author: &str) -> Manifest {
        let json = serde_json::json!({
            "id": id, "name": "Demo", "version": version, "author": author, "description": "A demo",
            "api": "1.0", "permissions": ["ssh"], "page_title": "Demo",
        });
        Manifest::parse(json.to_string().as_bytes()).unwrap()
    }

    fn action_of(list: &[Row], id: &str) -> Action {
        list.iter().find(|r| r.id == id).unwrap().action.clone()
    }

    #[test]
    fn available_list_marks_installed_and_updates() {
        let revoked = Revoked { id: "bad".into(), versions: vec!["*".into()], reason: "Steals tokens".into() };
        let index = index_of(
            vec![entry("fresh", "1.0.0", "Someone"), entry("same", "1.0.0", "Someone"), entry("newer", "1.10.0", "Someone"), entry("other", "2.0.0", "Another"), entry("bad", "1.0.0", "Someone")],
            vec![revoked],
        );
        let installed = vec![manifest("same", "1.0.0", "Someone"), manifest("newer", "1.9.0", "Someone"), manifest("other", "1.0.0", "Someone")];
        let list = rows(&index, &installed, true, "");
        assert_eq!(list.len(), 5);
        assert_eq!(action_of(&list, "fresh"), Action::Install);
        assert_eq!(action_of(&list, "same"), Action::Installed);
        assert_eq!(action_of(&list, "newer"), Action::Update { version: "1.10.0".into() });
        // Another author with the same id is not an update of the installed plugin.
        assert_eq!(action_of(&list, "other"), Action::Installed);
        assert_eq!(action_of(&list, "bad"), Action::Revoked { reason: "Steals tokens".into() });
        assert!(list.iter().all(|r| r.blocked.is_none()));

        // The buttons.
        let find = |id: &str| list.iter().find(|r| r.id == id).unwrap();
        let install = button(find("fresh"), None, false);
        assert!(install.enabled && install.primary && install.label == "Install");
        let update = button(find("newer"), None, false);
        assert!(update.enabled && update.label.contains("1.10.0"), "{update:?}");
        assert!(!button(find("same"), None, false).enabled);
        let bad = button(find("bad"), None, false);
        assert!(!bad.enabled && bad.hint.as_deref() == Some("Steals tokens"));
        // Running, or something else waiting: no second click.
        assert!(!button(find("fresh"), Some(&Job::Downloading), false).enabled);
        assert!(!button(find("fresh"), None, true).enabled);
        assert!(button(find("fresh"), Some(&Job::Failed("x".into())), false).enabled, "a failed one can be tried again");

        // The search: name, author and description, ignoring case.
        assert_eq!(rows(&index, &installed, true, "NEWER").len(), 1);
        assert_eq!(rows(&index, &installed, true, "another").len(), 1);
        assert_eq!(rows(&index, &installed, true, "does THINGS").len(), 5);
        assert!(rows(&index, &installed, true, "nothing like it").is_empty());
        // The permissions in plain words, the sensitive one flagged.
        let fresh = find("fresh");
        assert_eq!(fresh.permissions.len(), 2);
        assert!(fresh.permissions.iter().any(|(text, sensitive)| *sensitive && text.contains("private column")));
    }

    #[test]
    fn expired_index_disables_install() {
        let index = index_of(vec![entry("fresh", "1.0.0", "Someone"), entry("newer", "2.0.0", "Someone"), entry("same", "1.0.0", "Someone")], vec![]);
        let state = IndexState { index: Some(index.clone()), freshness: Some(Freshness::Expired), ..IndexState::default() };
        let can_install = index::installable(&state, false).is_some();
        assert!(!can_install, "an expired index is not installed from");
        let installed = vec![manifest("newer", "1.0.0", "Someone"), manifest("same", "1.0.0", "Someone")];
        let list = rows(&index, &installed, can_install, "");
        for id in ["fresh", "newer"] {
            let row = list.iter().find(|r| r.id == id).unwrap();
            assert_eq!(row.blocked, Some(OUT_OF_DATE));
            let look = button(row, None, false);
            assert!(!look.enabled, "{id}");
            assert_eq!(look.hint.as_deref(), Some("The index is out of date"));
        }
        assert_eq!(list.iter().find(|r| r.id == "same").unwrap().blocked, None);
        // A current one installs.
        let current = IndexState { freshness: Some(Freshness::Current), ..state };
        assert!(index::installable(&current, false).is_some());
    }

    #[test]
    fn index_strings_are_display_safe() {
        let mut nasty = entry("nasty", "1.0.0", "Some\u{202e}one\u{0}");
        nasty.name = "Evil\u{1b}[31m\u{200f}name\nsecond line".into();
        nasty.description = format!("{}\u{7}", "long ".repeat(200));
        nasty.source.repo = "https://github.com/x/\u{202d}y".into();
        let mut bad = entry("bad", "1.0.0", "Someone");
        bad.name = "Bad\u{2066}".into();
        let revoked = Revoked { id: "bad".into(), versions: vec!["1.0.0".into()], reason: "Reason\u{202e}\r\nwith tricks".into() };
        let index = index_of(vec![nasty.clone(), bad], vec![revoked]);
        let list = rows(&index, &[], true, "");
        let row = &list[0];
        assert_eq!(row.name, display_safe(&nasty.name, 60));
        assert_eq!(row.author, display_safe(&nasty.author, 60));
        assert_eq!(row.description, display_safe(&nasty.description, 300));
        assert_eq!(row.repo, display_safe(&nasty.source.repo, 200));
        let Action::Revoked { reason } = &list[1].action else { panic!("revoked") };
        let mut shown: Vec<&str> = vec![&row.name, &row.author, &row.description, &row.repo, &row.commit, &row.version, reason, &list[1].name];
        shown.extend(row.permissions.iter().map(|(text, _)| text.as_str()));
        for text in shown {
            assert!(!text.chars().any(|c| c.is_control() || ('\u{202a}'..='\u{202e}').contains(&c) || ('\u{2066}'..='\u{2069}').contains(&c) || c == '\u{200f}'), "{text:?}");
        }
        assert!(row.description.chars().count() <= 300);
        // The error of a check: its fixed part stays as it is, the whole is safe.
        assert_eq!(error_text(NOT_SET_UP), NOT_SET_UP);
        assert!(!error_text("HTTP 500\u{202e}\u{1b}").chars().any(|c| c.is_control() || c == '\u{202e}'));
    }

    #[test]
    fn ages_are_short() {
        let now: DateTime<Utc> = "2026-11-13T10:00:00Z".parse().unwrap();
        assert_eq!(age_text(now, now), "just now");
        assert_eq!(age_text(now - chrono::Duration::minutes(5), now), "5 min ago");
        assert_eq!(age_text(now - chrono::Duration::hours(3), now), "3 h ago");
        assert_eq!(age_text(now - chrono::Duration::days(12), now), "12 days ago");
    }
}
