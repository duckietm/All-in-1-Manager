//! The Room ripper: copies rooms from official Habbo or a retro through G-Earth, with their floor plan, decoration,
//! furni and (where the hotel lets the user read them) wired settings, and imports them into a Polaris hotel as new
//! rooms. It shares the catalogue ripper's G-Earth connection, wire format, hotel profiles and furnidata reader.
//!
//! * [`capture`] records every room the user enters (a passive extension) into a JSON file per room;
//! * [`import`] plans the import of a capture: one transaction, previewed first, recorded for Undo.

pub mod capture;
pub mod import;
pub mod packets;

pub use capture::{capture, list_captures, CaptureEvent, CaptureOptions, CaptureSummary, Command, RoomCapture};
pub use import::{plan, undo_statements, BaseItem, ImportChoice, ImportPlan, ImportRecord, ImportReport, PlanValue, Target};
pub use packets::Door;
