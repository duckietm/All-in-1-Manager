//! The approval card above an external plugin's page: a script it wants to run on the server, or database changes
//! it wants to make, shown in full (Ruling 15) with Approve, Reject and "Reject and stop this plugin". Nothing runs
//! before Approve (the runtime waits in `Approvals::request`).

use aio_plugin_host::approvals::{Approvals, Decision, Pending};
use aio_plugin_host::display_safe;
use aio_plugin_host::manifest::spoofing;
use aio_plugin_host::services::{SqlValue, WriteBatch};
use eframe::egui::{self, RichText};

use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::i18n::{t, tf};
use crate::plugins::external::ExternalInfo;

/// What the user chose on the card, besides deciding the request itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// Approve was clicked on request `id`: the page counts the work as running, then decides (so nothing can cut
    /// it off between the click and the start).
    Approve(u64),
    /// "Reject and stop this plugin": the page drops the instance.
    Stop,
}

/// A character that must not reach the screen as it is: control characters other than newline and tab, and the
/// invisible / direction-changing ones.
fn hidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || spoofing(c)
}

/// `text` with every hidden character as U+FFFD, nothing cut.
fn visible(text: &str) -> String {
    text.chars().map(|c| if hidden(c) { '\u{FFFD}' } else { c }).collect()
}

/// A text value in quotes, escaped so that line breaks and quotes cannot fake the layout; nothing cut.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if hidden(c) => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One parameter value in full: text quoted, bytes that are not UTF-8 as every byte in hex.
pub fn value_text(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".into(),
        SqlValue::Int(i) => i.to_string(),
        SqlValue::Float(f) => format!("{f:?}"),
        SqlValue::Text(s) => quoted(s),
        SqlValue::Bytes(b) => match std::str::from_utf8(b) {
            Ok(s) => quoted(s),
            Err(_) => format!("0x{}", b.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        },
    }
}

/// Every statement of the batch with every parameter value, in full.
pub fn writes_text(batch: &WriteBatch) -> String {
    let mut out = String::new();
    for (n, (sql, params)) in batch.statements.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", n + 1, visible(sql)));
        for (k, value) in params.iter().enumerate() {
            out.push_str(&format!("     ?{} = {}\n", k + 1, value_text(value)));
        }
    }
    out
}

/// Characters per line of the card: longer lines are cut into several, so every line has the same height and the card
/// only lays out the lines in view, however long the batch is.
pub const CARD_LINE: usize = 100;

/// What the card of a request shows, cut into lines of at most [`CARD_LINE`] characters: the full script, or every
/// statement with every value. Built once per request (the approval queue's formatter), never per frame.
pub fn card_lines(pending: &Pending) -> Vec<String> {
    let text = match pending {
        Pending::Script { .. } => pending.text(),
        Pending::Writes { batch, .. } => writes_text(batch),
    };
    let mut lines = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
        }
        lines.extend(chars.chunks(CARD_LINE).map(|c| c.iter().collect::<String>()));
    }
    lines
}

/// The badges every external plugin carries: "Not verified" (red) and "Developer folder".
pub fn badges(ui: &mut egui::Ui, palette: &Palette, info: &ExternalInfo) {
    if !info.verified {
        ui.label(RichText::new(t("Not verified")).size(12.5).strong().color(palette.error_text));
    }
    if info.dev {
        ui.label(RichText::new(t("Developer")).size(12.5).strong().color(palette.error_text));
    }
}

/// The server the waiting request was made on, the user a script without root runs as there, and whether the login
/// has changed since (Approve then runs nothing).
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub host: Option<String>,
    pub user: Option<String>,
    pub changed: bool,
}

/// Who a script runs as on the server: "root" with `as_root`, else the login user.
pub fn runs_as(as_root: bool, target: &Target) -> Option<String> {
    if as_root {
        Some("root".into())
    } else {
        target.user.clone()
    }
}

