//! AI video generation UI: the "Generate video" window and the inspector's
//! "Extend shot" section.

use crate::ai::openrouter::Client;
use crate::ai::video_gen::{self, GenRequest, Generated, Placement, VideoModel};
use crate::app::EditorApp;
use crate::model::{Id, MediaKind, Project, TrackKind};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Context, Grid, ProgressBar, RichText, TextEdit, Ui};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartImage {
    None,
    Playhead,
    SelectedLast,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaceChoice {
    Library,
    Playhead,
    End,
}

/// What the worker needs to build the start/end frames.
#[derive(Clone, Debug)]
enum Frames {
    None,
    Composite(f64),
    LastOf(Id),
    Bridge(Id, Id),
}

struct GenJob {
    label: String,
    task: Task<Generated>,
    placement: Placement,
    anchor: Option<Id>,
}

pub struct VideoGenUi {
    pub open: bool,
    prompt: String,
    start: StartImage,
    duration: u32,
    resolution: String,
    aspect: Option<String>,
    audio: bool,
    place: PlaceChoice,
    pub models: Vec<VideoModel>,
    pub models_job: Option<Task<Vec<VideoModel>>>,
    models_error: Option<String>,
    jobs: Vec<GenJob>,
    extend_prompt: String,
    extend_duration: u32,
    bridge: bool,
    /// Clip whose "Extend shot" window is open (from the timeline's right-click menu).
    pub extend_target: Option<Id>,
}

impl Default for VideoGenUi {
    fn default() -> Self {
        Self {
            open: false,
            prompt: String::new(),
            start: StartImage::None,
            duration: 0,
            resolution: String::new(),
            aspect: None,
            audio: false,
            place: PlaceChoice::Playhead,
            models: Vec::new(),
            models_job: None,
            models_error: None,
            jobs: Vec::new(),
            extend_prompt: String::new(),
            extend_duration: 0,
            bridge: false,
            extend_target: None,
        }
    }
}

impl VideoGenUi {
    pub fn busy_on(&self, clip: Id) -> bool {
        self.jobs.iter().any(|j| j.anchor == Some(clip))
    }
}

pub fn ensure_models(app: &mut EditorApp, ctx: &Context) {
    let v = &mut app.video_gen;
    if !v.models.is_empty() || v.models_job.is_some() || v.models_error.is_some() {
        return;
    }
    let Some(key) = app.ai.settings.api_key() else { return };
    v.models_job = Some(Task::spawn(ctx.clone(), move |_, _| video_gen::list_models(&Client::new(key)).map(Some)));
}

fn current_model(app: &EditorApp) -> Option<VideoModel> {
    let id = &app.ai.settings.video_model;
    app.video_gen.models.iter().find(|m| &m.id == id).cloned()
}

fn start_job(app: &mut EditorApp, ctx: &Context, req: GenRequest, frames: Frames, label: String, placement: Placement, anchor: Option<Id>) {
    let Some(key) = app.ai.settings.api_key() else {
        app.notify_error(crate::ai::openrouter::NO_KEY);
        app.ai.open_settings(ctx);
        return;
    };
    let project: Project = app.project.clone();
    let dir = video_gen::output_dir(app.project_path.as_deref());
    // B-roll is named after its prompt; extensions after the clip they extend.
    let stem = format!("ai-{}", video_gen::slug(if anchor.is_some() || req.prompt.is_empty() { &label } else { &req.prompt }));
    let task = Task::spawn(ctx.clone(), move |cancel, report| {
        let mut req = req;
        report(0.0, "Preparing frames…");
        match frames {
            Frames::None => {}
            Frames::Composite(t) => {
                let png = crate::export::render_frame_png(&project, t, 720)?;
                req.first_frame = Some((png, "image/png"));
            }
            Frames::LastOf(id) => {
                let clip = project.clip(id).cloned().ok_or_else(|| anyhow::anyhow!("clip was removed"))?;
                req.first_frame = Some((video_gen::clip_last_frame(&project, &clip)?, "image/jpeg"));
            }
            Frames::Bridge(a, b) => {
                let ca = project.clip(a).cloned().ok_or_else(|| anyhow::anyhow!("clip was removed"))?;
                let cb = project.clip(b).cloned().ok_or_else(|| anyhow::anyhow!("next clip was removed"))?;
                req.first_frame = Some((video_gen::clip_last_frame(&project, &ca)?, "image/jpeg"));
                req.last_frame = Some((video_gen::clip_first_frame(&project, &cb)?, "image/jpeg"));
            }
        }
        video_gen::generate(&Client::new(key), &req, &dir, &stem, cancel, report)
    });
    app.video_gen.jobs.push(GenJob { label, task, placement, anchor });
}

/// Applies finished jobs. Called every frame.
pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.video_gen.models_job.as_ref().and_then(Task::take_finished) {
        app.video_gen.models_job = None;
        match state {
            TaskState::Done(m) => app.video_gen.models = m,
            TaskState::Failed(e) => app.video_gen.models_error = Some(e),
            _ => {}
        }
    }
    let mut i = 0;
    while i < app.video_gen.jobs.len() {
        let Some(state) = app.video_gen.jobs[i].task.take_finished() else {
            i += 1;
            continue;
        };
        let job = app.video_gen.jobs.remove(i);
        match state {
            TaskState::Done(gen) => {
                let (media, clip) = video_gen::place_generated(&mut app.project, &gen, job.placement);
                app.selected_media = Some(media);
                if clip.is_some() {
                    app.selection = clip;
                }
                if let Some(item) = app.project.media(media).cloned() {
                    app.assets.ensure(&item);
                }
                app.ai.session_cost += gen.cost.unwrap_or(0.0);
                let cost = gen.cost.map_or(String::new(), |c| format!(" · cost ${c:.2}"));
                let where_ = if clip.is_some() { "added to the timeline" } else { "added to the media library" };
                app.notify(format!("✨ {} ready — {where_}{cost}", job.label));
            }
            TaskState::Failed(e) => app.notify_error(format!("{} failed: {e}", job.label)),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify(format!(
                "Stopped waiting for {}. The job may still finish and be billed on OpenRouter.",
                job.label
            )),
        }
    }
}

