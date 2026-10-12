//! Bottom panel: toolbar, ruler, track headers and the clip lanes.

use crate::app::EditorApp;
use crate::media::assets::AssetManager;
use crate::model::{Clip, Id, MediaItem, MediaKind, Project, Prop, TrackKind, Transition, TransitionKind, MIN_CLIP_LEN};
use crate::ui::{theme, widgets};
use egui::{
    pos2, vec2, Align, Align2, Button, Color32, Context, CursorIcon, FontId, Frame, Id as EguiId, Layout, Margin,
    Painter, Rect, RichText, Sense, Shape, Slider, Stroke, Ui,
};

const HEADER_W: f32 = 150.0;
const RULER_H: f32 = 26.0;
const TRACK_GAP: f32 = 5.0;
const HANDLE_W: f32 = 8.0;
const MIN_PPS: f32 = 2.0;
const MAX_PPS: f32 = 600.0;
const SNAP_PX: f64 = 9.0;

/// Actions in a clip's right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipMenu {
    Split,
    Duplicate,
    Delete,
    Extend,
    Reframe,
    Plate,
    Beats,
    LipSync,
}

fn track_height(kind: TrackKind) -> f32 {
    match kind {
        TrackKind::Video => 62.0,
        TrackKind::Text => 32.0,
        TrackKind::Audio => 46.0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragMode {
    Move,
    TrimStart,
    TrimEnd,
}

#[derive(Clone, Debug)]
pub struct ClipDrag {
    pub id: Id,
    pub mode: DragMode,
    pub orig: Clip,
    grab_dt: f64,
    prev_end: f64,
    next_start: f64,
    max_dur: Option<f64>,
    finite_source: bool,
}

pub struct TimelineState {
    /// Pixels per second.
    pub pps: f32,
    pub scroll_x: f64,
    pub scroll_y: f32,
    pub snap: bool,
    pub drag: Option<ClipDrag>,
    snap_guide: Option<f64>,
    body_width: f32,
    /// Scroll so this time is visible on the next frame.
    pub reveal: Option<f64>,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self { pps: 60.0, scroll_x: 0.0, scroll_y: 0.0, snap: true, drag: None, snap_guide: None, body_width: 800.0, reveal: None }
    }
}

pub fn show(ctx: &Context, app: &mut EditorApp) {
    egui::TopBottomPanel::bottom("timeline")
        .resizable(true)
        .default_height(300.0)
        .height_range(180.0..=640.0)
        .frame(Frame::none().fill(theme::PANEL))
        .show(ctx, |ui| {
            Frame::none().inner_margin(Margin::symmetric(10.0, 5.0)).show(ui, |ui| toolbar(ui, app));
            lanes(ui, app);
        });
}

fn toolbar(ui: &mut Ui, app: &mut EditorApp) {
    ui.horizontal(|ui| {
        let has_sel = app.selection.is_some();
        if ui.button("✂  Split").on_hover_text("Split at playhead (Ctrl+B)").clicked() {
            app.split_at_playhead();
        }
        if ui.add_enabled(has_sel, Button::new("📋  Duplicate")).on_hover_text("Ctrl+D").clicked() {
            app.duplicate_selection();
        }
        if ui.add_enabled(has_sel, Button::new("🗑  Delete")).on_hover_text("Delete").clicked() {
            app.delete_selection();
        }
        ui.separator();
        if ui.button("T+  Text").on_hover_text("Add text at playhead").clicked() {
            app.add_text(Default::default());
        }
        ui.separator();
        let snap_label = RichText::new("🔗  Snap").color(if app.timeline.snap { theme::ACCENT } else { theme::TEXT_DIM });
        if ui.selectable_label(app.timeline.snap, snap_label).on_hover_text("Snap to clips and playhead").clicked() {
            app.timeline.snap = !app.timeline.snap;
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Fit").on_hover_text("Fit the whole project").clicked() {
                let d = app.project.duration().max(1.0);
                app.timeline.pps = ((app.timeline.body_width - 60.0) / d as f32).clamp(MIN_PPS, MAX_PPS);
                app.timeline.scroll_x = 0.0;
            }
            if ui.small_button("+").clicked() {
                app.timeline.pps = (app.timeline.pps * 1.25).min(MAX_PPS);
            }
            ui.add(Slider::new(&mut app.timeline.pps, MIN_PPS..=MAX_PPS).logarithmic(true).show_value(false));
            if ui.small_button("-").clicked() {
                app.timeline.pps = (app.timeline.pps / 1.25).max(MIN_PPS);
            }
            ui.label(widgets::dim("Zoom"));
        });
    });
}

/// Picks a ruler step so labels are at least ~80 px apart.
fn ruler_step(pps: f64) -> f64 {
    const STEPS: [f64; 14] = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0];
    STEPS.iter().copied().find(|s| s * pps >= 80.0).unwrap_or(3600.0)
}

/// Best snap offset for any of `edges` towards `points` within `threshold`.
fn best_snap(edges: &[f64], points: &[f64], threshold: f64) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64)> = None;
    for &e in edges {
        for &p in points {
            let d = p - e;
            if d.abs() <= threshold && best.map_or(true, |(bd, _)| d.abs() < bd.abs()) {
                best = Some((d, p));
            }
        }
    }
    best
}

