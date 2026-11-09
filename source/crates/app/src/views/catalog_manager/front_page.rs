//! The Front page tab: the catalogue front page as the game draws it (Octane's frontpage4 layout, AIR's
//! `layout_frontpage_featured_xml`): the first slot as the tall picture on the left, the others in a list beside it,
//! and the voucher box under the list. Click a tile to edit it, drag one onto another to swap their places, and
//! **Choose ...** picks its image from the catalogue images of this computer and the server.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use aio_remote::catalog_admin::{self, Featured, Page, FEATURED_TYPES};
use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke};

use super::{small, Action, FeaturedRow, State};
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{t, tf};

/// The front page block of the game, in its pixels: 552 × 460, the first picture 184 wide, the list 360 wide and
/// 392 high with pictures of 356 × 126, the voucher box 345 × 61 under it.
const FRONT: egui::Vec2 = egui::vec2(552.0, 460.0);
const FIRST: egui::Vec2 = egui::vec2(184.0, 460.0);
const LIST_X: f32 = 192.0;
const LIST: egui::Vec2 = egui::vec2(360.0, 392.0);
const ITEM: egui::Vec2 = egui::vec2(356.0, 126.0);
const ITEM_GAP: f32 = 4.0;
const VOUCHER: egui::Rect = egui::Rect { min: egui::pos2(200.0, 399.0), max: egui::pos2(545.0, 460.0) };
/// The game window around it: the title bar and the page header.
const TITLE_BAR: f32 = 26.0;
const HEADER: f32 = 70.0;
const PAD: f32 = 10.0;

mod colors {
    use eframe::egui::Color32;
    pub const TITLE: Color32 = Color32::from_rgb(30, 114, 149);
    pub const HEADER: Color32 = Color32::from_rgb(28, 74, 96);
    pub const FRAME: Color32 = Color32::from_rgb(40, 40, 40);
    pub const BODY: Color32 = Color32::from_rgb(234, 234, 234);
    pub const TILE: Color32 = Color32::from_rgb(14, 63, 82);
    pub const VOUCHER: Color32 = Color32::from_rgb(81, 187, 238);
    pub const SELECTED: Color32 = Color32::from_rgb(255, 140, 0);
    pub const ENDED: Color32 = Color32::from_rgb(200, 40, 40);
}

/// What the tab keeps besides the slots: the tile being edited, the image picker, the server's images and the front
/// page's own row (its caption, icon, headline and voucher text).
#[derive(Default)]
pub(super) struct FrontState {
    pub selected: Option<usize>,
    /// The picker's search (Some while it is open) and whether it shows only promo images.
    pub picker: Option<String>,
    pub all_images: bool,
    /// The image files under the server's c_images (None until read), and whether that is being read.
    pub server_files: Option<Vec<String>>,
    pub listing: bool,
    /// Files fetched from the server for their preview (None: could not be), and those on their way.
    pub fetched: HashMap<String, Option<Arc<Vec<u8>>>>,
    pub fetching: HashSet<String>,
    pub header: Option<FrontHeader>,
    /// The screenshot's sample: nothing to fetch.
    pub sample: bool,
}

/// The catalogue page with a front page layout, as the header above the tiles shows it.
#[derive(Debug, Clone, Default)]
pub(super) struct FrontHeader {
    pub caption: String,
    pub icon: i64,
    /// page_headline: the client shows `c_images/catalogue/<headline>.gif`.
    pub headline: String,
    /// page_text1: the voucher box's label.
    pub text1: String,
}

/// The front page of the catalogue: a visible root page with a front page layout, else any page with one.
pub(super) fn front_page_of(pages: &[Page]) -> Option<&Page> {
    let front = |p: &&Page| p.layout.starts_with("frontpage");
    pages.iter().filter(front).find(|p| p.parent_id <= 0 && p.visible).or_else(|| pages.iter().find(front))
}

/// A featured image path as the client asks for it (`image.library.url` + path): under c_images, no way out of it.
pub(super) fn clean_path(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty()) {
        return None;
    }
    Some(path.to_string())
}

