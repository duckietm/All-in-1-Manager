//! Optionally turns SSH password login off, so only SSH keys get in; after testing that the key works.
//! (SshLoginStep.cs)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aio_core::keys::{self, AuthorizedKey};
use aio_core::{config_dir, InstallerSettings};
use aio_remote::ssh_login::{self, DROP_IN};
use aio_remote::{fail, Server, ServerLogin};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::i18n::{t, tf};

/// A new SSH login with `login` next to this session, closed again at once; returns the user it logged in as. In
/// dry-run mode it goes to the recorder, so nothing reaches a server. (ServerConnection.ConnectAsync + dispose)
pub(crate) async fn fresh_login(server: &dyn Server, login: ServerLogin) -> anyhow::Result<String> {
    if server.is_dry_run() {
        let check = crate::gui::dry_run::connect(login);
        let user = check.login_user();
        check.disconnect().await;
        return Ok(user);
    }
    let check = aio_remote::ssh::connect(login, &|_| {}, &|_| {}).await?;
    let user = check.login_user();
    check.disconnect().await;
    Ok(user)
}

struct Options {
    keys_only: bool,
    understood: bool,
    key_password: String,
    keys: String,
}

pub struct SshLoginStep {
    options: Mutex<Options>,
}

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(SshLoginStep {
        options: Mutex::new(Options { keys_only: false, understood: false, key_password: String::new(), keys: String::new() }),
    })
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Private keys this computer has for the server: saved by the SSH key step, the session's own, and the default
/// from Settings; without duplicates, only those that exist.
fn private_keys(login: &ServerLogin) -> Vec<String> {
    let mut keys: Vec<String> = config_dir::saved_private_keys(&login.host).iter().map(|p| p.to_string_lossy().into_owned()).collect();
    keys.extend(login.private_keys.iter().cloned());
    if let Some(preset) = InstallerSettings::load().default_key_path() {
        keys.push(preset.to_string_lossy().into_owned());
    }
    distinct_existing(keys)
}

fn distinct_existing(keys: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for key in keys {
        if !seen.contains(&key) && Path::new(&key).is_file() {
            seen.push(key);
        }
    }
    seen
}

fn public_keys(private_keys: &[String]) -> Vec<AuthorizedKey> {
    private_keys
        .iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(PathBuf::from(format!("{path}.pub"))).ok()?;
            keys::try_parse_public_key(&text).ok()
        })
        .collect()
}

fn names(keys: &[String]) -> String {
    keys.iter().map(|k| file_name(k)).collect::<Vec<_>>().join(", ")
}

/// Logs in again as root with the keys. (TestKeyLoginAsync)
async fn test_key_login(server: &dyn Server, ui: &StepUi, keys: &[String], password: &str) -> anyhow::Result<()> {
    let login = server.login();
    ui.log(&format!("> New SSH login to {}:{} as root with the key ...", login.host, login.port));
    let test = ServerLogin::key(&login.host, login.port, "root", keys.to_vec(), password);
    let user = fresh_login(server, test).await?;
    ui.log(&format!("  Logged in as {user} with the key."));
    Ok(())
}

#[async_trait]
impl InstallStep for SshLoginStep {
    fn title(&self) -> String {
        t("SSH login").into()
    }

    fn caption(&self) -> String {
        t("Keys only (optional)").into()
    }

    fn install_label(&self) -> String {
        t("Apply").into()
    }

