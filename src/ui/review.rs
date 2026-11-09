//! AI review windows: 🔍 Continuity check (vision model) and 🎬 Teaser / trailer
//! (best part of the video as its own project, free).

use crate::ai::continuity::{self, Engine, Report};
use crate::ai::highlights::{self, Window};
use crate::app::EditorApp;
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Color32, Context, RichText};
use std::path::PathBuf;

pub struct ReviewUi {
    pub continuity_open: bool,
    pub local: bool,
    job: Option<Task<Report>>,
    report: Option<Report>,
    local_model: Option<Option<String>>,
    local_job: Option<Task<Option<String>>>,
    pub teaser_open: bool,
    pub seconds: f64,
    pub vertical: bool,
    pub end_card: String,
    window: Option<Window>,
    made: Option<PathBuf>,
}

impl Default for ReviewUi {
    fn default() -> Self {
        Self {
            continuity_open: false,
            local: false,
            job: None,
            report: None,
            local_model: None,
            local_job: None,
            teaser_open: false,
            seconds: 30.0,
            vertical: false,
            end_card: "Full video out now".into(),
            window: None,
            made: None,
        }
    }
}

fn project_dir(app: &EditorApp) -> Option<PathBuf> {
    app.project_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.review.local_job.as_ref().and_then(|j| j.take_finished()) {
        app.review.local_job = None;
        app.review.local_model = Some(match state {
            TaskState::Done(m) => m,
            _ => None,
        });
    }
    let Some(state) = app.review.job.as_ref().and_then(|j| j.take_finished()) else { return };
    app.review.job = None;
    match state {
        TaskState::Done(r) => {
            app.notify(match r.issues.len() {
                0 => "Continuity check: no problems found".to_string(),
                n => format!("Continuity check: {n} possible problem{}", if n == 1 { "" } else { "s" }),
            });
            app.review.report = Some(r);
            app.review.continuity_open = true;
        }
        TaskState::Failed(e) => app.notify_error(format!("Continuity check failed: {e}")),
        _ => {}
    }
}

fn start_check(app: &mut EditorApp, ctx: &Context) -> Result<(), String> {
    let engine = if app.review.local {
        let model = app.review.local_model.clone().flatten().ok_or("No local vision model in Ollama (try `ollama pull qwen3-vl:8b`)")?;
        Engine::Local { url: app.ai.settings.ollama_url.clone(), model }
    } else {
        let key = app.ai.settings.api_key().ok_or(crate::ai::openrouter::NO_KEY)?;
        Engine::Cloud { key, model: app.ai.settings.vision_model.clone() }
    };
    let project = app.project.clone();
    let dir = project_dir(app);
    let story = dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("theme.txt")).ok()).unwrap_or_default();
    let name: String = project.name.chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect();
    let work = crate::util::config_dir().join("continuity").join(name.trim());
    app.review.report = None;
    app.review.job = Some(Task::spawn(ctx.clone(), move |cancel, progress| continuity::run(&project, dir.as_deref(), &engine, &story, &work, cancel, progress)));
    Ok(())
}

/// Opens the continuity window (and looks for a local vision model once).
pub fn open_continuity(app: &mut EditorApp, ctx: &Context) {
    app.review.continuity_open = true;
    if app.review.local_model.is_none() && app.review.local_job.is_none() {
        let (url, pref) = (app.ai.settings.ollama_url.clone(), app.ai.settings.local_model.clone());
        app.review.local_job = Some(Task::spawn(ctx.clone(), move |_, _| Ok(Some(continuity::local_vision_model(&url, &pref)))));
    }
}

fn estimate(app: &EditorApp) -> Option<f64> {
    let shots = continuity::shot_count(&app.project);
    let refs = project_dir(app).map(|d| continuity::find_references(&d).len()).unwrap_or(0);
    let (inp, out) = continuity::token_estimate(shots, refs);
    let m = app.ai.models.iter().find(|m| m.id == app.ai.settings.vision_model)?;
    Some(inp as f64 * m.prompt_per_m / 1e6 + out as f64 * m.completion_per_m / 1e6)
}