fn model_picker(ui: &mut Ui, app: &mut EditorApp, duration: u32, aspect: &str, image: bool) -> bool {
    let mut changed = false;
    let current = app.ai.settings.video_model.clone();
    egui::ComboBox::from_id_salt("video-model").selected_text(&current).width(280.0).height(400.0).show_ui(ui, |ui| {
        if app.video_gen.models_job.is_some() {
            ui.spinner();
        }
        for m in app.video_gen.models.clone() {
            if image && !m.first_frame {
                continue;
            }
            let d = m.pick_duration(duration as f64);
            let res = m.default_resolution();
            let price = video_gen::estimate_cost(&m, d, &res, aspect, false, image)
                .map_or(String::new(), |(c, rough)| format!("  ·  {}${c:.2} for {d}s {res}", if rough { "≈" } else { "" }));
            if ui.selectable_label(m.id == current, format!("{}{price}", m.id)).clicked() {
                app.ai.settings.video_model = m.id.clone();
                changed = true;
            }
        }
    });
    if changed {
        let _ = app.ai.settings.save();
    }
    changed
}

fn jobs_list(ui: &mut Ui, app: &EditorApp, only_anchor: Option<Id>) {
    for job in app.video_gen.jobs.iter().filter(|j| only_anchor.is_none() || j.anchor == only_anchor) {
        let (p, stage) = job.task.progress().unwrap_or((1.0, "Finishing…".into()));
        ui.label(RichText::new(&job.label).strong());
        ui.horizontal(|ui| {
            ui.add(ProgressBar::new(p).desired_width(200.0).text(stage));
            if ui.small_button("Stop").on_hover_text("Stop waiting (the provider may still bill the job)").clicked() {
                job.task.cancel();
            }
        });
    }
}

