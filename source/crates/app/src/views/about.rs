//! About: the version, the update check and the credits. (InstallerWindow.ShowAbout)

use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use crate::gui::theme::Palette;
use crate::gui::{widgets, Shared, View};
use crate::i18n::{n, t, tf};
use crate::update;

/// Who made what, in the order they joined in.
const CREDITS: [(&str, &[&str]); 2] = [
    ("DuckieTM", &[n("The installer, and the All-in-1 Converter this look and feel comes from.")]),
    (
        "Life",
        &[
            n("The branch choice (main / dev) for the clone steps and in Settings, the update check for the hotel's repositories, and the installer's own update at startup."),
            n("The Rust port: one program for Windows and Linux, the window in nine languages, and the dry-run mode."),
        ],
    ),
];

pub struct AboutView {
    check: Arc<Mutex<Option<String>>>,
    checking: Arc<Mutex<bool>>,
}

impl AboutView {
    pub fn new(_shared: &Shared) -> Self {
        Self { check: Arc::new(Mutex::new(None)), checking: Arc::new(Mutex::new(false)) }
    }
}

impl View for AboutView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        widgets::card(ui, palette, egui::Margin::same(20), |ui| {
            ui.label(RichText::new("All-in-1 Manager").size(16.0).strong());
            ui.label(RichText::new(tf("Version {version}", &[("version", update::CURRENT_VERSION)])).size(12.5).color(palette.subtle));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let checking = *self.checking.lock().unwrap();
                if ui.add_enabled(!checking, widgets::secondary_button(t("Check for updates"))).clicked() {
                    *self.checking.lock().unwrap() = true;
                    *self.check.lock().unwrap() = Some(t("Checking GitHub ...").into());
                    let (check, busy) = (self.check.clone(), self.checking.clone());
                    shared.spawn(async move {
                        let text = match update::latest().await {
                            Err(_) => tf("Could not reach GitHub: check your internet connection, or look at {page}.", &[("page", &update::download_page())]),
                            Ok(Some(release)) if update::is_newer(&release.version) => tf("Version {version} is available: {page}", &[("version", &release.version), ("page", &release.page_url)]),
                            Ok(_) => tf("You have the latest version ({version}).", &[("version", update::CURRENT_VERSION)]),
                        };
                        *check.lock().unwrap() = Some(text);
                        *busy.lock().unwrap() = false;
                    });
                }
                if let Some(text) = self.check.lock().unwrap().as_ref() {
                    ui.label(RichText::new(text).size(12.5).color(palette.muted));
                }
            });
            ui.add_space(4.0);
            widgets::muted(ui, palette, t("Installs a full hotel server (Polaris Emulator, Octane and Octane Renderer) on an Ubuntu Server or Debian machine over SSH."), 13.0);
            ui.add_space(12.0);
            ui.label(RichText::new(t("CREDITS")).size(11.0).strong().color(palette.muted));
            for (who, what) in CREDITS {
                ui.horizontal_top(|ui| {
                    ui.add_sized([110.0, 16.0], egui::Label::new(RichText::new(who).size(12.5).strong()));
                    ui.vertical(|ui| {
                        for what in what {
                            widgets::muted(ui, palette, t(what), 12.5);
                        }
                    });
                });
            }
            ui.add_space(12.0);
            ui.label(
                RichText::new(t("Fonts: Ubuntu (Ubuntu Font Licence 1.0) and Pixelify Sans (SIL Open Font License 1.1), in assets/fonts with their licences. The look is inspired by Habbo Hotel; Habbo is a trademark of Sulake, and this installer is not made by or connected to Sulake."))
                    .size(12.0)
                    .color(palette.subtle),
            );
        });
    }
}
