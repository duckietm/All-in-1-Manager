//! Questions the steps ask while they run (ChoiceDialog, PasswordDialog, NameDialog, ...). A step's task asks and
//! awaits the answer; the window shows the newest question as a modal over everything and sends the answer back.

use std::sync::Arc;

use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio::sync::oneshot;

use super::icons::{self, Icon};
use super::theme::Palette;
use super::widgets;
use crate::i18n::t;

/// One answer of a [`Dialogs::choose`] question.
#[derive(Debug, Clone)]
pub struct Choice<T> {
    pub title: String,
    pub description: String,
    pub value: T,
    pub primary: bool,
}

impl<T> Choice<T> {
    pub fn new(title: impl Into<String>, description: impl Into<String>, value: T) -> Self {
        Self { title: title.into(), description: description.into(), value, primary: false }
    }

    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
}

/// The answer of a [`Dialogs::input_or`] text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputReply {
    Ok(String),
    /// Cancel, Escape or a click outside.
    Cancel,
    /// The extra button.
    Extra,
}

enum Kind {
    /// Answers with the index of the option, or None when the window is closed.
    Choice { options: Vec<(String, String, bool)>, reply: Option<oneshot::Sender<Option<usize>>> },
    /// A text (or password) with OK / Cancel and an optional extra button.
    Input {
        label: String,
        value: String,
        password: bool,
        ok: String,
        extra: Option<String>,
        check: Option<Box<dyn Fn(&str) -> Option<String> + Send>>,
        problem: Option<String>,
        reply: Option<oneshot::Sender<InputReply>>,
    },
}

struct Dialog {
    window_title: String,
    heading: String,
    message: String,
    icon: Icon,
    kind: Kind,
}

/// The questions waiting for an answer. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Dialogs {
    queue: Arc<Mutex<Vec<Dialog>>>,
    ctx: egui::Context,
}

impl Dialogs {
    pub fn new(ctx: egui::Context) -> Self {
        Self { queue: Arc::new(Mutex::new(Vec::new())), ctx }
    }

