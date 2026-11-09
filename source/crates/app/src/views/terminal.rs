//! The SSH terminal (TerminalView.cs, TerminalControl.cs and TerminalSession.cs): an interactive xterm shell over the
//! login, drawn with egui on a monospace grid. The session survives switching pages (the Tools page keeps this view)
//! and ends on exit, when the connection to the server is lost, or with a new login.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aio_remote::server::{ShellChannel, ShellInput};
use aio_remote::{Server, ServerLogin};
use aio_terminal::colors::{self, BACKGROUND, CURSOR, DEFAULT_FOREGROUND, DIM_ALPHA, SCROLL_LABEL, SELECTION};
use aio_terminal::keys::{self, KeyAction, WheelAction};
use aio_terminal::{CellFlags, TerminalCell, TerminalEmulator, TerminalLine};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

use crate::gui::theme::Palette;
use crate::gui::{widgets, Shared, View};
use crate::i18n::{t, tf};

const MIN_FONT: f32 = 9.0;
const MAX_FONT: f32 = 28.0;
/// How long a notice (such as "Copied 12 characters.") replaces the state line.
const NOTICE_SECONDS: f64 = 2.5;

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn argb(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[1], c[2], c[3], c[0])
}

/// What the reader task and the view share (TerminalSession).
struct Shell {
    /// The C# `Emulator.Sync` lock.
    emulator: Mutex<TerminalEmulator>,
    closed: AtomicBool,
    /// Why the shell did not open.
    error: Mutex<Option<String>>,
}

/// A running shell: dropping it closes the shell (Dispose).
struct Session {
    shell: Arc<Shell>,
    input: UnboundedSender<ShellInput>,
    server_id: usize,
    task: tokio::task::JoinHandle<()>,
}

impl Session {
    fn start(shared: &Shared, server: Arc<dyn Server>, columns: usize, rows: usize) -> Self {
        let shell = Arc::new(Shell {
            emulator: Mutex::new(TerminalEmulator::new(columns, rows)),
            closed: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let (input, mut queued) = tokio::sync::mpsc::unbounded_channel::<ShellInput>();
        let server_id = session_id(&server);
        let ctx = shared.ctx.clone();
        let reader = shell.clone();
        let task = shared.runtime.spawn(async move {
            let ShellChannel { input: shell_input, mut output } = match server.open_shell(columns as u32, rows as u32).await {
                Ok(channel) => channel,
                Err(e) => {
                    *reader.error.lock() = Some(e.to_string());
                    reader.closed.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                    return;
                }
            };
            // Keys, pastes and resizes typed while the shell opened wait in `queued` and follow in order.
            let forward = shell_input.clone();
            tokio::spawn(async move {
                while let Some(message) = queued.recv().await {
                    let close = matches!(message, ShellInput::Close);
                    if forward.send(message).is_err() || close {
                        break;
                    }
                }
            });
            while let Some(data) = output.recv().await {
                let replies = {
                    let mut emulator = reader.emulator.lock();
                    emulator.feed(&data);
                    emulator.take_bells();
                    emulator.take_replies()
                };
                for reply in replies {
                    let _ = shell_input.send(ShellInput::Data(reply.into_bytes()));
                }
                ctx.request_repaint();
            }
            reader.closed.store(true, Ordering::SeqCst);
            ctx.request_repaint();
        });
        Self { shell, input, server_id, task }
    }

    fn is_closed(&self) -> bool {
        self.shell.closed.load(Ordering::SeqCst)
    }

    fn send(&self, text: &str) {
        if !self.is_closed() && !text.is_empty() {
            let _ = self.input.send(ShellInput::Data(text.as_bytes().to_vec()));
        }
    }

    fn resize(&self, columns: usize, rows: usize) {
        self.shell.emulator.lock().resize(columns, rows);
        if !self.is_closed() {
            let _ = self.input.send(ShellInput::Resize { columns: columns as u32, rows: rows as u32 });
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shell.closed.store(true, Ordering::SeqCst);
        let _ = self.input.send(ShellInput::Close);
        self.task.abort();
    }
}

fn session_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

/// A position in the text: an absolute line (counting the dropped scrollback) and a column.
type Position = (i64, usize);

pub struct TerminalView {
    session: Option<Session>,
    title: String,
    state: String,
    notice: Option<(String, f64)>,
    ssh_command: String,
    /// The session ended: New session becomes the main button.
    ended: bool,
    font_size: f32,
    /// Lines scrolled back from the bottom.
    scroll_offset: usize,
    selection: Option<(Position, Position)>,
    anchor: Position,
    selecting: bool,
    /// The last grid size, for the next session's first size.
    grid: Option<(usize, usize)>,
    wheel: f64,
    want_focus: bool,
    /// A paste was asked for (button, menu, middle click): the next Paste event is the clipboard.
    paste_pending: bool,
    last_pass: Option<u64>,
    palette: [[u8; 3]; 256],
}

impl TerminalView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            session: None,
            title: String::new(),
            state: String::new(),
            notice: None,
            ssh_command: String::new(),
            ended: false,
            font_size: 14.0,
            scroll_offset: 0,
            selection: None,
            anchor: (0, 0),
            selecting: false,
            grid: None,
            wheel: 0.0,
            want_focus: false,
            paste_pending: false,
            last_pass: None,
            palette: colors::palette(),
        }
    }

