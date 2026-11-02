//! Asks for a MariaDB account for the SQL editor (DatabaseLoginDialog.cs). The opening task waits for the answer;
//! the view draws the dialog as a modal.

use eframe::egui::{self, RichText};
use tokio::sync::oneshot;

use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{t, tf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub user: String,
    pub password: String,
    pub port: u16,
}

/// The dialog on screen.
pub struct Prompt {
    host: String,
    user: String,
    password: String,
    port: String,
    error: String,
    focus_password: bool,
    focused: bool,
    reply: Option<oneshot::Sender<Option<Login>>>,
}

impl Prompt {
    pub fn new(host: &str, initial: &Login, problem: Option<String>) -> (Self, oneshot::Receiver<Option<Login>>) {
        let (sender, receiver) = oneshot::channel();
        let prompt = Prompt {
            host: host.to_string(),
            user: initial.user.clone(),
            password: initial.password.clone(),
            port: initial.port.to_string(),
            error: problem.unwrap_or_default(),
            focus_password: initial.password.is_empty(),
            focused: false,
            reply: Some(sender),
        };
        (prompt, receiver)
    }

    /// Draws the dialog; true when it closed (answered or cancelled).
    pub fn show(&mut self, ctx: &egui::Context, palette: &Palette) -> bool {
        let mut answer: Option<Option<Login>> = None;
        let modal = egui::Modal::new(egui::Id::new("sql-mariadb-login")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(26, 24))).show(ctx, |ui| {
            ui.set_width(468.0);
            ui.label(RichText::new(t("MariaDB login")).size(11.5).color(palette.subtle));
            ui.add_space(4.0);
            ui.label(widgets::title_text(t("Log in to MariaDB"), 20.0));
            ui.add_space(6.0);
            ui.label(RichText::new(tf(
                "The SQL editor connects through the SSH session to 127.0.0.1 on {host}, like HeidiSQL with an SSH tunnel. The installer's root password is in configuration/database when this computer installed MariaDB.",
                &[("host", &self.host)],
            )).size(13.0));
            ui.add_space(10.0);
            let mut enter = false;
            let mut escape = false;
            let mut field = |ui: &mut egui::Ui, response: egui::Response| {
                if response.lost_focus() {
                    enter |= ui.input(|i| i.key_pressed(egui::Key::Enter));
                    escape |= ui.input(|i| i.key_pressed(egui::Key::Escape));
                }
                response
            };
            let (user, port) = ui
                .horizontal(|ui| {
                    let port_width = 110.0;
                    let user = ui.add(egui::TextEdit::singleline(&mut self.user).hint_text(t("User")).font(egui::FontId::proportional(14.0)).margin(egui::Margin::symmetric(12, 9)).desired_width(ui.available_width() - port_width - 8.0));
                    let user = field(ui, user);
                    let port = ui.add(egui::TextEdit::singleline(&mut self.port).hint_text("3306").font(egui::FontId::proportional(14.0)).margin(egui::Margin::symmetric(12, 9)).desired_width(port_width - 24.0));
                    let port = field(ui, port);
                    (user, port)
                })
                .inner;
            ui.add_space(6.0);
            let password = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text(t("Password")).font(egui::FontId::proportional(14.0)).margin(egui::Margin::symmetric(12, 9)).desired_width(f32::INFINITY));
            let password = field(ui, password);
            let _ = port;
            if !self.focused {
                self.focused = true;
                if self.focus_password { password.request_focus() } else { user.request_focus() }
            }
            if !self.error.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&self.error).size(12.5).color(palette.error_text));
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = ui.add(widgets::primary_button(t("Connect")).min_size(egui::vec2(96.0, 34.0))).clicked();
                let cancel = ui.add(widgets::secondary_button(t("Cancel")).min_size(egui::vec2(80.0, 34.0))).clicked();
                if cancel || escape {
                    answer = Some(None);
                } else if ok || enter {
                    match check(&self.user, &self.password, &self.port) {
                        Ok(login) => answer = Some(Some(login)),
                        Err(problem) => self.error = problem.to_string(),
                    }
                }
            });
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(None);
        }
        match answer {
            Some(answer) => {
                if let Some(reply) = self.reply.take() {
                    let _ = reply.send(answer);
                }
                true
            }
            None => false,
        }
    }
}

/// The checks of Accept: a user, and a port from 1 to 65535.
pub fn check(user: &str, password: &str, port: &str) -> Result<Login, &'static str> {
    if user.trim().is_empty() {
        return Err(t("Enter a user, such as root."));
    }
    match port.trim().parse::<u16>() {
        Ok(number) if number >= 1 => Ok(Login { user: user.trim().to_string(), password: password.to_string(), port: number }),
        _ => Err(t("The port is a number from 1 to 65535.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks() {
        assert_eq!(check(" ", "x", "3306"), Err("Enter a user, such as root."));
        assert_eq!(check("root", "x", "0"), Err("The port is a number from 1 to 65535."));
        assert_eq!(check("root", "x", "70000"), Err("The port is a number from 1 to 65535."));
        assert_eq!(check("root", "x", "abc"), Err("The port is a number from 1 to 65535."));
        assert_eq!(check(" root ", "p w", "3307"), Ok(Login { user: "root".into(), password: "p w".into(), port: 3307 }));
    }
}