/// Text without its HTML tags, as a one-line label.
fn plain(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Promo images: the ones a front page tile usually shows.
fn is_promo(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("feature") || lower.starts_with("web_promo") || lower.contains("promo")
}

/// The images of this computer's c_images folders (Habbo assets and the retro profiles), read when first shown.
#[derive(Default)]
pub(super) struct FrontImages {
    roots: Vec<PathBuf>,
    loaded: HashMap<String, Option<egui::TextureHandle>>,
    local: Option<Vec<String>>,
    read_this_frame: usize,
}

impl FrontImages {
    pub fn new() -> Self {
        let settings = aio_core::InstallerSettings::load();
        let assets = settings.assets_folder();
        let mut gamedata = vec![aio_remote::habbo_assets::gamedata_folder(&assets)];
        gamedata.extend(settings.retro_profiles().iter().map(|p| aio_remote::habbo_assets::gamedata_folder(&aio_remote::habbo_assets::retro::custom_folder(&assets, p))));
        FrontImages { roots: gamedata.iter().map(|g| g.join("c_images")).collect(), ..FrontImages::default() }
    }

    pub fn new_frame(&mut self) {
        self.read_this_frame = 0;
    }

    fn texture_from(ctx: &egui::Context, key: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
        let image = image::load_from_memory(bytes).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw());
        // Pixel art: no smoothing when it is drawn larger.
        Some(ctx.load_texture(key, color, egui::TextureOptions::NEAREST))
    }

    /// An image under c_images: from this computer, else what was fetched from the server. The second value is true
    /// when it still has to be fetched.
    fn image(&mut self, ctx: &egui::Context, path: &str, fetched: &HashMap<String, Option<Arc<Vec<u8>>>>) -> (Option<egui::TextureHandle>, bool) {
        let Some(path) = clean_path(path) else { return (None, false) };
        let local: Vec<PathBuf> = self.roots.iter().map(|r| r.join(&path)).filter(|p| p.is_file()).collect();
        let key = if local.is_empty() { format!("server:{path}") } else { format!("local:{path}") };
        if let Some(found) = self.loaded.get(&key) {
            return (found.clone(), false);
        }
        if local.is_empty() {
            match fetched.get(&path) {
                None => return (None, true),
                Some(None) => {
                    self.loaded.insert(key, None);
                    return (None, false);
                }
                Some(Some(_)) => {}
            }
        }
        if self.read_this_frame >= 12 {
            ctx.request_repaint();
            return (None, false);
        }
        self.read_this_frame += 1;
        let texture = match local.first() {
            Some(file) => std::fs::read(file).ok().and_then(|bytes| Self::texture_from(ctx, &key, &bytes)),
            None => fetched.get(&path).cloned().flatten().and_then(|bytes| Self::texture_from(ctx, &key, &bytes)),
        };
        self.loaded.insert(key, texture.clone());
        (texture, false)
    }

    /// Forgets what was read, so a reload shows changed files.
    pub fn clear(&mut self) {
        self.loaded.clear();
        self.local = None;
    }

    /// The image files of this computer's c_images/catalogue (two levels deep) and web_promo folders.
    fn local_files(&mut self) -> Vec<String> {
        if self.local.is_none() {
            let mut files = Vec::new();
            for root in &self.roots {
                for (folder, depth) in [("catalogue", 2), ("web_promo_small", 1), ("web_promo", 1)] {
                    collect(root, folder, depth, &mut files);
                }
            }
            files.sort();
            files.dedup();
            self.local = Some(files);
        }
        self.local.clone().unwrap_or_default()
    }
}

fn collect(root: &std::path::Path, relative: &str, depth: usize, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root.join(relative)) else { return };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let path = format!("{relative}/{name}");
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() && depth > 1 {
            collect(root, &path, depth - 1, out);
        } else if kind.is_file() && (name.to_lowercase().ends_with(".png") || name.to_lowercase().ends_with(".gif")) {
            out.push(path);
        }
    }
}

/// The shell command that lists the server's promo and catalogue images, relative to c_images.
pub(super) fn list_command() -> String {
    let folder = format!("{}/c_images", aio_remote::octane::gamedata());
    format!(
        "cd {} 2>/dev/null && find catalogue web_promo_small web_promo -maxdepth 2 -type f \\( -iname '*.png' -o -iname '*.gif' \\) 2>/dev/null | sort; true",
        aio_remote::server::shell_quote(&folder)
    )
}

/// Draws `texture` over `rect` as CSS `background-size: cover` does: filled, centred, the rest cut off.
fn cover(painter: &egui::Painter, texture: &egui::TextureHandle, rect: egui::Rect) {
    let [w, h] = texture.size().map(|v| v.max(1) as f32);
    let scale = (rect.width() / w).max(rect.height() / h);
    let (u, v) = (rect.width() / (w * scale), rect.height() / (h * scale));
    let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(u, v));
    painter.image(texture.id(), rect, uv, Color32::WHITE);
}

