//! The non-GUI part of `SourceCode/Terminal/TerminalControl.cs`: what a key press, a paste or the mouse wheel sends
//! to the shell. The egui view maps its key events to [`Key`] and [`Modifiers`] and calls [`key_action`].

/// The keys `TerminalControl.OnKeyDown` handles (Avalonia `Key` names in the comments where they differ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    /// `Key.Back`
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
    /// A letter key, `'A'..='Z'` (the key, not the typed character: Shift is in [`Modifiers`]).
    Letter(char),
    /// A digit key on the main row, 0 to 9 (`Key.D0`..`Key.D9`).
    Digit(u8),
    Space,
    /// `[` (`Key.OemOpenBrackets`)
    OpenBracket,
    /// `]` (`Key.OemCloseBrackets`)
    CloseBracket,
    /// `\` or `|` (`Key.OemBackslash`, `Key.OemPipe`)
    Backslash,
    /// `/` or `?` (`Key.OemQuestion`)
    Slash,
    /// `+` on the main row or the keypad (`Key.OemPlus`, `Key.Add`)
    Plus,
    /// `-` on the main row or the keypad (`Key.OemMinus`, `Key.Subtract`)
    Minus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// What the view does with a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Ctrl+Shift+C: copy the selection.
    Copy,
    /// Ctrl+Shift+V or Shift+Insert: paste the clipboard (see [`paste_text`]).
    Paste,
    /// Shift+PageUp/PageDown: scroll the view this many lines back (positive) or forward (negative).
    Scroll(i32),
    /// Ctrl+Plus / Ctrl+Minus: change the font size by this many points (the C# clamps the size to 9-28).
    FontSize(i32),
    /// Send these bytes to the shell (after clearing the selection and scrolling back to the bottom).
    Send(String),
}

/// `TerminalControl.OnKeyDown`: `rows` is the view's height in rows, `application_cursor` is
/// [`TerminalEmulator::application_cursor_keys`](crate::TerminalEmulator::application_cursor_keys). `None` leaves
/// the key to text input.
pub fn key_action(key: Key, mods: Modifiers, application_cursor: bool, rows: i32) -> Option<KeyAction> {
    let Modifiers { ctrl, shift, .. } = mods;
    if ctrl && shift && key == Key::Letter('C') {
        return Some(KeyAction::Copy);
    }
    if ctrl && shift && key == Key::Letter('V') || shift && key == Key::Insert {
        return Some(KeyAction::Paste);
    }
    if shift && matches!(key, Key::PageUp | Key::PageDown) {
        return Some(KeyAction::Scroll(if key == Key::PageUp { rows - 1 } else { -(rows - 1) }));
    }
    if ctrl && key == Key::Plus {
        return Some(KeyAction::FontSize(1));
    }
    if ctrl && key == Key::Minus {
        return Some(KeyAction::FontSize(-1));
    }
    key_sequence(key, mods, application_cursor).map(KeyAction::Send)
}

/// `TerminalControl.KeySequence`: the bytes a key sends, or `None` when the key is left to text input.
pub fn key_sequence(key: Key, mods: Modifiers, application_cursor: bool) -> Option<String> {
    let Modifiers { ctrl, shift, alt } = mods;
    // xterm's modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let modifier = 1 + shift as i32 + if alt { 2 } else { 0 } + if ctrl { 4 } else { 0 };
    let cursor = |f: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{f}")
        } else if application_cursor {
            format!("\x1bO{f}")
        } else {
            format!("\x1b[{f}")
        }
    };
    let tilde = |code: i32| if modifier > 1 { format!("\x1b[{code};{modifier}~") } else { format!("\x1b[{code}~") };
    let function = |f: char| if modifier > 1 { format!("\x1b[1;{modifier}{f}") } else { format!("\x1bO{f}") };

    let s = match key {
        Key::Enter => Some(if alt { "\x1b\r" } else { "\r" }.to_string()),
        Key::Backspace => Some(if ctrl { "\x08" } else if alt { "\x1b\x7f" } else { "\x7f" }.to_string()),
        Key::Tab => Some(if shift { "\x1b[Z" } else { "\t" }.to_string()),
        Key::Escape => Some("\x1b".to_string()),
        Key::Up => Some(cursor('A')),
        Key::Down => Some(cursor('B')),
        Key::Right => Some(cursor('C')),
        Key::Left => Some(cursor('D')),
        Key::Home => Some(cursor('H')),
        Key::End => Some(cursor('F')),
        Key::Insert => Some(tilde(2)),
        Key::Delete => Some(tilde(3)),
        Key::PageUp => Some(tilde(5)),
        Key::PageDown => Some(tilde(6)),
        Key::F(1) => Some(function('P')),
        Key::F(2) => Some(function('Q')),
        Key::F(3) => Some(function('R')),
        Key::F(4) => Some(function('S')),
        Key::F(5) => Some(tilde(15)),
        Key::F(6) => Some(tilde(17)),
        Key::F(7) => Some(tilde(18)),
        Key::F(8) => Some(tilde(19)),
        Key::F(9) => Some(tilde(20)),
        Key::F(10) => Some(tilde(21)),
        Key::F(11) => Some(tilde(23)),
        Key::F(12) => Some(tilde(24)),
        _ => None,
    };
    if s.is_some() {
        return s;
    }

    let letter = match key {
        Key::Letter(c) if c.is_ascii_uppercase() => Some(c as u8 - b'A'),
        _ => None,
    };
    if ctrl && !shift {
        let prefix = if alt { "\x1b" } else { "" };
        if let Some(l) = letter {
            return Some(format!("{prefix}{}", (l + 1) as char));
        }
        let c = match key {
            Key::Space | Key::Digit(2) => Some('\0'),
            Key::OpenBracket | Key::Digit(3) => Some('\x1b'),
            Key::Backslash | Key::Digit(4) => Some('\x1c'),
            Key::CloseBracket | Key::Digit(5) => Some('\x1d'),
            Key::Digit(6) => Some('\x1e'),
            Key::Digit(7) | Key::Slash => Some('\x1f'),
            Key::Digit(8) => Some('\x7f'),
            _ => None,
        };
        if let Some(c) = c {
            return Some(format!("{prefix}{c}"));
        }
    }
    // Alt acts as Meta and sends ESC before the key. AltGr arrives as Ctrl+Alt and is left to text input.
    if alt && !ctrl {
        if let Some(l) = letter {
            return Some(format!("\x1b{}", ((if shift { b'A' } else { b'a' }) + l) as char));
        }
        if let Key::Digit(d @ 0..=9) = key {
            return Some(format!("\x1b{}", (b'0' + d) as char));
        }
    }
    None
}

