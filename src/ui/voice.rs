//! "Dialogue & voices" window: ElevenLabs key, account voices, character → voice
//! profiles, speaking a line onto the timeline, and instant voice cloning.

use crate::ai::elevenlabs::{self, Client, Subscription, Voice, VoiceProfile, VoiceSettings};
use crate::app::EditorApp;
use crate::model::{AudioRole, Id, MediaKind, TrackKind};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Color32, Context, Grid, RichText, ScrollArea, Slider, TextEdit};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

type Peaks = HashMap<PathBuf, Arc<Vec<f32>>>;

#[derive(Default)]
pub struct VoiceUi {
    pub open: bool,
    key_input: String,
    voices: Vec<Voice>,
    voices_job: Option<Task<(Vec<Voice>, Subscription)>>,
    status: Option<Result<Subscription, String>>,
    character: String,
    text: String,
    settings: Option<VoiceSettings>,
    speak_job: Option<Task<(PathBuf, crate::media::ffmpeg::ProbeInfo, Peaks)>>,
    speak_at: f64,
    new_profile: String,
    clone_name: String,
    clone_files: Vec<PathBuf>,
    clone_rights: bool,
    clone_job: Option<Task<String>>,
    fal_input: String,
    /// The video clip chosen for lip-sync (from the clip's right-click menu or the selection).
    pub lipsync_target: Option<Id>,
    lipsync_job: Option<Task<(crate::ai::lipsync::Plan, PathBuf, crate::media::ffmpeg::ProbeInfo)>>,
    perf_direction: String,
    perf_sheet: Option<PathBuf>,
    perf_job: Option<Task<(crate::ai::performance::Plan, Vec<(PathBuf, crate::media::ffmpeg::ProbeInfo)>)>>,
}

pub(crate) fn start_performance(app: &mut EditorApp, ctx: &Context, plan: crate::ai::performance::Plan) {
    let Some(k) = app.ai.settings.fal_key() else { return };
    let project = app.project.clone();
    let model = app.ai.settings.performance_model.clone();
    let direction = app.voice.perf_direction.clone();
    let sheet = app.voice.perf_sheet.clone();
    let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref()).join("lipsync");
    app.voice.perf_job = Some(Task::spawn(ctx.clone(), move |cancel, report| {
        let opts = crate::ai::performance::Options { model: &model, direction: &direction, sheet: sheet.as_deref() };
        Ok(crate::ai::performance::perform(&project, &plan, &k, &opts, &dir, cancel, report)?.map(|r| (plan, r)))
    }));
}

/// Opens the lip-sync section for `clip` (from the timeline's right-click menu).
pub fn open_lipsync(app: &mut EditorApp, clip: Id) {
    app.voice.open = true;
    app.voice.lipsync_target = Some(clip);
}

fn start_lipsync(app: &mut EditorApp, ctx: &Context, plan: crate::ai::lipsync::Plan) {
    let Some(k) = app.ai.settings.fal_key() else { return };
    let project = app.project.clone();
    let model = app.ai.settings.lipsync_model.clone();
    let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref()).join("lipsync");
    app.voice.lipsync_job = Some(Task::spawn(ctx.clone(), move |cancel, report| {
        Ok(crate::ai::lipsync::lip_sync(&project, &plan, &k, &model, &dir, cancel, report)?.map(|(p, i)| (plan, p, i)))
    }));
}

fn key(app: &EditorApp) -> Option<String> {
    app.ai.settings.elevenlabs_key()
}

