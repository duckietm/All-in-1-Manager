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
        println!(
            "All-in-1 Installer {}\n\n  --dry-run            show the commands instead of running them on a server\n  --renderer=wgpu|glow  draw with DirectX / Vulkan (wgpu) or OpenGL (glow)\n  --version            print the version",
            update::CURRENT_VERSION
        );
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

    // --renderer=glow|wgpu: chosen by hand, or by the first attempt when its renderer could not start. A window
    // can be opened only once per process (winit), so the other renderer is tried in a new process.
    let chosen = args.iter().find_map(|a| a.strip_prefix("--renderer=")).and_then(gui::Renderer::from_name);
    let retry = args.iter().any(|a| a == "--renderer-retry");
    let renderer = chosen.unwrap_or_else(gui::Renderer::preferred);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    if let Err(error) = gui::run(runtime.handle().clone(), dry_run, renderer) {
        let message = format!("The window could not start with {}: {error}", renderer.name());
        eprintln!("{message}");
        log_start_failure(&message);
        if !retry {
            let mut command = match std::env::current_exe() {
                Ok(exe) => std::process::Command::new(exe),
                Err(_) => {
                    show_start_failure(&message);
                    std::process::exit(1);
                }
            };
            command.args(args.iter().filter(|a| !a.starts_with("--renderer")));
            command.arg(format!("--renderer={}", renderer.other().name())).arg("--renderer-retry");
            match command.status() {
                Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                Err(spawn) => show_start_failure(&format!("{message}\n\nStarting again with {} failed: {spawn}", renderer.other().name())),
            }
        } else {
            show_start_failure(&message);
        }
        std::process::exit(1);
    }
}

/// Appends a start failure to start-error.log in the configuration folder, the only trace a windowless program
/// can leave.
fn log_start_failure(message: &str) {
    use std::io::Write;
    let path = aio_core::config_dir::directory().join("start-error.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {} {message}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), update::CURRENT_VERSION);
    }
}

/// Neither renderer started: a message box says why instead of the program quietly not opening.
fn show_start_failure(message: &str) {
    let log = aio_core::config_dir::directory().join("start-error.log");
    let text = format!(
        "{message}\n\nThe graphics driver offers neither DirectX 12 nor OpenGL 2. On a virtual machine: turn on 3D acceleration for its display (VMware: Display > Accelerate 3D graphics) or install its guest tools, then start the installer again.\n\nDetails: {}",
        log.display()
    );
    rfd::MessageDialog::new().set_title("All-in-1 Installer").set_description(text).set_level(rfd::MessageLevel::Error).set_buttons(rfd::MessageButtons::Ok).show();
}
