//! The Bundles tab: room bundles and other bundles on a `single_bundle` page, made without SQL. On the left the bundle
//! pages; on the right the chosen one as the game shows it (the picture, the price, a grid of what is in it and the
//! texts) beside its settings: the contents (furni, or catalogue offers the bundle follows, Polaris' `@id` entries),
//! the price, the pictures and the texts. What a buyer really gets is worked out as the emulator does it.

use std::collections::HashMap;

use aio_remote::bundle_admin::{self, Entry, OfferInfo, PageTexts};
use aio_remote::catalog_admin::{self, Catalogue, Furni, Offer, Page};
use eframe::egui::{self, RichText};

use super::front_page::FrontImages;
use super::{small, Action, Ctl, State, Status, FIELD_HEIGHT};
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{t, tf};

/// The bundle being edited: as loaded and as edited, with what is needed to show its contents.
#[derive(Clone)]
pub(super) struct Editor {
    pub page: Page,
    pub page_saved: Page,
    pub texts: PageTexts,
    pub texts_saved: PageTexts,
    /// The page's offer (id 0: not made yet, it is made on Save).
    pub offer: Offer,
    pub offer_saved: Offer,
    /// The offers the contents name (and the ones they name), and every furni involved, for names and icons.
    pub offers: HashMap<i64, OfferInfo>,
    pub furni: HashMap<i64, Furni>,
    /// More than one offer on the page: the bundle is the first, the others are not shown by the layout.
    pub extra_offers: usize,
}

impl Editor {
    fn changed(&self) -> bool {
        self.page != self.page_saved || self.texts != self.texts_saved || self.offer != self.offer_saved
    }

    fn entries(&self) -> Vec<Entry> {
        bundle_admin::parse(&self.offer.item_ids).unwrap_or_default()
    }

    fn set_entries(&mut self, entries: &[Entry]) {
        self.offer.item_ids = bundle_admin::write(entries);
    }

    /// What a buyer gets, and the offers that could not be used.
    fn expansion(&self) -> (Vec<(i64, i64)>, Vec<i64>) {
        let lookup = |id: i64| self.offers.get(&id).map(|o| bundle_admin::Content { item_ids: o.item_ids.clone(), amount: o.amount });
        bundle_admin::expand(&self.offer.item_ids, self.offer.amount.max(1), &lookup)
    }

    fn furni_name(&self, id: i64) -> String {
        match self.furni.get(&id) {
            Some(f) if !f.public_name.trim().is_empty() => f.public_name.trim().to_string(),
            Some(f) => f.item_name.clone(),
            None => format!("#{id}"),
        }
    }
}

/// What can be added to the contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AddKind {
    Furni,
    Offer,
}

/// The window that adds to the contents: a search and its results.
pub(super) struct AddWindow {
    pub kind: AddKind,
    pub search: String,
    pub furni: Option<Vec<Furni>>,
    pub offers: Option<Vec<OfferInfo>>,
}

/// Which picture the image window chooses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Picture {
    Header,
    Teaser,
}

#[derive(Default)]
pub(super) struct BundlesTab {
    pub selected: Option<i64>,
    pub editor: Option<Editor>,
    pub new_name: String,
    pub new_parent: i64,
    pub add: Option<AddWindow>,
    /// The image window: which picture, and its filter.
    pub picture: Option<(Picture, String)>,
}

fn currency_name(points_type: i64) -> String {
    match points_type {
        0 => t("Duckets").into(),
        5 => t("Diamonds").into(),
        other => tf("Points type {type}", &[("type", &other.to_string())]),
    }
}

/// Text without its HTML tags, for the preview.
fn plain(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let lower = html.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    for c in lower.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").trim().to_string()
}