fn clip_color(clip: &Clip, media: Option<&MediaItem>, kind: TrackKind) -> Color32 {
    if clip.text().is_some() {
        theme::CLIP_TEXT
    } else if kind == TrackKind::Audio {
        theme::CLIP_AUDIO
    } else if media.is_some_and(|m| m.kind == MediaKind::Image) {
        theme::CLIP_IMAGE
    } else {
        theme::CLIP_VIDEO
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_clip(
    p: &Painter,
    rect: Rect,
    visible: Rect,
    clip: &Clip,
    media: Option<&MediaItem>,
    kind: TrackKind,
    assets: &AssetManager,
    pps: f64,
    selected: bool,
) {
    let base = clip_color(clip, media, kind);
    p.rect_filled(rect, 5.0, base);
    let inner = rect.shrink(1.5);
    let area = inner.intersect(visible);
    if area.width() <= 0.0 {
        return;
    }
    let cp = p.with_clip_rect(area);
    let mid = clip.media_id();

    match (kind, mid) {
        (TrackKind::Video, Some(mid)) => {
            if let Some(tex) = assets.thumbs.get(&mid) {
                let ts = tex.size_vec2();
                let tw = (inner.height() * ts.x / ts.y).max(8.0);
                let first = ((area.left() - inner.left()) / tw).floor().max(0.0);
                let mut x = inner.left() + first * tw;
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                while x < area.right() {
                    cp.image(tex.id(), Rect::from_min_size(pos2(x, inner.top()), vec2(tw, inner.height())), uv, Color32::from_gray(210));
                    x += tw;
                }
            }
            if media.is_some_and(|m| m.has_sound()) && (clip.volume > 0.001 || clip.is_animated(Prop::Volume)) {
                // A strip of waveform along the bottom, like CapCut.
                if let Some(peaks) = assets.waveforms.get(&mid) {
                    let strip = Rect::from_min_max(pos2(area.left(), inner.bottom() - 14.0), area.right_bottom());
                    cp.rect_filled(strip, 0.0, Color32::from_black_alpha(120));
                    widgets::draw_waveform(&cp, strip, peaks, Color32::from_rgb(120, 230, 200), |x| {
                        clip.source_in + ((x - rect.left()) as f64 / pps) * clip.speed
                    });
                }
            }
        }
        (TrackKind::Audio, Some(mid)) => {
            if let Some(peaks) = assets.waveforms.get(&mid) {
                widgets::draw_waveform(&cp, area.shrink2(vec2(0.0, 4.0)), peaks, Color32::from_rgb(140, 240, 180), |x| {
                    clip.source_in + ((x - rect.left()) as f64 / pps) * clip.speed
                });
            }
        }
        _ => {}
    }

    // Label pinned to the visible left edge.
    let name = match (clip.text(), media) {
        (Some(t), _) => format!("T  {}", t.content.lines().next().unwrap_or("")),
        (_, Some(m)) => m.name.clone(),
        _ => "Missing media".into(),
    };
    let mut label = format!("{name}  {}", widgets::format_short(clip.duration));
    if (clip.speed - 1.0).abs() > 1e-3 {
        label = format!("{:.2}×  {label}", clip.speed);
    }
    if clip.volume <= 0.001 && !clip.is_animated(Prop::Volume) && kind != TrackKind::Text {
        label = format!("🔇 {label}");
    }
    let g = cp.layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
    let lp = pos2(area.left().max(inner.left()) + 3.0, inner.top() + 2.0);
    if kind == TrackKind::Text {
        cp.galley(pos2(lp.x + 2.0, inner.center().y - g.size().y / 2.0), g, Color32::WHITE);
    } else {
        cp.rect_filled(Rect::from_min_size(lp, g.size() + vec2(8.0, 2.0)), 3.0, Color32::from_black_alpha(150));
        cp.galley(lp + vec2(4.0, 1.0), g, Color32::WHITE);
    }

    // Fade ramps.
    let (fi, fo) = clip.fades();
    let ramp = Stroke::new(1.5_f32, Color32::from_white_alpha(170));
    if fi > 0.0 {
        let x = inner.left() + (fi * pps) as f32;
        cp.add(Shape::convex_polygon(
            vec![inner.left_top(), pos2(x, inner.top()), inner.left_bottom()],
            Color32::from_black_alpha(90),
            Stroke::NONE,
        ));
        cp.line_segment([inner.left_bottom(), pos2(x, inner.top())], ramp);
    }
    if fo > 0.0 {
        let x = inner.right() - (fo * pps) as f32;
        cp.add(Shape::convex_polygon(
            vec![pos2(x, inner.top()), inner.right_top(), inner.right_bottom()],
            Color32::from_black_alpha(90),
            Stroke::NONE,
        ));
        cp.line_segment([pos2(x, inner.top()), inner.right_bottom()], ramp);
    }

    // Volume envelope (e.g. auto-ducking) as a line over the clip.
    if let Some(keys) = clip.keyframes.get(&Prop::Volume).filter(|k| k.len() > 1) {
        let top = inner.top() + 4.0;
        let span = (inner.height() - 8.0).max(1.0);
        let y_of = |v: f32| top + span * (1.0 - (v / 1.25).clamp(0.0, 1.0));
        let mut pts = Vec::new();
        let mut x = area.left();
        while x <= area.right() {
            let local = ((x - inner.left()) as f64 / pps).clamp(0.0, clip.duration);
            pts.push(pos2(x, y_of(crate::model::interpolate(keys, local))));
            x += 3.0;
        }
        cp.add(Shape::line(pts, Stroke::new(1.5_f32, Color32::from_rgb(255, 210, 90))));
    }

    // Keyframe diamonds along the bottom edge (volume keys too, unless there are many).
    let many_volume_keys = clip.keyframes.get(&Prop::Volume).is_some_and(|k| k.len() > 12);
    if clip.has_keyframes() {
        let y = inner.bottom() - 6.0;
        let times: Vec<f64> = if many_volume_keys {
            let mut c = clip.clone();
            c.keyframes.remove(&Prop::Volume);
            c.key_times()
        } else {
            clip.key_times()
        };
        for local in times {
            let x = inner.left() + (local * pps) as f32;
            if x >= inner.left() - 1.0 && x <= inner.right() + 1.0 {
                cp.add(diamond(pos2(x, y), 4.5, Color32::WHITE, Stroke::new(1.0_f32, Color32::from_black_alpha(160))));
            }
        }
    }

    let vp = p.with_clip_rect(visible);
    if selected {
        vp.rect_stroke(rect, 5.0, Stroke::new(2.0_f32, Color32::WHITE));
        for x in [rect.left() + 3.0, rect.right() - 3.0] {
            let h = Rect::from_center_size(pos2(x, rect.center().y), vec2(5.0, (rect.height() * 0.5).min(22.0)));
            vp.rect_filled(h, 2.0, Color32::WHITE);
        }
    } else {
        vp.rect_stroke(rect, 5.0, Stroke::new(1.0_f32, Color32::from_black_alpha(140)));
    }
}

/// A diamond-shaped keyframe marker.
pub fn diamond(c: egui::Pos2, r: f32, fill: Color32, stroke: Stroke) -> Shape {
    Shape::convex_polygon(vec![pos2(c.x, c.y - r), pos2(c.x + r, c.y), pos2(c.x, c.y + r), pos2(c.x - r, c.y)], fill, stroke)
}

fn header_toggle(ui: &mut Ui, painter: &Painter, rect: Rect, id: EguiId, icon: &str, active: bool, tip: &str) -> bool {
    let resp = ui.interact(rect, id, Sense::click()).on_hover_text(tip);
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, theme::SURFACE_HOVER);
    }
    let color = if active { theme::ACCENT } else { theme::TEXT_DIM };
    painter.text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(12.5), color);
    resp.clicked()
}

