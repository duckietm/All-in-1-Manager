//! The two dialogs of the Tools page that the shared questions do not cover: the whitelist and bans of Fail2Ban
//! and CrowdSec (WhitelistBansDialog.cs) and the password of the settings backup (PasswordDialog.cs). A tool's task
//! puts one in the page's state and awaits the answer; the page draws it as a modal.

use aio_remote::fail2ban;
use eframe::egui::{self, RichText};
use tokio::sync::oneshot;

use super::SharedState;
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{t, tf};

/// What the whitelist dialog changes: the new whitelist (None: unchanged) and the addresses to let in again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub whitelist: Option<Vec<String>>,
    pub unban: Vec<String>,
}

pub enum Modal {
    Whitelist(WhitelistModal),
    Password(PasswordModal),
}

pub struct WhitelistModal {
    title: String,
    status: String,
    text: String,
    current: Vec<String>,
    configured: bool,
    banned: Vec<(String, bool)>,
    client_ip: Option<String>,
    error: String,
    reply: Option<oneshot::Sender<Option<Changes>>>,
}

pub struct PasswordModal {
    window_title: String,
    heading: String,
    message: String,
    confirm: bool,
    minimum: usize,
    password: String,
    again: String,
    error: String,
    focused: bool,
    reply: Option<oneshot::Sender<Option<String>>>,
}

/// The whitelist and bans dialog; None when cancelled or nothing changed.
pub struct WhitelistQuestion<'a> {
    pub title: &'a str,
    pub status: String,
    pub current: Vec<String>,
    pub configured: bool,
    pub banned: Vec<String>,
    pub client_ip: Option<String>,
}

pub async fn ask_whitelist(state: &SharedState, ctx: &egui::Context, question: WhitelistQuestion<'_>) -> Option<Changes> {
    let (sender, receiver) = oneshot::channel();
    state.lock().unwrap().modal = Some(Modal::Whitelist(WhitelistModal {
        title: question.title.to_string(),
        status: question.status,
        text: question.current.join("\n"),
        current: question.current,
        configured: question.configured,
        banned: question.banned.into_iter().map(|b| (b, false)).collect(),
        client_ip: question.client_ip,
        error: String::new(),
        reply: Some(sender),
    }));
    ctx.request_repaint();
    receiver.await.ok().flatten()
}

/// Asks for a password, twice when `confirm` is set; None when cancelled.
pub async fn ask_password(
    state: &SharedState,
    ctx: &egui::Context,
    window_title: &str,
    heading: &str,
    message: &str,
    confirm: bool,
    minimum: usize,
) -> Option<String> {
    let (sender, receiver) = oneshot::channel();
    state.lock().unwrap().modal = Some(Modal::Password(PasswordModal {
        window_title: window_title.into(),
        heading: heading.into(),
        message: message.into(),
        confirm,
        minimum,
        password: String::new(),
        again: String::new(),
        error: String::new(),
        focused: false,
        reply: Some(sender),
    }));
    ctx.request_repaint();
    receiver.await.ok().flatten()
}

/// What Save does with the dialog's text: Err(problem) keeps it open, Ok(None) closes it without changes.
pub fn whitelist_changes(text: &str, current: &[String], configured: bool, picked: Vec<String>) -> Result<Option<Changes>, String> {
    let (entries, problem) = fail2ban::parse_whitelist(text);
    if let Some(problem) = problem {
        return Err(problem);
    }
    let same = entries.len() == current.len() && entries.iter().zip(current).all(|(a, b)| a.eq_ignore_ascii_case(b));
    let changed = !same || !configured;
    if !changed && picked.is_empty() {
        return Ok(None);
    }
    Ok(Some(Changes { whitelist: changed.then_some(entries), unban: picked }))
}

