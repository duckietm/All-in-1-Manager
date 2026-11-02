//! "A new version is available" at startup (UpdateDialog.cs and InstallerWindow.CheckForUpdateAsync /
//! ShowNewerVersionAsync): what is new, then Update and restart or Later. The download replaces the running program,
//! which then starts again by itself; after Later (or when there is nothing to update) the login opens.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use super::login::LoginOpener;
use crate::gui::dialogs::Choice;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, Fixed, Palette};
use crate::gui::{widgets, Shared};
use crate::i18n::{t, tf};
use crate::update::{self, Release};

#[derive(Debug, Clone, PartialEq)]
enum Download {
    Asking,
    Running { done: u64, total: u64 },
    Done,
    Failed(String),
}

/// The question on screen, drawn by the login dialog until it is answered.
pub struct UpdatePrompt {
    release: Release,
    state: Arc<Mutex<Download>>,
}

impl UpdatePrompt {
    fn new(release: Release) -> Self {
        Self { release, state: Arc::new(Mutex::new(Download::Asking)) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewerVersionChoice {
    Download,
    Later,
}

/// Asks GitHub for a newer installer; when there is none (or after "Later"), opens the login.
pub fn check_at_startup(shared: &Shared, login: LoginOpener) {
    let shared_task = shared.clone();
    shared.spawn(async move {
        let shared = shared_task;
        update::clean_up();
        if let Some(release) = update::check().await {
            shared.set_status(&tf("Version {version} of the installer is available.", &[("version", &release.version)]));
            login.offer_update(UpdatePrompt::new(release));
            return;
        }
        // No release to install by itself (none published, or a build that cannot replace itself): compare with the
        // version on GitHub and say where to get it.
        if let Ok(Some(latest)) = update::latest().await {
            if update::is_newer(&latest.version) {
                show_newer_version(&shared, &latest.version).await;
            }
        }
        login.open();
    });
}

/// A newer installer is on GitHub: offer its download page.
async fn show_newer_version(shared: &Shared, latest: &str) {
    let page = update::download_page();
    shared.set_status(&tf(
        "Version {latest} of the installer is available (you have {current}).",
        &[("latest", latest), ("current", update::CURRENT_VERSION)],
    ));
    let choice = shared
        .dialogs
        .choose(
            t("Update"),
            &tf("Version {latest} of the installer is available", &[("latest", latest)]),
            &tf(
                "You are running {current}. Download the new version from {page} (the Program folder), close this one and start the new one. Your settings and saved logins stay.",
                &[("current", update::CURRENT_VERSION), ("page", &page)],
            ),
            NewerVersionChoice::Later,
            vec![
                Choice::new(t("Open the download page"), page.clone(), NewerVersionChoice::Download).primary(),
                Choice::new(t("Later"), t("The About page can check again."), NewerVersionChoice::Later),
            ],
        )
        .await;
    if choice == NewerVersionChoice::Download {
        crate::gui::open_url(&page);
    }
}

/// Draws the question; true once it is closed (Later, or Close after a failed download).
pub(crate) fn show(ctx: &egui::Context, shared: &Shared, prompt: &mut UpdatePrompt) -> bool {
    let palette = Palette::current(ctx);
    let state = prompt.state.lock().unwrap().clone();
    let busy = matches!(state, Download::Running { .. } | Download::Done);
    // Closing the window while the download runs would leave it half done.
    if busy && ctx.input(|i| i.viewport().close_requested()) && state != Download::Done {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    }
    let release = &prompt.release;
    let mut apply = false;
    let mut closed = false;
    egui::Modal::new(egui::Id::new("update-dialog"))
        .frame(widgets::card_frame(&palette).inner_margin(egui::Margin::symmetric(26, 24)))
        .show(ctx, |ui| {
            ui.set_width(568.0);
            ui.horizontal_top(|ui| {
                egui::Frame::new().fill(theme::with_alpha(Fixed::brand(), 0x26)).corner_radius(20).inner_margin(10).show(ui, |ui| {
                    icons::show(ui, Icon::Download, 20.0, palette.text);
                });
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.label(RichText::new(tf("Version {version} is available", &[("version", &release.version)])).size(18.0).strong());
                    widgets::text(
                        ui,
                        &tf(
                            "You are running {current}. The new version is downloaded from github.com/{repository} and takes the place of this program; it starts again by itself.",
                            &[("current", update::CURRENT_VERSION), ("repository", update::REPOSITORY)],
                        ),
                        13.0,
                    );
                    what_is_new(ui, &palette, release);
                    match &state {
                        Download::Asking => {}
                        Download::Running { done, total } => {
                            let bar = if *total > 0 {
                                egui::ProgressBar::new((*done as f32 / *total as f32).clamp(0.0, 1.0))
                            } else {
                                egui::ProgressBar::new(0.0).animate(true)
                            };
                            ui.add(bar.desired_height(6.0).fill(Fixed::brand()));
                            let status = if *done == 0 && *total == 0 {
                                tf("Downloading {name} ...", &[("name", update::ASSET_NAME)])
                            } else {
                                tf(
                                    "Downloading {name}: {done} of {total}",
                                    &[("name", update::ASSET_NAME), ("done", &megabytes(*done)), ("total", &megabytes(*total))],
                                )
                            };
                            ui.label(RichText::new(status).size(12.0).color(palette.muted));
                        }
                        Download::Done => {
                            ui.add(egui::ProgressBar::new(1.0).desired_height(6.0).fill(Fixed::brand()));
                            ui.label(RichText::new(t("Downloaded. Starting the new version ...")).size(12.0).color(palette.muted));
                        }
                        Download::Failed(message) => {
                            ui.add(egui::Label::new(RichText::new(message).size(12.0).color(palette.error_text)).wrap());
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(state == Download::Asking, widgets::primary_button(t("Update and restart"))).clicked() {
                            apply = true;
                        }
                        let later = if matches!(state, Download::Failed(_)) { t("Close") } else { t("Later") };
                        if ui.add_enabled(!busy, widgets::secondary_button(later)).clicked() {
                            closed = true;
                        }
                    });
                });
            });
        });
    if !busy && !apply && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        closed = true;
    }
    if apply {
        start_download(shared, prompt);
    }
    closed
}

