//! Lip-sync Studio window (AI → 👄 Lip-sync Studio…, or right-click a clip): three steps
//! from a shot and a voice to a lip-synced clip, with a mini timeline (shot, face, audio
//! and lyrics) to trim the span, face detection or manual face marking, a recommended
//! method and a cost estimate before anything is spent. See `crate::ai::lipsync_studio`.

use crate::ai::faces::{self, FaceBox, Faces};
use crate::ai::lipsync::{self, Placement, Quality};
use crate::ai::lipsync_studio::{self as st, Method, Outcome, Request, Source};
use crate::app::EditorApp;
use crate::media::ffmpeg;
use crate::model::{Id, MediaKind, TrackKind};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{pos2, vec2, Button, Color32, ColorImage, Context, Rect, RichText, Sense, Stroke, TextEdit, TextureHandle, TextureOptions};
use std::path::PathBuf;
use std::sync::Arc;

const STRIP: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum VoiceMode {
    #[default]
    Timeline,
    File,
    Tts,
}

/// RGBA frames: (source time, width, height, pixels).
type Frames = Vec<(f64, u32, u32, Vec<u8>)>;

#[derive(Default)]
pub struct LipSyncStudioUi {
    pub open: bool,
    step: usize,
    video_clip: Option<Id>,
    voice_clip: Option<Id>,
    voice_mode: VoiceMode,
    voice_file: Option<PathBuf>,
    tts_voice: String,
    tts_text: String,
    tts_model: String,
    /// The trimmed timeline span (None = the whole overlap).
    span: Option<(f64, f64)>,
    /// None = the recommended method.
    method: Option<Method>,
    quality: Quality,
    placement: Placement,
    face: Option<FaceBox>,
    marking: bool,
    drag_from: Option<(f32, f32)>,
    direction: String,
    video_model: String,
    lipsync_pro: bool,
    perf_best: bool,
    sheet: Option<PathBuf>,
    // Background work and caches.
    frames_key: Option<(Id, i64, i64)>,
    frames_job: Option<Task<(Frames, (f64, u32, u32, Vec<u8>))>>,
    strip: Vec<(f64, TextureHandle)>,
    picker: Option<(f64, TextureHandle)>,
    peaks_key: Option<PathBuf>,
    peaks_job: Option<Task<Vec<f32>>>,
    peaks: Option<Arc<Vec<f32>>>,
    faces_job: Option<Task<Faces>>,
    faces: Option<Faces>,
    tts_job: Option<Task<PathBuf>>,
    run_job: Option<(Task<Outcome>, Request)>,
    refused: Option<String>,
    result: Option<Id>,
    handle_drag: Option<bool>,
}

impl LipSyncStudioUi {
    pub fn busy(&self) -> bool {
        self.run_job.is_some() || self.tts_job.is_some()
    }

    fn reset_shot(&mut self) {
        self.span = None;
        self.face = None;
        self.faces = None;
        self.faces_job = None;
        self.marking = false;
        self.frames_key = None;
        self.strip.clear();
        self.picker = None;
        self.refused = None;
        self.result = None;
        self.method = None;
    }
}

/// Opens the Studio on `clip` (from the clip's right-click menu or a result panel).
pub fn open_for(app: &mut EditorApp, clip: Id) {
    let voice = lipsync::find_voice(&app.project, clip);
    let s = &mut app.lipsync_studio;
    s.open = true;
    if s.video_clip != Some(clip) {
        s.reset_shot();
        s.video_clip = Some(clip);
        s.voice_clip = voice;
        s.voice_mode = VoiceMode::Timeline;
    }
    s.step = 0;
}

/// Opens the Studio on the selected video clip, if any.
pub fn open_new(app: &mut EditorApp) {
    let sel = app.selection.filter(|&id| app.project.clip(id).is_some_and(|c| app.project.track_kind_for(c) == TrackKind::Video));
    match sel {
        Some(id) => open_for(app, id),
        None => app.lipsync_studio.open = true,
    }
}

fn texture(ctx: &Context, name: String, w: u32, h: u32, rgba: &[u8]) -> TextureHandle {
    ctx.load_texture(name, ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba), TextureOptions::LINEAR)
}