impl Ctl {
    /// Opens a bundle page: its texts, its offer and what the contents name.
    pub(super) async fn open_bundle(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(page) = self.lock().pages.iter().find(|p| p.id == id).cloned() else { return };
        self.with(generation, |s| {
            s.busy = true;
            s.bundles.selected = Some(id);
            s.bundles.editor = None;
        });
        let texts = bundle_admin::load_texts(&session, &database, id).await;
        let offers = catalog_admin::load_offers(&session, &database, Catalogue::Normal, id).await;
        let (texts, mut offers) = match (texts, offers) {
            (Ok(texts), Ok(offers)) => (texts, offers),
            (Err(error), _) | (_, Err(error)) => {
                self.with(generation, |s| {
                    s.busy = false;
                    s.status = Status::error(error.to_string());
                });
                return;
            }
        };
        offers.sort_by_key(|o| (o.order_number, o.id));
        let extra_offers = offers.len().saturating_sub(1);
        let offer = offers.into_iter().next().unwrap_or_else(|| bundle_admin::new_bundle_offer(&page));
        let editor = Editor {
            page_saved: page.clone(),
            page,
            texts_saved: texts.clone(),
            texts,
            offer_saved: offer.clone(),
            offer,
            offers: HashMap::new(),
            furni: HashMap::new(),
            extra_offers,
        };
        self.with(generation, |s| {
            s.busy = false;
            if s.bundles.selected == Some(id) {
                s.bundles.editor = Some(editor);
            }
        });
        self.resolve_bundle().await;
    }

    /// Reads the offers the contents name and the names of every furni in it.
    pub(super) async fn resolve_bundle(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((page, item_ids)) = self.lock().bundles.editor.as_ref().map(|e| (e.page.id, e.offer.item_ids.clone())) else { return };
        let offers = bundle_admin::referenced_offers(&session, &database, &item_ids).await.unwrap_or_default();
        let mut ids = bundle_admin::furni_ids(&item_ids);
        for offer in offers.values() {
            ids.extend(bundle_admin::furni_ids(&offer.item_ids));
        }
        ids.sort_unstable();
        ids.dedup();
        let known: Vec<i64> = self.lock().bundles.editor.as_ref().map(|e| e.furni.keys().copied().collect()).unwrap_or_default();
        ids.retain(|id| !known.contains(id));
        let furni = catalog_admin::furni_by_ids(&session, &database, &ids).await.unwrap_or_default();
        self.with(generation, |s| {
            if let Some(editor) = s.bundles.editor.as_mut().filter(|e| e.page.id == page) {
                editor.offers = offers;
                for f in furni {
                    editor.furni.insert(f.id, f);
                }
            }
        });
    }

    /// Makes a new, hidden bundle page and opens it.
    pub(super) async fn new_bundle(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let page = {
            let s = self.lock();
            bundle_admin::new_bundle_page(&s.pages, s.bundles.new_parent, &s.bundles.new_name)
        };
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_page(&session, &database, Catalogue::Normal, &page).await;
        self.with(generation, |s| s.busy = false);
        match saved {
            Ok(id) => {
                self.with(generation, |s| {
                    s.unpublished = true;
                    s.bundles.new_name.clear();
                    s.status = Status::ok(t("New bundle page made, hidden until you tick Visible. Add what is in it, then Save.").into());
                });
                self.load_pages().await;
                self.open_bundle(id).await;
            }
            Err(error) => {
                self.with(generation, |s| s.status = Status::error(error.to_string()));
            }
        }
    }