fn start_download(shared: &Shared, prompt: &UpdatePrompt) {
    *prompt.state.lock().unwrap() = Download::Running { done: 0, total: 0 };
    let state = prompt.state.clone();
    let release = prompt.release.clone();
    let ctx = shared.ctx.clone();
    shared.spawn(async move {
        let progress_state = state.clone();
        let progress_ctx = ctx.clone();
        let result = update::download_and_replace(&release, move |done, total| {
            *progress_state.lock().unwrap() = Download::Running { done, total };
            progress_ctx.request_repaint();
        })
        .await;
        let failed = |error: String| {
            Download::Failed(tf("The update failed: {error}  Download it yourself from {page}", &[("error", &error), ("page", &release.page_url)]))
        };
        let next = match result {
            Err(error) => failed(format!("{error:#}")),
            Ok(_) => {
                *state.lock().unwrap() = Download::Done;
                ctx.request_repaint();
                // Let the last line appear before the window goes away.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                if update::start_again() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    Download::Done
                } else {
                    failed(t("the new version is in place, but it could not be started: close this one and start it yourself.").to_string())
                }
            }
        };
        *state.lock().unwrap() = next;
    });
}

/// The release text, shortened: a dialog is not the place for a full changelog.
fn notes(body: &str) -> Option<String> {
    let notes = body.replace('\r', "");
    let notes = notes.trim();
    if notes.is_empty() {
        return None;
    }
    if notes.chars().count() > 600 {
        let short: String = notes.chars().take(600).collect();
        Some(format!("{} ...", short.trim_end()))
    } else {
        Some(notes.to_string())
    }
}

/// What the update brings: every version since this one, newest first, with its release notes (headings, bullets
/// and bold of GitHub's Markdown drawn plainly), in a scrolling box.
fn what_is_new(ui: &mut egui::Ui, palette: &crate::gui::theme::Palette, release: &update::Release) {
    let changes: Vec<&(String, String)> = release.changes.iter().filter(|(_, notes)| !notes.trim().is_empty()).collect();
    if changes.is_empty() {
        if let Some(notes) = notes(&release.notes) {
            widgets::muted(ui, palette, &notes, 12.0);
        }
        return;
    }
    ui.label(RichText::new(t("What's new")).size(13.5).strong());
    egui::Frame::new().fill(palette.hover).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
            for (version, notes) in changes {
                ui.label(RichText::new(tf("Version {version}", &[("version", version)])).size(13.0).strong().color(palette.accent_text));
                for line in notes.lines() {
                    let line = line.trim_end();
                    let plain = line.replace("**", "").replace('`', "");
                    let trimmed = plain.trim_start();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(heading) = trimmed.strip_prefix('#') {
                        ui.label(RichText::new(heading.trim_start_matches('#').trim()).size(12.5).strong());
                    } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                        let depth = (plain.len() - trimmed.len()) as f32;
                        ui.horizontal_top(|ui| {
                            ui.add_space(6.0 + depth * 4.0);
                            ui.label(RichText::new("•").size(12.0).color(palette.muted));
                            widgets::muted(ui, palette, item, 12.0);
                        });
                    } else {
                        widgets::muted(ui, palette, trimmed, 12.0);
                    }
                }
                ui.add_space(6.0);
            }
        });
    });
}

fn megabytes(bytes: u64) -> String {
    if bytes == 0 {
        "?".into()
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_the_notes_and_sizes() {
        assert_eq!(notes(" \r\n "), None);
        assert_eq!(notes("Fixed\r\nthings").as_deref(), Some("Fixed\nthings"));
        let long = "é".repeat(700);
        let short = notes(&long).unwrap();
        assert_eq!(short.chars().count(), 604);
        assert!(short.ends_with(" ..."));
        assert_eq!(megabytes(0), "?");
        assert_eq!(megabytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }
}
