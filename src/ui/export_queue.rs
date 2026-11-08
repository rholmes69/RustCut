//! Export presets and the render queue: pick several versions (YouTube 16:9,
//! Shorts 9:16, 4:5, square, a quick draft, a 4K master) and they export one after
//! another in the background. Other shapes re-fit the picture to fill the frame.

use crate::app::EditorApp;
use crate::export::{ExportJob, ExportSettings, ExportState};
use crate::model::{Aspect, ClipSource, MediaKind, Project, TrackKind};
use crate::ui::{theme, widgets};
use egui::{Context, ProgressBar, RichText, Ui};
use std::path::{Path, PathBuf};

pub struct Preset {
    pub name: &'static str,
    /// `None` = the project's own shape.
    pub aspect: Option<Aspect>,
    pub short_side: u32,
    pub crf: u8,
    pub speed: &'static str,
    /// Added to the file name.
    pub suffix: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "YouTube · 16:9 · 1080p", aspect: Some(Aspect::Landscape), short_side: 1080, crf: 21, speed: "medium", suffix: "" },
    Preset { name: "Shorts / Reels / TikTok · 9:16 · 1080p", aspect: Some(Aspect::Portrait), short_side: 1080, crf: 21, speed: "medium", suffix: " (9x16)" },
    Preset { name: "Instagram post · 4:5 · 1080p", aspect: Some(Aspect::Social), short_side: 1080, crf: 21, speed: "medium", suffix: " (4x5)" },
    Preset { name: "Square · 1:1 · 1080p", aspect: Some(Aspect::Square), short_side: 1080, crf: 21, speed: "medium", suffix: " (1x1)" },
    Preset { name: "Quick draft · 720p", aspect: None, short_side: 720, crf: 28, speed: "veryfast", suffix: " (draft)" },
    Preset { name: "Master · 4K · high quality", aspect: None, short_side: 2160, crf: 17, speed: "slow", suffix: " (4K)" },
];