    fn rerun_label(&self) -> String {
        t("Apply again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Key,
            heading: t("SSH login with keys only").into(),
            description: t("Optional: turn SSH password login off, so nobody can log in by guessing a password. You decide.").into(),
            bullets: vec![
                t("First logs in again as root with your SSH key, in a separate session: when that fails, nothing is changed.").into(),
                t("Writes /etc/ssh/sshd_config.d/00-all-in-1-keys-only.conf: PasswordAuthentication no, KbdInteractiveAuthentication no, PermitRootLogin prohibit-password. sshd -t checks it before sshd reloads.").into(),
                t("Logs in with the key once more afterwards; when that fails, password login is put back automatically.").into(),
                t("Choose \"Keep password login\" to leave it as it is, or to allow passwords again after turning them off.").into(),
            ],
            footnote: Some(t("This session stays connected while sshd reloads.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server and your key").into(),
            t("Log in with the key (before)").into(),
            t("sshd: keys only").into(),
            t("Log in with the key (after)").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut guard = self.options.lock();
        let o = &mut *guard;
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.radio_value(&mut o.keys_only, false, t("Keep password login (or allow it again)"));
        ui.radio_value(&mut o.keys_only, true, t("Turn password login off: only SSH keys can log in"));
        widgets::result_banner(
            ui,
            palette,
            ResultKind::Warning,
            t("Keep your SSH key safe"),
            t("With password login off, you can only log in with your SSH key (and its password). Lose the key and the only way in is the console of your hosting provider (KVM / VNC / rescue mode). Keep a copy: Tools → Export encrypted settings."),
        );
        if o.keys_only {
            ui.checkbox(&mut o.understood, t("I understand: without my SSH key the only way in is the console of my hosting provider (KVM)"));
        }
        ui.label(RichText::new(t("Password of your SSH key (to test the key login)")).size(12.0).strong().color(palette.muted));
        ui.add(
            egui::TextEdit::singleline(&mut o.key_password)
                .password(true)
                .hint_text(t("Password of your SSH key (empty when it has none)"))
                .desired_width(380.0),
        );
        if !o.keys.is_empty() {
            ui.add(egui::Label::new(RichText::new(&o.keys).size(12.0).color(palette.subtle)).wrap());
        }
    }

    fn validate(&self) -> Option<String> {
        let o = self.options.lock();
        (o.keys_only && !o.understood).then(|| {
            t("Tick that you understand the warning: without your SSH key the only way in is the console of your hosting provider.").into()
        })
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let keys = private_keys(server.login());
        let state = ssh_login::inspect(server.as_ref(), &public_keys(&keys)).await?;
        {
            let mut o = self.options.lock();
            o.keys = if keys.is_empty() {
                t("No SSH key for this server on this computer (configuration/keys): install one in the SSH key step first.").into()
            } else {
                tf("Keys on this computer for this server: {keys}.", &[("keys", &names(&keys))])
            };
            if state.keys_only() {
                o.keys_only = true;
            }
        }
        if state.keys_only() {
            ui.detected(
                ResultKind::Success,
                t("Password login is off: only SSH keys can log in"),
                &tf(
                    "PermitRootLogin {value}. You can skip this step, or choose \"Keep password login\" to allow passwords again.",
                    &[("value", state.permit_root_login.as_deref().unwrap_or(""))],
                ),
            );
            return Ok(true);
        }
        ui.detected(
            if state.root_has_saved_key { ResultKind::Info } else { ResultKind::Warning },
            t("SSH password login is on"),
            if state.root_has_saved_key {
                t("Your SSH key is in root's authorized_keys, so it can be turned off. Or keep it, and skip this step.")
            } else {
                t("None of the keys on this computer is in root's authorized_keys yet: run the SSH key step first if you want keys only.")
            },
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let (keep, password) = {
            let o = self.options.lock();
            (!o.keys_only, o.key_password.clone())
        };
        log("> Checking the server ...");
        let keys = private_keys(server.login());
        let state = ssh_login::inspect(server, &public_keys(&keys)).await?;
        let on_off = |on: bool| if on { "on" } else { "off" };
        log(&format!(
            "  Password login: {}, keyboard-interactive: {}, PermitRootLogin {}",
            on_off(state.password_login),
            on_off(state.keyboard_interactive),
            state.permit_root_login.as_deref().unwrap_or("")
        ));

        if keep {
            ui.done(if state.keys_only() { t("keys only now") } else { t("password login on") });
            ui.done(t("not needed"));
            ssh_login::allow_password_login(server, &log).await?;
            ui.done(t("password login as configured"));
            ui.done(t("not needed"));
            return Ok(ui.succeeded(
                t("SSH password login is allowed"),
                if state.our_file {
                    t("The keys-only setting of this installer is removed.")
                } else {
                    t("Nothing was changed: password login stays as it was.")
                },
            ));
        }

        if !state.root_has_saved_key {
            fail!("None of your SSH keys on this computer is in root's authorized_keys. Run the SSH key step first, so you can still log in afterwards.");
        }
        log(&format!("  Keys to test: {}", names(&keys)));
        ui.done(&tf("{count} key(s)", &[("count", &keys.len().to_string())]));

        test_key_login(server, &ui, &keys, &password).await?;
        ui.done(t("root with key"));

        ssh_login::disable_password_login(server, &log).await?;
        ui.done("PasswordAuthentication no");

        if let Err(error) = test_key_login(server, &ui, &keys, &password).await {
            log("! The key login failed after the change: putting password login back.");
            ssh_login::allow_password_login(server, &log).await?;
            return Err(error);
        }
        let login = server.login().clone();
        let mut refused = String::new();
        if !login.uses_key() && !login.password.is_empty() {
            log("> A password login should now be refused ...");
            match fresh_login(server, login).await {
                Ok(_) => {
                    log("  The password still logs in!");
                    refused = format!(" {}", t("Warning: a password login still worked; check sshd on the server."));
                }
                Err(_) => log("  Refused, as it should be."),
            }
        }
        ui.done(t("root with key"));
        let message = tf(
            "Only SSH keys can log in now. Keep a copy of your key (Tools → Export encrypted settings). To allow passwords again: run this step with \"Keep password login\", or delete {file} on the server console.",
            &[("file", DROP_IN)],
        );
        if refused.is_empty() {
            Ok(ui.succeeded(t("Password login is off"), &message))
        } else {
            Ok(ui.succeeded_with_warning(t("Password login is off"), &format!("{message}{refused}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_distinct_and_existing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("id_a");
        std::fs::write(&a, "x").unwrap();
        let a = a.to_string_lossy().into_owned();
        let missing = dir.path().join("missing").to_string_lossy().into_owned();
        assert_eq!(distinct_existing(vec![a.clone(), missing, a.clone()]), vec![a.clone()]);
        assert_eq!(names(&[a]), "id_a");
    }
}
