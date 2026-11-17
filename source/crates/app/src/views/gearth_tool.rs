//! Tools → G-Earth: installs, updates, starts and removes G-Earth on this computer (see `tools::gearth`). Nothing is
//! downloaded without a "yes" to a question that names the source, the version and the size (and says when the
//! download cannot be verified), nothing is removed without a "yes", and G-Earth only starts on the Start button.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::gui::dialogs::{Choice, Dialogs};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{t, tf};
use crate::tools::gearth::{self, Installed, Release, Step};

/// How often the installed folder and the ports are looked at while the page is shown.
const REFRESH: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Job {
    #[default]
    Idle,
    Checking,
    Downloading(u64, u64),
    Unpacking,
    Starting,
    Removing,
}

#[derive(Default)]
struct State {
    installed: Option<Installed>,
    /// None: not looked yet.
    running: Option<bool>,
    probing: bool,
    looked: Option<Instant>,
    job: Job,
    cancel: Option<CancellationToken>,
    result: Option<(ResultKind, String, String)>,
}

pub struct GearthToolView {
    state: Arc<Mutex<State>>,
    folder: PathBuf,
    api: String,
}

/// What [`install_flow`] ended with.
#[derive(Debug)]
pub(crate) enum Outcome {
    UpToDate(String),
    Declined,
    Installed(Installed),
    Cancelled,
    Failed(String),
}

/// "123.4 MB".
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// The question before a download: source, version, size, and whether it can be verified.
fn confirmation(release: &Release, folder: &Path) -> String {
    let mut text = tf(
        "Download G-Earth {version} ({size}) from github.com/G-Realm/G-Earth (MIT licence, Java included) into {folder}?",
        &[("version", &release.version), ("size", &megabytes(release.size)), ("folder", &folder.display().to_string())],
    );
    text.push_str("\n\n");
    text.push_str(if release.verifiable() {
        t("The download is checked against the SHA-256 checksum the release publishes before anything is unpacked.")
    } else {
        t("Not verified: this release publishes no SHA-256 checksum, so the download cannot be checked.")
    });
    text
}