pub fn poll(app: &mut EditorApp, ctx: &Context) {
    let s = &mut app.lipsync_studio;
    if let Some(state) = s.frames_job.as_ref().and_then(Task::take_finished) {
        s.frames_job = None;
        if let TaskState::Done((strip, pick)) = state {
            s.strip = strip.iter().enumerate().map(|(i, (t, w, h, px))| (*t, texture(ctx, format!("ls-strip-{i}"), *w, *h, px))).collect();
            s.picker = Some((pick.0, texture(ctx, "ls-picker".into(), pick.1, pick.2, &pick.3)));
        }
    }
    if let Some(state) = s.peaks_job.as_ref().and_then(Task::take_finished) {
        s.peaks_job = None;
        if let TaskState::Done(p) = state {
            s.peaks = Some(Arc::new(p));
        }
    }
    if let Some(state) = s.faces_job.as_ref().and_then(Task::take_finished) {
        s.faces_job = None;
        match state {
            TaskState::Done(f) => {
                // One person: choose them straight away.
                if f.people() == 1 {
                    if let Some(b) = f.pick_frame().and_then(|fr| fr.faces.first()).copied() {
                        s.face = Some(faces::span_box(&faces::track(&f, b), b));
                    }
                }
                s.faces = Some(f);
            }
            TaskState::Failed(e) => app.notify_error(format!("Face detection: {e}")),
            _ => {}
        }
    }
    let s = &mut app.lipsync_studio;
    if let Some(state) = s.tts_job.as_ref().and_then(Task::take_finished) {
        s.tts_job = None;
        match state {
            TaskState::Done(path) => {
                let at = app.lipsync_studio.video_clip.and_then(|id| app.project.clip(id)).map_or(app.playhead, |c| c.start);
                match st::place_voice(&mut app.project, &path, at) {
                    Ok(id) => {
                        if let Some(m) = app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).cloned() {
                            app.assets.ensure(&m);
                        }
                        let s = &mut app.lipsync_studio;
                        s.voice_clip = Some(id);
                        s.voice_mode = VoiceMode::Timeline;
                        s.span = None;
                        app.notify("🗣 Line placed under the shot — music lowered under it");
                    }
                    Err(e) => app.notify_error(format!("Couldn't place the line: {e:#}")),
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("Text to speech: {e}")),
            _ => {}
        }
    }
    let s = &mut app.lipsync_studio;
    if let Some(state) = s.run_job.as_ref().and_then(|(t, _)| t.take_finished()) {
        let Some((_, req)) = s.run_job.take() else { return };
        match state {
            TaskState::Done(Outcome::Refused { reason, suggested }) => {
                let s = &mut app.lipsync_studio;
                s.refused = Some(reason);
                s.method = Some(suggested);
                app.notify_error("The model refused this still (not charged). Try Animate + sync — it's selected now.");
            }
            TaskState::Done(outcome) => match st::apply(&mut app.project, &req, outcome) {
                Ok(ids) => {
                    for id in &ids {
                        if let Some(m) = app.project.clip(*id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).cloned() {
                            app.assets.ensure(&m);
                        }
                    }
                    let first = ids.first().copied();
                    app.selection = first;
                    let s = &mut app.lipsync_studio;
                    s.result = first;
                    s.video_clip = first.or(s.video_clip);
                    s.frames_key = None;
                    app.notify(format!("👄 Lip-synced — {} clip(s) placed. Press Preview, or swap takes in the Studio.", ids.len()));
                }
                Err(e) => app.notify_error(format!("Lip-sync: {e:#}")),
            },
            TaskState::Failed(e) => app.notify_error(format!("Lip-sync: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify("Lip-sync stopped"),
        }
    }
}

/// The shot's frames for the strip and the face picker, re-read when the shot or span changes.
fn ensure_frames(app: &mut EditorApp, ctx: &Context, span: (f64, f64)) {
    let Some(id) = app.lipsync_studio.video_clip else { return };
    let key = (id, (span.0 * 10.0) as i64, (span.1 * 10.0) as i64);
    if app.lipsync_studio.frames_key == Some(key) || app.lipsync_studio.frames_job.is_some() {
        return;
    }
    let Some(c) = app.project.clip(id).cloned() else { return };
    let Some(m) = c.media_id().and_then(|m| app.project.media(m)).cloned() else { return };
    app.lipsync_studio.frames_key = Some(key);
    let still = m.kind == MediaKind::Image;
    let src = |t: f64| if still { 0.0 } else { c.source_in + (t - c.start) * c.speed };
    let times: Vec<f64> = (0..STRIP).map(|i| src(span.0 + (span.1 - span.0) * (i as f64 + 0.5) / STRIP as f64)).collect();
    let mid = src((span.0 + span.1) / 2.0);
    let path = m.path.clone();
    let (mw, mh) = (m.width.max(1), m.height.max(1));
    app.lipsync_studio.frames_job = Some(Task::spawn(ctx.clone(), move |_, _| {
        let (sw, sh) = ffmpeg::fit_within(mw, mh, 128, 72);
        let strip: Frames = times.iter().filter_map(|&t| ffmpeg::extract_frame(&path, t, sw, sh).ok().map(|px| (t, sw, sh, px))).collect();
        let (pw, ph) = ffmpeg::fit_within(mw, mh, 640, 360);
        let pick = ffmpeg::extract_frame(&path, mid, pw, ph)?;
        Ok(Some((strip, (mid, pw, ph, pick))))
    }));
}

fn ensure_peaks(app: &mut EditorApp, ctx: &Context) {
    let Some(path) = app.lipsync_studio.voice_clip.and_then(|id| app.project.clip(id)).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).map(|m| m.path.clone()) else { return };
    if app.lipsync_studio.peaks_key.as_ref() == Some(&path) || app.lipsync_studio.peaks_job.is_some() {
        return;
    }
    app.lipsync_studio.peaks_key = Some(path.clone());
    app.lipsync_studio.peaks = None;
    app.lipsync_studio.peaks_job = Some(Task::spawn(ctx.clone(), move |_, _| Ok(Some(ffmpeg::waveform(&path)?))));
}

/// Where the shot and the voice overlap on the timeline.
fn overlap(app: &EditorApp) -> Option<(f64, f64)> {
    let v = app.project.clip(app.lipsync_studio.video_clip?)?;
    let a = app.project.clip(app.lipsync_studio.voice_clip?)?;
    let (s, e) = (v.start.max(a.start), v.end().min(a.end()));
    (e - s > 0.2).then_some((s, e))
}

fn request(app: &EditorApp) -> Option<Request> {
    let s = &app.lipsync_studio;
    let (v, a) = (s.video_clip?, s.voice_clip?);
    let method = s.method.unwrap_or_else(|| st::recommend(&app.project, v, a, s.span));
    let mut r = Request::new(v, a, method);
    r.span = s.span;
    r.quality = s.quality;
    r.placement = s.placement;
    r.face = s.face;
    r.direction = s.direction.clone();
    if !s.video_model.is_empty() {
        r.video_model = s.video_model.clone();
    }
    r.lipsync_model = if s.lipsync_pro { lipsync::MODELS[1].0.into() } else { lipsync::MODELS[0].0.into() };
    r.performance_model = crate::ai::performance::MODELS[if s.perf_best { 1 } else { 0 }].0.into();
    r.sheet = s.sheet.clone();
    Some(r)
}

fn clip_name(app: &EditorApp, id: Id) -> String {
    app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| format!("clip {id}"))
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.lipsync_studio.open {
        return;
    }
    // Drop references to clips that were deleted.
    if app.lipsync_studio.video_clip.is_some_and(|id| app.project.clip(id).is_none()) {
        app.lipsync_studio.video_clip = None;
        app.lipsync_studio.reset_shot();
    }
    if app.lipsync_studio.voice_clip.is_some_and(|id| app.project.clip(id).is_none()) {
        app.lipsync_studio.voice_clip = None;
    }
    if let Some(span) = overlap(app) {
        let shown = app.lipsync_studio.span.unwrap_or(span);
        ensure_frames(app, ctx, shown);
        ensure_peaks(app, ctx);
    }
    let mut open = true;
    egui::Window::new(RichText::new("👄 Lip-sync Studio").strong())
        .id(egui::Id::new("lipsync-studio"))
        .open(&mut open)
        .default_width(600.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (i, label) in ["1 · Shot & face", "2 · Voice", "3 · Generate"].into_iter().enumerate() {
                    let on = app.lipsync_studio.step == i;
                    if ui.selectable_label(on, if on { RichText::new(label).strong() } else { RichText::new(label) }).clicked() {
                        app.lipsync_studio.step = i;
                    }
                }
            });
            ui.separator();
            mini_timeline(ui, app);
            ui.separator();
            match app.lipsync_studio.step {
                0 => step_shot(ui, app),
                1 => step_voice(ui, app),
                _ => step_generate(ui, app),
            }
            ui.separator();
            ui.horizontal(|ui| {
                let step = app.lipsync_studio.step;
                if ui.add_enabled(step > 0, Button::new("◀ Back")).clicked() {
                    app.lipsync_studio.step -= 1;
                }
                if step < 2 && ui.add_enabled(app.lipsync_studio.video_clip.is_some(), Button::new("Next ▶")).clicked() {
                    app.lipsync_studio.step += 1;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Dialogue & voices…").on_hover_text("Keys, voices and voice clones (ElevenLabs, fal.ai)").clicked() {
                        app.voice.open = true;
                    }
                });
            });
        });
    if !open {
        app.lipsync_studio.open = false;
    }
}