fn lanes(ui: &mut Ui, app: &mut EditorApp) {
    let ctx = ui.ctx().clone();
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);

    let header = Rect::from_min_max(rect.min, pos2(rect.min.x + HEADER_W, rect.max.y));
    let lanes_rect = Rect::from_min_max(pos2(header.max.x, rect.min.y), rect.max);
    let ruler = Rect::from_min_max(lanes_rect.min, pos2(lanes_rect.max.x, lanes_rect.min.y + RULER_H));
    let body = Rect::from_min_max(pos2(lanes_rect.min.x, ruler.max.y), lanes_rect.max);
    let header_body = Rect::from_min_max(pos2(header.min.x, ruler.max.y), header.max);
    app.timeline.body_width = body.width();

    // Layout of tracks (before scrolling is clamped).
    let content_h: f32 = app.project.tracks.iter().map(|t| track_height(t.kind) + TRACK_GAP).sum::<f32>() + 40.0;
    let max_scroll_y = (content_h - body.height()).max(0.0);

    // Zoom & scroll.
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    if pointer.is_some_and(|p| rect.contains(p)) {
        let (zoom, scroll) = ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        let tl = &mut app.timeline;
        if zoom != 1.0 {
            let px = pointer.map_or(body.left(), |p| p.x.max(body.left()));
            let t_at = tl.scroll_x + ((px - body.left()) / tl.pps) as f64;
            tl.pps = (tl.pps * zoom).clamp(MIN_PPS, MAX_PPS);
            tl.scroll_x = t_at - ((px - body.left()) / tl.pps) as f64;
        } else {
            tl.scroll_x -= (scroll.x / tl.pps) as f64;
            if max_scroll_y > 0.0 && pointer.is_some_and(|p| p.x < body.left() || p.y > ruler.bottom()) {
                tl.scroll_y -= scroll.y;
            } else {
                tl.scroll_x -= (scroll.y / tl.pps) as f64;
            }
        }
    }
    // Keep the playhead in view while playing.
    if app.playing {
        let tl = &mut app.timeline;
        let view_secs = (body.width() / tl.pps) as f64;
        if app.playhead > tl.scroll_x + view_secs * 0.92 || app.playhead < tl.scroll_x {
            tl.scroll_x = app.playhead - view_secs * 0.1;
        }
    }
    if let Some(t) = app.timeline.reveal.take() {
        let tl = &mut app.timeline;
        let view_secs = (body.width() / tl.pps) as f64;
        if t < tl.scroll_x || t > tl.scroll_x + view_secs * 0.85 {
            tl.scroll_x = t - view_secs * 0.1;
        }
    }
    app.timeline.scroll_x = app.timeline.scroll_x.max(0.0);
    app.timeline.scroll_y = app.timeline.scroll_y.clamp(0.0, max_scroll_y);

    let (pps, sx) = (app.timeline.pps as f64, app.timeline.scroll_x);
    let t2x = move |t: f64| body.left() + ((t - sx) * pps) as f32;
    let x2t = move |x: f32| sx + (x - body.left()) as f64 / pps;

    let mut lane_rows: Vec<(usize, f32, f32)> = Vec::new();
    let mut y = body.top() + 6.0 - app.timeline.scroll_y;
    for (i, t) in app.project.tracks.iter().enumerate() {
        let h = track_height(t.kind);
        lane_rows.push((i, y, y + h));
        y += h + TRACK_GAP;
    }

    // Background click: deselect + seek. Created first so clips get priority.
    let bg = ui.interact(body, ui.id().with("tl-bg"), Sense::click());
    if bg.clicked() {
        if let Some(p) = bg.interact_pointer_pos() {
            app.selection = None;
            app.seek(x2t(p.x).max(0.0));
        }
    }

    // Lane backgrounds.
    let body_painter = painter.with_clip_rect(body);
    let main_id = app.project.main_track_id();
    for &(i, y0, y1) in &lane_rows {
        let lane = Rect::from_min_max(pos2(body.left(), y0), pos2(body.right(), y1));
        let is_main = Some(app.project.tracks[i].id) == main_id;
        let fill = if is_main { theme::PANEL_ALT } else { Color32::from_rgb(22, 22, 26) };
        body_painter.rect_filled(lane, 4.0, fill);
    }

    // Clips.
    let mut clicked: Option<Id> = None;
    let mut menu_action: Option<(Id, ClipMenu)> = None;
    let playhead = app.playhead;
    let mut start_drag: Option<(Id, DragMode, f32)> = None;
    let mut hover_cursor: Option<CursorIcon> = None;
    {
        let project = &app.project;
        for &(ti, y0, y1) in &lane_rows {
            let track = &project.tracks[ti];
            for clip in &track.clips {
                let (x0, x1) = (t2x(clip.start), t2x(clip.end()));
                if x1 < body.left() || x0 > body.right() || y1 < body.top() || y0 > body.bottom() {
                    continue;
                }
                let r = Rect::from_min_max(pos2(x0, y0 + 2.0), pos2(x1.max(x0 + 2.0), y1 - 2.0));
                let media = clip.media_id().and_then(|m| project.media(m));
                let selected = app.selection == Some(clip.id);
                let faded = track.hidden || (track.muted && track.kind == TrackKind::Audio);
                paint_clip(&body_painter, r, body, clip, media, track.kind, &app.assets, pps, selected);
                if faded {
                    body_painter.rect_filled(r, 5.0, Color32::from_black_alpha(140));
                }
                if track.locked {
                    continue;
                }
                let hit = r.intersect(body);
                let body_resp = ui.interact(hit, EguiId::new(("clip", clip.id)), Sense::click_and_drag());
                let hw = HANDLE_W.min(r.width() / 4.0);
                let lr = Rect::from_min_max(r.left_top(), pos2(r.left() + hw, r.bottom())).intersect(body);
                let rr = Rect::from_min_max(pos2(r.right() - hw, r.top()), r.right_bottom()).intersect(body);
                let l_resp = ui.interact(lr, EguiId::new(("clip-l", clip.id)), Sense::drag());
                let r_resp = ui.interact(rr, EguiId::new(("clip-r", clip.id)), Sense::drag());
                if l_resp.hovered() || r_resp.hovered() {
                    hover_cursor = Some(CursorIcon::ResizeHorizontal);
                } else if body_resp.hovered() {
                    hover_cursor = Some(CursorIcon::Grab);
                }
                if body_resp.clicked() || body_resp.secondary_clicked() {
                    clicked = Some(clip.id);
                }
                // Right-click menu.
                let id = clip.id;
                let visual = track.kind == TrackKind::Video && media.is_some_and(|m| m.kind != MediaKind::Audio);
                let sounds = media.is_some_and(|m| m.has_sound());
                let can_split = clip.start + MIN_CLIP_LEN < playhead && playhead < clip.end() - MIN_CLIP_LEN;
                body_resp.context_menu(|ui| {
                    ui.set_min_width(210.0);
                    let mut pick = |a: ClipMenu| menu_action = Some((id, a));
                    if ui.add_enabled(can_split, Button::new("✂  Split at playhead")).clicked() {
                        pick(ClipMenu::Split);
                        ui.close_menu();
                    }
                    if ui.button("📋  Duplicate").clicked() {
                        pick(ClipMenu::Duplicate);
                        ui.close_menu();
                    }
                    if sounds {
                        ui.separator();
                        if ui.button("🥁  Detect beats").on_hover_text("Find tempo, beats, bars and song sections (free, on your computer)").clicked() {
                            pick(ClipMenu::Beats);
                            ui.close_menu();
                        }
                    }
                    if visual {
                        ui.separator();
                        if ui
                            .button("✨  Extend shot (AI)…")
                            .on_hover_text("Generate footage that continues from this clip's last frame")
                            .clicked()
                        {
                            pick(ClipMenu::Extend);
                            ui.close_menu();
                        }
                        if ui
                            .button("✨  Auto-reframe (AI)")
                            .on_hover_text("Keep the subject in frame for the current canvas ratio")
                            .clicked()
                        {
                            pick(ClipMenu::Reframe);
                            ui.close_menu();
                        }
                        if ui
                            .button("✨  Fill background (AI)")
                            .on_hover_text("Generate a canvas-filling backdrop instead of black bars")
                            .clicked()
                        {
                            pick(ClipMenu::Plate);
                            ui.close_menu();
                        }
                        if ui
                            .button("👄  Lip-sync to dialogue (AI)…")
                            .on_hover_text("Make the character's mouth match the voice clip over this shot (fal.ai)")
                            .clicked()
                        {
                            pick(ClipMenu::LipSync);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button(RichText::new("🗑  Delete").color(theme::DANGER)).clicked() {
                        pick(ClipMenu::Delete);
                        ui.close_menu();
                    }
                });
                let px = ctx.input(|i| i.pointer.press_origin()).map_or(0.0, |p| p.x);
                if l_resp.drag_started() {
                    start_drag = Some((clip.id, DragMode::TrimStart, px));
                } else if r_resp.drag_started() {
                    start_drag = Some((clip.id, DragMode::TrimEnd, px));
                } else if body_resp.drag_started() {
                    start_drag = Some((clip.id, DragMode::Move, px));
                }
            }
        }
    }
    // Cut markers between touching video clips: ⧓ for a transition, "+" to add one.
    let mut cut_click: Option<(Id, bool)> = None;
    if app.timeline.drag.is_none() && app.drag_media.is_none() {
        for &(ti, y0, y1) in &lane_rows {
            let track = &app.project.tracks[ti];
            if track.kind != TrackKind::Video || track.locked || y1 < body.top() || y0 > body.bottom() {
                continue;
            }
            let active = app.project.track_transitions(ti);
            for w in track.clips.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                let x = t2x(b.start);
                if (a.end() - b.start).abs() > 1e-3 || x < body.left() || x > body.right() {
                    continue;
                }
                let tr = active.iter().find(|t| t.to == b.id);
                if let Some(tr) = tr {
                    let span = Rect::from_min_max(pos2(t2x(tr.start()), y0 + 3.0), pos2(t2x(tr.end()), y1 - 3.0));
                    body_painter.rect_filled(span, 4.0, Color32::from_white_alpha(36));
                    body_painter.rect_stroke(span, 4.0, Stroke::new(1.0_f32, Color32::from_white_alpha(150)));
                }
                let r = Rect::from_center_size(pos2(x, (y0 + y1) / 2.0), vec2(16.0, 16.0));
                let resp = ui.interact(r, EguiId::new(("cut", b.id)), Sense::click());
                let fill = if tr.is_some() {
                    theme::ACCENT
                } else if resp.hovered() {
                    theme::SURFACE_HOVER
                } else {
                    Color32::from_black_alpha(170)
                };
                body_painter.rect_filled(r, 4.0, fill);
                let c = r.center();
                if tr.is_some() {
                    for s in [-1.0f32, 1.0] {
                        body_painter.add(Shape::convex_polygon(
                            vec![c + vec2(5.0 * s, -4.5), c, c + vec2(5.0 * s, 4.5)],
                            Color32::BLACK,
                            Stroke::NONE,
                        ));
                    }
                } else {
                    let st = Stroke::new(1.5_f32, Color32::WHITE);
                    body_painter.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], st);
                    body_painter.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], st);
                }
                let tip = match tr {
                    Some(t) => format!("{} · {:.1}s — click to edit", t.kind.label(), t.duration),
                    None => "Add a transition".to_string(),
                };
                if resp.on_hover_text(tip).clicked() {
                    cut_click = Some((b.id, tr.is_some()));
                }
            }
        }
    }
    if let Some((id, has)) = cut_click {
        if !has {
            let tr = Transition { kind: TransitionKind::Dissolve, duration: 0.5 };
            if app.project.set_transition(id, Some(tr)).is_ok() {
                app.notify("Cross-dissolve added — change it in the inspector or the Animation tab");
            }
        }
        app.selection = Some(id);
        hover_cursor = None;
    }

    if let Some(c) = hover_cursor.filter(|_| app.timeline.drag.is_none()) {
        ctx.set_cursor_icon(c);
    }
    if let Some(id) = clicked {
        app.selection = Some(id);
    }
    if let Some((id, action)) = menu_action {
        app.selection = Some(id);
        match action {
            ClipMenu::Split => app.split_at_playhead(),
            ClipMenu::Duplicate => app.duplicate_selection(),
            ClipMenu::Delete => app.delete_selection(),
            ClipMenu::Extend => app.video_gen.extend_target = Some(id),
            ClipMenu::Reframe => crate::ui::ai::start_reframe(app, &ctx, vec![id]),
            ClipMenu::Beats => {
                if let Some(m) = app.project.clip(id).and_then(|c| c.media_id()) {
                    crate::ui::music::detect(app, &ctx, m);
                }
            }
            ClipMenu::LipSync => crate::ui::voice::open_lipsync(app, id),
            ClipMenu::Plate => crate::ui::image_gen::start(app, &ctx, crate::ai::image_gen::Job::Plate { clip: id }),
        }
    }
    if let Some((id, mode, px)) = start_drag {
        if app.playing {
            app.pause();
        }
        if let Some(orig) = app.project.clip(id).cloned() {
            let (prev_end, next_start) = app.project.neighbor_bounds(id);
            let finite_source = orig
                .media_id()
                .and_then(|m| app.project.media(m))
                .is_some_and(|m| m.source_len().is_some());
            app.timeline.drag = Some(ClipDrag {
                id,
                mode,
                grab_dt: x2t(px) - orig.start,
                max_dur: app.project.max_duration(&orig),
                orig,
                prev_end,
                next_start,
                finite_source,
            });
            app.selection = Some(id);
        }
    }

    update_drag(app, &ctx, &lane_rows, x2t, pps);
    if let Some(d) = &app.timeline.drag {
        ctx.set_cursor_icon(if d.mode == DragMode::Move { CursorIcon::Grabbing } else { CursorIcon::ResizeHorizontal });
    }

    // Drop from the media library.
    if let (Some(mid), Some(p)) = (app.drag_media, ctx.input(|i| i.pointer.latest_pos())) {
        if body.contains(p) {
            let t = x2t(p.x).max(0.0);
            let lane = lane_rows.iter().find(|(_, y0, y1)| p.y >= *y0 && p.y < *y1).map(|r| r.0);
            let dur = app
                .project
                .media(mid)
                .map(|m| m.source_len().unwrap_or(crate::model::DEFAULT_STILL_LEN))
                .unwrap_or(1.0);
            let (gy0, gy1) = lane
                .and_then(|l| lane_rows.iter().find(|r| r.0 == l))
                .map_or((p.y - 20.0, p.y + 20.0), |r| (r.1, r.2));
            let ghost = Rect::from_min_max(pos2(t2x(t), gy0 + 2.0), pos2(t2x(t + dur), gy1 - 2.0));
            body_painter.rect_filled(ghost, 5.0, theme::ACCENT.gamma_multiply(0.35));
            body_painter.rect_stroke(ghost, 5.0, Stroke::new(1.5_f32, theme::ACCENT));
            if ctx.input(|i| i.pointer.any_released()) {
                if let Some(clip) = app.project.new_media_clip(mid, t) {
                    let id = clip.id;
                    app.project.place_clip(clip, lane);
                    app.selection = Some(id);
                }
            }
        }
    }

    // Empty hint.
    if app.project.clip_count() == 0 {
        body_painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "Drag media here, or double-click an item in the library",
            FontId::proportional(14.0),
            theme::TEXT_DIM,
        );
    }

    // Ruler.
    painter.rect_filled(ruler, 0.0, theme::PANEL);
    let rp = painter.with_clip_rect(ruler);
    let step = ruler_step(pps);
    let minor = step / 5.0;
    let mut t = (sx / minor).floor() * minor;
    while t2x(t) < ruler.right() {
        let x = t2x(t);
        let major = ((t / step).round() * step - t).abs() < minor * 0.1;
        let h = if major { 10.0 } else { 4.0 };
        rp.line_segment([pos2(x, ruler.bottom() - h), pos2(x, ruler.bottom())], Stroke::new(1.0_f32, theme::TEXT_DIM));
        if major {
            let label = if step < 1.0 { format!("{}", widgets::format_time(t)) } else { widgets::format_short(t) };
            rp.text(pos2(x + 4.0, ruler.top() + 3.0), Align2::LEFT_TOP, label, FontId::proportional(10.5), theme::TEXT_DIM);
        }
        t += minor;
    }
    // Beat grid and song sections.
    let sections = crate::music::timeline_sections(&app.project);
    for s in &sections {
        let (x0, x1) = (t2x(s.start).max(ruler.left()), t2x(s.end).min(ruler.right()));
        if x1 <= x0 {
            continue;
        }
        let color = match s.label.as_str() {
            "chorus" => Color32::from_rgb(230, 80, 140),
            "verse" => Color32::from_rgb(70, 140, 230),
            "break" => Color32::from_rgb(140, 110, 220),
            _ => Color32::from_rgb(110, 110, 125),
        };
        let strip = Rect::from_min_max(pos2(x0, ruler.top()), pos2(x1 - 1.0, ruler.top() + 3.0));
        rp.rect_filled(strip, 0.0, color);
        if x1 - x0 > 60.0 {
            rp.text(pos2(x0 + 3.0, ruler.top() + 13.0), Align2::LEFT_TOP, &s.label, FontId::proportional(9.5), color);
        }
    }
    let beats = crate::music::timeline_beats(&app.project);
    if !beats.is_empty() {
        let min_gap = beats.windows(2).map(|w| w[1].time - w[0].time).fold(f64::INFINITY, f64::min) * pps;
        for b in beats.iter().filter(|b| t2x(b.time) >= ruler.left() - 2.0 && t2x(b.time) <= ruler.right() + 2.0) {
            if !b.downbeat && min_gap < 5.0 {
                continue;
            }
            let x = t2x(b.time);
            let h = if b.downbeat { 9.0 } else { 5.0 };
            rp.line_segment([pos2(x, ruler.bottom() - h), pos2(x, ruler.bottom())], Stroke::new(if b.downbeat { 1.5_f32 } else { 1.0_f32 }, theme::ACCENT));
            if b.downbeat && min_gap * 4.0 > 14.0 {
                body_painter.line_segment([pos2(x, body.top()), pos2(x, body.bottom())], Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 205, 222, 28)));
            }
        }
    }
    let ruler_resp = ui.interact(ruler, ui.id().with("tl-ruler"), Sense::click_and_drag());
    if ruler_resp.clicked() || ruler_resp.dragged() {
        if let Some(p) = ruler_resp.interact_pointer_pos() {
            if app.playing {
                app.pause();
            }
            app.seek(x2t(p.x).max(0.0));
        }
    }
    if ruler_resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::PointingHand);
    }

    // Pending rough cut: shade what would be removed.
    if let Some(cuts) = app.rough_cut.preview_cuts() {
        for (a, b) in cuts {
            let (x0, x1) = (t2x(a).max(body.left()), t2x(b).min(body.right()));
            if x1 > x0 {
                body_painter.rect_filled(Rect::from_min_max(pos2(x0, body.top()), pos2(x1, body.bottom())), 0.0, Color32::from_rgba_unmultiplied(220, 40, 40, 70));
                painter.rect_filled(Rect::from_min_max(pos2(x0, ruler.bottom() - 4.0), pos2(x1, ruler.bottom())), 0.0, theme::DANGER);
            }
        }
    }

    // Snap guide.
    if let Some(g) = app.timeline.snap_guide {
        let x = t2x(g);
        body_painter.line_segment([pos2(x, body.top()), pos2(x, body.bottom())], Stroke::new(1.0_f32, Color32::from_rgb(255, 210, 60)));
    }

    // Playhead.
    let px = t2x(app.playhead);
    if px >= lanes_rect.left() - 1.0 && px <= lanes_rect.right() {
        painter.line_segment([pos2(px, ruler.top()), pos2(px, body.bottom())], Stroke::new(1.5_f32, Color32::WHITE));
        painter.add(Shape::convex_polygon(
            vec![pos2(px - 6.0, ruler.top()), pos2(px + 6.0, ruler.top()), pos2(px + 6.0, ruler.top() + 8.0), pos2(px, ruler.top() + 14.0), pos2(px - 6.0, ruler.top() + 8.0)],
            Color32::WHITE,
            Stroke::NONE,
        ));
    }

    // Track headers.
    painter.rect_filled(header, 0.0, theme::PANEL);
    painter.text(
        pos2(header.left() + 10.0, ruler.center().y),
        Align2::LEFT_CENTER,
        widgets::format_time(app.playhead),
        FontId::monospace(11.0),
        theme::ACCENT,
    );
    let hp = painter.with_clip_rect(header_body);
    let mut toggles: Vec<(usize, &'static str)> = Vec::new();
    for &(i, y0, y1) in &lane_rows {
        let row = Rect::from_min_max(pos2(header.left() + 6.0, y0), pos2(header.right() - 6.0, y1));
        if row.bottom() < header_body.top() || row.top() > header_body.bottom() {
            continue;
        }
        let track = &app.project.tracks[i];
        let is_main = Some(track.id) == main_id;
        hp.rect_filled(row, 4.0, if is_main { theme::PANEL_ALT } else { Color32::from_rgb(30, 30, 35) });
        let icon = match track.kind {
            TrackKind::Video if is_main => "🎬",
            TrackKind::Video => "🖼",
            TrackKind::Text if track.captions => "CC",
            TrackKind::Text => "T",
            TrackKind::Audio => "🎵",
        };
        let name = if is_main { "Main".to_string() } else { track.name.clone() };
        hp.text(pos2(row.left() + 8.0, row.center().y), Align2::LEFT_CENTER, format!("{icon}  {name}"), FontId::proportional(11.5), theme::TEXT);
        if row.top() < header_body.top() || row.bottom() > header_body.bottom() {
            continue; // partially scrolled out: no buttons
        }
        let mut x = row.right() - 22.0;
        let mut btn = |icon: &'static str, active: bool, tip: &str, key: &'static str| {
            let r = Rect::from_center_size(pos2(x, row.center().y), vec2(20.0, 20.0));
            x -= 22.0;
            if header_toggle(ui, &hp, r, EguiId::new((key, track.id)), icon, active, tip) {
                toggles.push((i, key));
            }
        };
        btn(if track.locked { "🔒" } else { "🔓" }, track.locked, "Lock track", "lock");
        if track.kind != TrackKind::Audio {
            btn("👁", !track.hidden, "Show / hide track", "hide");
        }
        if track.kind != TrackKind::Text {
            btn(if track.muted { "🔇" } else { "🔊" }, !track.muted, "Mute track", "mute");
        }
    }
    for (i, key) in toggles {
        let t = &mut app.project.tracks[i];
        match key {
            "lock" => t.locked = !t.locked,
            "hide" => t.hidden = !t.hidden,
            "mute" => t.muted = !t.muted,
            _ => {}
        }
    }
    painter.line_segment([pos2(header.right(), rect.top()), pos2(header.right(), rect.bottom())], Stroke::new(1.0_f32, theme::BORDER));
    painter.line_segment([pos2(rect.left(), ruler.bottom()), pos2(rect.right(), ruler.bottom())], Stroke::new(1.0_f32, theme::BORDER));
}

