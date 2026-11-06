//! Puts an SSH key in /root/.ssh/authorized_keys: a key pair generated here (saved in the configuration directory)
//! or a public key the user provides. Asks first when keys are already installed. (SshKeyStep.cs,
//! ExistingKeysDialog.cs)
//!
//! The wizard's key step (index 0): `Done` means the key was installed, then `Failed` and `Aborted`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_core::config_dir;
use aio_core::keys::{self, AuthorizedKey, SshKeyType};
use aio_remote::root_authorized_keys::{self, AuthorizedKeysMode, FILE_PATH};
use aio_remote::{fail, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::dialogs::Choice;
use crate::gui::icons::{self, Icon};
use crate::gui::step::{self, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::i18n::{n, t, tf};

const MINIMUM_PASSPHRASE_LENGTH: usize = 8;
const KEY_TYPES: [&str; 2] = [n("Ed25519 (recommended)"), n("RSA 4096 (older clients)")];

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(SshKeyStep { form: Arc::new(Mutex::new(Form { generate: true, ..Default::default() })) })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingKeysChoice {
    Replace,
    Add,
    Abort,
}

/// What the user filled in on the intro.
#[derive(Default)]
struct Form {
    generate: bool,
    /// 0: Ed25519, 1: RSA 4096.
    key_type: usize,
    comment: String,
    passphrase: String,
    passphrase_repeat: String,
    public_key: String,
    /// A problem with the loaded .pub file (C#: the status line).
    browse_problem: Option<String>,
    /// The public key field takes the focus once after "Use my own public key" is chosen or a file was loaded.
    focus_public_key: bool,
}

pub struct SshKeyStep {
    form: Arc<Mutex<Form>>,
}

/// The settings of a run, copied from the form when it starts.
enum KeySource {
    Generate { key_type: SshKeyType, comment: String, passphrase: String },
    Provide(String),
}

/// What is wrong with the form, or None. (Validate)
fn validate_form(form: &Form) -> Option<String> {
    if form.generate {
        let length = form.passphrase.chars().count();
        if length == 0 {
            return Some(t("Set a password for the new key.").into());
        }
        if length < MINIMUM_PASSPHRASE_LENGTH {
            return Some(tf(
                "The key password must be at least {count} characters.",
                &[("count", &MINIMUM_PASSPHRASE_LENGTH.to_string())],
            ));
        }
        if form.passphrase != form.passphrase_repeat {
            return Some(t("The two key passwords are not the same.").into());
        }
        return None;
    }
    keys::try_parse_public_key(&form.public_key).err()
}

/// The question's message: what authorized_keys holds, as ExistingKeysDialog lists it.
fn existing_keys_message(existing: &[AuthorizedKey], new_key: &AuthorizedKey) -> String {
    let already_there = existing.iter().any(|k| k.same_key(new_key));
    let mut message = if existing.len() == 1 {
        tf("{path} on the server already holds 1 key", &[("path", FILE_PATH)])
    } else {
        tf("{path} on the server already holds {count} keys", &[("path", FILE_PATH), ("count", &existing.len().to_string())])
    };
    message.push_str(if already_there { t(", including the key you are installing.") } else { "." });
    message.push(' ');
    message.push_str(t("What do you want to do?"));
    message.push('\n');
    for key in existing {
        let comment = if key.comment.is_empty() { t("(no comment)").to_string() } else { key.comment.clone() };
        let mut line = format!("\n{comment}   [{}]", key.key_type.replace("ssh-", "").to_uppercase());
        if key.same_key(new_key) {
            line.push_str(&format!("  [{}]", t("THIS KEY")));
        }
        message.push_str(&line);
        message.push_str(&format!("\n    {}", key.fingerprint()));
    }
    message
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_owned();
    text.push(suffix);
    PathBuf::from(text)
}

/// The ssh command that tests the key. (C#: "ssh -p <port> root@host" when the port is not 22)
fn ssh_command(host: &str, port: u16) -> String {
    if port == 22 {
        format!("ssh root@{host}")
    } else {
        format!("ssh -p {port} root@{host}")
    }
}

fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

/// Picks a .pub file (in a thread: the dialog blocks) and puts its text in the form.
fn browse(form: Arc<Mutex<Form>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new()
            .set_title(t("Select your public key (.pub)"))
            .add_filter(t("Public keys"), &["pub"])
            .add_filter(t("All files"), &["*"]);
        if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            let ssh = PathBuf::from(home).join(".ssh");
            if ssh.is_dir() {
                dialog = dialog.set_directory(ssh);
            }
        }
        let Some(path) = dialog.pick_file() else { return };
        let mut form_state = form.lock().unwrap();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
            form_state.browse_problem = Some(t("That file is too large to be a public key.").into());
        } else {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    form_state.public_key = String::from_utf8_lossy(&bytes).trim().to_string();
                    form_state.generate = false;
                    form_state.browse_problem = None;
                    form_state.focus_public_key = true;
                }
                Err(error) => form_state.browse_problem = Some(error.to_string()),
            }
        }
        drop(form_state);
        ctx.request_repaint();
    });
}

