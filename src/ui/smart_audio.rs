//! "Smart audio" panel (Audio tab) and the background jobs behind it.

use crate::app::EditorApp;
use crate::model::{Denoise, Id, NoiseLevel, LOUDNESS_TARGETS};
use crate::smart_audio::{self, DuckOptions, Loudness};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Button, RichText, Slider, Ui};

#[derive(Default)]
pub struct SmartAudioUi {
    loudness_job: Option<Task<Loudness>>,
    last_loudness: Option<Result<Loudness, String>>,
    noise_job: Option<(Id, NoiseLevel, Task<f32>)>,
}

impl SmartAudioUi {
    /// True while the noise floor of `clip` is being measured.
    pub fn measuring(&self, clip: Id) -> bool {
        self.noise_job.as_ref().is_some_and(|(id, _, _)| *id == clip)
    }
}

/// Turns on noise removal for a clip, measuring its noise floor in the background first.
pub fn request_denoise(app: &mut EditorApp, ctx: &egui::Context, id: Id, level: NoiseLevel) {
    let Some(clip) = app.project.clip(id).cloned() else { return };
    let project = app.project.clone();
    let task = Task::spawn(ctx.clone(), move |_, report| {
        report(0.1, "Measuring noise…");
        smart_audio::measure_noise_floor(&project, &clip).map(Some)
    });
    app.smart.noise_job = Some((id, level, task));
}

/// Applies finished background jobs. Called every frame.
pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.smart.loudness_job.as_ref().and_then(Task::take_finished) {
        app.smart.loudness_job = None;
        app.smart.last_loudness = match state {
            TaskState::Done(l) => Some(Ok(l)),
            TaskState::Failed(e) => Some(Err(e)),
            _ => None,
        };
    }
    let finished = app.smart.noise_job.as_ref().and_then(|(id, level, t)| t.take_finished().map(|s| (*id, *level, s)));
    if let Some((id, level, state)) = finished {
        app.smart.noise_job = None;
        match state {
            TaskState::Done(floor_db) => {
                if let Some(c) = app.project.clip_mut(id) {
                    c.denoise = Some(Denoise { level, floor_db });
                    app.notify(format!("Noise removal on ({}) — background noise measured at {floor_db:.0} dB", level.label()));
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("Noise removal failed: {e}")),
            _ => {}
        }
    }
}