fn refresh(app: &mut EditorApp, ctx: &Context) {
    let Some(k) = key(app) else { return };
    app.voice.voices_job = Some(Task::spawn(ctx.clone(), move |_, _| {
        let c = Client::new(k);
        Ok(Some((c.voices()?, c.subscription()?)))
    }));
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.voice.voices_job.as_ref().and_then(Task::take_finished) {
        app.voice.voices_job = None;
        match state {
            TaskState::Done((voices, sub)) => {
                // Create a profile for each cloned voice that has none yet.
                let mut added = false;
                for v in voices.iter().filter(|v| v.category == "cloned") {
                    if !app.ai.settings.voices.iter().any(|p| p.voice_id == v.voice_id) {
                        app.ai.settings.voices.push(VoiceProfile { name: v.name.clone(), voice_id: v.voice_id.clone() });
                        added = true;
                    }
                }
                if added {
                    let _ = app.ai.settings.save();
                }
                app.voice.voices = voices;
                app.voice.status = Some(Ok(sub));
            }
            TaskState::Failed(e) => app.voice.status = Some(Err(e)),
            _ => {}
        }
    }
    if let Some(state) = app.voice.speak_job.as_ref().and_then(Task::take_finished) {
        app.voice.speak_job = None;
        match state {
            TaskState::Done((path, info, peaks)) => place_line(app, path, info, peaks),
            TaskState::Failed(e) => app.notify_error(format!("Dialogue: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.voice.lipsync_job.as_ref().and_then(Task::take_finished) {
        app.voice.lipsync_job = None;
        match state {
            TaskState::Done((plan, path, info)) => match crate::ai::lipsync::apply(&mut app.project, &plan, &path, info) {
                Ok(id) => {
                    if let Some(m) = app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).cloned() {
                        app.assets.ensure(&m);
                    }
                    app.selection = Some(id);
                    app.voice.lipsync_target = None;
                    app.notify(format!("👄 Lip-synced {:.1} s of the shot to the dialogue", plan.end - plan.start));
                }
                Err(e) => app.notify_error(format!("Lip-sync: {e:#}")),
            },
            TaskState::Failed(e) => app.notify_error(format!("Lip-sync: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.voice.perf_job.as_ref().and_then(Task::take_finished) {
        app.voice.perf_job = None;
        match state {
            TaskState::Done((plan, results)) => {
                let items: Vec<_> = results.iter().map(|(p, i)| i.clone().into_media_item(p.clone())).collect();
                match crate::ai::performance::apply(&mut app.project, &plan, results) {
                    Ok(ids) => {
                        for item in &items {
                            if let Some(m) = app.project.media.iter().find(|m| m.path == item.path).cloned() {
                                app.assets.ensure(&m);
                            }
                        }
                        app.selection = ids.first().copied();
                        app.voice.lipsync_target = None;
                        app.notify(format!("🎤 Performance added: {} clip(s) over {:.1} s, synced to the song", ids.len(), plan.end() - plan.start()));
                    }
                    Err(e) => app.notify_error(format!("Performance: {e:#}")),
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("Performance: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.voice.clone_job.as_ref().and_then(Task::take_finished) {
        app.voice.clone_job = None;
        match state {
            TaskState::Done(id) => {
                let name = app.voice.clone_name.trim().to_string();
                app.ai.settings.voices.retain(|p| p.name.to_lowercase() != name.to_lowercase());
                app.ai.settings.voices.push(VoiceProfile { name: name.clone(), voice_id: id });
                let _ = app.ai.settings.save();
                app.notify(format!("🗣 Voice clone \"{name}\" created in your ElevenLabs account"));
                app.voice.clone_files.clear();
                app.voice.clone_rights = false;
                app.voice.voices.clear();
            }
            TaskState::Failed(e) => app.notify_error(format!("Voice clone: {e}")),
            _ => {}
        }
    }
}

/// Puts a generated line on an audio track as a voice clip and ducks the music under it.
fn place_line(app: &mut EditorApp, path: PathBuf, info: crate::media::ffmpeg::ProbeInfo, peaks: Peaks) {
    let media = app.project.add_media(info.into_media_item(path.clone()));
    let Some(mut clip) = app.project.new_media_clip(media, app.voice.speak_at) else { return };
    clip.start = app.voice.speak_at;
    clip.audio_role = AudioRole::Voice;
    let id = clip.id;
    app.project.place_clip(clip, None);
    if let Some(item) = app.project.media(media).cloned() {
        app.assets.ensure(&item);
    }
    let by_path: HashMap<Id, PathBuf> = app.project.media.iter().map(|m| (m.id, m.path.clone())).collect();
    let peaks_for = |m: Id| by_path.get(&m).and_then(|p| peaks.get(p)).cloned();
    let opts = crate::smart_audio::DuckOptions::from_mix(&app.project.audio);
    let ducked = crate::smart_audio::auto_duck(&mut app.project, &peaks_for, &opts).is_ok();
    app.selection = Some(id);
    app.notify(format!("🗣 Line added at {}{}", widgets::format_short(app.voice.speak_at), if ducked { " — music lowered under it" } else { "" }));
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.voice.open {
        return;
    }
    if app.voice.voices.is_empty() && app.voice.voices_job.is_none() && app.voice.status.is_none() && key(app).is_some() {
        refresh(app, ctx);
    }
    let mut open = true;
    let mut save = false;
    egui::Window::new(RichText::new("Dialogue & voices (ElevenLabs)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ScrollArea::vertical().max_height(ctx.screen_rect().height() - 160.0).show(ui, |ui| {
                // ── key ──
                ui.label(RichText::new("ElevenLabs account").strong());
                ui.horizontal(|ui| {
                    match (elevenlabs::env_key().is_some(), app.ai.settings.elevenlabs_key.is_some()) {
                        (true, _) => ui.label(RichText::new("✔ Key from ELEVENLABS_API_KEY").color(theme::ACCENT)),
                        (false, true) => ui.label(RichText::new("✔ Key saved").color(theme::ACCENT)),
                        _ => ui.label(RichText::new("No key yet").color(theme::DANGER)),
                    };
                    match &app.voice.status {
                        Some(Ok(s)) if s.limit > 0 => {
                            ui.label(widgets::dim(format!("· {} plan · {} of {} credits used", s.tier, s.used, s.limit)));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new("✖ couldn't connect").color(theme::DANGER)).on_hover_text(e);
                        }
                        _ => {}
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut app.voice.key_input).password(true).hint_text("paste your ElevenLabs API key").desired_width(260.0));
                    if ui.add_enabled(!app.voice.key_input.trim().is_empty(), Button::new("Save")).clicked() {
                        app.ai.settings.elevenlabs_key = Some(app.voice.key_input.trim().to_string());
                        app.voice.key_input.clear();
                        app.voice.status = None;
                        app.voice.voices.clear();
                        save = true;
                    }
                    if app.ai.settings.elevenlabs_key.is_some() && ui.button("Remove").clicked() {
                        app.ai.settings.elevenlabs_key = None;
                        app.voice.status = None;
                        save = true;
                    }
                    if ui.add_enabled(key(app).is_some() && app.voice.voices_job.is_none(), Button::new("↻ Refresh voices")).clicked() {
                        let ctx = ui.ctx().clone();
                        refresh(app, &ctx);
                    }
                    if app.voice.voices_job.is_some() {
                        ui.spinner();
                    }
                });
                ui.label(widgets::dim("Saved in your user profile, never in projects. Find the key under your ElevenLabs profile → API keys.").small());
                if key(app).is_none() {
                    return;
                }

                // ── characters ──
                ui.separator();
                ui.label(RichText::new("Characters").strong());
                ui.label(widgets::dim("Which ElevenLabs voice speaks for each person. Cloned voices are added automatically.").small());
                let voices = app.voice.voices.clone();
                let mut remove: Option<usize> = None;
                Grid::new("voice-profiles").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                    for (i, p) in app.ai.settings.voices.iter_mut().enumerate() {
                        if ui.add(TextEdit::singleline(&mut p.name).desired_width(140.0)).lost_focus() {
                            save = true;
                        }
                        let label = voices.iter().find(|v| v.voice_id == p.voice_id).map_or_else(|| p.voice_id.clone(), |v| format!("{} ({})", v.name, v.category));
                        egui::ComboBox::from_id_salt(("voice-of", i)).selected_text(label).width(220.0).show_ui(ui, |ui| {
                            for v in &voices {
                                if ui.selectable_label(v.voice_id == p.voice_id, format!("{} ({})", v.name, v.category)).clicked() {
                                    p.voice_id = v.voice_id.clone();
                                    save = true;
                                }
                            }
                        });
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    app.ai.settings.voices.remove(i);
                    save = true;
                }
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut app.voice.new_profile).hint_text("new character, e.g. Aidan B.").desired_width(160.0));
                    if ui.add_enabled(!app.voice.new_profile.trim().is_empty() && !voices.is_empty(), Button::new("+ Add")).clicked() {
                        let name = app.voice.new_profile.trim().to_string();
                        let first = voices.first().map(|v| v.voice_id.clone()).unwrap_or_default();
                        app.ai.settings.voices.push(VoiceProfile { name, voice_id: first });
                        app.voice.new_profile.clear();
                        save = true;
                    }
                });

                // ── speak a line ──
                ui.separator();
                ui.label(RichText::new("Speak a line").strong());
                if app.ai.settings.voices.is_empty() {
                    ui.label(widgets::dim("Add a character above first."));
                } else {
                    if app.voice.character.is_empty() || app.ai.settings.voice(&app.voice.character).is_none() {
                        app.voice.character = app.ai.settings.voices[0].name.clone();
                    }
                    let s = app.voice.settings.get_or_insert_with(VoiceSettings::default);
                    Grid::new("speak-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Who");
                        egui::ComboBox::from_id_salt("speak-who").selected_text(&app.voice.character).show_ui(ui, |ui| {
                            for p in &app.ai.settings.voices {
                                ui.selectable_value(&mut app.voice.character, p.name.clone(), &p.name);
                            }
                        });
                        ui.end_row();
                        ui.label("Says");
                        ui.add(TextEdit::multiline(&mut app.voice.text).desired_rows(3).desired_width(330.0).hint_text("e.g. You think I don't see the script? Girl, the curtain's falling."));
                        ui.end_row();
                        ui.label("Model");
                        let current = app.ai.settings.tts_model.clone();
                        let label = elevenlabs::MODELS.iter().find(|m| m.0 == current).map_or(current.clone(), |m| m.1.to_string());
                        egui::ComboBox::from_id_salt("speak-model").selected_text(label).show_ui(ui, |ui| {
                            for (id, label) in elevenlabs::MODELS {
                                if ui.selectable_label(*id == current, *label).clicked() {
                                    app.ai.settings.tts_model = id.to_string();
                                    save = true;
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Delivery");
                        ui.vertical(|ui| {
                            ui.add(Slider::new(&mut s.stability, 0.0..=1.0).text("stability (lower = more emotion)"));
                            ui.add(Slider::new(&mut s.similarity, 0.0..=1.0).text("likeness"));
                            ui.add(Slider::new(&mut s.style, 0.0..=1.0).text("style exaggeration"));
                        });
                        ui.end_row();
                    });
                    let chars = app.voice.text.chars().count();
                    ui.label(widgets::dim(format!(
                        "{chars} characters ≈ {chars} credits (≈ ${:.3}). Placed at the playhead ({}) on an audio track; music is lowered under it.",
                        chars as f64 * app.ai.settings.tts_usd_per_char,
                        widgets::format_short(app.playhead)
                    )).small());
                    if let Some(job) = &app.voice.speak_job {
                        let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
                        ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
                    } else if ui
                        .add_enabled(chars > 0, Button::new(RichText::new("🗣  Generate & add at playhead").strong().color(Color32::BLACK)).fill(theme::ACCENT))
                        .clicked()
                    {
                        if let (Some(k), Some(profile)) = (key(app), app.ai.settings.voice(&app.voice.character)) {
                            let text = app.voice.text.trim().to_string();
                            let model = app.ai.settings.tts_model.clone();
                            let settings = *app.voice.settings.as_ref().unwrap();
                            let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref()).join("voice");
                            let rate = app.ai.settings.tts_usd_per_char;
                            let audio: Vec<PathBuf> = app.project.media.iter().filter(|m| m.kind == MediaKind::Audio || m.has_sound()).map(|m| m.path.clone()).collect();
                            app.voice.speak_at = app.playhead;
                            app.voice.speak_job = Some(Task::spawn(ui.ctx().clone(), move |_, report| {
                                report(0.2, "Speaking…");
                                let path = elevenlabs::speak_to_file(&Client::new(k), &profile, &text, &model, settings, &dir, rate)?;
                                let info = crate::media::ffmpeg::probe(&path)?;
                                report(0.8, "Preparing the music duck…");
                                let mut peaks = Peaks::new();
                                for p in audio.iter().chain(std::iter::once(&path)) {
                                    if let Ok(w) = crate::media::ffmpeg::waveform(p) {
                                        peaks.insert(p.clone(), Arc::new(w));
                                    }
                                }
                                Ok(Some((path, info, peaks)))
                            }));
                        }
                    }
                }

                // ── lip-sync ──
                ui.separator();
                lipsync_section(ui, app, &mut save);

                // ── clone ──
                ui.separator();
                egui::CollapsingHeader::new(RichText::new("Clone a voice from reference clips").strong()).default_open(false).show(ui, |ui| {
                    ui.label(widgets::dim("Instant voice clone in your ElevenLabs account from clean speech (10 s minimum, 1–2 minutes is better). Uses your plan's cloning allowance.").small());
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(TextEdit::singleline(&mut app.voice.clone_name).hint_text("e.g. Aidan B.").desired_width(180.0));
                    });
                    for f in &app.voice.clone_files {
                        ui.label(widgets::dim(format!("• {}", f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).small());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Add clips…").clicked() {
                            if let Some(files) = rfd::FileDialog::new().add_filter("Audio", &["wav", "mp3", "m4a", "flac"]).pick_files() {
                                app.voice.clone_files.extend(files);
                            }
                        }
                        if !app.voice.clone_files.is_empty() && ui.button("Clear").clicked() {
                            app.voice.clone_files.clear();
                        }
                    });
                    ui.checkbox(&mut app.voice.clone_rights, "I have the rights to clone this person's voice");
                    if app.voice.clone_job.is_some() {
                        ui.spinner();
                    } else if ui
                        .add_enabled(app.voice.clone_rights && !app.voice.clone_files.is_empty() && !app.voice.clone_name.trim().is_empty(), Button::new("Create voice clone"))
                        .clicked()
                    {
                        if let Some(k) = key(app) {
                            let (name, files) = (app.voice.clone_name.trim().to_string(), app.voice.clone_files.clone());
                            app.voice.clone_job = Some(Task::spawn(ui.ctx().clone(), move |_, _| {
                                Client::new(k).clone_voice(&name, "Created with RustCut from voice reference clips", &files).map(Some)
                            }));
                        }
                    }
                });
            });
        });
    if save {
        if let Err(e) = app.ai.settings.save() {
            app.notify_error(format!("Could not save settings: {e:#}"));
        }
    }
    if !open {
        app.voice.open = false;
    }
    let _ = TrackKind::Audio;
}

