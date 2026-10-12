//! "Rough cut by prompt" window.

use crate::ai::openrouter::Client;
use crate::ai::rough_cut::{self, Plan, RoughCutRequest};
use crate::app::EditorApp;
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Context, Grid, ProgressBar, RichText, ScrollArea, TextEdit};

pub struct RoughCutUi {
    pub open: bool,
    prompt: String,
    target: String,
    use_transcript: bool,
    use_video: bool,
    speech_ok: Option<bool>,
    job: Option<Task<Plan>>,
    /// The plan under review (its cuts are shaded on the timeline until applied).
    pub plan: Option<Plan>,
    applied: bool,
    /// Use the local (Ollama) model instead of OpenRouter.
    local: bool,
}

impl Default for RoughCutUi {
    fn default() -> Self {
        Self {
            open: false,
            prompt: String::new(),
            target: String::new(),
            use_transcript: true,
            use_video: true,
            speech_ok: None,
            job: None,
            plan: None,
            applied: false,
            local: false,
        }
    }
}

impl RoughCutUi {
    /// Cut ranges to shade on the timeline while a plan is being reviewed.
    pub fn preview_cuts(&self) -> Option<Vec<(f64, f64)>> {
        self.plan.as_ref().filter(|_| !self.applied).map(Plan::removed)
    }
}

