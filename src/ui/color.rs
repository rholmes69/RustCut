//! Colour tools: the Inspector's "Colour grade" section (built-in looks, `.cube`
//! LUTs, balance), colour match to a reference clip, and the 📊 Scopes window
//! (histogram, luma waveform, vectorscope of the finished frame).

use crate::app::EditorApp;
use crate::media::ffmpeg;
use crate::model::{Clip, ClipSource, Grade, Id, MediaKind, Project};
use crate::render::{filters, grade};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{pos2, vec2, Color32, ColorImage, Context, Rect, RichText, Sense, Shape, Slider, Stroke, TextureHandle, TextureOptions, Ui};
use std::time::{Duration, Instant};

/// (clip, gain, offset) for each matched clip.
type Matched = Vec<(Id, [f32; 3], [f32; 3])>;

pub struct ColorUi {
    pub scopes_open: bool,
    /// The clip other clips are matched to.
    pub reference: Option<Id>,
    /// How far colour match goes (0..1).
    pub strength: f32,
    /// A grade copied with "Copy grade".
    pub copied: Option<Grade>,
    match_job: Option<Task<Matched>>,
    // Scopes: the frame wanted (time in ms + project), since when, and what's shown.
    wanted: Option<(i64, Project)>,
    wanted_since: Instant,
    shown: Option<(i64, Project)>,
    scope_job: Option<(Task<Scopes>, (i64, Project))>,
    scopes: Option<Scopes>,
    textures: Option<(TextureHandle, TextureHandle)>,
    scope_error: Option<String>,
}

impl Default for ColorUi {
    fn default() -> Self {
        Self {
            scopes_open: false,
            reference: None,
            strength: 0.8,
            copied: None,
            match_job: None,
            wanted: None,
            wanted_since: Instant::now(),
            shown: None,
            scope_job: None,
            scopes: None,
            textures: None,
            scope_error: None,
        }
    }
}

impl ColorUi {
    pub fn matching(&self) -> Option<(f32, String)> {
        self.match_job.as_ref().and_then(|j| j.progress())
    }
}

// ── colour match ─────────────────────────────────────────────────────────────

/// A small frame from the middle of a clip with its filter, adjustments and LUT
/// applied (`with_balance`: also its gain/offset).
fn clip_frame(project: &Project, id: Id, with_balance: bool) -> anyhow::Result<Vec<u8>> {
    let c = project.clip(id).ok_or_else(|| anyhow::anyhow!("no clip {id}"))?;
    let m = c.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow::anyhow!("clip {id} has no picture"))?;
    let t = if m.kind == MediaKind::Image { 0.0 } else { c.source_time(c.start + c.duration / 2.0).clamp(0.0, (m.duration - 0.05).max(0.0)) };
    let (w, h) = ffmpeg::fit_within(m.width.max(1), m.height.max(1), 192, 192);
    let mut px = ffmpeg::extract_frame(&m.path, t, w, h)?;
    filters::apply_cpu(&mut px, c.filter, &c.adjust);
    let mut g = c.grade.clone();
    if !with_balance {
        g.gain = [1.0; 3];
        g.offset = [0.0; 3];
    }
    grade::apply_cpu(&mut px, &g);
    Ok(px)
}

fn is_picture(project: &Project, c: &Clip) -> bool {
    !c.audio_only && matches!(c.source, ClipSource::Media(m) if project.media(m).is_some_and(|m| m.kind != MediaKind::Audio))
}

/// Video and image clips on `id`'s track (for "match all on this track").
pub fn track_pictures(project: &Project, id: Id) -> Vec<Id> {
    let Some((ti, _)) = project.locate(id) else { return Vec::new() };
    project.tracks[ti].clips.iter().filter(|c| is_picture(project, c)).map(|c| c.id).collect()
}