    /// Reuses a running shell on the same login unless `restart` is set.
    fn start(&mut self, shared: &Shared, restart: bool) {
        let Some(server) = shared.session() else {
            self.title = t("Not connected").into();
            self.state = t("Log in on the Tools page first.").into();
            return;
        };
        self.ssh_command = ssh_command(server.login());
        if !restart && self.session.as_ref().is_some_and(|s| !s.is_closed() && s.server_id == session_id(&server)) {
            self.want_focus = true;
            return;
        }
        self.session = None;
        // The last size of the grid; the first frame fits the shell to the panel.
        let (columns, rows) = self.grid.unwrap_or((120, 32));
        self.session = Some(Session::start(shared, server.clone(), columns, rows));
        self.scroll_offset = 0;
        self.selection = None;
        self.ended = false;
        let login = server.login();
        self.title = format!("{}@{}", login.user, login.host);
        let os = server.os().map(|o| o.pretty_name).filter(|n| !n.is_empty()).unwrap_or_else(|| "Linux".into());
        self.state = tf("Connected · {os} · type exit to end the session", &[("os", &os)]);
        self.want_focus = true;
    }

    fn show_notice(&mut self, text: &str, now: f64) {
        self.notice = Some((text.to_string(), now + NOTICE_SECONDS));
    }

    fn selected_text(&self) -> String {
        let (Some(session), Some((a, b))) = (&self.session, self.selection) else { return String::new() };
        session.shell.emulator.lock().selection_text(a, b)
    }

    fn copy(&mut self, ctx: &egui::Context, now: f64) {
        let text = self.selected_text();
        if text.is_empty() {
            self.show_notice(t("Select text with the mouse first."), now);
            return;
        }
        ctx.copy_text(text.clone());
        self.show_notice(&tf("Copied {count} characters.", &[("count", &text.encode_utf16().count().to_string())]), now);
    }

    /// Asks the window for the clipboard; it arrives as a Paste event on the next frame.
    fn request_paste(&mut self, ctx: &egui::Context) {
        if self.session.is_some() {
            self.paste_pending = true;
            self.want_focus = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
    }

    fn paste(&mut self, text: &str) {
        let Some(session) = &self.session else { return };
        let bracketed = session.shell.emulator.lock().bracketed_paste();
        if let Some(data) = keys::paste_text(text, bracketed) {
            self.send_input(&data);
        }
    }

    fn send_input(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(session) = &self.session else { return };
        self.scroll_offset = 0;
        self.selection = None;
        session.send(text);
    }

    fn scroll_view(&mut self, lines: i32) {
        let Some(session) = &self.session else { return };
        let emulator = session.shell.emulator.lock();
        let max = if emulator.alternate_screen() { 0 } else { emulator.scrollback().len() as i64 };
        self.scroll_offset = (self.scroll_offset as i64 + lines as i64).clamp(0, max) as usize;
    }

    fn select_all(&mut self) {
        if let Some(session) = &self.session {
            self.selection = Some(session.shell.emulator.lock().select_all());
        }
    }

    fn clear_scrollback(&mut self) {
        if let Some(session) = &self.session {
            session.shell.emulator.lock().feed(b"\x1b[3J");
        }
        self.scroll_offset = 0;
        self.selection = None;
    }

    fn change_font(&mut self, by: f32) {
        self.font_size = (self.font_size + by).clamp(MIN_FONT, MAX_FONT);
    }
}

/// `ssh [-i "key"] [-p port] user@host` for the user's own SSH client.
fn ssh_command(login: &ServerLogin) -> String {
    let key = login.private_keys.first().map(|k| format!("-i \"{k}\" ")).unwrap_or_default();
    let port = if login.port != 22 { format!("-p {} ", login.port) } else { String::new() };
    format!("ssh {key}{port}{}@{}", login.user, login.host)
}

/// egui's key as the terminal's key (None: left to text input).
fn terminal_key(key: egui::Key) -> Option<keys::Key> {
    use egui::Key as E;
    use keys::Key as K;
    Some(match key {
        E::Enter => K::Enter,
        E::Backspace => K::Backspace,
        E::Tab => K::Tab,
        E::Escape => K::Escape,
        E::ArrowUp => K::Up,
        E::ArrowDown => K::Down,
        E::ArrowRight => K::Right,
        E::ArrowLeft => K::Left,
        E::Home => K::Home,
        E::End => K::End,
        E::Insert => K::Insert,
        E::Delete => K::Delete,
        E::PageUp => K::PageUp,
        E::PageDown => K::PageDown,
        E::F1 => K::F(1),
        E::F2 => K::F(2),
        E::F3 => K::F(3),
        E::F4 => K::F(4),
        E::F5 => K::F(5),
        E::F6 => K::F(6),
        E::F7 => K::F(7),
        E::F8 => K::F(8),
        E::F9 => K::F(9),
        E::F10 => K::F(10),
        E::F11 => K::F(11),
        E::F12 => K::F(12),
        E::Space => K::Space,
        E::OpenBracket => K::OpenBracket,
        E::CloseBracket => K::CloseBracket,
        E::Backslash | E::Pipe => K::Backslash,
        E::Slash | E::Questionmark => K::Slash,
        E::Plus | E::Equals => K::Plus,
        E::Minus => K::Minus,
        E::Num0 => K::Digit(0),
        E::Num1 => K::Digit(1),
        E::Num2 => K::Digit(2),
        E::Num3 => K::Digit(3),
        E::Num4 => K::Digit(4),
        E::Num5 => K::Digit(5),
        E::Num6 => K::Digit(6),
        E::Num7 => K::Digit(7),
        E::Num8 => K::Digit(8),
        E::Num9 => K::Digit(9),
        other => {
            let name = other.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_uppercase() => K::Letter(c),
                _ => return None,
            }
        }
    })
}