/// "Add my IP": the address on a new line, unless it is on the list already.
pub fn add_address(text: &str, ip: &str) -> String {
    let text = text.trim_end();
    if fail2ban::parse_whitelist(text).0.iter().any(|e| e == ip) {
        return text.to_string();
    }
    if text.is_empty() {
        ip.to_string()
    } else {
        format!("{text}\n{ip}")
    }
}

/// The problem with a password, or None. (PasswordDialog.Accept)
pub fn password_problem(password: &str, again: &str, confirm: bool, minimum: usize) -> Option<String> {
    if password.chars().count() < minimum {
        return Some(tf("Use at least {minimum} characters.", &[("minimum", &minimum.to_string())]));
    }
    if confirm && password != again {
        return Some(t("The two passwords are not the same.").into());
    }
    None
}

/// Draws the open dialog, if any.
pub fn show(ctx: &egui::Context, palette: &Palette, state: &SharedState) {
    let Some(mut modal) = state.lock().unwrap().modal.take() else { return };
    let open = match &mut modal {
        Modal::Whitelist(dialog) => show_whitelist(ctx, palette, dialog),
        Modal::Password(dialog) => show_password(ctx, palette, dialog),
    };
    if open {
        let mut s = state.lock().unwrap();
        if s.modal.is_none() {
            s.modal = Some(modal);
        }
    }
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(26, 24))
}

