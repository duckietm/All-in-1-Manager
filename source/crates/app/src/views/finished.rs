//! The completed pages (FinishedView.cs): the summary of the steps and what to do next. The building blocks (step
//! list, section, code, copy box, key/value grid, CMS database login) are public for the other pages.

use aio_core::{config_dir, DatabaseCredentials};
use aio_remote::crowdsec::{self, CrowdSecResult};
use aio_remote::fail2ban::{self, Fail2BanSettings};
use aio_remote::gitea::{self, GiteaResult, GiteaSettings};
use aio_remote::web_access::WebAccessSettings;
use aio_remote::{atom_cms, custom_cms, emulator, epicnext, octane};
use eframe::egui::{self, Color32, RichText};

use crate::gui::icons::{self, Icon};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::{FinishInfo, FinishPage, StepSummary};
use crate::i18n::{n, t, tf};

pub type Steps = Vec<(String, StepSummary)>;

/// Every folder of /var/www/gamedata and what goes in it (the layout the Octane step configures).
pub const ASSETS: [(&str, &str); 19] = [
    ("config/", n("FurnitureData.json, ProductData.json, FigureData.json, FigureMap.json, EffectMap.json, HabboAvatarActions.json, ExternalTexts.json, UITexts.jsonc")),
    ("furniture/", n("furniture bundles (<name>.hab)")),
    ("clothes/", n("clothing bundles (<name>.hab)")),
    ("effect/", n("avatar effect bundles (<name>.hab)")),
    ("pets/", n("pet bundles (<name>.hab)")),
    ("bundled/generic/", n("generic bundles (<name>.hab)")),
    ("room/", n("room bundles (room/<name>/<name>.json)")),
    ("icons/", n("furniture icons (<name>_icon.png)")),
    ("images/", n("loading_icon.png, clear_icon.png, big_arrow.png, reception/ (hotel view) and wallet/ (currency icons)")),
    ("sounds/", n("sounds (<sample>.mp3, sound_machine_sample_<n>.mp3)")),
    ("c_images/album1584/", n("badges (<code>.gif); custom badges are added here by the emulator")),
    ("c_images/catalogue/", n("catalogue headers and icons (<name>.gif, icon_<n>.png)")),
    ("c_images/reception/", n("login screen (stretch_blue.png, sun.png, drape.png, ts.png, US_right.png ...)")),
    ("c_images/notifications/, Quests/, fx_icons/, habboway/, web_promo_small/, album1358/", n("notification, achievement, effect and promo images")),
    ("c_images/groupbadge/badgeparts/", n("group badge parts (badgepart_<part>.png)")),
    ("habbopages/", n("the hotel's help/info pages")),
    ("custom/", n("your own extra files (served as /custom/)")),
    ("news/news.json", n("login screen news (a welcome message is already there)")),
    ("camera/, c_images/groupbadge/badge/", n("filled by the emulator: do not upload")),
];

// ---------------------------------------------------------------------------------------------------------------
// Building blocks

/// A finish page drawn by a closure over its data.
pub struct Page(Box<dyn Fn(&mut egui::Ui, &Palette) + Send>);

impl Page {
    pub fn boxed(draw: impl Fn(&mut egui::Ui, &Palette) + Send + 'static) -> Box<dyn FinishPage> {
        Box::new(Page(Box::new(draw)))
    }
}

impl FinishPage for Page {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 18.0;
            (self.0)(ui, palette);
        });
    }
}

/// Every step done or already installed.
pub fn clean(steps: &[(String, StepSummary)]) -> bool {
    steps.iter().all(|(_, r)| matches!(r, StepSummary::Done | StepSummary::AlreadyInstalled))
}

/// What every step did, in a wrapping row. (StepList)
pub fn step_list(ui: &mut egui::Ui, palette: &Palette, steps: &[(String, StepSummary)]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (title, result) in steps {
            let (icon, color, label) = match result {
                StepSummary::Done => (Icon::Check, palette.success_text, t("done")),
                StepSummary::AlreadyInstalled => (Icon::Check, palette.success_text, t("already installed")),
                StepSummary::Skipped => (Icon::ChevronRight, palette.muted, t("skipped")),
                StepSummary::Failed => (Icon::Close, palette.error_text, t("failed")),
                StepSummary::NotRun => (Icon::ChevronRight, palette.muted, t("not run")),
            };
            icons::show(ui, icon, 14.0, color);
            ui.label(RichText::new(title).size(12.5).strong());
            ui.label(RichText::new(label).size(12.0).color(palette.subtle));
            ui.add_space(12.0);
        }
    });
}

/// A heading with its content under it. (Section)
pub fn section(ui: &mut egui::Ui, heading: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.label(widgets::title_text(heading, 15.0));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            content(ui);
        });
    });
}

/// Wrapping text in a colour.
pub fn colored(ui: &mut egui::Ui, text: &str, size: f32, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(size).color(color)).wrap());
}

/// Selectable monospace text (commands, paths, SQL). (Code)
pub fn code(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(palette.text)).wrap().selectable(true));
}

/// The text in large monospace with a Copy button, for pasting it elsewhere; the button then says Copied. (CopyBox)
pub fn copy_box(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    let id = ui.id().with(("copy_box", text));
    let copied = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    egui::Frame::new()
        .fill(palette.hover)
        .stroke(egui::Stroke::new(1.0_f32, palette.card_border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_min_width(520.0_f32.min(ui.available_width()));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                ui.add(egui::Label::new(RichText::new(text).monospace().size(18.0).strong().color(palette.text)).selectable(true));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(widgets::primary_button(if copied { t("Copied") } else { t("Copy") }).min_size(egui::vec2(70.0, 32.0))).clicked() {
                        ui.ctx().copy_text(text.to_string());
                        ui.data_mut(|d| d.insert_temp(id, true));
                    }
                });
            });
        });
}