fn modifiers(m: egui::Modifiers) -> keys::Modifiers {
    keys::Modifiers { ctrl: m.ctrl, shift: m.shift, alt: m.alt }
}

/// What a key press does. AltGr arrives as Ctrl+Alt on Windows: a printable key with it is left to text input, so
/// `@` or `[` on an Italian keyboard is not sent as a control code.
fn key_action(key: egui::Key, m: egui::Modifiers, application_cursor: bool, rows: usize) -> Option<KeyAction> {
    let key = terminal_key(key)?;
    let printable = matches!(
        key,
        keys::Key::Letter(_)
            | keys::Key::Digit(_)
            | keys::Key::Space
            | keys::Key::OpenBracket
            | keys::Key::CloseBracket
            | keys::Key::Backslash
            | keys::Key::Slash
            | keys::Key::Plus
            | keys::Key::Minus
    );
    if m.ctrl && m.alt && printable {
        return None;
    }
    keys::key_action(key, modifiers(m), application_cursor, rows as i32)
}

/// The size of one cell. The width is what text really advances per character: egui rounds every glyph to whole
/// pixels, so `glyph_width` alone drifts from the drawn text (the cursor then stood a space behind what was typed).
fn cell_size(fonts: &egui::text::Fonts, font: &FontId) -> egui::Vec2 {
    const SAMPLE: usize = 200;
    let width = fonts.layout_no_wrap("M".repeat(SAMPLE), font.clone(), Color32::WHITE).size().x / SAMPLE as f32;
    egui::vec2(width.max(1.0), fonts.row_height(font).ceil().max(1.0))
}

/// The cell under a point of the grid (PositionAt).
#[allow(clippy::too_many_arguments)]
fn position_at(
    point: egui::Vec2,
    cell: egui::Vec2,
    columns: usize,
    rows: usize,
    scrollback: usize,
    dropped: u64,
    alternate: bool,
    scroll_offset: usize,
) -> Position {
    let row = ((point.y / cell.y).floor() as i64).clamp(0, rows as i64 - 1);
    let column = ((point.x / cell.x).floor() as i64).clamp(0, columns as i64 - 1) as usize;
    let top = if alternate { scrollback as i64 } else { scrollback as i64 - scroll_offset as i64 };
    (dropped as i64 + top + row, column)
}

fn ordered(selection: Option<(Position, Position)>) -> Option<(Position, Position)> {
    selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
}

/// One run of cells drawn with the same colours and style.
#[derive(Debug, PartialEq)]
struct Run {
    start: usize,
    end: usize,
    text: String,
    foreground: i32,
    background: i32,
    flags: CellFlags,
}

/// The foreground of an inverse run whose background is the default: the terminal background.
const INVERSE_DEFAULT_FG: i32 = -2;
/// The background of an inverse run whose foreground is the default: the default foreground.
const INVERSE_DEFAULT_BG: i32 = -3;