fn option_label(ui: &mut egui::Ui, palette: &Palette, selected: &mut bool, value: bool, title: &str, caption: &str) -> bool {
    let mut clicked = false;
    ui.horizontal_top(|ui| {
        clicked |= ui.radio(*selected == value, "").clicked();
        ui.vertical(|ui| {
            clicked |= ui.add(egui::Label::new(RichText::new(title).size(14.0).strong()).sense(egui::Sense::click())).clicked();
            clicked |= ui.add(egui::Label::new(RichText::new(caption).size(12.5).color(palette.muted)).sense(egui::Sense::click())).clicked();
        });
    });
    if clicked {
        *selected = value;
    }
    clicked
}

fn labelled(ui: &mut egui::Ui, palette: &Palette, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(12.0).color(palette.muted).strong());
        ui.add_space(3.0);
        add(ui);
    });
}

impl SshKeyStep {
    fn clear_passwords(&self) {
        let mut form = self.form.lock().unwrap();
        form.passphrase.clear();
        form.passphrase_repeat.clear();
    }

    async fn run_key(&self, server: &dyn Server, ui: &StepUi, source: KeySource) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let host = server.login().host.clone();
        let mut pair = None;
        let mut saved_private_key: Option<PathBuf> = None;
        let mut passphrase = String::new();
        let key = match source {
            KeySource::Generate { key_type, comment, passphrase: secret } => {
                log(&format!(
                    "> Generating a new {} key pair ...",
                    if key_type == SshKeyType::Ed25519 { "Ed25519" } else { "RSA 4096" }
                ));
                passphrase = secret.clone();
                let generated = tokio::task::spawn_blocking(move || keys::generate(key_type, &comment, &secret)).await?;
                let key = generated.public_key.clone();
                // Saved before the server is touched, so a key on the server always has its private key here.
                let path = config_dir::save_key_pair(&host, &generated)?;
                log(&format!("  Private key: {} (encrypted with your key password)", path.display()));
                log(&format!("  Public key:  {}.pub", path.display()));
                log(&format!("  PuTTY key:   {}.ppk (same key and password, for HeidiSQL/plink, Pageant and WinSCP)", path.display()));
                saved_private_key = Some(path);
                pair = Some(generated);
                key
            }
            KeySource::Provide(text) => {
                let key = keys::try_parse_public_key(&text).map_err(aio_remote::ServerError)?;
                let path = config_dir::save_public_key(&host, &key)?;
                log(&format!("> Using your {} public key. Copy saved as {}", key.key_type, path.display()));
                key
            }
        };
        log(&format!("  Fingerprint: {}", key.fingerprint()));
        ui.done(&key.fingerprint());

        log(&format!("> Reading {FILE_PATH} ..."));
        let existing = root_authorized_keys::read(server).await?;
        log(&if existing.is_empty() { "  No keys installed yet.".to_string() } else { format!("  {} key(s) already installed:", existing.len()) });
        for k in &existing {
            log(&format!("    {} {} {}", k.key_type, k.fingerprint(), k.comment));
        }

