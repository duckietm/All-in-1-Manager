//! "notes": a teaching example for `aio-plugin-sdk`.
//!
//! What it shows, one feature per section below:
//!
//! 1. **Private files** (`files`, permission `files`): the note and its title are kept in the
//!    plugin's own data folder and survive restarts.
//! 2. **Dialogs** (`host::prompt_text`, `host::pick_save_file`): ask the user for a text, and let
//!    the user choose where to export. The plugin never sees a path; it only gets a handle to
//!    the one file the user picked.
//! 3. **Reading the database** (`sql::Select`, permission `sql:read:users`): a read-only table.
//!    Only `id` and `username` are requested. Private columns such as `mail` or `password`
//!    would need a separate `sql:read-private:...` permission, and a plugin should never ask
//!    for one it does not really need.
//! 4. **Failing gracefully**: every host call can fail (no server connected, access refused,
//!    user said no). The page shows a friendly banner instead of crashing.
//! 5. **Translations** (`aio_plugin_sdk::i18n`, the `i18n` folder): every text the page shows goes
//!    through `tr` (a fixed text) or `tr_with` (a text with `{name}` placeholders). The English
//!    text is the key; `i18n/it.json` maps each one to Italian, and a language the plugin does not
//!    translate shows English. The manifest's texts (name, description, page title, tile) are
//!    translated by the Manager from the same file, so they are keys there too. Keep keys and
//!    placeholders identical in the code and the file.
//!
//! Remember the model: `render` only *describes* the page from the current state; `on_event`
//! changes the state when the user does something; the Manager then calls `render` again.

use aio_plugin_sdk::i18n::{tr, tr_with};
use aio_plugin_sdk::{export_plugin, files, host, sql, ui::BannerKind, ErrorKind, Event, Plugin, Ui};

/// Private file with the text of the note.
const NOTES_FILE: &str = "notes.txt";
/// Private file with the note's title.
const TITLE_FILE: &str = "title.txt";
/// Title used until the user renames the note. It is an English key: `title()` translates it while it is in use,
/// and a title the user typed is shown as it is.
const DEFAULT_TITLE: &str = "My notes";
/// How many users the table shows.
const USER_LIMIT: u32 = 10;
/// The Manager's limit for the text of an input; a longer value would not be accepted.
const INPUT_LIMIT: usize = 4000;

/// The state of the page. The instance lives as long as the plugin is open, so fields keep
/// their values between `render` and `on_event` calls.
struct Notes {
    /// The note, as last typed.
    text: String,
    /// The title of the note.
    title: String,
    /// A message about the last action (saved, exported...), shown as a banner.
    message: Option<(BannerKind, String)>,
    /// The "Recent users" table: rows of `[id, username]`, or why it is not available.
    users: Result<Vec<[String; 2]>, String>,
}

/// Read a private text file; a missing file is not an error, it just gives `default`.
/// `read_to_string_if_exists` returns `Ok(None)` only for "not found"; any other failure is real.
fn load(path: &str, default: &str) -> String {
    match files::read_to_string_if_exists(path) {
        Ok(Some(text)) => text,
        Ok(None) => default.to_string(),
        Err(e) => {
            host::log(host::Level::Warn, &format!("could not read {path}: {e}"));
            default.to_string()
        }
    }
}

/// Ask the server for the first users. `Select` is a builder: nothing is sent until `run`.
/// The result is turned into plain strings, or into a message a person can understand.
fn load_users() -> Result<Vec<[String; 2]>, String> {
    let rows = sql::Select::from("users")
        // Only these two columns. Asking for a private one would be refused by the host.
        .columns(["id", "username"])
        .limit(USER_LIMIT)
        .run()
        .map_err(|e| match e.kind() {
            ErrorKind::Denied => {
                tr("The users table is not available: the plugin lacks the `sql:read:users` permission.")
            }
            ErrorKind::Rejected => tr("The request was rejected, so the user list was not loaded."),
            ErrorKind::Timeout => tr("The server did not answer in time. Try Refresh users again."),
            // `ErrorKind` is `#[non_exhaustive]`, so a catch-all arm is required. NotFound and
            // Other get the text of the error. `tr_with` fills `{error}` after translating.
            _ => tr_with("The user list is not available right now ({error}).", &[("error", &e.to_string())]),
        })?;
    let mut table = Vec::new();
    for row in rows {
        // `get` is typed: `id` is a number, `username` is text. A mismatch is an error.
        let id: i64 = row.get("id").map_err(|e| e.to_string())?;
        let username: String = row.get("username").map_err(|e| e.to_string())?;
        table.push([id.to_string(), username]);
    }
    Ok(table)
}

impl Notes {
    /// The title to show: the default one is translated, one the user typed is not.
    fn title(&self) -> String {
        if self.title == DEFAULT_TITLE {
            tr(DEFAULT_TITLE)
        } else {
            self.title.clone()
        }
    }