/// Parses "60", "90s", "1:30" or "2m" into seconds.
pub fn parse_length(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Some((m, sec)) = s.split_once(':') {
        return Some(m.trim().parse::<f64>().ok()? * 60.0 + sec.trim().parse::<f64>().ok()?);
    }
    if let Some(m) = s.strip_suffix("min").or_else(|| s.strip_suffix('m')) {
        return Some(m.trim().parse::<f64>().ok()? * 60.0);
    }
    s.trim_end_matches('s').trim().parse::<f64>().ok().filter(|v| *v > 0.0)
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.rough_cut.job.as_ref().and_then(Task::take_finished) {
        app.rough_cut.job = None;
        match state {
            TaskState::Done(plan) => {
                app.ai.session_cost += plan.usage.cost;
                app.rough_cut.applied = false;
                app.rough_cut.plan = Some(plan);
                app.notify("Rough cut ready — the red areas on the timeline would be cut. Review, then Apply.");
            }
            TaskState::Failed(e) => app.notify_error(format!("Rough cut failed: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify("Rough cut cancelled"),
        }
    }
}

fn copy_row(ui: &mut egui::Ui, label: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());
        if ui.small_button("📋 Copy").clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    });
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.rough_cut.open {
        return;
    }
    app.ai.ensure_models(ctx);
    if app.rough_cut.speech_ok.is_none() {
        app.rough_cut.speech_ok = Some(rough_cut::speech_setup().is_some());
    }
    let mut open = true;
    let mut analyze = false;
    let mut apply = false;
    let mut discard = false;
    let duration = app.project.duration();
    egui::Window::new(RichText::new("Rough cut by prompt (AI)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(520.0);
            if app.ai.settings.api_key().is_none() {
                ui.label(RichText::new("Add your OpenRouter key first.").color(theme::DANGER));
                if ui.button("🔑  OpenRouter settings…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
                return;
            }
            // ── results ──
            if let Some(plan) = app.rough_cut.plan.clone() {
                let kept = plan.kept_seconds();
                ui.label(
                    RichText::new(format!(
                        "{} → {}  ·  keeps {} part(s){}",
                        widgets::format_short(plan.source_duration),
                        widgets::format_short(kept),
                        plan.keep.len(),
                        if plan.usage.cost > 0.0 { format!("  ·  cost ${:.3}", plan.usage.cost) } else { String::new() }
                    ))
                    .strong()
                    .size(15.0),
                );
                if !plan.summary.is_empty() {
                    ui.label(widgets::dim(&plan.summary));
                }
                ui.label(
                    widgets::dim(format!(
                        "Based on {}{}.",
                        if plan.transcript_segments > 0 { format!("{} transcript segments", plan.transcript_segments) } else { "no transcript".into() },
                        if plan.sent_video { " and the video" } else { "" }
                    ))
                    .small(),
                );
                let blind = plan.kept_without_picture();
                if blind >= 0.5 {
                    ui.label(
                        RichText::new(format!(
                            "⚠ {} of the kept parts have no picture (black screen, audio only).",
                            widgets::format_short(blind)
                        ))
                        .color(theme::DANGER),
                    );
                }
                if let Some((a, b)) = plan.no_picture.iter().copied().max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0))) {
                    if b - a >= 5.0 {
                        ui.label(
                            widgets::dim(format!(
                                "Your timeline is black from {} to {} — check the source footage.",
                                widgets::format_short(a),
                                widgets::format_short(b)
                            ))
                            .small(),
                        );
                    }
                }
                ui.add_space(4.0);
                ScrollArea::vertical().max_height(150.0).id_salt("rc-keep").show(ui, |ui| {
                    for k in &plan.keep {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("{} – {}", widgets::format_time(k.start), widgets::format_time(k.end))).monospace().color(theme::ACCENT));
                            ui.label(widgets::dim(&k.reason));
                        });
                    }
                });
                ui.add_space(6.0);
                if app.rough_cut.applied {
                    ui.label(RichText::new("✔ Applied — press Ctrl+Z to undo the whole cut.").color(theme::ACCENT));
                } else {
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, "✂  Apply rough cut").clicked() {
                            apply = true;
                        }
                        if ui.button("Discard").clicked() {
                            discard = true;
                        }
                    });
                    ui.label(widgets::dim("Red areas on the timeline will be removed from every track; later clips close up.").small());
                }
                ui.separator();
                if !plan.titles.is_empty() {
                    copy_row(ui, "Title ideas", &plan.titles.join("\n"));
                    for t in &plan.titles {
                        ui.label(format!("• {t}"));
                    }
                }
                let chapters = plan.chapters_text();
                if !plan.description.is_empty() || !chapters.is_empty() {
                    let full = if chapters.is_empty() { plan.description.clone() } else { format!("{}\n\n{chapters}", plan.description) };
                    copy_row(ui, "YouTube description + chapters", &full);
                    ScrollArea::vertical().max_height(140.0).id_salt("rc-desc").show(ui, |ui| {
                        ui.label(widgets::dim(&full));
                    });
                }
                ui.add_space(6.0);
                if ui.button("↺  New rough cut").clicked() {
                    discard = true;
                }
                return;
            }

            // ── request ──
            ui.horizontal(|ui| {
                ui.label("Engine");
                ui.selectable_value(&mut app.rough_cut.local, false, "☁ OpenRouter (reads + watches, paid)");
                ui.selectable_value(&mut app.rough_cut.local, true, "💻 Local (free, offline)");
            });
            let local = app.rough_cut.local;
            ui.label(widgets::dim(if local {
                "Describe the edit. A local model reads each sentence of the transcript and keeps, cuts or trims it; long pauses are removed. Nothing leaves your computer."
            } else {
                "Describe the edit. The model reads the transcript and watches a small copy of your timeline, then marks what to keep."
            }));
            ui.add(
                TextEdit::multiline(&mut app.rough_cut.prompt)
                    .hint_text("e.g. 60-second highlight of this vlog. Keep the funny bits, cut the ums and long pauses.")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            let speech_ok = app.rough_cut.speech_ok == Some(true);
            let model_id = app.ai.settings.edit_model.clone();
            let model = app.ai.models.iter().find(|m| m.id == model_id).cloned();
            let video_ok = !local && model.as_ref().is_none_or(|m| m.video_input);
            if local {
                app.rough_cut.use_transcript = speech_ok;
            }
            Grid::new("rc-grid").num_columns(2).spacing([10.0, 7.0]).show(ui, |ui| {
                ui.label("Target length");
                ui.add(TextEdit::singleline(&mut app.rough_cut.target).hint_text("optional, e.g. 60 or 1:30").desired_width(160.0));
                ui.end_row();
                ui.label("Use");
                ui.vertical(|ui| {
                    ui.add_enabled_ui(speech_ok, |ui| {
                        ui.checkbox(&mut app.rough_cut.use_transcript, "The transcript (speech, ums, pauses)")
                            .on_disabled_hover_text("Download a speech model in the Captions tab first");
                    });
                    if !speech_ok {
                        app.rough_cut.use_transcript = false;
                        ui.label(widgets::dim("No speech model yet — download one in the Captions tab.").small());
                    }
                    ui.add_enabled_ui(video_ok, |ui| {
                        ui.checkbox(&mut app.rough_cut.use_video, "The video (reactions, action, laughter)");
                    });
                    if !video_ok {
                        app.rough_cut.use_video = false;
                        ui.label(widgets::dim(if local { "The local engine works from the transcript only." } else { "This model can't watch video." }).small());
                    }
                });
                ui.end_row();
                if local {
                    ui.label("Local model");
                    ui.horizontal(|ui| {
                        ui.label(&app.ai.settings.local_model);
                        if ui.small_button("Change…").clicked() {
                            app.ai.open_settings(ui.ctx());
                        }
                    });
                    ui.end_row();
                    return;
                }
                ui.label("Editor model");
                let mut changed = false;
                egui::ComboBox::from_id_salt("rc-model").selected_text(&model_id).width(300.0).height(360.0).show_ui(ui, |ui| {
                    for m in app.ai.models.iter().filter(|m| m.video_input && m.structured_output) {
                        let text = format!("{}  ·  ${:.2}/M in · ${:.2}/M out", m.id, m.prompt_per_m, m.completion_per_m);
                        if ui.selectable_label(m.id == model_id, text).clicked() {
                            app.ai.settings.edit_model = m.id.clone();
                            changed = true;
                        }
                    }
                });
                if changed {
                    let _ = app.ai.settings.save();
                }
                ui.end_row();
            });
            if local {
                ui.label(RichText::new("Cost: free (runs on your computer, about 1–3 s per sentence)").strong());
            } else if let Some(m) = &model {
                let est = rough_cut::estimate_cost(duration, app.rough_cut.use_video, m.prompt_per_m, m.completion_per_m);
                ui.label(RichText::new(format!("Estimated cost: about ${est:.2} for {} of timeline", widgets::format_short(duration))).strong());
            }
            if let Some(job) = &app.rough_cut.job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
                ui.label(stage);
                ui.add(ProgressBar::new(p).show_percentage().animate(true));
                if ui.button("Cancel").clicked() {
                    job.cancel();
                }
                return;
            }
            let ready = !app.rough_cut.prompt.trim().is_empty()
                && duration >= 1.0
                && (app.rough_cut.use_transcript || app.rough_cut.use_video);
            if ui
                .add_enabled(ready, Button::new(RichText::new("✨  Make the rough cut").strong().color(egui::Color32::BLACK)).fill(theme::ACCENT))
                .clicked()
            {
                analyze = true;
            }
            if duration < 1.0 {
                ui.label(widgets::dim("Add clips to the timeline first.").small());
            }
            ui.label(widgets::dim("Nothing changes until you review the result and click Apply.").small());
        });

    if analyze && app.rough_cut.local {
        let req = RoughCutRequest {
            prompt: app.rough_cut.prompt.trim().to_string(),
            target_seconds: parse_length(&app.rough_cut.target),
            use_transcript: true,
            use_video: false,
        };
        let project = app.project.clone();
        let decider = crate::ui::local_ai::decider(app);
        app.rough_cut.job = Some(Task::spawn(ctx.clone(), move |cancel, report| crate::ai::local::rough_cut(&project, &req, &decider, cancel, report)));
    } else if analyze {
        if let Some(key) = app.ai.settings.api_key() {
            let req = RoughCutRequest {
                prompt: app.rough_cut.prompt.trim().to_string(),
                target_seconds: parse_length(&app.rough_cut.target),
                use_transcript: app.rough_cut.use_transcript,
                use_video: app.rough_cut.use_video,
            };
            let project = app.project.clone();
            let model = app.ai.settings.edit_model.clone();
            app.rough_cut.job = Some(Task::spawn(ctx.clone(), move |cancel, report| {
                rough_cut::analyze(&project, &req, &Client::new(key), &model, cancel, report)
            }));
        }
    }
    if apply {
        if let Some(plan) = app.rough_cut.plan.clone() {
            if (app.project.duration() - plan.source_duration).abs() > 0.05 {
                app.notify_error("The timeline changed since the analysis — run the rough cut again.");
            } else {
                app.pause();
                let removed = plan.apply(&mut app.project);
                app.selection = None;
                app.seek(0.0);
                app.rough_cut.applied = true;
                app.notify(format!(
                    "Rough cut applied: removed {} — now {}. Ctrl+Z undoes it.",
                    widgets::format_short(removed),
                    widgets::format_short(app.project.duration())
                ));
            }
        }
    }
    if discard {
        app.rough_cut.plan = None;
        app.rough_cut.applied = false;
    }
    if !open {
        app.rough_cut.open = false;
        if !app.rough_cut.applied {
            app.rough_cut.plan = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(parse_length("60"), Some(60.0));
        assert_eq!(parse_length("90s"), Some(90.0));
        assert_eq!(parse_length("1:30"), Some(90.0));
        assert_eq!(parse_length("2m"), Some(120.0));
        assert_eq!(parse_length("2 min"), Some(120.0));
        assert_eq!(parse_length(""), None);
        assert_eq!(parse_length("abc"), None);
    }
}
