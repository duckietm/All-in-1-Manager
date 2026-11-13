//! The dashboard (InstallerWindow.ShowDashboard): the hotel banner, Log in, Install New Server, the components card
//! with the versions on the server and the updates on GitHub, and the addon and tool tiles.

use std::sync::Arc;

use aio_core::layout::AtomCmsPresence;
use aio_core::ServerLayout;
use aio_remote::server_status::{self, RepositoryUpdate, RepositoryVersion};
use eframe::egui::{self, Color32, RichText};
use parking_lot::Mutex;

use super::addons;
use super::tools::{Component, ToolsView};
use crate::gui::icons::{self, Icon};
use crate::gui::scenes::{self, Scene};
use crate::gui::theme::{self, hex, with_alpha, Fixed, Palette};
use crate::gui::{banner, widgets, Page, Request, Shared};
use crate::i18n::{n, t, tf};

pub enum Click {
    Login,
    Page(Page),
}

/// What a full server installation puts on the server.
pub const COMPONENTS: [(&str, &str, &str, Icon, &str); 3] = [
    ("Polaris Emulator", n("Java 25 game server with a MariaDB database"),"duckietm/Polaris-Emulator", Icon::Server, "#4F5FE6"),
    ("Octane", "React browser client (navigator, catalog, chat ...)", "duckietm/Octane", Icon::Cube, "#8B5CF6"),
    ("Octane Renderer", "Draws rooms, avatars and furniture for Octane", "duckietm/Octane-Renderer", Icon::Star, "#0E9F9E"),
];

#[derive(Default)]
struct Versions {
    /// The login the versions belong to.
    for_session: Option<usize>,
    loading: bool,
    list: Option<Vec<RepositoryVersion>>,
}

pub struct Dashboard {
    versions: Arc<Mutex<Versions>>,
}

