//! "Make a video" storyboard window: brief → script & shot list → storyboard frames
//! (approve / regenerate / swap / reorder) → animate approved shots → assemble.

use crate::ai::openrouter::Client;
use crate::ai::storyboard::{self, Storyboard};
use crate::ai::video_gen::{self, Generated, VideoModel};
use crate::app::EditorApp;
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::Aspect;
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Button, Color32, Context, Grid, ProgressBar, RichText, ScrollArea, TextEdit, TextureHandle, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const LENGTHS: [u32; 4] = [15, 30, 45, 60];
/// Rough price of one storyboard frame (Gemini Flash Image ≈ $0.07 at 1K).
const FRAME_PRICE: f64 = 0.07;

type Peaks = HashMap<PathBuf, Arc<Vec<f32>>>;

struct Produced {
    sb: Storyboard,
    rendered: Vec<(usize, Generated)>,
    music: Option<(PathBuf, ProbeInfo)>,
    peaks: Peaks,
    errors: Vec<String>,
}

enum Job {
    Plan(Task<Storyboard>),
    Frames(Task<Storyboard>),
    Produce(Task<Produced>),
}

impl Job {
    fn progress(&self) -> Option<(f32, String)> {
        match self {
            Job::Plan(t) | Job::Frames(t) => t.progress(),
            Job::Produce(t) => t.progress(),
        }
    }

    fn cancel(&self) {
        match self {
            Job::Plan(t) | Job::Frames(t) => t.cancel(),
            Job::Produce(t) => t.cancel(),
        }
    }
}

pub struct StoryboardUi {
    pub open: bool,
    brief: String,
    seconds: u32,
    aspect: Aspect,
    sb: Option<Storyboard>,
    job: Option<Job>,
    music: Option<PathBuf>,
    audio: bool,
    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_jobs: HashMap<PathBuf, Task<(u32, u32, Vec<u8>)>>,
    /// Music-video mode: plan shots for the song on the timeline.
    pub music_video: bool,
    /// Whole song (false) or its best 30 seconds (true).
    best30: bool,
    max_shots: usize,
    /// Photo of the lead character for every frame.
    character: Option<PathBuf>,
}

impl Default for StoryboardUi {
    fn default() -> Self {
        Self {
            open: false,
            brief: String::new(),
            seconds: 30,
            aspect: Aspect::Portrait,
            sb: None,
            job: None,
            music: None,
            audio: false,
            thumbs: HashMap::new(),
            thumb_jobs: HashMap::new(),
            music_video: false,
            best30: false,
            max_shots: 10,
            character: None,
        }
    }
}

fn key(app: &mut EditorApp, ctx: &Context) -> Option<String> {
    let k = app.ai.settings.api_key();
    if k.is_none() {
        app.notify_error(crate::ai::openrouter::NO_KEY);
        app.ai.open_settings(ctx);
    }
    k
}

fn out_root(app: &EditorApp) -> PathBuf {
    video_gen::output_dir(app.project_path.as_deref())
}