pub fn continuity_window(ctx: &Context, app: &mut EditorApp) {
    if !app.review.continuity_open {
        return;
    }
    let mut open = true;
    let mut run = false;
    let mut goto = None;
    let mut reshoot = None;
    egui::Window::new(RichText::new("🔍 Continuity check").strong()).id(egui::Id::new("continuity")).open(&mut open).default_width(470.0).show(ctx, |ui| {
        let shots = continuity::shot_count(&app.project);
        let refs = project_dir(app).map(|d| continuity::find_references(&d)).unwrap_or_default();
        ui.label(widgets::dim(format!(
            "Checks the {shots} shots on the main track for the same character changing face, hair or outfit between shots. Shot numbers match Edit → Timeline storyboard."
        )).small());
        if !refs.is_empty() {
            ui.label(widgets::dim(format!("Cast references found: {}", refs.iter().map(|r| r.0.as_str()).collect::<Vec<_>>().join(", "))).small());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut app.review.local, false, format!("Cloud · {}", app.ai.settings.vision_model));
            let local = match &app.review.local_model {
                None => "Local · looking…".to_string(),
                Some(Some(m)) => format!("Local · {m} (free, slow)"),
                Some(None) => "Local · no vision model".to_string(),
            };
            ui.add_enabled_ui(matches!(app.review.local_model, Some(Some(_))), |ui| ui.radio_value(&mut app.review.local, true, local));
        });
        if app.review.local {
            ui.label(RichText::new("Local vision models are slow (~30 min for 85 shots) and raise many false alarms; the cloud model is more reliable.").small().color(Color32::from_rgb(255, 184, 48)));
        }
        let cost = if app.review.local { "free".to_string() } else { estimate(app).map_or("≈ a few cents".into(), |c| format!("≈ ${:.3}", c.max(0.001))) };
        ui.horizontal(|ui| {
            let busy = app.review.job.is_some();
            if ui.add_enabled(!busy && shots >= 2, egui::Button::new(format!("Run the check ({cost})"))).clicked() {
                run = true;
            }
            if let Some(r) = &app.review.report {
                if let Some(dir) = r.sheets.first().and_then(|s| s.parent()) {
                    if ui.small_button("📂 Contact sheets").clicked() {
                        crate::ui::library::reveal(&dir.join("sheet-01.jpg"));
                    }
                }
            }
        });
        if let Some((p, stage)) = app.review.job.as_ref().and_then(|j| j.progress()) {
            ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
        }
        if let Some(r) = &app.review.report {
            ui.separator();
            if !r.summary.is_empty() {
                ui.label(RichText::new(&r.summary).italics());
            }
            if r.issues.is_empty() {
                ui.label(RichText::new("✅ No continuity problems found.").color(theme::ACCENT));
            }
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for is in &r.issues {
                    let col = match is.severity.as_str() {
                        "high" => theme::DANGER,
                        "medium" => Color32::from_rgb(255, 184, 48),
                        _ => theme::TEXT_DIM,
                    };
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("● {}", is.severity)).color(col).strong());
                            ui.label(RichText::new(&is.character).strong());
                            ui.label(&is.problem);
                        });
                        ui.horizontal_wrapped(|ui| {
                            for &s in &is.shots {
                                let Some(&clip) = r.clips.get(s - 1) else { continue };
                                if ui.small_button(format!("Shot {s}")).on_hover_text("Select it and move the playhead there").clicked() {
                                    goto = Some(clip);
                                }
                            }
                            if let Some(&last) = is.shots.last().and_then(|s| r.clips.get(s - 1)) {
                                if is.shots.len() > 1 && ui.small_button("🎬 Reshoot the odd one…").on_hover_text("Opens Reshoot for the last shot listed").clicked() {
                                    reshoot = Some(last);
                                }
                            }
                        });
                    });
                }
            });
            if r.usd > 0.0 {
                ui.label(widgets::dim(format!("Cost ${:.4}", r.usd)).small());
            }
            ui.label(widgets::dim("Vision models can miss things or be wrong — check each one by eye.").small());
        }
    });
    if run {
        if let Err(e) = start_check(app, ctx) {
            app.notify_error(e);
        }
    }
    if let Some(c) = goto {
        crate::ui::media_preview::show_in_timeline(app, c);
    }
    if let Some(c) = reshoot {
        crate::ui::reshoot::open(app, c);
    }
    if !open {
        app.review.continuity_open = false;
    }
}