fn id(server: &Arc<dyn aio_remote::Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl Dashboard {
    pub fn new(_shared: &Shared) -> Self {
        Self { versions: Arc::new(Mutex::new(Versions::default())) }
    }

    pub fn session_changed(&mut self, _shared: &Shared) {
        *self.versions.lock() = Versions::default();
    }

    /// Loads the installed versions (no network) once per login and after every update check.
    pub fn reload_versions(&self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        {
            let mut versions = self.versions.lock();
            if versions.loading {
                return;
            }
            versions.loading = true;
            versions.for_session = Some(id(&server));
        }
        let (versions, status) = (self.versions.clone(), shared.clone());
        shared.spawn(async move {
            let result = server_status::versions(server.as_ref()).await;
            let mut v = versions.lock();
            v.loading = false;
            match result {
                Ok(list) => v.list = Some(list),
                Err(error) => {
                    v.list = None;
                    status.set_status(&tf("Reading the installed versions failed: {error}", &[("error", &error.to_string())]));
                }
            }
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared, tools: &ToolsView) -> Option<Click> {
        let mut click = None;
        let session = shared.session();
        if let Some(server) = &session {
            let stale = self.versions.lock().for_session != Some(id(server));
            if stale {
                self.reload_versions(shared);
            }
        }
        if theme::hotel() {
            banner::hotel_banner(ui, palette, t("Your hotel, ready to go"), t("Set up Polaris, Octane, MariaDB, NGINX and Traefik on your own Ubuntu or Debian server, then run it from here."));
            ui.add_space(18.0);
        }
        if session.is_none()
            && wide_tile(
                ui,
                palette,
                t("Log in"),
                t("Log in to your server: with a password or your SSH key. The login is used for everything below and stays open until you close the program."),
                Icon::Account,
                hex("#16A34A"),
                Scene::Sky,
            )
        {
            click = Some(Click::Login);
        }
        if wide_tile(
            ui,
            palette,
            t("Install New Server"),
            t("Install the full hotel stack on the server you are logged in to: a fresh Ubuntu Server / Debian machine."),
            Icon::ServerPlus,
            Fixed::brand(),
            Scene::Hotel,
        ) {
            click = Some(Click::Page(Page::Install));
        }
        self.components_card(ui, palette, shared, tools);
        ui.add_space(18.0);

        // Only what the Plugins page has switched on; a section with nothing left is left out.
        let mut addon_entries = addons::visible(addons::entries());
        addon_entries.extend(addons::external_entries());
        for (heading, entries) in [(t("ADDONS"), addon_entries), (t("ASSETS"), addons::visible(addons::asset_entries()))] {
            if entries.is_empty() {
                continue;
            }
            section(ui, palette, heading);
            if let Some(page) = entry_grid(ui, palette, &entries) {
                click = Some(Click::Page(page));
            }
            ui.add_space(6.0);
        }
        section(ui, palette, t("TOOLS"));
        let tool_tiles = vec![
            addons::Entry {
                title: t("Tools").into(),
                description: t("Restarts, database backup and import, updates.").into(),
                icon: Icon::Wrench,
                color: hex("#0E9F9E"),
                scene: Scene::Parquet,
                page: Page::Tools,
            },
            addons::Entry {
                title: "Debug".into(),
                description: t("Live logs: the emulator, NGINX, Traefik, the firewall, Fail2Ban and CrowdSec.").into(),
                icon: Icon::Bug,
                color: hex("#DC2626"),
                scene: Scene::Hearts,
                page: Page::Debug,
            },
        ];
        if let Some(page) = entry_grid(ui, palette, &tool_tiles) {
            click = Some(Click::Page(page));
        }
        click
    }

    // The rows of the card: the full installation, plus AtomCMS once it is installed. Octane and its renderer depend
    // on each other: one row, updated together (the renderer first). Traefik, Gitea and CrowdSec follow from their images.
    fn components_card(&self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared, tools: &ToolsView) {
        let session = shared.session();
        let versions = self.versions.lock().list.clone();
        let updates = session.as_ref().and_then(|_| tools.last_updates());
        let images = session.as_ref().and_then(|_| tools.last_images()).unwrap_or_default();
        let rows: [(String, String, String, Icon, &str, String, Option<String>, Component); 3] = [
            (
                COMPONENTS[0].0.into(),
                t(COMPONENTS[0].1).into(),
                COMPONENTS[0].2.into(),
                COMPONENTS[0].3,
                COMPONENTS[0].4,
                aio_remote::emulator::repository(),
                None,
                Component::Emulator,
            ),
            (
                t("Octane and Octane Renderer").into(),
                t("Browser client and its renderer, updated together (renderer first)").into(),
                format!("{} + Octane-Renderer", COMPONENTS[1].2),
                COMPONENTS[1].3,
                COMPONENTS[1].4,
                aio_remote::octane::octane(),
                Some(aio_remote::octane::renderer()),
                Component::Octane,
            ),
            (
                "AtomCMS".into(),
                t("The hotel's website (addon), Octane on /client").into(),
                "ObjectRetros/atomcms".into(),
                Icon::Puzzle,
                "#E8590C",
                aio_remote::atom_cms::directory(),
                None,
                Component::AtomCms,
            ),
        ];
        widgets::card(ui, palette, egui::Margin::same(12), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(widgets::title_text(t("Full server installation"), 17.0));
                    let host = session.as_ref().map(|s| s.login().host.clone()).unwrap_or_default();
                    ui.label(
                        RichText::new(match (&session, &updates) {
                            (None, _) => t("Components the installer sets up. Log in to see the installed versions and updates.").to_string(),
                            (Some(_), None) => tf("The versions on {host}; checking GitHub for updates ...", &[("host", &host)]),
                            (Some(_), Some(_)) => tf("The versions on {host}, compared with GitHub (the branches chosen in Settings).", &[("host", &host)]),
                        })
                        .size(12.0)
                        .color(palette.muted),
                    );
                });
                if session.is_some() && updates.is_some() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        if ui.add(widgets::secondary_button(t("Check again"))).clicked() {
                            // The Tools page owns the check; the card follows its result.
                            shared.request(Request::CheckUpdates);
                        }
                    });
                }
            });
            ui.add_space(6.0);
            for (name, description, repository, icon, color, directory, directory2, component) in rows {
                let version = versions.as_ref().and_then(|v| v.iter().find(|v| v.directory == directory)).cloned();
                // AtomCMS is an addon: listed once it is installed, or always / never when Settings say so.
                let presence = ServerLayout::current().atom_cms_installed;
                if component == Component::AtomCms
                    && (presence == AtomCmsPresence::NotInstalled || (presence == AtomCmsPresence::Detect && version.is_none()))
                {
                    continue;
                }
                let mut update = updates.as_ref().and_then(|u| u.iter().find(|u| u.directory == directory)).cloned();
                // A combined row (Octane): the second clone's version and update count too.
                let version2 = directory2.as_ref().and_then(|d| versions.as_ref().and_then(|v| v.iter().find(|v| &v.directory == d)).cloned());
                let update2 = directory2.as_ref().and_then(|d| updates.as_ref().and_then(|u| u.iter().find(|u| &u.directory == d)).cloned());
                if let (Some(first), Some(second)) = (&update, &update2) {
                    update = Some(combine(first, second));
                }
                let color = hex(color);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    small_badge(ui, icon, color, 34.0, 17.0);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&name).size(13.5).strong());
                        ui.label(RichText::new(format!("{description}  ·  github.com/{repository}")).size(12.0).color(palette.muted));
                        for (v, label) in [(version2.as_ref(), Some("Renderer")), (version.as_ref(), directory2.as_ref().map(|_| "Octane  "))] {
                            let Some(v) = v else { continue };
                            let parts: Vec<String> = [
                                label.map(str::to_string),
                                v.build.clone(),
                                v.branch.as_ref().map(|b| format!("{b} @ {}", v.commit.clone().unwrap_or_default())),
                                v.date.clone(),
                            ]
                            .into_iter()
                            .flatten()
                            .collect();
                            ui.label(RichText::new(parts.join("  ·  ")).size(11.5).monospace().color(palette.subtle));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        state_chips(ui, palette, shared, session.is_some(), versions.is_some(), version.is_some(), &directory, component, updates.is_some(), update.as_ref());
                    });
                });
                ui.add_space(8.0);
            }
            // The Docker services: Traefik, and Gitea and CrowdSec once they are installed.
            for image in images {
                let traefik = image.name == "Traefik";
                let crowdsec = image.name == "CrowdSec";
                let color = hex(if traefik { "#2563EB" } else if crowdsec { "#7C3AED" } else { "#609926" });
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    small_badge(ui, if traefik { Icon::Swap } else if crowdsec { Icon::Shield } else { Icon::Git }, color, 34.0, 17.0);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&image.name).size(13.5).strong());
                        let what = if traefik {
                            tf("The web entrance on 80/443 with the certificates  ·  Docker image {image}", &[("image", &image.image)])
                        } else if crowdsec {
                            tf("Blocks attackers in Traefik (addon)  ·  Docker image {image}", &[("image", &image.image)])
                        } else {
                            tf("Your Git server (addon)  ·  Docker image {image}", &[("image", &image.image)])
                        };
                        ui.label(RichText::new(what).size(12.0).color(palette.muted));
                        let state = [image.version.clone(), Some(if image.running { t("running") } else { t("stopped") }.to_string())];
                        ui.label(RichText::new(state.into_iter().flatten().collect::<Vec<_>>().join("  ·  ")).size(11.5).monospace().color(palette.subtle));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        let component = if traefik { Component::Traefik } else if crowdsec { Component::CrowdSec } else { Component::Gitea };
                        if traefik && image.not_restarted && !image.newer_on_hub {
                            // The newer image is already on the server: a restart is all it takes, Docker Hub is not needed.
                            if ui
                                .add(widgets::primary_button(t("Restart")))
                                .on_hover_text(tf(
                                    "Recreates the Traefik container with the newer {image} that is already downloaded: the players are disconnected for a few seconds. Docker Hub is not needed.",
                                    &[("image", &image.image)],
                                ))
                                .clicked()
                            {
                                shared.request(Request::Restart(component));
                            }
                            chip(ui, t("NEW IMAGE WAITING"), palette.warning_back, palette.warning_text).on_hover_text(tf(
                                "A newer {image} is downloaded, but the container still runs the old one. Restart starts the new one.",
                                &[("image", &image.image)],
                            ));
                        } else if image.has_update() {
                            if ui.add(widgets::primary_button(t("Update"))).on_hover_text(if traefik {
                                t("Pulls the newest image and restarts Traefik: the players are disconnected for a few seconds.")
                            } else if crowdsec {
                                t("Pulls the newest image and detection rules and restarts CrowdSec; bans and enrollment stay, the hotel stays reachable.")
                            } else {
                                t("Pulls the newest image and restarts Gitea; the repositories stay.")
                            }).clicked() {
                                shared.request(Request::Update(component));
                            }
                            let label = match &image.newest_version {
                                Some(v) => tf("{version} AVAILABLE", &[("version", v.trim_start_matches('v'))]),
                                None if image.newer_on_hub => t("UPDATE AVAILABLE").into(),
                                None => t("NEW IMAGE WAITING").into(),
                            };
                            let tip = if image.newer_on_hub {
                                tf("Docker Hub has a newer {image}.", &[("image", &image.image)])
                            } else {
                                tf("A newer {image} is downloaded, but the container still runs the old one. Update starts the new one; a docker restart keeps the old image (it restarts the same container).", &[("image", &image.image)])
                            };
                            chip(ui, &label, palette.warning_back, palette.warning_text).on_hover_text(tip);
                        } else if let Some(problem) = &image.problem {
                            chip(ui, t("NOT CHECKED"), palette.hover, palette.muted).on_hover_text(problem);
                        } else {
                            chip(ui, t("UP TO DATE"), palette.success_back, palette.success_text);
                        }
                    });
                });
                ui.add_space(8.0);
            }
        });
    }
}

