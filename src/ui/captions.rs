//! Left-panel "Captions" tab: auto-captions with whisper.cpp.

use crate::app::EditorApp;
use crate::captions::{self, Engine, Segment, Source};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Button, Grid, ProgressBar, RichText, Slider, Ui};
use std::path::PathBuf;

pub const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("tr", "Turkish"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("id", "Indonesian"),
    ("vi", "Vietnamese"),
];

pub struct CaptionsUi {
    engine: Option<Option<Engine>>,
    model: &'static str,
    language: String,
    translate: bool,
    whole_timeline: bool,
    style: usize,
    max_words: usize,
    y: f32,
    replace: bool,
    download: Option<(String, Task<PathBuf>)>,
    job: Option<(Task<Vec<Segment>>, captions::Layout)>,
}

impl Default for CaptionsUi {
    fn default() -> Self {
        Self {
            engine: None,
            model: captions::DEFAULT_MODEL,
            language: "auto".into(),
            translate: false,
            whole_timeline: true,
            style: 0,
            max_words: 6,
            y: 0.33,
            replace: true,
            download: None,
            job: None,
        }
    }
}

impl CaptionsUi {
    /// Detected lazily the first time the tab is shown.
    fn engine(&mut self) -> Option<Engine> {
        self.engine.get_or_insert_with(captions::detect_engine).clone()
    }

    fn layout(&self) -> captions::Layout {
        let styles = captions::caption_styles();
        let style = styles.get(self.style).map(|s| s.1.clone()).unwrap_or_default();
        captions::Layout { style, max_words: self.max_words, y: self.y, replace: self.replace }
    }
}