/// Looks up the newest release and, after a "yes", installs it into `folder`.
pub(crate) async fn install_flow(
    dialogs: &Dialogs,
    api: &str,
    folder: &Path,
    current: Option<&Installed>,
    cancel: &CancellationToken,
    progress: impl Fn(Step),
) -> Outcome {
    let release = match gearth::latest(api).await {
        Ok(release) => release,
        Err(error) => return Outcome::Failed(format!("{error:#}")),
    };
    if current.and_then(|c| c.version.as_deref()) == Some(release.version.as_str()) {
        return Outcome::UpToDate(release.version);
    }
    let options = vec![
        Choice::new(t("Download and install"), t("G-Earth does not start until you press Start."), true).primary(),
        Choice::new(t("Cancel"), t("Nothing is downloaded."), false),
    ];
    if !dialogs.choose(t("G-Earth"), t("Download G-Earth?"), &confirmation(&release, folder), false, options).await {
        return Outcome::Declined;
    }
    match gearth::install(&release, folder, cancel, progress).await {
        Ok(installed) => Outcome::Installed(installed),
        Err(error) if error.is::<gearth::Cancelled>() => Outcome::Cancelled,
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

/// After a "yes", removes `folder`. None when the answer was no.
pub(crate) async fn remove_flow(dialogs: &Dialogs, folder: &Path) -> Option<anyhow::Result<()>> {
    let message = tf("The folder {folder} and everything in it is deleted. This cannot be undone.", &[("folder", &folder.display().to_string())]);
    let options = vec![Choice::new(t("Remove G-Earth"), t("Delete the folder."), true).primary(), Choice::new(t("Cancel"), t("Keep G-Earth."), false)];
    if !dialogs.choose(t("Are you sure?"), t("Remove G-Earth?"), &message, false, options).await {
        return None;
    }
    let folder = folder.to_path_buf();
    Some(tokio::task::spawn_blocking(move || gearth::remove(&folder)).await.unwrap_or_else(|e| Err(e.into())))
}

impl GearthToolView {
    pub fn new(_shared: &Shared) -> Self {
        let folder = gearth::folder();
        gearth::recover(&folder);
        Self { state: Arc::new(Mutex::new(State::default())), folder, api: crate::update::GITHUB_API.to_string() }
    }

    /// Looks at the folder and the ports again, at most every few seconds and never during a job.
    fn refresh(&self, shared: &Shared, now: bool) {
        let mut state = self.state.lock();
        if state.job != Job::Idle || state.probing || (!now && state.looked.is_some_and(|at| at.elapsed() < REFRESH)) {
            return;
        }
        state.installed = gearth::installed(&self.folder);
        state.looked = Some(Instant::now());
        state.probing = true;
        drop(state);
        let state = self.state.clone();
        shared.spawn(async move {
            let running = gearth::is_running(gearth::PORTS).await;
            let mut state = state.lock();
            state.running = Some(running);
            state.probing = false;
        });
    }

    fn install(&self, shared: &Shared) {
        let cancel = CancellationToken::new();
        let current = {
            let mut state = self.state.lock();
            state.job = Job::Checking;
            state.result = None;
            state.cancel = Some(cancel.clone());
            state.installed.clone()
        };
        let (state, dialogs, api, folder, ctx) = (self.state.clone(), shared.dialogs.clone(), self.api.clone(), self.folder.clone(), shared.ctx.clone());
        shared.spawn(async move {
            let progress = |step: Step| {
                state.lock().job = match step {
                    Step::Downloading(done, total) => Job::Downloading(done, total),
                    Step::Unpacking => Job::Unpacking,
                };
                ctx.request_repaint();
            };
            let outcome = install_flow(&dialogs, &api, &folder, current.as_ref(), &cancel, progress).await;
            let result = match outcome {
                Outcome::UpToDate(version) => {
                    Some((ResultKind::Info, tf("G-Earth {version} is the newest release.", &[("version", &version)]), String::new()))
                }
                Outcome::Declined => None,
                Outcome::Installed(installed) => {
                    let version = installed.version.clone().unwrap_or_default();
                    let heading = tf("G-Earth {version} is installed.", &[("version", &version)]);
                    if installed.verified {
                        Some((ResultKind::Success, heading, t("Verified: the SHA-256 checksum matches the one the release publishes. Press Start to run it.").into()))
                    } else {
                        Some((ResultKind::Warning, heading, t("Not verified: the release publishes no SHA-256 checksum. Press Start to run it.").into()))
                    }
                }
                Outcome::Cancelled => Some((ResultKind::Info, t("The download was cancelled.").into(), t("Nothing was installed.").into())),
                Outcome::Failed(error) => Some((ResultKind::Error, t("G-Earth could not be installed.").into(), error)),
            };
            let mut state = state.lock();
            state.job = Job::Idle;
            state.cancel = None;
            state.result = result;
            state.looked = None;
        });
    }

    fn start(&self, shared: &Shared) {
        let Some(launcher) = self.state.lock().installed.as_ref().map(|i| i.launcher.clone()) else { return };
        {
            let mut state = self.state.lock();
            state.job = Job::Starting;
            state.result = None;
        }
        let state = self.state.clone();
        shared.spawn(async move {
            let started = tokio::task::spawn_blocking(move || gearth::start(&launcher)).await.unwrap_or_else(|e| Err(e.into()));
            let mut state = state.lock();
            state.job = Job::Idle;
            state.looked = None;
            state.result = Some(match started {
                Ok(()) => (
                    ResultKind::Success,
                    t("G-Earth started.").into(),
                    t("Close your Habbo client, press Connect in G-Earth, then open the client and log in.").into(),
                ),
                Err(error) => (ResultKind::Error, t("G-Earth could not be started.").into(), format!("{error:#}")),
            });
        });
    }

    fn remove(&self, shared: &Shared) {
        {
            let mut state = self.state.lock();
            state.job = Job::Removing;
            state.result = None;
        }
        let (state, dialogs, folder) = (self.state.clone(), shared.dialogs.clone(), self.folder.clone());
        shared.spawn(async move {
            let removed = remove_flow(&dialogs, &folder).await;
            let mut state = state.lock();
            state.job = Job::Idle;
            state.looked = None;
            state.result = match removed {
                None => None,
                Some(Ok(())) => Some((ResultKind::Success, t("G-Earth was removed.").into(), String::new())),
                Some(Err(error)) => Some((ResultKind::Error, t("G-Earth could not be removed.").into(), format!("{error:#}"))),
            };
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.refresh(shared, false);
        ui.ctx().request_repaint_after(REFRESH);
        let (installed, running, job, result) = {
            let state = self.state.lock();
            (state.installed.clone(), state.running, state.job, state.result.clone())
        };

        ui.label(widgets::title_text(t("G-Earth"), 15.0));
        ui.add_space(6.0);
        widgets::muted(
            ui,
            palette,
            t("The packet logger the catalogue ripper reads from: the official release from github.com/G-Realm/G-Earth (MIT licence, Java included), kept in the Manager's configuration folder."),
            12.5,
        );
        ui.add_space(12.0);

        let installed_text = match &installed {
            None => t("Not installed").to_string(),
            Some(i) => {
                let version = i.version.clone().unwrap_or_else(|| t("version unknown").to_string());
                if i.verified {
                    tf("{version} (verified)", &[("version", &version)])
                } else {
                    tf("{version} (not verified)", &[("version", &version)])
                }
            }
        };
        let running_text = match running {
            None => t("checking …").to_string(),
            Some(true) => t("Yes, it listens on 127.0.0.1 (ports 9092-9101)").to_string(),
            Some(false) => t("No").to_string(),
        };
        egui::Grid::new("gearth-status").num_columns(2).spacing(egui::vec2(16.0, 6.0)).show(ui, |ui| {
            ui.label(RichText::new(t("Installed")).strong());
            ui.label(installed_text);
            ui.end_row();
            ui.label(RichText::new(t("Folder")).strong());
            let shown = installed.as_ref().map(|i| i.launcher.clone()).unwrap_or_else(|| self.folder.clone());
            ui.add(egui::Label::new(shown.display().to_string()).wrap());
            ui.end_row();
            ui.label(RichText::new(t("Running")).strong());
            ui.label(running_text);
            ui.end_row();
        });
        ui.add_space(12.0);

        match job {
            Job::Idle => {}
            Job::Downloading(done, total) => {
                let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                let text = if total > 0 { format!("{} / {}", megabytes(done), megabytes(total)) } else { megabytes(done) };
                ui.add(egui::ProgressBar::new(fraction).text(tf("Downloading G-Earth: {progress}", &[("progress", &text)])));
                ui.add_space(8.0);
            }
            other => {
                let text = match other {
                    Job::Checking => t("Looking up the newest release …"),
                    Job::Unpacking => t("Unpacking …"),
                    Job::Starting => t("Starting G-Earth …"),
                    _ => t("Waiting for your answer …"),
                };
                widgets::muted(ui, palette, text, 12.5);
                ui.add_space(8.0);
            }
        }

        let idle = job == Job::Idle;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            if ui.add_enabled(idle && installed.is_some(), widgets::primary_button(t("Start"))).clicked() {
                self.start(shared);
            }
            let install_label = if installed.is_some() { t("Check for an update") } else { t("Install G-Earth") };
            if ui.add_enabled(idle, widgets::secondary_button(install_label)).clicked() {
                self.install(shared);
            }
            if matches!(job, Job::Checking | Job::Downloading(..) | Job::Unpacking) && ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                if let Some(cancel) = self.state.lock().cancel.as_ref() {
                    cancel.cancel();
                }
            }
            if ui.add_enabled(self.folder.is_dir(), widgets::secondary_button(t("Open folder"))).clicked() {
                open_folder(&self.folder);
            }
            if ui.add_enabled(idle && self.folder.exists(), widgets::secondary_button(t("Remove"))).clicked() {
                self.remove(shared);
            }
        });

        if let Some((kind, heading, message)) = result {
            ui.add_space(12.0);
            widgets::result_banner(ui, palette, kind, &heading, &message);
        }
    }
}

fn open_folder(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::gearth::tests::{github, good_archive};

    /// Waits for the dialog the flow asks, returns its text, and answers it with choice `answer`.
    async fn answer(dialogs: &Dialogs, answer: usize) -> String {
        for _ in 0..500 {
            if let Some((_, message)) = dialogs.newest_text_for_tests() {
                assert!(dialogs.answer_for_tests(answer));
                return message;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no question was asked");
    }

    #[tokio::test]
    async fn nothing_without_yes() {
        let dialogs = Dialogs::new(egui::Context::default());
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("tools").join("G-Earth");
        let download = format!("/download/{}", gearth::ASSET);
        let cancel = CancellationToken::new();

        for (digest, verified) in [(true, true), (false, false)] {
            let server = github(good_archive(), digest, false, false).await;
            let flow = install_flow(&dialogs, &server.base, &folder, None, &cancel, |_| {});
            let (outcome, question) = tokio::join!(flow, answer(&dialogs, 1));
            assert!(matches!(outcome, Outcome::Declined), "{outcome:?}");
            // The question names the source, the version and the size, and says when it cannot be verified.
            assert!(question.contains("github.com/G-Realm/G-Earth"), "{question}");
            assert!(question.contains("1.5.4"), "{question}");
            assert!(question.contains(" MB"), "{question}");
            assert_eq!(question.contains("Not verified"), !verified, "{question}");
            assert!(!server.asked(&download), "downloaded without a yes");
            assert!(!root.path().join("tools").exists() || std::fs::read_dir(root.path().join("tools")).unwrap().next().is_none());
        }

        // With a yes it is installed; G-Earth is not started by it (Start is a separate button).
        let server = github(good_archive(), true, false, false).await;
        let flow = install_flow(&dialogs, &server.base, &folder, None, &cancel, |_| {});
        let (outcome, _) = tokio::join!(flow, answer(&dialogs, 0));
        let Outcome::Installed(installed) = outcome else { panic!("{outcome:?}") };
        assert!(installed.verified);
        // Already the newest: no question, no download.
        let before = server.hits.lock().len();
        let outcome = install_flow(&dialogs, &server.base, &folder, Some(&installed), &CancellationToken::new(), |_| {}).await;
        assert!(matches!(outcome, Outcome::UpToDate(ref v) if v == "1.5.4"), "{outcome:?}");
        assert!(!server.hits.lock()[before..].iter().any(|p| p == &download));

        // Remove: a no keeps it, a yes deletes it.
        let (removed, _) = tokio::join!(remove_flow(&dialogs, &folder), answer(&dialogs, 1));
        assert!(removed.is_none());
        assert!(gearth::installed(&folder).is_some());
        let (removed, _) = tokio::join!(remove_flow(&dialogs, &folder), answer(&dialogs, 0));
        removed.unwrap().unwrap();
        assert!(!folder.exists());
    }
}