    /// Saves the page, its pictures and texts, and the bundle offer (made when it is new).
    pub(super) async fn save_bundle(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(editor) = self.lock().bundles.editor.clone() else { return };
        if let Some(problem) = bundle_admin::problem(&editor.offer).or_else(|| catalog_admin::page_problem(&editor.page, &self.lock().pages)) {
            self.with(generation, |s| s.status = Status::error(t(&problem).to_string()));
            return;
        }
        self.with(generation, |s| s.busy = true);
        let mut result: aio_remote::server::Result<()> = Ok(());
        if editor.page != editor.page_saved {
            result = catalog_admin::save_page(&session, &database, Catalogue::Normal, &editor.page).await.map(|_| ());
        }
        if result.is_ok() && editor.texts != editor.texts_saved {
            result = bundle_admin::save_texts(&session, &database, editor.page.id, &editor.texts, &editor.texts_saved).await.map(|_| ());
        }
        if result.is_ok() && (editor.offer != editor.offer_saved || editor.offer.id <= 0) {
            result = catalog_admin::save_offer(&session, &database, Catalogue::Normal, &editor.offer).await.map(|_| ());
        }
        self.with(generation, |s| {
            s.busy = false;
            match &result {
                Ok(()) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("Bundle {name} saved. Publish to put it in the hotel.", &[("name", editor.page.caption.trim())]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_pages().await;
        self.open_bundle(editor.page.id).await;
    }

    pub(super) async fn search_bundle_add(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((kind, search)) = self.lock().bundles.add.as_ref().map(|a| (a.kind, a.search.clone())) else { return };
        self.with(generation, |s| s.busy = true);
        let result = match kind {
            AddKind::Furni => catalog_admin::search_furni(&session, &database, &search).await.map(|list| (Some(list), None)),
            AddKind::Offer => bundle_admin::search_offers(&session, &database, &search).await.map(|list| (None, Some(list))),
        };
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok((furni, offers)) => {
                    if let Some(add) = s.bundles.add.as_mut() {
                        add.furni = furni;
                        add.offers = offers;
                    }
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }
}

/// A furni's icon (from this computer's gamedata), or an empty square.
fn icon(ui: &mut egui::Ui, palette: &Palette, images: &mut FrontImages, furni: Option<&Furni>, side: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    ui.painter().rect_filled(rect, 6.0, palette.surface);
    if let Some(texture) = furni.and_then(|f| images.furni_icon(&f.item_name)) {
        let scale = (side / texture.size_vec2().x).min(side / texture.size_vec2().y).min(3.0);
        let shown = egui::Rect::from_center_size(rect.center(), texture.size_vec2() * scale);
        ui.painter().image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    rect
}

/// A catalogue picture (`c_images/catalogue/<name>.gif`), from this computer or the server; `want` collects the ones to
/// fetch.
fn picture(ui: &mut egui::Ui, palette: &Palette, s: &State, images: &mut FrontImages, name: &str, size: egui::Vec2, want: &mut Vec<String>) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, palette.surface);
    let name = name.trim();
    if name.is_empty() {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, t("No picture"), egui::FontId::proportional(11.0), palette.muted);
        return;
    }
    let path = format!("catalogue/{name}.gif");
    let (texture, fetch) = images.image(ui.ctx(), &path, &s.front.fetched);
    if fetch {
        want.push(path);
    }
    match texture {
        Some(texture) => {
            let scale = (size.x / texture.size_vec2().x).min(size.y / texture.size_vec2().y).min(2.0);
            let shown = egui::Rect::from_center_size(rect.center(), texture.size_vec2() * scale);
            ui.painter().image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        None => {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, format!("{name}.gif"), egui::FontId::proportional(11.0), palette.muted);
        }
    }
}

pub(super) fn show(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Bundles"), 17.0));
        ui.add_space(8.0);
        if ui.add_enabled(ready, small(t("Reload"))).clicked() {
            actions.push(Action::Refresh);
        }
    });
    widgets::muted(
        ui,
        palette,
        t("A bundle sells several furni at once for one price, on a page with the single_bundle layout (room bundles too). Put furni in it, or catalogue offers: the bundle then follows the offer, so a change to that offer changes the bundle."),
        12.5,
    );
    ui.add_space(6.0);
    let mut want = Vec::new();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(230.0);
            list(ui, palette, s, actions);
        });
        ui.separator();
        ui.vertical(|ui| {
            if s.bundles.editor.is_some() {
                editor(ui, palette, s, images, actions, &mut want);
            } else if s.bundles.selected.is_some() {
                widgets::muted(ui, palette, t("Loading ..."), 12.5);
            } else {
                widgets::muted(ui, palette, t("Choose a bundle on the left, or make a new one."), 12.5);
            }
        });
    });
    add_window(ui.ctx(), palette, s, images, actions);
    picture_window(ui.ctx(), palette, s, images, &mut want);
    want.dedup();
    if !want.is_empty() && !s.front.sample {
        s.front.wanted = want;
        if !s.front.fetching {
            s.front.fetching = true;
            actions.push(Action::FetchFrontImages);
        }
    }
}