/// Matches `targets` to the reference clip in the background.
pub fn start_match(app: &mut EditorApp, ctx: &Context, targets: Vec<Id>) -> Result<usize, String> {
    let reference = app.color.reference.filter(|r| app.project.clip(*r).is_some()).ok_or("Pick a reference clip first (right-click a clip → Use as colour reference)")?;
    let targets: Vec<Id> = targets.into_iter().filter(|t| *t != reference && app.project.clip(*t).is_some_and(|c| is_picture(&app.project, c))).collect();
    if targets.is_empty() {
        return Err("Nothing to match (pick other video or image clips)".into());
    }
    if app.color.match_job.is_some() {
        return Err("A colour match is already running".into());
    }
    let project = app.project.clone();
    let strength = app.color.strength;
    let n = targets.len();
    app.color.match_job = Some(Task::spawn(ctx.clone(), move |cancel, progress| {
        progress(0.0, "Reading the reference…");
        let r = clip_frame(&project, reference, true)?;
        let mut out = Vec::new();
        for (i, id) in targets.iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(None);
            }
            progress(i as f32 / n as f32, &format!("Matching clip {} of {n}…", i + 1));
            // A clip that can't be read (missing file, black frame) is skipped.
            if let Some((g, o)) = clip_frame(&project, *id, false).ok().and_then(|s| grade::match_colour(&s, &r, strength)) {
                out.push((*id, g, o));
            }
        }
        Ok(Some(out))
    }));
    Ok(n)
}

pub fn poll(app: &mut EditorApp) {
    let Some(state) = app.color.match_job.as_ref().and_then(|j| j.take_finished()) else { return };
    app.color.match_job = None;
    match state {
        TaskState::Done(list) => {
            let n = list.len();
            for (id, gain, offset) in list {
                if let Some(c) = app.project.clip_mut(id) {
                    c.grade.gain = gain;
                    c.grade.offset = offset;
                }
            }
            app.notify(format!("Colour matched {n} clip{} to the reference", if n == 1 { "" } else { "s" }));
        }
        TaskState::Failed(e) => app.notify_error(format!("Colour match failed: {e}")),
        _ => {}
    }
}

// ── Inspector section ────────────────────────────────────────────────────────

fn lut_label(g: &Grade) -> String {
    match &g.lut {
        None => "None".into(),
        Some(p) => grade::builtin_name(p).map(str::to_string).unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
    }
}

/// Sets a LUT, checking that it loads. `Err` with a message the user can act on.
pub fn set_lut(c: &mut Clip, path: std::path::PathBuf) -> Result<(), String> {
    grade::cached(&path).map_err(|e| format!("{e:#}"))?;
    c.grade.lut = Some(path);
    if c.grade.lut_mix < 0.05 {
        c.grade.lut_mix = 1.0;
    }
    Ok(())
}