/// Two columns: a muted label and a value drawn by `value`.
pub fn key_value_grid<'a>(
    ui: &mut egui::Ui,
    palette: &Palette,
    id: &str,
    column_spacing: f32,
    rows: impl IntoIterator<Item = (&'a str, &'a str)>,
    mut value: impl FnMut(&mut egui::Ui, &str, &str),
) {
    egui::Grid::new(id).num_columns(2).spacing(egui::vec2(column_spacing, 4.0)).show(ui, |ui| {
        for (key, v) in rows {
            ui.label(RichText::new(key).size(12.5).color(palette.muted));
            value(ui, key, v);
            ui.end_row();
        }
    });
}

/// The database login the server installation made for a CMS (configuration/database/<server>/mariadb.txt).
/// (CmsDatabaseLogin)
pub fn cms_database_login(ui: &mut egui::Ui, palette: &Palette, credentials: Option<&DatabaseCredentials>, host: &str) {
    widgets::text(
        ui,
        t("The installer cannot fill in the database settings of a CMS it does not know: put these in your CMS' settings (for example DB_HOST, DB_DATABASE, DB_USERNAME and DB_PASSWORD in its .env). Use the cms account the server installation made, not root."),
        13.0,
    );
    let Some(credentials) = credentials else {
        colored(
            ui,
            &tf(
                "The MariaDB passwords of {host} are not saved on this computer. They are in configuration/database/<server>/mariadb.txt on the computer that ran the MariaDB step.",
                &[("host", host)],
            ),
            13.0,
            palette.warning_text,
        );
        return;
    };
    let port = credentials.port.to_string();
    let rows = [
        (t("Host"), "127.0.0.1"),
        (t("Port"), port.as_str()),
        (t("Database"), credentials.database.as_str()),
        (t("User"), credentials.cms_user.as_str()),
        (t("Password"), credentials.cms_password.as_str()),
    ];
    let password_label = t("Password");
    key_value_grid(ui, palette, "cms_database_login", 18.0, rows, |ui, key, value| {
        if key == password_label {
            copy_box(ui, palette, value);
        } else {
            code(ui, palette, value);
        }
    });
    colored(
        ui,
        &tf("Saved on this computer in {path}.", &[("path", &config_dir::database_credentials_path(host).display().to_string())]),
        12.0,
        palette.subtle,
    );
}

/// The WinSCP session to upload the assets with.
pub fn winscp(ui: &mut egui::Ui, palette: &Palette, host: &str, ssh_port: u16, putty_key: Option<&str>) {
    widgets::text(ui, t("New session in WinSCP:"), 13.0);
    let host_port = format!("{host} / {ssh_port}");
    let key = format!("{}   {}", putty_key.unwrap_or(t("your .ppk key")), t("(Advanced... > SSH > Authentication > Private key file)"));
    let upload = format!("{}/  {}", octane::gamedata(), t("(keep the folder names of the table below)"));
    let rows = [
        (t("File protocol"), "SFTP"),
        (t("Host name / port"), host_port.as_str()),
        (t("User name"), "root"),
        (t("Private key"), key.as_str()),
        (t("Upload to"), upload.as_str()),
    ];
    key_value_grid(ui, palette, "winscp", 14.0, rows, |ui, _, value| code(ui, palette, value));
    colored(ui, t("WinSCP uses the .ppk directly; with a key password it asks for it once (or load the .ppk in Pageant)."), 12.0, palette.subtle);
}

/// Every gamedata folder and what goes in it.
pub fn asset_table(ui: &mut egui::Ui, palette: &Palette) {
    let gamedata = octane::gamedata();
    egui::Grid::new("asset_table").num_columns(2).spacing(egui::vec2(18.0, 6.0)).show(ui, |ui| {
        for (folder, contents) in ASSETS {
            ui.scope(|ui| {
                ui.set_max_width(380.0);
                ui.add(
                    egui::Label::new(RichText::new(format!("{gamedata}/{folder}")).monospace().size(12.0).strong().color(palette.text))
                        .wrap()
                        .selectable(true),
                );
            });
            widgets::text(ui, t(contents), 12.5);
            ui.end_row();
        }
    });
}

fn after_upload(ui: &mut egui::Ui, palette: &Palette, site: Option<&WebAccessSettings>) {
    widgets::text(ui, t("Give the emulator (group www-data) write access to the new files: run this on the server once the upload is done:"), 13.0);
    code(ui, palette, &format!("sudo {}", octane::PERMISSIONS_SCRIPT));
    let text = match site {
        Some(site) => tf(
            "Open the hotel at {url}, register the first account, log out, and make it administrator while it is offline (HeidiSQL over the SSH tunnel, database habbo):",
            &[("url", &site.url())],
        ),
        None => t("Open the hotel, register the first account, log out, and make it administrator while it is offline (HeidiSQL over the SSH tunnel, database habbo):").into(),
    };
    widgets::text(ui, &text, 13.0);
    code(ui, palette, "UPDATE users SET `rank` = 7 WHERE username = 'YourName';");
    colored(ui, t("Log in again to get the rank. Changed the Octane configuration? Rebuild it with /var/www/Octane/build.sh."), 12.0, palette.subtle);
}

const RUN_AGAIN: &str = n("Run the step again (see the step for what went wrong).");

// ---------------------------------------------------------------------------------------------------------------
// The pages

