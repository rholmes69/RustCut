//! AI menu: OpenRouter settings and auto-reframe.

use crate::ai::openrouter::{AiSettings, Client, KeyInfo, ModelInfo};
use crate::ai::reframe::{self, Analysis, Framing, ReframeOptions};
use crate::app::EditorApp;
use crate::model::{Aspect, Id, MediaKind, TrackKind};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Context, Grid, ProgressBar, RichText, Slider, TextEdit, Ui};

pub struct AiUi {
    pub settings: AiSettings,
    pub settings_open: bool,
    pub reframe_all_open: bool,
    key_input: String,
    models_job: Option<Task<Vec<ModelInfo>>>,
    pub models: Vec<ModelInfo>,
    key_job: Option<Task<KeyInfo>>,
    key_status: Option<Result<KeyInfo, String>>,
    pub options: ReframeOptions,
    target_aspect: Aspect,
    job: Option<(Task<Vec<(Id, Result<Analysis, String>)>>, ReframeOptions)>,
    pub session_cost: f64,
}

impl Default for AiUi {
    fn default() -> Self {
        Self {
            settings: AiSettings::load(),
            settings_open: false,
            reframe_all_open: false,
            key_input: String::new(),
            models_job: None,
            models: Vec::new(),
            key_job: None,
            key_status: None,
            options: ReframeOptions::default(),
            target_aspect: Aspect::Portrait,
            job: None,
            session_cost: 0.0,
        }
    }
}

impl AiUi {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Loads the OpenRouter model list (prices, capabilities) in the background.
    pub fn ensure_models(&mut self, ctx: &Context) {
        if self.models.is_empty() && self.models_job.is_none() {
            self.models_job = Some(Task::spawn(ctx.clone(), |_, _| Client::vision_models().map(Some)));
        }
    }

    pub fn open_settings(&mut self, ctx: &Context) {
        self.settings_open = true;
        if self.models.is_empty() && self.models_job.is_none() {
            self.models_job = Some(Task::spawn(ctx.clone(), |_, _| Client::vision_models().map(Some)));
        }
    }
}

/// Clips auto-reframe can work on: video/image clips on video tracks.
pub fn reframable(app: &EditorApp, id: Id) -> bool {
    let Some(clip) = app.project.clip(id) else { return false };
    app.project.track_kind_for(clip) == TrackKind::Video
        && clip.media_id().and_then(|m| app.project.media(m)).is_some_and(|m| m.kind != MediaKind::Audio)
}

/// Starts auto-reframing `ids` in the background.
pub fn start_reframe(app: &mut EditorApp, ctx: &Context, ids: Vec<Id>) {
    let Some(key) = app.ai.settings.api_key() else {
        app.notify_error(crate::ai::openrouter::NO_KEY);
        app.ai.open_settings(ctx);
        return;
    };
    let opts = app.ai.options.clone();
    let ids: Vec<Id> = ids.into_iter().filter(|id| reframable(app, *id)).collect();
    let movable: Vec<Id> = ids
        .iter()
        .copied()
        .filter(|id| app.project.clip(*id).is_some_and(|c| reframe::has_room(&app.project, c, opts.framing)))
        .collect();
    if movable.is_empty() {
        app.notify("Nothing to reframe: the footage already matches the canvas. Change the canvas ratio (e.g. 9:16) or use Medium/Tight framing.");
        return;
    }
    let project = app.project.clone();
    let model = app.ai.settings.vision_model.clone();
    let subject = opts.subject.clone();
    let task = Task::spawn(ctx.clone(), move |cancel, report| {
        let client = Client::new(key);
        let n = movable.len();
        let mut out = Vec::new();
        for (i, id) in movable.into_iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(None);
            }
            let prefix = if n > 1 { format!("Clip {}/{n}: ", i + 1) } else { String::new() };
            let mut progress = |p: f32, stage: &str| report((i as f32 + p) / n as f32, &format!("{prefix}{stage}"));
            match reframe::analyze_clip(&project, id, &subject, &client, &model, cancel, &mut progress) {
                Ok(Some(a)) => out.push((id, Ok(a))),
                Ok(None) => return Ok(None),
                Err(e) => out.push((id, Err(format!("{e:#}")))),
            }
        }
        Ok(Some(out))
    });
    app.ai.job = Some((task, opts));
}