/// Draws `texture` whole inside `rect`, on whole pixels when it fits.
fn contain(painter: &egui::Painter, texture: &egui::TextureHandle, rect: egui::Rect) {
    let [w, h] = texture.size().map(|v| v.max(1) as f32);
    let scale = (rect.width() / w).min(rect.height() / h).min(1.0);
    let size = egui::vec2(w * scale, h * scale);
    let min = (rect.center() - size / 2.0).round();
    painter.image(texture.id(), egui::Rect::from_min_size(min, size), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
}

/// A tile being dragged onto another: its index in the slots.
#[derive(Clone, Copy)]
struct Dragged(usize);

fn ended(row: &FeaturedRow) -> bool {
    catalog_admin::parse_expiry(&row.expiry).is_some_and(|end| end > 0 && end < chrono::Utc::now().timestamp())
}

/// The slots in the order the client shows them: by slot id.
fn order(rows: &[FeaturedRow]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].slot.slot_id, i));
    order
}

pub(super) fn show(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let busy = s.busy;
    let saved = s.featured_saved.clone();
    let pages = s.pages.clone();
    let header = s.front.header.clone().unwrap_or_default();
    let Some(rows) = s.featured.as_mut() else {
        ui.label(widgets::title_text(t("Front page"), 17.0));
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        if s.session.is_some() && !busy && ui.add(small(t("Reload"))).clicked() {
            actions.push(Action::LoadFeatured);
        }
        return;
    };
    let slots: Vec<Featured> = rows
        .iter()
        .map(|row| Featured { expire_timestamp: catalog_admin::parse_expiry(&row.expiry).unwrap_or(-2), ..row.slot.clone() })
        .collect();
    let bad_date = rows.iter().find(|row| catalog_admin::parse_expiry(&row.expiry).is_none()).map(|row| row.slot.slot_id);
    let problem = match bad_date {
        Some(id) => Some(tf("Slot {id}: the end date is not YYYY-MM-DD HH:MM.", &[("id", &id.to_string())])),
        None => catalog_admin::featured_problem(&slots),
    };
    let changed = *rows != saved;

    ui.horizontal_wrapped(|ui| {
        ui.label(widgets::title_text(t("Front page"), 17.0));
        ui.add_space(8.0);
        if ui.add_enabled(s.session.is_some() && !busy, small(t("Reload"))).clicked() {
            images.clear();
            s.front.fetched.clear();
            actions.push(Action::LoadFeatured);
        }
        if ui.add_enabled(!busy && rows.len() < catalog_admin::MAX_FEATURED_SLOTS, small(t("Add tile"))).clicked() {
            let current: Vec<Featured> = rows.iter().map(|row| row.slot.clone()).collect();
            rows.push(FeaturedRow::new(catalog_admin::new_featured(&current)));
            s.front.selected = Some(rows.len() - 1);
        }
        if ui.add_enabled(changed && problem.is_none() && !busy, widgets::primary_button(t("Save front page"))).clicked() {
            actions.push(Action::SaveFeatured);
        }
        if ui.add_enabled(changed && !busy, small(t("Undo changes"))).clicked() {
            *rows = saved.clone();
            s.front.selected = None;
        }
        if let Some(problem) = &problem {
            ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
        } else if changed {
            ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.muted));
        }
    });
    widgets::muted(
        ui,
        palette,
        t("The front page as the hotel shows it: the first slot is the tall picture, the others are listed beside it in the order of their slot. Click a tile to edit it; drag a tile onto another to swap their places."),
        12.0,
    );
    ui.add_space(8.0);

    if s.front.selected.is_some_and(|i| i >= rows.len()) {
        s.front.selected = None;
    }
    let mut want: Vec<String> = Vec::new();
    let window_width = FRONT.x + 2.0 * PAD;
    let wide = ui.available_width() >= window_width + 340.0;
    let mut swap: Option<(usize, usize)> = None;
    let mut remove: Option<usize> = None;
    let mut open_picker = false;
    if wide {
        ui.horizontal_top(|ui| {
            game_window(ui, &ctx, &mut s.front, images, rows, &header, &mut want, &mut swap, busy);
            ui.add_space(14.0);
            ui.vertical(|ui| editor(ui, palette, &mut s.front, rows, &pages, busy, &mut remove, &mut open_picker, &mut swap));
        });
    } else {
        game_window(ui, &ctx, &mut s.front, images, rows, &header, &mut want, &mut swap, busy);
        ui.add_space(10.0);
        editor(ui, palette, &mut s.front, rows, &pages, busy, &mut remove, &mut open_picker, &mut swap);
    }
    if let Some((a, b)) = swap {
        if a != b && a < rows.len() && b < rows.len() {
            let slot = rows[a].slot.slot_id;
            rows[a].slot.slot_id = rows[b].slot.slot_id;
            rows[b].slot.slot_id = slot;
        }
    }
    if let Some(index) = remove {
        rows.remove(index);
        s.front.selected = None;
    }
    if open_picker {
        s.front.picker = Some(String::new());
        if s.front.server_files.is_none() && !s.front.listing && !s.front.sample && s.session.is_some() {
            s.front.listing = true;
            actions.push(Action::ListFrontImages);
        }
    }
    picker(&ctx, palette, &mut s.front, rows, images, &mut want);
    if !want.is_empty() && !s.front.sample {
        want.retain(|f| !s.front.fetching.contains(f));
        want.dedup();
        if !want.is_empty() {
            s.front.fetching.extend(want.iter().cloned());
            actions.push(Action::FetchFrontImages(want));
        }
    }
}