fn lipsync_section(ui: &mut egui::Ui, app: &mut EditorApp, save: &mut bool) {
    use crate::ai::lipsync;
    ui.label(RichText::new("👄 Lip-sync a character to dialogue (fal.ai)").strong());
    ui.label(widgets::dim("Put the line on an audio track over the shot, pick the shot, and the part of it under the line is re-animated so the mouth matches the words. Only that part of the clip is replaced.").small());
    ui.horizontal(|ui| {
        ui.label("fal.ai key");
        match (lipsync::env_key().is_some(), app.ai.settings.fal_key.is_some()) {
            (true, _) => {
                ui.label(widgets::dim("from FAL_KEY"));
            }
            (false, true) => {
                ui.label(RichText::new("saved ✓").color(theme::ACCENT));
                if ui.small_button("Remove").clicked() {
                    app.ai.settings.fal_key = None;
                    *save = true;
                }
            }
            (false, false) => {
                ui.add(TextEdit::singleline(&mut app.voice.fal_input).password(true).hint_text("paste key from fal.ai/dashboard/keys").desired_width(240.0));
                if ui.add_enabled(!app.voice.fal_input.trim().is_empty(), Button::new("Save")).clicked() {
                    app.ai.settings.fal_key = Some(app.voice.fal_input.trim().to_string());
                    app.voice.fal_input.clear();
                    *save = true;
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Method");
        if ui.radio(!app.ai.settings.lipsync_performance, "Fix the mouth (cheapest)").on_hover_text("Keeps the footage and re-draws only the mouth to match the voice (Sync lipsync-2, ≈ $0.05/s). Best for dialogue.").clicked() {
            app.ai.settings.lipsync_performance = false;
            *save = true;
        }
        if ui.radio(app.ai.settings.lipsync_performance, "Performance shot (best for singing)").on_hover_text("Re-generates the shot from a still of the singer with Seedance 2.0, driven by the song and its lyrics, in overlapping ≤14 s segments (≈ $0.24–0.30/s).").clicked() {
            app.ai.settings.lipsync_performance = true;
            *save = true;
        }
    });
    if app.ai.settings.lipsync_performance {
        performance_section(ui, app, save);
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Quality");
        for (id, label, _) in lipsync::MODELS {
            if ui.radio(app.ai.settings.lipsync_model == *id, *label).clicked() {
                app.ai.settings.lipsync_model = id.to_string();
                *save = true;
            }
        }
    });
    // Target: the clip from the right-click menu, else the selected video clip.
    let target = app.voice.lipsync_target.filter(|id| app.project.clip(*id).is_some()).or(app.selection);
    let plan = target.map(|id| lipsync::plan(&app.project, id, None));
    match &plan {
        None => {
            ui.label(widgets::dim("Select the character's video clip on the timeline (or right-click it → Lip-sync to dialogue)."));
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("⚠ {e}")).color(theme::DANGER));
        }
        Some(Ok(p)) => {
            let name = |id: Id| app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).map(|m| m.name.clone()).unwrap_or_default();
            let secs = p.end - p.start;
            let usd = secs * lipsync::usd_per_second(&app.ai.settings.lipsync_model);
            ui.label(format!("Shot: {}  ·  Line: {}", name(p.video_clip), name(p.voice_clip)));
            ui.label(widgets::dim(format!("{} – {}  ({secs:.1} s)  ·  estimated ${usd:.2}", widgets::format_short(p.start), widgets::format_short(p.end))));
        }
    }
    if let Some(job) = &app.voice.lipsync_job {
        let text = job.progress().map(|(_, t)| t).unwrap_or_else(|| "Starting…".into());
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text);
            cancel = ui.small_button("Cancel").clicked();
        });
        if cancel {
            job.cancel();
        }
    } else if let Some(Ok(p)) = plan {
        let usd = (p.end - p.start) * lipsync::usd_per_second(&app.ai.settings.lipsync_model);
        let has_key = app.ai.settings.fal_key().is_some();
        if ui.add_enabled(has_key, Button::new(format!("👄 Lip-sync this shot (≈ ${usd:.2})"))).on_disabled_hover_text(lipsync::NO_KEY).clicked() {
            start_lipsync(app, ui.ctx(), p);
        }
    }
}