/// Shot, face and audio lanes over the overlap, with handles to trim the span.
fn mini_timeline(ui: &mut egui::Ui, app: &mut EditorApp) {
    let Some((o0, o1)) = overlap(app) else {
        ui.label(widgets::dim(match (app.lipsync_studio.video_clip, app.lipsync_studio.voice_clip) {
            (None, _) => "Pick the shot (step 1).",
            (_, None) => "Pick the voice: the song, a line, or text to speech (step 2).",
            _ => "The voice doesn't overlap this shot. Move one of them, or pick another voice clip.",
        }));
        return;
    };
    let (s0, s1) = app.lipsync_studio.span.unwrap_or((o0, o1));
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 132.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(24));
    let x_of = |t: f64| rect.left() + ((t - o0) / (o1 - o0)) as f32 * rect.width();
    let t_of = |x: f32| o0 + ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * (o1 - o0);
    let lane = |i: usize, h: f32| {
        let top = rect.top() + [4.0, 50.0, 90.0][i];
        Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), top + h))
    };
    // Shot lane: thumbnails across the span.
    let shot = lane(0, 42.0);
    let s = &app.lipsync_studio;
    let n = s.strip.len().max(1) as f32;
    let shown_w = (x_of(s1) - x_of(s0)).max(1.0);
    for (i, (_, tex)) in s.strip.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x_of(s0) + shown_w * i as f32 / n, shot.top()), vec2(shown_w / n - 1.0, shot.height()));
        painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // Face lane: the chosen face cropped from each thumbnail.
    let face_lane = lane(1, 36.0);
    match s.face {
        Some(f) => {
            let uv = Rect::from_min_max(pos2(f.left().max(0.0), f.top().max(0.0)), pos2(f.right().min(1.0), f.bottom().min(1.0)));
            let cell = (face_lane.height() * f.w / f.h.max(0.01) * 16.0 / 9.0).clamp(20.0, 80.0);
            let mut x = x_of(s0);
            let mut k = 0usize;
            while x + cell <= x_of(s1) + 0.5 && !s.strip.is_empty() {
                let tex = &s.strip[(k * s.strip.len() / ((shown_w / cell) as usize).max(1)).min(s.strip.len() - 1)].1;
                painter.image(tex.id(), Rect::from_min_size(pos2(x, face_lane.top()), vec2(cell - 1.0, face_lane.height())), uv, Color32::WHITE);
                x += cell;
                k += 1;
            }
        }
        None => {
            painter.text(face_lane.left_center() + vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, "face: chosen by the model (detect or mark it in step 1)", egui::FontId::proportional(11.0), Color32::from_gray(140));
        }
    }
    // Audio lane: the voice waveform and the lyric lines.
    let audio = lane(2, 38.0);
    if let (Some(peaks), Some(vc)) = (&s.peaks, s.voice_clip.and_then(|id| app.project.clip(id))) {
        let mid = audio.center().y;
        let mut x = audio.left();
        while x < audio.right() {
            let t = t_of(x);
            let src = vc.source_in + (t - vc.start) * vc.speed;
            let i = (src * ffmpeg::PEAKS_PER_SEC) as usize;
            let p = peaks.get(i).copied().unwrap_or(0.0) * vc.volume.min(1.5);
            painter.line_segment([pos2(x, mid - p * 17.0), pos2(x, mid + p * 17.0)], Stroke::new(1.0_f32, theme::ACCENT.gamma_multiply(0.8)));
            x += 2.0;
        }
    }
    for t in app.project.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.name.to_lowercase().contains("lyric")) {
        for c in t.clips.iter().filter(|c| c.end() > o0 && c.start < o1) {
            if let Some(txt) = c.text() {
                let x = x_of(c.start.max(o0));
                painter.line_segment([pos2(x, audio.top()), pos2(x, audio.bottom())], Stroke::new(1.0_f32, Color32::from_gray(90)));
                painter.text(pos2(x + 3.0, audio.top()), egui::Align2::LEFT_TOP, txt.content.replace('\n', " ").chars().take(40).collect::<String>(), egui::FontId::proportional(10.0), Color32::from_gray(200));
            }
        }
    }
    // Dim outside the span, and the two trim handles.
    let dim = Color32::from_black_alpha(150);
    painter.rect_filled(Rect::from_min_max(rect.min, pos2(x_of(s0), rect.bottom())), 0.0, dim);
    painter.rect_filled(Rect::from_min_max(pos2(x_of(s1), rect.top()), rect.max), 0.0, dim);
    for t in [s0, s1] {
        let x = x_of(t);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(3.0_f32, theme::ACCENT));
    }
    // Dragging near a handle trims; elsewhere it does nothing.
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (d0, d1) = ((p.x - x_of(s0)).abs(), (p.x - x_of(s1)).abs());
            app.lipsync_studio.handle_drag = (d0.min(d1) < 14.0).then_some(d0 > d1);
        }
    }
    if let (Some(right), Some(p)) = (app.lipsync_studio.handle_drag, resp.interact_pointer_pos()) {
        if resp.dragged() {
            let t = t_of(p.x);
            let (a, b) = if right { (s0, t.max(s0 + 0.5)) } else { (t.min(s1 - 0.5), s1) };
            app.lipsync_studio.span = Some((a.max(o0), b.min(o1)));
        }
    }
    if resp.drag_stopped() {
        app.lipsync_studio.handle_drag = None;
    }
    if resp.double_clicked() {
        app.lipsync_studio.span = None;
    }
    let len = s1 - s0;
    ui.horizontal(|ui| {
        ui.label(widgets::dim(format!("{} – {}  ({len:.1} s)", widgets::format_short(s0), widgets::format_short(s1))).small());
        ui.label(widgets::dim("drag the gold handles to trim · double-click to reset").small());
        if ui.small_button("▶ Play span").clicked() {
            app.seek(s0);
            app.play();
        }
    });
}