/// Octane and its renderer as one row: both counts, and the branch switch of either.
fn combine(octane: &RepositoryUpdate, renderer: &RepositoryUpdate) -> RepositoryUpdate {
    let mut combined = octane.clone();
    combined.behind = octane.behind + renderer.behind;
    if !octane.switches_branch() && renderer.switches_branch() {
        combined.target = renderer.target.clone();
        combined.branch = renderer.branch.clone();
    }
    let newest: Vec<String> = [
        renderer.has_update().then(|| format!("Renderer: {}", renderer.newest.clone().unwrap_or_default())),
        octane.has_update().then(|| format!("Octane: {}", octane.newest.clone().unwrap_or_default())),
    ]
    .into_iter()
    .flatten()
    .collect();
    combined.newest = if newest.is_empty() { None } else { Some(newest.join("; ")) };
    combined
}

#[allow(clippy::too_many_arguments)]
fn state_chips(
    ui: &mut egui::Ui,
    palette: &Palette,
    shared: &Shared,
    logged_in: bool,
    versions_read: bool,
    installed: bool,
    directory: &str,
    component: Component,
    checked: bool,
    update: Option<&RepositoryUpdate>,
) {
    if !logged_in {
        chip(ui, t("INCLUDED"), palette.success_back, palette.success_text);
    } else if !versions_read {
        chip(ui, t("READING ..."), palette.info_back, palette.info_text);
    } else if !installed && component == Component::AtomCms {
        chip(ui, t("NOT FOUND"), palette.warning_back, palette.warning_text)
            .on_hover_text(tf("Settings say AtomCMS is installed, but {directory} is not a git clone.", &[("directory", directory)]));
    } else if !installed {
        chip(ui, t("NOT INSTALLED"), palette.hover, palette.muted);
    } else if !checked {
        chip(ui, t("CHECKING GITHUB ..."), palette.info_back, palette.info_text);
    } else if let Some(update) = update.filter(|u| u.has_update() || u.switches_branch()) {
        let button = ui.add(widgets::primary_button(t("Update")));
        let button = if component == Component::Octane { button.on_hover_text(t("Updates Octane-Renderer first, then Octane, and builds the client.")) } else { button };
        if button.clicked() {
            shared.request(Request::Update(component));
        }
        let label = if update.has_update() {
            if update.behind == 1 { t("1 NEW COMMIT").to_string() } else { tf("{n} NEW COMMITS", &[("n", &update.behind.to_string())]) }
        } else {
            tf("TO {branch}", &[("branch", update.target.as_deref().unwrap_or(""))]).to_uppercase()
        };
        let mut tip = String::new();
        if update.switches_branch() {
            tip.push_str(&tf("Updating switches {from} to {to}. ", &[("from", &update.branch.clone().unwrap_or_default()), ("to", update.target.as_deref().unwrap_or(""))]));
        }
        if let Some(newest) = &update.newest {
            tip.push_str(&tf("Newest on GitHub: {commit}", &[("commit", newest)]));
        }
        chip(ui, &label, palette.warning_back, palette.warning_text).on_hover_text(tip);
    } else if update.is_some() {
        chip(ui, t("UP TO DATE"), palette.success_back, palette.success_text);
    } else {
        chip(ui, t("NOT CHECKED"), palette.hover, palette.muted);
    }
}

