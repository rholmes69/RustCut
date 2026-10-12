//! Export settings window with progress reporting.

use crate::app::{default_output_path, EditorApp};
use crate::export::{ExportJob, ExportSettings, ExportState};
use crate::model::Project;
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Context, Grid, ProgressBar, RichText, ScrollArea};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Draft,
    Standard,
    High,
}

impl Quality {
    fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft (fast, smaller)",
            Quality::Standard => "Recommended",
            Quality::High => "High (slow, larger)",
        }
    }
    fn crf(self) -> u8 {
        match self {
            Quality::Draft => 28,
            Quality::Standard => 21,
            Quality::High => 17,
        }
    }
    fn preset(self) -> &'static str {
        match self {
            Quality::Draft => "veryfast",
            Quality::Standard => "medium",
            Quality::High => "slow",
        }
    }
}

pub struct ExportDialog {
    pub visible: bool,
    short_side: u32,
    fps: u32,
    quality: Quality,
    path: PathBuf,
    save_srt: bool,
    save_thumbnail: bool,
    job: Option<ExportJob>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            visible: false,
            short_side: 1080,
            fps: 30,
            quality: Quality::Standard,
            path: PathBuf::new(),
            save_srt: true,
            save_thumbnail: true,
            job: None,
        }
    }
}

impl ExportDialog {
    pub fn open(&mut self, project: &Project, project_path: Option<&Path>) {
        if self.job.as_ref().is_some_and(|j| j.is_running()) {
            self.visible = true;
            return;
        }
        self.job = None;
        self.fps = project.fps;
        if self.path.as_os_str().is_empty() || !self.path.parent().is_some_and(|p| p.is_dir()) {
            self.path = default_output_path(project, project_path);
        } else if let Some(dir) = self.path.parent() {
            self.path = dir.join(default_output_path(project, None).file_name().unwrap_or_default());
        }
        self.visible = true;
    }
}

