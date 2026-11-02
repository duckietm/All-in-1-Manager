//! Port of `SourceCode/Terminal/TerminalEmulator.cs`: the part of xterm that common Linux programs use, including
//! 256-colour and truecolor SGR, scroll regions, the alternate screen, DEC line drawing and scrollback.
//!
//! It is not thread safe (the C# locks `Sync` around every call): keep it behind a `Mutex` shared by the reader task
//! and the view.

use std::collections::VecDeque;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

use crate::zero_width::ZERO_WIDTH;

/// The cell attributes, a bit set like the C# `[Flags] enum CellFlags : ushort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CellFlags(pub u16);

impl CellFlags {
    pub const NONE: CellFlags = CellFlags(0);
    pub const BOLD: CellFlags = CellFlags(1);
    pub const DIM: CellFlags = CellFlags(2);
    pub const ITALIC: CellFlags = CellFlags(4);
    pub const UNDERLINE: CellFlags = CellFlags(8);
    pub const INVERSE: CellFlags = CellFlags(16);
    pub const HIDDEN: CellFlags = CellFlags(32);
    pub const STRIKE: CellFlags = CellFlags(64);
    /// The right half of a double-width character, which the cell before it draws.
    pub const WIDE_TAIL: CellFlags = CellFlags(128);
    pub const WIDE: CellFlags = CellFlags(256);

    /// True when any of the bits of `other` is set.
    pub fn has(self, other: CellFlags) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for CellFlags {
    type Output = CellFlags;
    fn bitor(self, rhs: CellFlags) -> CellFlags {
        CellFlags(self.0 | rhs.0)
    }
}

impl BitOrAssign for CellFlags {
    fn bitor_assign(&mut self, rhs: CellFlags) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for CellFlags {
    type Output = CellFlags;
    fn bitand(self, rhs: CellFlags) -> CellFlags {
        CellFlags(self.0 & rhs.0)
    }
}

impl BitAndAssign for CellFlags {
    fn bitand_assign(&mut self, rhs: CellFlags) {
        self.0 &= rhs.0;
    }
}

impl Not for CellFlags {
    type Output = CellFlags;
    fn not(self) -> CellFlags {
        CellFlags(!self.0)
    }
}

/// A character cell. Each colour is [`TerminalCell::DEFAULT_COLOR`], a palette index 0-255, or
/// [`TerminalCell::RGB`] | 0xRRGGBB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalCell {
    pub rune: char,
    pub foreground: i32,
    pub background: i32,
    pub flags: CellFlags,
}

impl TerminalCell {
    pub const DEFAULT_COLOR: i32 = -1;
    pub const RGB: i32 = 0x1000000;

    pub fn blank(background: i32) -> TerminalCell {
        TerminalCell { rune: ' ', foreground: Self::DEFAULT_COLOR, background, flags: CellFlags::NONE }
    }
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell::blank(TerminalCell::DEFAULT_COLOR)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLine {
    pub cells: Vec<TerminalCell>,
    /// The text wrapped onto the next line, so copying joins the two.
    pub wrapped: bool,
}

impl TerminalLine {
    pub fn new(columns: usize, background: i32) -> TerminalLine {
        TerminalLine { cells: vec![TerminalCell::blank(background); columns], wrapped: false }
    }

    pub fn clear(&mut self, from: i64, to: i64, background: i32) {
        let from = from.max(0) as usize;
        let to = (to.max(0) as usize).min(self.cells.len());
        for i in from..to {
            self.cells[i] = TerminalCell::blank(background);
        }
    }

