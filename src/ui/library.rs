//! Left panel: media library, audio, text presets, filters and animations.

use crate::app::{EditorApp, LeftTab};
use crate::model::{FilterPreset, Id, MediaItem, MediaKind, TrackKind};
use crate::render::text;
use crate::ui::{theme, widgets};
use egui::{
    pos2, vec2, Align2, Button, Color32, Context, FontId, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke, Ui,
};
use std::path::PathBuf;

pub fn show(ctx: &Context, app: &mut EditorApp) {
    egui::SidePanel::left("library")
        .default_width(340.0)
        .width_range(280.0..=560.0)
        .resizable(true)
        .frame(Frame::none().fill(theme::PANEL).inner_margin(Margin::same(10.0)))
        .show(ctx, |ui| {
            tabs(ui, app);
            ui.add_space(4.0);
            ui.separator();
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match app.left_tab {
                LeftTab::Media => media_tab(ui, app, false),
                LeftTab::Audio => media_tab(ui, app, true),
                LeftTab::Text => text_tab(ui, app),
                LeftTab::Filters => filters_tab(ui, app),
                LeftTab::Animation => animation_tab(ui, app),
                LeftTab::Captions => crate::ui::captions::tab(ui, app),
            });
        });
}

fn tabs(ui: &mut Ui, app: &mut EditorApp) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let w = ((ui.available_width() - 10.0) / 6.0).floor();
        for (tab, icon, label) in [
            (LeftTab::Media, "🎬", "Media"),
            (LeftTab::Audio, "🎵", "Audio"),
            (LeftTab::Text, "T", "Text"),
            (LeftTab::Filters, "🎨", "Filters"),
            (LeftTab::Animation, "✨", "Animate"),
            (LeftTab::Captions, "CC", "Captions"),
        ] {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 48.0), Sense::click());
            let selected = app.left_tab == tab;
            let color = if selected {
                theme::ACCENT
            } else if resp.hovered() {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            };
            if selected || resp.hovered() {
                ui.painter().rect_filled(rect, 6.0, theme::PANEL_ALT);
            }
            ui.painter().text(rect.center() - vec2(0.0, 8.0), Align2::CENTER_CENTER, icon, FontId::proportional(18.0), color);
            ui.painter().text(rect.center() + vec2(0.0, 13.0), Align2::CENTER_CENTER, label, FontId::proportional(11.0), color);
            if resp.clicked() {
                app.left_tab = tab;
            }
        }
    });
}

enum CardAction {
    Add(Id),
    Remove(Id),
    Reveal(PathBuf),
}

fn media_tab(ui: &mut Ui, app: &mut EditorApp, audio: bool) {
    if audio {
        crate::ui::smart_audio::panel(ui, app);
        ui.add_space(6.0);
        ui.separator();
        widgets::section_title(ui, "Audio library");
    }
    let label = if audio { "⬆  Import audio" } else { "⬆  Import media" };
    if ui
        .add_sized([ui.available_width(), 38.0], Button::new(RichText::new(label).strong()).fill(theme::SURFACE))
        .clicked()
    {
        app.import_dialog();
    }
    ui.label(widgets::dim("or drag & drop files anywhere on the window").small());
    if app.assets.importing > 0 {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Importing {}…", app.assets.importing));
        });
    }
    ui.add_space(8.0);

    let items: Vec<MediaItem> =
        app.project.media.iter().filter(|m| (m.kind == MediaKind::Audio) == audio).cloned().collect();
    if items.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(if audio { "🎵" } else { "🎬" }).size(36.0).color(theme::TEXT_DIM));
            ui.label(widgets::dim(if audio {
                "No audio yet. Import music or voice-overs."
            } else {
                "No media yet. Import videos and images\nto start editing."
            }));
        });
        return;
    }
    for m in &items {
        app.assets.ensure(m);
    }
    ui.label(widgets::dim("Double-click or drag onto the timeline").small());
    ui.add_space(4.0);

    let avail = ui.available_width();
    let cols = ((avail + 8.0) / 120.0).floor().max(2.0);
    let card_w = ((avail - 8.0 * (cols - 1.0)) / cols).floor();
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for m in &items {
            if let Some(a) = media_card(ui, app, m, card_w) {
                action = Some(a);
            }
        }
    });
    match action {
        Some(CardAction::Add(id)) => app.add_media_to_timeline(id),
        Some(CardAction::Remove(id)) => {
            app.project.remove_media(id);
            app.notify("Removed from project");
        }
        Some(CardAction::Reveal(path)) => reveal_in_folder(&path),
        None => {}
    }
}