/// The game's catalogue window with the front page in it.
#[allow(clippy::too_many_arguments)]
fn game_window(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    front: &mut FrontState,
    images: &mut FrontImages,
    rows: &mut [FeaturedRow],
    header: &FrontHeader,
    want: &mut Vec<String>,
    swap: &mut Option<(usize, usize)>,
    busy: bool,
) {
    let size = egui::vec2(FRONT.x + 2.0 * PAD, TITLE_BAR + HEADER + FRONT.y + 2.0 * PAD);
    let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, CornerRadius::same(8), colors::BODY);
    painter.rect_stroke(outer, CornerRadius::same(8), Stroke::new(1.5_f32, colors::FRAME), egui::StrokeKind::Inside);
    // Title bar.
    let title = egui::Rect::from_min_size(outer.min, egui::vec2(outer.width(), TITLE_BAR));
    painter.rect_filled(title, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, colors::TITLE);
    painter.text(title.center(), egui::Align2::CENTER_CENTER, t("Catalogue"), egui::FontId::proportional(14.0), Color32::WHITE);
    // The page header: its headline image, icon and caption.
    let head = egui::Rect::from_min_size(title.left_bottom(), egui::vec2(outer.width(), HEADER));
    painter.rect_filled(head, CornerRadius::ZERO, colors::HEADER);
    if !header.headline.trim().is_empty() {
        let path = format!("catalogue/{}.gif", header.headline.trim());
        let (texture, fetch) = images.image(ctx, &path, &front.fetched);
        if fetch {
            want.push(path);
        }
        if let Some(texture) = texture {
            let [w, h] = texture.size().map(|v| v as f32);
            let rect = egui::Rect::from_min_size(head.min, egui::vec2(w.min(head.width()), h.min(head.height())));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(rect.width() / w, rect.height() / h));
            painter.with_clip_rect(head).image(texture.id(), rect, uv, Color32::WHITE);
        }
    }
    let mut x = head.left() + 14.0;
    if header.icon > 0 {
        let path = format!("catalogue/icon_{}.png", header.icon);
        let (texture, fetch) = images.image(ctx, &path, &front.fetched);
        if fetch {
            want.push(path);
        }
        if let Some(texture) = texture {
            contain(&painter, &texture, egui::Rect::from_min_size(egui::pos2(x, head.top() + 12.0), egui::vec2(28.0, 28.0)));
            x += 36.0;
        }
    }
    let caption = if header.caption.is_empty() { t("Front page").to_string() } else { header.caption.clone() };
    painter.text(egui::pos2(x, head.top() + 14.0), egui::Align2::LEFT_TOP, caption, egui::FontId::proportional(17.0), Color32::WHITE);

    let origin = head.left_bottom() + egui::vec2(PAD, PAD);
    let at = |r: egui::Rect| r.translate(origin.to_vec2());
    let order = order(rows);

    // The first slot: the tall picture.
    let first_rect = at(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), FIRST));
    match order.first() {
        Some(&index) => tile(ui, ctx, front, images, rows, index, first_rect, want, swap, busy),
        None => placeholder(ui, first_rect, &t("No tiles yet: Add tile")),
    }

    // The list beside it, scrolling like the game's.
    let list_rect = at(egui::Rect::from_min_size(egui::pos2(LIST_X, 0.0), LIST));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        egui::ScrollArea::vertical().id_salt("front-page-list").drag_to_scroll(false).max_height(LIST.y).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = ITEM_GAP;
            for &index in order.iter().skip(1) {
                let (rect, _) = ui.allocate_exact_size(ITEM, Sense::hover());
                tile(ui, ctx, front, images, rows, index, rect, want, swap, busy);
            }
            if order.len() < 2 {
                let (rect, _) = ui.allocate_exact_size(ITEM, Sense::hover());
                placeholder(ui, rect, &t("The other slots are listed here"));
            }
        });
    });

    // The voucher box with the page's text 1.
    let voucher = at(VOUCHER);
    painter.rect_filled(voucher, CornerRadius::same(6), colors::VOUCHER);
    let label = plain(&header.text1);
    painter.with_clip_rect(voucher.shrink(4.0)).text(voucher.min + egui::vec2(10.0, 6.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(12.0), Color32::BLACK);
    let input = egui::Rect::from_min_size(voucher.min + egui::vec2(10.0, 25.0), egui::vec2(216.0, 25.0));
    painter.rect_filled(input, CornerRadius::same(6), Color32::WHITE);
    painter.rect_stroke(input, CornerRadius::same(6), Stroke::new(1.0_f32, Color32::from_gray(123)), egui::StrokeKind::Inside);
    let button = egui::Rect::from_min_size(egui::pos2(voucher.right() - 9.0 - 70.0, voucher.top() + 26.5), egui::vec2(70.0, 22.0));
    painter.rect_filled(button, CornerRadius::same(4), Color32::from_gray(238));
    painter.rect_stroke(button, CornerRadius::same(4), Stroke::new(1.0_f32, Color32::from_gray(90)), egui::StrokeKind::Inside);
    painter.text(button.center(), egui::Align2::CENTER_CENTER, t("Redeem"), egui::FontId::proportional(12.0), Color32::BLACK);

    // What is being dragged, at the pointer.
    if let (Some(dragged), Some(pointer)) = (egui::DragAndDrop::payload::<Dragged>(ctx), ctx.pointer_hover_pos()) {
        if let Some(row) = rows.get(dragged.0) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("front-page-drag")));
            let galley = painter.layout_no_wrap(row.slot.caption.clone(), egui::FontId::proportional(13.0), Color32::WHITE);
            let rect = egui::Rect::from_min_size(pointer + egui::vec2(12.0, 8.0), galley.size() + egui::vec2(12.0, 8.0));
            painter.rect_filled(rect, CornerRadius::same(4), Color32::from_black_alpha(200));
            painter.galley(rect.min + egui::vec2(6.0, 4.0), galley, Color32::WHITE);
        }
    }
}