/// Install New Server: the summary, the WinSCP upload and where the assets go. (Build)
pub fn server(info: FinishInfo) -> Box<dyn FinishPage> {
    let FinishInfo { host, ssh_port, site, putty_key, steps, .. } = info;
    let putty_key = putty_key.map(|p| p.display().to_string());
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            if ok { t("Successfully completed") } else { t("Completed, with steps to look at") },
            &format!(
                "{} {}",
                if ok {
                    t("The server is installed.")
                } else {
                    t("Some steps were skipped or failed (see below): run them again later if you need them.")
                },
                t("Now put the game assets on the server: download them with Assets → Habbo assets and send them with Assets → Deploy Assets (or upload them yourself with WinSCP into the folders below).")
            ),
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. Download the assets"), |ui| {
            widgets::text(ui, t("Assets → Habbo assets downloads everything (gamedata, furniture, clothes, badges ...) in the server's layout; Assets → Deploy Assets then sends only what is new."), 13.0);
        });
        section(ui, t("2. Or upload them yourself with WinSCP"), |ui| winscp(ui, palette, &host, ssh_port, putty_key.as_deref()));
        section(ui, &tf("3. Where everything goes ({gamedata})", &[("gamedata", &octane::gamedata())]), |ui| asset_table(ui, palette));
        section(ui, t("4. After uploading"), |ui| after_upload(ui, palette, site.as_ref()));
    })
}

/// The AtomCMS addon: the web installer key, the paths it already has, and admin rights. (BuildAtomCms)
pub fn atom_cms(site: Option<WebAccessSettings>, installation_key: Option<String>, installation_completed: bool, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let url = site.as_ref().map(|s| s.url()).unwrap_or_else(|| t("your hotel").into());
        let client = site.as_ref().map(|s| s.client_url()).unwrap_or_else(|| "/client/".into());
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            if ok { t("AtomCMS is installed") } else { t("AtomCMS was not installed") },
            &if ok {
                tf("{url} is now AtomCMS; Octane is on {client} and opens from the CMS after logging in.", &[("url", &url), ("client", &client)])
            } else {
                t("Run the AtomCMS step again when the server is ready (see the step for what is missing).").into()
            },
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. AtomCMS' web installer"), |ui| {
            if installation_completed {
                widgets::text(ui, t("The web installer is already finished. Change these settings later in the housekeeping (Website settings)."), 13.0);
            } else {
                widgets::text(ui, &tf("Open {url}: AtomCMS asks for its installation key first. Copy it from here:", &[("url", &url)]), 13.0);
                match &installation_key {
                    None => code(ui, palette, "SELECT installation_key FROM website_installation;"),
                    Some(key) => copy_box(ui, palette, key),
                }
                widgets::text(ui, t("Then go through the settings; the paths for this server are already filled in:"), 13.0);
            }
            let settings: Vec<(String, String)> = atom_cms::website_settings("").into_iter().filter(|(k, _)| k != "hotel_name").collect();
            key_value_grid(ui, palette, "atom_settings", 18.0, settings.iter().map(|(k, v)| (k.as_str(), v.as_str())), |ui, _, v| code(ui, palette, v));
        });
        section(ui, t("2. Become administrator"), |ui| {
            widgets::text(
                ui,
                &tf("Register your account on {url}, log out, and give it the highest rank while it is offline (HeidiSQL over the SSH tunnel, database habbo):", &[("url", &url)]),
                13.0,
            );
            code(ui, palette, "UPDATE users SET `rank` = 7 WHERE username = 'YourName';");
            colored(ui, t("The housekeeping is on /housekeeping (rank 6 and up)."), 12.0, palette.subtle);
        });
        section(ui, t("3. Later"), |ui| {
            widgets::text(
                ui,
                &tf(
                    "Captcha (Cloudflare Turnstile or Google reCAPTCHA): put the keys in {directory}/.env, then turn it on in the housekeeping.",
                    &[("directory", &atom_cms::directory())],
                ),
                13.0,
            );
            widgets::text(ui, t("Updating AtomCMS (git pull, composer install, migrations, theme) and Octane: run this addon again."), 13.0);
            colored(ui, &tf("Rebuilding only Octane (for /client): {octane}/build.sh.", &[("octane", &octane::octane())]), 12.0, palette.subtle);
        });
    })
}

/// The CMS addon with your own CMS: the database login to fill in, and what else is the owner's. (BuildCustomCms)
pub fn custom_cms(
    site: Option<WebAccessSettings>,
    name: Option<String>,
    root: Option<String>,
    php: bool,
    credentials: Option<DatabaseCredentials>,
    host: String,
    steps: Steps,
) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let url = site.as_ref().map(|s| s.url()).unwrap_or_else(|| t("your hotel").into());
        let client = site.as_ref().map(|s| s.client_url()).unwrap_or_else(|| "/client/".into());
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Warning } else { ResultKind::Error },
            if ok { t("Your CMS is installed: its database settings are next") } else { t("Your CMS was not installed") },
            &if ok {
                tf(
                    "{url} is now your CMS ({directory}, {kind}, root {root}); Octane is on {client}.",
                    &[
                        ("url", &url),
                        ("directory", &name.as_deref().map(custom_cms::directory).unwrap_or_default()),
                        ("kind", if php { "PHP" } else { t("static") }),
                        ("root", root.as_deref().unwrap_or("/")),
                        ("client", &client),
                    ],
                )
            } else {
                t("Run the step again when the server is ready (see the step for what is missing).").into()
            },
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. Database settings of your CMS"), |ui| cms_database_login(ui, palette, credentials.as_ref(), &host));
        section(ui, t("2. Hotel paths for your CMS"), |ui| {
            widgets::text(
                ui,
                t("Point your CMS at the hotel: the client is on /client (the CMS opens it with the SSO ticket, as AtomCMS does), the images on /gamedata:"),
                13.0,
            );
            code(
                ui,
                palette,
                &format!(
                    "client: /client/index.html?sso=<ticket>\nbadges: /gamedata/c_images/album1584\nfurniture icons: /gamedata/icons\nRCON: 127.0.0.1, the port in {}/config.ini (rcon.port)",
                    emulator::directory()
                ),
            );
        });
        section(ui, t("3. Later"), |ui| {
            widgets::text(ui, t("Updating: run this addon again (git pull for a GitHub link, or choose a new ZIP; your old folder is moved aside first)."), 13.0);
            widgets::text(ui, t("PHP packages (composer install) or a theme build (npm) that your CMS needs: run them in the SSH terminal (Tools), in the CMS folder."), 13.0);
            colored(ui, t("Back to AtomCMS: choose AtomCMS in Addons → CMS."), 12.0, palette.subtle);
        });
    })
}