/// Splits a line into runs (TerminalControl.DrawLine). A wide character always ends a run to keep the grid, and a
/// non-ASCII character gets a run of its own, since it often comes from a fallback font with another width.
fn runs(cells: &[TerminalCell], count: usize) -> Vec<Run> {
    let shape = CellFlags(!(CellFlags::WIDE.0 | CellFlags::WIDE_TAIL.0));
    let mut out = Vec::new();
    let mut x = 0;
    while x < count {
        let first = cells[x];
        let flags = first.flags & shape;
        let (mut fg, mut bg) = (first.foreground, first.background);
        if flags.has(CellFlags::INVERSE) {
            (fg, bg) = (
                if bg == TerminalCell::DEFAULT_COLOR { INVERSE_DEFAULT_FG } else { bg },
                if fg == TerminalCell::DEFAULT_COLOR { INVERSE_DEFAULT_BG } else { fg },
            );
        }
        let start = x;
        let mut text = String::new();
        while x < count {
            let c = cells[x];
            if c.flags & shape != flags || c.foreground != first.foreground || c.background != first.background {
                break;
            }
            if c.flags.has(CellFlags::WIDE_TAIL) {
                x += 1;
                if !text.is_empty() {
                    break;
                }
                continue;
            }
            let single = c.rune as u32 > 0x7e || c.flags.has(CellFlags::WIDE);
            if single && !text.is_empty() {
                break;
            }
            text.push(c.rune);
            x += 1;
            if c.flags.has(CellFlags::WIDE) && x < count && cells[x].flags.has(CellFlags::WIDE_TAIL) {
                x += 1;
            }
            if single {
                break;
            }
        }
        if x == start {
            x += 1;
            continue;
        }
        out.push(Run { start, end: x, text, foreground: fg, background: bg, flags });
    }
    out
}

impl TerminalView {
    fn color(&self, value: i32, foreground: bool, dim: bool) -> Color32 {
        let c = match value {
            INVERSE_DEFAULT_FG => BACKGROUND,
            INVERSE_DEFAULT_BG => DEFAULT_FOREGROUND,
            v => colors::to_rgb(v, foreground, &self.palette),
        };
        if dim {
            Color32::from_rgba_unmultiplied(c[0], c[1], c[2], DIM_ALPHA)
        } else {
            rgb(c)
        }
    }