fn start_plan(app: &mut EditorApp, ctx: &Context) {
    let Some(key) = key(app, ctx) else { return };
    if app.storyboard.music_video {
        let Some(song) = crate::multicam::song_clip(&app.project) else {
            app.notify_error("Put the song on an audio track first");
            return;
        };
        let mut project = app.project.clone();
        project.aspect = app.storyboard.aspect;
        let (theme, max_shots, best30) = (app.storyboard.brief.clone(), app.storyboard.max_shots, app.storyboard.best30);
        let character = app.storyboard.character.clone();
        let model = app.ai.settings.edit_model.clone();
        let root = out_root(app);
        app.storyboard.job = Some(Job::Plan(Task::spawn(ctx.clone(), move |_, report| {
            report(0.1, "Finding the beat and sections…");
            let media = project.clip(song).and_then(|c| c.media_id());
            if let Some(m) = media.filter(|m| !project.music.contains_key(m)) {
                let path = project.media(m).map(|x| x.path.clone()).unwrap_or_default();
                project.music.insert(m, crate::music::analyze(&path)?);
            }
            let clip = project.clip(song).cloned().ok_or_else(|| anyhow::anyhow!("the song clip was removed"))?;
            let range = if best30 { crate::ai::music_video::best_window(&project, 30.0).unwrap_or((clip.start, clip.end())) } else { (clip.start, clip.end()) };
            report(0.3, "Directing the shots…");
            let mut sb = crate::ai::music_video::plan(&Client::new(key), &model, &theme, &project, song, range, max_shots, &root)?;
            if sb.fresh_analysis.is_none() {
                sb.fresh_analysis = media.and_then(|m| project.music.get(&m).cloned().map(|a| (m, a)));
            }
            sb.character = character;
            let _ = sb.save();
            Ok(Some(sb))
        })));
        return;
    }
    let (brief, seconds, aspect) = (app.storyboard.brief.clone(), app.storyboard.seconds as f64, app.storyboard.aspect);
    let model = app.ai.settings.edit_model.clone();
    let root = out_root(app);
    let character = app.storyboard.character.clone();
    app.storyboard.job = Some(Job::Plan(Task::spawn(ctx.clone(), move |_, report| {
        report(0.2, "Writing the script and shot list…");
        let mut sb = storyboard::plan(&Client::new(key), &model, &brief, seconds, aspect, &root)?;
        sb.character = character;
        let _ = sb.save();
        Ok(Some(sb))
    })));
}

/// Draws the frames of `only` (or every shot without a frame when empty).
fn start_frames(app: &mut EditorApp, ctx: &Context, only: Vec<usize>) {
    let Some(key) = key(app, ctx) else { return };
    let Some(mut sb) = app.storyboard.sb.clone() else { return };
    let model = app.ai.settings.image_model.clone();
    for &i in &only {
        if let Some(s) = sb.shots.get_mut(i) {
            s.frame = None;
        }
    }
    app.storyboard.job = Some(Job::Frames(Task::spawn(ctx.clone(), move |cancel, report| {
        storyboard::generate_missing_frames(&Client::new(key), &mut sb, &model, cancel, report)?;
        Ok(Some(sb))
    })));
}

fn start_produce(app: &mut EditorApp, ctx: &Context, model: VideoModel) {
    let Some(key) = key(app, ctx) else { return };
    let Some(sb) = app.storyboard.sb.clone() else { return };
    let shots = sb.approved();
    // A music video already has its soundtrack: no extra music, no generated sound.
    let (audio, music) = if sb.is_music_video() { (false, None) } else { (app.storyboard.audio, app.storyboard.music.clone()) };
    app.storyboard.job = Some(Job::Produce(Task::spawn(ctx.clone(), move |cancel, report| {
        let client = Client::new(key);
        let mut sb = sb;
        report(0.0, &format!("Animating {} shots…", sb.to_animate(&shots).len()));
        let (rendered, errors, _reused) = storyboard::produce_shots(&client, &mut sb, &shots, &model, audio, cancel, report);
        if rendered.is_empty() {
            anyhow::bail!("no shot could be animated: {}", errors.join("; "));
        }
        report(0.96, "Preparing music and ducking…");
        let music = match music {
            Some(p) => Some((p.clone(), ffmpeg::probe(&p)?)),
            None => None,
        };
        let mut peaks = Peaks::new();
        if music.is_some() {
            for (_, g) in rendered.iter().filter(|(_, g)| g.info.has_audio) {
                if let Ok(w) = ffmpeg::waveform(&g.path) {
                    peaks.insert(g.path.clone(), Arc::new(w));
                }
            }
            if let Some((p, _)) = &music {
                if let Ok(w) = ffmpeg::waveform(p) {
                    peaks.insert(p.clone(), Arc::new(w));
                }
            }
        }
        let mut sb = sb;
        sb.spent += rendered.iter().filter_map(|(_, g)| g.cost).sum::<f64>();
        let _ = sb.save();
        Ok(Some(Produced { sb, rendered, music, peaks, errors }))
    })));
}