fn list(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let pages: Vec<Page> = bundle_admin::bundle_pages(&s.pages).into_iter().cloned().collect();
    ui.label(RichText::new(tf("Bundle pages ({count})", &[("count", &pages.len().to_string())])).size(13.5).strong());
    if pages.is_empty() {
        widgets::muted(ui, palette, t("No bundle pages yet."), 12.0);
    }
    for page in &pages {
        let selected = s.bundles.selected == Some(page.id);
        let caption = if page.caption.trim().is_empty() { format!("#{}", page.id) } else { page.caption.trim().to_string() };
        let mut text = RichText::new(caption).size(13.0);
        if !page.visible {
            text = text.color(palette.muted);
        }
        let response = ui.add_sized([226.0, 26.0], egui::SelectableLabel::new(selected, text));
        let response = if page.visible { response } else { response.on_hover_text(t("Hidden: players do not see it yet.")) };
        if response.clicked() && !selected && ready {
            actions.push(Action::OpenBundle(page.id));
        }
    }
    ui.add_space(12.0);
    ui.label(RichText::new(t("New bundle")).size(13.5).strong());
    ui.add_sized([226.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.bundles.new_name).hint_text(t("Name, e.g. Sunny Café")).vertical_align(egui::Align::Center));
    let parents: Vec<&Page> = s.pages.iter().filter(|p| p.parent_id == -1).collect();
    let chosen = parents.iter().find(|p| p.id == s.bundles.new_parent).map(|p| p.caption.clone()).unwrap_or_else(|| t("Top of the catalogue").to_string());
    egui::ComboBox::from_id_salt("bundle-new-parent").selected_text(tf("Under: {page}", &[("page", &chosen)])).width(226.0).show_ui(ui, |ui| {
        ui.selectable_value(&mut s.bundles.new_parent, -1, t("Top of the catalogue"));
        for page in parents {
            ui.selectable_value(&mut s.bundles.new_parent, page.id, page.caption.clone());
        }
    });
    if ui.add_enabled(ready, widgets::primary_button(t("Make the bundle page"))).clicked() {
        actions.push(Action::NewBundle);
    }
}

fn editor(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>, want: &mut Vec<String>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(editor) = s.bundles.editor.clone() else { return };
    let (furni_total, missing) = editor.expansion();

    // The page's name, visibility and the save buttons.
    let mut page = editor.page.clone();
    ui.horizontal(|ui| {
        ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut page.caption).hint_text(t("Name of the page")).vertical_align(egui::Align::Center));
        ui.checkbox(&mut page.visible, t("Visible")).on_hover_text(t("Players see the page in the catalogue."));
        let problem = bundle_admin::problem(&editor.offer);
        if ui.add_enabled(ready && editor.changed() && problem.is_none(), widgets::primary_button(t("Save"))).clicked() {
            actions.push(Action::SaveBundle);
        }
        if editor.changed() && ui.add_enabled(ready, small(t("Undo the changes"))).clicked() {
            actions.push(Action::OpenBundle(editor.page.id));
        }
        if let Some(problem) = problem {
            ui.label(RichText::new(t(&problem)).size(12.0).color(palette.warning_text));
        }
    });
    if editor.extra_offers > 0 {
        widgets::muted(ui, palette, &tf("This page has {count} more offers: a bundle page shows only the first one. Move the others in the Catalogue explorer.", &[("count", &editor.extra_offers.to_string())]), 12.0);
    }
    ui.add_space(6.0);

    // The game's view beside the settings when there is room for both, else above them.
    let preview_width = 360.0;
    if ui.available_width() >= preview_width + 12.0 + 420.0 {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(preview_width);
                ui.label(RichText::new(t("In the game")).size(13.0).strong());
                preview(ui, palette, s, images, &editor, &furni_total, want);
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width().min(600.0));
                settings(ui, palette, s, images, actions, &editor, &furni_total, &missing);
            });
        });
    } else {
        ui.vertical(|ui| {
            ui.set_width(preview_width.min(ui.available_width()));
            ui.label(RichText::new(t("In the game")).size(13.0).strong());
            preview(ui, palette, s, images, &editor, &furni_total, want);
        });
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width().min(600.0));
            settings(ui, palette, s, images, actions, &editor, &furni_total, &missing);
        });
    }
    if let Some(e) = s.bundles.editor.as_mut() {
        if e.page.caption != page.caption || e.page.visible != page.visible {
            e.page.caption = page.caption;
            e.page.visible = page.visible;
        }
    }
}

