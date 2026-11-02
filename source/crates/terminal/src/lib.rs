//! The SSH terminal without its GUI: [`emulator::TerminalEmulator`] (a port of `SourceCode/Terminal/TerminalEmulator.cs`)
//! keeps the screen, and [`keys`] turns keys and pastes into the bytes sent to the shell (the non-GUI part of
//! `TerminalControl.cs`). [`colors`] has the view's palette.
//!
//! A session (see `TerminalSession.cs`) feeds the shell's output into [`TerminalEmulator::feed`], sends
//! [`TerminalEmulator::take_replies`] back to the shell after each feed, sends the key/paste bytes, and on a view
//! resize calls [`TerminalEmulator::resize`] and then changes the SSH channel's window size.

pub mod colors;
pub mod emulator;
pub mod keys;
mod zero_width;

pub use emulator::{CellFlags, TerminalCell, TerminalEmulator, TerminalLine};
pub use keys::{Key, KeyAction, Modifiers};
