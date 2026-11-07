//! "Replace clip" window: swap a clip's media, and that of every other clip using the same
//! media, for another item from the library. Timing, look, keyframes and transitions stay.

use crate::app::EditorApp;
use crate::model::{Id, MediaItem, MediaKind};
use crate::ui::{theme, widgets};
use egui::{pos2, Align2, Button, Color32, Context, FontId, Rect, RichText, Sense, Stroke};

#[derive(Default)]
pub struct ReplaceUi {
    clip: Option<Id>,
    /// Every clip that uses the same media (true) or only this one.
    all: bool,
    pick: Option<Id>,
}

/// Opens the window for `clip` (from the timeline's right-click menu).
pub fn open(app: &mut EditorApp, clip: Id) {
    app.replace = ReplaceUi { clip: Some(clip), all: true, pick: None };
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    let Some(clip_id) = app.replace.clip else { return };
    let Some(clip) = app.project.clip(clip_id).cloned() else {
        app.replace = ReplaceUi::default();
        return;
    };
    let Some(old) = clip.media_id().and_then(|m| app.project.media(m)).cloned() else {
        app.replace = ReplaceUi::default();
        return;
    };
    let uses = app.project.clips_using(old.id);
    let choices: Vec<MediaItem> = app.project.media.iter().filter(|m| app.project.can_replace_with(&clip, m.id)).cloned().collect();
    for m in &choices {
        app.assets.ensure(m);
    }
    let mut open = true;
    let mut apply = None;
    egui::Window::new(RichText::new("🔁 Replace clip").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(500.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("Replace “{}” with an item from the library.", old.name));
            ui.horizontal(|ui| {
                let all = if uses.len() == 1 { "Its only clip".to_string() } else { format!("All {} clips that use it", uses.len()) };
                ui.radio_value(&mut app.replace.all, true, all);
                ui.radio_value(&mut app.replace.all, false, "Only this clip");
            });
            ui.label(widgets::dim("Each clip keeps its place, length, position, effects, keyframes and transition.").small());
            ui.separator();
            if choices.is_empty() {
                let what = if old.kind == MediaKind::Audio { "audio" } else { "video or image" };
                ui.label(widgets::dim(format!("Import another {what} first (Media tab), then try again.")));
                return;
            }
            egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for m in &choices {
                        let picked = app.replace.pick == Some(m.id);
                        let resp = tile(ui, app, m, picked);
                        if resp.double_clicked() {
                            apply = Some(m.id);
                        } else if resp.clicked() {
                            app.replace.pick = Some(m.id);
                        }
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let n = if app.replace.all { uses.len() } else { 1 };
                let label = format!("🔁  Replace {n} clip{}", if n == 1 { "" } else { "s" });
                if ui.add_enabled(app.replace.pick.is_some(), Button::new(RichText::new(label).strong().color(Color32::BLACK)).fill(theme::ACCENT)).on_disabled_hover_text("Pick a library item").clicked() {
                    apply = app.replace.pick;
                }
                ui.label(widgets::dim("Double-click an item to replace right away.").small());
            });
        });
    if let Some(new) = apply {
        let targets = if app.replace.all { uses } else { vec![clip_id] };
        let (n, retimed) = app.project.replace_media(&targets, new);
        let name = app.project.media(new).map(|m| m.name.clone()).unwrap_or_default();
        let fit = if retimed > 0 { format!(" ({retimed} slowed or shortened: “{name}” is shorter than the cut)") } else { String::new() };
        app.notify(format!("🔁 Replaced {n} clip{} with “{name}”{fit}. Ctrl+Z undoes it.", if n == 1 { "" } else { "s" }));
        app.replace = ReplaceUi::default();
        return;
    }
    if !open {
        app.replace = ReplaceUi::default();
    }
}

/// A clickable library item: thumbnail (cover-cropped), kind icon and name.
fn tile(ui: &mut egui::Ui, app: &EditorApp, m: &MediaItem, picked: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(146.0, 104.0), Sense::click());
    let painter = ui.painter_at(rect);
    let thumb = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 80.0));
    painter.rect_filled(thumb, 4.0, theme::SURFACE);
    match app.assets.thumbs.get(&m.id) {
        Some(tex) => {
            let ts = tex.size_vec2();
            let (ta, ra) = (ts.x / ts.y, thumb.width() / thumb.height());
            let uv = if ta > ra {
                let f = ra / ta;
                Rect::from_min_max(pos2((1.0 - f) / 2.0, 0.0), pos2((1.0 + f) / 2.0, 1.0))
            } else {
                let f = ta / ra;
                Rect::from_min_max(pos2(0.0, (1.0 - f) / 2.0), pos2(1.0, (1.0 + f) / 2.0))
            };
            painter.image(tex.id(), thumb.shrink(1.0), uv, Color32::WHITE);
        }
        None => {
            let icon = match m.kind {
                MediaKind::Image => "🖼",
                MediaKind::Audio => "🎵",
                MediaKind::Video => "🎬",
            };
            painter.text(thumb.center(), Align2::CENTER_CENTER, icon, FontId::proportional(24.0), theme::TEXT_DIM);
        }
    }
    if m.source_len().is_some() {
        let d = widgets::format_short(m.duration);
        painter.text(thumb.right_bottom() - egui::vec2(5.0, 4.0), Align2::RIGHT_BOTTOM, d, FontId::proportional(11.0), Color32::WHITE);
    }
    let name: String = if m.name.chars().count() > 22 { format!("{}…", m.name.chars().take(21).collect::<String>()) } else { m.name.clone() };
    painter.text(pos2(rect.left() + 2.0, thumb.bottom() + 4.0), Align2::LEFT_TOP, name, FontId::proportional(12.0), theme::TEXT);
    let stroke = if picked {
        Stroke::new(2.0_f32, theme::ACCENT)
    } else if resp.hovered() {
        Stroke::new(1.0_f32, theme::TEXT_DIM)
    } else {
        Stroke::new(1.0_f32, theme::BORDER)
    };
    painter.rect_stroke(thumb, 4.0, stroke);
    resp.on_hover_text(&m.name)
}
