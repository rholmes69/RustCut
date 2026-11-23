//! Google Drive window (File → ☁ Google Drive…): connect a Google account once, then upload
//! exported videos and whole projects (as a zip streamed straight to Drive) into a "RustCut"
//! folder on My Drive. See `crate::gdrive`; the sign-in panel is `crate::ui::google`.

use crate::app::EditorApp;
use crate::gdrive::{self, Uploaded};
use crate::google::SCOPE_DRIVE;
use crate::task::{Task, TaskState};
use crate::ui::widgets;
use egui::{Context, RichText};
use std::path::PathBuf;

#[derive(Clone)]
pub enum Upload {
    /// A file on disk, e.g. an exported video.
    File(PathBuf),
    /// The open project and its media, as a zip.
    Project,
}

#[derive(Default)]
pub struct DriveUi {
    pub open: bool,
    /// Waiting to start (after sign-in, or on the next frame).
    pending: Option<Upload>,
    job: Option<(String, Task<(Uploaded, usize)>)>,
    last: Option<Uploaded>,
    error: Option<String>,
}

impl DriveUi {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

/// Starts an upload, or opens the window to connect first and uploads after sign-in.
pub fn upload(app: &mut EditorApp, what: Upload) {
    if app.gdrive.job.is_some() {
        app.gdrive.open = true;
        app.notify_error("A Google Drive upload is already running");
        return;
    }
    if let Upload::Project = what {
        if app.project.media.is_empty() {
            app.notify_error("The project has no media to upload yet");
            return;
        }
    }
    let d = &mut app.gdrive;
    d.error = None;
    d.last = None;
    d.pending = Some(what);
    if !app.google.can(SCOPE_DRIVE) {
        app.gdrive.open = true;
    }
}

/// File → Upload a video to Google Drive…: pick any file, starting in the project's folder.
pub fn pick_and_upload(app: &mut EditorApp) {
    if let Some(path) = pick_video(app, "Upload a video to Google Drive") {
        upload(app, Upload::File(path));
    }
}

/// A file dialog for a video, starting next to the project.
pub fn pick_video(app: &EditorApp, title: &str) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().set_title(title).add_filter("Video", &["mp4", "mov", "m4v", "webm", "mkv", "gif"]).add_filter("Any file", &["*"]);
    if let Some(dir) = app.last_export.as_deref().and_then(|p| p.parent()).or_else(|| app.project_path.as_deref().and_then(|p| p.parent())) {
        dlg = dlg.set_directory(dir);
    }
    dlg.pick_file()
}

pub fn open_window(app: &mut EditorApp) {
    app.gdrive.open = true;
}

fn zip_name(project_name: &str) -> String {
    let clean: String = project_name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let clean = clean.trim();
    format!("{} (RustCut project).zip", if clean.is_empty() { "Untitled" } else { clean })
}