#[derive(Clone, Debug)]
enum QState {
    Waiting,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

struct Queued {
    label: String,
    project: Project,
    settings: ExportSettings,
    state: QState,
}

#[derive(Default)]
pub struct ExportQueue {
    pub open: bool,
    /// The file the versions are named after, and the frame rate.
    pub base: PathBuf,
    pub fps: u32,
    pub picked: Vec<bool>,
    items: Vec<Queued>,
    running: Option<(usize, ExportJob)>,
}

impl ExportQueue {
    pub fn active(&self) -> bool {
        self.running.is_some() || self.items.iter().any(|i| matches!(i.state, QState::Waiting))
    }
}

/// The project re-shaped for `aspect`: video and image layers that were simply
/// fitted (centred, not scaled or animated) are scaled to fill the new frame, so a
/// 16:9 shot fills a 9:16 Short (centre crop) instead of floating in black bars.
pub fn reshaped(project: &Project, aspect: Aspect) -> Project {
    let mut p = project.clone();
    if p.aspect == aspect {
        return p;
    }
    p.aspect = aspect;
    let (cw, ch) = aspect.size_for(1080);
    let (ow, oh) = project.aspect.size_for(1080);
    let media: std::collections::HashMap<_, _> = p.media.iter().map(|m| (m.id, (m.kind, m.width as f64, m.height as f64))).collect();
    for t in p.tracks.iter_mut().filter(|t| t.kind == TrackKind::Video) {
        for c in &mut t.clips {
            let ClipSource::Media(mid) = c.source else { continue };
            let Some(&(kind, w, h)) = media.get(&mid) else { continue };
            if kind == MediaKind::Audio || c.has_keyframes() {
                continue;
            }
            let tr = &mut c.transform;
            let plain = tr.x.abs() < 0.01 && tr.y.abs() < 0.01 && tr.rotation.abs() < 0.01;
            // Filled the old frame (scale ≈ its fill scale) or just fitted (≈ 1).
            let old_fill = crate::render::fill_scale(w, h, ow as f64, oh as f64) as f32;
            if plain && ((tr.scale - 1.0).abs() < 0.02 || (tr.scale - old_fill).abs() < 0.02) {
                tr.scale = crate::render::fill_scale(w, h, cw as f64, ch as f64) as f32;
            }
        }
    }
    p
}

fn output_path(base: &Path, suffix: &str) -> PathBuf {
    let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    base.with_file_name(format!("{stem}{suffix}.mp4"))
}

/// Adds the picked presets to the queue (and starts it).
pub fn enqueue(app: &mut EditorApp, base: &Path, fps: u32) -> usize {
    let picked: Vec<usize> = (0..PRESETS.len()).filter(|i| app.export_queue.picked.get(*i).copied().unwrap_or(false)).collect();
    for &i in &picked {
        let pr = &PRESETS[i];
        let aspect = pr.aspect.unwrap_or(app.project.aspect);
        let project = reshaped(&app.project, aspect);
        let settings = ExportSettings { path: output_path(base, pr.suffix), short_side: pr.short_side, fps, crf: pr.crf, preset: pr.speed.into() };
        app.export_queue.items.push(Queued { label: pr.name.into(), project, settings, state: QState::Waiting });
    }
    picked.len()
}

/// Starts the next waiting export when nothing is running.
pub fn poll(ctx: &Context, app: &mut EditorApp) {
    let q = &mut app.export_queue;
    if let Some((i, job)) = &q.running {
        let st = match job.state() {
            ExportState::Running(_) => return,
            ExportState::Done(_) => QState::Done,
            ExportState::Failed(e) => QState::Failed(e),
            ExportState::Cancelled => QState::Cancelled,
        };
        let i = *i;
        q.items[i].state = st;
        q.running = None;
        if !q.active() {
            let ok = q.items.iter().filter(|x| matches!(x.state, QState::Done)).count();
            app.notify(format!("Batch export finished: {ok} of {} versions done", app.export_queue.items.len()));
            return;
        }
    }
    let q = &mut app.export_queue;
    if let Some(i) = q.items.iter().position(|x| matches!(x.state, QState::Waiting)) {
        q.items[i].state = QState::Running;
        let job = ExportJob::start(q.items[i].project.clone(), q.items[i].settings.clone(), ctx.clone());
        q.running = Some((i, job));
    }
}

/// Opens the batch window, naming the versions after the project's default export file.
pub fn show(app: &mut EditorApp) {
    if app.export_queue.base.as_os_str().is_empty() {
        app.export_queue.base = crate::app::default_output_path(&app.project, app.project_path.as_deref());
    }
    if app.export_queue.fps == 0 {
        app.export_queue.fps = app.project.fps;
    }
    app.export_queue.open = true;
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.export_queue.open {
        return;
    }
    let mut open = true;
    let (base, fps) = (app.export_queue.base.clone(), app.export_queue.fps.max(1));
    egui::Window::new(RichText::new("📦 Batch export").strong()).id(egui::Id::new("batch-export-win")).open(&mut open).default_width(420.0).resizable(false).show(ctx, |ui| {
        ui.label(widgets::dim(format!("Named after {} · {fps} fps", base.display())).small());
        section(ui, app, &base, fps);
    });
    if !open {
        app.export_queue.open = false;
        if app.export_queue.active() {
            app.notify("The render queue keeps going in the background — File → Batch export to check it");
        }
    }
}

/// The preset checklist and the queue.
fn section(ui: &mut Ui, app: &mut EditorApp, base: &Path, fps: u32) {
    if app.export_queue.picked.len() != PRESETS.len() {
        app.export_queue.picked = vec![false; PRESETS.len()];
    }
    ui.vertical(|ui| {
        let own = app.project.aspect;
        for (i, pr) in PRESETS.iter().enumerate() {
            let file = output_path(base, pr.suffix);
            let reshape = pr.aspect.is_some_and(|a| a != own);
            let r = ui.checkbox(&mut app.export_queue.picked[i], pr.name);
            r.on_hover_text(format!(
                "{}{}",
                file.file_name().unwrap_or_default().to_string_lossy(),
                if reshape { "\nThe picture is re-fitted to fill this shape (centre crop). For smart framing, run AI → Auto-reframe on a copy of the project." } else { "" }
            ));
        }
        let n = app.export_queue.picked.iter().filter(|p| **p).count();
        ui.horizontal(|ui| {
            if ui.add_enabled(n > 0, egui::Button::new(format!("Export {n} version{}", if n == 1 { "" } else { "s" }))).clicked() {
                enqueue(app, base, fps);
                let ctx = ui.ctx().clone();
                poll(&ctx, app);
            }
            ui.label(widgets::dim("Saved next to the file above, one after another in the background.").small());
        });
    });
    queue_list(ui, app);
}

fn queue_list(ui: &mut Ui, app: &mut EditorApp) {
    if app.export_queue.items.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Render queue").strong());
    let running = app.export_queue.running.as_ref().map(|(i, j)| (*i, j.state()));
    let mut cancel = false;
    for (i, it) in app.export_queue.items.iter().enumerate() {
        ui.horizontal(|ui| {
            let name = it.settings.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            match &it.state {
                QState::Waiting => {
                    ui.label(widgets::dim("⏳"));
                }
                QState::Running => {
                    let p = match &running {
                        Some((ri, ExportState::Running(p))) if *ri == i => *p,
                        _ => 0.0,
                    };
                    ui.add(ProgressBar::new(p).desired_width(90.0).show_percentage());
                }
                QState::Done => {
                    ui.label(RichText::new("✅").color(theme::ACCENT));
                }
                QState::Failed(e) => {
                    ui.label(RichText::new("⚠").color(theme::DANGER)).on_hover_text(e);
                }
                QState::Cancelled => {
                    ui.label(widgets::dim("✖"));
                }
            }
            ui.label(RichText::new(&it.label).small());
            ui.add(egui::Label::new(widgets::dim(name).small()).truncate()).on_hover_text(it.settings.path.display().to_string());
            if matches!(it.state, QState::Done) && ui.small_button("📂").on_hover_text("Show in folder").clicked() {
                crate::ui::library::reveal(&it.settings.path);
            }
        });
    }
    ui.horizontal(|ui| {
        if app.export_queue.active() && ui.button("Cancel the queue").clicked() {
            cancel = true;
        }
        if !app.export_queue.active() && ui.small_button("Clear list").clicked() {
            app.export_queue.items.clear();
        }
    });
    if cancel {
        let q = &mut app.export_queue;
        for it in q.items.iter_mut().filter(|x| matches!(x.state, QState::Waiting)) {
            it.state = QState::Cancelled;
        }
        if let Some((_, j)) = &q.running {
            j.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaItem;

    #[test]
    fn reshaping_fills_the_new_frame() {
        let mut p = Project::default();
        let m = p.add_media(MediaItem { id: 0, path: "a.mp4".into(), name: "a".into(), kind: MediaKind::Video, duration: 10.0, width: 1920, height: 1080, fps: 30.0, has_audio: true });
        let main = p.main_track();
        let c = p.new_media_clip(m, 0.0).unwrap();
        let fitted = c.id;
        p.tracks[main].clips.push(c);
        let mut moved = p.new_media_clip(m, 10.0).unwrap();
        moved.transform.x = 0.3;
        let moved_id = moved.id;
        p.tracks[main].clips.push(moved);
        let v = reshaped(&p, Aspect::Portrait);
        assert_eq!(v.aspect, Aspect::Portrait);
        let s = v.clip(fitted).unwrap().transform.scale;
        assert!((s - 1920.0 / 1080.0 * 1920.0 / 1080.0).abs() < 0.01, "16:9 in 9:16 fills at ~3.16×, got {s}");
        assert_eq!(v.clip(moved_id).unwrap().transform.scale, 1.0, "hand-placed layers are left alone");
        assert_eq!(output_path(Path::new("C:/v/Song.mp4"), " (9x16)"), PathBuf::from("C:/v/Song (9x16).mp4"));
    }
}