/// Applies finished jobs. Called every frame.
pub fn poll(app: &mut EditorApp) {
    let finished = match &app.storyboard.job {
        Some(Job::Plan(t)) | Some(Job::Frames(t)) => t.take_finished().map(|s| s.map_plain()),
        Some(Job::Produce(t)) => t.take_finished().map(|s| match s {
            TaskState::Done(p) => Finished::Produced(Box::new(p)),
            TaskState::Failed(e) => Finished::Failed(e),
            _ => Finished::Stopped,
        }),
        None => None,
    };
    let Some(f) = finished else { return };
    let was_plan = matches!(app.storyboard.job, Some(Job::Plan(_)));
    app.storyboard.job = None;
    match f {
        Finished::Storyboard(mut sb) => {
            if let Some((m, a)) = sb.fresh_analysis.take() {
                app.project.music.entry(m).or_insert(a);
            }
            let before = app.storyboard.sb.as_ref().map_or(0.0, |s| s.spent);
            app.ai.session_cost += (sb.spent - if was_plan { 0.0 } else { before }).max(0.0);
            let missing = sb.shots.iter().filter(|s| s.frame.is_none()).count();
            if was_plan {
                app.notify(format!("📝 “{}”: {} shots written — review them, then draw the storyboard", sb.title, sb.shots.len()));
            } else if missing > 0 {
                app.notify_error(format!("{missing} storyboard frame(s) could not be drawn — try Regenerate or another image model"));
            } else {
                app.notify("🎨 Storyboard ready — approve the shots you like");
            }
            app.storyboard.sb = Some(sb);
        }
        Finished::Produced(p) => {
            let p = *p;
            let before = app.storyboard.sb.as_ref().map_or(0.0, |s| s.spent);
            app.ai.session_cost += (p.sb.spent - before).max(0.0);
            let peaks = p.peaks;
            let peaks_for = |path: &Path| peaks.get(path).cloned();
            let report = storyboard::assemble_any(&mut app.project, &p.sb, &p.rendered, p.music.clone(), &peaks_for);
            for (_, g) in &p.rendered {
                if let Some(item) = app.project.media.iter().find(|m| m.path == g.path).cloned() {
                    app.assets.ensure(&item);
                }
            }
            app.selection = report.clips.first().copied();
            if let Some(start) = report.clips.first().and_then(|c| app.project.clip(*c)).map(|c| c.start) {
                app.reveal(start);
            }
            let mut msg = format!(
                "🎬 “{}” assembled: {} shots, {:.0} s, {} captions{}{} · −14 LUFS",
                p.sb.title,
                report.clips.len(),
                report.duration,
                report.captions,
                if report.music.is_some() { ", music" } else { "" },
                if report.ducked { " (ducked)" } else { "" }
            );
            msg += &format!(" · storyboard total ${:.2}", p.sb.spent);
            app.notify(msg);
            if !p.errors.is_empty() {
                app.notify_error(format!("Some shots failed: {}", p.errors.join("; ")));
            }
            app.storyboard.sb = Some(p.sb);
        }
        Finished::Failed(e) => app.notify_error(format!("Storyboard: {e}")),
        Finished::Stopped => app.notify("Storyboard step stopped. Video jobs already submitted may still be billed."),
    }
}

enum Finished {
    Storyboard(Storyboard),
    Produced(Box<Produced>),
    Failed(String),
    Stopped,
}

trait MapPlain {
    fn map_plain(self) -> Finished;
}

impl MapPlain for TaskState<Storyboard> {
    fn map_plain(self) -> Finished {
        match self {
            TaskState::Done(sb) => Finished::Storyboard(sb),
            TaskState::Failed(e) => Finished::Failed(e),
            _ => Finished::Stopped,
        }
    }
}