pub fn show(ctx: &Context, app: &mut EditorApp) {
    if !app.export.visible {
        return;
    }
    let mut open = true;
    let duration = app.project.duration();
    let aspect = app.project.aspect;
    let mut start = false;
    let has_captions = app.project.tracks.iter().any(|t| t.captions && !t.clips.is_empty());
    let thumbnail = app.project.thumbnail.clone().filter(|t| t.exists());
    let loudness = app.project.audio.loudness;
    let picture_gaps = app.project.picture_gaps(1.0);
    let mut close = false;
    let dlg = &mut app.export;

    egui::Window::new(RichText::new("Export video").strong())
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_width(440.0);
            match dlg.job.as_ref().map(|j| (j.state(), j.elapsed())) {
                None => {
                    Grid::new("export-grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                        ui.label("File");
                        ui.horizontal(|ui| {
                            let name = dlg.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                                .on_hover_text(dlg.path.display().to_string());
                            if ui.button("Browse…").clicked() {
                                let mut d = rfd::FileDialog::new().add_filter("MP4 video", &["mp4"]);
                                if let Some(dir) = dlg.path.parent() {
                                    d = d.set_directory(dir);
                                }
                                if let Some(name) = dlg.path.file_name() {
                                    d = d.set_file_name(name.to_string_lossy());
                                }
                                if let Some(mut p) = d.save_file() {
                                    if p.extension().is_none() {
                                        p.set_extension("mp4");
                                    }
                                    dlg.path = p;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("Folder");
                        ui.label(widgets::dim(dlg.path.parent().map(|p| p.display().to_string()).unwrap_or_default()));
                        ui.end_row();

                        ui.label("Resolution");
                        egui::ComboBox::from_id_salt("export-res")
                            .selected_text(res_label(dlg.short_side, aspect))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for s in [480, 720, 1080, 1440, 2160] {
                                    ui.selectable_value(&mut dlg.short_side, s, res_label(s, aspect));
                                }
                            });
                        ui.end_row();

                        ui.label("Frame rate");
                        egui::ComboBox::from_id_salt("export-fps")
                            .selected_text(format!("{} fps", dlg.fps))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for f in [24, 25, 30, 50, 60] {
                                    ui.selectable_value(&mut dlg.fps, f, format!("{f} fps"));
                                }
                            });
                        ui.end_row();

                        ui.label("Quality");
                        egui::ComboBox::from_id_salt("export-q")
                            .selected_text(dlg.quality.label())
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for q in [Quality::Draft, Quality::Standard, Quality::High] {
                                    ui.selectable_value(&mut dlg.quality, q, q.label());
                                }
                            });
                        ui.end_row();

                        ui.label("Format");
                        ui.label(widgets::dim("MP4 · H.264 · AAC 192 kbps"));
                        ui.end_row();

                        ui.label("Duration");
                        ui.label(widgets::format_time(duration));
                        ui.end_row();

                        ui.label("Loudness");
                        ui.label(widgets::dim(match loudness {
                            Some(t) => format!("Normalised to {t:.0} LUFS (Audio tab)"),
                            None => "As mixed — normalise in the Audio tab".to_string(),
                        }));
                        ui.end_row();

                        if let Some(t) = &thumbnail {
                            ui.label("Thumbnail");
                            ui.checkbox(&mut dlg.save_thumbnail, "Also save the thumbnail")
                                .on_hover_text(format!("Copies {} next to the video", t.display()));
                            ui.end_row();
                        }

                        if has_captions {
                            ui.label("Subtitles");
                            ui.checkbox(&mut dlg.save_srt, "Also save captions as .srt");
                            ui.end_row();
                        }
                    });
                    for (a, b) in &picture_gaps {
                        ui.label(
                            RichText::new(format!(
                                "⚠ No video from {} to {} — only audio plays there (black screen).",
                                widgets::format_short(*a),
                                widgets::format_short(*b)
                            ))
                            .color(theme::DANGER),
                        );
                    }
                    if !picture_gaps.is_empty() {
                        ui.label(widgets::dim("The video is as long as its longest track. Trim the audio or add clips to fill the gap.").small());
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, "  Export  ").clicked() {
                            start = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Some((ExportState::Running(p), elapsed)) => {
                    ui.label(format!("Rendering {}…", dlg.path.file_name().unwrap_or_default().to_string_lossy()));
                    ui.add_space(6.0);
                    ui.add(ProgressBar::new(p).show_percentage().animate(true));
                    let secs = elapsed.as_secs_f32();
                    let eta = if p > 0.02 { format!(" · ~{:.0}s left", secs / p - secs) } else { String::new() };
                    ui.label(widgets::dim(format!("{secs:.0}s elapsed{eta}")));
                    ui.add_space(8.0);
                    if ui.button("Cancel export").clicked() {
                        if let Some(j) = &dlg.job {
                            j.cancel();
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
                Some((ExportState::Done(path), elapsed)) => {
                    ui.label(RichText::new("✅  Export complete").strong().color(theme::ACCENT).size(15.0));
                    ui.label(widgets::dim(format!("{} · {:.1}s", path.display(), elapsed.as_secs_f32())));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("📂  Show in folder").clicked() {
                            crate::ui::library::reveal(&path);
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
                Some((ExportState::Failed(err), _)) => {
                    ui.label(RichText::new("Export failed").strong().color(theme::DANGER));
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(err).monospace().size(11.0)).wrap());
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            dlg.job = None;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
                Some((ExportState::Cancelled, _)) => {
                    ui.label("Export cancelled.");
                    ui.horizontal(|ui| {
                        if ui.add(Button::new("Back")).clicked() {
                            dlg.job = None;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });

    if start {
        if let Some(t) = app.project.thumbnail.as_ref().filter(|t| app.export.save_thumbnail && t.exists()) {
            if let Err(e) = crate::ai::image_gen::copy_thumbnail_next_to(t, &app.export.path) {
                app.notify_error(format!("Could not save the thumbnail: {e}"));
            }
        }
        if has_captions && app.export.save_srt {
            let srt = app.export.path.with_extension("srt");
            let text = crate::captions::to_srt(&crate::captions::timeline_segments(&app.project));
            if let Err(e) = std::fs::write(&srt, text) {
                app.notify_error(format!("Could not save subtitles: {e}"));
            }
        }
        let settings = ExportSettings {
            path: app.export.path.clone(),
            short_side: app.export.short_side,
            fps: app.export.fps,
            crf: app.export.quality.crf(),
            preset: app.export.quality.preset().into(),
        };
        app.export.job = Some(ExportJob::start(app.project.clone(), settings, ctx.clone()));
    }
    if !open || close {
        // Closing the window keeps a running export going in the background.
        app.export.visible = false;
        if app.export.job.as_ref().is_some_and(|j| j.is_running()) {
            app.notify("Export continues in the background — Ctrl+E to check progress");
        }
    }
}

fn res_label(short: u32, aspect: crate::model::Aspect) -> String {
    let (w, h) = aspect.size_for(short);
    let name = match short {
        480 => "480p",
        720 => "720p HD",
        1080 => "1080p Full HD",
        1440 => "1440p 2K",
        2160 => "2160p 4K",
        _ => "",
    };
    format!("{name}  ({w}×{h})")
}