fn media_card(ui: &mut Ui, app: &mut EditorApp, m: &MediaItem, w: f32) -> Option<CardAction> {
    let thumb_h = (w * 9.0 / 16.0).round();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, thumb_h + 22.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let thumb = Rect::from_min_size(rect.min, vec2(w, thumb_h));
    painter.rect_filled(thumb, 6.0, theme::SURFACE);

    match m.kind {
        MediaKind::Audio => {
            if let Some(peaks) = app.assets.waveforms.get(&m.id) {
                let area = thumb.shrink2(vec2(6.0, 10.0));
                let dur = m.duration.max(0.01);
                widgets::draw_waveform(&painter, area, peaks, theme::ACCENT, |x| {
                    ((x - area.left()) / area.width()) as f64 * dur
                });
            } else {
                painter.text(thumb.center(), Align2::CENTER_CENTER, "🎵", FontId::proportional(26.0), theme::TEXT_DIM);
            }
        }
        _ => {
            if let Some(tex) = app.assets.thumbs.get(&m.id) {
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
            } else {
                let icon = if m.kind == MediaKind::Image { "🖼" } else { "🎬" };
                painter.text(thumb.center(), Align2::CENTER_CENTER, icon, FontId::proportional(24.0), theme::TEXT_DIM);
            }
        }
    }

    if m.kind != MediaKind::Image {
        let text = widgets::format_short(m.duration);
        let g = painter.layout_no_wrap(text, FontId::proportional(10.5), Color32::WHITE);
        let r = Rect::from_min_size(thumb.right_bottom() - g.size() - vec2(10.0, 6.0), g.size() + vec2(6.0, 2.0));
        painter.rect_filled(r, 3.0, Color32::from_black_alpha(170));
        painter.galley(r.min + vec2(3.0, 1.0), g, Color32::WHITE);
    }
    let missing = !m.path.exists();
    if missing {
        painter.text(thumb.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, "missing", FontId::proportional(10.5), theme::DANGER);
    }

    let used = app.project.tracks.iter().flat_map(|t| &t.clips).any(|c| c.media_id() == Some(m.id));
    let name_rect = Rect::from_min_max(pos2(rect.left(), thumb.bottom() + 3.0), rect.right_bottom());
    let name_color = if used { theme::TEXT } else { theme::TEXT_DIM };
    painter
        .with_clip_rect(name_rect)
        .text(name_rect.left_top(), Align2::LEFT_TOP, &m.name, FontId::proportional(11.5), name_color);

    // Hover "+" button.
    let plus = Rect::from_center_size(thumb.right_top() + vec2(-14.0, 14.0), vec2(20.0, 20.0));
    let mut action = None;
    if resp.hovered() {
        painter.circle_filled(plus.center(), 10.0, theme::ACCENT);
        painter.text(plus.center(), Align2::CENTER_CENTER, "+", FontId::proportional(16.0), Color32::BLACK);
    }
    let selected = app.selected_media == Some(m.id);
    if selected || resp.hovered() {
        let c = if selected { theme::ACCENT } else { theme::BORDER };
        painter.rect_stroke(thumb, 6.0, Stroke::new(1.5_f32, c));
    }
    resp.clone().on_hover_text(format!(
        "{}\n{}",
        m.name,
        match m.kind {
            MediaKind::Audio => format!("Audio · {}", widgets::format_time(m.duration)),
            MediaKind::Image => format!("Image · {}×{}", m.width, m.height),
            MediaKind::Video => format!(
                "Video · {}×{} · {:.2} fps · {}{}",
                m.width,
                m.height,
                m.fps,
                widgets::format_time(m.duration),
                if m.has_audio { " · with audio" } else { "" }
            ),
        }
    ));

    if resp.clicked() {
        app.selected_media = Some(m.id);
        if resp.interact_pointer_pos().is_some_and(|p| plus.contains(p)) {
            action = Some(CardAction::Add(m.id));
        }
    }
    if resp.double_clicked() && !missing {
        action = Some(CardAction::Add(m.id));
    }
    if resp.drag_started() && !missing {
        app.drag_media = Some(m.id);
    }
    resp.context_menu(|ui| {
        if ui.add_enabled(!missing, Button::new("➕ Add to timeline")).clicked() {
            action = Some(CardAction::Add(m.id));
            ui.close_menu();
        }
        if ui.button("📂 Show in folder").clicked() {
            action = Some(CardAction::Reveal(m.path.clone()));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(RichText::new("🗑 Remove from project").color(theme::DANGER)).clicked() {
            action = Some(CardAction::Remove(m.id));
            ui.close_menu();
        }
    });
    action
}

fn text_tab(ui: &mut Ui, app: &mut EditorApp) {
    widgets::section_title(ui, "Text styles");
    ui.label(widgets::dim("Click a style to add it at the playhead.").small());
    ui.add_space(6.0);
    let w = ui.available_width();
    for (name, style) in text::presets() {
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 58.0), Sense::click());
        let painter = ui.painter_at(rect);
        let fill = if resp.hovered() { theme::SURFACE_HOVER } else { theme::SURFACE };
        painter.rect_filled(rect, 8.0, fill);
        let size = (style.font_size * 0.3).clamp(15.0, 28.0);
        let font = FontId::new(size, theme::text_family(style.bold));
        let g = painter.layout_no_wrap(name.to_string(), font, widgets::with_alpha(style.color, 1.0));
        let pos = rect.center() - g.size() / 2.0;
        if let Some(bg) = style.background {
            painter.rect_filled(Rect::from_min_size(pos, g.size()).expand(5.0), 3.0, widgets::with_alpha(bg, 1.0));
        }
        if let Some(ol) = style.outline {
            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                painter.galley_with_override_text_color(pos + vec2(dx, dy) * 1.5, g.clone(), widgets::with_alpha(ol, 1.0));
            }
        }
        painter.galley(pos, g, Color32::WHITE);
        if resp.hovered() {
            painter.text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, "+", FontId::proportional(18.0), theme::ACCENT);
        }
        if resp.clicked() {
            app.add_text(style);
        }
        ui.add_space(4.0);
    }
}