    pub fn is_open(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    fn push(&self, dialog: Dialog) {
        self.queue.lock().push(dialog);
        self.ctx.request_repaint();
    }

    /// A question with described answers, one per button. Closing the window returns `cancel`. (ChoiceDialog)
    pub async fn choose<T: Clone + Send>(&self, window_title: &str, heading: &str, message: &str, cancel: T, options: Vec<Choice<T>>) -> T {
        let (sender, receiver) = oneshot::channel();
        self.push(Dialog {
            window_title: window_title.into(),
            heading: heading.into(),
            message: message.into(),
            icon: Icon::Alert,
            kind: Kind::Choice {
                options: options.iter().map(|o| (o.title.clone(), o.description.clone(), o.primary)).collect(),
                reply: Some(sender),
            },
        });
        match receiver.await {
            Ok(Some(index)) => options[index].value.clone(),
            _ => cancel,
        }
    }

    /// A text field; `check` returns what is wrong with the text (shown under it), or None. None when cancelled.
    pub async fn input(
        &self,
        window_title: &str,
        heading: &str,
        message: &str,
        label: &str,
        value: &str,
        password: bool,
        ok: &str,
        check: Option<Box<dyn Fn(&str) -> Option<String> + Send>>,
    ) -> Option<String> {
        match self.input_or(window_title, heading, message, label, value, password, ok, None, check).await {
            InputReply::Ok(text) => Some(text),
            _ => None,
        }
    }

    /// [`Dialogs::input`] with an extra button next to Cancel (`extra`, when given).
    #[allow(clippy::too_many_arguments)]
    pub async fn input_or(
        &self,
        window_title: &str,
        heading: &str,
        message: &str,
        label: &str,
        value: &str,
        password: bool,
        ok: &str,
        extra: Option<&str>,
        check: Option<Box<dyn Fn(&str) -> Option<String> + Send>>,
    ) -> InputReply {
        let (sender, receiver) = oneshot::channel();
        self.push(Dialog {
            window_title: window_title.into(),
            heading: heading.into(),
            message: message.into(),
            icon: if password { Icon::Key } else { Icon::Info },
            kind: Kind::Input {
                label: label.into(),
                value: value.into(),
                password,
                ok: ok.into(),
                extra: extra.map(str::to_string),
                check,
                problem: None,
                reply: Some(sender),
            },
        });
        receiver.await.unwrap_or(InputReply::Cancel)
    }

    /// Test hook: answers the newest question as if the user clicked choice `index` (a choice dialog) or, for a text
    /// field, OK with its text (`index` 0), Cancel (1) or the extra button (2). False when no question is open.
    #[cfg(test)]
    pub fn answer_for_tests(&self, index: usize) -> bool {
        let Some(dialog) = self.queue.lock().pop() else { return false };
        match dialog.kind {
            Kind::Choice { reply, .. } => {
                let _ = reply.map(|r| r.send(Some(index)));
            }
            Kind::Input { reply, value, .. } => {
                let answer = match index {
                    0 => InputReply::Ok(value),
                    1 => InputReply::Cancel,
                    _ => InputReply::Extra,
                };
                let _ = reply.map(|r| r.send(answer));
            }
        }
        true
    }

    /// Draws the newest question; the others wait behind it.
    pub fn show(&self, ctx: &egui::Context) {
        let mut queue = self.queue.lock();
        let depth = queue.len();
        let Some(dialog) = queue.last_mut() else { return };
        let palette = Palette::current(ctx);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("dialog").with(depth)).frame(widgets::card_frame(&palette).inner_margin(24)).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.label(RichText::new(&dialog.window_title).size(11.5).color(palette.subtle));
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                egui::Frame::new().fill(palette.warning_back).corner_radius(20).inner_margin(10).show(ui, |ui| {
                    icons::show(ui, dialog.icon, 20.0, palette.warning_text);
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(widgets::title_text(&dialog.heading, 20.0));
                    if !dialog.message.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&dialog.message).size(13.0));
                    }
                    ui.add_space(8.0);
                    match &mut dialog.kind {
                        Kind::Choice { options, reply } => {
                            for (index, (title, description, primary)) in options.iter().enumerate() {
                                if widgets::described_button(ui, &palette, title, description, *primary).clicked() {
                                    if let Some(sender) = reply.take() {
                                        let _ = sender.send(Some(index));
                                    }
                                    close = true;
                                }
                            }
                        }
                        Kind::Input { label, value, password, ok, extra, check, problem, reply } => {
                            ui.label(RichText::new(label.as_str()).strong());
                            let field = ui.add(egui::TextEdit::singleline(value).password(*password).desired_width(f32::INFINITY));
                            if !field.has_focus() && problem.is_none() {
                                field.request_focus();
                            }
                            if let Some(problem) = problem.as_ref() {
                                ui.label(RichText::new(problem.as_str()).size(12.5).color(palette.error_text));
                            }
                            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let ok_clicked = ui.add(widgets::primary_button(ok)).clicked() || enter;
                                let cancel = ui.add(widgets::secondary_button(t("Cancel"))).clicked();
                                let extra_clicked = extra.as_deref().is_some_and(|label| ui.add(widgets::secondary_button(label)).clicked());
                                if ok_clicked {
                                    *problem = check.as_ref().and_then(|c| c(value));
                                    if problem.is_none() {
                                        if let Some(sender) = reply.take() {
                                            let _ = sender.send(InputReply::Ok(value.clone()));
                                        }
                                        close = true;
                                    }
                                } else if cancel || extra_clicked {
                                    if let Some(sender) = reply.take() {
                                        let _ = sender.send(if extra_clicked { InputReply::Extra } else { InputReply::Cancel });
                                    }
                                    close = true;
                                }
                            });
                        }
                    }
                });
            });
        });
        if modal.should_close() && !close {
            // Escape or a click outside: like closing the window.
            match &mut dialog.kind {
                Kind::Choice { reply, .. } => {
                    if let Some(sender) = reply.take() {
                        let _ = sender.send(None);
                    }
                }
                Kind::Input { reply, .. } => {
                    if let Some(sender) = reply.take() {
                        let _ = sender.send(InputReply::Cancel);
                    }
                }
            }
            close = true;
        }
        if close {
            queue.pop();
        }
    }
}