    pub fn resize(&mut self, columns: usize) {
        self.cells.resize(columns, TerminalCell::blank(TerminalCell::DEFAULT_COLOR));
    }
}

/// A streaming UTF-8 decoder that replaces invalid input like .NET's `UTF8Encoding.GetDecoder()` (one U+FFFD per
/// maximal invalid subpart) and keeps an incomplete sequence for the next call.
#[derive(Debug, Default, Clone)]
struct Utf8Decoder {
    code_point: u32,
    needed: u8,
    lower: u8,
    upper: u8,
}

impl Utf8Decoder {
    fn decode(&mut self, data: &[u8], out: &mut Vec<char>) {
        for &b in data {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<char>) {
        if self.needed == 0 {
            match b {
                0x00..=0x7f => out.push(b as char),
                0xc2..=0xdf => self.start(1, b as u32 & 0x1f, 0x80, 0xbf),
                0xe0 => self.start(2, 0, 0xa0, 0xbf),
                0xe1..=0xec | 0xee..=0xef => self.start(2, b as u32 & 0x0f, 0x80, 0xbf),
                0xed => self.start(2, b as u32 & 0x0f, 0x80, 0x9f),
                0xf0 => self.start(3, 0, 0x90, 0xbf),
                0xf1..=0xf3 => self.start(3, b as u32 & 0x07, 0x80, 0xbf),
                0xf4 => self.start(3, 4, 0x80, 0x8f),
                _ => out.push('\u{fffd}'),
            }
            return;
        }
        if b < self.lower || b > self.upper {
            // The sequence so far is one invalid subpart; the byte starts over.
            self.needed = 0;
            out.push('\u{fffd}');
            self.byte(b, out);
            return;
        }
        self.lower = 0x80;
        self.upper = 0xbf;
        self.code_point = (self.code_point << 6) | (b as u32 & 0x3f);
        self.needed -= 1;
        if self.needed == 0 {
            out.push(char::from_u32(self.code_point).unwrap_or('\u{fffd}'));
        }
    }

    fn start(&mut self, needed: u8, code_point: u32, lower: u8, upper: u8) {
        self.needed = needed;
        self.code_point = code_point;
        self.lower = lower;
        self.upper = upper;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    Dcs,
    DcsEscape,
}

#[derive(Debug, Clone, Copy)]
struct Saved {
    x: i32,
    y: i32,
    fg: i32,
    bg: i32,
    flags: CellFlags,
    origin: bool,
    g0: bool,
    g1: bool,
    charset: usize,
    wrap: bool,
}

pub struct TerminalEmulator {
    columns: i32,
    rows: i32,
    cursor_x: i32,
    cursor_y: i32,
    cursor_visible: bool,
    application_cursor_keys: bool,
    bracketed_paste: bool,
    title: String,
    scrollback: VecDeque<TerminalLine>,
    scrollback_dropped: u64,
    replies: Vec<String>,
    bells: usize,

    main: Vec<TerminalLine>,
    alternate: Vec<TerminalLine>,
    alternate_active: bool,

    utf8: Utf8Decoder,
    chars: Vec<char>,

    // Pen and modes
    fg: i32,
    bg: i32,
    flags: CellFlags,
    pending_wrap: bool,
    auto_wrap: bool,
    origin_mode: bool,
    insert_mode: bool,
    new_line_mode: bool,
    top: i32,
    bottom: i32,
    tab_stops: Vec<bool>,
    line_drawing: [bool; 2],
    charset: usize, // 0 = G0, 1 = G1 (shift out)
    saved: Saved,
    last_rune: char,

    // Parser
    state: State,
    params: String,
    private: char,
    intermediate: char,
    osc: String,
    /// The OSC string's length in UTF-16 code units, which the C# limits to 4096.
    osc_units: usize,
}

impl TerminalEmulator {
    pub const MAX_SCROLLBACK: usize = 5000;

    pub fn new(columns: usize, rows: usize) -> TerminalEmulator {
        let columns = (columns.min(i32::MAX as usize) as i32).max(2);
        let rows = (rows.min(i32::MAX as usize) as i32).max(1);
        let screen = || (0..rows).map(|_| TerminalLine::new(columns as usize, TerminalCell::DEFAULT_COLOR)).collect();
        let mut t = TerminalEmulator {
            columns,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            application_cursor_keys: false,
            bracketed_paste: false,
            title: String::new(),
            scrollback: VecDeque::new(),
            scrollback_dropped: 0,
            replies: Vec::new(),
            bells: 0,
            main: screen(),
            alternate: screen(),
            alternate_active: false,
            utf8: Utf8Decoder::default(),
            chars: Vec::with_capacity(4096),
            fg: TerminalCell::DEFAULT_COLOR,
            bg: TerminalCell::DEFAULT_COLOR,
            flags: CellFlags::NONE,
            pending_wrap: false,
            auto_wrap: true,
            origin_mode: false,
            insert_mode: false,
            new_line_mode: false,
            top: 0,
            bottom: rows - 1,
            tab_stops: Vec::new(),
            line_drawing: [false; 2],
            charset: 0,
            saved: Saved {
                x: 0,
                y: 0,
                fg: TerminalCell::DEFAULT_COLOR,
                bg: TerminalCell::DEFAULT_COLOR,
                flags: CellFlags::NONE,
                origin: false,
                g0: false,
                g1: false,
                charset: 0,
                wrap: true,
            },
            last_rune: ' ',
            state: State::Ground,
            params: String::new(),
            private: '\0',
            intermediate: '\0',
            osc: String::new(),
            osc_units: 0,
        };
        t.reset();
        t
    }

    // ---- Public state -------------------------------------------------------------------------------------------

    pub fn columns(&self) -> usize {
        self.columns as usize
    }

    pub fn rows(&self) -> usize {
        self.rows as usize
    }

    pub fn cursor_x(&self) -> usize {
        self.cursor_x as usize
    }

    pub fn cursor_y(&self) -> usize {
        self.cursor_y as usize
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn alternate_screen(&self) -> bool {
        self.alternate_active
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Lines scrolled off the top of the main screen, oldest first.
    pub fn scrollback(&self) -> &VecDeque<TerminalLine> {
        &self.scrollback
    }

    /// Lines dropped from the scrollback so far, which keeps absolute line numbers in selections stable.
    pub fn scrollback_dropped(&self) -> u64 {
        self.scrollback_dropped
    }

    /// Responses to send back to the program (cursor position and device attribute reports), oldest first. The C#
    /// raises the `Reply` event for each; call this after every [`feed`](Self::feed) and send them to the shell.
    pub fn take_replies(&mut self) -> Vec<String> {
        std::mem::take(&mut self.replies)
    }

    /// The number of BEL characters since the last call (the C# `Bell` event).
    pub fn take_bells(&mut self) -> usize {
        std::mem::take(&mut self.bells)
    }

    /// A row of the current screen (main or alternate).
    pub fn line(&self, row: usize) -> &TerminalLine {
        &self.screen()[row]
    }

    /// Gets a line by index, counting the scrollback first and then the screen.
    pub fn absolute_line(&self, index: i64) -> Option<&TerminalLine> {
        if index < 0 {
            return None;
        }
        let index = index as u64;
        let sb = self.scrollback.len() as u64;
        if index < sb {
            Some(&self.scrollback[index as usize])
        } else if index - sb < self.rows as u64 {
            Some(&self.screen()[(index - sb) as usize])
        } else {
            None
        }
    }

    pub fn absolute_count(&self) -> usize {
        self.scrollback.len() + self.rows as usize
    }

    fn screen(&self) -> &Vec<TerminalLine> {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.main
        }
    }

    fn screen_mut(&mut self) -> &mut Vec<TerminalLine> {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.main
        }
    }

    fn line_mut(&mut self, row: i32) -> &mut TerminalLine {
        &mut self.screen_mut()[row as usize]
    }

    fn reset(&mut self) {
        self.fg = TerminalCell::DEFAULT_COLOR;
        self.bg = TerminalCell::DEFAULT_COLOR;
        self.flags = CellFlags::NONE;
        self.pending_wrap = false;
        self.origin_mode = false;
        self.insert_mode = false;
        self.new_line_mode = false;
        self.auto_wrap = true;
        self.cursor_visible = true;
        self.application_cursor_keys = false;
        self.bracketed_paste = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.line_drawing = [false; 2];
        self.charset = 0;
        self.tab_stops = vec![false; self.columns as usize];
        let mut i = 8;
        while i < self.columns as usize {
            self.tab_stops[i] = true;
            i += 8;
        }
        self.saved = Saved {
            x: 0,
            y: 0,
            fg: self.fg,
            bg: self.bg,
            flags: CellFlags::NONE,
            origin: false,
            g0: false,
            g1: false,
            charset: 0,
            wrap: true,
        };
    }

    // ---- Input --------------------------------------------------------------------------------------------------

    pub fn feed(&mut self, data: &[u8]) {
        let mut chars = std::mem::take(&mut self.chars);
        for chunk in data.chunks(4096) {
            chars.clear();
            self.utf8.decode(chunk, &mut chars);
            for &c in chars.iter() {
                self.process(c);
            }
        }
        chars.clear();
        self.chars = chars;
    }

    fn process(&mut self, ch: char) {
        let c = ch as u32;
        // CAN and SUB abort a sequence. ESC starts a new one anywhere except inside OSC and DCS strings.
        if c == 0x18 || c == 0x1a {
            self.state = State::Ground;
            return;
        }
        match self.state {
            State::Ground => {
                if c < 0x20 || c == 0x7f {
                    self.control(c);
                } else {
                    self.print(ch);
                }
            }
            State::Escape => self.escape(ch),
            State::EscapeIntermediate => self.escape_intermediate(ch),
            State::Csi => {
                if c == 0x1b {
                    self.enter(State::Escape);
                    return;
                }
                if c < 0x20 {
                    self.control(c);
                    return;
                }
                if ch.is_ascii_digit() || ch == ';' || ch == ':' {
                    self.params.push(ch);
                } else if matches!(ch, '?' | '>' | '=' | '<') && self.params.is_empty() {
                    self.private = ch;
                } else if (0x20..=0x2f).contains(&c) {
                    self.intermediate = ch;
                } else if (0x40..=0x7e).contains(&c) {
                    self.state = State::Ground;
                    self.csi(ch);
                } else {
                    self.state = State::Ground;
                }
            }
            State::Osc => {
                if c == 0x07 {
                    self.state = State::Ground;
                    self.osc();
                } else if c == 0x1b {
                    self.state = State::OscEscape;
                } else if self.osc_units < 4096 {
                    self.osc.push(ch);
                    self.osc_units += ch.len_utf16();
                }
            }
            State::OscEscape => {
                self.state = State::Ground;
                if ch == '\\' {
                    self.osc();
                } else {
                    self.enter(State::Escape);
                    self.escape(ch);
                }
            }
            State::Dcs => {
                if c == 0x1b {
                    self.state = State::DcsEscape;
                } else if c == 0x07 {
                    self.state = State::Ground;
                }
            }
            State::DcsEscape => {
                self.state = if ch == '\\' { State::Ground } else { State::Dcs };
            }
        }
    }

    fn enter(&mut self, state: State) {
        self.state = state;
        self.params.clear();
        self.private = '\0';
        self.intermediate = '\0';
    }

    fn control(&mut self, c: u32) {
        match c {
            0x07 => self.bells += 1,
            0x08 => {
                // BS
                if self.pending_wrap {
                    self.pending_wrap = false;
                } else if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                }
            }
            0x09 => {
                // HT
                self.pending_wrap = false;
                self.cursor_x = self.next_tab(self.cursor_x);
            }
            0x0a..=0x0c => {
                // LF VT FF
                self.index();
                if self.new_line_mode {
                    self.cursor_x = 0;
                }
            }
            0x0d => {
                self.cursor_x = 0;
                self.pending_wrap = false;
            }
            0x0e => self.charset = 1, // SO
            0x0f => self.charset = 0, // SI
            0x1b => self.enter(State::Escape),
            _ => {}
        }
    }

    fn next_tab(&self, x: i32) -> i32 {
        for i in (x + 1)..self.columns {
            if self.tab_stops[i as usize] {
                return i;
            }
        }
        self.columns - 1
    }

    fn print(&mut self, c: char) {
        let c = if self.line_drawing[self.charset] { line_drawing(c) } else { c };
        let width = rune_width(c);
        if width == 0 {
            return; // combining marks and zero-width characters are not drawn
        }
        self.last_rune = c;
        let width = width as i32;

        if self.pending_wrap && self.auto_wrap {
            let y = self.cursor_y;
            self.line_mut(y).wrapped = true;
            self.cursor_x = 0;
            self.index();
        }
        self.pending_wrap = false;
        if width == 2 && self.cursor_x == self.columns - 1 {
            if !self.auto_wrap {
                return;
            }
            let (x, y, bg) = (self.cursor_x, self.cursor_y, self.bg);
            let line = self.line_mut(y);
            line.wrapped = true;
            line.cells[x as usize] = TerminalCell::blank(bg);
            self.cursor_x = 0;
            self.index();
        }

        if self.insert_mode {
            self.insert_cells(width);
        }
        let (x, y, fg, bg, flags) = (self.cursor_x as usize, self.cursor_y, self.fg, self.bg, self.flags);
        let cells = &mut self.line_mut(y).cells;
        fix_wide(cells, x as i64);
        if width == 2 {
            fix_wide(cells, x as i64 + 1);
        }
        cells[x] = TerminalCell {
            rune: c,
            foreground: fg,
            background: bg,
            flags: flags | if width == 2 { CellFlags::WIDE } else { CellFlags::NONE },
        };
        if width == 2 {
            cells[x + 1] = TerminalCell { rune: ' ', foreground: fg, background: bg, flags: flags | CellFlags::WIDE_TAIL };
        }

        if self.cursor_x + width >= self.columns {
            self.cursor_x = self.columns - 1;
            self.pending_wrap = self.auto_wrap;
        } else {
            self.cursor_x += width;
        }
    }

    fn index(&mut self) {
        if self.cursor_y == self.bottom {
            self.scroll_up(1);
        } else if self.cursor_y < self.rows - 1 {
            self.cursor_y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor_y == self.top {
            self.scroll_down(1);
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    fn scroll_up(&mut self, count: i32) {
        let count = count.min(self.bottom - self.top + 1);
        let (top, bottom, columns, bg) = (self.top as usize, self.bottom as usize, self.columns as usize, self.bg);
        for _ in 0..count {
            let line = self.screen_mut().remove(top);
            if top == 0 && !self.alternate_active {
                self.scrollback.push_back(line);
                if self.scrollback.len() > Self::MAX_SCROLLBACK {
                    self.scrollback.pop_front();
                    self.scrollback_dropped += 1;
                }
            }
            self.screen_mut().insert(bottom, TerminalLine::new(columns, bg));
        }
    }

    fn scroll_down(&mut self, count: i32) {
        let count = count.min(self.bottom - self.top + 1);
        let (top, bottom, columns, bg) = (self.top as usize, self.bottom as usize, self.columns as usize, self.bg);
        for _ in 0..count {
            let screen = self.screen_mut();
            screen.remove(bottom);
            screen.insert(top, TerminalLine::new(columns, bg));
        }
    }

    fn escape(&mut self, ch: char) {
        self.state = State::Ground;
        match ch {
            '[' => self.enter(State::Csi),
            ']' => {
                self.osc.clear();
                self.osc_units = 0;
                self.state = State::Osc;
            }
            'P' => self.state = State::Dcs,
            '_' | '^' | 'X' => self.state = State::Dcs, // APC, PM and SOS are ignored like DCS.
            '(' | ')' | '*' | '+' | '#' | '%' | ' ' => {
                self.intermediate = ch;
                self.state = State::EscapeIntermediate;
            }
            '7' => self.save_cursor(),
            '8' => self.restore_cursor(),
            'D' => {
                self.pending_wrap = false;
                self.index();
            }
            'E' => {
                self.pending_wrap = false;
                self.cursor_x = 0;
                self.index();
            }
            'M' => {
                self.pending_wrap = false;
                self.reverse_index();
            }
            'H' => {
                if self.cursor_x < self.columns {
                    self.tab_stops[self.cursor_x as usize] = true;
                }
            }
            'c' => {
                self.alternate_active = false;
                let columns = self.columns as i64;
                for line in self.main.iter_mut() {
                    line.clear(0, columns, TerminalCell::DEFAULT_COLOR);
                }
                self.cursor_x = 0;
                self.cursor_y = 0;
                self.reset();
            }
            '=' | '>' => {} // Keypad modes. The keypad sends the same either way here.
            _ => {}
        }
    }

    fn escape_intermediate(&mut self, ch: char) {
        self.state = State::Ground;
        match self.intermediate {
            '(' => self.line_drawing[0] = ch == '0',
            ')' => self.line_drawing[1] = ch == '0',
            '#' if ch == '8' => {
                // DECALN fills the screen with E.
                for line in self.screen_mut().iter_mut() {
                    for cell in line.cells.iter_mut() {
                        *cell = TerminalCell {
                            rune: 'E',
                            foreground: TerminalCell::DEFAULT_COLOR,
                            background: TerminalCell::DEFAULT_COLOR,
                            flags: CellFlags::NONE,
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        self.saved = Saved {
            x: self.cursor_x,
            y: self.cursor_y,
            fg: self.fg,
            bg: self.bg,
            flags: self.flags,
            origin: self.origin_mode,
            g0: self.line_drawing[0],
            g1: self.line_drawing[1],
            charset: self.charset,
            wrap: self.pending_wrap,
        };
    }

    fn restore_cursor(&mut self) {
        let s = self.saved;
        self.cursor_x = s.x.clamp(0, self.columns - 1);
        self.cursor_y = s.y.clamp(0, self.rows - 1);
        self.fg = s.fg;
        self.bg = s.bg;
        self.flags = s.flags;
        self.origin_mode = s.origin;
        self.line_drawing = [s.g0, s.g1];
        self.charset = s.charset;
        self.pending_wrap = s.wrap;
    }

    fn osc(&mut self) {
        if let Some(semicolon) = self.osc.find(';') {
            if semicolon > 0 && matches!(&self.osc[..semicolon], "0" | "2") {
                self.title = self.osc[semicolon + 1..].to_string();
            }
        }
    }

    // Empty parameters read as 0. Callers apply their own default through arg().
    fn params(&self) -> Vec<i32> {
        if self.params.is_empty() {
            return Vec::new();
        }
        // int.TryParse fails on an empty part and on overflow, which both read as 0.
        self.params.split([';', ':']).map(|part| part.parse::<i32>().map(|v| v.min(65535)).unwrap_or(0)).collect()
    }

    fn csi(&mut self, final_char: char) {
        let p = self.params();
        let n = arg(&p, 0, 1);
        if self.intermediate != '\0' {
            if self.intermediate == '!' && final_char == 'p' {
                self.soft_reset();
            }
            return; // Cursor style (SP q) and other intermediates are ignored.
        }
        if self.private == '?' {
            if final_char == 'h' || final_char == 'l' {
                for &mode in &p {
                    self.dec_mode(mode, final_char == 'h');
                }
            }
            return;
        }
        if self.private == '>' {
            if final_char == 'c' {
                self.replies.push("\x1b[>0;276;0c".to_string());
            }
            return;
        }
        if self.private != '\0' {
            return;
        }

        if final_char != 'b' {
            self.pending_wrap = false;
        }
        match final_char {
            '@' => self.insert_cells(n),
            'A' => self.cursor_y = (if self.cursor_y < self.top { 0 } else { self.top }).max(self.cursor_y - n),
            'B' | 'e' => {
                self.cursor_y = (if self.cursor_y > self.bottom { self.rows - 1 } else { self.bottom }).min(self.cursor_y + n)
            }
            'C' | 'a' => self.cursor_x = (self.columns - 1).min(self.cursor_x + n),
            'D' => self.cursor_x = 0.max(self.cursor_x - n),
            'E' => {
                self.cursor_x = 0;
                self.cursor_y = self.bottom.min(self.cursor_y + n);
            }
            'F' => {
                self.cursor_x = 0;
                self.cursor_y = self.top.max(self.cursor_y - n);
            }
            'G' | '`' => self.cursor_x = (n - 1).clamp(0, self.columns - 1),
            'H' | 'f' => self.move_to(arg(&p, 1, 1) - 1, arg(&p, 0, 1) - 1),
            'I' => {
                for _ in 0..n {
                    self.cursor_x = self.next_tab(self.cursor_x);
                }
            }
            'Z' => {
                for _ in 0..n {
                    let mut x = self.cursor_x - 1;
                    while x > 0 && !self.tab_stops[x as usize] {
                        x -= 1;
                    }
                    self.cursor_x = x.max(0);
                }
            }
            'J' => self.erase_display(p.first().copied().unwrap_or(0)),
            'K' => self.erase_line(p.first().copied().unwrap_or(0)),
            'L' => {
                if self.cursor_y >= self.top && self.cursor_y <= self.bottom {
                    let t = self.top;
                    self.top = self.cursor_y;
                    self.scroll_down(n);
                    self.top = t;
                    self.cursor_x = 0;
                }
            }
            'M' => {
                if self.cursor_y >= self.top && self.cursor_y <= self.bottom {
                    let t = self.top;
                    self.top = self.cursor_y;
                    self.scroll_up(n);
                    self.top = t;
                    self.cursor_x = 0;
                }
            }
            'P' => self.delete_cells(n),
            'S' => self.scroll_up(n),
            'T' => self.scroll_down(n),
            'X' => {
                let (x, y, bg) = (self.cursor_x as i64, self.cursor_y, self.bg);
                self.line_mut(y).clear(x, x + n as i64, bg);
            }
            'b' => {
                let count = (n as i64).min(self.columns as i64 * self.rows as i64);
                for _ in 0..count {
                    let rune = self.last_rune;
                    self.print(rune);
                }
            }
            'c' => self.replies.push("\x1b[?62;22c".to_string()),
            'd' => self.cursor_y = (n - 1 + if self.origin_mode { self.top } else { 0 }).clamp(0, self.rows - 1),
            'g' => {
                if p.is_empty() || p[0] == 0 {
                    if self.cursor_x < self.columns {
                        self.tab_stops[self.cursor_x as usize] = false;
                    }
                } else if p[0] == 3 {
                    self.tab_stops.iter_mut().for_each(|t| *t = false);
                }
            }
            'h' | 'l' => {
                for &mode in &p {
                    if mode == 4 {
                        self.insert_mode = final_char == 'h';
                    } else if mode == 20 {
                        self.new_line_mode = final_char == 'h';
                    }
                }
            }
            'm' => self.sgr(&p),
            'n' => {
                if n == 5 {
                    self.replies.push("\x1b[0n".to_string());
                } else if n == 6 {
                    let row = self.cursor_y + 1 - if self.origin_mode { self.top } else { 0 };
                    self.replies.push(format!("\x1b[{};{}R", row, self.cursor_x + 1));
                }
            }
            'r' => {
                let top = arg(&p, 0, 1) - 1;
                let mut bottom = arg(&p, 1, self.rows) - 1;
                if bottom >= self.rows {
                    bottom = self.rows - 1;
                }
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.move_to(0, 0);
                }
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.auto_wrap = true;
        self.origin_mode = false;
        self.insert_mode = false;
        self.application_cursor_keys = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.fg = TerminalCell::DEFAULT_COLOR;
        self.bg = TerminalCell::DEFAULT_COLOR;
        self.flags = CellFlags::NONE;
        self.line_drawing = [false; 2];
        self.charset = 0;
    }

    fn move_to(&mut self, x: i32, y: i32) {
        let y = if self.origin_mode { (y + self.top).clamp(self.top, self.bottom) } else { y };
        self.cursor_x = x.clamp(0, self.columns - 1);
        self.cursor_y = y.clamp(0, self.rows - 1);
        self.pending_wrap = false;
    }

    fn dec_mode(&mut self, mode: i32, on: bool) {
        match mode {
            1 => self.application_cursor_keys = on,
            6 => {
                self.origin_mode = on;
                self.move_to(0, 0);
            }
            7 => self.auto_wrap = on,
            25 => self.cursor_visible = on,
            47 | 1047 => self.switch_screen(on, false),
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => self.switch_screen(on, true),
            2004 => self.bracketed_paste = on,
            _ => {}
        }
    }

    fn switch_screen(&mut self, alternate: bool, save_cursor: bool) {
        if alternate == self.alternate_active {
            return;
        }
        if alternate {
            if save_cursor {
                self.save_cursor();
            }
            self.alternate_active = true;
            let columns = self.columns as i64;
            for line in self.alternate.iter_mut() {
                line.clear(0, columns, TerminalCell::DEFAULT_COLOR);
                line.wrapped = false;
            }
        } else {
            self.alternate_active = false;
            if save_cursor {
                self.restore_cursor();
            }
        }
        self.pending_wrap = false;
    }

    fn erase_display(&mut self, mode: i32) {
        let (columns, bg) = (self.columns as i64, self.bg);
        match mode {
            0 => {
                self.erase_line(0);
                for y in (self.cursor_y + 1)..self.rows {
                    let line = self.line_mut(y);
                    line.clear(0, columns, bg);
                    line.wrapped = false;
                }
            }
            1 => {
                self.erase_line(1);
                for y in 0..self.cursor_y {
                    let line = self.line_mut(y);
                    line.clear(0, columns, bg);
                    line.wrapped = false;
                }
            }
            2 => {
                // Like xterm, "clear" moves the main screen's content into the scrollback.
                if !self.alternate_active {
                    let mut last = self.rows - 1;
                    while last >= 0 && is_blank(&self.main[last as usize]) {
                        last -= 1;
                    }
                    for y in 0..=last {
                        let line = std::mem::replace(&mut self.main[y as usize], TerminalLine::new(columns as usize, bg));
                        self.scrollback.push_back(line);
                    }
                    self.trim_scrollback();
                }
                for line in self.screen_mut().iter_mut() {
                    line.clear(0, columns, bg);
                    line.wrapped = false;
                }
            }
            3 => {
                self.scrollback_dropped += self.scrollback.len() as u64;
                self.scrollback.clear();
            }
            _ => {}
        }
    }

    fn trim_scrollback(&mut self) {
        let extra = self.scrollback.len().saturating_sub(Self::MAX_SCROLLBACK);
        if extra == 0 {
            return;
        }
        self.scrollback.drain(..extra);
        self.scrollback_dropped += extra as u64;
    }

    fn erase_line(&mut self, mode: i32) {
        let (x, y, columns, bg) = (self.cursor_x as i64, self.cursor_y, self.columns as i64, self.bg);
        let line = self.line_mut(y);
        match mode {
            0 => {
                line.clear(x, columns, bg);
                line.wrapped = false;
            }
            1 => line.clear(0, x + 1, bg),
            2 => {
                line.clear(0, columns, bg);
                line.wrapped = false;
            }
            _ => {}
        }
    }

    fn insert_cells(&mut self, count: i32) {
        let (x, y, columns, bg) = (self.cursor_x as usize, self.cursor_y, self.columns as usize, self.bg);
        let count = (count as usize).min(columns - x);
        let cells = &mut self.line_mut(y).cells;
        cells.copy_within(x..columns - count, x + count);
        for cell in &mut cells[x..x + count] {
            *cell = TerminalCell::blank(bg);
        }
    }

    fn delete_cells(&mut self, count: i32) {
        let (x, y, columns, bg) = (self.cursor_x as usize, self.cursor_y, self.columns as usize, self.bg);
        let count = (count as usize).min(columns - x);
        let cells = &mut self.line_mut(y).cells;
        cells.copy_within(x + count..columns, x);
        for cell in &mut cells[columns - count..columns] {
            *cell = TerminalCell::blank(bg);
        }
    }

    fn sgr(&mut self, p: &[i32]) {
        let p: &[i32] = if p.is_empty() { &[0] } else { p };
        let mut i = 0;
        while i < p.len() {
            let v = p[i];
            match v {
                0 => {
                    self.fg = TerminalCell::DEFAULT_COLOR;
                    self.bg = TerminalCell::DEFAULT_COLOR;
                    self.flags = CellFlags::NONE;
                }
                1 => self.flags |= CellFlags::BOLD,
                2 => self.flags |= CellFlags::DIM,
                3 => self.flags |= CellFlags::ITALIC,
                4 => self.flags |= CellFlags::UNDERLINE,
                5 | 6 => {} // blink
                7 => self.flags |= CellFlags::INVERSE,
                8 => self.flags |= CellFlags::HIDDEN,
                9 => self.flags |= CellFlags::STRIKE,
                21 => self.flags |= CellFlags::UNDERLINE,
                22 => self.flags &= !(CellFlags::BOLD | CellFlags::DIM),
                23 => self.flags &= !CellFlags::ITALIC,
                24 => self.flags &= !CellFlags::UNDERLINE,
                25 => {}
                27 => self.flags &= !CellFlags::INVERSE,
                28 => self.flags &= !CellFlags::HIDDEN,
                29 => self.flags &= !CellFlags::STRIKE,
                30..=37 => self.fg = v - 30,
                39 => self.fg = TerminalCell::DEFAULT_COLOR,
                40..=47 => self.bg = v - 40,
                49 => self.bg = TerminalCell::DEFAULT_COLOR,
                90..=97 => self.fg = v - 90 + 8,
                100..=107 => self.bg = v - 100 + 8,
                38 | 48 => {
                    let color;
                    if i + 2 < p.len() && p[i + 1] == 5 {
                        color = p[i + 2].clamp(0, 255);
                        i += 2;
                    } else if i + 4 < p.len() && p[i + 1] == 2 {
                        color = TerminalCell::RGB
                            | (p[i + 2].clamp(0, 255) << 16)
                            | (p[i + 3].clamp(0, 255) << 8)
                            | p[i + 4].clamp(0, 255);
                        i += 4;
                    } else {
                        return;
                    }
                    if v == 38 {
                        self.fg = color;
                    } else {
                        self.bg = color;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    // ---- Resize -------------------------------------------------------------------------------------------------

    pub fn resize(&mut self, columns: usize, rows: usize) {
        let columns = (columns.min(i32::MAX as usize) as i32).max(2);
        let rows = (rows.min(i32::MAX as usize) as i32).max(1);
        if columns == self.columns && rows == self.rows {
            return;
        }
        let (new_columns, new_rows) = (columns as usize, rows as usize);

        for is_main in [true, false] {
            let mut screen = std::mem::take(if is_main { &mut self.main } else { &mut self.alternate });
            for line in screen.iter_mut() {
                line.resize(new_columns);
            }
            let current = is_main != self.alternate_active;
            if rows < self.rows {
                // Lines above the cursor go to the scrollback, empty lines below it are dropped.
                let remove = self.rows - rows;
                let cursor = if current { self.cursor_y } else { self.rows - 1 };
                let below = remove.min(self.rows - 1 - cursor);
                let mut i = 0;
                while i < below && screen.len() > new_rows {
                    if is_blank(screen.last().unwrap()) || !current {
                        screen.pop();
                    } else {
                        break;
                    }
                    i += 1;
                }
                while screen.len() > new_rows {
                    let line = screen.remove(0);
                    if is_main {
                        self.scrollback.push_back(line);
                    }
                    if current {
                        self.cursor_y -= 1;
                    }
                }
            } else {
                while screen.len() < new_rows {
                    if is_main && current && !self.scrollback.is_empty() {
                        let mut line = self.scrollback.pop_back().unwrap();
                        line.resize(new_columns);
                        screen.insert(0, line);
                        self.cursor_y += 1;
                        self.saved.y += 1;
                    } else {
                        screen.push(TerminalLine::new(new_columns, TerminalCell::DEFAULT_COLOR));
                    }
                }
            }
            if is_main {
                self.main = screen;
            } else {
                self.alternate = screen;
            }
        }
        for line in self.scrollback.iter_mut() {
            line.resize(new_columns);
        }
        self.trim_scrollback();

        self.columns = columns;
        self.rows = rows;
        self.cursor_x = self.cursor_x.clamp(0, columns - 1);
        self.cursor_y = self.cursor_y.clamp(0, rows - 1);
        self.saved.x = self.saved.x.clamp(0, columns - 1);
        self.saved.y = self.saved.y.clamp(0, rows - 1);
        self.top = 0;
        self.bottom = rows - 1;
        self.pending_wrap = false;
        let tabs = (0..new_columns)
            .map(|i| if i < self.tab_stops.len() { self.tab_stops[i] } else { i % 8 == 0 && i > 0 })
            .collect();
        self.tab_stops = tabs;
    }

    // ---- Text ---------------------------------------------------------------------------------------------------

    /// The text between two absolute positions (`(line, column)`, lines counted as in
    /// [`absolute_line`](Self::absolute_line)), inclusive, with wrapped lines joined.
    pub fn text(&self, from: (i64, usize), to: (i64, usize)) -> String {
        let (from, to) = if to.0 < from.0 || to.0 == from.0 && to.1 < from.1 { (to, from) } else { (from, to) };
        let mut text = String::new();
        for y in from.0..=to.0 {
            let Some(line) = self.absolute_line(y) else { continue };
            let last = line.cells.len() as i64 - 1;
            let start = if y == from.0 { from.1 as i64 } else { 0 };
            let end = if y == to.0 { (to.1 as i64).min(last) } else { last };
            let part = cells_text(&line.cells, start, end);
            let joined = line.wrapped && y != to.0;
            text.push_str(if joined { &part } else { part.trim_end() });
            if y != to.0 && !joined {
                text.push('\n');
            }
        }
        text
    }

    /// The selected text for a selection between two positions whose lines are absolute *including* the dropped
    /// scrollback (`scrollback_dropped() + index`), in either order, as `TerminalControl.SelectedText` builds it. In
    /// the alternate screen the rows come right after the scrollback and wrapped lines are not joined.
    pub fn selection_text(&self, a: (i64, usize), b: (i64, usize)) -> String {
        let (from, to) = if a.0 < b.0 || a.0 == b.0 && a.1 <= b.1 { (a, b) } else { (b, a) };
        let dropped = self.scrollback_dropped as i64;
        let index = |line: i64| line - dropped;
        if self.alternate_active {
            let sb = self.scrollback.len() as i64;
            self.alternate_text(index(from.0) - sb, from.1, index(to.0) - sb, to.1)
        } else {
            self.text((index(from.0), from.1), (index(to.0), to.1))
        }
    }

    /// The text of screen rows `from_row..=to_row` (rows may lie outside the screen), with trailing spaces trimmed.
    pub fn alternate_text(&self, from_row: i64, from_column: usize, to_row: i64, to_column: usize) -> String {
        let mut text = String::new();
        let mut row = from_row.max(0);
        while row <= (self.rows as i64 - 1).min(to_row) {
            let cells = &self.line(row as usize).cells;
            let a = if row == from_row { from_column as i64 } else { 0 };
            let b = if row == to_row { to_column as i64 } else { cells.len() as i64 - 1 };
            let part = cells_text(cells, a, b.min(cells.len() as i64 - 1));
            text.push_str(part.trim_end());
            if row != to_row {
                text.push('\n');
            }
            row += 1;
        }
        text
    }

    /// The selection a double click makes (`TerminalControl.SelectWord`): the start and end columns of the word at
    /// `column` of `line`, or `None` when that cell is not part of a word.
    pub fn word_at(line: &TerminalLine, column: usize) -> Option<(usize, usize)> {
        fn word(r: char) -> bool {
            r > ' ' && !"\"'`()[]{}<>,;|".contains(r)
        }
        let cells = &line.cells;
        if column >= cells.len() || !word(cells[column].rune) {
            return None;
        }
        let (mut a, mut b) = (column, column);
        while a > 0 && word(cells[a - 1].rune) {
            a -= 1;
        }
        while b + 1 < cells.len() && word(cells[b + 1].rune) {
            b += 1;
        }
        Some((a, b))
    }

    /// The selection "Select all" makes, in absolute lines including the dropped scrollback: from the first
    /// scrollback line (or the first screen row in the alternate screen) to the last cell of the screen.
    pub fn select_all(&self) -> ((i64, usize), (i64, usize)) {
        let dropped = self.scrollback_dropped as i64;
        let sb = self.scrollback.len() as i64;
        let first = dropped + if self.alternate_active { sb } else { 0 };
        ((first, 0), (dropped + sb + self.rows as i64 - 1, self.columns as usize - 1))
    }
}

fn cells_text(cells: &[TerminalCell], start: i64, end: i64) -> String {
    let mut part = String::new();
    let mut x = start.max(0);
    while x <= end {
        let cell = &cells[x as usize];
        if !cell.flags.has(CellFlags::WIDE_TAIL) {
            part.push(cell.rune);
        }
        x += 1;
    }
    part
}

fn arg(p: &[i32], index: usize, fallback: i32) -> i32 {
    if index < p.len() && p[index] > 0 {
        p[index]
    } else {
        fallback
    }
}

// Overwriting half of a wide character blanks its other half.
fn fix_wide(cells: &mut [TerminalCell], x: i64) {
    if x < 0 || x as usize >= cells.len() {
        return;
    }
    let x = x as usize;
    if cells[x].flags.has(CellFlags::WIDE_TAIL) && x > 0 {
        cells[x - 1] = TerminalCell::blank(cells[x - 1].background);
    }
    if cells[x].flags.has(CellFlags::WIDE) && x + 1 < cells.len() {
        cells[x + 1] = TerminalCell::blank(cells[x + 1].background);
    }
}

fn is_blank(line: &TerminalLine) -> bool {
    line.cells.iter().all(|c| {
        c.rune == ' ' && c.background == TerminalCell::DEFAULT_COLOR && !c.flags.has(CellFlags::INVERSE)
    })
}

// DEC Special Graphics, the line-drawing set selected by ESC ( 0 and used by mc, dialog and older ncurses.
fn line_drawing(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

/// Returns 0 for combining and zero-width characters, 2 for East Asian wide characters and emoji, otherwise 1.
pub fn rune_width(c: char) -> usize {
    let c = c as u32;
    if c < 0x300 {
        return 1;
    }
    // Unicode categories NonSpacingMark, EnclosingMark and Format, as .NET's tables have them.
    let zero = ZERO_WIDTH
        .binary_search_by(|&(a, b)| {
            if b < c {
                std::cmp::Ordering::Less
            } else if a > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok();
    if zero || c == 0x200b {
        return 0;
    }
    let wide = matches!(c,
        0x1100..=0x115f | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff | 0xa000..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff | 0x1f680..=0x1f6ff | 0x20000..=0x3fffd);
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> String {
        let mut d = Utf8Decoder::default();
        let mut out = Vec::new();
        for c in chunks {
            d.decode(c, &mut out);
        }
        out.into_iter().collect()
    }

    #[test]
    fn utf8_decoder_matches_lossy_and_streams() {
        let samples: &[&[u8]] = &[
            b"plain",
            "àè 中文 😀".as_bytes(),
            b"\xff\xc0\xe4\xb8x\xed\xa0\x80y\xf4\x90\x80\x80z",
            b"\xe0\x80\xaf\xf0\x8f\xbf\xbf\xc2",
            b"\xf0\x9f\x98",
        ];
        for s in samples {
            let lossy = String::from_utf8_lossy(s).into_owned();
            // A trailing incomplete sequence stays pending in a streaming decoder.
            let streamed = decode(&[s]);
            assert!(lossy.starts_with(&streamed), "{s:?}: {lossy:?} vs {streamed:?}");
            let bytewise: Vec<&[u8]> = s.chunks(1).collect();
            assert_eq!(decode(&bytewise), streamed);
        }
    }

    #[test]
    fn selection_helpers() {
        let mut t = TerminalEmulator::new(20, 3);
        t.feed(b"echo \"hello-world\" ok");
        let line = t.line(0).clone();
        assert_eq!(TerminalEmulator::word_at(&line, 8), Some((6, 16)));
        assert_eq!(TerminalEmulator::word_at(&line, 4), None);
        assert_eq!(TerminalEmulator::word_at(&line, 5), None);
        let (a, b) = t.select_all();
        assert_eq!(t.selection_text(b, a), "echo \"hello-world\" ok\n");
        t.feed(b"\x1b[?1049h\x1b[H  alt  \r\nrow2");
        let (a, b) = t.select_all();
        assert_eq!(a, (0, 0));
        assert_eq!(t.selection_text(a, b), "  alt\nrow2\n");
        assert_eq!(t.selection_text((0, 2), (1, 1)), "alt\nro");
    }
}
