//! Local AI (Ollama) UI: the settings section and the "Smart transitions" window.

use crate::ai::local::{self, Decider};
use crate::app::EditorApp;
use crate::model::{Id, TrackKind};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Color32, Context, Grid, RichText, TextEdit, Ui};

#[derive(Default)]
pub struct LocalAiUi {
    pub models: Vec<String>,
    models_job: Option<Task<Vec<String>>>,
    models_error: Option<String>,
    test_job: Option<Task<(bool, f64)>>,
    test_status: Option<Result<(bool, f64), String>>,
    pub transitions_open: bool,
    style: String,
    only_track: bool,
    job: Option<Task<Vec<(Id, usize)>>>,
}

pub fn decider(app: &EditorApp) -> Decider {
    Decider::new(&app.ai.settings.ollama_url, &app.ai.settings.local_model)
}

fn refresh_models(app: &mut EditorApp, ctx: &Context) {
    let url = app.ai.settings.ollama_url.clone();
    app.local.models_error = None;
    app.local.models_job = Some(Task::spawn(ctx.clone(), move |_, _| Decider::models(&url).map(Some)));
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.local.models_job.as_ref().and_then(Task::take_finished) {
        app.local.models_job = None;
        match state {
            TaskState::Done(m) => app.local.models = m,
            TaskState::Failed(e) => app.local.models_error = Some(e),
            _ => {}
        }
    }
    if let Some(state) = app.local.test_job.as_ref().and_then(Task::take_finished) {
        app.local.test_job = None;
        app.local.test_status = match state {
            TaskState::Done(r) => Some(Ok(r)),
            TaskState::Failed(e) => Some(Err(e)),
            _ => None,
        };
    }
    if let Some(state) = app.local.job.as_ref().and_then(Task::take_finished) {
        app.local.job = None;
        match state {
            TaskState::Done(choices) => {
                let tally = local::apply_transitions(&mut app.project, &choices);
                let parts: Vec<String> = tally
                    .iter()
                    .zip(local::TRANSITION_OPTIONS)
                    .filter(|(n, _)| **n > 0)
                    .map(|(n, label)| format!("{n}× {}", label.split(" (").next().unwrap_or(label).to_lowercase()))
                    .collect();
                app.notify(format!("🔀 Smart transitions on {} cuts: {}. Ctrl+Z undoes them.", choices.len(), parts.join(", ")));
            }
            TaskState::Failed(e) => app.notify_error(format!("Smart transitions: {e}")),
            _ => {}
        }
    }
}

/// "Local AI (Ollama)" rows for the AI settings window.
pub fn settings_section(ui: &mut Ui, app: &mut EditorApp) -> bool {
    let mut save = false;
    if app.local.models.is_empty() && app.local.models_job.is_none() && app.local.models_error.is_none() {
        let ctx = ui.ctx().clone();
        refresh_models(app, &ctx);
    }
    ui.separator();
    ui.label(RichText::new("Local AI (Ollama): free, offline, private").strong());
    ui.label(widgets::dim("Used by the offline rough cut and smart transitions. Small decision models answer one multiple-choice question at a time.").small());
    Grid::new("local-ai-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        ui.label("Model");
        ui.vertical(|ui| {
            let current = app.ai.settings.local_model.clone();
            egui::ComboBox::from_id_salt("local-model").selected_text(&current).width(260.0).show_ui(ui, |ui| {
                if app.local.models_job.is_some() {
                    ui.spinner();
                }
                for m in app.local.models.clone() {
                    if m.ends_with(":cloud") || m.contains("embed") {
                        continue; // not local / not a chat model
                    }
                    if ui.selectable_label(m == current, &m).clicked() {
                        app.ai.settings.local_model = m;
                        app.local.test_status = None;
                        save = true;
                    }
                }
            });
            if let Some(e) = &app.local.models_error {
                ui.label(RichText::new(e).color(theme::DANGER).small());
            }
            ui.label(widgets::dim("qwen3 did best in our editing test (15/15); tev1 12/15.").small());
        });
        ui.end_row();
        ui.label("Ollama URL");
        ui.horizontal(|ui| {
            if ui.add(TextEdit::singleline(&mut app.ai.settings.ollama_url).desired_width(200.0)).lost_focus() {
                save = true;
            }
            if ui.small_button("↻").on_hover_text("Reload the model list").clicked() {
                let ctx = ui.ctx().clone();
                refresh_models(app, &ctx);
            }
        });
        ui.end_row();
        ui.label("Check");
        ui.horizontal(|ui| {
            if app.local.test_job.is_some() {
                ui.spinner();
                ui.label(widgets::dim("Loading the model can take a few seconds…").small());
            } else if ui.button("Test model").clicked() {
                let d = decider(app);
                app.local.test_job = Some(Task::spawn(ui.ctx().clone(), move |_, _| d.self_test().map(Some)));
            }
            match &app.local.test_status {
                Some(Ok((true, s))) => {
                    ui.label(RichText::new(format!("✔ answered correctly in {s:.1} s")).color(theme::ACCENT));
                }
                Some(Ok((false, s))) => {
                    ui.label(RichText::new(format!("⚠ answered, but wrongly ({s:.1} s)")).color(theme::DANGER));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new("✖ failed").color(theme::DANGER)).on_hover_text(e);
                }
                None => {}
            }
        });
        ui.end_row();
    });
    save
}

