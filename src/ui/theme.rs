//! Dark, CapCut-inspired look.

use egui::{Color32, FontData, FontDefinitions, FontFamily, Rounding, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(15, 15, 17);
pub const PANEL: Color32 = Color32::from_rgb(25, 25, 29);
pub const PANEL_ALT: Color32 = Color32::from_rgb(33, 33, 38);
pub const SURFACE: Color32 = Color32::from_rgb(43, 43, 50);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(56, 56, 65);
pub const BORDER: Color32 = Color32::from_rgb(50, 50, 58);
pub const ACCENT: Color32 = Color32::from_rgb(0, 205, 222);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0, 110, 122);
pub const TEXT: Color32 = Color32::from_rgb(232, 232, 238);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 152);
pub const DANGER: Color32 = Color32::from_rgb(240, 84, 84);

pub const CLIP_VIDEO: Color32 = Color32::from_rgb(28, 92, 104);
pub const CLIP_IMAGE: Color32 = Color32::from_rgb(78, 62, 132);
pub const CLIP_TEXT: Color32 = Color32::from_rgb(168, 104, 34);
pub const CLIP_AUDIO: Color32 = Color32::from_rgb(30, 104, 70);

/// Font families used for on-canvas text (same TTF files the exporter uses).
pub const FONT_TEXT: &str = "rc-text";
pub const FONT_TEXT_BOLD: &str = "rc-text-bold";

pub fn text_family(bold: bool) -> FontFamily {
    FontFamily::Name(if bold { FONT_TEXT_BOLD } else { FONT_TEXT }.into())
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (family, bold) in [(FONT_TEXT, false), (FONT_TEXT_BOLD, true)] {
        let mut list = Vec::new();
        if let Some(bytes) = crate::render::text::font_path(bold).and_then(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert(family.to_owned(), FontData::from_owned(bytes));
            list.push(family.to_owned());
        }
        list.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(family.into()), list);
    }
    ctx.set_fonts(fonts);

    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL_ALT;
    v.extreme_bg_color = BG;
    v.faint_bg_color = PANEL_ALT;
    v.code_bg_color = SURFACE;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_rounding = Rounding::same(10.0);
    v.menu_rounding = Rounding::same(8.0);
    v.slider_trailing_fill = true;

    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.bg_fill = SURFACE_HOVER;
    v.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_DIM);
    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.open.weak_bg_fill = SURFACE_HOVER;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = Rounding::same(6.0);
    }

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.slider_width = 140.0;
    style.spacing.interact_size.y = 24.0;
    ctx.set_style(style);
}