/// The page as the game draws it (Octane's single bundle layout): the description on top, the picture with the price,
/// the special text and the grid of what is in it, and the text at the bottom.
fn preview(ui: &mut egui::Ui, palette: &Palette, s: &State, images: &mut FrontImages, editor: &Editor, furni: &[(i64, i64)], want: &mut Vec<String>) {
    let game = egui::Color32::from_rgb(234, 234, 234);
    let ink = egui::Color32::from_rgb(30, 30, 30);
    egui::Frame::new().fill(egui::Color32::from_rgb(28, 74, 96)).corner_radius(8).inner_margin(6).show(ui, |ui| {
        ui.horizontal(|ui| {
            picture(ui, palette, s, images, &editor.texts.headline, egui::vec2(120.0, 40.0), want);
            ui.vertical(|ui| {
                ui.label(RichText::new(editor.page.caption.trim()).size(14.0).strong().color(egui::Color32::WHITE));
                let text1 = plain(&editor.texts.text1);
                if !text1.is_empty() {
                    ui.add(egui::Label::new(RichText::new(text1).size(11.0).color(egui::Color32::from_gray(220))).truncate());
                }
            });
        });
        egui::Frame::new().fill(game).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let details = plain(&editor.texts.details);
            if !details.is_empty() {
                ui.add(egui::Label::new(RichText::new(details).size(11.5).color(ink)).wrap());
                ui.add_space(4.0);
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    picture(ui, palette, s, images, &editor.texts.teaser, egui::vec2(150.0, 150.0), want);
                    let mut price = Vec::new();
                    if editor.offer.cost_credits > 0 {
                        price.push(tf("{amount} credits", &[("amount", &editor.offer.cost_credits.to_string())]));
                    }
                    if editor.offer.cost_points > 0 {
                        price.push(format!("{} {}", editor.offer.cost_points, currency_name(editor.offer.points_type)));
                    }
                    let price = if price.is_empty() { t("Free").to_string() } else { price.join(" + ") };
                    ui.label(RichText::new(price).size(13.0).strong().color(ink));
                });
                ui.vertical(|ui| {
                    let special = plain(&editor.texts.teaser_text);
                    if !special.is_empty() {
                        ui.add(egui::Label::new(RichText::new(special).size(11.0).color(ink)).wrap());
                    }
                    // The grid of what is in it, four to a row, as the game shows it.
                    for row in furni.chunks(4) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (id, count) in row {
                                let rect = icon(ui, palette, images, editor.furni.get(id), 44.0);
                                if *count > 1 {
                                    ui.painter().text(rect.right_bottom() - egui::vec2(3.0, 2.0), egui::Align2::RIGHT_BOTTOM, format!("x{count}"), egui::FontId::proportional(11.0), egui::Color32::from_rgb(255, 255, 255));
                                }
                            }
                        });
                    }
                    if furni.is_empty() {
                        ui.label(RichText::new(t("Nothing in it yet")).size(11.0).color(egui::Color32::from_gray(110)));
                    }
                });
            });
            let footer = plain(&editor.texts.text2);
            if !footer.is_empty() {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(footer).size(11.0).color(ink)).wrap());
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn settings(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>, editor: &Editor, furni: &[(i64, i64)], missing: &[i64]) {
    let ready = s.session.is_some() && !s.busy;
    let mut entries = editor.entries();
    let mut changed = false;

    ui.label(RichText::new(t("What is in it")).size(14.0).strong());
    let mut remove = None;
    let mut swap = None;
    let count = entries.len();
    for (index, entry) in entries.iter_mut().enumerate() {
        widgets::card_frame(palette).inner_margin(6).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                match *entry {
                    Entry::Furni { id, .. } => {
                        icon(ui, palette, images, editor.furni.get(&id), 34.0);
                        ui.vertical(|ui| {
                            ui.set_width(170.0);
                            ui.label(RichText::new(editor.furni_name(id)).size(13.0));
                            let class = editor.furni.get(&id).map(|f| f.item_name.clone()).unwrap_or_default();
                            widgets::muted(ui, palette, &format!("{class} · #{id}"), 11.0);
                        });
                    }
                    Entry::Offer { id, .. } => {
                        let first = editor.offers.get(&id).and_then(|o| bundle_admin::furni_ids(&o.item_ids).first().copied());
                        icon(ui, palette, images, first.and_then(|f| editor.furni.get(&f)), 34.0);
                        ui.vertical(|ui| {
                            ui.set_width(170.0);
                            match editor.offers.get(&id) {
                                Some(offer) => {
                                    // The furni it sells by name; the offer's own name under it.
                                    let names: Vec<String> = bundle_admin::furni_ids(&offer.item_ids).iter().map(|f| editor.furni_name(*f)).collect();
                                    let title = if names.is_empty() { offer.name.clone() } else { names.join(", ") };
                                    ui.add(egui::Label::new(RichText::new(title).size(13.0)).truncate());
                                    ui.add(egui::Label::new(RichText::new(tf("Follows offer #{id} {name} ({page})", &[("id", &id.to_string()), ("name", &offer.name), ("page", &offer.page)])).size(11.0).color(palette.muted)).truncate());
                                }
                                None => {
                                    ui.label(RichText::new(tf("Offer #{id}", &[("id", &id.to_string())])).size(13.0).color(palette.warning_text));
                                    widgets::muted(ui, palette, t("This offer does not exist: the emulator leaves it out."), 11.0);
                                }
                            }
                        });
                    }
                }
                ui.label(RichText::new("×").size(13.0));
                let mut n = entry.count();
                if ui.add_sized([60.0, 24.0], egui::DragValue::new(&mut n).range(1..=bundle_admin::MAX_AMOUNT)).changed() {
                    *entry = entry.with_count(n);
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(small("✕")).on_hover_text(t("Take it out")).clicked() {
                        remove = Some(index);
                    }
                    if ui.add_enabled(index + 1 < count, small("↓")).clicked() {
                        swap = Some((index, index + 1));
                    }
                    if ui.add_enabled(index > 0, small("↑")).clicked() {
                        swap = Some((index - 1, index));
                    }
                });
            });
        });
    }
    if let Some((a, b)) = swap {
        entries.swap(a, b);
        changed = true;
    }
    if let Some(index) = remove {
        entries.remove(index);
        changed = true;
    }
    if entries.is_empty() {
        widgets::muted(ui, palette, t("Nothing in it yet: add furni, or an offer the bundle should follow."), 12.0);
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, widgets::secondary_button(t("Add furni ..."))).clicked() {
            s.bundles.add = Some(AddWindow { kind: AddKind::Furni, search: String::new(), furni: None, offers: None });
        }
        if ui.add_enabled(ready, widgets::secondary_button(t("Add an offer ..."))).on_hover_text(t("The bundle follows the offer: change the offer's furni or amount, and the bundle changes with it.")).clicked() {
            s.bundles.add = Some(AddWindow { kind: AddKind::Offer, search: String::new(), furni: None, offers: None });
        }
    });
    if changed {
        if let Some(e) = s.bundles.editor.as_mut() {
            e.set_entries(&entries);
        }
    }

    // What a buyer gets, worked out as the emulator does.
    ui.add_space(6.0);
    let total: i64 = furni.iter().map(|(_, n)| n).sum();
    ui.label(RichText::new(tf("The buyer gets {count} furni", &[("count", &total.to_string())])).size(13.0).strong());
    let names: Vec<String> = furni.iter().map(|(id, n)| format!("{n} × {}", editor.furni_name(*id))).collect();
    if !names.is_empty() {
        ui.add(egui::Label::new(RichText::new(names.join(", ")).size(12.0).color(palette.muted)).wrap());
    }
    if !missing.is_empty() {
        let ids: Vec<String> = missing.iter().map(|id| format!("#{id}")).collect();
        ui.label(RichText::new(tf("Left out by the emulator (missing, or naming itself): {offers}", &[("offers", &ids.join(", "))])).size(12.0).color(palette.warning_text));
    }

    // The price.
    ui.add_space(10.0);
    ui.label(RichText::new(t("Price")).size(14.0).strong());
    let mut offer = editor.offer.clone();
    egui::Grid::new("bundle-price").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(RichText::new(t("Credits")).size(12.5).color(palette.muted));
        ui.add(egui::DragValue::new(&mut offer.cost_credits).range(0..=1_000_000));
        ui.end_row();
        ui.label(RichText::new(t("Points")).size(12.5).color(palette.muted));
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut offer.cost_points).range(0..=1_000_000));
            egui::ComboBox::from_id_salt("bundle-points-type").selected_text(currency_name(offer.points_type)).width(130.0).show_ui(ui, |ui| {
                for kind in [0, 5] {
                    ui.selectable_value(&mut offer.points_type, kind, currency_name(kind));
                }
            });
            ui.add(egui::DragValue::new(&mut offer.points_type).range(0..=1000)).on_hover_text(t("The points type number"));
        });
        ui.end_row();
        ui.label(RichText::new(t("HC only")).size(12.5).color(palette.muted));
        ui.checkbox(&mut offer.club_only, t("Only Habbo Club members can buy it"));
        ui.end_row();
        ui.label(RichText::new(t("Offer name")).size(12.5).color(palette.muted));
        ui.add_sized([240.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut offer.catalog_name).vertical_align(egui::Align::Center));
        ui.end_row();
    });

    // The pictures and texts.
    ui.add_space(10.0);
    ui.label(RichText::new(t("Pictures and texts")).size(14.0).strong());
    let mut texts = editor.texts.clone();
    egui::Grid::new("bundle-texts").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (which, label, value) in [(Picture::Teaser, t("Big picture"), &mut texts.teaser), (Picture::Header, t("Header picture"), &mut texts.headline)] {
            ui.label(RichText::new(label).size(12.5).color(palette.muted));
            ui.horizontal(|ui| {
                ui.add_sized([200.0, FIELD_HEIGHT], egui::TextEdit::singleline(value).hint_text(t("Image name, without .gif")).vertical_align(egui::Align::Center));
                if ui.add(small(t("Choose ..."))).clicked() {
                    s.bundles.picture = Some((which, String::new()));
                    if s.front.server_files.is_none() && !s.front.listing && !s.front.sample && s.session.is_some() {
                        s.front.listing = true;
                        actions.push(Action::ListFrontImages);
                    }
                }
            });
            ui.end_row();
        }
        let width = ui.available_width().clamp(240.0, 420.0);
        for (label, hint, value) in [
            (t("Description"), t("On top of the page"), &mut texts.details),
            (t("Special text"), t("Over what is in the bundle"), &mut texts.teaser_text),
            (t("Bottom text"), t("Under it all"), &mut texts.text2),
            (t("Header text"), t("Beside the header picture"), &mut texts.text1),
        ] {
            ui.label(RichText::new(label).size(12.5).color(palette.muted));
            ui.add_sized([width, 44.0], egui::TextEdit::multiline(value).hint_text(hint));
            ui.end_row();
        }
    });
    widgets::muted(ui, palette, t("The texts may hold simple HTML (<b>, <i>, <br>, links); anything else is taken out on Save."), 11.5);
    if let Some(e) = s.bundles.editor.as_mut() {
        if e.texts != texts {
            e.texts = texts;
        }
        // The contents may have changed above; keep them, and take the rest of the offer from the form.
        let item_ids = e.offer.item_ids.clone();
        if e.offer != (Offer { item_ids: item_ids.clone(), ..offer.clone() }) {
            e.offer = Offer { item_ids, ..offer };
        }
    }
}