/// Applies finished jobs. Called every frame.
pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.ai.models_job.as_ref().and_then(Task::take_finished) {
        app.ai.models_job = None;
        match state {
            TaskState::Done(m) => app.ai.models = m,
            TaskState::Failed(e) => app.notify_error(format!("Could not load the model list: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.ai.key_job.as_ref().and_then(Task::take_finished) {
        app.ai.key_job = None;
        app.ai.key_status = match state {
            TaskState::Done(k) => Some(Ok(k)),
            TaskState::Failed(e) => Some(Err(e)),
            _ => None,
        };
    }
    let finished = app.ai.job.as_ref().and_then(|(t, o)| t.take_finished().map(|s| (s, o.clone())));
    if let Some((state, opts)) = finished {
        app.ai.job = None;
        match state {
            TaskState::Done(results) => {
                let (mut ok, mut cost, mut errors) = (0, 0.0, Vec::new());
                let mut subject = String::new();
                for (id, r) in results {
                    match r.and_then(|a| {
                        cost += a.usage.cost;
                        subject = a.subject.clone();
                        reframe::apply_analysis(&mut app.project, id, &a, &opts).map_err(|e| format!("{e:#}"))
                    }) {
                        Ok(_) => ok += 1,
                        Err(e) => errors.push(e),
                    }
                }
                app.ai.session_cost += cost;
                if ok > 0 {
                    let who = if subject.is_empty() { String::new() } else { format!(" following “{subject}”") };
                    app.notify(format!("Reframed {ok} clip(s){who} · cost ${cost:.4} — fine-tune the position keyframes (diamond buttons) if needed"));
                }
                if let Some(e) = errors.first() {
                    app.notify_error(format!("{} clip(s) could not be reframed: {e}", errors.len()));
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("Auto-reframe failed: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify("Auto-reframe cancelled"),
        }
    }
}

fn options_grid(ui: &mut Ui, opts: &mut ReframeOptions, id: &str) {
    Grid::new(id).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Follow");
        ui.add(TextEdit::singleline(&mut opts.subject).hint_text("main subject (auto)").desired_width(170.0))
            .on_hover_text("Optional: describe who/what to keep in frame, e.g. “the woman in the red jacket”");
        ui.end_row();
        ui.label("Framing");
        egui::ComboBox::from_id_salt(format!("{id}-framing")).selected_text(opts.framing.label()).show_ui(ui, |ui| {
            for f in Framing::ALL {
                ui.selectable_value(&mut opts.framing, f, f.label());
            }
        });
        ui.end_row();
        ui.label("Camera");
        ui.add(Slider::new(&mut opts.smoothness, 0.0..=1.0).custom_formatter(|v, _| {
            if v < 0.3 { "Follow closely".into() } else if v < 0.7 { "Smooth".into() } else { "Steady".into() }
        }));
        ui.end_row();
    });
}

fn progress_row(ui: &mut Ui, app: &EditorApp) -> bool {
    let Some((task, _)) = &app.ai.job else { return false };
    let (p, stage) = task.progress().unwrap_or((1.0, "Finishing…".into()));
    ui.label(stage);
    ui.add(ProgressBar::new(p).show_percentage().animate(true));
    if ui.button("Cancel").clicked() {
        task.cancel();
    }
    true
}

/// Inspector section for the selected clip.
pub fn reframe_section(ui: &mut Ui, app: &mut EditorApp, clip_id: Id) {
    ui.label(widgets::dim("Keeps the subject in frame for this canvas using an OpenRouter vision model.").small());
    let canvas = app.project.aspect;
    let room = app.project.clip(clip_id).is_some_and(|c| reframe::has_room(&app.project, c, app.ai.options.framing));
    let mut opts = app.ai.options.clone();
    options_grid(ui, &mut opts, "reframe-one");
    app.ai.options = opts;
    if progress_row(ui, app) {
        return;
    }
    if app.ai.settings.api_key().is_none() {
        if ui.button("🔑  Set up OpenRouter…").clicked() {
            app.ai.open_settings(ui.ctx());
        }
        return;
    }
    let w = ui.available_width();
    let button = Button::new(RichText::new(format!("✨  Auto-reframe for {}", canvas.short_label())).strong().color(egui::Color32::BLACK))
        .fill(theme::ACCENT)
        .min_size(egui::vec2(w, 30.0));
    if ui
        .add_enabled(room, button)
        .on_disabled_hover_text("The footage already fits this canvas — change the canvas ratio (top bar) or pick Medium/Tight framing")
        .clicked()
    {
        let ctx = ui.ctx().clone();
        start_reframe(app, &ctx, vec![clip_id]);
    }
    if !room {
        ui.label(widgets::dim(format!("Clip already fits {} — change the canvas ratio first.", canvas.short_label())).small());
    }
}

pub fn windows(ctx: &Context, app: &mut EditorApp) {
    settings_window(ctx, app);
    reframe_all_window(ctx, app);
}

fn settings_window(ctx: &Context, app: &mut EditorApp) {
    if !app.ai.settings_open {
        return;
    }
    let mut open = true;
    let mut save = false;
    egui::Window::new(RichText::new("AI settings").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(widgets::dim("RustCut's AI features use models through OpenRouter, billed to your OpenRouter account."));
            ui.hyperlink_to("Get or manage keys at openrouter.ai/keys", "https://openrouter.ai/keys");
            ui.add_space(8.0);
            let ai = &mut app.ai;
            Grid::new("ai-settings").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("API key");
                ui.vertical(|ui| {
                    match ai.settings.key_source() {
                        Some(src) => ui.label(RichText::new(format!("✔ Using key from {src}")).color(theme::ACCENT)),
                        None => ui.label(RichText::new("No key set").color(theme::DANGER)),
                    };
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut ai.key_input).password(true).hint_text("sk-or-…").desired_width(230.0));
                        if ui.add_enabled(!ai.key_input.trim().is_empty(), Button::new("Save")).clicked() {
                            ai.settings.api_key = Some(ai.key_input.trim().to_string());
                            ai.key_input.clear();
                            ai.key_status = None;
                            save = true;
                        }
                        if ai.settings.api_key.is_some() && ui.button("Remove").on_hover_text("Forget the saved key").clicked() {
                            ai.settings.api_key = None;
                            ai.key_status = None;
                            save = true;
                        }
                    });
                    ui.label(widgets::dim("Saved in your user profile, never in projects. OPENROUTER_API_KEY takes precedence.").small());
                });
                ui.end_row();

                ui.label("Check");
                ui.horizontal(|ui| {
                    if ai.key_job.is_some() {
                        ui.spinner();
                    } else if ui.add_enabled(ai.settings.api_key().is_some(), Button::new("Test key")).clicked() {
                        let key = ai.settings.api_key().unwrap_or_default();
                        ai.key_job = Some(Task::spawn(ui.ctx().clone(), move |_, _| Client::new(key).key_info().map(Some)));
                    }
                    match &ai.key_status {
                        Some(Ok(k)) => {
                            let left = k.limit_remaining.map_or(String::new(), |l| format!(" · ${l:.2} left"));
                            let label = if k.label.is_empty() { "valid".to_string() } else { k.label.clone() };
                            ui.label(RichText::new(format!("✔ {label} · ${:.4} used{left}", k.usage)).color(theme::ACCENT));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new("✖ failed").color(theme::DANGER)).on_hover_text(e);
                        }
                        None => {}
                    }
                });
                ui.end_row();

                ui.label("Vision model");
                ui.vertical(|ui| {
                    let current = ai.settings.vision_model.clone();
                    let price = ai.models.iter().find(|m| m.id == current).map(|m| format!("  ·  ${:.2}/M in", m.prompt_per_m));
                    egui::ComboBox::from_id_salt("vision-model")
                        .selected_text(format!("{current}{}", price.unwrap_or_default()))
                        .width(300.0)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            if ai.models_job.is_some() {
                                ui.spinner();
                            }
                            for m in ai.models.iter().filter(|m| m.structured_output) {
                                let text = format!("{}  ·  ${:.2}/M in · ${:.2}/M out", m.id, m.prompt_per_m, m.completion_per_m);
                                if ui.selectable_label(m.id == current, text).clicked() {
                                    ai.settings.vision_model = m.id.clone();
                                    save = true;
                                }
                            }
                        });
                    ui.horizontal(|ui| {
                        let r = ui.add(TextEdit::singleline(&mut ai.settings.vision_model).desired_width(220.0));
                        if r.lost_focus() {
                            save = true;
                        }
                        if ui.small_button("Default").clicked() {
                            ai.settings.vision_model = crate::ai::openrouter::DEFAULT_VISION_MODEL.into();
                            save = true;
                        }
                    });
                    ui.label(widgets::dim("Needs image input + structured output. Cheap, fast models work well.").small());
                });
                ui.end_row();
            });
            if crate::ui::local_ai::settings_section(ui, app) {
                save = true;
            }
            ui.add_space(6.0);
            ui.label(
                widgets::dim(
                    "Privacy: auto-reframe sends small still frames (≤512 px) of the clip to OpenRouter and the chosen model \
                     provider. Nothing is sent until you run a feature.",
                )
                .small(),
            );
            if app.ai.session_cost > 0.0 {
                ui.label(widgets::dim(format!("Spent this session: ${:.4}", app.ai.session_cost)).small());
            }
        });
    if save {
        if let Err(e) = app.ai.settings.save() {
            app.notify_error(format!("Could not save AI settings: {e:#}"));
        }
    }
    if !open {
        app.ai.settings_open = false;
    }
}