pub fn panel(ui: &mut Ui, app: &mut EditorApp) {
    widgets::section_title(ui, "Smart audio");
    let w = ui.available_width();

    // ── auto-ducking ──
    let voice = smart_audio::voice_clips(&app.project).len();
    let music = smart_audio::music_clips(&app.project).len();
    ui.label(RichText::new("Auto-ducking").strong());
    ui.label(widgets::dim(format!("Lowers music while someone speaks. {voice} voice · {music} music clips.")).small());
    egui::Grid::new("duck-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Lower by");
        ui.add(Slider::new(&mut app.project.audio.duck_db, -30.0..=-3.0).suffix(" dB").fixed_decimals(0));
        ui.end_row();
        ui.label("Sensitivity");
        // Higher sensitivity = lower threshold.
        let mut sens = 1.0 - app.project.audio.duck_sensitivity;
        if ui.add(Slider::new(&mut sens, 0.5..=0.97).show_value(false)).on_hover_text("Raise if quiet speech is missed").changed() {
            app.project.audio.duck_sensitivity = 1.0 - sens;
        }
        ui.end_row();
    });
    ui.horizontal(|ui| {
        let ready = voice > 0 && music > 0;
        let duck = Button::new(RichText::new("🎚  Duck music under voice").strong()).fill(theme::SURFACE);
        if ui
            .add_enabled(ready, duck.min_size(egui::vec2(w * 0.62, 30.0)))
            .on_disabled_hover_text("Needs at least one voice clip and one music clip (see clip audio roles in the inspector)")
            .clicked()
        {
            duck_now(app);
        }
        if ui.add_enabled(music > 0, Button::new("Remove")).on_hover_text("Remove ducking from music clips").clicked() {
            let n = smart_audio::clear_ducking(&mut app.project);
            app.notify(format!("Removed ducking from {n} clip(s)"));
        }
    });
    ui.add_space(10.0);

    // ── loudness ──
    ui.label(RichText::new("Loudness").strong());
    let current = app.project.audio.loudness;
    let label = current
        .and_then(|t| LOUDNESS_TARGETS.iter().find(|(v, _)| (*v - t).abs() < 0.05).map(|(_, l)| l.to_string()))
        .or_else(|| current.map(|t| format!("{t:.0} LUFS")))
        .unwrap_or_else(|| "Off — export as mixed".into());
    egui::ComboBox::from_id_salt("loudness").selected_text(label).width(w - 8.0).show_ui(ui, |ui| {
        ui.selectable_value(&mut app.project.audio.loudness, None, "Off — export as mixed");
        for (v, l) in LOUDNESS_TARGETS {
            ui.selectable_value(&mut app.project.audio.loudness, Some(v), l);
        }
    });
    ui.horizontal(|ui| {
        if let Some(job) = &app.smart.loudness_job {
            ui.spinner();
            ui.label("Measuring…");
            let _ = job;
        } else if ui
            .add_enabled(app.project.duration() > 0.0, Button::new("📏  Measure loudness"))
            .on_hover_text("Analyse the current mix (before normalisation)")
            .clicked()
        {
            let project = app.project.clone();
            app.smart.loudness_job =
                Some(Task::spawn(ui.ctx().clone(), move |_, _| smart_audio::measure_loudness(&project).map(Some)));
            app.smart.last_loudness = None;
        }
        match &app.smart.last_loudness {
            Some(Ok(l)) => {
                ui.label(RichText::new(format!("{:.1} LUFS", l.integrated)).strong().color(theme::ACCENT))
                    .on_hover_text(format!("Integrated {:.1} LUFS · range {:.1} LU · true peak {:.1} dBFS", l.integrated, l.range, l.peak));
            }
            Some(Err(e)) => {
                ui.label(RichText::new("failed").color(theme::DANGER)).on_hover_text(e);
            }
            None => {}
        }
    });
    if let (Some(Ok(l)), Some(t)) = (&app.smart.last_loudness, current) {
        ui.label(widgets::dim(format!("Export will be adjusted by {:+.1} dB to reach {t:.0} LUFS.", t - l.integrated)).small());
    }
    ui.add_space(6.0);
    ui.label(widgets::dim("Noise removal and audio roles are per clip: select a clip → inspector → Audio.").small());
}

fn duck_now(app: &mut EditorApp) {
    // Waveforms of voice clips come from the background asset jobs.
    let voice_media: Vec<Id> =
        smart_audio::voice_clips(&app.project).iter().filter_map(|c| c.media_id()).collect();
    let missing: Vec<Id> = voice_media.iter().copied().filter(|m| !app.assets.waveforms.contains_key(m)).collect();
    if !missing.is_empty() {
        for m in &missing {
            if let Some(item) = app.project.media(*m).cloned() {
                app.assets.ensure(&item);
            }
        }
        app.notify("Still analysing voice audio — try again in a few seconds");
        return;
    }
    let waveforms = app.assets.waveforms.clone();
    let peaks_for = move |id: Id| waveforms.get(&id).cloned();
    let opts = DuckOptions::from_mix(&app.project.audio);
    match smart_audio::auto_duck(&mut app.project, &peaks_for, &opts) {
        Ok(r) if r.speech.is_empty() => app.notify("No speech detected — try a higher sensitivity"),
        Ok(r) => {
            let secs: f64 = r.speech.iter().map(|(a, b)| b - a).sum();
            app.notify(format!(
                "Ducked {} music clip(s) under {:.0}s of speech in {} passage(s) — see the yellow volume line",
                r.music_clips,
                secs,
                r.speech.len()
            ));
        }
        Err(e) => app.notify_error(e),
    }
}