    /// Draws the grid and handles its keyboard and mouse input.
    fn grid(&mut self, ui: &mut egui::Ui, shared: &Shared, rect: Rect, now: f64) {
        let ctx = ui.ctx().clone();
        let id = ui.id().with("terminal-grid");
        let response = ui.interact(rect, id, egui::Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Text);
        let font = FontId::monospace(self.font_size);
        let cell = ui.fonts(|f| cell_size(f, &font));
        let columns = ((rect.width() / cell.x) as usize).max(20);
        let rows = ((rect.height() / cell.y) as usize).max(5);
        if rect.width() >= 10.0 && rect.height() >= 10.0 {
            self.grid = Some((columns, rows));
            // FitToBounds: the shell follows the panel.
            if let Some(session) = &self.session {
                let size = {
                    let e = session.shell.emulator.lock();
                    (e.columns(), e.rows())
                };
                if size != (columns, rows) {
                    session.resize(columns, rows);
                    self.scroll_offset = 0;
                }
            }
        }

        if response.clicked() || response.drag_started() || response.middle_clicked() || response.secondary_clicked() || self.want_focus {
            response.request_focus();
            self.want_focus = false;
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
            });
        }

        self.mouse(ui, &response, rect, cell, now);
        if focused || self.paste_pending {
            self.keyboard(ui, focused, now);
        }
        self.context_menu(&response, &ctx, now);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, rgb(BACKGROUND));
        let Some(session) = &self.session else { return };
        let closed = session.is_closed();
        let shell = session.shell.clone();
        let emulator = shell.emulator.lock();
        let scrollback = emulator.scrollback().len();
        self.scroll_offset = self.scroll_offset.min(if emulator.alternate_screen() { 0 } else { scrollback });
        let first_line = scrollback as i64 - self.scroll_offset as i64;
        let selection = ordered(self.selection);
        let rows_drawn = emulator.rows();
        for row in 0..rows_drawn {
            let line = if emulator.alternate_screen() { Some(emulator.line(row)) } else { emulator.absolute_line(first_line + row as i64) };
            let Some(line) = line else { continue };
            let absolute = emulator.scrollback_dropped() as i64 + first_line + row as i64;
            let y = rect.top() + row as f32 * cell.y;
            self.draw_line(&painter, line, rect.left(), y, cell, &font, emulator.columns(), absolute, selection);
        }
        if self.scroll_offset == 0 && emulator.cursor_visible() && !closed {
            let cursor = Rect::from_min_size(
                egui::pos2(rect.left() + emulator.cursor_x() as f32 * cell.x, rect.top() + emulator.cursor_y() as f32 * cell.y),
                cell,
            );
            if focused {
                painter.rect_filled(cursor, 0.0, argb(CURSOR));
            } else {
                painter.rect_stroke(cursor.shrink(0.5), 0.0, egui::Stroke::new(1.0_f32, rgb(DEFAULT_FOREGROUND)), egui::StrokeKind::Inside);
            }
        }
        if self.scroll_offset > 0 {
            let label = tf(" ↑ {count} lines back (type or scroll down to return) ", &[("count", &self.scroll_offset.to_string())]);
            let galley = painter.layout_no_wrap(label, FontId::monospace((self.font_size - 2.0).max(10.0)), Color32::BLACK);
            let at = egui::pos2(rect.right() - galley.size().x - 8.0, rect.top() + 4.0);
            painter.rect_filled(Rect::from_min_size(at, galley.size()), 0.0, rgb(SCROLL_LABEL));
            painter.galley(at, galley, Color32::BLACK);
        }
        drop(emulator);
        let _ = shared;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &self,
        painter: &egui::Painter,
        line: &TerminalLine,
        left: f32,
        y: f32,
        cell: egui::Vec2,
        font: &FontId,
        columns: usize,
        absolute: i64,
        selection: Option<(Position, Position)>,
    ) {
        let cells = &line.cells;
        let count = cells.len().min(columns + 1);
        for run in runs(cells, count) {
            let rect = Rect::from_min_size(egui::pos2(left + run.start as f32 * cell.x, y), egui::vec2((run.end - run.start) as f32 * cell.x, cell.y));
            if run.background != TerminalCell::DEFAULT_COLOR {
                painter.rect_filled(rect, 0.0, self.color(run.background, false, false));
            }
            let fg = self.color(run.foreground, true, run.flags.has(CellFlags::DIM));
            if !run.flags.has(CellFlags::HIDDEN) && !run.text.trim().is_empty() {
                let format = egui::TextFormat { font_id: font.clone(), color: fg, italics: run.flags.has(CellFlags::ITALIC), ..Default::default() };
                let job = egui::text::LayoutJob::single_section(run.text.clone(), format);
                let galley = painter.layout_job(job);
                let at = Pos2::new(rect.left(), y);
                if run.flags.has(CellFlags::BOLD) {
                    // The monospace font has no bold face: a second pass half a pixel to the right.
                    painter.galley(at + egui::vec2(0.5, 0.0), galley.clone(), fg);
                }
                painter.galley(at, galley, fg);
            }
            if run.flags.has(CellFlags::UNDERLINE) || run.flags.has(CellFlags::STRIKE) {
                let stroke = egui::Stroke::new(1.0_f32, self.color(run.foreground, true, false));
                if run.flags.has(CellFlags::UNDERLINE) {
                    let uy = y + cell.y - 1.5;
                    painter.line_segment([egui::pos2(rect.left(), uy), egui::pos2(rect.right(), uy)], stroke);
                }
                if run.flags.has(CellFlags::STRIKE) {
                    let sy = y + cell.y / 2.0;
                    painter.line_segment([egui::pos2(rect.left(), sy), egui::pos2(rect.right(), sy)], stroke);
                }
            }
        }
        if let Some(((from_line, from_col), (to_line, to_col))) = selection {
            if absolute >= from_line && absolute <= to_line {
                let a = if absolute == from_line { from_col } else { 0 };
                let b = if absolute == to_line { to_col as i64 } else { cells.len() as i64 - 1 };
                if b >= a as i64 {
                    let rect = Rect::from_min_size(egui::pos2(left + a as f32 * cell.x, y), egui::vec2((b - a as i64 + 1) as f32 * cell.x, cell.y));
                    painter.rect_filled(rect, 0.0, argb(SELECTION));
                }
            }
        }
    }

    fn position(&self, pointer: Pos2, rect: Rect, cell: egui::Vec2) -> Option<Position> {
        let session = self.session.as_ref()?;
        let e = session.shell.emulator.lock();
        Some(position_at(
            pointer - rect.min,
            cell,
            e.columns(),
            e.rows(),
            e.scrollback().len(),
            e.scrollback_dropped(),
            e.alternate_screen(),
            self.scroll_offset,
        ))
    }

    fn mouse(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect, cell: egui::Vec2, now: f64) {
        let _ = now;
        if self.session.is_none() {
            return;
        }
        // The wheel: scrollback, or arrow keys in full-screen programs.
        if response.hovered() {
            let rows = self.grid.map(|g| g.1).unwrap_or(24) as f64;
            let notches: f64 = ui.input(|i| {
                i.events
                    .iter()
                    .map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => match unit {
                            egui::MouseWheelUnit::Line => delta.y as f64,
                            egui::MouseWheelUnit::Point => delta.y as f64 / 50.0,
                            egui::MouseWheelUnit::Page => delta.y as f64 * rows / 3.0,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            self.wheel += notches;
            let lines = (self.wheel * 3.0).trunc();
            if lines != 0.0 {
                self.wheel -= lines / 3.0;
                let (alternate, app) = {
                    let e = self.session.as_ref().unwrap().shell.emulator.lock();
                    (e.alternate_screen(), e.application_cursor_keys())
                };
                match keys::wheel_action(lines / 3.0, alternate, app) {
                    Some(WheelAction::Scroll(n)) => self.scroll_view(n),
                    Some(WheelAction::Send(arrows)) => {
                        if let Some(session) = &self.session {
                            session.send(&arrows);
                        }
                    }
                    None => {}
                }
            }
        }
        if response.middle_clicked() {
            self.request_paste(ui.ctx());
            return;
        }
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        if response.double_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                self.select_word(pos, rect, cell);
            }
            self.selecting = false;
            return;
        }
        if pressed && response.hovered() {
            if let Some(pos) = pointer.and_then(|p| self.position(p, rect, cell)) {
                self.selection = None;
                self.selecting = true;
                self.anchor = pos;
            }
        }
        if self.selecting {
            if !ui.input(|i| i.pointer.primary_down()) {
                self.selecting = false;
                return;
            }
            if let Some(pointer) = pointer {
                let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                if moved && pointer.y < rect.top() {
                    self.scroll_view(1);
                } else if moved && pointer.y > rect.bottom() {
                    self.scroll_view(-1);
                }
                if let Some(here) = self.position(pointer, rect, cell) {
                    if self.selection.is_some() || here != self.anchor {
                        self.selection = Some((self.anchor, here));
                    }
                }
            }
        }
    }

    /// Double click: the word under the pointer (SelectWord).
    fn select_word(&mut self, pointer: Pos2, rect: Rect, cell: egui::Vec2) {
        let Some((line, column)) = self.position(pointer, rect, cell) else { return };
        let Some(session) = &self.session else { return };
        let e = session.shell.emulator.lock();
        let index = line - e.scrollback_dropped() as i64;
        let row = if e.alternate_screen() {
            let row = index - e.scrollback().len() as i64;
            (row >= 0 && (row as usize) < e.rows()).then(|| e.line(row as usize))
        } else {
            e.absolute_line(index)
        };
        if let Some((a, b)) = row.and_then(|row| TerminalEmulator::word_at(row, column)) {
            drop(e);
            self.selection = Some(((line, a), (line, b)));
        }
    }

    fn keyboard(&mut self, ui: &egui::Ui, focused: bool, now: f64) {
        let ctx = ui.ctx().clone();
        let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
        let Some(session) = &self.session else { return };
        let (app, rows) = {
            let e = session.shell.emulator.lock();
            (e.application_cursor_keys(), e.rows())
        };
        let mut handled_key = false;
        for event in events {
            match event {
                egui::Event::Paste(text) if self.paste_pending => {
                    self.paste_pending = false;
                    self.paste(&text);
                }
                _ if !focused => {}
                egui::Event::Key { key, pressed: true, modifiers: m, .. } => {
                    handled_key = false;
                    match key_action(key, m, app, rows) {
                        Some(KeyAction::Copy) => self.copy(&ctx, now),
                        Some(KeyAction::Paste) => self.request_paste(&ctx),
                        Some(KeyAction::Scroll(n)) => self.scroll_view(n),
                        Some(KeyAction::FontSize(by)) => self.change_font(by as f32),
                        Some(KeyAction::Send(data)) => {
                            handled_key = true;
                            self.send_input(&data);
                        }
                        None => {}
                    }
                }
                // egui turns the clipboard shortcuts into these events, without the key press.
                egui::Event::Copy => {
                    if mods.shift {
                        self.copy(&ctx, now);
                    } else if let Some(data) = keys::key_sequence(keys::Key::Letter('C'), modifiers(mods), app) {
                        self.send_input(&data);
                    }
                }
                egui::Event::Cut => {
                    let key = if mods.ctrl { keys::Key::Letter('X') } else { keys::Key::Delete };
                    if let Some(data) = keys::key_sequence(key, modifiers(mods), app) {
                        self.send_input(&data);
                    }
                }
                egui::Event::Paste(text) => {
                    if mods.ctrl && !mods.shift {
                        // Ctrl+V is ^V for the shell; Ctrl+Shift+V and Shift+Insert paste.
                        if let Some(data) = keys::key_sequence(keys::Key::Letter('V'), modifiers(mods), app) {
                            self.send_input(&data);
                        }
                    } else {
                        self.paste(&text);
                    }
                }
                egui::Event::Text(text) => {
                    if !handled_key {
                        self.send_input(&text);
                    }
                    handled_key = false;
                }
                egui::Event::Ime(egui::ImeEvent::Commit(text)) => self.send_input(&text),
                _ => {}
            }
        }
    }

    fn context_menu(&mut self, response: &egui::Response, ctx: &egui::Context, now: f64) {
        let mut action = None;
        response.context_menu(|ui| {
            if ui.add(egui::Button::new(t("Copy")).shortcut_text("Ctrl+Shift+C")).clicked() {
                action = Some(0);
            }
            if ui.add(egui::Button::new(t("Paste")).shortcut_text("Ctrl+Shift+V")).clicked() {
                action = Some(1);
            }
            ui.separator();
            if ui.button(t("Select all")).clicked() {
                action = Some(2);
            }
            if ui.button(t("Clear scrollback")).clicked() {
                action = Some(3);
            }
            if action.is_some() {
                ui.close_menu();
            }
        });
        match action {
            Some(0) => self.copy(ctx, now),
            Some(1) => self.request_paste(ctx),
            Some(2) => self.select_all(),
            Some(3) => self.clear_scrollback(),
            _ => {}
        }
    }
}