/// The CMS addon with EpicNext: the first admin, the client URL and where its settings are. (BuildEpicNext)
pub fn epicnext(site: Option<WebAccessSettings>, atom_tables: bool, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let url = site.as_ref().map(|s| s.url()).unwrap_or_else(|| t("your hotel").into());
        let client = site.as_ref().map(|s| s.client_url()).unwrap_or_else(|| "/client/".into());
        let message = if !ok {
            t("Run the step again when the server is ready (see the step for what went wrong).").to_string()
        } else {
            let mut text = tf("Octane is on {client}. Make yourself admin and set the client URL below.", &[("client", &client)]);
            if atom_tables {
                text.push(' ');
                text.push_str(t("The hotel database has AtomCMS' website_* tables: tables with the same name keep AtomCMS' layout, so some EpicNext pages may fail."));
            }
            text
        };
        widgets::result_banner(
            ui,
            palette,
            if !ok {
                ResultKind::Error
            } else if atom_tables {
                ResultKind::Warning
            } else {
                ResultKind::Success
            },
            &if ok { tf("EpicNext is the website of {url}", &[("url", &url)]) } else { t("EpicNext was not installed").into() },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. First admin"), |ui| {
            widgets::text(
                ui,
                &tf(
                    "Register on {url}/register (or log in with an account you already have in the hotel), then give it rank 7 (SQL Editor in the menu on the left):",
                    &[("url", &url)],
                ),
                13.0,
            );
            code(ui, palette, "UPDATE users SET rank = 7 WHERE username = 'yourname';");
        });
        section(ui, t("2. Client URL"), |ui| {
            widgets::text(
                ui,
                t("EpicNext's /client page opens Octane with the player's SSO ticket. The step set nitro_client_url (Admin → CMS Settings) to Octane on this site:"),
                13.0,
            );
            code(ui, palette, "/client/index.html");
            colored(ui, t("Change it only for a client somewhere else."), 12.0, palette.subtle);
        });
        section(ui, t("3. Later"), |ui| {
            widgets::text(ui, t("Updating: run this addon again (git pull, pnpm install, migrate and build; the site is down during the build)."), 13.0);
            widgets::text(
                ui,
                &tf(
                    "More settings (mail, Valkey, Turnstile, OAuth ...) are in {directory}/.env; afterwards: systemctl restart {service}.",
                    &[("directory", epicnext::DIRECTORY), ("service", epicnext::SERVICE)],
                ),
                13.0,
            );
            colored(
                ui,
                &tf("Its log: journalctl -u {service}. Back to AtomCMS: choose AtomCMS in Addons → CMS.", &[("service", epicnext::SERVICE)]),
                12.0,
                palette.subtle,
            );
        });
    })
}

/// The Gitea addon: the address, the admin login and how to use it. (BuildGitea)
pub fn gitea(settings: Option<GiteaSettings>, result: Option<GiteaResult>, login_file: String, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let url = settings.as_ref().map(|s| s.url()).unwrap_or_else(|| t("your Gitea").into());
        let no_certificate = result.as_ref().is_some_and(|r| !r.certificate_valid);
        let message = if !ok {
            t(RUN_AGAIN).to_string()
        } else if no_certificate {
            tf(
                "{domain} has no certificate yet: check its A record{proxy}; Traefik keeps trying.",
                &[
                    ("domain", settings.as_ref().map(|s| s.domain.as_str()).unwrap_or("")),
                    ("proxy", if settings.as_ref().is_some_and(|s| s.cloudflare) { t(" (proxy on)") } else { "" }),
                ],
            )
        } else {
            t("Open it and log in as the admin.").into()
        };
        widgets::result_banner(
            ui,
            palette,
            if !ok || no_certificate { ResultKind::Warning } else { ResultKind::Success },
            &if ok { tf("Gitea runs on {url}", &[("url", &url)]) } else { t("Gitea was not installed").into() },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. Log in"), |ui| {
            match result.as_ref().and_then(|r| r.admin_password.as_deref()) {
                Some(password) => {
                    widgets::text(
                        ui,
                        &tf(
                            "Open {url} and log in as {user} with this password (saved on this computer in {file}):",
                            &[("url", &url), ("user", settings.as_ref().map(|s| s.admin_user.as_str()).unwrap_or("")), ("file", &login_file)],
                        ),
                        13.0,
                    );
                    copy_box(ui, palette, password);
                }
                None => widgets::text(
                    ui,
                    t("The admin account was there already and is kept. A new password: docker exec -u git gitea gitea admin user change-password --username <name> --password <new>"),
                    13.0,
                ),
            }
            colored(ui, t("Registration is closed: make accounts in Site Administration → User Accounts."), 12.0, palette.subtle);
        });
        section(ui, t("2. Use it"), |ui| {
            widgets::text(ui, t("Clone and push over https, with your Gitea name and password (or an access token from Settings → Applications):"), 13.0);
            code(ui, palette, &format!("git clone {url}/<owner>/<repository>.git"));
            colored(ui, t("SSH (git@...) is off: Cloudflare's proxy does not pass it, and the server's own SSH stays apart."), 12.0, palette.subtle);
        });
        section(ui, t("3. Later"), |ui| {
            widgets::text(
                ui,
                &tf(
                    "Update Gitea: run this addon again. Its repositories, accounts and settings are in {directory}/data: include it in your backups.",
                    &[("directory", gitea::DIRECTORY)],
                ),
                13.0,
            );
            colored(
                ui,
                &tf("Settings beyond these: {directory}/data/gitea/conf/app.ini (docs.gitea.com).", &[("directory", gitea::DIRECTORY)]),
                12.0,
                palette.subtle,
            );
        });
    })
}

