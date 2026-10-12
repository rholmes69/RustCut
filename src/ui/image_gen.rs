//! AI image UI: the "Generate image" window (thumbnail maker and stills) and the
//! inspector's "Background fill" section.

use crate::ai::image_gen::{self, ImageModel, Job, Output, StillKind, ThumbStyle};
use crate::ai::openrouter::Client;
use crate::ai::video_gen;
use crate::app::EditorApp;
use crate::media::ffmpeg;
use crate::model::{Id, Project};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Context, Grid, ProgressBar, RichText, TextEdit, TextureHandle, Ui};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Thumbnail,
    Stills,
}

/// A small RGBA preview of a generated image.
type Preview = (u32, u32, Vec<u8>);

struct ImageJob {
    job: Job,
    /// Timeline time for stills.
    start: f64,
    task: Task<(Output, Option<Preview>)>,
}

pub struct ImageGenUi {
    pub open: bool,
    pub tab: Tab,
    headline: String,
    style: ThumbStyle,
    extra: String,
    kind: StillKind,
    prompt: String,
    models: Vec<ImageModel>,
    models_job: Option<Task<Vec<ImageModel>>>,
    models_error: Option<String>,
    jobs: Vec<ImageJob>,
    thumb_tex: Option<(std::path::PathBuf, TextureHandle)>,
    thumb_preview: Option<(std::path::PathBuf, Preview)>,
}

impl Default for ImageGenUi {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Thumbnail,
            headline: String::new(),
            style: ThumbStyle::Bold,
            extra: String::new(),
            kind: StillKind::TitleCard,
            prompt: String::new(),
            models: Vec::new(),
            models_job: None,
            models_error: None,
            jobs: Vec::new(),
            thumb_tex: None,
            thumb_preview: None,
        }
    }
}

impl ImageGenUi {
    pub fn busy_on(&self, clip: Id) -> bool {
        self.jobs.iter().any(|j| matches!(j.job, Job::Plate { clip: c } if c == clip))
    }

    fn busy_with(&self, f: impl Fn(&Job) -> bool) -> bool {
        self.jobs.iter().any(|j| f(&j.job))
    }
}

fn ensure_models(app: &mut EditorApp, ctx: &Context) {
    let g = &mut app.image_gen;
    if !g.models.is_empty() || g.models_job.is_some() || g.models_error.is_some() {
        return;
    }
    let Some(key) = app.ai.settings.api_key() else { return };
    g.models_job = Some(Task::spawn(ctx.clone(), move |_, _| image_gen::list_models(&Client::new(key)).map(Some)));
}

/// Starts generating `job`. Results are applied in [`poll`].
pub fn start(app: &mut EditorApp, ctx: &Context, job: Job) {
    let Some(key) = app.ai.settings.api_key() else {
        app.notify_error(crate::ai::openrouter::NO_KEY);
        app.ai.open_settings(ctx);
        return;
    };
    let project: Project = app.project.clone();
    let dir = video_gen::output_dir(app.project_path.as_deref());
    let model = app.ai.settings.image_model.clone();
    let start = app.playhead;
    let preview = matches!(job, Job::Thumbnail { .. });
    let j = job.clone();
    let task = Task::spawn(ctx.clone(), move |_, report| {
        report(0.1, "Generating image…");
        let out = image_gen::run(&Client::new(key), &project, &j, &model, &dir)?;
        let thumb = if preview {
            let (w, h) = ffmpeg::fit_within(out.info.width, out.info.height, 420, 300);
            ffmpeg::extract_frame(&out.path, 0.0, w, h).ok().map(|rgba| (w, h, rgba))
        } else {
            None
        };
        Ok(Some((out, thumb)))
    });
    app.image_gen.jobs.push(ImageJob { job, start, task });
}