fn step_shot(ui: &mut egui::Ui, app: &mut EditorApp) {
    match app.lipsync_studio.video_clip {
        Some(id) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Shot: {}", clip_name(app, id))).strong());
                if ui.small_button("Use the selected clip").clicked() {
                    if let Some(sel) = app.selection.filter(|&s| s != id) {
                        open_for(app, sel);
                    }
                }
            });
        }
        None => {
            ui.label("Select the singer's or speaker's clip on the timeline, then:");
            if ui.button("Use the selected clip").clicked() {
                open_new(app);
            }
        }
    }
    ui.horizontal(|ui| {
        ui.label(widgets::dim("…or bring in"));
        let len = app.lipsync_studio.voice_clip.and_then(|id| app.project.clip(id)).map_or(5.0, |c| c.duration);
        let at = app.lipsync_studio.voice_clip.and_then(|id| app.project.clip(id)).map_or(app.playhead, |c| c.start);
        let mut pick: Option<Source> = None;
        if ui.button("a video file…").clicked() {
            pick = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "mkv", "webm"]).pick_file().map(Source::File);
        }
        if ui.button("a still…").on_hover_text("An image of the singer: Animate + sync brings it to life").clicked() {
            pick = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp"]).pick_file().map(Source::Still);
        }
        if let Some(src) = pick {
            match st::place_source(&mut app.project, &src, at, len) {
                Ok(id) => {
                    if let Some(m) = app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).cloned() {
                        app.assets.ensure(&m);
                    }
                    let voice = app.lipsync_studio.voice_clip;
                    open_for(app, id);
                    app.lipsync_studio.voice_clip = voice.or(app.lipsync_studio.voice_clip);
                    app.notify(format!("Placed at {}", widgets::format_short(at)));
                }
                Err(e) => app.notify_error(format!("{e:#}")),
            }
        }
    });
    let Some(id) = app.lipsync_studio.video_clip else { return };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Face").strong());
        let busy = app.lipsync_studio.faces_job.is_some();
        if busy {
            ui.spinner();
            ui.label(widgets::dim("Finding faces…"));
        } else if ui.button("🔍 Detect faces (≈ $0.01)").on_hover_text("A vision model boxes every face in a few frames (OpenRouter)").clicked() {
            start_faces(app, ui.ctx(), id);
        }
        let s = &mut app.lipsync_studio;
        if ui.selectable_label(s.marking, "✏ Mark face manually").on_hover_text("Drag a box around the face on the frame below").clicked() {
            s.marking = !s.marking;
        }
        if s.face.is_some() && ui.small_button("Clear").on_hover_text("Let the lip-sync model find the face itself").clicked() {
            s.face = None;
        }
    });
    face_picker(ui, app);
    if let Some(f) = app.lipsync_studio.face {
        if let Some(h) = faces::size_hint(f.h) {
            ui.label(RichText::new(format!("⚠ {h}")).color(theme::DANGER).small());
        } else if faces::recommend_pro(f.h) {
            ui.label(widgets::dim("A close-up: High (1080p) and the pro model will show the difference.").small());
        }
    }
    ui.label(widgets::dim("Aspect: the shot keeps its framing. To reframe it, use AI → Auto-reframe.").small());
}