/// The "Colour grade" section of a video/image clip in the Inspector.
pub fn inspector_section(ui: &mut Ui, app: &mut EditorApp, c: &mut Clip) {
    let ctx = ui.ctx().clone();
    egui::Grid::new("grade-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Look");
        egui::ComboBox::from_id_salt("lut-combo").selected_text(lut_label(&c.grade)).width(170.0).show_ui(ui, |ui| {
            if ui.selectable_label(c.grade.lut.is_none(), "None").clicked() {
                c.grade.lut = None;
            }
            ui.separator();
            for (name, desc, _) in grade::BUILTIN {
                let on = c.grade.lut.as_deref().and_then(grade::builtin_name) == Some(*name);
                if ui.selectable_label(on, *name).on_hover_text(*desc).clicked() {
                    match grade::builtin_path(name).map_err(|e| e.to_string()).and_then(|p| set_lut(c, p)) {
                        Ok(()) => {}
                        Err(e) => app.notify_error(e),
                    }
                }
            }
            ui.separator();
            if ui.button("📂 Load .cube LUT…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("3D LUT", &["cube"]).pick_file() {
                    if let Err(e) = set_lut(c, p) {
                        app.notify_error(e);
                    }
                }
            }
        });
        ui.end_row();
        if c.grade.lut.is_some() {
            ui.label("Strength");
            ui.add(Slider::new(&mut c.grade.lut_mix, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
            ui.end_row();
        }
    });

    // Balance (set by colour match, or by hand).
    let id = c.id;
    egui::CollapsingHeader::new(if c.grade.has_balance() { "Balance  ●" } else { "Balance" }).id_salt("grade-balance").show(ui, |ui| {
        egui::Grid::new("balance-grid").num_columns(4).spacing([6.0, 6.0]).show(ui, |ui| {
            ui.label("");
            for (n, col) in [("R", Color32::from_rgb(240, 90, 90)), ("G", Color32::from_rgb(90, 210, 110)), ("B", Color32::from_rgb(100, 150, 250))] {
                ui.label(RichText::new(n).color(col).strong());
            }
            ui.end_row();
            ui.label("Gain");
            for i in 0..3 {
                ui.add(egui::DragValue::new(&mut c.grade.gain[i]).range(0.4..=2.0).speed(0.005).fixed_decimals(2));
            }
            ui.end_row();
            ui.label("Offset");
            for i in 0..3 {
                ui.add(egui::DragValue::new(&mut c.grade.offset[i]).range(-0.3..=0.3).speed(0.002).fixed_decimals(3));
            }
            ui.end_row();
        });
        if c.grade.has_balance() && ui.small_button("Reset balance").clicked() {
            c.grade.gain = [1.0; 3];
            c.grade.offset = [0.0; 3];
        }
    });

    // Colour match.
    ui.add_space(4.0);
    let is_ref = app.color.reference == Some(id);
    let ref_ok = app.color.reference.is_some_and(|r| r != id && app.project.clip(r).is_some());
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(is_ref, "🎯 Reference").on_hover_text("Use this clip's look as the colour reference").clicked() {
            app.color.reference = if is_ref { None } else { Some(id) };
        }
        let busy = app.color.match_job.is_some();
        if ui.add_enabled(ref_ok && !busy, egui::Button::new("🎨 Match to reference")).on_hover_text("Adjust this clip's colour balance to look like the reference").clicked() {
            if let Err(e) = start_match(app, &ctx, vec![id]) {
                app.notify_error(e);
            }
        }
        if ui
            .add_enabled(app.color.reference.is_some() && !busy, egui::Button::new("🎨 Match whole track"))
            .on_hover_text("Match every video/image clip on this track to the reference — evens out AI shots that came back in different looks")
            .clicked()
        {
            let targets = track_pictures(&app.project, id);
            if let Err(e) = start_match(app, &ctx, targets) {
                app.notify_error(e);
            }
        }
    });
    if app.color.reference.is_some() {
        ui.horizontal(|ui| {
            ui.label(widgets::dim("Match strength").small());
            ui.add(Slider::new(&mut app.color.strength, 0.2..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
        });
    }
    if let Some((p, stage)) = app.color.matching() {
        ui.add(egui::ProgressBar::new(p).text(stage));
    }

    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Copy grade").clicked() {
            app.color.copied = Some(c.grade.clone());
        }
        if let Some(g) = app.color.copied.clone() {
            if ui.small_button("Paste grade").clicked() {
                c.grade = g.clone();
            }
            if ui.small_button("Paste to whole track").on_hover_text("Give every video/image clip on this track the copied grade").clicked() {
                for t in track_pictures(&app.project, id) {
                    if let Some(x) = app.project.clip_mut(t) {
                        x.grade = g.clone();
                    }
                }
                c.grade = g;
            }
        }
        if !c.grade.is_identity() && ui.small_button("Reset grade").clicked() {
            c.grade = Grade::default();
        }
    });
}

// ── scopes ───────────────────────────────────────────────────────────────────

const WAVE_W: usize = 256;
const WAVE_H: usize = 128;
const VEC_N: usize = 140;

pub struct Scopes {
    /// R, G, B, luma.
    hist: [[u32; 256]; 4],
    wave: ColorImage,
    vector: ColorImage,
    avg_luma: f32,
    /// Share of pixels crushed to black / clipped to white.
    crushed: f32,
    clipped: f32,
}