fn performance_section(ui: &mut egui::Ui, app: &mut EditorApp, save: &mut bool) {
    use crate::ai::performance;
    ui.horizontal(|ui| {
        ui.label("Model");
        for (id, label, usd) in performance::MODELS {
            if ui.radio(app.ai.settings.performance_model == *id, format!("{label} (${usd:.2}/s)")).clicked() {
                app.ai.settings.performance_model = id.to_string();
                *save = true;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Direction");
        ui.add(TextEdit::singleline(&mut app.voice.perf_direction).hint_text(performance::DEFAULT_DIRECTION).desired_width(320.0));
    });
    ui.horizontal(|ui| {
        ui.label("Character sheet");
        match &app.voice.perf_sheet {
            Some(p) => {
                ui.label(widgets::dim(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                if ui.small_button("✕").clicked() {
                    app.voice.perf_sheet = None;
                }
            }
            None => {
                if ui.small_button("Choose… (optional)").on_hover_text("A reference sheet keeps the face consistent when the singer turns").clicked() {
                    app.voice.perf_sheet = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp"]).pick_file();
                }
            }
        }
    });
    let target = app.voice.lipsync_target.filter(|id| app.project.clip(*id).is_some()).or(app.selection);
    let plan = target.map(|id| performance::plan(&app.project, id, None));
    let model = app.ai.settings.performance_model.clone();
    match &plan {
        None => {
            ui.label(widgets::dim("Select the singer's shot (video or still) on the timeline, over the song."));
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("⚠ {e}")).color(theme::DANGER));
        }
        Some(Ok(p)) => {
            ui.label(widgets::dim(format!("{} – {}  ·  {} segment(s)  ·  estimated ${:.2}", widgets::format_short(p.start()), widgets::format_short(p.end()), p.segments.len(), p.estimate(&model))));
            for (i, s) in p.segments.iter().enumerate() {
                let words = if s.lyrics.is_empty() { "(no lyrics on the timeline here)".to_string() } else { s.lyrics.chars().take(90).collect() };
                ui.label(widgets::dim(format!("  {}. {}–{}  {}", i + 1, widgets::format_short(s.start), widgets::format_short(s.end), words)).small());
            }
        }
    }
    if let Some(job) = &app.voice.perf_job {
        let text = job.progress().map(|(_, t)| t).unwrap_or_else(|| "Starting…".into());
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text);
            cancel = ui.small_button("Cancel").clicked();
        });
        if cancel {
            job.cancel();
        }
    } else if let Some(Ok(p)) = plan {
        let has_key = app.ai.settings.fal_key().is_some();
        if ui.add_enabled(has_key, Button::new(format!("🎤 Make performance shot (≈ ${:.2})", p.estimate(&model)))).on_disabled_hover_text(crate::ai::lipsync::NO_KEY).clicked() {
            start_performance(app, ui.ctx(), p);
        }
    }
}