fn placeholder(ui: &egui::Ui, rect: egui::Rect, text: &str) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), Color32::from_gray(214));
    painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
    let galley = painter.layout(text.to_string(), egui::FontId::proportional(13.0), Color32::from_gray(90), rect.width() - 20.0);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::from_gray(90));
}

/// One featured picture: its image, its caption in the dark bar at the bottom, its slot, and whether it ended.
#[allow(clippy::too_many_arguments)]
fn tile(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    front: &mut FrontState,
    images: &mut FrontImages,
    rows: &[FeaturedRow],
    index: usize,
    rect: egui::Rect,
    want: &mut Vec<String>,
    swap: &mut Option<(usize, usize)>,
    busy: bool,
) {
    let row = &rows[index];
    let id = egui::Id::new(("front-page-tile", index));
    let response = ui.interact(rect, id, if busy { Sense::click() } else { Sense::click_and_drag() });
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, colors::TILE);
    let path = row.slot.image.trim();
    let (texture, fetch) = images.image(ctx, path, &front.fetched);
    if fetch {
        if let Some(path) = clean_path(path) {
            want.push(path);
        }
    }
    match texture {
        Some(texture) => cover(&painter, &texture, rect),
        None => {
            let waiting = clean_path(path).is_some_and(|p| front.fetching.contains(&p) || !front.fetched.contains_key(&p)) && !front.sample;
            let text = if path.is_empty() {
                t("No image").to_string()
            } else if waiting {
                "…".to_string()
            } else {
                tf("Image not found: {path}", &[("path", path)])
            };
            let galley = painter.layout(text, egui::FontId::proportional(12.0), Color32::from_gray(200), rect.width() - 20.0);
            painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::from_gray(200));
        }
    }
    // The caption: a dark bar 3 px from the bottom, as wide as its text.
    if !row.slot.caption.is_empty() {
        let mut job = egui::text::LayoutJob::single_section(row.slot.caption.clone(), egui::TextFormat::simple(egui::FontId::proportional(16.0), Color32::WHITE));
        job.wrap = egui::text::TextWrapping { max_width: rect.width() - 20.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let galley = painter.layout_job(job);
        let bar = egui::Rect::from_min_size(egui::pos2(rect.left() + 5.0, rect.bottom() - 3.0 - galley.size().y - 10.0), galley.size() + egui::vec2(10.0, 10.0));
        painter.rect_filled(bar, CornerRadius::same(3), Color32::from_black_alpha(153));
        painter.galley(bar.min + egui::vec2(5.0, 5.0), galley, Color32::WHITE);
    }
    // The slot, and a mark when its end date passed.
    let badge = painter.layout_no_wrap(format!("#{}", row.slot.slot_id), egui::FontId::proportional(11.0), Color32::WHITE);
    let badge_rect = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), badge.size() + egui::vec2(8.0, 4.0));
    painter.rect_filled(badge_rect, CornerRadius::same(3), Color32::from_black_alpha(150));
    painter.galley(badge_rect.min + egui::vec2(4.0, 2.0), badge, Color32::WHITE);
    if ended(row) {
        let mark = painter.layout_no_wrap(t("Ended").to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
        let mark_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - mark.size().x - 12.0, rect.top() + 4.0), mark.size() + egui::vec2(8.0, 4.0));
        painter.rect_filled(mark_rect, CornerRadius::same(3), colors::ENDED);
        painter.galley(mark_rect.min + egui::vec2(4.0, 2.0), mark, Color32::WHITE);
    }

    let selected = front.selected == Some(index);
    let target = response.dnd_hover_payload::<Dragged>().is_some_and(|d| d.0 != index);
    if selected || target {
        painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(3.0_f32, colors::SELECTED), egui::StrokeKind::Inside);
    } else if response.hovered() {
        painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(2.0_f32, Color32::from_white_alpha(200)), egui::StrokeKind::Inside);
    }
    if !busy {
        response.dnd_set_drag_payload(Dragged(index));
    }
    if let Some(dragged) = response.dnd_release_payload::<Dragged>() {
        if dragged.0 != index {
            *swap = Some((dragged.0, index));
            front.selected = Some(dragged.0);
        }
    }
    let hover = match row.slot.kind.as_str() {
        "page_id" => tf("Opens page #{id}", &[("id", &row.slot.page_id.to_string())]),
        "product_name" => tf("Opens the offer {name}", &[("name", &row.slot.product_name)]),
        _ => tf("Opens the page {name}", &[("name", &row.slot.page_name)]),
    };
    if response.on_hover_text(format!("{hover}\n{}", row.slot.image)).clicked() {
        front.selected = Some(index);
    }
}