/// Remove Gitea. (BuildRemoveGitea)
pub fn remove_gitea(domain: Option<String>, data_deleted: bool, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let message = if !ok {
            t(RUN_AGAIN).to_string()
        } else {
            let mut text = domain.as_deref().map(|d| format!("{} ", tf("{domain} no longer reaches Gitea.", &[("domain", d)]))).unwrap_or_default();
            text.push_str(&if data_deleted {
                tf("{directory} is deleted.", &[("directory", gitea::DIRECTORY)])
            } else {
                tf("Its repositories, accounts and settings stay in {directory}.", &[("directory", gitea::DIRECTORY)])
            });
            text
        };
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            if ok { t("Gitea is removed") } else { t("Gitea was not removed") },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("Next"), |ui| {
            widgets::text(
                ui,
                if data_deleted {
                    t("Gitea again: Addons → Gitea → Install, with a new admin.")
                } else {
                    t("Gitea again: Addons → Gitea → Install; it starts with the repositories and accounts that stayed.")
                },
                13.0,
            );
            if let Some(domain) = &domain {
                widgets::text(ui, &tf("Remove the A record of {domain} from your DNS when you no longer need it.", &[("domain", domain)]), 13.0);
            }
        });
    })
}

/// The Fail2Ban addon: the settings, the whitelist and where to change them. (BuildFail2Ban)
pub fn fail2ban(settings: Option<Fail2BanSettings>, ssh_ports: Vec<i64>, client_ip: Option<String>, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let ports = if ssh_ports.is_empty() { "22".to_string() } else { ssh_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ") };
        let message = match (&settings, ok) {
            (_, false) => t(RUN_AGAIN).to_string(),
            (None, true) => t("Fail2ban runs.").into(),
            (Some(s), true) => tf(
                "{retries} failed logins within {find} minutes ban an address for {ban} minutes{increment}.",
                &[
                    ("retries", &s.max_retry.to_string()),
                    ("find", &s.find_time_minutes.to_string()),
                    ("ban", &s.ban_time_minutes.to_string()),
                    ("increment", if s.increment { t(", longer for repeat offenders") } else { "" }),
                ],
            ),
        };
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            &if ok { tf("Fail2Ban protects SSH (port {ports})", &[("ports", &ports)]) } else { t("Fail2Ban was not set up").into() },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. Whitelist"), |ui| {
            match settings.as_ref().filter(|s| !s.whitelist.is_empty()) {
                Some(s) => {
                    widgets::text(ui, t("Never banned (besides this server itself):"), 13.0);
                    code(ui, palette, &s.whitelist.join("\n"));
                }
                None => widgets::text(ui, t("The whitelist is empty: only the server itself is never banned."), 13.0),
            }
            if let Some(ip) = &client_ip {
                if !settings.as_ref().is_some_and(|s| s.whitelist.contains(ip)) {
                    let retries = settings.as_ref().map(|s| s.max_retry).unwrap_or(5).to_string();
                    colored(
                        ui,
                        &tf(
                            "This computer ({ip}) is not on it: after {retries} wrong passwords you are locked out for a while too.",
                            &[("ip", ip), ("retries", &retries)],
                        ),
                        12.0,
                        palette.warning_text,
                    );
                }
            }
            colored(ui, t("Edit it, and see or lift bans: Tools → Fail2Ban."), 12.0, palette.subtle);
        });
        section(ui, t("2. Later"), |ui| {
            widgets::text(
                ui,
                t("Locked out anyway? Log in from another address (or the provider's web console) and run: fail2ban-client set sshd unbanip <your IP>"),
                13.0,
            );
            colored(
                ui,
                &tf(
                    "Status on the server: fail2ban-client status sshd. Settings: {file} (rewritten by this addon and by Tools → Fail2Ban).",
                    &[("file", fail2ban::CONFIG_FILE)],
                ),
                12.0,
                palette.subtle,
            );
        });
    })
}

/// The Default catalogue addon: what is in the hotel now, the backup, and what the client still needs.
pub fn default_catalog(
    result: Option<aio_remote::default_catalog::CatalogResult>,
    version: String,
    language: Option<aio_remote::furniture_language::FurniLanguage>,
    switched: Option<aio_remote::furniture_language::SwitchResult>,
    texts: Option<(usize, usize)>,
    steps: Steps,
) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let message = match (&result, ok) {
            (Some(r), true) => tf(
                "{pages} pages, {items} catalogue items, {furniture} furniture. Players see it after they reload the hotel.",
                &[("pages", &r.pages.to_string()), ("items", &r.catalog_items.to_string()), ("furniture", &r.furniture.to_string())],
            ),
            (None, true) => t("The catalogue was already installed.").into(),
            (_, false) => t(RUN_AGAIN).to_string(),
        };
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            &if ok { tf("The default catalogue ({version}) is installed", &[("version", &version)]) } else { t("The catalogue was not installed").into() },
            &message,
        );
        step_list(ui, palette, &steps);
        if let Some(r) = &result {
            section(ui, t("1. As it was before"), |ui| {
                widgets::text(ui, t("The database before the import (Tools → Import full database puts it back):"), 13.0);
                code(ui, palette, &r.safety_backup);
                if let Some(old) = &r.old_furniture_data {
                    widgets::text(ui, t("The FurnitureData.json that was there:"), 13.0);
                    code(ui, palette, old);
                }
            });
        }
        if let Some(language) = language {
            section(ui, t("Furniture language"), |ui| {
                widgets::text(ui, &tf("The furniture names and descriptions are in {language}.", &[("language", super::default_catalog::native_name(language))]), 13.0);
                if let Some(s) = &switched {
                    colored(
                        ui,
                        &tf(
                            "From {domain}: {furni} furni ({custom} of the hotel's own kept). Changed in the database: {sprites} sprite ids, {items} catalogue items, {offers} offer ids.",
                            &[
                                ("domain", language.domain),
                                ("furni", &s.furni.to_string()),
                                ("custom", &s.custom_kept.to_string()),
                                ("sprites", &s.plan.sprite_ids.len().to_string()),
                                ("items", &s.plan.item_ids.len().to_string()),
                                ("offers", &s.plan.offer_ids.len().to_string()),
                            ],
                        ),
                        12.0,
                        palette.subtle,
                    );
                }
                if let Some((count, kept)) = texts {
                    widgets::text(
                        ui,
                        &tf(
                            "ExternalTexts.json is in that language too: {count} texts ({kept} of the hotel's own kept).",
                            &[("count", &count.to_string()), ("kept", &kept.to_string())],
                        ),
                        13.0,
                    );
                }
                colored(ui, t("Another language later: Tools → Furniture language."), 12.0, palette.subtle);
            });
        }
        section(ui, t("2. The furniture files"), |ui| {
            widgets::text(
                ui,
                t("The catalogue names furniture; the client also needs their files and the catalogue icons in gamedata. Put them there with Deploy Assets, or check with the Asset Explorer."),
                13.0,
            );
            colored(ui, &tf("FurnitureData.json is in {file}.", &[("file", &aio_remote::default_catalog::furniture_data())]), 12.0, palette.subtle);
        });
    })
}