/// A small label such as UP TO DATE. Hotel look: the pixel font with an edge in its own colour.
pub fn chip(ui: &mut egui::Ui, text: &str, back: Color32, fore: Color32) -> egui::Response {
    let hotel = theme::hotel();
    let mut frame = egui::Frame::new().fill(back).corner_radius(if hotel { 4 } else { 6 }).inner_margin(egui::Margin::symmetric(7, 2));
    if hotel {
        frame = frame.stroke(egui::Stroke::new(1.0_f32, fore));
    }
    let text = RichText::new(text).size(if hotel { 11.0 } else { 10.0 }).strong().color(fore).extra_letter_spacing(0.4);
    let text = if hotel { text.family(theme::title_family()) } else { text };
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::FontSelection::Default);
    let margin = frame.inner_margin;
    let size = galley.size() + egui::vec2((margin.left + margin.right) as f32, (margin.top + margin.bottom) as f32);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().add(frame.paint(rect));
    ui.painter().galley(rect.min + egui::vec2(margin.left as f32, margin.top as f32), galley, fore);
    response
}

fn small_badge(ui: &mut egui::Ui, icon: Icon, color: Color32, size: f32, icon_size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(rect, size / 4.0, with_alpha(color, 0x26));
    icons::image(icon, icon_size, color).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(icon_size, icon_size)));
}