/// The window that adds furni or an offer to the contents.
fn add_window(ctx: &egui::Context, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(add) = s.bundles.add.as_mut() else { return };
    let title = if add.kind == AddKind::Furni { t("Add furni") } else { t("Add an offer") };
    let mut open = true;
    let mut chosen: Option<Entry> = None;
    egui::Window::new(title).id(egui::Id::new("bundle-add")).open(&mut open).collapsible(false).resizable(false).default_width(520.0).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        let hint = if add.kind == AddKind::Furni { t("Name, class name or items_base id") } else { t("Offer name or id") };
        ui.horizontal(|ui| {
            let response = ui.add_sized([320.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut add.search).hint_text(hint).vertical_align(egui::Align::Center));
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(ready, small(t("Search"))).clicked() || enter) && ready {
                actions.push(Action::SearchBundleAdd);
            }
        });
        if add.kind == AddKind::Offer {
            widgets::muted(ui, palette, t("The bundle follows the offer: what it sells, as often as you set."), 11.5);
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(320.0).min_scrolled_height(320.0).auto_shrink([false, false]).show(ui, |ui| match add.kind {
            AddKind::Furni => match add.furni.as_ref() {
                None => widgets::muted(ui, palette, t("Search for a furni."), 12.0),
                Some(list) if list.is_empty() => widgets::muted(ui, palette, t("No furni found."), 12.0),
                Some(list) => {
                    for furni in list {
                        ui.horizontal(|ui| {
                            icon(ui, palette, images, Some(furni), 36.0);
                            ui.vertical(|ui| {
                                ui.set_width(300.0);
                                let name = if furni.public_name.trim().is_empty() { furni.item_name.as_str() } else { furni.public_name.trim() };
                                ui.label(RichText::new(name).size(13.0));
                                widgets::muted(ui, palette, &format!("{} · #{}", furni.item_name, furni.id), 11.0);
                            });
                            if ui.add(small(t("Add"))).clicked() {
                                chosen = Some(Entry::Furni { id: furni.id, amount: Some(1) });
                                if let Some(e) = s.bundles.editor.as_mut() {
                                    e.furni.insert(furni.id, furni.clone());
                                }
                            }
                        });
                    }
                }
            },
            AddKind::Offer => match add.offers.as_ref() {
                None => widgets::muted(ui, palette, t("Search for an offer."), 12.0),
                Some(list) if list.is_empty() => widgets::muted(ui, palette, t("No offer found."), 12.0),
                Some(list) => {
                    let own = s.bundles.editor.as_ref().map(|e| e.offer.id).unwrap_or(0);
                    for offer in list {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(370.0);
                                ui.label(RichText::new(&offer.name).size(13.0));
                                widgets::muted(ui, palette, &format!("#{} · {} · {}", offer.id, offer.page, offer.item_ids), 11.0);
                            });
                            if offer.id == own {
                                widgets::muted(ui, palette, t("This bundle"), 11.5);
                            } else if ui.add(small(t("Add"))).clicked() {
                                chosen = Some(Entry::Offer { id: offer.id, times: None });
                            }
                        });
                    }
                }
            },
        });
    });
    if let Some(entry) = chosen {
        if let Some(e) = s.bundles.editor.as_mut() {
            let mut entries = e.entries();
            // The same furni or offer again: one more of it.
            match entries.iter_mut().find(|x| std::mem::discriminant(*x) == std::mem::discriminant(&entry) && x.id() == entry.id()) {
                Some(existing) => *existing = existing.with_count(existing.count() + 1),
                None => entries.push(entry),
            }
            e.set_entries(&entries);
        }
        actions.push(Action::ResolveBundle);
    }
    if !open {
        s.bundles.add = None;
    }
}