fn snap_points(project: &Project, exclude: Id, playhead: f64) -> Vec<f64> {
    let mut pts = vec![0.0, playhead];
    pts.extend(crate::music::timeline_beats(project).iter().map(|b| b.time));
    for c in project.tracks.iter().flat_map(|t| &t.clips).filter(|c| c.id != exclude) {
        pts.push(c.start);
        pts.push(c.end());
    }
    pts
}

fn update_drag(app: &mut EditorApp, ctx: &Context, lane_rows: &[(usize, f32, f32)], x2t: impl Fn(f32) -> f64, pps: f64) {
    let Some(drag) = app.timeline.drag.clone() else { return };
    if !ctx.input(|i| i.pointer.primary_down()) {
        if drag.mode == DragMode::Move {
            app.project.resolve_overlap(drag.id);
        }
        app.project.cleanup_tracks();
        app.timeline.drag = None;
        app.timeline.snap_guide = None;
        return;
    }
    let Some(p) = ctx.input(|i| i.pointer.latest_pos()) else { return };
    let t = x2t(p.x);
    let points = if app.timeline.snap { snap_points(&app.project, drag.id, app.playhead) } else { Vec::new() };
    let threshold = SNAP_PX / pps;
    let o = &drag.orig;
    let mut guide = None;

    match drag.mode {
        DragMode::Move => {
            let mut start = (t - drag.grab_dt).max(0.0);
            if let Some((d, g)) = best_snap(&[start, start + o.duration], &points, threshold) {
                start = (start + d).max(0.0);
                guide = Some(g);
            }
            let kind = app.project.track_kind_for(o);
            let current = app.project.locate(drag.id).map(|l| l.0);
            let target = lane_rows
                .iter()
                .find(|(i, y0, y1)| {
                    let tr = &app.project.tracks[*i];
                    p.y >= *y0 && p.y < *y1 && tr.kind == kind && !tr.locked
                })
                .map(|r| r.0)
                .or(current);
            if let Some(target) = target {
                app.project.move_clip_live(drag.id, target, start);
            }
        }
        DragMode::TrimStart => {
            let mut ns = t;
            if let Some((d, g)) = best_snap(&[ns], &points, threshold) {
                ns += d;
                guide = Some(g);
            }
            let source_min = if drag.finite_source { o.start - o.source_in / o.speed } else { f64::NEG_INFINITY };
            let lo = source_min.max(drag.prev_end).max(0.0);
            let hi = o.end() - MIN_CLIP_LEN;
            let ns = ns.clamp(lo.min(hi), hi);
            if let Some(c) = app.project.clip_mut(drag.id) {
                c.start = ns;
                c.duration = o.end() - ns;
                c.keyframes = o.keyframes.clone();
                c.shift_keyframes(o.start - ns);
                if drag.finite_source {
                    c.source_in = (o.source_in + (ns - o.start) * o.speed).max(0.0);
                }
            }
        }
        DragMode::TrimEnd => {
            let mut ne = t;
            if let Some((d, g)) = best_snap(&[ne], &points, threshold) {
                ne += d;
                guide = Some(g);
            }
            let max_end = (o.start + drag.max_dur.unwrap_or(f64::INFINITY)).min(drag.next_start);
            let lo = o.start + MIN_CLIP_LEN;
            let ne = ne.clamp(lo, max_end.max(lo));
            if let Some(c) = app.project.clip_mut(drag.id) {
                c.duration = ne - o.start;
            }
        }
    }
    app.timeline.snap_guide = guide;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping() {
        let (d, g) = best_snap(&[4.9], &[5.0, 10.0], 0.2).unwrap();
        assert!((d - 0.1).abs() < 1e-9 && g == 5.0);
        assert_eq!(best_snap(&[4.0], &[5.0], 0.2), None);
        // Picks the closest of several edges.
        let (d, g) = best_snap(&[1.0, 3.95], &[0.8, 4.0], 0.3).unwrap();
        assert_eq!(g, 4.0);
        assert!((d - 0.05).abs() < 1e-9);
    }

    #[test]
    fn steps() {
        assert_eq!(ruler_step(100.0), 1.0);
        assert_eq!(ruler_step(10.0), 10.0);
        assert_eq!(ruler_step(1000.0), 0.1);
    }
}
