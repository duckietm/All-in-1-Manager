//! Colours and fonts of the window in its two looks: "Hotel" (the hotel's blues, outlined cards,
//! green go buttons, pixel titles; the default) and "Professional" (the neutral indigo look).
//! The fixed brand/sidebar/console colours and the light/dark tokens (`Pro.*` in ProfessionalTheme.cs)
//! follow the look chosen in Settings → Look (`Look` in configuration/settings.json, read once at startup).

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, FontFamily};

/// The look: Hotel unless settings.json says `"Look": "professional"`.
static HOTEL: AtomicBool = AtomicBool::new(true);

pub fn hotel() -> bool {
    HOTEL.load(Ordering::Relaxed)
}

pub fn set_hotel(hotel: bool) {
    HOTEL.store(hotel, Ordering::Relaxed);
}

pub fn look_name(hotel: bool) -> &'static str {
    if hotel { crate::i18n::n("hotel") } else { crate::i18n::n("professional") }
}

/// settings.json `Look`.
pub fn init(settings: &aio_core::InstallerSettings) {
    set_hotel(!settings.is_professional_look());
}

pub fn hex(value: &str) -> Color32 {
    let v = value.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&v[i..i + 2], 16).unwrap_or(0);
    match v.len() {
        8 => Color32::from_rgba_unmultiplied(byte(2), byte(4), byte(6), byte(0)),
        _ => Color32::from_rgb(byte(0), byte(2), byte(4)),
    }
}

pub fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn pick(hotel_value: &str, professional: &str) -> Color32 {
    hex(if hotel() { hotel_value } else { professional })
}

pub struct Fixed;

impl Fixed {
    pub fn brand() -> Color32 { pick("#0E86C4", "#4F5FE6") }
    pub fn hotel_yellow() -> Color32 { hex("#FFCD38") }
    pub fn sidebar() -> Color32 { pick("#0E3A52", "#0E1424") }
    pub fn sidebar_hover() -> Color32 { pick("#155072", "#1A2338") }
    pub fn sidebar_selected() -> Color32 { pick("#1C6690", "#232F52") }
    pub fn sidebar_text() -> Color32 { pick("#DCEEF8", "#C9D2E3") }
    pub fn sidebar_muted() -> Color32 { pick("#8DB6CB", "#7283A6") }
    pub fn sidebar_accent() -> Color32 { pick("#FFCD38", "#8B9BFF") }
    pub fn console_background() -> Color32 { pick("#0B1620", "#0B101C") }
    pub fn console_text() -> Color32 { pick("#D3E2EC", "#D3DAE6") }
    pub fn console_border() -> Color32 { pick("#1F3646", "#1F293D") }
    /// The hotel's green "go" button, with its darker bottom edge.
    pub fn go() -> Color32 { hex("#3FA535") }
    pub fn go_edge() -> Color32 { hex("#23661C") }
    pub fn outline() -> Color32 { hex("#0F2D3F") }
}

/// Card corners and edge width of the look.
pub fn card_corners() -> u8 {
    if hotel() { 8 } else { 12 }
}

pub fn card_edge() -> f32 {
    if hotel() { 2.0 } else { 1.0 }
}

/// The font of page and card titles: Pixelify Sans in the Hotel look.
pub fn title_family() -> FontFamily {
    if hotel() { FontFamily::Name("pixel".into()) } else { FontFamily::Proportional }
}

/// Pixelify Sans in the Hotel look, the bold UI font in the other: for titles painted by hand.
pub fn title_family_or_bold() -> FontFamily {
    if hotel() { FontFamily::Name("pixel".into()) } else { FontFamily::Name("bold".into()) }
}

/// The light/dark tokens.
#[derive(Clone, Copy)]
pub struct Palette {
    pub surface: Color32,
    pub card: Color32,
    pub card_border: Color32,
    pub card_border_strong: Color32,
    /// The dark edge around cards in the Hotel look (the card border in the Professional look).
    pub outline: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub subtle: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub warning_back: Color32,
    pub warning_text: Color32,
    pub success_back: Color32,
    pub success_text: Color32,
    pub error_back: Color32,
    pub error_text: Color32,
    pub info_back: Color32,
    pub info_text: Color32,
}

impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        if hotel() { Self::hotel(dark) } else { Self::professional(dark) }
    }

    fn hotel(dark: bool) -> Self {
        let p = |light: &str, dark_value: &str| hex(if dark { dark_value } else { light });
        Self {
            surface: p("#DCEFF8", "#0A1824"),
            card: p("#FFFFFF", "#12293A"),
            card_border: p("#B9D3E1", "#25445A"),
            card_border_strong: p("#6E93A8", "#3A6680"),
            outline: p("#1D4257", "#04101A"),
            text: p("#10232F", "#E6F1F8"),
            muted: p("#46606F", "#9DB9CA"),
            subtle: p("#7891A0", "#6A889B"),
            hover: p("#E9F5FB", "#183448"),
            selected: p("#FFF4CC", "#3A3312"),
            accent_text: p("#0A6FA3", "#7FCBF2"),
            accent_soft: p("#E2F3FB", "#173A52"),
            warning_back: p("#FFF1C2", "#3A2E0A"),
            warning_text: p("#8A5300", "#FFCD38"),
            success_back: p("#DDF5D6", "#123A1A"),
            success_text: p("#2B7A21", "#7EDB6E"),
            error_back: p("#FFE0DC", "#3D1614"),
            error_text: p("#B3261E", "#FF8A80"),
            info_back: p("#DDF0FA", "#13324A"),
            info_text: p("#0A6FA3", "#7FCBF2"),
        }
    }

    fn professional(dark: bool) -> Self {
        let p = |light: &str, dark_value: &str| hex(if dark { dark_value } else { light });
        Self {
            surface: p("#F4F6FA", "#0D121C"),
            card: p("#FFFFFF", "#151B28"),
            card_border: p("#E2E7EF", "#232C3E"),
            card_border_strong: p("#C9D1DE", "#34405A"),
            outline: p("#E2E7EF", "#232C3E"),
            text: p("#0F172A", "#E7EBF3"),
            muted: p("#5E6B80", "#97A3B8"),
            subtle: p("#8C97AA", "#66738C"),
            hover: p("#EFF2F7", "#1C2434"),
            selected: p("#ECEFFF", "#1E2550"),
            accent_text: p("#4352D4", "#A9B4FF"),
            accent_soft: p("#ECEFFF", "#1E2550"),
            warning_back: p("#FEF3C7", "#3A2C0C"),
            warning_text: p("#8A4B0B", "#F5C451"),
            success_back: p("#DCFCE7", "#11301F"),
            success_text: p("#15803D", "#5BD68A"),
            error_back: p("#FEE2E2", "#3A1717"),
            error_text: p("#B42318", "#F78A80"),
            info_back: p("#E6F0FF", "#152340"),
            info_text: p("#1D4ED8", "#8AB4FF"),
        }
    }

    pub fn current(ctx: &egui::Context) -> Self {
        Self::for_dark(ctx.style().visuals.dark_mode)
    }

    /// The card edge: the dark outline in the Hotel look, the light border otherwise.
    pub fn card_stroke(&self) -> egui::Stroke {
        egui::Stroke::new(card_edge(), if hotel() { self.outline } else { self.card_border })
    }
}

/// Ubuntu (body text) and Pixelify Sans (titles) for the Hotel look, both bundled with their
/// licences in assets/fonts. The Professional look keeps egui's own fonts.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.into(), egui::FontData::from_static(bytes).into());
    };
    add("ubuntu", include_bytes!("../../assets/fonts/Ubuntu-Regular.ttf"));
    add("ubuntu-bold", include_bytes!("../../assets/fonts/Ubuntu-Bold.ttf"));
    add("pixelify", include_bytes!("../../assets/fonts/PixelifySans.ttf"));
    // egui's fonts stay behind as fallbacks for emoji and symbols.
    let proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let with_fallbacks = |first: &str| std::iter::once(first.to_string()).chain(proportional.iter().cloned()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Name("pixel".into()), with_fallbacks("pixelify"));
    fonts.families.insert(FontFamily::Name("bold".into()), with_fallbacks("ubuntu-bold"));
    if hotel() {
        fonts.families.insert(FontFamily::Proportional, with_fallbacks("ubuntu"));
    }
    // Arrows (→ ← ↑ ↓ ⇄), checks and box symbols are in none of the fonts above: the symbol part of DejaVu Sans fills
    // them in for every family.
    add("symbols", include_bytes!("../../assets/fonts/DejaVuSans-Symbols.ttf"));
    for family in fonts.families.values_mut() {
        family.push("symbols".into());
    }
    ctx.set_fonts(fonts);
}

/// Text, widget and selection colours for the current light/dark mode and look.
pub fn apply(ctx: &egui::Context) {
    let dark = ctx.style().visuals.dark_mode;
    let palette = Palette::for_dark(dark);
    let hotel = hotel();
    ctx.style_mut(|style| {
        let visuals = &mut style.visuals;
        visuals.panel_fill = palette.surface;
        visuals.window_fill = palette.card;
        visuals.window_stroke = palette.card_stroke();
        visuals.extreme_bg_color = match (hotel, dark) {
            (true, true) => hex("#0D2030"),
            (false, true) => hex("#0F1522"),
            _ => hex("#FFFFFF"),
        };
        visuals.override_text_color = Some(palette.text);
        visuals.selection.bg_fill = with_alpha(Fixed::brand(), 90);
        visuals.hyperlink_color = palette.accent_text;
        let corners = egui::CornerRadius::same(if hotel { 6 } else { 8 });
        visuals.widgets.inactive.corner_radius = corners;
        visuals.widgets.hovered.corner_radius = corners;
        visuals.widgets.active.corner_radius = corners;
        let width = if hotel { 1.5_f32 } else { 1.0 };
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(width, palette.card_border_strong);
        visuals.widgets.inactive.weak_bg_fill = palette.card;
        visuals.widgets.hovered.weak_bg_fill = palette.hover;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(width, if hotel { palette.outline } else { palette.card_border_strong });
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}