impl View for TerminalView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let now = ui.input(|i| i.time);
        let pass = ui.ctx().cumulative_pass_nr();
        // Back on screen (AttachedToVisualTree): reuse the shell, or start one.
        if self.last_pass.is_none_or(|last| pass > last + 1) {
            self.start(shared, false);
        }
        self.last_pass = Some(pass);

        if let Some(session) = &self.session {
            let error = session.shell.error.lock().take();
            if let Some(error) = error {
                self.title = t("No shell").into();
                self.state = tf("The server did not open a shell: {message}", &[("message", &error)]);
                self.session = None;
            } else if session.is_closed() && !self.ended {
                self.ended = true;
                self.state = t("The session has ended (exit, or the connection was closed). Click New session to start another.").into();
            } else {
                // A window title set by the remote program (OSC 0/2) replaces user@host.
                let title = session.shell.emulator.lock().title().to_string();
                if !title.is_empty() {
                    self.title = title;
                }
            }
        }
        if self.notice.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.notice = None;
        }
        if let Some((_, until)) = &self.notice {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((until - now).max(0.05)));
        }

        // The bar: title and state, the buttons on the right.
        let mut new_session = false;
        let mut copy = false;
        let mut paste = false;
        let mut font = 0.0;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 360.0).max(160.0));
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(super::server_status::bold(&self.title, 13.0)).truncate());
                let state = self.notice.as_ref().map(|(n, _)| n.as_str()).unwrap_or(&self.state);
                ui.add(egui::Label::new(RichText::new(state).size(12.0).color(palette.muted)).truncate());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::secondary_button("A+")).clicked() {
                    font = 1.0;
                }
                if ui.add(widgets::secondary_button("A−")).clicked() {
                    font = -1.0;
                }
                paste = ui.add(widgets::secondary_button(t("Paste"))).clicked();
                copy = ui.add(widgets::secondary_button(t("Copy"))).clicked();
                let button = if self.ended { widgets::primary_button(t("New session")) } else { widgets::secondary_button(t("New session")) };
                new_session = ui.add(button).clicked();
            });
        });
        ui.add_space(10.0);
        if new_session {
            self.start(shared, true);
        }
        if copy {
            self.copy(ui.ctx(), now);
        }
        if paste {
            self.request_paste(ui.ctx());
        }
        if font != 0.0 {
            self.change_font(font);
            self.want_focus = true;
        }

        // The terminal fills the page above the help lines.
        let help_height = 62.0;
        let height = (ui.available_height() - help_height - 10.0).max(120.0);
        let frame_rect = Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), height));
        ui.allocate_rect(frame_rect, egui::Sense::hover());
        ui.painter().rect_filled(frame_rect, 10.0, rgb(BACKGROUND));
        let inner = Rect::from_min_max(frame_rect.min + egui::vec2(10.0, 8.0), frame_rect.max - egui::vec2(10.0, 8.0));
        self.grid(ui, shared, inner, now);
        ui.add_space(10.0);

        ui.add(
            egui::Label::new(
                RichText::new(t("Select with the mouse (double-click: a word) · Ctrl+Shift+C copies · Ctrl+Shift+V or the middle button pastes · Shift+PgUp / the wheel scrolls back · Ctrl + / − text size · right-click for more."))
                    .size(11.5)
                    .color(palette.subtle),
            )
            .wrap(),
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Your own SSH client:")).size(11.5).color(palette.subtle));
            ui.add(egui::Label::new(RichText::new(&self.ssh_command).monospace().size(12.0).color(palette.muted)).selectable(true));
            let small = egui::Button::new(RichText::new(t("Copy")).size(11.0)).min_size(egui::vec2(0.0, 20.0));
            if ui.add(small).clicked() {
                ui.ctx().copy_text(self.ssh_command.clone());
                self.show_notice(t("ssh command copied."), now);
            }
        });
    }

    fn session_changed(&mut self, _shared: &Shared) {
        // The shell belonged to the old login.
        self.session = None;
        self.scroll_offset = 0;
        self.selection = None;
        self.selecting = false;
        self.paste_pending = false;
        self.ended = false;
        self.title.clear();
        self.state.clear();
        // Opens a shell on the new login the next time it is shown.
        self.last_pass = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(rune: char) -> TerminalCell {
        TerminalCell { rune, ..TerminalCell::default() }
    }

    #[test]
    fn ssh_commands() {
        let login = ServerLogin::password("hotel.example.com", 22, "root", "");
        assert_eq!(ssh_command(&login), "ssh root@hotel.example.com");
        let login = ServerLogin::key("10.0.0.2", 2222, "admin", vec!["C:\\keys\\id_ed25519".into()], "");
        assert_eq!(ssh_command(&login), "ssh -i \"C:\\keys\\id_ed25519\" -p 2222 admin@10.0.0.2");
    }

    #[test]
    fn keys_map() {
        let none = egui::Modifiers::NONE;
        assert_eq!(key_action(egui::Key::ArrowUp, none, false, 24), Some(KeyAction::Send("\x1b[A".into())));
        assert_eq!(key_action(egui::Key::A, egui::Modifiers::CTRL, false, 24), Some(KeyAction::Send("\x01".into())));
        assert_eq!(key_action(egui::Key::PageUp, egui::Modifiers::SHIFT, false, 24), Some(KeyAction::Scroll(23)));
        assert_eq!(key_action(egui::Key::Equals, egui::Modifiers::CTRL, false, 24), Some(KeyAction::FontSize(1)));
        assert_eq!(key_action(egui::Key::A, none, false, 24), None);
        // AltGr (Ctrl+Alt) with a printable key is left to text input.
        let altgr = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
        assert_eq!(key_action(egui::Key::OpenBracket, altgr, false, 24), None);
        assert_eq!(key_action(egui::Key::ArrowLeft, altgr, false, 24), Some(KeyAction::Send("\x1b[1;7D".into())));
        assert_eq!(terminal_key(egui::Key::Z), Some(keys::Key::Letter('Z')));
        assert_eq!(terminal_key(egui::Key::F20), None);
    }

    #[test]
    fn positions() {
        let cell = egui::vec2(8.0, 17.0);
        // Main screen, 100 lines of scrollback, 3 dropped, scrolled back 10 lines.
        assert_eq!(position_at(egui::vec2(17.0, 35.0), cell, 80, 24, 100, 3, false, 10), (3 + 90 + 2, 2));
        // Outside the grid it clamps to the edge.
        assert_eq!(position_at(egui::vec2(-5.0, 9999.0), cell, 80, 24, 0, 0, false, 0), (23, 0));
        // The alternate screen comes right after the scrollback and does not scroll.
        assert_eq!(position_at(egui::vec2(0.0, 0.0), cell, 80, 24, 100, 0, true, 10), (100, 0));
        assert_eq!(ordered(Some(((5, 2), (4, 9)))), Some(((4, 9), (5, 2))));
    }

    #[test]
    fn line_runs() {
        let mut cells: Vec<TerminalCell> = "ab é".chars().map(cell).collect();
        cells[1].foreground = 1;
        let r = runs(&cells, cells.len());
        let texts: Vec<&str> = r.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", " ", "é"]);
        assert_eq!((r[3].start, r[3].end), (3, 4));
        // Inverse with default colours swaps to the sentinels.
        let mut inverse = vec![cell('x')];
        inverse[0].flags = CellFlags::INVERSE;
        let r = runs(&inverse, 1);
        assert_eq!((r[0].foreground, r[0].background), (INVERSE_DEFAULT_FG, INVERSE_DEFAULT_BG));
        // A wide character takes its tail with it.
        let mut wide = vec![cell('中'), cell(' '), cell('a')];
        wide[0].flags = CellFlags::WIDE;
        wide[1].flags = CellFlags::WIDE_TAIL;
        let r = runs(&wide, 3);
        assert_eq!((r[0].start, r[0].end, r[0].text.as_str()), (0, 2, "中"));
        assert_eq!(r[1].text, "a");
    }
}


#[cfg(test)]
mod cell_tests {
    use super::*;

    #[test]
    fn the_grid_follows_the_drawn_text() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            crate::gui::theme::install_fonts(&ctx);
            ctx.set_pixels_per_point(ppp);
            let _ = ctx.run(Default::default(), |_| {});
            for size in [12.0_f32, 13.0, 14.0, 15.0, 18.0] {
                let font = FontId::monospace(size);
                let line = "root@test1:/var/www# chown -R www-data:www-data atomcms/ && ls -la --color=auto /var/www/gamedata/";
                let (cell, drawn) = ctx.fonts(|f| (cell_size(f, &font), f.layout_no_wrap(line.into(), font.clone(), Color32::WHITE).size().x));
                let drift = (cell.x * line.chars().count() as f32 - drawn).abs();
                assert!(drift < cell.x / 4.0, "ppp {ppp} size {size}: {drift} px apart");
            }
        }
    }
}