/// Remove Fail2Ban. (BuildRemoveFail2Ban)
pub fn remove_fail2ban(steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            if ok { t("Fail2Ban is removed") } else { t("Fail2Ban was not removed") },
            if ok { t("SSH is no longer protected by fail2ban; every ban is lifted.") } else { t(RUN_AGAIN) },
        );
        step_list(ui, palette, &steps);
        section(ui, t("Next"), |ui| widgets::text(ui, t("Fail2Ban again: Addons → Fail2Ban → Install."), 13.0));
    })
}

/// The CrowdSec addon: accept the engine in the console, what it does, and later. (BuildCrowdSec)
pub fn crowdsec(site: Option<WebAccessSettings>, name: Option<String>, enrolled_now: bool, result: Option<CrowdSecResult>, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let not_connected = result.as_ref().is_some_and(|r| !r.bouncer_connected);
        let name = name.clone().unwrap_or_default();
        widgets::result_banner(
            ui,
            palette,
            if !ok || not_connected { ResultKind::Warning } else { ResultKind::Success },
            if ok { t("CrowdSec protects the hotel") } else { t("CrowdSec was not set up") },
            if !ok {
                t(RUN_AGAIN)
            } else if not_connected {
                t("Traefik's plugin had not fetched the ban list yet: check Debug → Traefik for crowdsec lines in a minute.")
            } else {
                t("Every request through Traefik is checked; banned addresses get 403 Forbidden.")
            },
        );
        step_list(ui, palette, &steps);
        section(ui, t("1. Accept the server in the CrowdSec console"), |ui| {
            if enrolled_now {
                widgets::text(
                    ui,
                    &tf(
                        "Open the CrowdSec console with the button below (or app.crowdsec.net) and accept \"{name}\": on the welcome page or under Security Engines, click Accept enroll. Only then do its alerts and bans show in the console.",
                        &[("name", &name)],
                    ),
                    13.0,
                );
                colored(ui, t("Not there? Refresh the page after a minute: CrowdSec sends the request when it starts."), 12.0, palette.subtle);
            } else {
                widgets::text(
                    ui,
                    &tf(
                        "This server was enrolled before and stays enrolled as \"{name}\": its alerts are in the CrowdSec console (app.crowdsec.net).",
                        &[("name", &name)],
                    ),
                    13.0,
                );
            }
        });
        section(ui, t("2. What it does"), |ui| {
            widgets::text(
                ui,
                if site.as_ref().is_some_and(|s| s.cloudflare) {
                    t("Behind Cloudflare: CrowdSec and the bouncer use the visitor's real address from Cloudflare's X-Forwarded-For (trusted only from Cloudflare's ranges, refreshed weekly), never Cloudflare's own.")
                } else {
                    t("Visitors connect straight to the server: CrowdSec sees their own addresses.")
                },
                13.0,
            );
            widgets::text(
                ui,
                t("It bans for scans, probing, known exploits (CVEs) and brute force seen in Traefik's access log, plus the community blocklist. The hotel's own files (gamedata, the client) are never counted."),
                13.0,
            );
            colored(
                ui,
                &tf(
                    "If CrowdSec stops, the hotel stays reachable (nobody is blocked until it runs again). Traefik's access log: {log} (a week kept).",
                    &[("log", crowdsec::ACCESS_LOG)],
                ),
                12.0,
                palette.subtle,
            );
        });
        section(ui, t("3. Later"), |ui| {
            widgets::text(ui, t("Whitelist and bans: Tools → CrowdSec. Live log: Debug → CrowdSec."), 13.0);
            colored(
                ui,
                t("On the server: docker exec crowdsec cscli decisions list (bans), cscli decisions delete -i <IP> (unban), cscli alerts list, cscli metrics."),
                12.0,
                palette.subtle,
            );
            colored(
                ui,
                &tf(
                    "Update CrowdSec: run this addon again (its data and enrollment stay). Your own compose additions go in {directory}/compose.override.yaml.",
                    &[("directory", crowdsec::DIRECTORY)],
                ),
                12.0,
                palette.subtle,
            );
        });
    })
}

/// Remove CrowdSec. (BuildRemoveCrowdSec)
pub fn remove_crowdsec(data_deleted: bool, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let message = if !ok {
            t(RUN_AGAIN).to_string()
        } else {
            format!(
                "{} {}",
                t("Traefik no longer checks visitors with CrowdSec."),
                if data_deleted {
                    tf("{directory} is deleted.", &[("directory", crowdsec::DIRECTORY)])
                } else {
                    tf("Its data and enrollment stay in {directory}.", &[("directory", crowdsec::DIRECTORY)])
                }
            )
        };
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            if ok { t("CrowdSec is removed") } else { t("CrowdSec was not removed") },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("Next"), |ui| {
            widgets::text(
                ui,
                if data_deleted {
                    t("CrowdSec again: Addons → CrowdSec → Install, with a new enrollment key.")
                } else {
                    t("CrowdSec again: Addons → CrowdSec → Install; no new key needed.")
                },
                13.0,
            );
            widgets::text(ui, t("Remove the engine from your CrowdSec console (app.crowdsec.net → Security Engines) when you no longer need it."), 13.0);
        });
    })
}