fn start_faces(app: &mut EditorApp, ctx: &Context, clip: Id) {
    let Some((a, b)) = app.lipsync_studio.span.or_else(|| app.project.clip(clip).map(|c| (c.start, c.end()))) else { return };
    let frames = match faces::sample(&app.project, clip, a, b) {
        Ok(f) => f,
        Err(e) => return app.notify_error(format!("Face detection: {e:#}")),
    };
    let client = match crate::ai::openrouter::Client::from_settings(&app.ai.settings) {
        Ok(c) => c,
        Err(_) => return app.notify_error("Face detection needs an OpenRouter key (AI → AI settings). You can mark the face manually instead."),
    };
    let model = app.ai.settings.vision_model.clone();
    app.lipsync_studio.faces_job = Some(Task::spawn(ctx.clone(), move |cancel, report| faces::detect(&frames, &client, &model, cancel, report)));
}

/// The middle frame with the detected faces (click one) or a manual box (drag).
fn face_picker(ui: &mut egui::Ui, app: &mut EditorApp) {
    let s = &mut app.lipsync_studio;
    let Some((t, tex)) = &s.picker else {
        if s.frames_job.is_some() {
            ui.spinner();
        }
        return;
    };
    let size = tex.size_vec2();
    let w = ui.available_width().min(480.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, w * size.y / size.x), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    let to_screen = |b: &FaceBox| Rect::from_min_max(pos2(rect.left() + b.left() * rect.width(), rect.top() + b.top() * rect.height()), pos2(rect.left() + b.right() * rect.width(), rect.top() + b.bottom() * rect.height()));
    let frac = |p: egui::Pos2| (((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
    // Detected faces nearest this frame.
    let detected: Vec<FaceBox> = s.faces.as_ref().and_then(|f| f.frames.iter().min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()))).map(|fr| fr.faces.clone()).unwrap_or_default();
    for b in &detected {
        painter.rect_stroke(to_screen(b), 2.0, Stroke::new(1.5_f32, Color32::from_rgb(120, 200, 255)));
    }
    if let Some(f) = s.face {
        painter.rect_stroke(to_screen(&f), 2.0, Stroke::new(2.5_f32, theme::ACCENT));
        painter.text(to_screen(&f).left_top() - vec2(0.0, 2.0), egui::Align2::LEFT_BOTTOM, "lip-synced face", egui::FontId::proportional(11.0), theme::ACCENT);
    }
    if s.marking {
        if resp.drag_started() {
            s.drag_from = resp.interact_pointer_pos().map(frac);
        }
        if let (Some(a), Some(p)) = (s.drag_from, resp.interact_pointer_pos()) {
            let b = FaceBox::from_corners(a, frac(p));
            painter.rect_stroke(to_screen(&b), 2.0, Stroke::new(2.0_f32, Color32::YELLOW));
            if resp.drag_stopped() {
                if b.w > 0.02 && b.h > 0.02 {
                    s.face = Some(b);
                    s.marking = false;
                }
                s.drag_from = None;
            }
        }
    } else if resp.clicked() {
        if let Some((x, y)) = resp.interact_pointer_pos().map(frac) {
            if let Some(b) = detected.iter().find(|b| b.contains(x, y)).copied() {
                let tracked = s.faces.as_ref().map(|f| faces::track(f, b)).unwrap_or_default();
                s.face = Some(faces::span_box(&tracked, b));
            }
        }
    }
    let hint = match (&s.faces, s.marking) {
        (_, true) => "Drag a box around the face.".to_string(),
        (Some(f), _) if f.people() > 1 => format!("{} people found — click the one who sings or speaks.", f.people()),
        (Some(f), _) if f.people() == 0 => "No face found. Mark it manually.".to_string(),
        (Some(_), _) => "One face found and chosen.".to_string(),
        (None, _) => "Optional: with one person in the shot, the model finds the face itself.".to_string(),
    };
    ui.label(widgets::dim(hint).small());
}

fn step_voice(ui: &mut egui::Ui, app: &mut EditorApp) {
    ui.horizontal(|ui| {
        let s = &mut app.lipsync_studio;
        ui.radio_value(&mut s.voice_mode, VoiceMode::Timeline, "On the timeline");
        ui.radio_value(&mut s.voice_mode, VoiceMode::File, "Audio file");
        ui.radio_value(&mut s.voice_mode, VoiceMode::Tts, "Text to speech");
    });
    match app.lipsync_studio.voice_mode {
        VoiceMode::Timeline => {
            let Some(v) = app.lipsync_studio.video_clip.and_then(|id| app.project.clip(id)).cloned() else {
                ui.label(widgets::dim("Pick the shot first."));
                return;
            };
            let options: Vec<(Id, String)> = app
                .project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Audio)
                .flat_map(|t| t.clips.iter())
                .filter(|c| c.end() > v.start + 0.2 && c.start < v.end() - 0.2)
                .map(|c| (c.id, format!("{}  ({})", clip_name(app, c.id), widgets::format_short(c.start))))
                .collect();
            if options.is_empty() {
                ui.label(widgets::dim("No audio under this shot. Bring in a file or make a line with text to speech."));
            }
            let current = app.lipsync_studio.voice_clip;
            let label = options.iter().find(|o| Some(o.0) == current).map_or("Choose…".to_string(), |o| o.1.clone());
            let mut chosen = None;
            egui::ComboBox::from_id_salt("ls-voice").width(360.0).selected_text(label).show_ui(ui, |ui| {
                for (id, name) in &options {
                    if ui.selectable_label(Some(*id) == current, name).clicked() {
                        chosen = Some(*id);
                    }
                }
            });
            if let Some(id) = chosen {
                app.lipsync_studio.voice_clip = Some(id);
                app.lipsync_studio.span = None;
            }
            lyric_range(ui, app);
        }
        VoiceMode::File => {
            ui.horizontal(|ui| {
                if ui.button("Choose audio…").clicked() {
                    app.lipsync_studio.voice_file = rfd::FileDialog::new().add_filter("Audio", &["wav", "mp3", "m4a", "flac", "aac"]).pick_file();
                }
                if let Some(p) = app.lipsync_studio.voice_file.clone() {
                    ui.label(widgets::dim(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    if ui.button("Place under the shot").clicked() {
                        let at = app.lipsync_studio.video_clip.and_then(|id| app.project.clip(id)).map_or(app.playhead, |c| c.start);
                        match st::place_voice(&mut app.project, &p, at) {
                            Ok(id) => {
                                let s = &mut app.lipsync_studio;
                                s.voice_clip = Some(id);
                                s.voice_mode = VoiceMode::Timeline;
                                s.span = None;
                            }
                            Err(e) => app.notify_error(format!("{e:#}")),
                        }
                    }
                }
            });
            ui.label(widgets::dim("The full mix works: there's no need to isolate the vocals.").small());
        }
        VoiceMode::Tts => tts(ui, app),
    }
}

/// Pick the first and last lyric line to set the span.
fn lyric_range(ui: &mut egui::Ui, app: &mut EditorApp) {
    let Some((o0, o1)) = overlap(app) else { return };
    let lines: Vec<(f64, f64, String)> = app
        .project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Text)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.start >= o0 - 0.05 && c.end() <= o1 + 0.05)
        .filter_map(|c| c.text().map(|t| (c.start, c.end(), t.content.replace('\n', " ").chars().take(50).collect())))
        .collect();
    if lines.len() < 2 {
        return;
    }
    let (s0, s1) = app.lipsync_studio.span.unwrap_or((o0, o1));
    ui.horizontal(|ui| {
        ui.label("Lyrics from");
        let from = lines.iter().min_by(|a, b| (a.0 - s0).abs().total_cmp(&(b.0 - s0).abs())).map(|l| l.2.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("ls-from").width(200.0).selected_text(from).show_ui(ui, |ui| {
            for l in &lines {
                if ui.selectable_label(false, &l.2).clicked() {
                    app.lipsync_studio.span = Some((l.0.max(o0), s1.max(l.0 + 0.5)));
                }
            }
        });
        ui.label("to");
        let to = lines.iter().min_by(|a, b| (a.1 - s1).abs().total_cmp(&(b.1 - s1).abs())).map(|l| l.2.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("ls-to").width(200.0).selected_text(to).show_ui(ui, |ui| {
            for l in &lines {
                if ui.selectable_label(false, &l.2).clicked() {
                    app.lipsync_studio.span = Some((s0.min(l.1 - 0.5), l.1.min(o1)));
                }
            }
        });
    });
}

fn tts(ui: &mut egui::Ui, app: &mut EditorApp) {
    use crate::ai::elevenlabs;
    let voices: Vec<String> = app.ai.settings.voices.iter().map(|v| v.name.clone()).collect();
    let s = &mut app.lipsync_studio;
    if s.tts_voice.is_empty() {
        s.tts_voice = voices.first().cloned().unwrap_or_default();
    }
    if s.tts_model.is_empty() {
        s.tts_model = app.ai.settings.tts_model.clone();
    }
    ui.horizontal(|ui| {
        ui.label("Voice");
        egui::ComboBox::from_id_salt("ls-tts-voice").width(180.0).selected_text(if s.tts_voice.is_empty() { "No voices yet" } else { &s.tts_voice }).show_ui(ui, |ui| {
            for v in &voices {
                ui.selectable_value(&mut s.tts_voice, v.clone(), v);
            }
        });
        ui.label("Model");
        egui::ComboBox::from_id_salt("ls-tts-model").width(170.0).selected_text(&s.tts_model).show_ui(ui, |ui| {
            for (id, label) in elevenlabs::MODELS {
                ui.selectable_value(&mut s.tts_model, id.to_string(), *label);
            }
        });
    });
    ui.add(TextEdit::multiline(&mut s.tts_text).hint_text("Ma'am, his bond is one hundred thousand dollars…").desired_rows(2).desired_width(f32::INFINITY));
    let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref()).join("voice");
    let cached = (!s.tts_text.trim().is_empty() && !s.tts_voice.is_empty()).then(|| elevenlabs::cached_line(&dir, &s.tts_voice, &s.tts_text)).flatten();
    if s.tts_job.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Speaking…");
        });
        return;
    }
    let chars = s.tts_text.chars().count();
    let usd = chars as f64 * app.ai.settings.tts_usd_per_char;
    let at = s.video_clip.and_then(|id| app.project.clip(id)).map_or(app.playhead, |c| c.start);
    if let Some(path) = cached {
        if ui.button(format!("♻ Use the line made before (free) at {}", widgets::format_short(at))).clicked() {
            match st::place_voice(&mut app.project, &path, at) {
                Ok(id) => {
                    let s = &mut app.lipsync_studio;
                    s.voice_clip = Some(id);
                    s.voice_mode = VoiceMode::Timeline;
                    s.span = None;
                }
                Err(e) => app.notify_error(format!("{e:#}")),
            }
        }
        return;
    }
    let ready = chars > 0 && !s.tts_voice.is_empty() && app.ai.settings.elevenlabs_key().is_some();
    if ui.add_enabled(ready, Button::new(format!("🗣 Make the line (≈ ${usd:.3}, {chars} credits) at {}", widgets::format_short(at)))).on_disabled_hover_text(elevenlabs::NO_KEY).clicked() {
        let Some(profile) = app.ai.settings.voice(&s.tts_voice) else { return };
        let Some(key) = app.ai.settings.elevenlabs_key() else { return };
        let (text, model, rate) = (s.tts_text.clone(), s.tts_model.clone(), app.ai.settings.tts_usd_per_char);
        let settings = elevenlabs::VoiceSettings { stability: 0.5, similarity: 0.8, style: 0.0 };
        s.tts_job = Some(Task::spawn(ui.ctx().clone(), move |_, report| {
            report(0.3, "Speaking…");
            Ok(Some(elevenlabs::speak_to_file(&elevenlabs::Client::new(key), &profile, &text, &model, settings, &dir, rate)?))
        }));
    }
}

