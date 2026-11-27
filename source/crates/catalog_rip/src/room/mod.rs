//! The Room ripper: copies rooms from official Habbo or a retro through G-Earth, with their floor plan, decoration,
//! furni and (where the hotel lets the user read them) wired settings, and imports them into a Polaris hotel as new
//! rooms. It shares the catalogue ripper's G-Earth connection, wire format, hotel profiles and furnidata reader.
//!
//! * [`capture`] records every room the user enters (a passive extension) into a JSON file per room;
//! * [`import`] plans the import of a capture: one transaction, previewed first, recorded for Undo.

pub mod capture;
pub mod import;
pub mod packet_log;
pub mod packets;

pub use capture::{capture, furnidata_rares, list_captures, CaptureEvent, CaptureOptions, CaptureSummary, Command, Recording, RoomCapture};
pub use import::{can_change_hands, catalog_offers_query, catalog_pages_query, plan, reachable_pages, redeemable, undo_already_gone, undo_check_queries, undo_model_statement, undo_problems, undo_statements, BaseItem, CatalogPage, Catalogue, ImportChoice, ImportPlan, ImportRecord, ImportReport, ItemNow, PlanValue, RoomNow, Target, Valuable, ValuableItem, UNDO_ROOM_STATEMENT};
pub use packets::Door;