/// Small window for "Extend shot" opened from a clip's right-click menu.
fn extend_window(ctx: &Context, app: &mut EditorApp) {
    let Some(id) = app.video_gen.extend_target else { return };
    let Some(name) = app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).map(|m| m.name.clone())
    else {
        app.video_gen.extend_target = None;
        return;
    };
    let mut open = true;
    egui::Window::new(RichText::new("Extend shot (AI)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(&name).strong());
            extend_section(ui, app, id);
            if !app.video_gen.busy_on(id) {
                ui.label(widgets::dim("You can close this window; the result is inserted after the clip when ready.").small());
            }
        });
    if !open {
        app.video_gen.extend_target = None;
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    extend_window(ctx, app);
    if !app.video_gen.open {
        return;
    }
    ensure_models(app, ctx);
    let mut open = true;
    let mut go = false;
    let selected_video = app.selection.filter(|id| {
        app.project.clip(*id).is_some_and(|c| {
            app.project.track_kind_for(c) == TrackKind::Video
                && c.media_id().and_then(|m| app.project.media(m)).is_some_and(|m| m.kind != MediaKind::Audio)
        })
    });
    egui::Window::new(RichText::new("Generate video (AI)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(470.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(470.0);
            if app.ai.settings.api_key().is_none() {
                ui.label(RichText::new("Add your OpenRouter key first.").color(theme::DANGER));
                if ui.button("🔑  OpenRouter settings…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
                return;
            }
            if let Some(e) = &app.video_gen.models_error {
                ui.label(RichText::new(format!("Could not load video models: {e}")).color(theme::DANGER));
                if ui.button("Retry").clicked() {
                    app.video_gen.models_error = None;
                }
                return;
            }
            ui.label(widgets::dim("Describe a shot. It's generated by a video model on OpenRouter and billed to your account."));
            ui.add(
                TextEdit::multiline(&mut app.video_gen.prompt)
                    .hint_text("e.g. Slow drone shot over a misty pine forest at golden hour, cinematic")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            let (cw, ch) = app.project.aspect.size_for(1080);
            let model = current_model(app);
            let image = app.video_gen.start != StartImage::None;
            Grid::new("vg-grid").num_columns(2).spacing([10.0, 7.0]).show(ui, |ui| {
                ui.label("Start from");
                ui.vertical(|ui| {
                    let can_frames = model.as_ref().is_none_or(|m| m.first_frame);
                    let v = &mut app.video_gen;
                    ui.radio_value(&mut v.start, StartImage::None, "Text only");
                    ui.add_enabled_ui(can_frames, |ui| {
                        ui.radio_value(&mut v.start, StartImage::Playhead, "Frame at the playhead");
                        ui.add_enabled_ui(selected_video.is_some(), |ui| {
                            ui.radio_value(&mut v.start, StartImage::SelectedLast, "Last frame of the selected clip");
                        });
                    });
                    if !can_frames {
                        v.start = StartImage::None;
                        ui.label(widgets::dim("This model can't start from an image.").small());
                    }
                });
                ui.end_row();

                ui.label("Model");
                let aspect_hint = app.video_gen.aspect.clone().unwrap_or_else(|| "16:9".into());
                let dur = if app.video_gen.duration == 0 { 6 } else { app.video_gen.duration };
                model_picker(ui, app, dur, &aspect_hint, image);
                ui.end_row();

                if let Some(m) = &model {
                    let v = &mut app.video_gen;
                    if v.duration == 0 || !m.durations.contains(&v.duration) {
                        v.duration = m.pick_duration(6.0);
                    }
                    if v.resolution.is_empty() || !m.resolutions.contains(&v.resolution) {
                        v.resolution = m.default_resolution();
                    }
                    if v.aspect.as_ref().is_some_and(|a| !m.aspect_ratios.contains(a)) {
                        v.aspect = None;
                    }
                    ui.label("Length");
                    let mut d = m.durations.clone();
                    d.sort_unstable();
                    egui::ComboBox::from_id_salt("vg-dur").selected_text(format!("{} s", v.duration)).show_ui(ui, |ui| {
                        for x in d {
                            ui.selectable_value(&mut v.duration, x, format!("{x} s"));
                        }
                    });
                    ui.end_row();
                    ui.label("Resolution");
                    egui::ComboBox::from_id_salt("vg-res").selected_text(&v.resolution).show_ui(ui, |ui| {
                        for r in &m.resolutions {
                            ui.selectable_value(&mut v.resolution, r.clone(), r);
                        }
                    });
                    ui.end_row();
                    ui.label("Shape");
                    let auto = m.pick_aspect(cw as f64, ch as f64);
                    egui::ComboBox::from_id_salt("vg-ar")
                        .selected_text(v.aspect.clone().unwrap_or_else(|| format!("Match canvas ({auto})")))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut v.aspect, None, format!("Match canvas ({auto})"));
                            for a in &m.aspect_ratios {
                                ui.selectable_value(&mut v.aspect, Some(a.clone()), a);
                            }
                        });
                    ui.end_row();
                    if m.audio != Some(false) {
                        ui.label("Audio");
                        ui.checkbox(&mut v.audio, "Generate sound").on_hover_text("Costs more on most models");
                        ui.end_row();
                    } else {
                        v.audio = false;
                    }
                }

                ui.label("Then");
                egui::ComboBox::from_id_salt("vg-place")
                    .selected_text(match app.video_gen.place {
                        PlaceChoice::Library => "Add to the media library",
                        PlaceChoice::Playhead => "Place at the playhead",
                        PlaceChoice::End => "Append to the end",
                    })
                    .show_ui(ui, |ui| {
                        let v = &mut app.video_gen;
                        ui.selectable_value(&mut v.place, PlaceChoice::Playhead, "Place at the playhead");
                        ui.selectable_value(&mut v.place, PlaceChoice::End, "Append to the end");
                        ui.selectable_value(&mut v.place, PlaceChoice::Library, "Add to the media library");
                    });
                ui.end_row();
            });

            ui.add_space(6.0);
            let Some(m) = model else {
                if app.video_gen.models_job.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading models…");
                    });
                } else if !app.video_gen.models.is_empty() {
                    ui.label(RichText::new("Pick a model above.").color(theme::DANGER));
                }
                return;
            };
            let v = &app.video_gen;
            let aspect = v.aspect.clone().unwrap_or_else(|| m.pick_aspect(cw as f64, ch as f64));
            let estimate = video_gen::estimate_cost(&m, v.duration, &v.resolution, &aspect, v.audio, image);
            ui.label(match estimate {
                Some((c, rough)) => RichText::new(format!("Estimated cost: {}${c:.2}", if rough { "≈ " } else { "" })).strong(),
                None => RichText::new("Cost: see the model's pricing on openrouter.ai").strong(),
            });
            ui.label(widgets::dim("Usually takes 30 seconds to a few minutes. You can keep editing meanwhile.").small());
            let ready = !v.prompt.trim().is_empty() || image;
            if ui.add_enabled(ready, Button::new(RichText::new("✨  Generate").strong().color(egui::Color32::BLACK)).fill(theme::ACCENT)).clicked() {
                go = true;
            }
            if !app.video_gen.jobs.is_empty() {
                ui.separator();
                jobs_list(ui, app, None);
            }
        });

    if go {
        if let Some(m) = current_model(app) {
            let (cw, ch) = app.project.aspect.size_for(1080);
            let v = &app.video_gen;
            let req = GenRequest {
                model: m.id.clone(),
                prompt: v.prompt.trim().to_string(),
                duration: v.duration,
                resolution: v.resolution.clone(),
                aspect_ratio: v.aspect.clone().unwrap_or_else(|| m.pick_aspect(cw as f64, ch as f64)),
                generate_audio: v.audio,
                first_frame: None,
                last_frame: None,
            };
            let frames = match v.start {
                StartImage::None => Frames::None,
                StartImage::Playhead => Frames::Composite(app.playhead),
                StartImage::SelectedLast => selected_video.map_or(Frames::None, Frames::LastOf),
            };
            let placement = match v.place {
                PlaceChoice::Library => Placement::Library,
                PlaceChoice::Playhead => Placement::At(app.playhead),
                PlaceChoice::End => Placement::End,
            };
            let label = format!("B-roll “{}”", video_gen::slug(&req.prompt).replace('-', " "));
            start_job(app, ctx, req, frames, label, placement, None);
        }
    }
    if !open {
        app.video_gen.open = false;
    }
}

