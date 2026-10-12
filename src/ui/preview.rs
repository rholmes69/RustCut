//! Centre panel: composited canvas preview with on-canvas manipulation and transport.

use crate::app::EditorApp;
use crate::model::{ActiveTransition, Clip, Id, MediaItem, Prop, TrackKind, TransitionKind};
use crate::render::{layer_size, text};
use crate::ui::{theme, widgets};
use egui::epaint::{Mesh, Vertex};
use egui::{
    pos2, vec2, Align, Align2, Button, Color32, Context, FontId, Frame, Layout, Margin, Pos2, Rect, RichText, Sense,
    Shape, Stroke, TextureId, Ui, Vec2,
};

#[derive(Default)]
pub struct PreviewUi {
    /// Clip being dragged on the canvas and its un-snapped position.
    drag: Option<(Id, Vec2)>,
    snapped: (bool, bool),
}

/// A drawn layer, used for hit-testing and the selection outline.
struct Hit {
    id: Id,
    center: Pos2,
    half: Vec2,
    angle: f32,
}

impl Hit {
    fn contains(&self, p: Pos2) -> bool {
        let (s, c) = (-self.angle).sin_cos();
        let d = p - self.center;
        let local = vec2(d.x * c - d.y * s, d.x * s + d.y * c);
        local.x.abs() <= self.half.x && local.y.abs() <= self.half.y
    }

    fn corners(&self) -> [Pos2; 4] {
        let (s, c) = self.angle.sin_cos();
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(sx, sy): (f32, f32)| {
            let (x, y) = (sx * self.half.x, sy * self.half.y);
            self.center + vec2(x * c - y * s, x * s + y * c)
        })
    }
}

pub fn show(ctx: &Context, app: &mut EditorApp) {
    egui::CentralPanel::default().frame(Frame::none().fill(theme::BG)).show(ctx, |ui| {
        egui::TopBottomPanel::bottom("transport")
            .exact_height(46.0)
            .frame(Frame::none().fill(theme::PANEL_ALT).inner_margin(Margin::symmetric(12.0, 6.0)))
            .show_inside(ui, |ui| transport(ui, app));
        egui::CentralPanel::default()
            .frame(Frame::none().fill(theme::BG))
            .show_inside(ui, |ui| canvas(ui, app));
    });
}

fn transport(ui: &mut Ui, app: &mut EditorApp) {
    let duration = app.project.duration();
    let rect = ui.max_rect();
    ui.horizontal_centered(|ui| {
        ui.label(RichText::new(widgets::format_time(app.playhead)).monospace().color(theme::ACCENT));
        ui.label(RichText::new(format!("/ {}", widgets::format_time(duration))).monospace().color(theme::TEXT_DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (w, h) = app.project.aspect.size_for(1080);
            ui.label(widgets::dim(format!("{} · {}×{} · {} fps", app.project.aspect.short_label(), w, h, app.project.fps)));
        });
    });

    // Transport buttons, centred in the bar.
    let (bw, gap) = (34.0, 4.0);
    let mut x = rect.center().x - (5.0 * bw + 4.0 * gap) / 2.0;
    let mut slot = || {
        let r = Rect::from_min_size(pos2(x, rect.center().y - 15.0), vec2(bw, 30.0));
        x += bw + gap;
        r
    };
    let big = |s: &str| Button::new(RichText::new(s).size(16.0)).frame(false);
    let play_icon = if app.playing { "⏸" } else { "▶" };
    let play = Button::new(RichText::new(play_icon).size(17.0).color(Color32::BLACK)).fill(theme::ACCENT).rounding(15.0);
    if ui.put(slot(), big("⏮")).on_hover_text("Go to start (Home)").clicked() {
        app.seek(0.0);
    }
    if ui.put(slot(), big("⏪")).on_hover_text("Previous frame (←)").clicked() {
        app.seek(app.playhead - app.frame_dt());
    }
    if ui.put(slot(), play).on_hover_text("Play / pause (Space)").clicked() {
        app.toggle_play();
    }
    if ui.put(slot(), big("⏩")).on_hover_text("Next frame (→)").clicked() {
        app.seek(app.playhead + app.frame_dt());
    }
    if ui.put(slot(), big("⏭")).on_hover_text("Go to end (End)").clicked() {
        app.seek(duration);
    }
}