/// Applies finished downloads and transcriptions. Called every frame.
pub fn poll(app: &mut EditorApp) {
    let download = app.captions.download.as_ref().and_then(|(name, t)| t.take_finished().map(|s| (name.clone(), s)));
    if let Some((name, state)) = download {
        app.captions.download = None;
        match state {
            TaskState::Done(_) => app.notify(format!("Speech model “{name}” is ready")),
            TaskState::Failed(e) => app.notify_error(format!("Model download failed: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify("Download cancelled"),
        }
    }
    let job = app.captions.job.as_ref().and_then(|(t, layout)| t.take_finished().map(|s| (s, layout.clone())));
    if let Some((state, layout)) = job {
        app.captions.job = None;
        match state {
            TaskState::Done(segments) if segments.is_empty() => app.notify("No speech detected"),
            TaskState::Done(segments) => {
                let (ti, n) = captions::apply_captions(&mut app.project, &segments, &layout);
                let name = app.project.tracks.get(ti).map(|t| t.name.clone()).unwrap_or_default();
                app.selection = None;
                app.notify(format!("Added {n} captions on the “{name}” track — edit them like any text clip"));
            }
            TaskState::Failed(e) => app.notify_error(format!("Captioning failed: {e}")),
            TaskState::Cancelled | TaskState::Running { .. } => app.notify("Captioning cancelled"),
        }
    }
}

/// The selected clip if it has sound to transcribe.
fn selected_audio_clip(app: &EditorApp) -> Option<crate::model::Id> {
    let clip = app.selection.and_then(|id| app.project.clip(id))?;
    let media = clip.media_id().and_then(|m| app.project.media(m))?;
    media.has_sound().then_some(clip.id)
}

pub fn tab(ui: &mut Ui, app: &mut EditorApp) {
    widgets::section_title(ui, "Auto captions");
    ui.label(
        widgets::dim("Turns speech into timed, editable captions. Runs on your computer with whisper.cpp — nothing is uploaded.")
            .small(),
    );
    ui.add_space(6.0);

    let Some(engine) = app.captions.engine() else {
        ui.label(RichText::new("No speech engine found").color(theme::DANGER).strong());
        ui.label(widgets::dim(
            "Install an FFmpeg 8+ build that includes whisper (for example the gyan.dev “full” build on Windows), \
             or whisper.cpp's whisper-cli, then restart RustCut.",
        ));
        if ui.button("Check again").clicked() {
            app.captions.engine = None;
        }
        return;
    };

    let selected = selected_audio_clip(app);
    if selected.is_none() {
        app.captions.whole_timeline = true;
    }
    let busy = app.captions.job.is_some() || app.captions.download.is_some();
    let c = &mut app.captions;

    ui.add_enabled_ui(!busy, |ui| {
        Grid::new("captions-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Source");
            ui.vertical(|ui| {
                ui.radio_value(&mut c.whole_timeline, true, "Whole timeline");
                ui.add_enabled_ui(selected.is_some(), |ui| {
                    ui.radio_value(&mut c.whole_timeline, false, "Selected clip")
                        .on_disabled_hover_text("Select a video or audio clip that has sound");
                });
            });
            ui.end_row();

            ui.label("Model");
            let current = captions::model_info(c.model).map_or("", |m| m.label);
            egui::ComboBox::from_id_salt("cap-model").selected_text(current).width(180.0).show_ui(ui, |ui| {
                for m in captions::MODELS {
                    let mark = if captions::is_downloaded(m.name) { "✓ " } else { "" };
                    ui.selectable_value(&mut c.model, m.name, format!("{mark}{} · {} MB", m.label, m.size_mb));
                }
            });
            ui.end_row();

            ui.label("Language");
            let lang_label = LANGUAGES.iter().find(|l| l.0 == c.language).map_or("Auto-detect", |l| l.1);
            egui::ComboBox::from_id_salt("cap-lang").selected_text(lang_label).width(180.0).show_ui(ui, |ui| {
                for (code, name) in LANGUAGES {
                    ui.selectable_value(&mut c.language, code.to_string(), *name);
                }
            });
            ui.end_row();

            ui.label("");
            ui.checkbox(&mut c.translate, "Translate to English");
            ui.end_row();

            ui.label("Style");
            let styles = captions::caption_styles();
            egui::ComboBox::from_id_salt("cap-style")
                .selected_text(styles.get(c.style).map_or("", |s| s.0))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (i, (name, _)) in styles.iter().enumerate() {
                        ui.selectable_value(&mut c.style, i, *name);
                    }
                });
            ui.end_row();

            ui.label("Words each");
            ui.add(Slider::new(&mut c.max_words, 1..=15))
                .on_hover_text("1–3 for punchy TikTok-style captions, 8+ for classic subtitles");
            ui.end_row();

            ui.label("Position");
            ui.add(Slider::new(&mut c.y, -0.45..=0.45).custom_formatter(|v, _| {
                if v < -0.15 {
                    "Top".into()
                } else if v > 0.15 {
                    "Bottom".into()
                } else {
                    "Middle".into()
                }
            }));
            ui.end_row();

            ui.label("");
            ui.checkbox(&mut c.replace, "Replace previous auto-captions");
            ui.end_row();
        });
    });
    ui.add_space(8.0);

    let w = ui.available_width();
    let model = app.captions.model;
    let info = captions::model_info(model).expect("known model");
    if let Some((name, task)) = &app.captions.download {
        let (p, _) = task.progress().unwrap_or((1.0, String::new()));
        ui.label(format!("Downloading the “{name}” model…"));
        ui.add(ProgressBar::new(p).show_percentage().animate(true));
        if ui.button("Cancel").clicked() {
            task.cancel();
        }
    } else if !captions::is_downloaded(model) {
        let label = format!("⬇  Download {} model ({} MB)", info.name, info.size_mb);
        if ui.add_sized([w, 34.0], Button::new(RichText::new(label).strong()).fill(theme::SURFACE)).clicked() {
            let name = model.to_string();
            let task_name = name.clone();
            let task = Task::spawn(ui.ctx().clone(), move |cancel, report| {
                captions::download_model(&task_name, cancel, |p| report(p, "Downloading…"))
            });
            app.captions.download = Some((name, task));
        }
        ui.label(widgets::dim("One-time download from huggingface.co (ggerganov/whisper.cpp).").small());
    } else if let Some((task, _)) = &app.captions.job {
        let (p, stage) = task.progress().unwrap_or((1.0, "Finishing…".into()));
        ui.label(stage);
        ui.add(ProgressBar::new(p).show_percentage().animate(true));
        if ui.button("Cancel").clicked() {
            task.cancel();
        }
    } else {
        let source = if app.captions.whole_timeline { Some(Source::Timeline) } else { selected.map(Source::Clip) };
        let ready = source.is_some() && app.project.duration() > 0.0;
        let button = Button::new(RichText::new("✨  Generate captions").strong().color(egui::Color32::BLACK)).fill(theme::ACCENT);
        if ui.add_enabled(ready, button.min_size(egui::vec2(w, 36.0))).clicked() {
            if let Some(source) = source {
                let project = app.project.clone();
                let opts = captions::Options {
                    model: captions::model_path(model),
                    language: app.captions.language.clone(),
                    translate: app.captions.translate,
                    max_len: None,
                };
                let engine = engine.clone();
                let task = Task::spawn(ui.ctx().clone(), move |cancel, report| {
                    captions::transcribe(&project, source, &opts, &engine, cancel, |p, s| report(p, s))
                });
                app.captions.job = Some((task, app.captions.layout()));
            }
        }
        if app.project.duration() <= 0.0 {
            ui.label(widgets::dim("Add a clip with speech to the timeline first.").small());
        }
    }

    // Existing captions.
    let caption_clips: usize =
        app.project.tracks.iter().filter(|t| t.captions).map(|t| t.clips.len()).sum();
    let segments = captions::timeline_segments(&app.project);
    if caption_clips > 0 || !segments.is_empty() {
        ui.add_space(12.0);
        ui.separator();
        if caption_clips > 0 {
            ui.label(format!("{caption_clips} captions on the timeline"));
            if ui
                .add_enabled(!busy, Button::new("Apply style & position to them"))
                .on_hover_text("Restyle every auto-caption with the settings above")
                .clicked()
            {
                let layout = app.captions.layout();
                let n = captions::restyle_captions(&mut app.project, &layout.style, layout.y);
                app.notify(format!("Restyled {n} captions"));
            }
        }
        if ui.button("💾  Export .srt subtitles…").clicked() {
            let name = format!("{}.srt", app.project.name);
            if let Some(path) = rfd::FileDialog::new().add_filter("SubRip subtitles", &["srt"]).set_file_name(name).save_file() {
                match std::fs::write(&path, captions::to_srt(&segments)) {
                    Ok(()) => app.notify(format!("Saved {}", path.display())),
                    Err(e) => app.notify_error(format!("Could not save subtitles: {e}")),
                }
            }
        }
    }

    ui.add_space(12.0);
    ui.label(
        widgets::dim(format!(
            "Engine: {}. Captions are ordinary text clips: fix wording in the inspector, drag to retime.",
            engine.label()
        ))
        .small(),
    );
}