/// `TerminalControl.PasteAsync`: terminals send Enter as CR, so every line ending becomes CR. Bracketed paste
/// ([`TerminalEmulator::bracketed_paste`](crate::TerminalEmulator::bracketed_paste)) tells the shell the text was
/// pasted, so it does not run it line by line. Returns `None` for an empty clipboard.
pub fn paste_text(text: &str, bracketed: bool) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    // string.ReplaceLineEndings: CRLF, CR, LF, FF, NEL, LS and PS.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\r');
            }
            '\n' | '\x0c' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\r'),
            _ => out.push(c),
        }
    }
    Some(if bracketed { format!("\x1b[200~{}\x1b[201~", out.replace("\x1b[201~", "")) } else { out })
}

/// What the mouse wheel does (`TerminalControl.OnPointerWheelChanged`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelAction {
    /// Scroll the view this many lines back (positive) or forward (negative).
    Scroll(i32),
    /// Full-screen programs like less or htop have no scrollback, so the wheel sends arrow keys.
    Send(String),
}

/// `delta_y` is the wheel delta in notches (positive = away from the user); each notch is 3 lines.
pub fn wheel_action(delta_y: f64, alternate_screen: bool, application_cursor: bool) -> Option<WheelAction> {
    // Math.Round rounds half to even.
    let lines = (delta_y * 3.0).round_ties_even() as i32;
    if lines == 0 {
        return None;
    }
    if alternate_screen {
        let arrow = match (application_cursor, lines > 0) {
            (true, true) => "\x1bOA",
            (true, false) => "\x1bOB",
            (false, true) => "\x1b[A",
            (false, false) => "\x1b[B",
        };
        Some(WheelAction::Send(arrow.repeat(lines.unsigned_abs() as usize)))
    } else {
        Some(WheelAction::Scroll(lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers { ctrl: false, shift: false, alt: false };
    const CTRL: Modifiers = Modifiers { ctrl: true, shift: false, alt: false };
    const SHIFT: Modifiers = Modifiers { ctrl: false, shift: true, alt: false };
    const ALT: Modifiers = Modifiers { ctrl: false, shift: false, alt: true };

    fn seq(key: Key, mods: Modifiers, app: bool) -> Option<String> {
        key_sequence(key, mods, app)
    }

    #[test]
    fn cursor_keys() {
        assert_eq!(seq(Key::Up, NONE, false).unwrap(), "\x1b[A");
        assert_eq!(seq(Key::Up, NONE, true).unwrap(), "\x1bOA");
        assert_eq!(seq(Key::Left, CTRL, true).unwrap(), "\x1b[1;5D");
        assert_eq!(seq(Key::Home, SHIFT, false).unwrap(), "\x1b[1;2H");
        assert_eq!(seq(Key::End, NONE, true).unwrap(), "\x1bOF");
        let all = Modifiers { ctrl: true, shift: true, alt: true };
        assert_eq!(seq(Key::Right, all, false).unwrap(), "\x1b[1;8C");
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(seq(Key::Enter, NONE, false).unwrap(), "\r");
        assert_eq!(seq(Key::Enter, ALT, false).unwrap(), "\x1b\r");
        assert_eq!(seq(Key::Backspace, NONE, false).unwrap(), "\x7f");
        assert_eq!(seq(Key::Backspace, CTRL, false).unwrap(), "\x08");
        assert_eq!(seq(Key::Backspace, ALT, false).unwrap(), "\x1b\x7f");
        assert_eq!(seq(Key::Tab, SHIFT, false).unwrap(), "\x1b[Z");
        assert_eq!(seq(Key::Escape, CTRL, false).unwrap(), "\x1b");
        assert_eq!(seq(Key::Delete, NONE, false).unwrap(), "\x1b[3~");
        assert_eq!(seq(Key::PageDown, CTRL, false).unwrap(), "\x1b[6;5~");
        assert_eq!(seq(Key::F(1), NONE, true).unwrap(), "\x1bOP");
        assert_eq!(seq(Key::F(4), SHIFT, false).unwrap(), "\x1b[1;2S");
        assert_eq!(seq(Key::F(5), NONE, false).unwrap(), "\x1b[15~");
        assert_eq!(seq(Key::F(12), ALT, false).unwrap(), "\x1b[24;3~");
        assert_eq!(seq(Key::F(13), NONE, false), None);
    }

    #[test]
    fn ctrl_and_alt() {
        assert_eq!(seq(Key::Letter('C'), CTRL, false).unwrap(), "\x03");
        assert_eq!(seq(Key::Letter('A'), Modifiers { ctrl: true, alt: true, shift: false }, false).unwrap(), "\x1b\x01");
        assert_eq!(seq(Key::Space, CTRL, false).unwrap(), "\0");
        assert_eq!(seq(Key::OpenBracket, CTRL, false).unwrap(), "\x1b");
        assert_eq!(seq(Key::Backslash, CTRL, false).unwrap(), "\x1c");
        assert_eq!(seq(Key::Digit(5), CTRL, false).unwrap(), "\x1d");
        assert_eq!(seq(Key::Digit(6), CTRL, false).unwrap(), "\x1e");
        assert_eq!(seq(Key::Slash, CTRL, false).unwrap(), "\x1f");
        assert_eq!(seq(Key::Digit(8), CTRL, false).unwrap(), "\x7f");
        assert_eq!(seq(Key::Digit(1), CTRL, false), None);
        assert_eq!(seq(Key::Letter('X'), ALT, false).unwrap(), "\x1bx");
        assert_eq!(seq(Key::Letter('X'), Modifiers { alt: true, shift: true, ctrl: false }, false).unwrap(), "\x1bX");
        assert_eq!(seq(Key::Digit(7), ALT, false).unwrap(), "\x1b7");
        assert_eq!(seq(Key::Letter('q'), NONE, false), None);
        assert_eq!(seq(Key::Letter('Q'), NONE, false), None);
        assert_eq!(seq(Key::Letter('Q'), Modifiers { ctrl: true, shift: true, alt: false }, false), None);
    }

    #[test]
    fn view_actions() {
        let cs = Modifiers { ctrl: true, shift: true, alt: false };
        assert_eq!(key_action(Key::Letter('C'), cs, false, 24), Some(KeyAction::Copy));
        assert_eq!(key_action(Key::Letter('V'), cs, false, 24), Some(KeyAction::Paste));
        assert_eq!(key_action(Key::Insert, SHIFT, false, 24), Some(KeyAction::Paste));
        assert_eq!(key_action(Key::PageUp, SHIFT, false, 24), Some(KeyAction::Scroll(23)));
        assert_eq!(key_action(Key::PageDown, SHIFT, false, 24), Some(KeyAction::Scroll(-23)));
        assert_eq!(key_action(Key::Plus, CTRL, false, 24), Some(KeyAction::FontSize(1)));
        assert_eq!(key_action(Key::Minus, CTRL, false, 24), Some(KeyAction::FontSize(-1)));
        assert_eq!(key_action(Key::Up, NONE, true, 24), Some(KeyAction::Send("\x1bOA".into())));
        assert_eq!(key_action(Key::Plus, NONE, false, 24), None);
    }

    #[test]
    fn paste() {
        assert_eq!(paste_text("", true), None);
        assert_eq!(paste_text("a\r\nb\nc\rd\u{2028}e", false).unwrap(), "a\rb\rc\rd\re");
        assert_eq!(paste_text("ls\n", true).unwrap(), "\x1b[200~ls\r\x1b[201~");
        assert_eq!(paste_text("x\x1b[201~y", true).unwrap(), "\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn wheel() {
        assert_eq!(wheel_action(0.1, false, false), None);
        assert_eq!(wheel_action(1.0, false, false), Some(WheelAction::Scroll(3)));
        assert_eq!(wheel_action(-0.5, false, false), Some(WheelAction::Scroll(-2)));
        assert_eq!(wheel_action(1.0, true, true), Some(WheelAction::Send("\x1bOA\x1bOA\x1bOA".into())));
        assert_eq!(wheel_action(-0.34, true, false), Some(WheelAction::Send("\x1b[B".into())));
    }
}