fn reframe_all_window(ctx: &Context, app: &mut EditorApp) {
    if !app.ai.reframe_all_open {
        return;
    }
    let mut open = true;
    let mut run = false;
    let ids: Vec<Id> =
        app.project.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).filter(|id| reframable(app, *id)).collect();
    egui::Window::new(RichText::new("Auto-reframe all clips").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(widgets::dim("Switches the canvas and keeps the subject of every video and image clip in frame."));
            ui.add_space(6.0);
            Grid::new("reframe-all-aspect").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("New canvas");
                egui::ComboBox::from_id_salt("reframe-all-aspect-combo")
                    .selected_text(app.ai.target_aspect.label())
                    .show_ui(ui, |ui| {
                        for a in Aspect::ALL {
                            ui.selectable_value(&mut app.ai.target_aspect, a, a.label());
                        }
                    });
                ui.end_row();
            });
            let mut opts = app.ai.options.clone();
            options_grid(ui, &mut opts, "reframe-all");
            app.ai.options = opts;
            ui.add_space(6.0);
            if progress_row(ui, app) {
                return;
            }
            let frames: usize = ids
                .iter()
                .filter_map(|id| app.project.clip(*id))
                .map(|c| (c.duration / reframe::sample_interval(c.duration)).ceil() as usize + 1)
                .sum();
            ui.label(widgets::dim(format!("{} clip(s) · about {frames} frames to analyse", ids.len())).small());
            if app.ai.settings.api_key().is_none() {
                if ui.button("🔑  Set up OpenRouter…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
            } else if widgets::accent_button(ui, "✨  Reframe all").clicked() {
                run = true;
            }
        });
    if run {
        app.project.aspect = app.ai.target_aspect;
        start_reframe(app, ctx, ids);
    }
    if !open {
        app.ai.reframe_all_open = false;
    }
}
