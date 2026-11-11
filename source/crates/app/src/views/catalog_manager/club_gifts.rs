//! The Club gifts tab: the free gifts Habbo Club members choose in the catalogue. A gift is an offer on the page with
//! the `club_gift` layout: its furni, the HC days it asks for (extradata), the VIP mark (club_only) and its place in
//! the list. A member gets one gift per started 31 days of HC (all their HC time, earlier memberships too), minus the
//! gifts taken; a gift whose days are not reached stays greyed out in the game. Prices are not used: gifts are free.

use aio_remote::catalog_admin::{self, Catalogue, Furni, Offer};
use eframe::egui::{self, RichText};

use super::{small, Action, Ctl, State, Status, FIELD_HEIGHT};
use super::front_page::FrontImages;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{n, t, tf};

/// A gift as shown: the offer as edited and as saved, and its furni (for the name and icon).
#[derive(Clone)]
pub(super) struct Gift {
    pub offer: Offer,
    pub saved: Offer,
    pub furni: Vec<Furni>,
}

impl Gift {
    fn changed(&self) -> bool {
        self.offer != self.saved
    }

    fn days(&self) -> i64 {
        catalog_admin::club_gift_days(&self.offer.extradata).max(0)
    }

    fn title(&self) -> String {
        match self.furni.first() {
            Some(f) if !f.public_name.trim().is_empty() => f.public_name.trim().to_string(),
            Some(f) => f.item_name.clone(),
            None => self.offer.catalog_name.clone(),
        }
    }
}

/// What the tab keeps.
#[derive(Default)]
pub(super) struct ClubGifts {
    /// The club gift page shown (None: the one the emulator uses, or the first).
    pub page: Option<i64>,
    /// The gifts of that page (None until loaded).
    pub gifts: Option<Vec<Gift>>,
    pub search: String,
    /// The furni found for adding (None before a search).
    pub results: Option<Vec<Furni>>,
    /// The window of the club furni to choose from (Some while it is open).
    pub picker: Option<Picker>,
}

/// The club furni window: every furni whose class name starts with [`PICKER_PREFIX`], a filter, and the ones ticked.
#[derive(Default)]
pub(super) struct Picker {
    /// None while it loads.
    pub furni: Option<Vec<Furni>>,
    pub filter: String,
    pub ticked: std::collections::BTreeSet<i64>,
}

/// The club furni: their class names start with this.
const PICKER_PREFIX: &str = "hc";

/// The waiting times offered in the list, in days.
fn presets() -> [(i64, &'static str); 6] {
    [
        (0, n("Straight away")),
        (31, n("After 1 month of HC")),
        (93, n("After 3 months of HC")),
        (186, n("After 6 months of HC")),
        (365, n("After 1 year of HC")),
        (730, n("After 2 years of HC")),
    ]
}

fn days_text(days: i64) -> String {
    match presets().into_iter().find(|(d, _)| *d == days) {
        Some((_, text)) => t(text).to_string(),
        None => tf("After {days} days of HC", &[("days", &days.to_string())]),
    }
}

/// The page the tab works on: the chosen one if it still is a club gift page, else the one the emulator uses (the
/// first without a problem), else the first.
fn chosen_page(s: &State) -> Option<i64> {
    let pages = catalog_admin::club_gift_pages(&s.pages);
    if let Some(id) = s.gifts_tab.page.filter(|id| pages.iter().any(|p| p.id == *id)) {
        return Some(id);
    }
    pages.iter().find(|p| catalog_admin::club_gift_page_problem(p).is_none()).or(pages.first()).map(|p| p.id)
}

fn problem_text(problem: &str) -> &'static str {
    match problem {
        "hidden" => t("It is hidden (not visible)."),
        "disabled" => t("It is disabled."),
        _ => t("Only staff can open it (lowest rank 2 or more)."),
    }
}

