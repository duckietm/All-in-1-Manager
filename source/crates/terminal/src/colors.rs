//! The view's colours from `TerminalControl.cs`, as `[r, g, b]` (and alpha where the C# uses one).

use crate::TerminalCell;

pub const BACKGROUND: [u8; 3] = [0x0b, 0x12, 0x20];
pub const DEFAULT_FOREGROUND: [u8; 3] = [0xd5, 0xdc, 0xe8];
/// The selection overlay, `[a, r, g, b]`.
pub const SELECTION: [u8; 4] = [0x70, 0x4f, 0x5f, 0xe6];
/// The focused cursor block, `[a, r, g, b]` (unfocused: a 1px DEFAULT_FOREGROUND outline).
pub const CURSOR: [u8; 4] = [0xc0, 0xd5, 0xdc, 0xe8];
/// Dim text is drawn with this alpha.
pub const DIM_ALPHA: u8 = 0xb0;
/// The "lines back" label's background.
pub const SCROLL_LABEL: [u8; 3] = [0xe3, 0xb3, 0x41];

// The 16 ANSI colours (tuned for the dark background), then the xterm 6x6x6 cube and the grey ramp.
const ANSI: [u32; 16] = [
    0x1B2232, 0xE5534B, 0x57AB5A, 0xC69026, 0x539BF5, 0xB083F0, 0x39C5CF, 0xC9D1D9, 0x636E7B, 0xFF7B72, 0x7EE787,
    0xE3B341, 0x79C0FF, 0xD2A8FF, 0x56D4DD, 0xF0F6FC,
];

pub fn palette() -> [[u8; 3]; 256] {
    let mut colors = [[0u8; 3]; 256];
    for (i, c) in ANSI.iter().enumerate() {
        colors[i] = [(c >> 16) as u8, (c >> 8) as u8, *c as u8];
    }
    let levels = [0u8, 95, 135, 175, 215, 255];
    for i in 0..216 {
        colors[16 + i] = [levels[i / 36], levels[i / 6 % 6], levels[i % 6]];
    }
    for i in 0..24 {
        let v = (8 + i * 10) as u8;
        colors[232 + i] = [v, v, v];
    }
    colors
}

/// `TerminalControl.ToColor`: a cell colour (default, palette index or RGB) as `[r, g, b]`.
pub fn to_rgb(value: i32, foreground: bool, palette: &[[u8; 3]; 256]) -> [u8; 3] {
    if value == TerminalCell::DEFAULT_COLOR {
        if foreground {
            DEFAULT_FOREGROUND
        } else {
            BACKGROUND
        }
    } else if value >= TerminalCell::RGB {
        [(value >> 16) as u8, (value >> 8) as u8, value as u8]
    } else {
        palette[value.clamp(0, 255) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_and_colors() {
        let p = palette();
        assert_eq!(p[1], [0xe5, 0x53, 0x4b]);
        assert_eq!(p[16], [0, 0, 0]);
        assert_eq!(p[196], [255, 0, 0]);
        assert_eq!(p[231], [255, 255, 255]);
        assert_eq!(p[232], [8, 8, 8]);
        assert_eq!(p[255], [238, 238, 238]);
        assert_eq!(to_rgb(-1, true, &p), DEFAULT_FOREGROUND);
        assert_eq!(to_rgb(-1, false, &p), BACKGROUND);
        assert_eq!(to_rgb(TerminalCell::RGB | 0x102030, true, &p), [0x10, 0x20, 0x30]);
    }
}