pub(crate) fn thumb(app: &mut EditorApp, ui: &Ui, path: &Path) -> Option<egui::TextureId> {
    let g = &mut app.storyboard;
    if let Some(t) = g.thumbs.get(path) {
        return Some(t.id());
    }
    if let Some(state) = g.thumb_jobs.get(path).and_then(Task::take_finished) {
        g.thumb_jobs.remove(path);
        if let TaskState::Done((w, h, rgba)) = state {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let tex = ui.ctx().load_texture(format!("sb-{}", path.display()), img, egui::TextureOptions::LINEAR);
            let id = tex.id();
            g.thumbs.insert(path.to_path_buf(), tex);
            return Some(id);
        }
        return None;
    }
    if !g.thumb_jobs.contains_key(path) {
        let p = path.to_path_buf();
        g.thumb_jobs.insert(
            path.to_path_buf(),
            Task::spawn(ui.ctx().clone(), move |_, _| {
                let info = ffmpeg::probe(&p)?;
                let (w, h) = ffmpeg::fit_within(info.width, info.height, 360, 360);
                Ok(Some((w, h, ffmpeg::extract_frame(&p, 0.0, w, h)?)))
            }),
        );
    }
    None
}

#[derive(Clone, Copy)]
enum CardAction {
    Regenerate(usize),
    Swap(usize),
    Left(usize),
    Right(usize),
    Remove(usize),
}

fn brief_stage(ui: &mut Ui, app: &mut EditorApp) {
    let song = crate::multicam::song_clip(&app.project);
    ui.horizontal(|ui| {
        let g = &mut app.storyboard;
        ui.selectable_value(&mut g.music_video, false, RichText::new("📝 Ad / story").strong());
        ui.add_enabled_ui(song.is_some(), |ui| {
            ui.selectable_value(&mut g.music_video, true, RichText::new("🎵 Music video for my song").strong())
                .on_disabled_hover_text("Put the song on an audio track first");
        });
    });
    if song.is_none() {
        app.storyboard.music_video = false;
    }
    let mv = app.storyboard.music_video;
    ui.label(RichText::new(if mv { "What's the video's idea?" } else { "What should the video be?" }).strong());
    ui.add(
        TextEdit::multiline(&mut app.storyboard.brief)
            .hint_text(if mv {
                "e.g. neon city at night, a lonely dancer in a red jacket, rain, slow motion in the verses and fast cuts in the chorus"
            } else {
                "e.g. 30-second ad for a neighbourhood coffee shop called Ember, warm and cozy, ends with “Visit us today”"
            })
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        let g = &mut app.storyboard;
        if mv {
            ui.label("Cover");
            ui.selectable_value(&mut g.best30, false, "Whole song");
            ui.selectable_value(&mut g.best30, true, "Best 30 s");
            ui.add_space(12.0);
            ui.label("Unique shots");
            ui.add(egui::DragValue::new(&mut g.max_shots).range(3..=24))
                .on_hover_text("New shots to generate; repeated sections (2nd chorus…) reuse them, like real music videos");
        } else {
            ui.label("Length");
            egui::ComboBox::from_id_salt("sb-len").selected_text(format!("{} s", g.seconds)).show_ui(ui, |ui| {
                for l in LENGTHS {
                    ui.selectable_value(&mut g.seconds, l, format!("{l} s"));
                }
            });
        }
        ui.add_space(12.0);
        ui.label("Canvas");
        egui::ComboBox::from_id_salt("sb-aspect").selected_text(g.aspect.label()).show_ui(ui, |ui| {
            for a in Aspect::ALL {
                ui.selectable_value(&mut g.aspect, a, a.label());
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Lead character");
        let name = app.storyboard.character.as_ref().and_then(|p| p.file_name()).map_or("none (invented)".to_string(), |n| n.to_string_lossy().into_owned());
        ui.label(widgets::dim(name));
        if ui.small_button("Choose photo…").on_hover_text("A clear photo of the person; every frame keeps their likeness. Only use people who agreed to it.").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp"]).pick_file() {
                app.storyboard.character = Some(p);
            }
        }
        if app.storyboard.character.is_some() && ui.small_button("✕").clicked() {
            app.storyboard.character = None;
        }
    });
    ui.add_space(6.0);
    ui.label(widgets::dim(if mv {
        "Shots are timed to the song's bars (faster cuts in loud parts). Step 1 plans them (under a cent; beats are detected first if needed). \
         Step 2 draws one picture per unique shot (≈ $0.07 each). Step 3 animates only approved shots and cuts them on the beat."
    } else {
        "Step 1 writes a script and shot list (under a cent). Step 2 draws one picture per shot (≈ $0.07 each). \
         Step 3 animates only the shots you approve, at video prices shown before you start."
    }));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let ready = !app.storyboard.brief.trim().is_empty();
        let label = if mv { "🎬  Plan the music video" } else { "📝  Write script & shot list" };
        if ui.add_enabled(ready, Button::new(RichText::new(label).strong().color(Color32::BLACK)).fill(theme::ACCENT)).clicked() {
            let ctx = ui.ctx().clone();
            start_plan(app, &ctx);
        }
        if ui.button("📂  Open saved storyboard…").clicked() {
            let dir = out_root(app);
            if let Some(p) = rfd::FileDialog::new().set_directory(&dir).add_filter("Storyboard", &["json"]).pick_file() {
                match Storyboard::load(&p) {
                    Ok(sb) => {
                        app.storyboard.aspect = sb.aspect;
                        app.storyboard.brief = sb.brief.clone();
                        app.storyboard.sb = Some(sb);
                    }
                    Err(e) => app.notify_error(format!("Could not open the storyboard: {e:#}")),
                }
            }
        }
    });
}

