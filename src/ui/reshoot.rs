//! "Reshoot this clip" window: rewrite the prompt for one cut on the timeline, draw a
//! new still, animate it and swap it in — only that cut changes.

use crate::ai::openrouter::Client;
use crate::ai::reshoot::{self, Source};
use crate::ai::storyboard::Storyboard;
use crate::ai::video_gen::{Generated, VideoModel};
use crate::app::EditorApp;
use crate::model::Id;
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Color32, Context, RichText, TextEdit};

const FRAME_PRICE: f64 = 0.07;

#[derive(Default)]
pub struct ReshootUi {
    clip: Option<Id>,
    src: Source,
    description: String,
    motion: String,
    lead: bool,
    keep_look: bool,
    redo_lipsync: bool,
    /// The one-shot storyboard holding the drawn still.
    sb: Option<Storyboard>,
    spent: f64,
    draw_job: Option<Task<(Storyboard, f64)>>,
    animate_job: Option<Task<Generated>>,
}

/// Opens the window for `clip` (from the timeline's right-click menu).
pub fn open(app: &mut EditorApp, clip: Id) {
    let src = reshoot::find_source(&app.project, clip);
    let r = &mut app.reshoot;
    *r = ReshootUi::default();
    r.description = src.description();
    r.motion = src.motion();
    r.lead = !src.no_lead();
    r.keep_look = true;
    r.redo_lipsync = src.performance;
    r.src = src;
    r.clip = Some(clip);
}

fn busy(app: &EditorApp) -> bool {
    app.reshoot.draw_job.is_some() || app.reshoot.animate_job.is_some()
}

fn start_draw(app: &mut EditorApp, ctx: &Context) {
    let Some(clip) = app.reshoot.clip else { return };
    let Some(key) = app.ai.settings.api_key() else {
        app.notify_error(crate::ai::openrouter::NO_KEY);
        app.ai.open_settings(ctx);
        return;
    };
    let Some(c) = app.project.clip(clip) else { return };
    let dir = match &app.reshoot.sb {
        Some(sb) => sb.dir.clone(),
        None => reshoot::work_dir(app.project_path.as_deref(), clip),
    };
    let r = &app.reshoot;
    let sb = reshoot::one_shot(&app.project, &r.src, &r.description, &r.motion, c.duration, !r.lead, &dir);
    let look = if r.keep_look { reshoot::current_look(&app.project, clip, &dir).ok() } else { None };
    let model = app.ai.settings.image_model.clone();
    app.reshoot.draw_job = Some(Task::spawn(ctx.clone(), move |_, report| {
        report(0.1, "Drawing the new still…");
        let mut sb = sb;
        let (_, cost) = reshoot::draw(&Client::new(key), &mut sb, &model, look.as_deref())?;
        Ok(Some((sb, cost)))
    }));
}

fn start_animate(app: &mut EditorApp, ctx: &Context, m: VideoModel) {
    let Some(key) = app.ai.settings.api_key() else { return };
    let Some(mut sb) = app.reshoot.sb.clone() else { return };
    // Motion may have been edited after drawing.
    sb.shots[0].motion = app.reshoot.motion.trim().to_string();
    app.reshoot.animate_job = Some(Task::spawn(ctx.clone(), move |cancel, report| reshoot::animate(&Client::new(key), &sb, &m, cancel, report)));
}