fn step_generate(ui: &mut egui::Ui, app: &mut EditorApp) {
    let Some(req) = request(app) else {
        ui.label(widgets::dim("Pick the shot (step 1) and the voice (step 2) first."));
        return;
    };
    let recommended = st::recommend(&app.project, req.video_clip, req.voice_clip, req.span);
    ui.label(RichText::new("Method").strong());
    for m in Method::ALL {
        let label = if m == recommended { format!("{}  ★ recommended", m.label()) } else { m.label().to_string() };
        if ui.radio(req.method == m, label).on_hover_text(m.help()).clicked() {
            app.lipsync_studio.method = Some(m);
        }
    }
    ui.label(widgets::dim(req.method.help()).small());
    let s = &mut app.lipsync_studio;
    ui.horizontal(|ui| {
        ui.label("Quality");
        ui.radio_value(&mut s.quality, Quality::Standard, Quality::Standard.label());
        ui.radio_value(&mut s.quality, Quality::High, Quality::High.label());
    });
    ui.horizontal(|ui| {
        ui.label("Result");
        ui.radio_value(&mut s.placement, Placement::Replace, "Replace the span");
        ui.radio_value(&mut s.placement, Placement::Above, "On the track above (keep the original)");
    });
    match req.method {
        Method::FixMouth => {
            ui.checkbox(&mut s.lipsync_pro, "Pro model (sharper teeth and lips, ≈ $0.083/s)");
        }
        Method::Performance => {
            ui.checkbox(&mut s.perf_best, "Best quality (≈ $0.30/s instead of $0.24/s)");
            ui.horizontal(|ui| {
                ui.label("Direction");
                ui.add(TextEdit::singleline(&mut s.direction).hint_text(crate::ai::performance::DEFAULT_DIRECTION).desired_width(360.0));
            });
            ui.horizontal(|ui| {
                ui.label("Character sheet");
                match s.sheet.clone() {
                    Some(p) => {
                        ui.label(widgets::dim(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                        if ui.small_button("✕").clicked() {
                            s.sheet = None;
                        }
                    }
                    None => {
                        if ui.small_button("Choose… (optional)").clicked() {
                            s.sheet = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp"]).pick_file();
                        }
                    }
                }
            });
        }
        Method::AnimateSync => {
            ui.horizontal(|ui| {
                ui.label("Video model");
                let current = if s.video_model.is_empty() { st::DEFAULT_VIDEO_MODEL.to_string() } else { s.video_model.clone() };
                egui::ComboBox::from_id_salt("ls-video-model").width(320.0).selected_text(st::animate_model(&current).1).show_ui(ui, |ui| {
                    for (id, label, usd, _, _) in st::ANIMATE_MODELS {
                        if ui.selectable_label(current == *id, format!("{label}  (≈ ${usd:.2}/s)")).clicked() {
                            s.video_model = id.to_string();
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Gestures");
                ui.add(TextEdit::singleline(&mut s.direction).hint_text("e.g. she sways, then rests her hand on her chest").desired_width(360.0));
            });
            ui.label(widgets::dim("Added to a steady-face prompt (soft lips, no tears, face toward the camera) so the lip-sync draws a clean mouth.").small());
        }
    }
    let Some(req) = request(app) else { return };
    let est = st::estimate(&app.project, &req);
    ui.separator();
    match &est {
        Ok(e) => {
            ui.label(RichText::new(format!("Estimated ${:.2}  ·  {} – {} ({:.1} s)", e.usd, widgets::format_short(e.start), widgets::format_short(e.end), e.end - e.start)).strong());
            for (label, usd) in &e.parts {
                ui.label(widgets::dim(format!("  • {label}: ${usd:.2}")).small());
            }
            for w in &e.warnings {
                ui.label(RichText::new(format!("⚠ {w}")).color(theme::DANGER).small());
            }
        }
        Err(e) => {
            ui.label(RichText::new(format!("⚠ {e}")).color(theme::DANGER));
        }
    }
    if let Some(r) = &app.lipsync_studio.refused {
        ui.label(RichText::new(format!("The last try was refused (not charged): {}", r.chars().take(140).collect::<String>())).color(theme::DANGER).small());
    }
    if let Some((job, _)) = &app.lipsync_studio.run_job {
        let (p, text) = job.progress().unwrap_or((0.0, "Starting…".into()));
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(p).desired_width(260.0).text(text));
            cancel = ui.small_button("Cancel").clicked();
        });
        if cancel {
            job.cancel();
        }
    } else if let Ok(e) = est {
        let fal = app.ai.settings.fal_key();
        let needs_or = req.method == Method::AnimateSync && crate::ai::openrouter::Client::from_settings(&app.ai.settings).is_err();
        let ready = fal.is_some() && !needs_or;
        let hover = if fal.is_none() { lipsync::NO_KEY } else { "Animate + sync needs an OpenRouter key (AI → AI settings)" };
        if ui.add_enabled(ready, Button::new(RichText::new(format!("👄 Generate (≈ ${:.2})", e.usd)).strong())).on_disabled_hover_text(hover).clicked() {
            start_run(app, ui.ctx(), req);
        }
    }
    takes(ui, app);
}

fn start_run(app: &mut EditorApp, ctx: &Context, req: Request) {
    let Some(key) = app.ai.settings.fal_key() else { return };
    let client = crate::ai::openrouter::Client::from_settings(&app.ai.settings).ok();
    let project = app.project.clone();
    let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref()).join("lipsync");
    let r = req.clone();
    app.lipsync_studio.refused = None;
    let task = Task::spawn(ctx.clone(), move |cancel, report| st::run(&project, &r, &key, client.as_ref(), &dir, cancel, report));
    app.lipsync_studio.run_job = Some((task, req));
}

/// The result's takes: swap back to the original or an earlier result.
fn takes(ui: &mut egui::Ui, app: &mut EditorApp) {
    let Some(id) = app.lipsync_studio.result.or(app.lipsync_studio.video_clip) else { return };
    let list = app.project.takes_for(id);
    if list.len() < 2 {
        return;
    }
    let current = app.project.clip(id).and_then(|c| c.media_id());
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Takes").strong());
        let name = |m: Id| app.project.media(m).map(|x| x.name.clone()).unwrap_or_default();
        let mut pick = None;
        egui::ComboBox::from_id_salt("ls-takes").width(300.0).selected_text(current.map(name).unwrap_or_default()).show_ui(ui, |ui| {
            for (i, m) in list.iter().enumerate() {
                let label = if i == 0 { format!("Original — {}", name(*m)) } else { format!("Take {i} — {}", name(*m)) };
                if ui.selectable_label(Some(*m) == current, label).clicked() {
                    pick = Some(*m);
                }
            }
        });
        if let Some(m) = pick {
            if app.project.use_take(id, m) {
                if let Some(item) = app.project.media(m).cloned() {
                    app.assets.ensure(&item);
                }
                app.lipsync_studio.frames_key = None;
            }
        }
        if ui.button("▶ Preview").clicked() {
            if let Some(c) = app.project.clip(id) {
                let t = c.start;
                app.seek(t);
                app.play();
            }
        }
    });
}