/// Draws a media layer with its blur (offset soft copies) and glow (bright wash).
/// An approximation of the export's Gaussian blur and eq glow, good enough to judge timing.
fn draw_layer(painter: &egui::Painter, tex: TextureId, hit: &Hit, alpha: f32, blur_px: f32, glow: f32) {
    if blur_px < 0.5 {
        draw_quad(painter, tex, hit, alpha);
    } else {
        let taps = [(0.0f32, 0.0f32), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7), (-0.7, -0.7)];
        // Copy k drawn at 1/(k+1) over the previous ones = the running average of all copies.
        for (k, (dx, dy)) in taps.iter().enumerate() {
            let shifted = Hit { center: hit.center + vec2(*dx, *dy) * blur_px, ..*hit };
            draw_quad(painter, tex, &shifted, if k == 0 { alpha } else { alpha / (k as f32 + 1.0) });
        }
    }
    if glow > 0.01 {
        let corners = hit.corners();
        let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
        let wash = Color32::from_white_alpha((glow.clamp(0.0, 1.0) * alpha * 0.30 * 255.0) as u8);
        let mut mesh = Mesh::with_texture(tex);
        for i in 0..4 {
            mesh.vertices.push(Vertex { pos: corners[i], uv: uvs[i], color: wash });
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
    }
}