/// The selected clip if it is a video or image on a video track.
fn selected_visual(app: &EditorApp) -> Option<(Id, Id, FilterPreset)> {
    let clip = app.selection.and_then(|id| app.project.clip(id))?;
    let media = clip.media_id()?;
    (app.project.track_kind_for(clip) == TrackKind::Video).then_some((clip.id, media, clip.filter))
}

fn filters_tab(ui: &mut Ui, app: &mut EditorApp) {
    widgets::section_title(ui, "Filters");
    let sel = selected_visual(app);
    ui.label(
        widgets::dim(if sel.is_some() {
            "Click a filter to apply it to the selected clip."
        } else {
            "Select a video or image clip on the timeline to apply a filter."
        })
        .small(),
    );
    ui.add_space(6.0);
    let sample_media = sel.map(|s| s.1).or(app.selected_media).or_else(|| {
        app.project.media.iter().find(|m| m.kind != MediaKind::Audio).map(|m| m.id)
    });
    let avail = ui.available_width();
    let card_w = ((avail - 16.0) / 3.0).floor();
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for preset in FilterPreset::ALL {
            let tex = app.assets.filter_thumb(sample_media, preset);
            let th = (card_w * 9.0 / 16.0).round();
            let (rect, resp) = ui.allocate_exact_size(vec2(card_w, th + 20.0), Sense::click());
            let painter = ui.painter_at(rect);
            let img = Rect::from_min_size(rect.min, vec2(card_w, th));
            painter.image(tex, img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let active = sel.is_some_and(|s| s.2 == preset);
            if active || resp.hovered() {
                let c = if active { theme::ACCENT } else { theme::TEXT_DIM };
                painter.rect_stroke(img, 4.0, Stroke::new(2.0_f32, c));
            }
            painter.text(
                pos2(img.center().x, img.bottom() + 10.0),
                Align2::CENTER_CENTER,
                preset.label(),
                FontId::proportional(11.5),
                if active { theme::ACCENT } else { theme::TEXT },
            );
            if resp.clicked() {
                clicked = Some(preset);
            }
        }
    });
    if let Some(preset) = clicked {
        match sel.and_then(|s| app.project.clip_mut(s.0)) {
            Some(clip) => clip.filter = preset,
            None => app.notify("Select a video or image clip first"),
        }
    }
    ui.add_space(10.0);
    ui.label(widgets::dim("Fine-tune brightness, contrast and saturation in the inspector →").small());
}