impl Ctl {
    /// The pages again (a page may have been made or changed elsewhere), then the gifts of the chosen page with their
    /// furni.
    pub(super) async fn load_gifts(&self) {
        self.load_pages().await;
        let Some((generation, session, database)) = self.session() else { return };
        let Some(page) = chosen_page(&self.lock()) else {
            self.with(generation, |s| {
                s.gifts_tab.page = None;
                s.gifts_tab.gifts = Some(Vec::new());
            });
            return;
        };
        self.with(generation, |s| s.busy = true);
        let offers = catalog_admin::load_offers(&session, &database, Catalogue::Normal, page).await;
        let furni = match &offers {
            Ok(offers) => {
                let mut ids: Vec<i64> = offers.iter().flat_map(|o| catalog_admin::item_ids(&o.item_ids).unwrap_or_default()).collect();
                ids.sort_unstable();
                ids.dedup();
                catalog_admin::furni_by_ids(&session, &database, &ids).await.unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        self.with(generation, |s| {
            s.busy = false;
            match offers {
                Ok(mut offers) => {
                    offers.sort_by_key(|o| (o.order_number, o.id));
                    let gifts = offers
                        .into_iter()
                        .map(|offer| {
                            let ids = catalog_admin::item_ids(&offer.item_ids).unwrap_or_default();
                            let furni = ids.iter().filter_map(|id| furni.iter().find(|f| f.id == *id).cloned()).collect();
                            Gift { saved: offer.clone(), offer, furni }
                        })
                        .collect();
                    s.gifts_tab.page = Some(page);
                    s.gifts_tab.gifts = Some(gifts);
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Makes the club gift page (top level, visible, every rank).
    pub(super) async fn make_gift_page(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let page = catalog_admin::new_club_gift_page(&self.lock().pages);
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_page(&session, &database, Catalogue::Normal, &page).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(id) => {
                    s.unpublished = true;
                    s.gifts_tab.page = Some(id);
                    s.status = Status::ok(t("The Club gifts page is made. Add the gifts, then Publish.").into());
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_gifts().await;
    }

    /// Makes the page one the emulator uses: visible, enabled, every rank.
    pub(super) async fn fix_gift_page(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(mut page) = self.lock().pages.iter().find(|p| p.id == id).cloned() else { return };
        page.visible = true;
        page.enabled = true;
        page.min_rank = page.min_rank.min(1);
        self.with(generation, |s| s.busy = true);
        let saved = catalog_admin::save_page(&session, &database, Catalogue::Normal, &page).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(t("The page is visible to every member now. Publish to put it in the hotel.").into());
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_gifts().await;
    }

    pub(super) async fn search_gift_furni(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let text = self.lock().gifts_tab.search.clone();
        if text.trim().chars().count() < 2 && text.trim().parse::<i64>().is_err() {
            self.with(generation, |s| s.status = Status::error(t("Type at least two letters of the furni's name, or its id.").into()));
            return;
        }
        self.with(generation, |s| s.busy = true);
        let found = catalog_admin::search_furni(&session, &database, &text).await;
        self.with(generation, |s| {
            s.busy = false;
            match found {
                Ok(found) => s.gifts_tab.results = Some(found),
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
    }

    /// Adds the furni as a gift at the end of the list: free, straight away, with the VIP mark.
    /// Adds the furni as gifts at the end of the list, in this order: free, straight away, with the VIP mark.
    pub(super) async fn add_gifts(self, list: Vec<Furni>) {
        let Some((generation, session, database)) = self.session() else { return };
        let (page, mut gifts) = {
            let s = self.lock();
            (s.gifts_tab.page, s.gifts_tab.gifts.as_ref().map(|g| g.iter().map(|g| g.saved.clone()).collect::<Vec<_>>()).unwrap_or_default())
        };
        let Some(page) = page else { return };
        if list.is_empty() {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let mut added = Vec::new();
        let mut failed = None;
        for furni in &list {
            // Each new gift goes after the last one and gets a name none of the others has.
            let mut gift = catalog_admin::new_club_gift(furni, page, &gifts);
            if let Some(problem) = catalog_admin::offer_problem(&gift) {
                failed = Some(t(&problem).to_string());
                break;
            }
            match catalog_admin::save_offer(&session, &database, Catalogue::Normal, &gift).await {
                Ok(id) => {
                    gift.id = id;
                    gifts.push(gift);
                    added.push(if furni.public_name.trim().is_empty() { furni.item_name.clone() } else { furni.public_name.trim().to_string() });
                }
                Err(error) => {
                    failed = Some(error.to_string());
                    break;
                }
            }
        }
        self.with(generation, |s| {
            s.busy = false;
            if !added.is_empty() {
                s.unpublished = true;
            }
            s.status = match (failed, added.len()) {
                (Some(error), 0) => Status::error(error),
                (Some(error), n) => Status::error(tf("{count} gifts added, then: {error}", &[("count", &n.to_string()), ("error", &error)])),
                (None, 1) => Status::ok(tf("{name} is a club gift now. Publish to put it in the hotel.", &[("name", &added[0])])),
                (None, n) => Status::ok(tf("{count} club gifts added. Publish to put them in the hotel.", &[("count", &n.to_string())])),
            };
        });
        self.load_gifts().await;
    }

    /// Opens the club furni window and reads the furni into it.
    pub(super) async fn open_gift_picker(self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.gifts_tab.picker = Some(Picker::default()));
        let found = catalog_admin::furni_with_prefix(&session, &database, PICKER_PREFIX).await;
        self.with(generation, |s| match found {
            Ok(found) => {
                if let Some(picker) = s.gifts_tab.picker.as_mut() {
                    picker.furni = Some(found);
                }
            }
            Err(error) => {
                s.gifts_tab.picker = None;
                s.status = Status::error(error.to_string());
            }
        });
    }

    pub(super) async fn remove_gift(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(gift) = self.lock().gifts_tab.gifts.as_ref().and_then(|g| g.iter().find(|g| g.offer.id == id).cloned()) else { return };
        let options = vec![Choice::new(t("Remove"), t("Members cannot choose it any more."), true).primary(), Choice::new(t("Cancel"), t("Keep the gift."), false)];
        let message = t("Members who already chose it keep their furni. The History tab can undo it.");
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Remove the club gift {name}?", &[("name", &gift.title())]), message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = catalog_admin::delete_offer(&session, &database, Catalogue::Normal, id).await;
        self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    s.unpublished = true;
                    s.status = Status::ok(tf("{name} is no longer a club gift. Publish to put it in the hotel.", &[("name", &gift.title())]));
                }
                Err(error) => s.status = Status::error(error.to_string()),
            }
        });
        self.load_gifts().await;
    }

    /// Saves the gifts whose days, VIP mark or place changed.
    pub(super) async fn save_gifts(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let changed: Vec<Offer> = self.lock().gifts_tab.gifts.as_ref().map(|g| g.iter().filter(|g| g.changed()).map(|g| g.offer.clone()).collect()).unwrap_or_default();
        if changed.is_empty() {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let mut failed = None;
        for offer in &changed {
            if let Err(error) = catalog_admin::save_offer(&session, &database, Catalogue::Normal, offer).await {
                failed = Some(error.to_string());
                break;
            }
        }
        self.with(generation, |s| {
            s.busy = false;
            s.unpublished = true;
            s.status = match failed {
                Some(error) => Status::error(error),
                None => Status::ok(tf("{count} club gifts saved. Publish to put them in the hotel.", &[("count", &changed.len().to_string())])),
            };
        });
        self.load_gifts().await;
    }
}

/// The furni's icon from this computer's gamedata, or an empty square of the same size.
fn icon(ui: &mut egui::Ui, palette: &Palette, images: &mut FrontImages, furni: Option<&Furni>) {
    icon_sized(ui, palette, images, furni, 40.0);
}

fn icon_sized(ui: &mut egui::Ui, palette: &Palette, images: &mut FrontImages, furni: Option<&Furni>, side: f32) {
    let size = egui::vec2(side, side);
    let texture = furni.and_then(|f| images.furni_icon(&f.item_name));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 6.0, palette.surface);
    if let Some(texture) = texture {
        let scale = (size.x / texture.size_vec2().x).min(size.y / texture.size_vec2().y).min(3.0);
        let shown = egui::Rect::from_center_size(rect.center(), texture.size_vec2() * scale);
        ui.painter().image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
}

pub(super) fn show(ui: &mut egui::Ui, palette: &Palette, s: &mut State, images: &mut FrontImages, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Club gifts"), 17.0));
        ui.add_space(8.0);
        if ui.add_enabled(ready, small(t("Reload"))).clicked() {
            actions.push(Action::LoadGifts);
        }
    });
    widgets::muted(
        ui,
        palette,
        t("The free gifts Habbo Club members choose in the catalogue. A member gets one gift for every started 31 days of HC (all their HC time, earlier memberships too), minus the gifts already taken. A gift can ask for more HC days first: until then it stays greyed out in the game."),
        12.5,
    );
    widgets::muted(ui, palette, t("To try it: give a player Habbo Club on the Players page; a new member can choose one gift straight away. Publish after every change."), 12.0);
    ui.add_space(8.0);

    // The page.
    let pages: Vec<catalog_admin::Page> = catalog_admin::club_gift_pages(&s.pages).into_iter().cloned().collect();
    let Some(page_id) = chosen_page(s) else {
        widgets::card_frame(palette).inner_margin(14).corner_radius(10).show(ui, |ui| {
            ui.label(RichText::new(t("There is no Club gifts page yet.")).size(14.0).strong());
            widgets::muted(ui, palette, t("The gifts live on a catalogue page with the layout club_gift. Make it here: it goes at the top of the catalogue, and you can move it in the Catalogue explorer."), 12.5);
            ui.add_space(4.0);
            if ui.add_enabled(ready, widgets::primary_button(t("Make the Club gifts page"))).clicked() {
                actions.push(Action::MakeGiftPage);
            }
        });
        return;
    };
    let page = pages.iter().find(|p| p.id == page_id).cloned().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("Page")).size(12.5).color(palette.muted));
        let caption = |p: &catalog_admin::Page| format!("{} (#{})", if p.caption.trim().is_empty() { t("No name") } else { p.caption.trim() }, p.id);
        if pages.len() > 1 {
            egui::ComboBox::from_id_salt("club-gift-page").selected_text(caption(&page)).width(260.0).show_ui(ui, |ui| {
                for p in &pages {
                    if ui.selectable_label(p.id == page_id, caption(p)).clicked() && p.id != page_id {
                        s.gifts_tab.page = Some(p.id);
                        actions.push(Action::LoadGifts);
                    }
                }
            });
        } else {
            ui.label(RichText::new(caption(&page)).size(13.0).strong());
        }
    });
    let usable = pages.iter().filter(|p| catalog_admin::club_gift_page_problem(p).is_none()).count();
    if let Some(problem) = catalog_admin::club_gift_page_problem(&page) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tf("The emulator does not use this page: {problem}", &[("problem", problem_text(problem))])).size(12.5).color(palette.warning_text));
            if ui.add_enabled(ready, small(t("Show it to every member"))).clicked() {
                actions.push(Action::FixGiftPage(page.id));
            }
        });
    }
    if usable > 1 {
        widgets::muted(ui, palette, &tf("There are {count} club gift pages: the emulator uses only one of them. Keep one, and move the gifts there.", &[("count", &usable.to_string())]), 12.0);
    }
    ui.add_space(8.0);

    // The gifts.
    let Some(gifts) = s.gifts_tab.gifts.as_mut() else {
        widgets::muted(ui, palette, t("Loading ..."), 12.5);
        return;
    };
    ui.label(RichText::new(tf("Gifts ({count})", &[("count", &gifts.len().to_string())])).size(14.0).strong());
    if gifts.is_empty() {
        widgets::muted(ui, palette, t("No gifts yet: find a furni below and press Add."), 12.5);
    }
    let count = gifts.len();
    let mut swap = None;
    let mut remove = None;
    for (index, gift) in gifts.iter_mut().enumerate() {
        widgets::card_frame(palette).inner_margin(8).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width().min(860.0));
            ui.horizontal(|ui| {
                icon(ui, palette, images, gift.furni.first());
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(gift.title()).size(13.5).strong());
                        if gift.changed() {
                            ui.label(RichText::new(t("changed")).size(11.5).color(palette.accent_text));
                        }
                    });
                    let item = gift.furni.iter().map(|f| f.item_name.as_str()).collect::<Vec<_>>().join(", ");
                    widgets::muted(ui, palette, &format!("{} · {}", if item.is_empty() { gift.offer.item_ids.as_str() } else { item.as_str() }, gift.offer.catalog_name), 11.5);
                });
                let mut days = gift.days();
                ui.vertical(|ui| {
                    ui.label(RichText::new(t("Can be chosen")).size(11.5).color(palette.muted));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("club-gift-days", gift.offer.id)).selected_text(days_text(days)).width(190.0).show_ui(ui, |ui| {
                            for (preset, text) in presets() {
                                ui.selectable_value(&mut days, preset, t(text));
                            }
                        });
                        ui.add_sized([86.0, 28.0], egui::DragValue::new(&mut days).range(0..=catalog_admin::MAX_CLUB_GIFT_DAYS).speed(1).suffix(t(" days")))
                            .on_hover_text(t("The HC days a member needs first, in whole days (31 is one month)."));
                    });
                });
                if days != gift.days() || catalog_admin::club_gift_days(&gift.offer.extradata) < 0 {
                    gift.offer.extradata = days.to_string();
                }
                ui.checkbox(&mut gift.offer.club_only, t("VIP mark")).on_hover_text(t("Shows the HC mark beside the gift in the game."));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(ready, small(t("Remove"))).clicked() {
                        remove = Some(gift.offer.id);
                    }
                    if ui.add_enabled(index + 1 < count, small("↓")).on_hover_text(t("Lower in the list")).clicked() {
                        swap = Some((index, index + 1));
                    }
                    if ui.add_enabled(index > 0, small("↑")).on_hover_text(t("Higher in the list")).clicked() {
                        swap = Some((index - 1, index));
                    }
                });
            });
        });
        ui.add_space(4.0);
    }
    if let Some((a, b)) = swap {
        gifts.swap(a, b);
        // The list's order is the gifts' order: 1, 2, 3 ...
        for (index, gift) in gifts.iter_mut().enumerate() {
            gift.offer.order_number = index as i64 + 1;
        }
    }
    if let Some(id) = remove {
        actions.push(Action::RemoveGift(id));
    }
    let changed = gifts.iter().filter(|g| g.changed()).count();
    if changed > 0 {
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, widgets::primary_button(&tf("Save the changes ({count})", &[("count", &changed.to_string())]))).clicked() {
                actions.push(Action::SaveGifts);
            }
            if ui.add_enabled(ready, small(t("Undo the changes"))).clicked() {
                for gift in gifts.iter_mut() {
                    gift.offer = gift.saved.clone();
                }
                gifts.sort_by_key(|g| (g.offer.order_number, g.offer.id));
            }
        });
    }
    let taken: Vec<String> = gifts.iter().map(|g| g.offer.item_ids.clone()).collect();

    // Adding.
    ui.add_space(14.0);
    ui.label(RichText::new(t("Add a gift")).size(14.0).strong());
    widgets::muted(ui, palette, t("Find the furni by its name, class name or items_base id. A gift is free, can be chosen straight away and has the VIP mark; change that above after adding."), 12.0);
    ui.horizontal(|ui| {
        let response = ui.add_sized([300.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.gifts_tab.search).hint_text(t("For example: throne, hc_chr or 4501")));
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(ready, small(t("Search"))).clicked() || enter) && ready {
            actions.push(Action::SearchGiftFurni);
        }
        ui.add_space(8.0);
        if ui.add_enabled(ready, widgets::primary_button(t("Choose HC furni ..."))).on_hover_text(t("Every furni whose class name starts with hc, as pictures: tick the ones you want and add them at once.")).clicked() {
            actions.push(Action::OpenGiftPicker);
        }
    });
    picker(ui.ctx(), palette, s, images, &taken, actions);
    let Some(results) = s.gifts_tab.results.as_ref() else { return };
    if results.is_empty() {
        widgets::muted(ui, palette, t("No furni found."), 12.5);
        return;
    }
    ui.add_space(4.0);
    for furni in results {
        ui.horizontal(|ui| {
            icon(ui, palette, images, Some(furni));
            ui.vertical(|ui| {
                ui.set_width(330.0);
                let name = if furni.public_name.trim().is_empty() { furni.item_name.as_str() } else { furni.public_name.trim() };
                ui.label(RichText::new(name).size(13.0));
                widgets::muted(ui, palette, &format!("{} · #{}", furni.item_name, furni.id), 11.5);
            });
            let already = taken.iter().any(|ids| catalog_admin::item_ids(ids).unwrap_or_default().contains(&furni.id));
            if already {
                widgets::muted(ui, palette, t("Already a gift"), 12.0);
            } else if ui.add_enabled(ready, small(t("Add"))).clicked() {
                actions.push(Action::AddGifts(vec![furni.clone()]));
            }
        });
    }
}