/// The form of the tile being edited.
#[allow(clippy::too_many_arguments)]
fn editor(
    ui: &mut egui::Ui,
    palette: &Palette,
    front: &mut FrontState,
    rows: &mut [FeaturedRow],
    pages: &[Page],
    busy: bool,
    remove: &mut Option<usize>,
    open_picker: &mut bool,
    swap: &mut Option<(usize, usize)>,
) {
    widgets::card_frame(palette).inner_margin(12).corner_radius(8).show(ui, |ui| {
        ui.set_width(ui.available_width().clamp(300.0, 460.0));
        let Some(index) = front.selected else {
            ui.label(RichText::new(t("Click a tile to edit it.")).size(13.0).color(palette.muted));
            ui.add_space(4.0);
            ui.label(
                RichText::new(t("Each tile shows an image from c_images and opens a catalogue page or an offer. The first is 184 × 460 pixels, the others 356 × 126; a larger image is cut to fit, centred."))
                    .size(12.0)
                    .color(palette.muted),
            );
            return;
        };
        let order = order(rows);
        let position = order.iter().position(|&i| i == index).unwrap_or(0);
        let row = &mut rows[index];
        ui.label(RichText::new(tf("Tile {n} of {count}", &[("n", &(position + 1).to_string()), ("count", &order.len().to_string())])).size(15.0).strong());
        ui.label(
            RichText::new(if position == 0 { t("The tall picture on the left.") } else { t("In the list beside the tall picture.") })
                .size(12.0)
                .color(palette.muted),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(position > 0 && !busy, small(t("◀ Earlier"))).clicked() {
                *swap = Some((index, order[position - 1]));
            }
            if ui.add_enabled(position + 1 < order.len() && !busy, small(t("Later ▶"))).clicked() {
                *swap = Some((index, order[position + 1]));
            }
            ui.label(RichText::new(t("Slot")).size(12.0).color(palette.muted));
            ui.add(egui::DragValue::new(&mut row.slot.slot_id).range(1..=99).speed(1));
        });
        ui.add_space(6.0);
        let field = (ui.available_width() - 80.0).clamp(200.0, 360.0);
        egui::Grid::new("front-page-editor").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            label(ui, palette, t("Caption"));
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut row.slot.caption).desired_width(field));
                let count = row.slot.caption.chars().count();
                if count > catalog_admin::MAX_FEATURED_CAPTION {
                    ui.label(RichText::new(tf("{count} of at most {max} characters.", &[("count", &count.to_string()), ("max", &catalog_admin::MAX_FEATURED_CAPTION.to_string())])).size(11.5).color(palette.error_text));
                }
            });
            ui.end_row();

            label(ui, palette, t("Image"));
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut row.slot.image).hint_text("catalogue/feature_cata_vert_....png").desired_width(field));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, small(t("Choose ..."))).clicked() {
                        *open_picker = true;
                    }
                    ui.label(RichText::new(t("A path under c_images.")).size(11.5).color(palette.muted));
                });
                if !row.slot.image.trim().is_empty() && clean_path(&row.slot.image).is_none() {
                    ui.label(RichText::new(t("Not a path under c_images.")).size(11.5).color(palette.error_text));
                }
            });
            ui.end_row();

            label(ui, palette, t("Opens"));
            let kind_label = |kind: &str| match kind {
                "page_id" => t("A page, by its id"),
                "product_name" => t("An offer, by its product name"),
                _ => t("A page, by its internal name"),
            };
            egui::ComboBox::from_id_salt("front-page-kind").width(field.min(260.0)).selected_text(kind_label(&row.slot.kind)).show_ui(ui, |ui| {
                for kind in FEATURED_TYPES {
                    ui.selectable_value(&mut row.slot.kind, kind.to_string(), kind_label(kind));
                }
            });
            ui.end_row();

            label(ui, palette, t("Target"));
            ui.vertical(|ui| match row.slot.kind.as_str() {
                "page_id" => {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut row.slot.page_id).range(0..=i32::MAX as i64).speed(1));
                        page_choice(ui, pages, &mut |page| row.slot.page_id = page.id);
                    });
                    match pages.iter().find(|p| p.id == row.slot.page_id) {
                        Some(page) => ui.label(RichText::new(&page.caption).size(12.0).color(palette.muted)),
                        None => ui.label(RichText::new(t("No page has this id.")).size(11.5).color(palette.error_text)),
                    };
                }
                "product_name" => {
                    ui.add(egui::TextEdit::singleline(&mut row.slot.product_name).desired_width(field));
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut row.slot.page_name).desired_width((field - 110.0).max(120.0)));
                        page_choice(ui, pages, &mut |page| row.slot.page_name = page.caption_save.clone());
                    });
                    let name = row.slot.page_name.trim();
                    match pages.iter().find(|p| p.caption_save == name) {
                        Some(page) => ui.label(RichText::new(&page.caption).size(12.0).color(palette.muted)),
                        None => ui.label(RichText::new(t("No page has this internal name.")).size(11.5).color(palette.error_text)),
                    };
                }
            });
            ui.end_row();

            label(ui, palette, t("Ends"));
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut row.expiry).hint_text("YYYY-MM-DD HH:MM").desired_width(150.0)).on_hover_text(t("Empty: it never ends. In the time zone of this computer."));
                    if ui.add_enabled(!row.expiry.is_empty(), small(t("Never"))).clicked() {
                        row.expiry.clear();
                    }
                });
                if ended(row) {
                    ui.label(
                        RichText::new(t("This date has passed. The emulator still sends the tile (as one that never ends), so the hotel keeps showing it: clear the date or remove the tile."))
                            .size(11.5)
                            .color(palette.error_text),
                    );
                }
            });
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, small(t("Remove tile"))).clicked() {
                *remove = Some(index);
            }
            if ui.add(small(t("Done"))).clicked() {
                front.selected = None;
            }
        });
    });
}