/// Inspector section: extend the selected clip with generated footage.
pub fn extend_section(ui: &mut Ui, app: &mut EditorApp, clip_id: Id) {
    ui.label(widgets::dim("Generates new footage that continues from this clip's last frame and inserts it right after.").small());
    if app.ai.settings.api_key().is_none() {
        if ui.button("🔑  Set up OpenRouter…").clicked() {
            app.ai.open_settings(ui.ctx());
        }
        return;
    }
    ensure_models(app, ui.ctx());
    if app.video_gen.busy_on(clip_id) {
        jobs_list(ui, app, Some(clip_id));
        return;
    }
    let Some(clip) = app.project.clip(clip_id).cloned() else { return };
    let Some(media) = clip.media_id().and_then(|m| app.project.media(m)).cloned() else { return };
    let next = app.project.next_adjacent(clip_id);
    let image_models: Vec<VideoModel> = app.video_gen.models.iter().filter(|m| m.first_frame).cloned().collect();
    let model = current_model(app).filter(|m| m.first_frame).or_else(|| image_models.first().cloned());
    let Some(m) = model else {
        if app.video_gen.models_job.is_some() {
            ui.spinner();
        } else if let Some(e) = &app.video_gen.models_error {
            ui.label(RichText::new(e).color(theme::DANGER).small());
        }
        return;
    };
    let aspect = m.pick_aspect(media.width.max(1) as f64, media.height.max(1) as f64);
    let can_bridge = next.is_some() && m.last_frame;
    ui.horizontal(|ui| {
        ui.label("Model");
        let dur = if app.video_gen.extend_duration == 0 { 4 } else { app.video_gen.extend_duration };
        if app.ai.settings.video_model != m.id {
            // The saved model can't start from an image; show the one actually used.
            app.ai.settings.video_model = m.id.clone();
        }
        model_picker(ui, app, dur, &aspect, true);
    });
    ui.label(widgets::dim("Some models refuse realistic people; if one fails, try another (e.g. Grok Imagine or Kling).").small());
    let v = &mut app.video_gen;
    if v.extend_duration == 0 || !m.durations.contains(&v.extend_duration) {
        v.extend_duration = m.pick_duration(4.0);
    }
    if !can_bridge {
        v.bridge = false;
    }
    Grid::new("extend-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Add");
        let mut d = m.durations.clone();
        d.sort_unstable();
        egui::ComboBox::from_id_salt("extend-dur").selected_text(format!("{} s", v.extend_duration)).show_ui(ui, |ui| {
            for x in d {
                ui.selectable_value(&mut v.extend_duration, x, format!("{x} s"));
            }
        });
        ui.end_row();
        ui.label("Direction");
        ui.add(TextEdit::singleline(&mut v.extend_prompt).hint_text("continue naturally (optional)").desired_width(170.0));
        ui.end_row();
        if can_bridge {
            ui.label("");
            ui.checkbox(&mut v.bridge, "Blend into the next clip").on_hover_text("Uses the next clip's first frame as the last frame");
            ui.end_row();
        }
    });
    let res = m.default_resolution();
    let estimate = video_gen::estimate_cost(&m, v.extend_duration, &res, &aspect, false, true);
    let label = format!(
        "✨  Extend by {} s{}",
        v.extend_duration,
        estimate.map_or(String::new(), |(c, r)| format!("  ({}${c:.2})", if r { "≈" } else { "" }))
    );
    let w = ui.available_width();
    if ui.add(Button::new(RichText::new(label).strong().color(egui::Color32::BLACK)).fill(theme::ACCENT).min_size(egui::vec2(w, 30.0))).clicked() {
        let bridge = v.bridge;
        let base = if bridge { video_gen::BRIDGE_PROMPT } else { video_gen::EXTEND_PROMPT };
        let prompt = if v.extend_prompt.trim().is_empty() { base.to_string() } else { format!("{base} {}", v.extend_prompt.trim()) };
        let req = GenRequest {
            model: m.id.clone(),
            prompt,
            duration: v.extend_duration,
            resolution: res,
            aspect_ratio: aspect,
            generate_audio: false,
            first_frame: None,
            last_frame: None,
        };
        let frames = match (bridge, next) {
            (true, Some(n)) => Frames::Bridge(clip_id, n),
            _ => Frames::LastOf(clip_id),
        };
        let label = format!("{} for “{}”", if bridge { "Bridge shot" } else { "Shot extension" }, media.name);
        let ctx = ui.ctx().clone();
        start_job(app, &ctx, req, frames, label, Placement::After(clip_id), Some(clip_id));
    }
}