/// The club furni window: a grid of every `hc` furni (hc_chr, hc2_..., hcsohva ...) with its icon and name; a click ticks one, and **Add** makes the
/// ticked ones gifts. Furni that already is a gift is shown, greyed out.
fn picker(ctx: &egui::Context, palette: &Palette, s: &mut State, images: &mut FrontImages, taken: &[String], actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(picker) = s.gifts_tab.picker.as_mut() else { return };
    let is_gift = |id: i64| taken.iter().any(|ids| catalog_admin::item_ids(ids).unwrap_or_default().contains(&id));
    let mut open = true;
    let mut close = false;
    egui::Window::new(t("Choose HC furni"))
        .id(egui::Id::new("club-gift-picker"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let Some(list) = picker.furni.as_ref() else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    widgets::muted(ui, palette, t("Reading the HC furni ..."), 12.5);
                });
                return;
            };
            ui.horizontal(|ui| {
                ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut picker.filter).hint_text(t("Filter by name")).vertical_align(egui::Align::Center));
                widgets::muted(ui, palette, &tf("{count} furni start with hc", &[("count", &list.len().to_string())]), 12.0);
            });
            let filter = picker.filter.trim().to_lowercase();
            let shown: Vec<&Furni> = list
                .iter()
                .filter(|f| filter.is_empty() || f.item_name.to_lowercase().contains(&filter) || f.public_name.to_lowercase().contains(&filter) || f.id.to_string() == filter)
                .collect();
            ui.horizontal(|ui| {
                let free: Vec<i64> = shown.iter().filter(|f| !is_gift(f.id)).map(|f| f.id).collect();
                if ui.add_enabled(!free.is_empty(), small(t("Tick all shown"))).clicked() {
                    picker.ticked.extend(free);
                }
                if ui.add_enabled(!picker.ticked.is_empty(), small(t("Untick all"))).clicked() {
                    picker.ticked.clear();
                }
            });
            ui.add_space(4.0);
            // A fixed height: the list scrolls inside a window of about 560 x 440, whatever the screen.
            egui::ScrollArea::vertical().max_height(290.0).min_scrolled_height(290.0).auto_shrink([false, false]).show(ui, |ui| {
                if shown.is_empty() {
                    widgets::muted(ui, palette, t("No furni found."), 12.5);
                    return;
                }
                let tile = egui::vec2(100.0, 106.0);
                let gap = 6.0;
                let per_row = (((ui.available_width() + gap) / (tile.x + gap)).floor() as usize).max(1);
                for row in shown.chunks(per_row) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for furni in row {
                            let gift = is_gift(furni.id);
                            let ticked = picker.ticked.contains(&furni.id);
                            let (rect, response) = ui.allocate_exact_size(tile, if gift { egui::Sense::hover() } else { egui::Sense::click() });
                            let (fill, stroke) = if ticked {
                                (palette.accent_text.gamma_multiply(0.18), egui::Stroke::new(2.0_f32, palette.accent_text))
                            } else if response.hovered() && !gift {
                                (palette.surface, egui::Stroke::new(1.0_f32, palette.accent_text.gamma_multiply(0.6)))
                            } else {
                                (palette.surface, egui::Stroke::new(1.0_f32, palette.card_border))
                            };
                            ui.painter().rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
                            let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)).layout(egui::Layout::top_down(egui::Align::Center)));
                            if gift {
                                inner.set_opacity(0.45);
                            }
                            icon_sized(&mut inner, palette, images, Some(furni), 56.0);
                            let name = if furni.public_name.trim().is_empty() { furni.item_name.as_str() } else { furni.public_name.trim() };
                            inner.add(egui::Label::new(RichText::new(name).size(11.5)).truncate());
                            inner.add(egui::Label::new(RichText::new(if gift { t("Already a gift").to_string() } else { furni.item_name.clone() }).size(10.5).color(palette.muted)).truncate());
                            if ticked {
                                ui.painter().text(rect.right_top() + egui::vec2(-8.0, 6.0), egui::Align2::RIGHT_TOP, "✔", egui::FontId::proportional(15.0), palette.accent_text);
                            }
                            let response = response.on_hover_text(format!("{name}\n{} · #{}", furni.item_name, furni.id));
                            if response.clicked() && !gift {
                                if ticked {
                                    picker.ticked.remove(&furni.id);
                                } else {
                                    picker.ticked.insert(furni.id);
                                }
                            }
                        }
                    });
                    ui.add_space(gap);
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let count = picker.ticked.len();
                let label = if count == 1 { t("Add 1 gift").to_string() } else { tf("Add {count} gifts", &[("count", &count.to_string())]) };
                if ui.add_enabled(ready && count > 0, widgets::primary_button(&label)).clicked() {
                    let chosen: Vec<Furni> = list.iter().filter(|f| picker.ticked.contains(&f.id)).cloned().collect();
                    actions.push(Action::AddGifts(chosen));
                    close = true;
                }
                if ui.add(small(t("Cancel"))).clicked() {
                    close = true;
                }
                widgets::muted(ui, palette, t("Each gift is free, can be chosen straight away and has the VIP mark."), 12.0);
            });
        });
    if !open || close {
        s.gifts_tab.picker = None;
    }
}