/// Remove the CMS. (BuildRemoveCms)
pub fn remove_cms(site: Option<WebAccessSettings>, removed: Option<String>, files_deleted: bool, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let message = if !ok {
            t(RUN_AGAIN).to_string()
        } else {
            let mut text = match &removed {
                Some(cms) if files_deleted => format!("{} ", tf("{cms} is removed and its files are deleted.", &[("cms", cms)])),
                Some(cms) => format!("{} ", tf("{cms} is removed; its files stay on the server.", &[("cms", cms)])),
                None => String::new(),
            };
            text.push_str(t("Octane has its own login screen again, and NGINX and Traefik are as after the server installation."));
            text
        };
        let url = site.as_ref().map(|s| s.url()).unwrap_or_else(|| t("your hotel").into());
        widgets::result_banner(
            ui,
            palette,
            if ok { ResultKind::Success } else { ResultKind::Warning },
            &if ok { tf("Octane is the website of {url} again", &[("url", &url)]) } else { t("The CMS was not removed").into() },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("Next"), |ui| {
            widgets::text(ui, t("The CMS' tables and its users stay in the hotel database. Players log in on Octane's own login screen now."), 13.0);
            widgets::text(ui, t("A CMS again: Addons → CMS."), 13.0);
        });
    })
}

/// Deploy Assets: what each of its steps did. (BuildDeploy)
pub fn deploy(site: Option<WebAccessSettings>, summaries: Vec<Option<String>>, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let failed = steps.iter().any(|(_, r)| matches!(r, StepSummary::Failed | StepSummary::NotRun));
        let done = summaries.iter().flatten().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let message = if failed {
            let mut text = t("Run the step again (see the step for the reason).").to_string();
            if !done.is_empty() {
                text.push_str(&format!(" {} {done}", t("Done:")));
            }
            text
        } else if done.is_empty() {
            t("Nothing needed to change.").into()
        } else {
            done
        };
        widgets::result_banner(
            ui,
            palette,
            if failed { ResultKind::Warning } else { ResultKind::Success },
            if failed { t("Not everything finished") } else { t("The server has the assets") },
            &message,
        );
        step_list(ui, palette, &steps);
        section(ui, t("Next"), |ui| {
            let client = site.as_ref().map(|s| s.client_url()).unwrap_or_else(|| t("the hotel").into());
            widgets::text(ui, &tf("Players get the new files when they reload {client}.", &[("client", &client)]), 13.0);
            widgets::text(
                ui,
                t("New furniture also needs its rows in items_base and catalog_items: Assets → Import Assets adds them (and Import custom furniture for a retro's own furni)."),
                13.0,
            );
            colored(ui, t("Download more with Assets → Habbo assets and sync again: only the new files are sent."), 12.0, palette.subtle);
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_steps() {
        assert!(clean(&[("a".into(), StepSummary::Done), ("b".into(), StepSummary::AlreadyInstalled)]));
        assert!(!clean(&[("a".into(), StepSummary::Done), ("b".into(), StepSummary::Skipped)]));
        assert!(clean(&[]));
    }

    #[test]
    fn asset_table_complete() {
        assert_eq!(ASSETS.len(), 19);
        assert!(ASSETS.iter().all(|(folder, contents)| !folder.is_empty() && !contents.is_empty()));
    }
}

/// Import Assets' finish page: where the furniture went, how to move it, and the way back.
pub fn import_assets(imported: super::import_assets::Imported, steps: Steps, shared: crate::gui::Shared) -> Box<dyn FinishPage> {
    let upload = std::sync::Arc::new(std::sync::Mutex::new(super::import_assets::FileUpload::default()));
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let heading = match (&imported.result, ok) {
            (Some(_), true) => tf("{count} furniture imported", &[("count", &imported.furniture.to_string())]),
            (None, true) => t("Nothing was imported.").into(),
            (_, false) => t("The furniture was not imported").into(),
        };
        let message = match (&imported.result, ok) {
            (Some(r), true) if r.page_id <= 0 => t("items_base follows the merged FurnitureData.json again; there was no new furniture to import.").into(),
            (Some(r), true) if r.pages > 1 => tf(
                "{items} catalogue items on {pages} pages under {caption} (id {id}). Players see them after they reload the hotel.",
                &[("items", &imported.catalogue_items.to_string()), ("pages", &r.pages.to_string()), ("caption", &imported.caption), ("id", &r.page_id.to_string())],
            ),
            (Some(r), true) => tf(
                "{items} catalogue items on the page {caption} (id {id}). Players see it after they reload the hotel.",
                &[("items", &imported.catalogue_items.to_string()), ("caption", &imported.caption), ("id", &r.page_id.to_string())],
            ),
            (None, true) => t("Every downloaded Habbo furniture was in the database already.").into(),
            (_, false) => t(RUN_AGAIN).to_string(),
        };
        widgets::result_banner(ui, palette, if ok { ResultKind::Success } else { ResultKind::Warning }, &heading, &message);
        step_list(ui, palette, &steps);
        let Some(r) = &imported.result else { return };
        if !imported.missing_files.is_empty() {
            let files = &imported.missing_files;
            let habs = files.iter().filter(|f| f.folder == "furniture").count();
            let total: u64 = files.iter().map(|f| f.size).sum();
            section(ui, t("The furniture's files on the server"), |ui| {
                widgets::text(
                    ui,
                    &tf(
                        "{habs} .hab files and {icons} icons of the imported furniture are not on the server yet ({size}); without them the client cannot show these furni.",
                        &[("habs", &habs.to_string()), ("icons", &(files.len() - habs).to_string()), ("size", &format!("{:.1} MB", total as f64 / 1_048_576.0))],
                    ),
                    13.0,
                );
                let state = upload.lock().unwrap();
                let (running, sent, result) = (state.running, state.sent, state.result.clone());
                drop(state);
                match (&result, running) {
                    (_, true) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            let done = if total > 0 { (sent as f32 / total as f32).min(1.0) } else { 0.0 };
                            ui.add(egui::ProgressBar::new(done).desired_width(320.0).show_percentage());
                        });
                    }
                    (Some(Ok(message)), false) => widgets::result_banner(ui, palette, ResultKind::Success, t("Uploaded"), message),
                    (Some(Err(message)), false) => {
                        widgets::result_banner(ui, palette, ResultKind::Warning, t("The upload stopped"), message);
                        if ui.add(widgets::primary_button(t("Try again"))).clicked() {
                            super::import_assets::upload_missing(&shared, files.clone(), upload.clone());
                        }
                    }
                    (None, false) => {
                        if ui.add(widgets::primary_button(&tf("Upload the {count} missing files", &[("count", &files.len().to_string())]))).clicked() {
                            super::import_assets::upload_missing(&shared, files.clone(), upload.clone());
                        }
                    }
                }
            });
        }
        let page = r.page_id.to_string();
        if r.page_id > 0 {
            section(ui, t("1. Put the page where you want it"), |ui| {
                widgets::text(ui, t("Under another page (the id of that page as parent_id), and visible:"), 13.0);
                copy_box(ui, palette, &format!("UPDATE `catalog_pages` SET `parent_id` = <parent id>, `visible` = '1' WHERE `id` = {page};"));
                widgets::text(ui, t("Or move its furniture to a page you have, and the empty page away:"), 13.0);
                if r.pages > 1 {
                    copy_box(
                        ui,
                        palette,
                        &format!("UPDATE `catalog_items` SET `page_id` = <page id> WHERE `page_id` IN (SELECT `id` FROM `catalog_pages` WHERE `parent_id` = {page});\nDELETE FROM `catalog_pages` WHERE `parent_id` = {page} OR `id` = {page};"),
                    );
                } else {
                    copy_box(ui, palette, &format!("UPDATE `catalog_items` SET `page_id` = <page id> WHERE `page_id` = {page};\nDELETE FROM `catalog_pages` WHERE `id` = {page};"));
                }
                widgets::text(ui, t("Run it in the SQL Editor, then restart the emulator (Tools) or reload the catalogue in the hotel."), 13.0);
            });
        }
        section(ui, t("2. As it was before"), |ui| {
            widgets::text(ui, t("The database before the import (Tools → Import full database puts it back):"), 13.0);
            code(ui, palette, &r.safety_backup);
            if let Some(sql) = &imported.local_sql {
                widgets::text(ui, t("The SQL that ran, on this computer:"), 13.0);
                code(ui, palette, &sql.display().to_string());
            }
            if let Some((merged, old)) = &imported.furnidata {
                widgets::text(ui, t("The merged FurnitureData.json, on this computer:"), 13.0);
                code(ui, palette, &merged.display().to_string());
                if let Some(old) = old {
                    widgets::text(ui, t("The FurnitureData.json that was on the server:"), 13.0);
                    code(ui, palette, old);
                }
            }
        });
    })
}