fn compute(w: u32, h: u32, px: &[u8]) -> Scopes {
    let mut hist = [[0u32; 256]; 4];
    let mut wave = vec![0u32; WAVE_W * WAVE_H];
    let mut vec_counts = vec![[0u32; 4]; VEC_N * VEC_N];
    let (mut sum, mut crushed, mut clipped) = (0f64, 0usize, 0usize);
    let n = (w * h) as usize;
    for (i, p) in px.chunks_exact(4).enumerate() {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        hist[0][p[0] as usize] += 1;
        hist[1][p[1] as usize] += 1;
        hist[2][p[2] as usize] += 1;
        hist[3][(l as usize).min(255)] += 1;
        sum += l as f64;
        crushed += (l < 4.0) as usize;
        clipped += (l > 251.0) as usize;
        let x = (i % w as usize) * WAVE_W / w as usize;
        let y = WAVE_H - 1 - ((l / 255.0) * (WAVE_H - 1) as f32) as usize;
        wave[y * WAVE_W + x] += 1;
        let cb = (-0.168736 * r - 0.331264 * g + 0.5 * b) / 255.0;
        let cr = (0.5 * r - 0.418688 * g - 0.081312 * b) / 255.0;
        // ±0.5 maps to the edge, with a little headroom.
        let vx = ((cb / 1.1 + 0.5) * (VEC_N - 1) as f32).clamp(0.0, (VEC_N - 1) as f32) as usize;
        let vy = ((0.5 - cr / 1.1) * (VEC_N - 1) as f32).clamp(0.0, (VEC_N - 1) as f32) as usize;
        let cell = &mut vec_counts[vy * VEC_N + vx];
        cell[0] += p[0] as u32;
        cell[1] += p[1] as u32;
        cell[2] += p[2] as u32;
        cell[3] += 1;
    }
    // Waveform: brightness grows with how many pixels land there.
    let rows_per_col = (n as f32 / WAVE_W as f32).max(1.0);
    let wave_px: Vec<Color32> = wave
        .iter()
        .map(|&k| {
            let v = ((k as f32 / rows_per_col * 40.0).sqrt() * 255.0).min(255.0) as u8;
            Color32::from_rgb(v / 3, v, v / 2)
        })
        .collect();
    let vec_px: Vec<Color32> = vec_counts
        .iter()
        .map(|c| {
            if c[3] == 0 {
                return Color32::BLACK;
            }
            let k = ((c[3] as f32).ln_1p() / 5.0).min(1.0);
            let avg = [0, 1, 2].map(|i| (c[i] / c[3]) as f32);
            let [r, g, b] = avg.map(|v| (v * (0.35 + 0.65 * k)).min(255.0) as u8);
            Color32::from_rgb(r.max((60.0 * k) as u8), g.max((60.0 * k) as u8), b.max((60.0 * k) as u8))
        })
        .collect();
    Scopes {
        hist,
        wave: ColorImage { size: [WAVE_W, WAVE_H], pixels: wave_px },
        vector: ColorImage { size: [VEC_N, VEC_N], pixels: vec_px },
        avg_luma: (sum / n.max(1) as f64 / 255.0) as f32,
        crushed: crushed as f32 / n.max(1) as f32,
        clipped: clipped as f32 / n.max(1) as f32,
    }
}

/// Keeps the scopes up to date: re-renders the finished frame a moment after the
/// playhead or the project stops changing (not during playback).
pub fn poll_scopes(ctx: &Context, app: &mut EditorApp) {
    let cu = &mut app.color;
    if !cu.scopes_open {
        return;
    }
    if let Some((job, key)) = &cu.scope_job {
        if let Some(state) = job.take_finished() {
            let key = key.clone();
            cu.scope_job = None;
            match state {
                TaskState::Done(s) => {
                    let wave = ctx.load_texture("scope-wave", s.wave.clone(), TextureOptions::LINEAR);
                    let vector = ctx.load_texture("scope-vector", s.vector.clone(), TextureOptions::NEAREST);
                    cu.textures = Some((wave, vector));
                    cu.scopes = Some(s);
                    cu.scope_error = None;
                }
                TaskState::Failed(e) => cu.scope_error = Some(e),
                _ => {}
            }
            cu.shown = Some(key);
        }
    }
    if app.playing || app.project.duration() <= 0.0 {
        return;
    }
    let t = (app.playhead * 1000.0).round() as i64;
    let same = |k: &Option<(i64, Project)>| k.as_ref().is_some_and(|(kt, kp)| *kt == t && *kp == app.project);
    if !same(&cu.wanted) {
        cu.wanted = Some((t, app.project.clone()));
        cu.wanted_since = Instant::now();
    }
    if cu.scope_job.is_none() && !same(&cu.shown) {
        if cu.wanted_since.elapsed() < Duration::from_millis(250) {
            ctx.request_repaint_after(Duration::from_millis(260));
            return;
        }
        let key = cu.wanted.clone().unwrap();
        let (project, at) = (key.1.clone(), key.0 as f64 / 1000.0);
        let job = Task::spawn(ctx.clone(), move |_, _| {
            let (w, h, px) = crate::export::render_frame_rgba(&project, at, 270)?;
            Ok(Some(compute(w, h, &px)))
        });
        cu.scope_job = Some((job, key));
    }
}

