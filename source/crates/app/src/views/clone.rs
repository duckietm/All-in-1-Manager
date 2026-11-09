//! Clones a hotel repository on the server, or updates an existing clone. (CloneStep.cs)

use std::sync::Arc;

use aio_core::settings::{self, InstallerSettings, BRANCH_CHOICES};
use aio_remote::git_clone::{self, CloneTargetKind};
use aio_remote::{emulator, fail, octane, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::dialogs::Choice;
use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::i18n::{n, t, tf};

/// Polaris, Octane and Octane-Renderer, as ServerWizard.ServerSteps builds them.
pub fn steps() -> Vec<Arc<dyn InstallStep>> {
    vec![
        Arc::new(CloneStep::new(
            "Polaris",
            n("Emulator"),
            "duckietm/Polaris-Emulator",
            emulator::repository,
            n("The Polaris game server (emulator): Java sources, the database and the hotel assets."),
        )),
        Arc::new(CloneStep::new("Octane", n("Client"), "duckietm/Octane", octane::octane, n("The Octane browser client: the hotel's user interface."))),
        Arc::new(CloneStep::new(
            "Renderer",
            n("Octane-Renderer"),
            "duckietm/Octane-Renderer",
            octane::renderer,
            n("The Octane Renderer: draws rooms, avatars and furniture for Octane."),
        )),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingFolderChoice {
    MoveAside,
    Abort,
}

struct Options {
    /// The branch chosen in the list.
    branch: String,
    /// The branch of the clone on the server, as the last check found it; None when there is no clone.
    cloned_branch: Option<String>,
}

pub struct CloneStep {
    title: &'static str,
    caption: &'static str,
    repository: &'static str,
    url: String,
    name: String,
    description: &'static str,
    /// The folder on the server; read from the server layout each time, since the Settings page can change it.
    target: fn() -> String,
    options: Mutex<Options>,
}

fn same_branch(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// The line under the branch list: what the step does with an existing clone. None when there is no clone.
fn branch_note(target: &str, cloned: Option<&str>, selected: &str) -> Option<String> {
    let cloned = cloned?;
    Some(if same_branch(Some(cloned), selected) {
        tf("{target} is on {cloned}: the step updates that branch.", &[("target", target), ("cloned", cloned)])
    } else {
        tf(
            "{target} is on {cloned}: the step switches to {branch} and updates it.",
            &[("target", target), ("cloned", cloned), ("branch", selected)],
        )
    })
}

impl CloneStep {
    fn new(title: &'static str, caption: &'static str, repository: &'static str, target: fn() -> String, description: &'static str) -> Self {
        let step = Self {
            title,
            caption,
            repository,
            url: format!("https://github.com/{repository}.git"),
            name: repository.rsplit('/').next().unwrap_or(repository).to_string(),
            description,
            target,
            options: Mutex::new(Options { branch: settings::DEFAULT_BRANCH.into(), cloned_branch: None }),
        };
        step.show_saved_branch();
        step
    }

    /// Puts the saved branch in the list; the Settings page may have changed it since this step was made.
    fn show_saved_branch(&self) {
        let branch = settings::choice_for(&InstallerSettings::load().branch_for(self.repository)).branch;
        self.options.lock().branch = branch.to_string();
    }

    /// The chosen branch stays chosen for this repository, also on the Settings page and next time.
    fn branch_chosen(&self, branch: &str) {
        let mut settings = InstallerSettings::load();
        settings.set_branch(self.repository, branch);
        let _ = settings.save();
    }

    fn selected_branch(&self) -> String {
        self.options.lock().branch.clone()
    }

    fn set_cloned_branch(&self, branch: Option<String>) {
        self.options.lock().cloned_branch = branch;
    }
}

#[async_trait]
impl InstallStep for CloneStep {
    fn title(&self) -> String {
        self.title.into()
    }

    fn caption(&self) -> String {
        t(self.caption).into()
    }

    fn install_label(&self) -> String {
        tf("Clone {name}", &[("name", &self.name)])
    }

    fn rerun_label(&self) -> String {
        t("Update (git pull)").into()
    }

    fn intro(&self) -> Intro {
        let target = (self.target)();
        Intro {
            icon: Icon::Download,
            heading: tf("Clone {repository}", &[("repository", self.repository)]),
            description: t(self.description).into(),
            bullets: vec![
                tf(
                    "Clones https://github.com/{repository} to {target} (the full history, so it can be updated with git pull).",
                    &[("repository", self.repository), ("target", &target)],
                ),
                t("You choose the branch: main for the stable version, dev for the newest changes.").into(),
                t("Installs git first when the server does not have it.").into(),
                t("Already cloned there? Then it is updated with git pull --ff-only (switching branch first when you picked another one); changed files are never overwritten.").into(),
                t("Something else in the folder? You choose: move it aside to a .bak folder, or abort. Nothing is deleted.").into(),
            ],
            footnote: None,
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the folder").into(), t("Clone or update").into(), t("Check the clone").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let (mut branch, cloned) = {
            let options = self.options.lock();
            (options.branch.clone(), options.cloned_branch.clone())
        };
        ui.add_space(8.0);
        ui.label(RichText::new(t("Branch")).size(12.0).color(palette.muted).strong());
        ui.add_space(5.0);
        let before = branch.clone();
        egui::ComboBox::from_id_salt(("clone-branch", self.repository))
            .width(380.0)
            .selected_text(settings::choice_for(&branch).to_string())
            .show_ui(ui, |ui| {
                for choice in BRANCH_CHOICES {
                    ui.selectable_value(&mut branch, choice.branch.to_string(), choice.to_string());
                }
            });
        if branch != before {
            self.options.lock().branch = branch.clone();
            self.branch_chosen(&branch);
        }
        if let Some(note) = branch_note(&(self.target)(), cloned.as_deref(), &branch) {
            ui.add_space(5.0);
            ui.label(RichText::new(note).size(11.5).color(palette.subtle));
        }
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        self.show_saved_branch();
        let target = (self.target)();
        let selected = self.selected_branch();
        let state = git_clone::inspect(server.as_ref(), &self.url, &target).await?;
        self.set_cloned_branch(if state.kind == CloneTargetKind::SameRepository { state.branch.clone() } else { None });
        ui.wake();
        match state.kind {
            CloneTargetKind::SameRepository => {
                let other_branch = !same_branch(state.branch.as_deref(), &selected);
                let changes = if state.changed_files > 0 {
                    tf(" {count} tracked file(s) were changed locally.", &[("count", &state.changed_files.to_string())])
                } else {
                    String::new()
                };
                let behind = if other_branch {
                    tf(" You chose {branch}: run the step again to switch to that branch.", &[("branch", &selected)])
                } else if state.behind > 0 {
                    tf(" {count} new commit(s) are available: run the step again to update.", &[("count", &state.behind.to_string())])
                } else {
                    t(" It is up to date.").to_string()
                };
                ui.detected(
                    if other_branch || state.behind > 0 { ResultKind::Info } else { ResultKind::Success },
                    &tf("{name} is already cloned in {target}", &[("name", &self.name), ("target", &target)]),
                    &format!(
                        "{} at {}.{behind}{changes}",
                        state.branch.as_deref().unwrap_or(""),
                        state.commit.as_deref().unwrap_or("")
                    ),
                );
                Ok(!other_branch)
            }
            CloneTargetKind::OtherContent => {
                let what = match &state.origin {
                    None => t("It is not a git clone.").to_string(),
                    Some(origin) => tf("It is a clone of {origin}.", &[("origin", origin)]),
                };
                ui.detected(
                    ResultKind::Warning,
                    &tf("{target} already has other content", &[("target", &target)]),
                    &format!("{what}{}", t(" Cloning asks first: move it aside to a .bak folder, or abort.")),
                );
                Ok(false)
            }
            CloneTargetKind::Missing => {
                ui.detected(
                    ResultKind::Info,
                    &tf("{target} does not exist yet", &[("target", &target)]),
                    &tf("Clone {name} ({branch}) now, or skip this step.", &[("name", &self.name), ("branch", &selected)]),
                );
                Ok(false)
            }
        }
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let target = (self.target)();
        let branch = self.selected_branch();
        log(&format!("> Checking {target} ..."));
        let state = git_clone::inspect(server, &self.url, &target).await?;
        match state.kind {
            CloneTargetKind::OtherContent => {
                log(&match &state.origin {
                    None => "  The folder has files, but is not a git clone.".to_string(),
                    Some(origin) => format!("  The folder is a clone of {origin}."),
                });
                let choice = ui
                    .choose(
                        t("Folder not empty"),
                        &tf("{target} already has other content", &[("target", &target)]),
                        &match &state.origin {
                            None => tf("It is not a clone of {name}. It has to make room for the clone.", &[("name", &self.name)]),
                            Some(origin) => tf("It is a clone of {origin}, not of {name}.", &[("origin", origin), ("name", &self.name)]),
                        },
                        ExistingFolderChoice::Abort,
                        vec![
                            Choice::new(
                                t("Move it aside and clone"),
                                tf("{target} is renamed to {target}.bak-<date>; nothing is deleted.", &[("target", &target)]),
                                ExistingFolderChoice::MoveAside,
                            )
                            .primary(),
                            Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), ExistingFolderChoice::Abort),
                        ],
                    )
                    .await;
                if choice == ExistingFolderChoice::Abort {
                    return Ok(ui.aborted(&server.login().host));
                }
                let backup = git_clone::move_aside(server, &target, &log).await?;
                ui.done(&tf("moved to {backup}", &[("backup", &backup)]));
            }
            CloneTargetKind::SameRepository => {
                let current = state.branch.clone().unwrap_or_default();
                log(&format!("  {} is already cloned here ({current} at {}).", self.name, state.commit.as_deref().unwrap_or("")));
                ui.done(&if same_branch(state.branch.as_deref(), &branch) {
                    t("already cloned: update").to_string()
                } else {
                    tf("already cloned: {from} -> {to}", &[("from", &current), ("to", &branch)])
                });
            }
            CloneTargetKind::Missing => {
                log("  The folder does not exist yet.");
                ui.done(t("empty"));
            }
        }

        let update = state.kind == CloneTargetKind::SameRepository;
        let commit = if update {
            git_clone::update(server, &target, Some(&branch), &log).await?
        } else {
            git_clone::clone(server, &self.url, &target, Some(&branch), &log).await?
        };
        ui.done(if update { "git pull" } else { "git clone" });

        let after = git_clone::inspect(server, &self.url, &target).await?;
        if after.kind != CloneTargetKind::SameRepository {
            fail!("{target} is not a clone of {} after cloning.", self.name);
        }
        self.set_cloned_branch(after.branch.clone());
        log(&format!("  {target}: {commit}"));
        ui.done(&commit);

        Ok(ui.succeeded(
            &tf("{name} is in {target}", &[("name", &self.name), ("target", &target)]),
            &tf("Branch {commit}.", &[("commit", &commit)]),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_only_with_a_clone() {
        assert_eq!(branch_note("/var/www/Octane", None, "main"), None);
        assert_eq!(
            branch_note("/var/www/Octane", Some("Dev"), "dev").as_deref(),
            Some("/var/www/Octane is on Dev: the step updates that branch.")
        );
        assert_eq!(
            branch_note("/var/www/Octane", Some("main"), "dev").as_deref(),
            Some("/var/www/Octane is on main: the step switches to dev and updates it.")
        );
    }

    #[test]
    fn steps_match_the_server_wizard() {
        let steps = steps();
        let titles: Vec<String> = steps.iter().map(|s| s.title()).collect();
        assert_eq!(titles, ["Polaris", "Octane", "Renderer"]);
    }
}