fn shot_card(ui: &mut Ui, app: &mut EditorApp, i: usize, n: usize, card_w: f32, busy: bool) -> (Option<CardAction>, bool) {
    let has_character = app.storyboard.sb.as_ref().is_some_and(|s| s.character.is_some());
    let sb_uses = app.storyboard.sb.as_ref().map_or(0, |s| s.uses(i));
    let mut action = None;
    let mut changed = false;
    let frame = app.storyboard.sb.as_ref().and_then(|s| s.shots[i].frame.clone());
    let aspect = app.storyboard.sb.as_ref().map_or(Aspect::Portrait, |s| s.aspect);
    let (rw, rh) = aspect.ratio();
    let img_h = (card_w * rh as f32 / rw as f32).min(card_w * 1.25);
    let img_w = img_h * rw as f32 / rh as f32;
    egui::Frame::group(ui.style()).fill(theme::PANEL_ALT).show(ui, |ui| {
        ui.set_width(card_w);
        let approved = app.storyboard.sb.as_ref().is_some_and(|s| s.shots[i].approved);
        let tex = frame.as_ref().and_then(|p| thumb(app, ui, p));
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(img_w, img_h), egui::Sense::hover());
            match tex {
                Some(t) => {
                    ui.painter().image(t, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().rect_filled(rect, 4.0, theme::SURFACE);
                    let text = if frame.is_some() { "loading…" } else if busy { "drawing…" } else { "not drawn yet" };
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), theme::TEXT_DIM);
                }
            }
            if approved {
                ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(3.0_f32, theme::ACCENT));
            }
        });
        let Some(sb) = app.storyboard.sb.as_mut() else { return };
        let shot = &mut sb.shots[i];
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Shot {}", i + 1)).strong());
            if sb_uses > 1 {
                ui.label(widgets::dim(format!("×{sb_uses}"))).on_hover_text("Used for this many cuts in the video");
            }
            ui.add_enabled_ui(!busy, |ui| {
                changed |= ui.add(egui::DragValue::new(&mut shot.seconds).range(storyboard::MIN_SHOT..=storyboard::MAX_SHOT).speed(0.1).suffix(" s")).changed();
            });
        });
        ui.add_enabled_ui(!busy, |ui| {
            let before = (shot.description.clone(), shot.motion.clone());
            changed |= ui.add(TextEdit::multiline(&mut shot.description).desired_rows(3).desired_width(card_w).hint_text("What the frame shows")).changed();
            changed |= ui.add(TextEdit::singleline(&mut shot.caption).desired_width(card_w).hint_text("On-screen caption (optional)")).changed();
            changed |= ui.add(TextEdit::singleline(&mut shot.motion).desired_width(card_w).hint_text("Motion, e.g. slow push-in")).changed();
            if has_character {
                let mut lead = !shot.no_lead;
                if ui.checkbox(&mut lead, "Lead appears in this shot").on_hover_text("Untick for shots of other people (e.g. a band member), then ↻ redraw the frame").changed() {
                    shot.no_lead = !lead;
                    changed = true;
                }
            }
            if (shot.description.clone(), shot.motion.clone()) != before {
                shot.video = None;
            }
            ui.horizontal_wrapped(|ui| {
                if shot.frame.is_some() {
                    let label = if shot.approved { RichText::new("✔ Approved").color(theme::ACCENT).strong() } else { RichText::new("Approve") };
                    if ui.selectable_label(shot.approved, label).clicked() {
                        shot.approved = !shot.approved;
                        changed = true;
                    }
                }
                if ui.small_button("↻").on_hover_text("Regenerate this frame (≈ $0.07)").clicked() {
                    action = Some(CardAction::Regenerate(i));
                }
                if ui.small_button("📂").on_hover_text("Swap in your own image").clicked() {
                    action = Some(CardAction::Swap(i));
                }
                if i > 0 && ui.small_button("◀").on_hover_text("Move earlier").clicked() {
                    action = Some(CardAction::Left(i));
                }
                if i + 1 < n && ui.small_button("▶").on_hover_text("Move later").clicked() {
                    action = Some(CardAction::Right(i));
                }
                if n > 1 && ui.small_button("🗑").on_hover_text("Remove this shot").clicked() {
                    action = Some(CardAction::Remove(i));
                }
            });
        });
    });
    (action, changed)
}