pub fn scopes_window(ctx: &Context, app: &mut EditorApp) {
    if !app.color.scopes_open {
        return;
    }
    let mut open = true;
    egui::Window::new(RichText::new("📊 Scopes").strong()).id(egui::Id::new("scopes")).open(&mut open).default_width(300.0).resizable(false).show(ctx, |ui| {
        let cu = &app.color;
        if app.playing {
            ui.label(widgets::dim("Paused frames only — the scopes update when playback stops.").small());
        }
        if let Some(e) = &cu.scope_error {
            ui.label(RichText::new(e).color(theme::DANGER).small());
        }
        let (Some(s), Some((wave, vector))) = (&cu.scopes, &cu.textures) else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Rendering the frame…");
            });
            return;
        };
        let w = 280.0;
        // Histogram.
        ui.label(RichText::new("Histogram").small().strong());
        let (r, _) = ui.allocate_exact_size(vec2(w, 90.0), Sense::hover());
        let p = ui.painter_at(r);
        p.rect_filled(r, 2.0, Color32::from_gray(12));
        let max = s.hist[..3].iter().flat_map(|h| h[2..254].iter()).copied().max().unwrap_or(1).max(1) as f32;
        for (ch, col) in [(3, Color32::from_white_alpha(70)), (0, Color32::from_rgba_unmultiplied(240, 70, 70, 150)), (1, Color32::from_rgba_unmultiplied(70, 220, 90, 150)), (2, Color32::from_rgba_unmultiplied(80, 140, 255, 150))] {
            let pts: Vec<_> = (0..256).map(|i| pos2(r.left() + i as f32 / 255.0 * r.width(), r.bottom() - (s.hist[ch][i] as f32 / max).min(1.0) * (r.height() - 2.0))).collect();
            p.add(Shape::line(pts, Stroke::new(1.2_f32, col)));
        }
        // Waveform.
        ui.label(RichText::new("Luma waveform").small().strong());
        let (r, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
        let p = ui.painter_at(r);
        p.image(wave.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        for (lvl, label) in [(1.0, "100"), (0.5, "50"), (0.0, "0")] {
            let y = r.bottom() - lvl * r.height();
            p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(0.5_f32, Color32::from_white_alpha(40)));
            p.text(pos2(r.left() + 2.0, y.clamp(r.top() + 6.0, r.bottom() - 6.0)), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(9.0), Color32::from_white_alpha(120));
        }
        // Vectorscope.
        ui.label(RichText::new("Vectorscope").small().strong());
        let size = 150.0;
        let (r, _) = ui.allocate_exact_size(vec2(w, size), Sense::hover());
        let sq = Rect::from_center_size(r.center(), vec2(size, size));
        let p = ui.painter_at(r);
        p.image(vector.id(), sq, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let c = sq.center();
        p.circle_stroke(c, size / 2.0 / 1.1, Stroke::new(0.6_f32, Color32::from_white_alpha(50)));
        p.circle_stroke(c, size / 4.0 / 1.1, Stroke::new(0.5_f32, Color32::from_white_alpha(35)));
        // Skin-tone line (~123° from +Cb): healthy skin sits along it.
        let a = 123f32.to_radians();
        p.line_segment([c, c + vec2(a.cos(), -a.sin()) * (size / 2.0)], Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 190, 140, 120)));
        p.text(c + vec2(a.cos(), -a.sin()) * (size / 2.0 - 4.0), egui::Align2::RIGHT_TOP, "skin", egui::FontId::proportional(9.0), Color32::from_rgba_unmultiplied(255, 190, 140, 170));
        // Readout.
        let warn = |v: f32| if v > 0.02 { theme::DANGER } else { theme::TEXT_DIM };
        ui.horizontal(|ui| {
            ui.label(widgets::dim(format!("Avg {:.0}%", s.avg_luma * 100.0)).small());
            ui.label(RichText::new(format!("Crushed {:.1}%", s.crushed * 100.0)).small().color(warn(s.crushed)));
            ui.label(RichText::new(format!("Clipped {:.1}%", s.clipped * 100.0)).small().color(warn(s.clipped)));
        });
        if cu.scope_job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(widgets::dim("Updating…").small());
            });
        }
    });
    if !open {
        app.color.scopes_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_of_a_grey_ramp() {
        let (w, h) = (64u32, 8u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| {
            let v = ((i % w) * 255 / (w - 1)) as u8;
            [v, v, v, 255]
        }).collect();
        let s = compute(w, h, &px);
        assert!((s.avg_luma - 0.5).abs() < 0.02, "{}", s.avg_luma);
        assert_eq!(s.hist[0], s.hist[2]);
        assert!(s.crushed > 0.0 && s.clipped > 0.0);
        // Greys sit in the middle of the vectorscope.
        let mid = VEC_N / 2;
        assert!(s.vector.pixels[mid * VEC_N + mid] != Color32::BLACK || s.vector.pixels[(mid - 1) * VEC_N + mid - 1] != Color32::BLACK);
    }
}