        let mut mode = AuthorizedKeysMode::Add;
        if !existing.is_empty() {
            let already_there = existing.iter().any(|k| k.same_key(&key));
            let choice = ui
                .choose(
                    t("SSH keys already present"),
                    t("Some keys are already present"),
                    &existing_keys_message(&existing, &key),
                    ExistingKeysChoice::Abort,
                    vec![
                        Choice::new(
                            t("Clean all keys and place this one"),
                            t("Every key above is removed; only the new key can log in as root. A backup of the file is kept on the server."),
                            ExistingKeysChoice::Replace,
                        ),
                        // The safe choice is the highlighted one; removing keys can lock other people out.
                        Choice::new(
                            t("Add my key to it"),
                            if already_there {
                                t("Keeps all keys. Your key is already there, so nothing changes.")
                            } else {
                                t("Keeps all keys and adds the new one, so there are multiple keys.")
                            },
                            ExistingKeysChoice::Add,
                        )
                        .primary(),
                        Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), ExistingKeysChoice::Abort),
                    ],
                )
                .await;
            if choice == ExistingKeysChoice::Abort {
                log("! Installation aborted by the user. Nothing was changed on the server.");
                if let Some(path) = &saved_private_key {
                    // The new key never reached the server; do not leave an unused key pair behind.
                    let _ = std::fs::remove_file(path);
                    let _ = std::fs::remove_file(with_suffix(path, ".pub"));
                    let _ = std::fs::remove_file(with_suffix(path, ".ppk"));
                    log("  The unused key pair was removed from the configuration directory.");
                }
                ui.fail();
                ui.result(ResultKind::Warning, t("Installation aborted"), &tf("Nothing was changed on {host}.", &[("host", &host)]));
                ui.set_status(t("Installation aborted."));
                return Ok(StepOutcome::Aborted);
            }
            mode = if choice == ExistingKeysChoice::Replace { AuthorizedKeysMode::Replace } else { AuthorizedKeysMode::Add };
            log(if mode == AuthorizedKeysMode::Replace {
                "  Chosen: remove all keys and place the new one."
            } else {
                "  Chosen: add the new key to the existing ones."
            });
        }
        ui.done(&if existing.is_empty() { t("none yet").to_string() } else { tf("{count} found", &[("count", &existing.len().to_string())]) });

        log(&format!("> Installing the key in {FILE_PATH} ..."));
        if let Some(backup) = root_authorized_keys::install(server, &key, mode).await? {
            log(&format!("  Backup of the old file: {backup}"));
        }
        let now = root_authorized_keys::read(server).await?;
        if !now.iter().any(|k| k.same_key(&key)) {
            fail!("The key is not in authorized_keys after writing it.");
        }
        log(&format!("  Done. {} key(s) installed now.", now.len()));
        ui.done(&if mode == AuthorizedKeysMode::Replace {
            t("replaced all keys").to_string()
        } else {
            tf("{count} key(s) installed", &[("count", &now.len().to_string())])
        });

        // Key login can only be tested when the private key was generated here.
        let Some(pair) = pair else {
            log("> Key login test skipped: the private key stays on your computer.");
            ui.done(t("skipped (your own key)"));
            return Ok(ui.succeeded(
                t("SSH key installed"),
                &tf(
                    "Your key can now log in as root on {host}. Test it with: {command}",
                    &[("host", &host), ("command", &ssh_command(&host, server.login().port))],
                ),
            ));
        };

        log("> Testing key login as root ...");
        let problem = root_authorized_keys::test_key_login(server, &pair.private_key_pem, &passphrase).await;
        let Some(problem) = problem else {
            log("  Logged in as root with the new key.");
            ui.done(t("works"));
            return Ok(ui.succeeded(
                t("SSH key installed and tested"),
                &tf(
                    "The new key logs in as root on {host}. It is stored in {path}.",
                    &[("host", &host), ("path", &config_dir::keys_directory(&host).display().to_string())],
                ),
            ));
        };

        let permit = root_authorized_keys::permit_root_login(server).await?;
        let hint = if permit.as_deref() == Some("no") {
            t("The server's sshd has 'PermitRootLogin no', so root cannot log in over SSH at all.")
        } else {
            t("Check the sshd settings (PubkeyAuthentication, AuthorizedKeysFile) on the server.")
        };
        log(&format!("! Key login failed: {problem}. {hint}"));
        ui.fail();
        Ok(ui.succeeded_with_warning(t("Key installed, but key login did not work"), &format!("{problem}. {hint}")))
    }
}

#[async_trait]
impl InstallStep for SshKeyStep {
    fn title(&self) -> String {
        t("SSH key").into()
    }

    fn caption(&self) -> String {
        "authorized_keys".into()
    }

    fn install_label(&self) -> String {
        t("Install key").into()
    }