fn animation_tab(ui: &mut Ui, app: &mut EditorApp) {
    widgets::section_title(ui, "Animations");
    let selected = app.selection.and_then(|id| app.project.clip(id)).cloned();
    ui.label(
        widgets::dim(if selected.is_some() {
            "Applies to the selected clip."
        } else {
            "Select a clip on the timeline first."
        })
        .small(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Duration");
        ui.add(egui::Slider::new(&mut app.anim_len, 0.1..=3.0).suffix(" s").fixed_decimals(1));
    });
    ui.add_space(6.0);
    let len = app.anim_len;
    let w = ui.available_width();
    let mut apply: Option<(Option<f64>, Option<f64>)> = None;
    ui.add_enabled_ui(selected.is_some(), |ui| {
        for (label, desc, fi, fo) in [
            ("Fade in", "Starts from transparent / silent", Some(len), None),
            ("Fade out", "Ends transparent / silent", None, Some(len)),
            ("Fade in & out", "Both ends", Some(len), Some(len)),
            ("Remove animations", "Back to hard cuts", Some(0.0), Some(0.0)),
        ] {
            let resp = ui.add_sized([w, 34.0], Button::new(label).fill(theme::SURFACE)).on_hover_text(desc);
            if resp.clicked() {
                apply = Some((fi, fo));
            }
        }
    });
    if let (Some((fi, fo)), Some(c)) = (apply, &selected) {
        if let Some(clip) = app.project.clip_mut(c.id) {
            if let Some(v) = fi {
                clip.fade_in = v.min(clip.duration / 2.0);
            }
            if let Some(v) = fo {
                clip.fade_out = v.min(clip.duration / 2.0);
            }
        }
    }

    ui.add_space(14.0);
    widgets::section_title(ui, "Motion (keyframes)");
    ui.label(widgets::dim("Adds keyframes you can fine-tune in the inspector.").small());
    ui.add_space(4.0);
    let kind = selected.as_ref().map(|c| app.project.track_kind_for(c));
    let visual = kind.is_some_and(|k| k != crate::model::TrackKind::Audio);
    let is_text = selected.as_ref().is_some_and(|c| c.text().is_some());
    let mut preset = None;
    ui.add_enabled_ui(visual, |ui| {
        for p in crate::model::MotionPreset::ALL {
            let enabled = !(is_text && p.uses_rotation());
            if ui.add_enabled(enabled, Button::new(p.label()).fill(theme::SURFACE).min_size(vec2(w, 32.0))).clicked() {
                preset = Some(p);
            }
        }
    });
    if let (Some(p), Some(c)) = (preset, &selected) {
        if let Some(clip) = app.project.clip_mut(c.id) {
            p.apply(clip);
            app.notify(format!("{} applied — see its keyframes (diamond buttons) in the inspector", p.label()));
        }
    }
    if selected.is_some() && !visual {
        ui.label(widgets::dim("Motion presets apply to video, image and text clips.").small());
    }

    ui.add_space(14.0);
    widgets::section_title(ui, "Transitions");
    ui.label(
        widgets::dim("Applies to the cut after the selected clip (or before it, if it is the last). Uses the duration above.")
            .small(),
    );
    ui.add_space(4.0);
    let target = selected.as_ref().and_then(|c| app.project.next_adjacent(c.id).or_else(|| app.project.previous_adjacent(c.id).map(|_| c.id)));
    let mut pick = None;
    ui.add_enabled_ui(target.is_some(), |ui| {
        let bw = ((w - 8.0) / 2.0).floor();
        egui::Grid::new("transition-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for (i, k) in crate::model::TransitionKind::ALL.into_iter().enumerate() {
                if ui.add(Button::new(k.label()).fill(theme::SURFACE).min_size(vec2(bw, 30.0))).clicked() {
                    pick = Some(Some(k));
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if ui.add(Button::new("Remove transition").min_size(vec2(w, 28.0))).clicked() {
            pick = Some(None);
        }
    });
    if selected.is_some() && target.is_none() {
        ui.label(widgets::dim("Place the clip right next to another clip on a video track first.").small());
    }
    if let (Some(choice), Some(to)) = (pick, target) {
        let tr = choice.map(|kind| crate::model::Transition { kind, duration: len });
        match app.project.set_transition(to, tr) {
            Ok(()) => app.notify(match choice {
                Some(k) => format!("{} added", k.label()),
                None => "Transition removed".to_string(),
            }),
            Err(e) => app.notify_error(e),
        }
    }
}

fn reveal_in_folder(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

pub fn reveal(path: &std::path::Path) {
    reveal_in_folder(path);
}