/// The window that chooses a page picture from the catalogue images (`c_images/catalogue/*.gif`) of this computer
/// and the server.
fn picture_window(ctx: &egui::Context, palette: &Palette, s: &mut State, images: &mut FrontImages, want: &mut Vec<String>) {
    let Some((which, mut filter)) = s.bundles.picture.clone() else { return };
    let mut names: Vec<String> = images.local_files().into_iter().chain(s.front.server_files.clone().unwrap_or_default()).filter_map(|path| {
        let name = path.strip_prefix("catalogue/")?;
        (!name.contains('/') && name.to_lowercase().ends_with(".gif")).then(|| name[..name.len() - 4].to_string())
    }).collect();
    names.sort();
    names.dedup();
    let mut open = true;
    let mut picked = None;
    let title = if which == Picture::Teaser { t("Choose the big picture") } else { t("Choose the header picture") };
    egui::Window::new(title).id(egui::Id::new("bundle-picture")).open(&mut open).collapsible(false).resizable(false).default_width(560.0).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut filter).hint_text(t("Filter by name")).vertical_align(egui::Align::Center));
            let note = if s.front.listing { t("Reading the server's images ...").to_string() } else { tf("{count} pictures", &[("count", &names.len().to_string())]) };
            widgets::muted(ui, palette, &note, 12.0);
        });
        let query = filter.trim().to_lowercase();
        let shown: Vec<&String> = names.iter().filter(|n| query.is_empty() || n.to_lowercase().contains(&query)).take(120).collect();
        egui::ScrollArea::vertical().max_height(330.0).min_scrolled_height(330.0).auto_shrink([false, false]).show(ui, |ui| {
            for row in shown.chunks(4) {
                ui.horizontal(|ui| {
                    for name in row {
                        ui.vertical(|ui| {
                            ui.set_width(124.0);
                            picture(ui, palette, s, images, name, egui::vec2(120.0, 90.0), want);
                            let response = ui.add(egui::Button::new(RichText::new(name.as_str()).size(11.0)).truncate().min_size(egui::vec2(120.0, 22.0)));
                            if response.clicked() {
                                picked = Some((*name).clone());
                            }
                        });
                    }
                });
                ui.add_space(4.0);
            }
            if shown.is_empty() {
                widgets::muted(ui, palette, t("No pictures found: type the name by hand."), 12.0);
            }
        });
    });
    if let Some(name) = picked {
        if let Some(e) = s.bundles.editor.as_mut() {
            match which {
                Picture::Teaser => e.texts.teaser = name,
                Picture::Header => e.texts.headline = name,
            }
        }
        s.bundles.picture = None;
    } else if !open {
        s.bundles.picture = None;
    } else {
        s.bundles.picture = Some((which, filter));
    }
}