    fn rerun_label(&self) -> String {
        t("Install another key").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Key,
            heading: t("SSH key for root").into(),
            description: tf(
                "The key is placed in {path}, so you (and the installer) can log in as root without a password.",
                &[("path", FILE_PATH)],
            ),
            bullets: Vec::new(),
            footnote: None,
        }
    }

    /// Back to the choice between generating a key and providing one. (ShowOptions)
    fn enter(&self, server: &dyn Server) {
        let mut form = self.form.lock().unwrap();
        if form.comment.trim().is_empty() {
            form.comment = format!("all-in-1-manager@{}", server.login().host);
        }
        form.browse_problem = None;
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Prepare the key").into(),
            t("Check installed keys").into(),
            t("Install the key").into(),
            t("Test key login as root").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        ui.add_space(4.0);
        option_label(
            ui,
            palette,
            &mut form.generate,
            true,
            t("Generate a new key pair"),
            t("Creates the keys here and stores them in the configuration directory (OpenSSH and PuTTY .ppk)."),
        );
        if form.generate {
            ui.add_space(4.0);
            egui::Frame::new().inner_margin(egui::Margin { left: 30, ..Default::default() }).show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    labelled(ui, palette, t("Key type"), |ui| {
                        egui::ComboBox::from_id_salt("ssh-key-type")
                            .width(260.0)
                            .selected_text(t(KEY_TYPES[form.key_type]))
                            .show_ui(ui, |ui| {
                                for (index, name) in KEY_TYPES.iter().enumerate() {
                                    ui.selectable_value(&mut form.key_type, index, t(name));
                                }
                            });
                    });
                    ui.add_space(12.0);
                    labelled(ui, palette, t("Comment"), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut form.comment).desired_width(320.0));
                    });
                });
                ui.add_space(10.0);
                ui.horizontal_top(|ui| {
                    labelled(ui, palette, t("Key password (passphrase)"), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.passphrase)
                                .password(true)
                                .hint_text(t("At least 8 characters"))
                                .desired_width(260.0),
                        );
                    });
                    ui.add_space(12.0);
                    labelled(ui, palette, t("Repeat password"), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.passphrase_repeat)
                                .password(true)
                                .hint_text(t("Type it again"))
                                .desired_width(260.0),
                        );
                    });
                });
                ui.add_space(10.0);
                ui.scope(|ui| {
                    ui.set_max_width(640.0);
                    widgets::muted(
                        ui,
                        palette,
                        t("The private key is encrypted with this password. You need it every time you use the key; it is not stored anywhere, so keep it safe."),
                        12.0,
                    );
                });
            });
        }
        ui.add_space(12.0);
        if option_label(
            ui,
            palette,
            &mut form.generate,
            false,
            t("Use my own public key"),
            t("Paste your public key (id_ed25519.pub, id_rsa.pub ...) or load the file."),
        ) {
            form.focus_public_key = true;
        }
        if !form.generate {
            ui.add_space(4.0);
            egui::Frame::new().inner_margin(egui::Margin { left: 30, ..Default::default() }).show(ui, |ui| {
                let field = ui.add(
                    egui::TextEdit::multiline(&mut form.public_key)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... you@computer"),
                );
                if form.focus_public_key {
                    field.request_focus();
                    form.focus_public_key = false;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(widgets::secondary_button(t("Load .pub file..."))).clicked() {
                        browse(self.form.clone(), ui.ctx().clone());
                    }
                    ui.add_space(10.0);
                    widgets::muted(ui, palette, t("Never paste a private key: only the public key goes to the server."), 12.0);
                });
                if let Some(problem) = &form.browse_problem {
                    ui.label(RichText::new(problem.as_str()).size(12.0).color(palette.error_text));
                }
            });
        }
        ui.add_space(12.0);
        let directory = config_dir::directory();
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Folder, 16.0, palette.muted);
            ui.add_space(6.0);
            widgets::muted(ui, palette, &tf("Keys are stored in {path}", &[("path", &directory.display().to_string())]), 12.0);
            ui.add_space(10.0);
            if ui.add(widgets::secondary_button(t("Open folder"))).clicked() {
                open_folder(&directory);
            }
        });
    }

    fn validate(&self) -> Option<String> {
        validate_form(&self.form.lock().unwrap())
    }

    /// True when a key this installer saved for the host (configuration/keys/<host>) is in authorized_keys.
    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let host = server.login().host.clone();
        let installed = match root_authorized_keys::read(server.as_ref()).await {
            Ok(installed) => installed,
            Err(error) => {
                ui.detected(ResultKind::Warning, t("Could not check the installed keys"), &step::error_message(&error));
                return Ok(false);
            }
        };
        let saved = config_dir::saved_public_keys(&host);
        if let Some((path, key)) = saved.iter().find(|(_, s)| installed.iter().any(|k| k.same_key(s))) {
            let others = if installed.len() > 1 {
                tf(", together with {count} other key(s).", &[("count", &(installed.len() - 1).to_string())])
            } else {
                ".".to_string()
            };
            ui.detected(
                ResultKind::Success,
                t("An SSH key is already installed"),
                &format!(
                    "{}{others} {}",
                    tf(
                        "{file} ({fingerprint}) from the configuration folder is in {path}",
                        &[
                            ("file", &path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                            ("fingerprint", &key.fingerprint()),
                            ("path", FILE_PATH),
                        ],
                    ),
                    t("You can skip this step.")
                ),
            );
            return Ok(true);
        }
        if installed.is_empty() {
            ui.detected(
                ResultKind::Info,
                t("No SSH key installed yet"),
                t("Root has no keys on this server. Install one now, or skip and keep using the password."),
            );
        } else {
            ui.detected(
                ResultKind::Info,
                &tf("{count} key(s) already installed", &[("count", &installed.len().to_string())]),
                &tf(
                    "{path} has keys, but none from this installer's configuration folder. Skip if one of them is yours, or install a key.",
                    &[("path", FILE_PATH)],
                ),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let source = {
            let form = self.form.lock().unwrap();
            if form.generate {
                KeySource::Generate {
                    key_type: if form.key_type == 1 { SshKeyType::Rsa4096 } else { SshKeyType::Ed25519 },
                    comment: form.comment.clone(),
                    passphrase: form.passphrase.clone(),
                }
            } else {
                KeySource::Provide(form.public_key.clone())
            }
        };
        let result = self.run_key(server.as_ref(), &ui, source).await;
        // The key password is only needed for this run; do not keep it in the form.
        self.clear_passwords();
        // The failure as the C# shows it: "Installing the SSH key failed".
        Ok(result.unwrap_or_else(|error| {
            let message = step::error_message(&error);
            ui.log(&format!("! {message}"));
            ui.fail();
            ui.result(ResultKind::Error, t("Installing the SSH key failed"), &message);
            ui.set_status(&format!("{}.", t("Installing the SSH key failed")));
            StepOutcome::Failed
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(generate: bool, passphrase: &str, repeat: &str, public_key: &str) -> Form {
        Form {
            generate,
            passphrase: passphrase.into(),
            passphrase_repeat: repeat.into(),
            public_key: public_key.into(),
            ..Default::default()
        }
    }

    #[test]
    fn validates_the_key_password() {
        assert_eq!(validate_form(&form(true, "", "", "")).as_deref(), Some("Set a password for the new key."));
        assert_eq!(validate_form(&form(true, "short", "short", "")).as_deref(), Some("The key password must be at least 8 characters."));
        assert_eq!(validate_form(&form(true, "long enough", "long enougH", "")).as_deref(), Some("The two key passwords are not the same."));
        assert_eq!(validate_form(&form(true, "long enough", "long enough", "")), None);
    }

    #[test]
    fn validates_the_public_key() {
        assert_eq!(validate_form(&form(false, "", "", "")).as_deref(), Some("Paste a public key or load a .pub file."));
        let pair = keys::generate(SshKeyType::Ed25519, "me@pc", "password1");
        assert_eq!(validate_form(&form(false, "", "", &pair.public_key.line())), None);
    }

    #[test]
    fn lists_the_existing_keys() {
        let ours = AuthorizedKey::new("ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAIOurs", "me@pc");
        let other = AuthorizedKey::new("ssh-rsa", "AAAAB3NzaC1yc2EAAAADAQABAAAB", "");
        let message = existing_keys_message(&[other.clone(), ours.clone()], &ours);
        assert!(message.starts_with("/root/.ssh/authorized_keys on the server already holds 2 keys, including the key you are installing. What do you want to do?"));
        assert!(message.contains("(no comment)   [RSA]"));
        assert!(message.contains("me@pc   [ED25519]  [THIS KEY]"));
        assert!(existing_keys_message(&[other], &ours).contains("holds 1 key. What"));
    }

    #[test]
    fn ssh_command_with_port() {
        assert_eq!(ssh_command("h", 22), "ssh root@h");
        assert_eq!(ssh_command("h", 2222), "ssh -p 2222 root@h");
    }
}