/// The cards of every request of `info`'s plugin that waits for the user, oldest first.
pub fn show(ui: &mut egui::Ui, palette: &Palette, approvals: &Approvals, info: &ExternalInfo, name: &str, target: &Target) -> Outcome {
    let mut outcome = Outcome::Nothing;
    for (id, pending) in approvals.pending().into_iter().filter(|(_, p)| p.plugin() == info.manifest.id) {
        // Built once when the request was queued; gone if it was decided meanwhile.
        let Some(lines) = approvals.card(id) else { continue };
        let mut decision = None;
        let frame = widgets::card_frame(palette).stroke(egui::Stroke::new(2.0_f32, palette.warning_text)).inner_margin(egui::Margin::symmetric(20, 16));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::title_text(name, 17.0));
                badges(ui, palette, info);
            });
            let what = match &*pending {
                Pending::Script { as_root, .. } => {
                    let user = runs_as(*as_root, target);
                    if user.as_deref() == Some("root") {
                        ui.label(RichText::new(t("⚠ Runs as root")).size(18.0).strong().color(palette.error_text));
                    }
                    if *as_root {
                        t("wants to run this script on the server as root, with full control of the server:").to_string()
                    } else {
                        match user {
                            Some(user) => tf("wants to run this script on the server as the login user {user}:", &[("user", &display_safe(&user, 60))]),
                            None => t("wants to run this script on the server as the login user:").to_string(),
                        }
                    }
                }
                Pending::Writes { batch, .. } => {
                    tf("wants to make these changes to the hotel database, in one transaction ({count} statements):", &[("count", &batch.statements.len().to_string())])
                }
            };
            ui.label(RichText::new(what).size(13.5));
            let server = match &target.host {
                Some(host) => tf("On the server {host}", &[("host", host)]),
                None => t("Not connected to a server").to_string(),
            };
            ui.label(RichText::new(server).size(13.5).strong());
            if target.changed {
                widgets::result_banner(ui, palette, ResultKind::Error, t("The server changed since the request"), t("Approving now runs nothing: reject it, and let the plugin ask again on this server."));
            }
            ui.add_space(4.0);
            egui::Frame::new().fill(palette.hover).corner_radius(6).inner_margin(8).show(ui, |ui| {
                // Only the lines in view are laid out: a long batch costs no more per frame than a short one.
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::both().id_salt(("approval", id)).max_height(320.0).auto_shrink([false, true]).show_rows(ui, row_height, lines.len(), |ui, rows| {
                    for line in &lines[rows] {
                        ui.add(egui::Label::new(RichText::new(line.as_str()).monospace()).extend());
                    }
                });
            });
            ui.add_space(6.0);
            widgets::muted(ui, palette, t("Nothing runs until you approve. Read all of it: the plugin is not verified."), 12.5);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(widgets::primary_button(t("Approve"))).clicked() {
                    decision = Some(Decision::Approved);
                }
                if ui.add(widgets::secondary_button(t("Reject"))).clicked() {
                    decision = Some(Decision::Rejected);
                }
                if ui.add(widgets::secondary_button(t("Reject and stop this plugin"))).on_hover_text(t("Rejects this and closes the plugin; Reload starts it again")).clicked() {
                    decision = Some(Decision::Rejected);
                    outcome = Outcome::Stop;
                }
            });
        });
        ui.add_space(10.0);
        match decision {
            Some(Decision::Approved) => outcome = Outcome::Approve(id),
            Some(d) => approvals.decide(id, d),
            None => {}
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_shown_in_full() {
        let long = "x".repeat(500);
        let bytes: Vec<u8> = (0..200u8).map(|b| b | 0x80).collect();
        let batch = WriteBatch {
            statements: vec![
                ("UPDATE `rooms` SET `name` = ? WHERE `id` = ?".into(), vec![SqlValue::Text(long.clone()), SqlValue::Int(7)]),
                ("INSERT INTO `items` (`data`, `note`, `x`) VALUES (?, ?, ?)".into(), vec![SqlValue::Bytes(bytes.clone()), SqlValue::Text("a\u{202E}b\n\"c".into()), SqlValue::Null]),
            ],
            summary: vec!["short".into()],
        };
        let text = writes_text(&batch);
        assert!(text.contains(&format!("?1 = \"{long}\"")), "{text}");
        assert!(text.contains("?2 = 7"));
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        assert!(text.contains(&format!("?1 = 0x{hex}")), "every byte in hex");
        assert!(text.contains("?2 = \"a\u{FFFD}b\\n\\\"c\""), "{text}");
        assert!(text.contains("?3 = NULL"));
        assert!(text.starts_with("1. UPDATE `rooms`"));
        assert!(!text.contains('\u{202E}'));
    }

    #[test]
    fn card_lines_are_cut_and_built_once_per_request() {
        let long = "y".repeat(CARD_LINE * 2 + 5);
        let batch = WriteBatch { statements: vec![("INSERT INTO `rooms` (`name`) VALUES (?)".into(), vec![SqlValue::Text(long.clone())])], summary: vec![] };
        let lines = card_lines(&Pending::Writes { plugin: "demo".into(), batch });
        assert!(lines.iter().all(|l| l.chars().count() <= CARD_LINE), "{lines:?}");
        assert_eq!(lines.concat().matches('y').count(), long.len(), "nothing is lost by the cut");
        let script = card_lines(&Pending::Script { plugin: "demo".into(), script: "echo a\n\necho b".into(), as_root: false });
        assert_eq!(script, vec!["echo a".to_string(), String::new(), "echo b".to_string()]);

        // The queue builds the card once, when the request comes in; drawing it any number of times builds nothing.
        let built = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approvals = Approvals::new();
        let counted = built.clone();
        approvals.set_formatter(move |p| {
            counted.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            card_lines(p)
        });
        let request = approvals.request(Pending::Script { plugin: "demo".into(), script: "id".into(), as_root: false });
        for _ in 0..5 {
            let (id, _) = approvals.pending()[0].clone();
            assert_eq!(approvals.card(id).unwrap().to_vec(), vec!["id".to_string()]);
        }
        assert_eq!(built.load(std::sync::atomic::Ordering::Acquire), 1);
        drop(request);
        assert!(approvals.pending().is_empty());
    }

    #[test]
    fn a_script_names_who_it_runs_as() {
        let target = Target { host: Some("hotel".into()), user: Some("deploy".into()), changed: false };
        assert_eq!(runs_as(false, &target).as_deref(), Some("deploy"));
        assert_eq!(runs_as(true, &target).as_deref(), Some("root"));
        assert_eq!(runs_as(false, &Target::default()), None);
    }

    #[test]
    fn utf8_bytes_are_shown_as_text() {
        assert_eq!(value_text(&SqlValue::Bytes(b"ok".to_vec())), "\"ok\"");
        assert_eq!(value_text(&SqlValue::Float(1.5)), "1.5");
    }
}
