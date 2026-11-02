//! The Addons page: every addon as a picture button. The dashboard shows the same buttons.

use eframe::egui::{self, Color32};

use crate::gui::icons::Icon;
use crate::gui::scenes::{self, Scene};
use crate::gui::theme::{hex, Palette};
use crate::gui::Page;
use crate::i18n::{t, tf};

/// One button: title, description, icon, colour, scene and the page it opens.
pub struct Entry {
    pub title: String,
    pub description: String,
    pub icon: Icon,
    pub color: Color32,
    pub scene: Scene,
    pub page: Page,
}

fn entry(title: &str, description: String, icon: Icon, color: &str, scene: Scene, page: Page) -> Entry {
    Entry { title: title.into(), description, icon, color: hex(color), scene, page }
}

/// Every addon, in the order of the page.
pub fn entries() -> Vec<Entry> {
    let gamedata = aio_remote::octane::gamedata();
    vec![
        entry("CMS", t("AtomCMS or your own CMS as the hotel's website; Octane moves to /client.").into(), Icon::Puzzle, "#E8590C", Scene::Room, Page::AtomCms),
        entry("Gitea", t("Your own Git server (like a small GitHub) on git.yourdomain, in Docker behind Traefik.").into(), Icon::Git, "#609926", Scene::Garden, Page::Gitea),
        entry("Fail2Ban", t("Bans addresses that guess SSH passwords; a whitelist for your own IPs (Tools → Fail2Ban).").into(), Icon::Shield, "#DC2626", Scene::Bricks, Page::Fail2Ban),
        entry("CrowdSec", t("Blocks attackers on every route through Traefik, with the community's blocklist. Cloudflare aware.").into(), Icon::Shield, "#7C3AED", Scene::Space, Page::CrowdSec),
        entry(t("Default catalogue"), t("A complete catalogue for a new hotel: pages, items, furniture and FurnitureData.json.").into(), Icon::Shopping, "#D97706", Scene::Disco, Page::DefaultCatalog),
        entry(t("Deploy Assets"), tf("The All-in-1 downloader's assets and the UI texts to {gamedata}.", &[("gamedata", &gamedata)]), Icon::Download, "#8B5CF6", Scene::Beach, Page::DeployAssets),
        entry("Asset Explorer", tf("Two panes: this computer and {gamedata}. Upload, download, rename.", &[("gamedata", &gamedata)]), Icon::FolderSync, "#2563EB", Scene::Lido, Page::AssetExplorer),
        entry("SQL Editor", t("Like HeidiSQL: browse and edit tables, run queries, export to CSV.").into(), Icon::Database, "#0891B2", Scene::Snow, Page::SqlEditor),
    ]
}

/// Picture buttons in columns (1, 2 or 4; never 3). `outside` is the colour around them. Returns the page clicked.
pub fn grid(ui: &mut egui::Ui, palette: &Palette, entries: &[Entry], height: f32, outside: Color32) -> Option<Page> {
    let width = ui.available_width();
    let mut columns = ((width / 250.0) as usize).clamp(1, 4);
    if columns == 3 {
        columns = 2;
    }
    let gap = 14.0;
    let tile_width = ((width - gap * (columns as f32 - 1.0)) / columns as f32).floor();
    let mut clicked = None;
    for row in entries.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for e in row {
                let response = scenes::button(
                    ui,
                    palette,
                    egui::vec2(tile_width, height),
                    e.scene,
                    e.color,
                    e.icon,
                    &e.title,
                    &e.description,
                    scenes::Layout::Card,
                    outside,
                    true,
                );
                if response.clicked() {
                    clicked = Some(e.page);
                }
            }
        });
        ui.add_space(gap);
    }
    clicked
}

/// The Addons page.
pub fn show(ui: &mut egui::Ui, palette: &Palette) -> Option<Page> {
    ui.add_space(4.0);
    grid(ui, palette, &entries(), 140.0, palette.surface)
}
