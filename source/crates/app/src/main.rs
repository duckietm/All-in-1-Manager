//! All-in-1 Installer: sets up a full hotel server (Polaris Emulator, Octane, Octane Renderer, MariaDB, NGINX, PHP,
//! Traefik, and the addons) on an Ubuntu Server or Debian machine over SSH.
//!
//! `--dry-run` connects to nothing: every command the steps would send is shown in their logs instead.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;
mod i18n;
mod update;
mod views;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("All-in-1 Installer {}\n\n  --dry-run   show the commands instead of running them on a server\n  --version   print the version", update::CURRENT_VERSION);
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("{}", update::CURRENT_VERSION);
        return;
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");

    update::clean_up();
    let settings = aio_core::InstallerSettings::load();
    // The layout and the look are read once at startup, so every step of a run uses the same.
    aio_core::ServerLayout::init(settings.layout.clone().unwrap_or_default());
    gui::theme::init(&settings);
    i18n::init(&settings);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    if let Err(error) = gui::run(runtime.handle().clone(), dry_run) {
        eprintln!("The window could not start: {error}");
        std::process::exit(1);
    }
}