pub fn poll(ctx: &Context, app: &mut EditorApp) {
    // A sign-in that ended without Drive access drops the waiting upload.
    if app.gdrive.pending.is_some() && !app.google.signing_in() && app.google.error.is_some() && !app.google.can(SCOPE_DRIVE) {
        app.gdrive.pending = None;
    }
    // Start a waiting upload once signed in.
    let ready = app.gdrive.job.is_none() && app.gdrive.pending.is_some() && !app.google.signing_in() && app.google.can(SCOPE_DRIVE);
    if ready {
        let what = app.gdrive.pending.take().unwrap();
        let (label, task) = match what {
            Upload::File(path) => {
                let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (label, Task::spawn(ctx.clone(), move |cancel, report| Ok(gdrive::upload_file(&path, cancel, report)?.map(|u| (u, 0)))))
            }
            Upload::Project => {
                let project = app.project.clone();
                let name = zip_name(&project.name);
                (name.clone(), Task::spawn(ctx.clone(), move |cancel, report| gdrive::upload_project_zip(&project, &name, cancel, report)))
            }
        };
        app.gdrive.job = Some((label.clone(), task));
        app.notify(format!("Uploading {label} to Google Drive…"));
    }

    // Upload finished?
    if let Some(state) = app.gdrive.job.as_ref().and_then(|(_, t)| t.take_finished()) {
        let (label, _) = app.gdrive.job.take().unwrap();
        match state {
            TaskState::Done((u, missing)) => {
                if missing > 0 {
                    app.notify_error(format!("Uploaded {} to Google Drive — {missing} missing media file(s) were left out", u.name));
                } else {
                    app.notify(format!("Uploaded {} ({}) to Google Drive > {}", u.name, gdrive::size_label(u.size), gdrive::FOLDER_NAME));
                }
                app.gdrive.last = Some(u);
            }
            TaskState::Failed(e) => {
                app.notify_error(format!("Google Drive upload of {label} failed: {e}"));
                app.gdrive.error = Some(e);
                app.gdrive.open = true;
                app.google.reload();
            }
            _ => app.notify(format!("Cancelled the upload of {label}")),
        }
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.gdrive.open {
        return;
    }
    let mut open = true;
    let last_export = app.last_export.clone().filter(|p| p.is_file());
    let mut upload_req: Option<Upload> = None;
    let mut pick = false;
    egui::Window::new(RichText::new("☁ Google Drive").strong()).id(egui::Id::new("gdrive")).open(&mut open).default_width(460.0).show(ctx, |ui| {
        let (d, g) = (&mut app.gdrive, &mut app.google);

        if let Some(e) = &d.error {
            ui.colored_label(crate::ui::theme::DANGER, e);
            ui.add_space(6.0);
        }

        if let Some((label, job)) = &d.job {
            ui.label(RichText::new(format!("Uploading {label}")).strong());
            if let Some((p, stage)) = job.progress() {
                ui.add(egui::ProgressBar::new(p).show_percentage());
                ui.label(widgets::dim(stage));
            }
            if ui.button("Cancel upload").clicked() {
                job.cancel();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            ui.separator();
        } else if let Some(u) = &d.last {
            ui.label(RichText::new(format!("✅ Uploaded {} ({})", u.name, gdrive::size_label(u.size))).strong());
            ui.horizontal(|ui| {
                if ui.button("🔗 Open in Google Drive").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(&u.link));
                }
                if ui.button("📋 Copy link").clicked() {
                    ctx.copy_text(u.link.clone());
                }
            });
            ui.separator();
        }

        if !g.can(SCOPE_DRIVE) {
            if d.pending.is_some() {
                ui.label(RichText::new("Connect your Google account to start the upload.").strong());
                ui.add_space(4.0);
            }
            crate::ui::google::connect_ui(ui, ctx, g, SCOPE_DRIVE);
            return;
        }

        crate::ui::google::account_line(ui, ctx, g, SCOPE_DRIVE);
        ui.label(format!("Uploads go to the \"{}\" folder in My Drive.", gdrive::FOLDER_NAME));
        ui.add_space(6.0);
        ui.add_enabled_ui(d.job.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if let Some(path) = last_export.clone() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.button(format!("🎬 Upload {name}")).on_hover_text(path.display().to_string()).clicked() {
                        upload_req = Some(Upload::File(path));
                    }
                }
                if ui.button("🎬 Upload a video…").clicked() {
                    pick = true;
                }
                if ui.button("📦 Upload project as zip").on_hover_text("The project and all its media, packed as a zip on the way up — no local copy, no extra disk space").clicked() {
                    upload_req = Some(Upload::Project);
                }
            });
        });
    });
    if !open {
        app.gdrive.open = false;
    }
    if pick {
        pick_and_upload(app);
    }
    if let Some(u) = upload_req {
        upload(app, u);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_names() {
        assert_eq!(zip_name("Falling Deeper - lyric video"), "Falling Deeper - lyric video (RustCut project).zip");
        assert_eq!(zip_name("a/b:c"), "a_b_c (RustCut project).zip");
        assert_eq!(zip_name("  "), "Untitled (RustCut project).zip");
    }
}