/// Applies finished jobs. Called every frame.
pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.image_gen.models_job.as_ref().and_then(Task::take_finished) {
        app.image_gen.models_job = None;
        match state {
            TaskState::Done(m) => app.image_gen.models = m,
            TaskState::Failed(e) => app.image_gen.models_error = Some(e),
            _ => {}
        }
    }
    let mut i = 0;
    while i < app.image_gen.jobs.len() {
        let Some(state) = app.image_gen.jobs[i].task.take_finished() else {
            i += 1;
            continue;
        };
        let job = app.image_gen.jobs.remove(i);
        let label = job.job.label();
        match state {
            TaskState::Done((out, preview)) => {
                app.ai.session_cost += out.cost.unwrap_or(0.0);
                let cost = out.cost.map_or(String::new(), |c| format!(" · cost ${c:.3}"));
                match job.job {
                    Job::Thumbnail { .. } => {
                        app.project.thumbnail = Some(out.path.clone());
                        if let Some(p) = preview {
                            app.image_gen.thumb_preview = Some((out.path.clone(), p));
                        }
                        app.notify(format!("🖼 Thumbnail saved — {}{cost}", out.path.display()));
                    }
                    Job::Still { kind, .. } => {
                        let (media, clip) = image_gen::place_still(&mut app.project, kind, &out, job.start);
                        if let Some(item) = app.project.media(media).cloned() {
                            app.assets.ensure(&item);
                        }
                        app.selected_media = Some(media);
                        if clip.is_some() {
                            app.selection = clip;
                        }
                        app.notify(format!("✨ {label} added on an overlay track{cost}"));
                    }
                    Job::Plate { clip } => match image_gen::apply_plate(&mut app.project, clip, &out) {
                        Some(media) => {
                            if let Some(item) = app.project.media(media).cloned() {
                                app.assets.ensure(&item);
                            }
                            app.notify(format!("✨ Background fill added{cost}"));
                        }
                        None => app.notify_error("Background fill finished, but the clip was deleted"),
                    },
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("{label} failed: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify(format!("{label} stopped")),
        }
    }
}

fn model_picker(ui: &mut Ui, app: &mut EditorApp, need_reference: bool) {
    let current = app.ai.settings.image_model.clone();
    let mut changed = false;
    egui::ComboBox::from_id_salt("image-model").selected_text(&current).width(280.0).height(360.0).show_ui(ui, |ui| {
        if app.image_gen.models_job.is_some() {
            ui.spinner();
        }
        for m in app.image_gen.models.clone() {
            let mut tags = Vec::new();
            if m.reference_input {
                tags.push("edits images");
            }
            if m.transparent {
                tags.push("transparent");
            }
            let text = if tags.is_empty() { m.id.clone() } else { format!("{}  ·  {}", m.id, tags.join(", ")) };
            let resp = ui.add_enabled(!need_reference || m.reference_input, egui::SelectableLabel::new(m.id == current, text));
            if resp.clicked() {
                app.ai.settings.image_model = m.id.clone();
                changed = true;
            }
        }
    });
    if changed {
        let _ = app.ai.settings.save();
    }
}

fn jobs_list(ui: &mut Ui, app: &EditorApp, filter: impl Fn(&Job) -> bool) {
    for job in app.image_gen.jobs.iter().filter(|j| filter(&j.job)) {
        let (p, stage) = job.task.progress().unwrap_or((1.0, "Finishing…".into()));
        ui.label(RichText::new(job.job.label()).strong());
        ui.horizontal(|ui| {
            ui.add(ProgressBar::new(p).desired_width(220.0).text(stage).animate(true));
            if ui.small_button("Stop").on_hover_text("Stop waiting (the provider may still bill the image)").clicked() {
                job.task.cancel();
            }
        });
    }
}

fn go_button(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    let w = ui.available_width();
    ui.add_enabled(enabled, Button::new(RichText::new(text).strong().color(egui::Color32::BLACK)).fill(theme::ACCENT).min_size(egui::vec2(w, 30.0)))
        .clicked()
}

fn thumbnail_tab(ui: &mut Ui, app: &mut EditorApp) {
    ui.label(widgets::dim("Uses the frame at the playhead as the photo and turns it into a YouTube-style thumbnail."));
    ui.label(widgets::dim(format!("Frame: {}  (move the playhead to pick another)", widgets::format_time(app.playhead))).small());
    ui.add_space(4.0);
    let g = &mut app.image_gen;
    Grid::new("thumb-grid").num_columns(2).spacing([10.0, 7.0]).show(ui, |ui| {
        ui.label("Headline");
        ui.add(TextEdit::singleline(&mut g.headline).hint_text("e.g. I TRIED IT FOR 30 DAYS").desired_width(300.0));
        ui.end_row();
        ui.label("Style");
        egui::ComboBox::from_id_salt("thumb-style").selected_text(g.style.label()).show_ui(ui, |ui| {
            for s in ThumbStyle::ALL {
                ui.selectable_value(&mut g.style, s, s.label());
            }
        });
        ui.end_row();
        ui.label("Also");
        ui.add(TextEdit::singleline(&mut g.extra).hint_text("optional, e.g. add a red arrow pointing at the cake").desired_width(300.0));
        ui.end_row();
    });
    ui.horizontal(|ui| {
        ui.label("Model");
        model_picker(ui, app, true);
    });
    ui.add_space(6.0);
    let busy = app.image_gen.busy_with(|j| matches!(j, Job::Thumbnail { .. }));
    if go_button(ui, "🖼  Make thumbnail  (a few cents)", !busy) {
        let g = &app.image_gen;
        let job = Job::Thumbnail { time: app.playhead, headline: g.headline.clone(), style: g.style, extra: g.extra.clone() };
        let ctx = ui.ctx().clone();
        start(app, &ctx, job);
    }
    jobs_list(ui, app, |j| matches!(j, Job::Thumbnail { .. }));

    let Some(path) = app.project.thumbnail.clone() else { return };
    ui.separator();
    if let Some((p, (w, h, rgba))) = app.image_gen.thumb_preview.take() {
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let tex = ui.ctx().load_texture("thumbnail-preview", img, egui::TextureOptions::LINEAR);
        app.image_gen.thumb_tex = Some((p, tex));
    }
    match &app.image_gen.thumb_tex {
        Some((p, tex)) if *p == path => {
            let size = tex.size_vec2();
            let scale = (420.0 / size.x).min(1.0);
            ui.image((tex.id(), size * scale));
        }
        _ => {
            ui.label(widgets::dim("Current thumbnail:"));
        }
    }
    ui.label(widgets::dim(path.display().to_string()).small());
    ui.horizontal(|ui| {
        if ui.button("📂  Show in folder").clicked() {
            crate::ui::library::reveal(&path);
        }
        if ui.button("Remove").on_hover_text("Forget this thumbnail (the file stays on disk)").clicked() {
            app.project.thumbnail = None;
            app.image_gen.thumb_tex = None;
        }
    });
    ui.label(widgets::dim("Saved next to the video when you export (option in the Export window).").small());
}

fn stills_tab(ui: &mut Ui, app: &mut EditorApp) {
    ui.label(widgets::dim("Generates a picture and drops it on an overlay track at the playhead. It's an ordinary image clip: move, resize, animate it."));
    ui.add_space(4.0);
    let g = &mut app.image_gen;
    Grid::new("still-grid").num_columns(2).spacing([10.0, 7.0]).show(ui, |ui| {
        ui.label("Make a");
        ui.horizontal(|ui| {
            for k in StillKind::ALL {
                ui.selectable_value(&mut g.kind, k, k.label());
            }
        });
        ui.end_row();
        ui.label("Of");
        let hint = match g.kind {
            StillKind::TitleCard => "e.g. “Day 1 in Lisbon”, warm sunset colours, film grain",
            StillKind::LowerThird => "e.g. sleek dark-teal bar for a name and job title",
            StillKind::Sticker => "e.g. a cartoon thumbs-up with sparkles",
        };
        ui.add(TextEdit::multiline(&mut g.prompt).hint_text(hint).desired_rows(2).desired_width(300.0));
        ui.end_row();
    });
    ui.horizontal(|ui| {
        ui.label("Model");
        model_picker(ui, app, false);
    });
    let kind = app.image_gen.kind;
    if kind.transparent() {
        let current = app.ai.settings.image_model.clone();
        let can = app.image_gen.models.iter().any(|m| m.id == current && m.transparent);
        if !can {
            ui.label(widgets::dim(format!("Needs a transparent background, so {} is used for this one.", image_gen::DEFAULT_TRANSPARENT_MODEL)).small());
        }
    }
    ui.add_space(6.0);
    let ready = !app.image_gen.prompt.trim().is_empty();
    if go_button(ui, &format!("✨  Generate {}  (a few cents)", kind.label().to_lowercase()), ready) {
        let job = Job::Still { kind, prompt: app.image_gen.prompt.trim().to_string() };
        let ctx = ui.ctx().clone();
        start(app, &ctx, job);
    }
    jobs_list(ui, app, |j| matches!(j, Job::Still { .. }));
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.image_gen.open {
        return;
    }
    ensure_models(app, ctx);
    let mut open = true;
    egui::Window::new(RichText::new("Generate image (AI)").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(450.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(450.0);
            if app.ai.settings.api_key().is_none() {
                ui.label(RichText::new("Add your OpenRouter key first.").color(theme::DANGER));
                if ui.button("🔑  OpenRouter settings…").clicked() {
                    app.ai.open_settings(ui.ctx());
                }
                return;
            }
            if let Some(e) = app.image_gen.models_error.clone() {
                ui.label(RichText::new(format!("Could not load image models: {e}")).color(theme::DANGER));
                if ui.button("Retry").clicked() {
                    app.image_gen.models_error = None;
                }
                return;
            }
            ui.horizontal(|ui| {
                let g = &mut app.image_gen;
                ui.selectable_value(&mut g.tab, Tab::Thumbnail, RichText::new("🖼 Thumbnail").strong());
                ui.selectable_value(&mut g.tab, Tab::Stills, RichText::new("✨ Title card / lower third / sticker").strong());
            });
            ui.separator();
            match app.image_gen.tab {
                Tab::Thumbnail => thumbnail_tab(ui, app),
                Tab::Stills => stills_tab(ui, app),
            }
            if app.ai.session_cost > 0.0 {
                ui.add_space(4.0);
                ui.label(widgets::dim(format!("Spent this session: ${:.3}", app.ai.session_cost)).small());
            }
        });
    if !open {
        app.image_gen.open = false;
    }
}

/// Inspector section: fill the canvas behind a clip that doesn't cover it.
pub fn plate_section(ui: &mut Ui, app: &mut EditorApp, clip_id: Id) {
    let Some(clip) = app.project.clip(clip_id).cloned() else { return };
    if app.image_gen.busy_on(clip_id) {
        jobs_list(ui, app, |j| matches!(j, Job::Plate { clip } if *clip == clip_id));
        return;
    }
    if let Some(plate) = clip.background_plate {
        let name = app.project.media(plate).map(|m| m.name.clone()).unwrap_or_default();
        ui.label(widgets::dim(format!("Behind this clip: {name}")).small());
        ui.horizontal(|ui| {
            if ui.button("↻  Regenerate").clicked() {
                let ctx = ui.ctx().clone();
                start(app, &ctx, Job::Plate { clip: clip_id });
            }
            if ui.button("Remove").clicked() {
                if let Some(c) = app.project.clip_mut(clip_id) {
                    c.background_plate = None;
                }
            }
        });
        return;
    }
    if !image_gen::needs_plate(&app.project, &clip) {
        ui.label(widgets::dim("This clip already fills the canvas. Use it when a landscape clip sits in a vertical canvas (or similar) to replace the black bars.").small());
        return;
    }
    ui.label(widgets::dim("Extends this shot's picture to fill the empty bars around it, instead of black or a blurred copy.").small());
    if app.ai.settings.api_key().is_none() {
        if ui.button("🔑  Set up OpenRouter…").clicked() {
            app.ai.open_settings(ui.ctx());
        }
        return;
    }
    if go_button(ui, "✨  Fill background  (a few cents)", true) {
        let ctx = ui.ctx().clone();
        start(app, &ctx, Job::Plate { clip: clip_id });
    }
}