pub fn poll(app: &mut EditorApp, ctx: &Context) {
    if let Some(state) = app.reshoot.draw_job.as_ref().and_then(Task::take_finished) {
        app.reshoot.draw_job = None;
        match state {
            TaskState::Done((sb, cost)) => {
                app.reshoot.spent += cost;
                app.reshoot.sb = Some(sb);
            }
            TaskState::Failed(e) => app.notify_error(format!("Reshoot: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.reshoot.animate_job.as_ref().and_then(Task::take_finished) {
        app.reshoot.animate_job = None;
        let Some(clip) = app.reshoot.clip else { return };
        match state {
            TaskState::Done(g) => match reshoot::apply(&mut app.project, clip, &g.path, g.info.clone()) {
                Ok(id) => {
                    if let Some(m) = app.project.clip(id).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).cloned() {
                        app.assets.ensure(&m);
                    }
                    app.selection = Some(id);
                    let spent = app.reshoot.spent + g.cost.unwrap_or(0.0);
                    let redo = app.reshoot.redo_lipsync;
                    app.reshoot = ReshootUi::default();
                    if redo {
                        match crate::ai::performance::plan(&app.project, id, None) {
                            Ok(plan) => {
                                crate::ui::voice::open_lipsync(app, id);
                                app.ai.settings.lipsync_performance = true;
                                crate::ui::voice::start_performance(app, ctx, plan);
                                app.notify(format!("🎬 Clip reshot (${spent:.2}) — now redoing its lip-sync performance…"));
                            }
                            Err(e) => app.notify_error(format!("Clip reshot, but the lip-sync couldn't be redone: {e:#}")),
                        }
                    } else {
                        app.notify(format!("🎬 Clip reshot and replaced (${spent:.2})"));
                    }
                }
                Err(e) => app.notify_error(format!("Reshoot: {e:#}")),
            },
            TaskState::Failed(e) => app.notify_error(format!("Reshoot: {e}")),
            _ => {}
        }
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    let Some(clip) = app.reshoot.clip else { return };
    let Some(c) = app.project.clip(clip).cloned() else {
        if !busy(app) {
            app.reshoot = ReshootUi::default();
        }
        return;
    };
    let mut open = true;
    egui::Window::new(RichText::new("🎬 Reshoot this clip").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let is_busy = busy(app);
            ui.label(format!("{} – {}  ({:.1} s)", widgets::format_short(c.start), widgets::format_short(c.end()), c.duration));
            match (&app.reshoot.src.storyboard, app.reshoot.src.shot) {
                (Some(sb), Some(i)) => {
                    let uses = sb.uses(i);
                    let note = if uses > 1 { format!(" — used for {uses} cuts; only this one changes") } else { String::new() };
                    ui.label(widgets::dim(format!("From storyboard shot {}{note}. Its style and cast are kept.", i + 1)).small());
                }
                (Some(_), None) => {
                    ui.label(widgets::dim("Uses the project's storyboard style and cast.").small());
                }
                _ => {
                    ui.label(widgets::dim("No storyboard found for this clip — describe the shot from scratch.").small());
                }
            }
            ui.add_space(4.0);
            ui.add_enabled_ui(!is_busy, |ui| {
                ui.label("What the new shot shows");
                ui.add(TextEdit::multiline(&mut app.reshoot.description).desired_rows(4).desired_width(440.0).hint_text("e.g. Close-up of Aishyla singing in the pouring rain, neon reflections on her wet face"));
                ui.label("Motion");
                ui.add(TextEdit::singleline(&mut app.reshoot.motion).desired_width(440.0).hint_text("e.g. slow push-in, she tilts her head up"));
                let has_people = app.reshoot.src.storyboard.as_ref().is_some_and(|sb| sb.character.is_some() || !sb.cast.is_empty());
                if has_people {
                    ui.checkbox(&mut app.reshoot.lead, "The lead / cast appear in this shot").on_hover_text("Untick for shots of other people or places");
                }
                ui.checkbox(&mut app.reshoot.keep_look, "Keep the current shot's look (wardrobe, setting, colours)");
                if app.reshoot.src.performance {
                    let est = (c.duration.ceil() + 1.0) * crate::ai::performance::usd_per_second(&app.ai.settings.performance_model);
                    ui.checkbox(&mut app.reshoot.redo_lipsync, format!("Redo the lip-sync performance afterwards (≈ ${est:.2})")).on_hover_text("This clip was lip-synced; the new footage needs it again (fal.ai Seedance)");
                }
            });
            ui.separator();

            // Still preview.
            let frame = app.reshoot.sb.as_ref().and_then(|sb| sb.shots[0].frame.clone());
            if let Some(path) = &frame {
                let (rw, rh) = app.project.aspect.ratio();
                let w = 440.0_f32;
                let h = (w * rh as f32 / rw as f32).min(300.0);
                let w = h * rw as f32 / rh as f32;
                let tex = crate::ui::storyboard::thumb(app, ui, path);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
                    match tex {
                        Some(t) => ui.painter().image(t, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE),
                        None => ui.painter().rect_filled(rect, 4.0, theme::SURFACE),
                    }
                });
            }

            if let Some(job) = &app.reshoot.draw_job {
                let text = job.progress().map(|(_, t)| t).unwrap_or_else(|| "Drawing…".into());
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(text);
                });
                return;
            }
            if let Some(job) = &app.reshoot.animate_job {
                let (p, text) = job.progress().unwrap_or((0.0, "Animating…".into()));
                let mut cancel = false;
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(p).desired_width(340.0).text(text).animate(true));
                    cancel = ui.small_button("Stop").on_hover_text("Stop waiting (a submitted job may still be billed)").clicked();
                });
                if cancel {
                    job.cancel();
                }
                return;
            }

            let has_text = !app.reshoot.description.trim().is_empty();
            ui.horizontal(|ui| {
                let label = if frame.is_some() { format!("↻ Redraw still (≈ ${FRAME_PRICE:.2})") } else { format!("🎨 Draw new still (≈ ${FRAME_PRICE:.2})") };
                if ui.add_enabled(has_text, Button::new(label)).on_disabled_hover_text("Describe the new shot first").clicked() {
                    start_draw(app, ui.ctx());
                }
                if app.reshoot.spent > 0.0 {
                    ui.label(widgets::dim(format!("spent ${:.2}", app.reshoot.spent)));
                }
            });

            if let Some(sb) = app.reshoot.sb.clone().filter(|_| frame.is_some()) {
                crate::ui::video_gen::ensure_models(app, ui.ctx());
                let models: Vec<VideoModel> = app.video_gen.models.iter().filter(|m| m.first_frame).cloned().collect();
                let Some(m) = models.iter().find(|m| m.id == app.ai.settings.video_model).cloned().or_else(|| models.first().cloned()) else {
                    ui.spinner();
                    return;
                };
                let est = reshoot::estimate_animation(&sb, &m).map_or("price on openrouter.ai".to_string(), |c| format!("≈ ${c:.2}"));
                ui.label(widgets::dim(format!("Video model: {}", m.id)).small());
                let w = ui.available_width();
                if ui.add(Button::new(RichText::new(format!("🎬 Animate & replace this clip ({est})")).strong().color(Color32::BLACK)).fill(theme::ACCENT).min_size(egui::vec2(w, 30.0))).clicked() {
                    start_animate(app, ui.ctx(), m);
                }
            }
        });
    if !open && !busy(app) {
        app.reshoot = ReshootUi::default();
    }
}