/// True while it stays open.
fn show_whitelist(ctx: &egui::Context, palette: &Palette, dialog: &mut WhitelistModal) -> bool {
    let mut answer: Option<Option<Changes>> = None;
    let modal = egui::Modal::new(egui::Id::new("tools-whitelist")).frame(frame(palette)).show(ctx, |ui| {
        ui.set_width(548.0);
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(RichText::new(&dialog.title).size(11.5).color(palette.subtle));
        ui.label(widgets::title_text(t("Whitelist and bans"), 20.0));
        widgets::text(ui, &dialog.status, 13.0);
        ui.label(RichText::new(t("Whitelist (never banned)")).size(12.0).strong().color(palette.muted));
        egui::ScrollArea::vertical().id_salt("whitelist-text").max_height(140.0).show(ui, |ui| {
            ui.add_sized(
                [ui.available_width(), 140.0],
                egui::TextEdit::multiline(&mut dialog.text)
                    .font(egui::TextStyle::Monospace)
                    .hint_text(t("One address or network per line, for example\n203.0.113.7\n192.168.1.0/24")),
            );
        });
        widgets::muted(ui, palette, t("IPv4, IPv6, or a network such as 192.168.1.0/24. Text after # is a comment."), 12.0);
        let label = match &dialog.client_ip {
            Some(ip) => tf("Add my IP ({ip})", &[("ip", ip)]),
            None => t("Add my IP").to_string(),
        };
        if ui.add_enabled(dialog.client_ip.is_some(), widgets::secondary_button(&label)).clicked() {
            if let Some(ip) = &dialog.client_ip {
                dialog.text = add_address(&dialog.text, ip);
            }
        }
        ui.label(RichText::new(t("Banned now: tick to let in again")).size(12.0).strong().color(palette.muted));
        egui::ScrollArea::vertical().id_salt("whitelist-bans").max_height(180.0).show(ui, |ui| {
            if dialog.banned.is_empty() {
                ui.label(RichText::new(t("No address is banned right now.")).size(12.5).color(palette.subtle));
            }
            for (address, ticked) in &mut dialog.banned {
                let text = if dialog.client_ip.as_deref() == Some(address.as_str()) {
                    tf("{address} (this computer)", &[("address", address)])
                } else {
                    address.clone()
                };
                ui.checkbox(ticked, text);
            }
        });
        if !dialog.error.is_empty() {
            ui.label(RichText::new(&dialog.error).size(12.5).color(palette.error_text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui.add(widgets::primary_button(t("Save"))).clicked() {
                let picked = dialog.banned.iter().filter(|(_, ticked)| *ticked).map(|(a, _)| a.clone()).collect();
                match whitelist_changes(&dialog.text, &dialog.current, dialog.configured, picked) {
                    Err(problem) => dialog.error = problem,
                    Ok(changes) => answer = Some(changes),
                }
            }
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                answer = Some(None);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(None);
    }
    match answer {
        Some(changes) => {
            if let Some(reply) = dialog.reply.take() {
                let _ = reply.send(changes);
            }
            false
        }
        None => true,
    }
}

fn password_field<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .password(true)
        .hint_text(hint.to_string())
        .font(egui::FontId::proportional(14.0))
        .desired_width(f32::INFINITY)
        .margin(egui::Margin::symmetric(12, 9))
}

fn show_password(ctx: &egui::Context, palette: &Palette, dialog: &mut PasswordModal) -> bool {
    let mut answer: Option<Option<String>> = None;
    let modal = egui::Modal::new(egui::Id::new("tools-password")).frame(frame(palette)).show(ctx, |ui| {
        ui.set_width(468.0);
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(RichText::new(&dialog.window_title).size(11.5).color(palette.subtle));
        ui.label(widgets::title_text(&dialog.heading, 20.0));
        widgets::text(ui, &dialog.message, 13.0);
        let first = ui.add(password_field(&mut dialog.password, t("Password")));
        if !dialog.focused {
            first.request_focus();
            dialog.focused = true;
        }
        let mut enter = first.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if dialog.confirm {
            let second = ui.add(password_field(&mut dialog.again, t("The same password again")));
            enter |= second.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        }
        if !dialog.error.is_empty() {
            ui.label(RichText::new(&dialog.error).size(12.5).color(palette.error_text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            let ok = ui.add(widgets::primary_button(t("OK"))).clicked();
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                answer = Some(None);
            } else if ok || enter {
                match password_problem(&dialog.password, &dialog.again, dialog.confirm, dialog.minimum) {
                    Some(problem) => dialog.error = problem,
                    None => answer = Some(Some(dialog.password.clone())),
                }
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(None);
    }
    match answer {
        Some(password) => {
            if let Some(reply) = dialog.reply.take() {
                let _ = reply.send(password);
            }
            false
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_without_changes_closes() {
        let current = vec!["203.0.113.7".to_string()];
        assert_eq!(whitelist_changes("203.0.113.7", &current, true, vec![]), Ok(None));
        // Not set up by the installer yet: the whitelist is saved even when it is the same.
        assert_eq!(
            whitelist_changes("203.0.113.7", &current, false, vec![]),
            Ok(Some(Changes { whitelist: Some(current.clone()), unban: vec![] }))
        );
        assert_eq!(
            whitelist_changes("203.0.113.7", &current, true, vec!["1.2.3.4".into()]),
            Ok(Some(Changes { whitelist: None, unban: vec!["1.2.3.4".into()] }))
        );
        let changed = whitelist_changes("203.0.113.7\n192.168.1.0/24", &current, true, vec![]).unwrap().unwrap();
        assert_eq!(changed.whitelist.unwrap().len(), 2);
        assert!(whitelist_changes("not an address", &current, true, vec![]).is_err());
    }

    #[test]
    fn add_my_ip_once() {
        assert_eq!(add_address("", "1.2.3.4"), "1.2.3.4");
        assert_eq!(add_address("10.0.0.1\n", "1.2.3.4"), "10.0.0.1\n1.2.3.4");
        assert_eq!(add_address("1.2.3.4", "1.2.3.4"), "1.2.3.4");
    }

    #[test]
    fn password_checks() {
        assert_eq!(password_problem("short", "", false, 8).as_deref(), Some("Use at least 8 characters."));
        assert_eq!(password_problem("long enough", "other", true, 8).as_deref(), Some("The two passwords are not the same."));
        assert_eq!(password_problem("long enough", "long enough", true, 8), None);
        assert_eq!(password_problem("x", "", false, 1), None);
    }
}
