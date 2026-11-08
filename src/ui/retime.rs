//! Background media jobs from the timeline: reverse a clip, speed-ramp a clip
//! (both render a new file and swap it in), and make preview proxies.

use crate::app::EditorApp;
use crate::media::retime::{self, Ramp};
use crate::media::{ffmpeg, proxy};
use crate::model::{ClipSource, Id, MediaKind};
use crate::task::{Task, TaskState};
use egui::Context;
use std::path::PathBuf;

/// A finished render to swap into a clip: (clip, the media it used, new file, what).
type Swap = (Id, Id, PathBuf, String);

#[derive(Default)]
pub struct RetimeUi {
    job: Option<Task<Swap>>,
    proxies: Option<Task<usize>>,
    /// Optical-flow slow motion for ramps (much slower to render).
    pub smooth: bool,
}

impl RetimeUi {
    pub fn progress(&self) -> Option<(f32, String)> {
        self.job.as_ref().and_then(|j| j.progress()).or_else(|| self.proxies.as_ref().and_then(|j| j.progress()))
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn making_proxies(&self) -> bool {
        self.proxies.is_some()
    }
}

fn out_dir(app: &EditorApp) -> PathBuf {
    match &app.project_path {
        Some(p) => p.parent().map(|d| d.join("RustCut AI").join("retimed")).unwrap_or_default(),
        None => crate::util::config_dir().join("retimed"),
    }
}

/// (media id, source path, start, span, has sound) for a video clip.
fn source(app: &EditorApp, id: Id) -> Result<(Id, PathBuf, f64, f64, bool), String> {
    let c = app.project.clip(id).ok_or("That clip no longer exists")?;
    let m = c.media_id().and_then(|m| app.project.media(m)).ok_or("Pick a video clip")?;
    if m.kind != MediaKind::Video {
        return Err("Reverse and speed ramps work on video clips".into());
    }
    if !m.path.is_file() {
        return Err(format!("File not found: {}", m.path.display()));
    }
    let span = (c.duration * c.speed).min((m.duration - c.source_in).max(0.05));
    Ok((m.id, m.path.clone(), c.source_in, span, m.has_sound() && c.volume > 0.001))
}

pub fn start_reverse(app: &mut EditorApp, ctx: &Context, id: Id) -> Result<(), String> {
    if app.retime.busy() {
        return Err("Another reverse / speed ramp is still rendering".into());
    }
    let (mid, path, a, span, sound) = source(app, id)?;
    let dir = out_dir(app);
    app.retime.job = Some(Task::spawn(ctx.clone(), move |cancel, progress| {
        Ok(retime::reverse(&path, a, span, sound, &dir, cancel, progress)?.map(|p| (id, mid, p, "Reversed".to_string())))
    }));
    Ok(())
}

pub fn start_ramp(app: &mut EditorApp, ctx: &Context, id: Id, ramp: Ramp, smooth: bool) -> Result<(), String> {
    if app.retime.busy() {
        return Err("Another reverse / speed ramp is still rendering".into());
    }
    let (mid, path, a, span, _) = source(app, id)?;
    let out_len = app.project.clip(id).map(|c| c.duration).unwrap_or(span);
    let (dir, fps) = (out_dir(app), app.project.fps.max(24));
    app.retime.job = Some(Task::spawn(ctx.clone(), move |cancel, progress| {
        Ok(retime::ramp(&path, a, span, out_len, ramp, fps, smooth, &dir, cancel, progress)?.map(|p| (id, mid, p, format!("Speed ramp: {}", ramp.label()))))
    }));
    Ok(())
}

/// Makes proxies for every big video in the project that lacks one.
pub fn start_proxies(app: &mut EditorApp, ctx: &Context) -> usize {
    let todo: Vec<_> = app.project.media.iter().filter(|m| proxy::wanted(m) && !proxy::exists(m) && m.path.is_file()).cloned().collect();
    let n = todo.len();
    if n == 0 || app.retime.proxies.is_some() {
        return 0;
    }
    app.retime.proxies = Some(Task::spawn(ctx.clone(), move |cancel, progress| {
        let mut done = 0;
        for (i, m) in todo.iter().enumerate() {
            let label = format!("Proxy {} of {n}: {}", i + 1, m.name);
            if !proxy::make(m, cancel, |p| progress((i as f32 + p) / n as f32, &label))? {
                return Ok(None);
            }
            done += 1;
        }
        Ok(Some(done))
    }));
    n
}

/// (have, need) proxies for the project's big videos.
pub fn proxy_counts(app: &EditorApp) -> (usize, usize) {
    let big: Vec<_> = app.project.media.iter().filter(|m| proxy::wanted(m)).collect();
    (big.iter().filter(|m| proxy::exists(m)).count(), big.len())
}

pub fn cancel(app: &mut EditorApp) {
    if let Some(j) = &app.retime.job {
        j.cancel();
    }
    if let Some(j) = &app.retime.proxies {
        j.cancel();
    }
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.retime.proxies.as_ref().and_then(|j| j.take_finished()) {
        app.retime.proxies = None;
        match state {
            TaskState::Done(n) => app.notify(format!("Made {n} preview prox{} — scrubbing big videos is smoother now", if n == 1 { "y" } else { "ies" })),
            TaskState::Failed(e) => app.notify_error(format!("Proxies: {e}")),
            _ => {}
        }
    }
    let Some(state) = app.retime.job.as_ref().and_then(|j| j.take_finished()) else { return };
    app.retime.job = None;
    match state {
        TaskState::Done((id, old_media, path, what)) => {
            // Only swap if the clip still uses the same media.
            let same = app.project.clip(id).is_some_and(|c| c.media_id() == Some(old_media));
            if !same {
                return app.notify_error(format!("{what} finished, but the clip changed meanwhile — the file is in {}", path.display()));
            }
            let info = match ffmpeg::probe(&path) {
                Ok(i) => i,
                Err(e) => return app.notify_error(format!("{what}: {e:#}")),
            };
            let duration = info.duration;
            let media = app.project.add_media(info.into_media_item(path));
            if let Some(c) = app.project.clip_mut(id) {
                let ramped = what.starts_with("Speed ramp");
                c.source = ClipSource::Media(media);
                c.source_in = 0.0;
                if ramped {
                    // The file already has the new timing and the clip's length.
                    c.speed = 1.0;
                    c.duration = c.duration.min(duration);
                } else {
                    c.duration = c.duration.min(duration / c.speed.max(0.01));
                }
            }
            app.selection = Some(id);
            app.notify(format!("{what} — Ctrl+Z to undo"));
        }
        TaskState::Failed(e) => app.notify_error(e),
        _ => {}
    }
}
