//! All-in-1 Installer remote side: the SSH connection and, per install step, the scripts it runs on the server as
//! root. Every module mirrors one file of the C# installer's `SourceCode/Remote`, with the same commands in the same
//! order; `tests/golden` holds what the C# code sends, and the parity tests compare with it.

pub mod recorder;
pub mod script;
pub mod server;
pub mod ssh;

pub mod asset_sync;
pub mod catalog_texts;
pub mod atom_cms;
pub mod cms_site;
pub mod crowdsec;
pub mod custom_cms;
pub mod default_catalog;
pub mod dev_tools;
pub mod doctor;
pub mod emulator;
pub mod emulator_assets;
pub mod emulator_monitor;
pub mod external_texts;
pub mod epicnext;
pub mod fail2ban;
pub mod furniture_language;
pub mod gamedata_files;
pub mod git_clone;
pub mod gitea;
pub mod habbo_assets;
pub mod harden;
pub mod hotel_site;
pub mod local_ports;
pub mod mariadb;
pub mod nginx;
pub mod octane;
pub mod php;
pub mod plus_import;
pub mod root_authorized_keys;
pub mod server_status;
pub mod server_tools;
pub mod server_users;
pub mod sql_script;
pub mod sql_session;
pub mod ssh_login;
pub mod ui_texts;
pub mod web_access;

pub use server::{CommandResult, Log, OsInfo, Result, Server, ServerError, ServerLogin};