/// Saves a teaser project next to the current one. Returns its path.
pub fn create_teaser(app: &mut EditorApp, seconds: f64, vertical: bool, end_card: &str) -> Result<(PathBuf, Window), String> {
    let w = highlights::best(&app.project, seconds).ok_or("Add clips to the timeline first")?;
    let teaser = highlights::make(&app.project, &w, end_card, vertical);
    let dir = project_dir(app).ok_or("Save the project first — the teaser is saved next to it")?;
    let file = format!("{}{}.rcproj", teaser.name, if vertical { " (9x16)" } else { "" });
    let path = dir.join(file.replace(['/', '\\', ':'], "-"));
    teaser.save(&path).map_err(|e| format!("{e:#}"))?;
    app.review.made = Some(path.clone());
    app.review.window = Some(w.clone());
    Ok((path, w))
}

pub fn teaser_window(ctx: &Context, app: &mut EditorApp) {
    if !app.review.teaser_open {
        return;
    }
    let mut open = true;
    let (mut find, mut create, mut export, mut open_it) = (false, false, false, false);
    egui::Window::new(RichText::new("🎬 Teaser / trailer").strong()).id(egui::Id::new("teaser")).open(&mut open).default_width(400.0).resizable(false).show(ctx, |ui| {
        let rv = &mut app.review;
        ui.label(widgets::dim("Finds the best part of the video — song energy, a chorus start, lively cutting and many different shots — and makes it a project of its own. Free; nothing in this project changes.").small());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Length");
            for s in [15.0, 30.0, 60.0] {
                if ui.selectable_label((rv.seconds - s).abs() < 0.1, format!("{s:.0} s")).clicked() {
                    rv.seconds = s;
                    rv.window = None;
                }
            }
        });
        ui.checkbox(&mut rv.vertical, "Vertical 9:16 (Shorts, Reels, TikTok)");
        ui.horizontal(|ui| {
            ui.label("End card");
            ui.add(egui::TextEdit::singleline(&mut rv.end_card).desired_width(220.0).hint_text("empty = none"));
        });
        ui.horizontal(|ui| {
            if ui.button("🔎 Find the best part").clicked() {
                find = true;
            }
            if ui.add(egui::Button::new(RichText::new("Create teaser").strong())).clicked() {
                create = true;
            }
        });
        if let Some(w) = &rv.window {
            ui.label(format!("{} – {}  ({:.0} s)", widgets::format_time(w.start), widgets::format_time(w.end), w.end - w.start));
            ui.label(widgets::dim(&w.why).small());
        }
        if let Some(p) = &rv.made {
            ui.separator();
            ui.label(RichText::new(format!("✅ Saved {}", p.file_name().unwrap_or_default().to_string_lossy())).color(theme::ACCENT));
            ui.horizontal(|ui| {
                if ui.button("⬆ Export it (1080p)").on_hover_text("Adds it to the render queue; this project stays open").clicked() {
                    export = true;
                }
                if ui.button("Open teaser").clicked() {
                    open_it = true;
                }
                if ui.small_button("📂").clicked() {
                    crate::ui::library::reveal(p);
                }
            });
        }
    });
    if find {
        app.review.window = highlights::best(&app.project, app.review.seconds);
        if let Some(w) = &app.review.window {
            let t = w.start;
            app.seek(t);
            app.timeline.reveal = Some(t);
        }
    }
    if create {
        let (s, v, card) = (app.review.seconds, app.review.vertical, app.review.end_card.clone());
        match create_teaser(app, s, v, &card) {
            Ok((p, _)) => app.notify(format!("Teaser saved: {}", p.display())),
            Err(e) => app.notify_error(e),
        }
    }
    if export {
        if let Some(p) = app.review.made.clone() {
            match crate::model::Project::load(&p) {
                Ok(proj) => {
                    crate::ui::export_queue::enqueue_project(app, proj, p.with_extension("mp4"), 1080);
                    crate::ui::export_queue::poll(ctx, app);
                    crate::ui::export_queue::show(app);
                }
                Err(e) => app.notify_error(format!("{e:#}")),
            }
        }
    }
    if open_it {
        if app.is_dirty() {
            app.notify_error("Save this project first (Ctrl+S), then open the teaser");
        } else if let Some(p) = app.review.made.clone() {
            app.open_path(&p);
            app.review.teaser_open = false;
        }
    }
    if !open {
        app.review.teaser_open = false;
    }
}