fn board_stage(ui: &mut Ui, app: &mut EditorApp) {
    let busy = app.storyboard.job.is_some();
    let Some(sb) = app.storyboard.sb.as_mut() else { return };
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            changed |= ui.add(TextEdit::singleline(&mut sb.title).font(egui::TextStyle::Heading).desired_width(420.0)).changed();
        });
        ui.label(widgets::dim(format!("{} · {} shots · {:.0} s", sb.aspect.short_label(), sb.shots.len(), sb.total_seconds())));
    });
    ui.horizontal(|ui| {
        ui.label("Look");
        ui.add_enabled_ui(!busy, |ui| {
            changed |= ui.add(TextEdit::singleline(&mut sb.style).desired_width(f32::INFINITY).hint_text("Shared visual style")).changed();
        });
    });
    egui::CollapsingHeader::new("Script").default_open(false).show(ui, |ui| {
        ui.label(&sb.script);
        if !sb.music.is_empty() {
            ui.label(widgets::dim(format!("Suggested music: {}", sb.music)));
        }
    });
    let n = sb.shots.len();
    let avail = ui.available_width();
    let cols = ((avail / 230.0).floor() as usize).clamp(1, 6);
    let card_w = (avail / cols as f32) - 22.0;
    let mut action = None;
    ScrollArea::vertical().max_height(ui.available_height() - 150.0).auto_shrink([false, true]).show(ui, |ui| {
        for row in (0..n).collect::<Vec<_>>().chunks(cols) {
            ui.horizontal_top(|ui| {
                for &i in row {
                    let (a, c) = shot_card(ui, app, i, n, card_w, busy);
                    action = action.or(a);
                    changed |= c;
                }
            });
        }
    });
    let Some(sb) = app.storyboard.sb.as_mut() else { return };
    if let Some(a) = action {
        match a {
            CardAction::Left(i) => sb.swap(i, i - 1),
            CardAction::Right(i) => sb.swap(i, i + 1),
            CardAction::Remove(i) => sb.remove_shot(i),
            CardAction::Swap(i) => {
                if let Some(p) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp"]).pick_file() {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
                    let dest = sb.dir.join(format!("shot-{:02}-own.{ext}", i + 1));
                    let _ = std::fs::create_dir_all(&sb.dir);
                    match std::fs::copy(&p, &dest) {
                        Ok(_) => {
                            sb.shots[i].frame = Some(dest);
                            sb.shots[i].approved = true;
                            sb.shots[i].video = None;
                        }
                        Err(e) => {
                            let msg = format!("Could not use that image: {e}");
                            app.notify_error(msg);
                            return;
                        }
                    }
                }
            }
            CardAction::Regenerate(i) => {
                let ctx = ui.ctx().clone();
                start_frames(app, &ctx, vec![i]);
                return;
            }
        }
        changed = true;
    }
    if changed {
        if let Some(sb) = &app.storyboard.sb {
            let _ = sb.save();
        }
    }
    footer(ui, app);
}