fn label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(palette.muted));
}

/// A list of the catalogue pages to pick from.
fn page_choice(ui: &mut egui::Ui, pages: &[Page], pick: &mut dyn FnMut(&Page)) {
    egui::ComboBox::from_id_salt(("front-page-choose-page", ui.id())).selected_text(t("Pick a page")).width(100.0).height(360.0).show_ui(ui, |ui| {
        let mut sorted: Vec<&Page> = pages.iter().filter(|p| !p.layout.starts_with("frontpage")).collect();
        sorted.sort_by_key(|p| (p.caption.to_lowercase(), p.id));
        for page in sorted {
            let text = format!("{}  ({}, #{})", page.caption, page.caption_save, page.id);
            if ui.selectable_label(false, text).clicked() {
                pick(page);
            }
        }
    });
}

/// The image picker: the promo and catalogue images of this computer and the server, as thumbnails.
fn picker(ctx: &egui::Context, palette: &Palette, front: &mut FrontState, rows: &mut [FeaturedRow], images: &mut FrontImages, want: &mut Vec<String>) {
    let Some(search) = front.picker.as_mut() else { return };
    let Some(index) = front.selected.filter(|&i| i < rows.len()) else {
        front.picker = None;
        return;
    };
    let mut files = images.local_files();
    files.extend(front.server_files.iter().flatten().cloned());
    files.sort();
    files.dedup();
    let query = search.trim().to_lowercase();
    let all = front.all_images;
    let shown: Vec<String> = files.into_iter().filter(|f| all || is_promo(f)).filter(|f| query.is_empty() || f.to_lowercase().contains(&query)).collect();
    let current = clean_path(&rows[index].slot.image).unwrap_or_default();
    let mut chosen = None;
    let mut close = false;
    egui::Modal::new(egui::Id::new("front-page-picker")).frame(widgets::card_frame(palette).inner_margin(egui::Margin::symmetric(20, 16))).show(ctx, |ui| {
        ui.set_width(760.0);
        ui.label(RichText::new(t("Choose the image")).size(16.0).strong());
        ui.label(RichText::new(t("The images under c_images (catalogue, web_promo_small and web_promo) of this computer and the server. A click puts its path in the tile.")).size(11.5).color(palette.muted));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(search).hint_text(RichText::new(t("Search")).italics()).desired_width(260.0));
            ui.checkbox(&mut front.all_images, t("Also the other catalogue images"));
        });
        ui.add_space(6.0);
        if front.listing {
            ui.label(RichText::new(t("Reading the server's catalogue images ...")).size(12.0).color(palette.muted));
        }
        let cell = egui::vec2(178.0, 120.0);
        let label_height = 16.0;
        egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.6).auto_shrink([false, true]).show(ui, |ui| {
            if shown.is_empty() && !front.listing {
                ui.label(RichText::new(t("No images found.")).color(palette.muted));
            }
            let columns = ((ui.available_width() + 6.0) / (cell.x + 6.0)).floor().max(1.0) as usize;
            for chunk in shown.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for file in chunk {
                        let (rect, response) = ui.allocate_exact_size(cell + egui::vec2(0.0, label_height), Sense::click());
                        let image_rect = egui::Rect::from_min_size(rect.min, cell);
                        let selected = *file == current;
                        ui.painter().rect_filled(image_rect, CornerRadius::same(5), colors::TILE);
                        ui.painter().rect_stroke(
                            image_rect,
                            CornerRadius::same(5),
                            Stroke::new(if selected { 3.0_f32 } else if response.hovered() { 2.0 } else { 1.0 }, if selected { colors::SELECTED } else { Color32::from_gray(150) }),
                            egui::StrokeKind::Inside,
                        );
                        if ui.is_rect_visible(rect) {
                            let (texture, fetch) = images.image(ctx, file, &front.fetched);
                            if fetch && want.len() < 24 {
                                want.push(file.clone());
                            }
                            match texture {
                                Some(texture) => contain(ui.painter(), &texture, image_rect.shrink(4.0)),
                                None => {
                                    ui.painter().text(image_rect.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(14.0), Color32::from_gray(200));
                                }
                            }
                        }
                        let name = file.rsplit('/').next().unwrap_or(file).to_string();
                        let mut job = egui::text::LayoutJob::single_section(name, egui::TextFormat::simple(egui::FontId::proportional(10.5), palette.muted));
                        job.wrap = egui::text::TextWrapping { max_width: rect.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                        let galley = ui.painter().layout_job(job);
                        ui.painter().galley(egui::pos2(rect.center().x - galley.size().x / 2.0, image_rect.bottom() + 1.0), galley, palette.muted);
                        if response.on_hover_text(file).clicked() {
                            chosen = Some(file.clone());
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                close = true;
            }
            ui.label(RichText::new(tf("{count} shown", &[("count", &shown.len().to_string())])).size(11.5).color(palette.muted));
        });
    });
    if let Some(file) = chosen {
        rows[index].slot.image = file;
        close = true;
    }
    if close {
        front.picker = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_stay_under_c_images() {
        assert_eq!(clean_path(" /catalogue/feature_cata_vert_x.png ").as_deref(), Some("catalogue/feature_cata_vert_x.png"));
        assert_eq!(clean_path("catalogue\\a.png").as_deref(), Some("catalogue/a.png"));
        assert_eq!(clean_path("../etc/passwd"), None);
        assert_eq!(clean_path("catalogue/../../x.png"), None);
        assert_eq!(clean_path(""), None);
    }

    #[test]
    fn the_front_page_is_a_visible_root_page_with_a_front_page_layout() {
        let page = |id, parent_id, layout: &str, visible| Page { id, parent_id, layout: layout.into(), visible, ..Page::default() };
        let pages = vec![page(1, -1, "frontpage4", false), page(2, -1, "default_3x3", true), page(3, -1, "frontpage4", true)];
        assert_eq!(front_page_of(&pages).map(|p| p.id), Some(3));
        assert_eq!(front_page_of(&pages[..2]).map(|p| p.id), Some(1));
        assert!(front_page_of(&pages[1..2]).is_none());
    }

    #[test]
    fn the_voucher_label_is_plain_text() {
        assert_eq!(plain("<b>Redeem</b> a voucher&nbsp;code <br/>here:"), "Redeem a voucher code here:");
    }

    #[test]
    fn tiles_are_shown_in_slot_order() {
        let row = |slot_id| FeaturedRow::new(Featured { slot_id, ..Featured::default() });
        assert_eq!(order(&[row(4), row(1), row(3), row(2)]), vec![1, 3, 2, 0]);
    }
}