fn section(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(11.0).strong().color(palette.muted));
    ui.add_space(6.0);
}

/// A clickable card: outlined like a hotel window, yellow edge when the mouse is on it.
fn tile_frame(ui: &mut egui::Ui, palette: &Palette, margin: egui::Margin, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    // The frame's margin and edge go around the content: the content gets what is left of the width.
    let content = ui.available_width() - (margin.left + margin.right) as f32 - 2.0 * theme::card_edge();
    let response = widgets::card_frame(palette).inner_margin(margin).show(ui, |ui| {
        ui.set_width(content);
        ui.vertical(add);
    });
    let response = response.response.interact(egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        let edge = if theme::hotel() { Fixed::hotel_yellow() } else { Fixed::brand() };
        ui.painter().rect_stroke(response.rect, theme::card_corners() as f32, egui::Stroke::new(theme::card_edge(), edge), egui::StrokeKind::Inside);
    }
    response
}

/// A wide button: in the Hotel look the picture button, otherwise the plain card.
fn wide_tile(ui: &mut egui::Ui, palette: &Palette, title: &str, description: &str, icon: Icon, color: Color32, scene: Scene) -> bool {
    if !theme::hotel() {
        return tile(ui, palette, title, description, icon, color);
    }
    let clicked = scenes::button(ui, palette, egui::vec2(ui.available_width(), 92.0), scene, color, icon, title, description, scenes::Layout::Side, palette.surface, true)
        .clicked();
    ui.add_space(14.0);
    clicked
}

/// Buttons in columns: in the Hotel look the picture buttons, otherwise the plain cards.
fn entry_grid(ui: &mut egui::Ui, palette: &Palette, entries: &[addons::Entry]) -> Option<Page> {
    if theme::hotel() {
        return addons::grid(ui, palette, entries, 132.0, palette.surface);
    }
    let tiles: Vec<(&str, String, Icon, Color32, Page)> = entries.iter().map(|e| (e.title.as_str(), e.description.clone(), e.icon, e.color, e.page)).collect();
    tile_grid(ui, palette, &tiles)
}

fn tile(ui: &mut egui::Ui, palette: &Palette, title: &str, description: &str, icon: Icon, color: Color32) -> bool {
    let clicked = tile_frame(ui, palette, egui::Margin::symmetric(22, 20), |ui| {
        ui.horizontal(|ui| {
            small_badge(ui, icon, color, 52.0, 26.0);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width() - 40.0);
                ui.label(widgets::title_text(title, 19.0));
                widgets::muted(ui, palette, description, 12.5);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                icons::show(ui, Icon::ChevronRight, 22.0, palette.subtle);
            });
        });
    })
    .clicked();
    ui.add_space(12.0);
    clicked
}

/// Tiles in columns: the column count of the width (1, 2 or 4; never 3).
fn tile_grid(ui: &mut egui::Ui, palette: &Palette, tiles: &[(&str, String, Icon, Color32, Page)]) -> Option<Page> {
    let width = ui.available_width();
    let mut columns = ((width / 250.0) as usize).clamp(1, 4);
    if columns == 3 {
        columns = 2;
    }
    let gap = 12.0;
    let tile_width = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    let mut clicked = None;
    for row in tiles.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (title, description, icon, color, page) in row {
                ui.allocate_ui(egui::vec2(tile_width, 0.0), |ui| {
                    ui.set_width(tile_width);
                    let response = tile_frame(ui, palette, egui::Margin::symmetric(16, 14), |ui| {
                        ui.set_min_height(86.0);
                        ui.horizontal(|ui| {
                            small_badge(ui, *icon, *color, 40.0, 20.0);
                            ui.add_space(6.0);
                            ui.label(widgets::title_text(title, 16.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                icons::show(ui, Icon::ChevronRight, 18.0, palette.subtle);
                            });
                        });
                        ui.add_space(6.0);
                        widgets::muted(ui, palette, description, 12.0);
                    });
                    if response.clicked() {
                        clicked = Some(*page);
                    }
                });
            }
        });
        ui.add_space(gap);
    }
    clicked
}