fn footer(ui: &mut Ui, app: &mut EditorApp) {
    let music_video = app.storyboard.sb.as_ref().is_some_and(Storyboard::is_music_video);
    ui.separator();
    if let Some(job) = &app.storyboard.job {
        let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
        ui.horizontal(|ui| {
            ui.add(ProgressBar::new(p).desired_width(360.0).text(stage).animate(true));
            if ui.button("Stop").on_hover_text("Stop waiting (jobs already submitted may still be billed)").clicked() {
                job.cancel();
            }
        });
        return;
    }
    let Some(sb) = app.storyboard.sb.clone() else { return };
    let missing = sb.shots.iter().filter(|s| s.frame.is_none()).count();
    let approved = sb.approved();
    ui.horizontal(|ui| {
        if missing > 0 {
            let text = format!("🎨  Draw storyboard: {missing} frame{} (≈ ${:.2})", if missing == 1 { "" } else { "s" }, missing as f64 * FRAME_PRICE);
            if ui.add(Button::new(RichText::new(text).strong().color(Color32::BLACK)).fill(theme::ACCENT)).clicked() {
                let ctx = ui.ctx().clone();
                start_frames(app, &ctx, Vec::new());
            }
        }
        let drawn = sb.shots.iter().filter(|s| s.frame.is_some()).count();
        if drawn > 0 && ui.button("✔ Approve all drawn").clicked() {
            if let Some(sb) = app.storyboard.sb.as_mut() {
                for s in sb.shots.iter_mut().filter(|s| s.frame.is_some()) {
                    s.approved = true;
                }
                let _ = sb.save();
            }
        }
        if ui.button("Start over").clicked() {
            app.storyboard.sb = None;
        }
        if sb.spent > 0.0 {
            ui.label(widgets::dim(format!("Spent on this storyboard: ${:.2}", sb.spent)));
        }
    });
    if sb.shots.iter().all(|s| s.frame.is_none()) {
        return;
    }

    crate::ui::video_gen::ensure_models(app, ui.ctx());
    let models: Vec<VideoModel> = app.video_gen.models.iter().filter(|m| m.first_frame).cloned().collect();
    let model = models.iter().find(|m| m.id == app.ai.settings.video_model).cloned().or_else(|| models.first().cloned());
    Grid::new("sb-produce").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Video model");
        let current = model.as_ref().map_or("loading…".to_string(), |m| m.id.clone());
        egui::ComboBox::from_id_salt("sb-vmodel").selected_text(&current).width(300.0).show_ui(ui, |ui| {
            for m in &models {
                let est = storyboard::estimate_video_cost(&sb, &sb.to_animate(&approved), m, false).map_or(String::new(), |(c, r)| format!("  ·  {}${c:.2}", if r { "≈" } else { "" }));
                if ui.selectable_label(m.id == current, format!("{}{est}", m.id)).clicked() {
                    app.ai.settings.video_model = m.id.clone();
                    let _ = app.ai.settings.save();
                }
            }
        });
        ui.end_row();
        if music_video {
            ui.label("Music");
            ui.label(widgets::dim("The song on your timeline (kept at full volume)"));
            ui.end_row();
        } else {
        ui.label("Music");
        ui.horizontal(|ui| {
            let name = app.storyboard.music.as_ref().and_then(|p| p.file_name()).map_or("None".to_string(), |n| n.to_string_lossy().into_owned());
            ui.label(name);
            if ui.small_button("Choose…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Audio", &["mp3", "wav", "m4a", "aac", "ogg", "flac"]).pick_file() {
                    app.storyboard.music = Some(p);
                }
            }
            if app.storyboard.music.is_some() && ui.small_button("✕").clicked() {
                app.storyboard.music = None;
            }
        });
        ui.end_row();
        if model.as_ref().is_some_and(|m| m.audio != Some(false)) {
            ui.label("Sound");
            ui.checkbox(&mut app.storyboard.audio, "Generate sound for each shot").on_hover_text("Costs more on most models; music is ducked under it");
            ui.end_row();
        }
        }
    });
    let Some(m) = model else {
        ui.spinner();
        return;
    };
    let new = sb.to_animate(&approved);
    let est = storyboard::estimate_video_cost(&sb, &new, &m, app.storyboard.audio);
    let price = if new.is_empty() { "$0, already animated".to_string() } else { est.map_or("price on openrouter.ai".to_string(), |(c, r)| format!("{}${c:.2}", if r { "≈ " } else { "" })) };
    let text = if new.len() == approved.len() {
        format!("🎬  Animate {} approved shot{} & assemble ({price})", approved.len(), if approved.len() == 1 { "" } else { "s" })
    } else if new.is_empty() {
        format!("🎬  Assemble {} animated shot{} again (free)", approved.len(), if approved.len() == 1 { "" } else { "s" })
    } else {
        format!("🎬  Animate {} new shot{}, reuse {} & assemble ({price})", new.len(), if new.len() == 1 { "" } else { "s" }, approved.len() - new.len())
    };
    ui.add_space(4.0);
    let w = ui.available_width();
    if ui
        .add_enabled(!approved.is_empty(), Button::new(RichText::new(text).strong().color(Color32::BLACK)).fill(theme::ACCENT).min_size(egui::vec2(w, 32.0)))
        .on_disabled_hover_text("Approve at least one drawn shot")
        .clicked()
    {
        let ctx = ui.ctx().clone();
        start_produce(app, &ctx, m);
    }
    ui.label(widgets::dim(if music_video {
        "Shots render in parallel (usually 1–4 minutes) and are cut onto the song's bars. Shots already animated are reused, not paid for again."
    } else {
        "Shots render in parallel (usually 1–4 minutes). They're added after anything already on the timeline, with cross-dissolves, captions, ducked music and −14 LUFS loudness. Shots already animated are reused."
    }).small());
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.storyboard.open {
        return;
    }
    let mut open = true;
    let screen = ctx.screen_rect();
    egui::Window::new(RichText::new("Make a video from a prompt (AI storyboard)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([screen.width().min(1100.0) - 40.0, screen.height().min(820.0) - 60.0])
        .default_pos(screen.center() - egui::vec2(screen.width().min(1100.0) / 2.0 - 20.0, screen.height().min(820.0) / 2.0 - 30.0))
        .show(ctx, |ui| {
            if app.ai.settings.api_key().is_none() {
                ui.label(RichText::new("Add your OpenRouter key first.").color(theme::DANGER));
                if ui.button("🔑  OpenRouter settings…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
                return;
            }
            if app.storyboard.sb.is_none() {
                brief_stage(ui, app);
                if matches!(app.storyboard.job, Some(Job::Plan(_))) {
                    footer_progress(ui, app);
                }
            } else {
                board_stage(ui, app);
            }
        });
    if !open {
        app.storyboard.open = false;
    }
}

fn footer_progress(ui: &mut Ui, app: &EditorApp) {
    if let Some(job) = &app.storyboard.job {
        let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
        ui.add(ProgressBar::new(p).desired_width(360.0).text(stage).animate(true));
    }
}