pub fn transitions_window(ctx: &Context, app: &mut EditorApp) {
    if !app.local.transitions_open {
        return;
    }
    let mut open = true;
    let sel_track = app.selection.and_then(|id| app.project.locate(id)).map(|(ti, _)| ti).filter(|&ti| app.project.tracks[ti].kind == TrackKind::Video);
    egui::Window::new(RichText::new("Smart transitions (local AI)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(400.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(widgets::dim(
                "For every cut between touching clips, a local model picks a hard cut, dissolve, fade through black, slide or zoom from the song section, energy, beat and the clips. Free and offline.",
            ));
            ui.add_space(4.0);
            let only = app.local.only_track && sel_track.is_some();
            let cuts = local::describe_cuts(&app.project, if only { sel_track } else { None }, &app.local.style);
            Grid::new("st-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Style");
                ui.add(TextEdit::singleline(&mut app.local.style).hint_text("optional, e.g. moody, mostly hard cuts").desired_width(250.0));
                ui.end_row();
                ui.label("Cuts");
                ui.vertical(|ui| {
                    ui.add_enabled_ui(sel_track.is_some(), |ui| {
                        ui.checkbox(&mut app.local.only_track, "Only the selected clip's track").on_disabled_hover_text("Select a clip on a video track");
                    });
                    ui.label(widgets::dim(format!("{} cuts · model {}", cuts.len(), app.ai.settings.local_model)).small());
                });
                ui.end_row();
            });
            if music_hint(app) {
                ui.label(widgets::dim("Tip: detect beats first (Music menu) so the model knows the song sections and beats.").small());
            }
            ui.add_space(4.0);
            if let Some(job) = &app.local.job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(p).desired_width(280.0).text(stage).animate(true));
                    if ui.button("Stop").clicked() {
                        job.cancel();
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                let label = format!("🔀  Choose transitions for {} cuts", cuts.len());
                if ui.add_enabled(!cuts.is_empty(), Button::new(RichText::new(label).strong().color(Color32::BLACK)).fill(theme::ACCENT)).clicked() {
                    let d = decider(app);
                    let cuts = cuts.clone();
                    let task = Task::spawn(ui.ctx().clone(), move |cancel, report| local::choose_transitions(&cuts, &d, cancel, report));
                    app.local.job = Some(task);
                }
                if ui.button("⚙ Model…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
            });
            if cuts.is_empty() {
                ui.label(widgets::dim("No touching clips on visible video tracks.").small());
            }
            ui.label(widgets::dim("Replaces existing transitions at those cuts. One Ctrl+Z undoes it all.").small());
        });
    if !open {
        app.local.transitions_open = false;
    }
}

fn music_hint(app: &EditorApp) -> bool {
    crate::music::timeline_beats(&app.project).is_empty() && app.project.tracks.iter().any(|t| t.kind == TrackKind::Audio && !t.clips.is_empty())
}