/// Fix offer IDs' finish page: what changed, and the way back.
pub fn offer_fix(updated: usize, generated: usize, safety_backup: Option<String>, furnidata: Option<(std::path::PathBuf, Option<String>)>, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        let (kind, heading, message) = if ok {
            (
                ResultKind::Success,
                t("The offer IDs are in line").to_string(),
                tf("{updates} catalogue items got another offer_id, {generated} furni a new offer id.", &[("updates", &updated.to_string()), ("generated", &generated.to_string())]),
            )
        } else {
            (ResultKind::Warning, t("The offer IDs were not fixed").to_string(), t(RUN_AGAIN).to_string())
        };
        widgets::result_banner(ui, palette, kind, &heading, &message);
        step_list(ui, palette, &steps);
        if safety_backup.is_none() && furnidata.is_none() {
            return;
        }
        section(ui, t("As it was before"), |ui| {
            if let Some(backup) = &safety_backup {
                widgets::text(ui, t("The database before the update (Tools → Import full database puts it back):"), 13.0);
                code(ui, palette, backup);
            }
            if let Some((local, old)) = &furnidata {
                if let Some(old) = old {
                    widgets::text(ui, t("The FurnitureData.json that was on the server:"), 13.0);
                    code(ui, palette, old);
                }
                widgets::text(ui, t("The new one, on this computer:"), 13.0);
                code(ui, palette, &local.display().to_string());
            }
        });
    })
}

/// Fix items_base's finish page: what changed, and the way back.
pub fn item_fix(rows: usize, changes: usize, safety_backup: Option<String>, steps: Steps) -> Box<dyn FinishPage> {
    Page::boxed(move |ui, palette| {
        let ok = clean(&steps);
        if ok {
            widgets::result_banner(
                ui,
                palette,
                ResultKind::Success,
                t("items_base is in line"),
                &tf("{rows} furni got {changes} new values. Players see it after they reload the hotel.", &[("rows", &rows.to_string()), ("changes", &changes.to_string())]),
            );
        } else {
            widgets::result_banner(ui, palette, ResultKind::Warning, t("items_base was not fixed"), t(RUN_AGAIN));
        }
        step_list(ui, palette, &steps);
        if let Some(backup) = &safety_backup {
            section(ui, t("As it was before"), |ui| {
                widgets::text(ui, t("The database before the update (Tools → Import full database puts it back):"), 13.0);
                code(ui, palette, backup);
            });
        }
    })
}
