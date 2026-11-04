//! Debug aid: `AIO_SCREENSHOT=<file.png>` (or `HABBO_SCREENSHOT`, as in the converter) saves one screenshot of the
//! window and closes it. `AIO_SCREENSHOT_PAGE` picks the page (dashboard, install, cms, gitea, fail2ban, crowdsec, catalog, addons,
//! deploy, assets, explorer, sql, debug, tools, status, doctor, history, monitor, terminal, settings, about, login); `AIO_SCREENSHOT_THEME=dark|light`
//! forces the theme, `AIO_SCREENSHOT_LOOK=hotel|professional` the look and `AIO_SCREENSHOT_LANG` the language.
//! `AIO_SCREENSHOT_CONNECT=1` logs in to the dry-run recorder first, so the pages show their logged-in state, and
//! `AIO_SCREENSHOT_STEP=<n>` opens the n-th step of the page's wizard, and `AIO_SCREENSHOT_LANGUAGES=1` opens the
//! language list at the top right.

use eframe::egui;

use super::{App, Page};

fn var(name: &str) -> Option<String> {
    std::env::var(format!("AIO_{name}")).or_else(|_| std::env::var(format!("HABBO_{name}"))).ok()
}

/// What the health check finds on the screenshot server: a stopped emulator, a certificate close to its end, an old
/// backup and security updates; the rest is fine.
const DOCTOR_SAMPLE: &str = "emulator=failed\nemulator_port=\nmariadb=active\nnginx=active\nphp=active\ndocker=active\n\
traefik=true\ncrowdsec=true\nfail2ban=missing\nhotel=200\ndisk_used=46\ndisk_free=41231686041\nmem_total=8331239424\n\
mem_available=3123345408\nload=0.62\ncores=4\nhttps=hotel.example.com\ncert_days=12\nbackup_age=9\nupdates=14\n\
security=3\nreboot=\nufw_installed=yes\nufw=yes\nntp=yes\n";

pub struct Screenshot {
    path: Option<String>,
    requested: bool,
}

impl Screenshot {
    pub fn from_env() -> Self {
        Self { path: var("SCREENSHOT"), requested: false }
    }

    pub fn active(&self) -> bool {
        self.path.is_some()
    }

    pub fn tick(&mut self, app: &mut App, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else { return };
        let frame = ctx.cumulative_pass_nr();
        if frame == 0 {
            if let Some(lang) = var("SCREENSHOT_LANG") {
                crate::i18n::set(&lang);
            }
            if let Some(look) = var("SCREENSHOT_LOOK") {
                super::theme::set_hotel(!look.eq_ignore_ascii_case("professional"));
                super::theme::install_fonts(ctx);
            }
            if var("SCREENSHOT_LANGUAGES").is_some() {
                super::language_picker::open(ctx);
            }
            match var("SCREENSHOT_THEME").as_deref() {
                Some("dark") => ctx.set_theme(egui::Theme::Dark),
                Some("light") => ctx.set_theme(egui::Theme::Light),
                _ => {}
            }
            if var("SCREENSHOT_CONNECT").is_some() {
                let login = aio_remote::ServerLogin::password("hotel.example.com", 22, "root", "");
                let recorder = super::dry_run::connect(login);
                // The health check gets a server with a few things to look at, so its page shows every colour.
                recorder.set_responder(|_, command, _| match command {
                    "id -un" => aio_remote::CommandResult::ok("root\n"),
                    "id -u" => aio_remote::CommandResult::ok("0\n"),
                    _ if command.contains("emulator_port=") => aio_remote::CommandResult::ok(DOCTOR_SAMPLE),
                    _ => aio_remote::CommandResult::default(),
                });
                let server: std::sync::Arc<dyn aio_remote::Server> = recorder;
                app.set_session(server);
            }
            let page = var("SCREENSHOT_PAGE").unwrap_or_default();
            if page == "login" {
                app.show_login(None);
            }
            let target = match page.as_str() {
                "install" => Page::Install,
                "cms" | "atomcms" => Page::AtomCms,
                "customcms" => Page::CustomCms,
                "epicnext" => Page::EpicNext,
                "gitea" => Page::Gitea,
                "fail2ban" => Page::Fail2Ban,
                "crowdsec" => Page::CrowdSec,
                "catalog" => Page::DefaultCatalog,
                "addons" => Page::Addons,
                "deploy" => Page::DeployAssets,
                "explorer" => Page::AssetExplorer,
                "assets" => Page::GetAssets,
                "sql" => Page::SqlEditor,
                "debug" => Page::Debug,
                "history" => Page::History,
                "tools" => Page::Tools,
                "status" => Page::ServerStatus,
                "doctor" => Page::Doctor,
                "monitor" => Page::EmulatorMonitor,
                "terminal" => Page::Terminal,
                "settings" => Page::Settings,
                "about" => Page::About,
                _ => Page::Dashboard,
            };
            app.page = target;
            app.go(target);
            // AIO_SCREENSHOT_STEP=<n>: the n-th step of the wizard on that page (1 is the first).
            if let (Some(step), Some(server)) = (var("SCREENSHOT_STEP").and_then(|s| s.parse::<usize>().ok()), app.shared.session()) {
                if let Some(wizard) = app.wizards.get(target) {
                    wizard.jump_to(step.saturating_sub(1), &server);
                }
            }
        }
        // A few frames for the fonts, images and the first detection to settle.
        let wait: u64 = var("SCREENSHOT_FRAMES").and_then(|v| v.parse().ok()).unwrap_or(12);
        if frame == wait && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Some(buffer) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                let _ = buffer.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