fn draw_quad(painter: &egui::Painter, tex: TextureId, hit: &Hit, alpha: f32) {
    let corners = hit.corners();
    let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
    let color = Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8);
    let mut mesh = Mesh::with_texture(tex);
    for i in 0..4 {
        mesh.vertices.push(Vertex { pos: corners[i], uv: uvs[i], color });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// Where a video clip's layer sits on the canvas at time `t`.
fn video_hit(clip: &Clip, media: &MediaItem, canvas: Rect, t: f64) -> Hit {
    let tf = clip.transform_at(t);
    let (w, h) = layer_size(media.width as f64, media.height as f64, canvas.width() as f64, canvas.height() as f64, tf.scale as f64);
    Hit {
        id: clip.id,
        center: canvas.center() + vec2(tf.x * canvas.width(), tf.y * canvas.height()),
        half: vec2(w as f32, h as f32) / 2.0,
        angle: tf.rotation.to_radians(),
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let k = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// One side of a transition: texture (if decoded yet), placement and opacity.
struct Side {
    tex: Option<TextureId>,
    hit: Hit,
    alpha: f32,
}

/// Draws a transition between two layers, mirroring FFmpeg's `xfade` (measured shapes).
fn draw_transition(cp: &egui::Painter, canvas: Rect, kind: TransitionKind, p: f32, a: &Side, b: &Side) {
    let draw = |painter: &egui::Painter, side: &Side, offset: egui::Vec2, alpha: f32| {
        if let Some(tex) = side.tex {
            let hit = Hit { center: side.hit.center + offset, ..side.hit };
            draw_quad(painter, tex, &hit, side.alpha * alpha);
        }
    };
    let (w, h) = (canvas.width(), canvas.height());
    let zero = egui::Vec2::ZERO;
    use TransitionKind as K;
    match kind {
        K::Dissolve => {
            draw(cp, a, zero, 1.0);
            draw(cp, b, zero, p);
        }
        K::DipBlack | K::DipWhite => {
            let color = if kind == K::DipBlack { Color32::BLACK } else { Color32::WHITE };
            draw(cp, a, zero, 1.0);
            cp.rect_filled(canvas, 0.0, color.gamma_multiply((p / 0.15).min(1.0)));
            draw(cp, b, zero, smoothstep(0.15, 1.0, p));
        }
        K::SlideLeft => {
            draw(cp, a, vec2(-p * w, 0.0), 1.0);
            draw(cp, b, vec2((1.0 - p) * w, 0.0), 1.0);
        }
        K::SlideRight => {
            draw(cp, a, vec2(p * w, 0.0), 1.0);
            draw(cp, b, vec2(-(1.0 - p) * w, 0.0), 1.0);
        }
        K::SlideUp => {
            draw(cp, a, vec2(0.0, -p * h), 1.0);
            draw(cp, b, vec2(0.0, (1.0 - p) * h), 1.0);
        }
        K::SlideDown => {
            draw(cp, a, vec2(0.0, p * h), 1.0);
            draw(cp, b, vec2(0.0, -(1.0 - p) * h), 1.0);
        }
        K::WipeLeft | K::WipeRight | K::WipeUp | K::WipeDown => {
            draw(cp, a, zero, 1.0);
            let r = canvas;
            let reveal = match kind {
                K::WipeLeft => Rect::from_min_max(egui::pos2(r.right() - p * w, r.top()), r.max),
                K::WipeRight => Rect::from_min_max(r.min, egui::pos2(r.left() + p * w, r.bottom())),
                K::WipeUp => Rect::from_min_max(egui::pos2(r.left(), r.bottom() - p * h), r.max),
                _ => Rect::from_min_max(r.min, egui::pos2(r.right(), r.top() + p * h)),
            };
            draw(&cp.with_clip_rect(reveal), b, zero, 1.0);
        }
        K::ZoomIn => {
            let zoom = 1.0 / smoothstep(0.5, 1.0, 1.0 - p).max(0.04);
            if let Some(tex) = a.tex {
                let c = canvas.center();
                let hit = Hit { center: c + (a.hit.center - c) * zoom, half: a.hit.half * zoom, ..a.hit };
                draw_quad(cp, tex, &hit, a.alpha);
            }
            draw(cp, b, zero, smoothstep(0.5, 1.0, p));
        }
    }
}

fn canvas(ui: &mut Ui, app: &mut EditorApp) {
    let view = ui.available_rect_before_wrap();
    let (cw, ch) = app.project.aspect.size_for(1080);
    let avail = view.shrink(18.0);
    let s = (avail.width() / cw as f32).min(avail.height() / ch as f32).max(0.01);
    let canvas = Rect::from_center_size(avail.center(), vec2(cw as f32 * s, ch as f32 * s));
    let resp = ui.allocate_rect(view, Sense::hover());
    let painter = ui.painter_at(view);
    painter.rect_filled(canvas, 0.0, Color32::BLACK);

    let t = app.playhead;
    let textures = app.preview.update(&app.project, t, app.playing);
    let cp = painter.with_clip_rect(canvas);
    let short = canvas.width().min(canvas.height()) as f64;
    let mut hits: Vec<Hit> = Vec::new();

    for (ti, track) in app.project.tracks.iter().enumerate().rev() {
        if track.hidden {
            continue;
        }
        let active: Vec<ActiveTransition> =
            app.project.track_transitions(ti).into_iter().filter(|tr| tr.contains(t)).collect();
        let in_transition = |id: Id| active.iter().any(|tr| tr.from == id || tr.to == id);
        for tr in &active {
            let side = |id: Id| -> Option<Side> {
                let clip = app.project.clip(id)?;
                let media = app.project.media(clip.media_id()?)?;
                let alpha = clip.opacity_at(t);
                Some(Side { tex: textures.get(&id).copied(), hit: video_hit(clip, media, canvas, t), alpha })
            };
            if let (Some(a), Some(b)) = (side(tr.from), side(tr.to)) {
                draw_transition(&cp, canvas, tr.kind, tr.progress(t), &a, &b);
                hits.push(if tr.progress(t) < 0.5 { a.hit } else { b.hit });
            }
        }
        for clip in track.clips.iter().filter(|c| c.is_active(t) && !in_transition(c.id)) {
            let alpha = clip.opacity_at(t) * clip.fade_gain(t);
            let tf = clip.transform_at(t);
            let center = canvas.center() + vec2(tf.x * canvas.width(), tf.y * canvas.height());
            match track.kind {
                TrackKind::Video => {
                    let Some(media) = clip.media_id().and_then(|m| app.project.media(m)) else { continue };
                    let (w, h) = layer_size(
                        media.width as f64,
                        media.height as f64,
                        canvas.width() as f64,
                        canvas.height() as f64,
                        tf.scale as f64,
                    );
                    let hit = Hit {
                        id: clip.id,
                        center,
                        half: vec2(w as f32, h as f32) / 2.0,
                        angle: tf.rotation.to_radians(),
                    };
                    if let (Some(tex), Some(plate)) = (
                        textures.get(&crate::playback::plate_key(clip.id)),
                        clip.background_plate.and_then(|m| app.project.media(m)),
                    ) {
                        let (pw, ph) = (plate.width.max(1) as f64, plate.height.max(1) as f64);
                        let s = crate::render::fill_scale(pw, ph, canvas.width() as f64, canvas.height() as f64);
                        let (w, h) = layer_size(pw, ph, canvas.width() as f64, canvas.height() as f64, s);
                        let plate_hit = Hit { id: clip.id, center: canvas.center(), half: vec2(w as f32, h as f32) / 2.0, angle: 0.0 };
                        draw_quad(&cp, *tex, &plate_hit, alpha);
                    }
                    if let Some(tex) = textures.get(&clip.id) {
                        let px_scale = canvas.height().min(canvas.width()) / 1080.0;
                        draw_layer(&cp, *tex, &hit, alpha, clip.value_at(Prop::Blur, t) * px_scale, clip.value_at(Prop::Glow, t));
                    }
                    hits.push(hit);
                }
                TrackKind::Text => {
                    let Some(style) = clip.text() else { continue };
                    let px = text::font_px(style.font_size, tf.scale, short) as f32;
                    let font = FontId::new(px.max(1.0), theme::text_family(style.bold));
                    let galley = cp.layout_no_wrap(style.content.clone(), font.clone(), Color32::WHITE);
                    let size = galley.size();
                    let mut pos = center - size / 2.0;
                    let mut center = center;
                    if let Some(full) = style.align_to.as_ref().filter(|f| !f.is_empty()) {
                        // Start where the full line would start if it were centred.
                        let full_w = cp.layout_no_wrap(full.clone(), font, Color32::WHITE).size().x;
                        pos.x = center.x - full_w / 2.0;
                        center.x = pos.x + size.x / 2.0;
                    }
                    if let Some(bg) = style.background {
                        let pad = text::box_padding(px as f64) as f32;
                        cp.rect_filled(Rect::from_min_size(pos, size).expand(pad), 0.0, widgets::with_alpha(bg, alpha));
                    }
                    if let Some(ol) = style.outline {
                        let w = text::outline_width(px as f64) as f32;
                        let col = widgets::with_alpha(ol, alpha);
                        for (dx, dy) in [
                            (-1.0, 0.0),
                            (1.0, 0.0),
                            (0.0, -1.0),
                            (0.0, 1.0),
                            (-0.7, -0.7),
                            (0.7, -0.7),
                            (-0.7, 0.7),
                            (0.7, 0.7),
                        ] {
                            cp.galley_with_override_text_color(pos + vec2(dx, dy) * w, galley.clone(), col);
                        }
                    }
                    cp.galley_with_override_text_color(pos, galley, widgets::with_alpha(style.color, alpha));
                    hits.push(Hit { id: clip.id, center, half: size / 2.0 + vec2(4.0, 4.0), angle: 0.0 });
                }
                TrackKind::Audio => {}
            }
        }
    }

    if app.project.clip_count() == 0 {
        painter.text(
            canvas.center() - vec2(0.0, 12.0),
            Align2::CENTER_CENTER,
            "🎬",
            FontId::proportional(40.0),
            theme::TEXT_DIM,
        );
        painter.text(
            canvas.center() + vec2(0.0, 26.0),
            Align2::CENTER_CENTER,
            "Import media and add it to the timeline",
            FontId::proportional(15.0),
            theme::TEXT_DIM,
        );
    }

    // Selection outline.
    if let Some(hit) = app.selection.and_then(|id| hits.iter().find(|h| h.id == id)) {
        let c = hit.corners();
        let stroke = Stroke::new(1.5_f32, theme::ACCENT);
        painter.add(Shape::closed_line(c.to_vec(), stroke));
        for p in c {
            painter.circle(p, 4.0, Color32::WHITE, stroke);
        }
    }

    // Centre guides while snapped.
    let guide = Stroke::new(1.0_f32, theme::ACCENT.gamma_multiply(0.8));
    if app.preview_ui.drag.is_some() {
        if app.preview_ui.snapped.0 {
            painter.line_segment([pos2(canvas.center().x, canvas.top()), pos2(canvas.center().x, canvas.bottom())], guide);
        }
        if app.preview_ui.snapped.1 {
            painter.line_segment([pos2(canvas.left(), canvas.center().y), pos2(canvas.right(), canvas.center().y)], guide);
        }
    }

    // Interaction: click to select, drag to move, scroll to scale.
    let interact = ui.interact(canvas, ui.id().with("canvas"), Sense::click_and_drag());
    let topmost = |p: Pos2| hits.iter().rev().find(|h| h.contains(p)).map(|h| h.id);
    if interact.clicked() {
        app.selection = interact.interact_pointer_pos().and_then(topmost);
    }
    if interact.drag_started() {
        if let Some(p) = interact.interact_pointer_pos() {
            // Prefer the selected clip if the press is on it.
            let on_selected = app.selection.and_then(|id| hits.iter().find(|h| h.id == id)).is_some_and(|h| h.contains(p));
            let target = if on_selected { app.selection } else { topmost(p) };
            if let Some(id) = target {
                if let Some(c) = app.project.clip(id) {
                    let tf = c.transform_at(t);
                    app.preview_ui.drag = Some((id, vec2(tf.x, tf.y)));
                    app.selection = Some(id);
                }
            }
        }
    }
    if interact.dragged() {
        if let Some((id, raw)) = &mut app.preview_ui.drag {
            let d = interact.drag_delta();
            raw.x += d.x / canvas.width();
            raw.y += d.y / canvas.height();
            let snap = |v: f32| if v.abs() < 0.012 { (0.0, true) } else { (v, false) };
            let (x, sx) = snap(raw.x);
            let (y, sy) = snap(raw.y);
            app.preview_ui.snapped = (sx, sy);
            if let Some(c) = app.project.clip_mut(*id) {
                // Animated position gets a keyframe at the playhead.
                c.set_value_at(Prop::X, t, x);
                c.set_value_at(Prop::Y, t, y);
            }
        }
    }
    if interact.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
        app.preview_ui.drag = None;
    }
    if interact.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            if let Some(id) = app.selection.filter(|id| hits.iter().any(|h| h.id == *id)) {
                if let Some(c) = app.project.clip_mut(id) {
                    let s = (c.value_at(Prop::Scale, t) * (1.0 + scroll * 0.0015)).clamp(0.05, 5.0);
                    c.set_value_at(Prop::Scale, t, s);
                }
            }
        }
        if hits.iter().any(|h| interact.hover_pos().is_some_and(|p| h.contains(p))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    let _ = resp;
}