    fn say(&mut self, kind: BannerKind, text: impl Into<String>) {
        self.message = Some((kind, text.into()));
    }

    /// "Export...": ask the user where to save, then write the note there.
    fn export(&mut self) {
        // The user chooses the file in the Manager's own dialog. `None` means they cancelled.
        let Some(file) = host::pick_save_file(&tr("Export notes"), "notes.txt", ["txt"]) else {
            return;
        };
        let content = format!("{}\n\n{}\n", self.title(), self.text);
        match file.write(content.as_bytes()) {
            Ok(()) => self.say(BannerKind::Success, tr_with("Exported to {file}.", &[("file", &file.name())])),
            Err(e) => self.say(BannerKind::Error, tr_with("Could not export: {error}", &[("error", &e.to_string())])),
        }
    }

    /// "Rename note title...": a small text prompt with the current title prefilled.
    fn rename(&mut self) {
        let Some(new_title) = host::prompt_text(&tr("Rename note"), &tr("Title"), &self.title()) else {
            return; // cancelled
        };
        let new_title = new_title.trim().to_string();
        if new_title.is_empty() {
            self.say(BannerKind::Warning, tr("The title cannot be empty."));
            return;
        }
        match files::write_str(TITLE_FILE, &new_title) {
            Ok(()) => {
                self.title = new_title;
                self.say(BannerKind::Success, tr("Title renamed."));
            }
            Err(e) => self.say(BannerKind::Error, tr_with("Could not save the title: {error}", &[("error", &e.to_string())])),
        }
    }
}

impl Plugin for Notes {
    /// Called lazily, on first use (the first render or event), not when the plugin is installed.
    /// It loads what was saved before. It may call the host (here: one SQL query), and that
    /// call counts against the time limit of that first render or event.
    fn new() -> Self {
        let mut text = load(NOTES_FILE, "");
        // A notes.txt edited by hand can be longer than an input accepts: keep the first
        // INPUT_LIMIT characters (cut on a character boundary, never inside one) and say so.
        let mut message = None;
        if let Some((cut, _)) = text.char_indices().nth(INPUT_LIMIT) {
            text.truncate(cut);
            message = Some((
                BannerKind::Info,
                tr_with(
                    "The saved note was longer than {limit} characters and was shortened.",
                    &[("limit", &INPUT_LIMIT.to_string())],
                ),
            ));
        }
        Notes { text, title: load(TITLE_FILE, DEFAULT_TITLE), message, users: load_users() }
    }

    /// Describe the page from the current state. No side effects here.
    fn render(&mut self, ui: &mut Ui) {
        ui.heading(self.title());

        if let Some((kind, text)) = &self.message {
            ui.banner(*kind, text.as_str());
        }

        // Call `tr` here, in `render`, every time: when the user switches the Manager's language the page is
        // drawn again and every text comes out in the new language. (Texts made outside `render`, like the
        // banners below, are translated once, in the language of that moment.)
        //
        // The note. The Manager reports the text when the field loses focus (Enter or a click
        // elsewhere), and the 4,000 character limit of an input applies.
        ui.input("notes", tr("Notes"), self.text.as_str());
        ui.row(|ui| {
            ui.button("export", tr("Export..."));
            ui.button("rename", tr("Rename note title..."));
        });

        // The read-only table, or a banner that explains why there is none.
        ui.card(tr("Recent users"), |ui| match &self.users {
            Ok(rows) if rows.is_empty() => ui.text(tr("There are no users yet.")),
            Ok(rows) => ui.table([tr("ID"), tr("Username")], rows.iter().cloned()),
            Err(why) => ui.banner(BannerKind::Warning, why.as_str()),
        });
        ui.button("refresh", tr("Refresh users"));
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        match event {
            // Editing: save at once so nothing is lost if the page is closed.
            Event::Changed { id, value } if id == "notes" => {
                match files::write_str(NOTES_FILE, &value) {
                    Ok(()) => self.message = None,
                    // Keep what the user typed on screen, but tell them it is not saved.
                    Err(e) => self.say(BannerKind::Error, tr_with("Could not save the note: {error}", &[("error", &e.to_string())])),
                }
                self.text = value;
            }
            Event::Clicked { id } => match id.as_str() {
                "export" => self.export(),
                "rename" => self.rename(),
                "refresh" => {
                    // Old status banners ("Exported...") no longer apply.
                    self.message = None;
                    self.users = load_users();
                }
                _ => {}
            },
            _ => {}
        }
        // Returning `Err(text)` would show `text` to the user as an error. Here every
        // failure was already turned into a banner, so we always succeed.
        Ok(())
    }
}

export_plugin!(Notes);
