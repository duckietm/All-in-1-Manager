//! The Addons page: every addon as a filled button. The dashboard shows the same buttons.

use eframe::egui::{self, Color32};

use crate::gui::icons::Icon;
use crate::gui::scenes::{self, Scene};
use crate::gui::theme::{hex, Palette};
use crate::gui::Page;
use crate::i18n::t;

/// One button: title, description, icon, colour, fill colour (scene) and the page it opens.
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
    vec![
        entry("CMS", t("AtomCMS or your own CMS as the hotel's website; Octane moves to /client.").into(), Icon::Puzzle, "#E8590C", Scene::Room, Page::AtomCms),
        entry("Gitea", t("Your own Git server (like a small GitHub) on git.yourdomain, in Docker behind Traefik.").into(), Icon::Git, "#609926", Scene::Garden, Page::Gitea),
        entry("Fail2Ban", t("Bans addresses that guess SSH passwords; a whitelist for your own IPs (Tools → Fail2Ban).").into(), Icon::Shield, "#DC2626", Scene::Bricks, Page::Fail2Ban),
        entry("CrowdSec", t("Blocks attackers on every route through Traefik, with the community's blocklist. Cloudflare aware.").into(), Icon::Shield, "#7C3AED", Scene::Space, Page::CrowdSec),
        entry(t("Default catalogue"), t("A complete catalogue for a new hotel: pages, items, furniture and FurnitureData.json.").into(), Icon::Shopping, "#D97706", Scene::Disco, Page::DefaultCatalog),
        entry("SQL Editor", t("Like HeidiSQL: browse and edit tables, run queries, export to CSV.").into(), Icon::Database, "#0891B2", Scene::Snow, Page::SqlEditor),
    ]
}

/// The pages of the sidebar's ASSETS section, for the dashboard.
pub fn asset_entries() -> Vec<Entry> {
    let gamedata = aio_remote::octane::gamedata();
    vec![
        entry(
            t("Habbo assets"),
            t("Habbo's furniture, clothes, effects, badges, sounds and game data, downloaded here as .hab and JSON.").into(),
            Icon::Download,
            "#0EA5E9",
            Scene::Sky,
            Page::GetAssets,
        ),
        entry(
            t("Nitro retro"),
            t("A Nitro hotel's custom furniture and clothes, downloaded on this computer as .hab, to upload by hand.").into(),
            Icon::Cube,
            "#DB2777",
            Scene::Hearts,
            Page::RetroAssets,
        ),
        entry(
            t("Asset tools"),
            t("Decompile SWF and HAB files, compile edited HAB files, and convert SWF and NITRO to HAB, on this computer.").into(),
            Icon::Swap,
            "#059669",
            Scene::Garden,
            Page::AssetTools,
        ),
        entry(
            t("Deploy Assets"),
            crate::i18n::tf("The downloaded assets and the UI texts to {gamedata}.", &[("gamedata", &gamedata)]),
            Icon::ArrowUp,
            "#8B5CF6",
            Scene::Beach,
            Page::DeployAssets,
        ),
        entry(
            t("Import Assets"),
            t("The Habbo furniture the database lacks into items_base and the catalogue, with an overview first.").into(),
            Icon::Database,
            "#D97706",
            Scene::Parquet,
            Page::ImportAssets,
        ),
        entry(
            t("Import custom furniture"),
            t("A Nitro retro profile's custom furniture merged into Habbo's FurnitureData.json, and into items_base and the catalogue.").into(),
            Icon::Cube,
            "#BE185D",
            Scene::Hotel,
            Page::ImportCustom,
        ),
        entry(
            t("Fix offer IDs"),
            t("catalog_items.offer_id and FurnitureData.json in line, so every furni can be bought.").into(),
            Icon::Shopping,
            "#0D9488",
            Scene::Lido,
            Page::OfferFix,
        ),
        entry(
            t("Fix items_base"),
            t("sprite_id, size, sit / lay / walk and interaction modes of the furni in items_base, as FurnitureData.json and the .hab files give them.").into(),
            Icon::Wrench,
            "#4F46E5",
            Scene::Room,
            Page::ItemFix,
        ),
        entry(
            "Asset Explorer",
            crate::i18n::tf("Two panes: this computer and {gamedata}. Upload, download, rename.", &[("gamedata", &gamedata)]),
            Icon::FolderSync,
            "#2563EB",
            Scene::Lido,
            Page::AssetExplorer,
        ),
    ]
}

/// The colour of an external plugin's tile when its manifest gives none that is `#rrggbb`.
const EXTERNAL_COLOR: &str = "#4F5FE6";

/// A manifest tile colour: exactly `#rrggbb`, else the default.
pub fn tile_color(color: &str) -> Color32 {
    let valid = color.len() == 7 && color.starts_with('#') && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    hex(if valid { color } else { EXTERNAL_COLOR })
}

/// The tiles of the external plugins that are on and have one (manifest `tile`). Their texts are plugin-supplied.
pub fn external_entries() -> Vec<Entry> {
    crate::plugins::external_pages()
        .into_iter()
        .filter_map(|(page, info)| {
            let tile = info.manifest.tile?;
            Some(Entry {
                title: aio_plugin_host::display_safe(&tile.title, 40),
                description: aio_plugin_host::display_safe(&tile.description, 160),
                icon: Icon::Puzzle,
                color: tile_color(&tile.color),
                scene: Scene::Room,
                page,
            })
        })
        .collect()
}

/// Only the buttons of plugins that are switched on (the Plugins page).
pub fn visible(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.retain(|e| crate::plugins::page_visible(e.page));
    entries
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
    let mut entries = visible(entries());
    entries.extend(external_entries());
    if entries.is_empty() {
        crate::gui::widgets::muted(ui, palette, t("Every addon is switched off: switch them on again on the Plugins page."), 13.5);
        return None;
    }
    grid(ui, palette, &entries, 140.0, palette.surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_colors_must_be_rrggbb() {
        assert_eq!(tile_color("#4f8cff"), hex("#4f8cff"));
        for bad in ["4f8cff", "#4f8cf", "#4f8cffaa", "#zzzzzz", "red", "", "#ｆｆｆｆｆｆ"] {
            assert_eq!(tile_color(bad), hex(EXTERNAL_COLOR), "{bad}");
        }
    }
}