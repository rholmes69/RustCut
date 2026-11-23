//! Google Drive window (File → ☁ Google Drive…): connect a Google account once, then upload
//! exported videos and whole projects (as a zip streamed straight to Drive) into a "RustCut"
//! folder on My Drive. See `crate::gdrive`.

use crate::app::EditorApp;
use crate::gdrive::{self, Settings, Uploaded};
use crate::task::{Task, TaskState};
use crate::ui::widgets;
use egui::{Context, RichText, TextEdit};
use std::path::PathBuf;

const SETUP_HELP: &str = "One-time setup (about 5 minutes, free):\n\
1. Go to console.cloud.google.com and create a project (any name, e.g. RustCut).\n\
2. APIs & Services → Library → enable the Google Drive API.\n\
3. OAuth consent screen (Google Auth Platform): External, add your own Google address as a test user.\n\
4. Clients → Create client → Application type: Desktop app → Create, then Download JSON.\n\
5. Click Load client file… below and pick that JSON (or paste the id and secret).\n\
Tip: while the consent screen is in Testing, Google ends the sign-in after 7 days. Click Publish app on the Audience page to stay signed in; RustCut only asks for access to the files it uploads.";

#[derive(Clone)]
pub enum Upload {
    /// A file on disk, e.g. an exported video.
    File(PathBuf),
    /// The open project and its media, as a zip.
    Project,
}

pub struct DriveUi {
    pub open: bool,
    /// The last video exported in this session, offered as a one-click upload.
    pub last_export: Option<PathBuf>,
    settings: Option<Settings>,
    client_id: String,
    client_secret: String,
    show_secret: bool,
    sign_in: Option<Task<Settings>>,
    /// The consent page, shown as a link in case the browser didn't open.
    sign_in_url: std::sync::Arc<std::sync::Mutex<String>>,
    /// Waiting to start (after sign-in, or on the next frame).
    pending: Option<Upload>,
    job: Option<(String, Task<(Uploaded, usize)>)>,
    last: Option<Uploaded>,
    error: Option<String>,
}

impl Default for DriveUi {
    fn default() -> Self {
        Self {
            open: false,
            last_export: None,
            settings: None,
            client_id: String::new(),
            client_secret: String::new(),
            show_secret: false,
            sign_in: None,
            sign_in_url: Default::default(),
            pending: None,
            job: None,
            last: None,
            error: None,
        }
    }
}

impl DriveUi {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn settings(&mut self) -> &Settings {
        if self.settings.is_none() {
            let s = gdrive::load();
            self.client_id = s.client_id.clone();
            self.client_secret = s.client_secret.clone();
            self.settings = Some(s);
        }
        self.settings.as_ref().unwrap()
    }
}

/// Starts an upload, or opens the window to connect first and uploads after sign-in.
pub fn upload(app: &mut EditorApp, what: Upload) {
    let d = &mut app.gdrive;
    if d.job.is_some() {
        d.open = true;
        app.notify_error("A Google Drive upload is already running");
        return;
    }
    if let Upload::Project = what {
        if app.project.media.is_empty() {
            app.notify_error("The project has no media to upload yet");
            return;
        }
    }
    d.error = None;
    d.last = None;
    d.pending = Some(what);
    if !d.settings().connected() {
        d.open = true;
    }
}

/// File → Upload a video to Google Drive…: pick any file, starting in the project's folder.
pub fn pick_and_upload(app: &mut EditorApp) {
    let mut dlg = rfd::FileDialog::new().set_title("Upload a video to Google Drive").add_filter("Video", &["mp4", "mov", "m4v", "webm", "mkv", "gif"]).add_filter("Any file", &["*"]);
    if let Some(dir) = app.project_path.as_deref().and_then(|p| p.parent()) {
        dlg = dlg.set_directory(dir);
    }
    if let Some(path) = dlg.pick_file() {
        upload(app, Upload::File(path));
    }
}

pub fn open_window(app: &mut EditorApp) {
    app.gdrive.settings();
    app.gdrive.open = true;
}

fn zip_name(project_name: &str) -> String {
    let clean: String = project_name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let clean = clean.trim();
    format!("{} (RustCut project).zip", if clean.is_empty() { "Untitled" } else { clean })
}

pub fn poll(ctx: &Context, app: &mut EditorApp) {
    // Sign-in finished?
    if let Some(state) = app.gdrive.sign_in.as_ref().and_then(|t| t.take_finished()) {
        let d = &mut app.gdrive;
        d.sign_in = None;
        match state {
            TaskState::Done(s) => {
                d.settings = Some(s.clone());
                let who = if s.email.is_empty() { "Google Drive".to_string() } else { s.email.clone() };
                app.notify(format!("Connected to {who}"));
            }
            TaskState::Failed(e) => {
                d.error = Some(e);
                d.pending = None;
            }
            _ => d.pending = None,
        }
    }

    // Start a waiting upload once signed in.
    let ready = app.gdrive.job.is_none() && app.gdrive.sign_in.is_none() && app.gdrive.pending.is_some() && app.gdrive.settings().connected();
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
                    app.notify(format!("Uploaded {} ({}) to Google Drive → {}", u.name, gdrive::size_label(u.size), gdrive::FOLDER_NAME));
                }
                app.gdrive.last = Some(u);
            }
            TaskState::Failed(e) => {
                app.notify_error(format!("Google Drive upload of {label} failed: {e}"));
                app.gdrive.error = Some(e);
                app.gdrive.open = true;
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
    let last_export = app.gdrive.last_export.clone().filter(|p| p.is_file());
    let mut upload_req: Option<Upload> = None;
    let mut pick = false;
    egui::Window::new(RichText::new("☁ Google Drive").strong()).id(egui::Id::new("gdrive")).open(&mut open).default_width(460.0).show(ctx, |ui| {
        let d = &mut app.gdrive;
        let s = d.settings().clone();

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

        if s.connected() {
            let who = if s.email.is_empty() { "your Google account".to_string() } else { s.email.clone() };
            ui.label(format!("Connected to {who}. Uploads go to the \"{}\" folder in My Drive.", gdrive::FOLDER_NAME));
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
            ui.add_space(6.0);
            if ui.small_button("Disconnect").on_hover_text("Forget this Google account on this computer").clicked() {
                match gdrive::sign_out() {
                    Ok(s) => d.settings = Some(s),
                    Err(e) => d.error = Some(format!("{e:#}")),
                }
            }
            return;
        }

        if d.pending.is_some() {
            ui.label(RichText::new("Connect your Google account to start the upload.").strong());
            ui.add_space(4.0);
        }
        if let Some(job) = &d.sign_in {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Waiting for you to allow access in the browser…");
            });
            let url = d.sign_in_url.lock().unwrap().clone();
            if !url.is_empty() {
                ui.hyperlink_to("The browser didn't open? Click here.", url);
            }
            if ui.button("Cancel").clicked() {
                job.cancel();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }

        ui.collapsing("How to get a client id (one time)", |ui| {
            ui.label(widgets::dim(SETUP_HELP));
            ui.hyperlink_to("Open Google Cloud Console", "https://console.cloud.google.com/auth/clients");
        });
        ui.add_space(4.0);
        egui::Grid::new("gdrive-client").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Client id");
            ui.add(TextEdit::singleline(&mut d.client_id).desired_width(300.0).hint_text("…apps.googleusercontent.com"));
            ui.end_row();
            ui.label("Client secret");
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut d.client_secret).password(!d.show_secret).desired_width(240.0));
                ui.checkbox(&mut d.show_secret, "Show");
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Load client file…").on_hover_text("The JSON Google Cloud Console downloads for a Desktop app client").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("OAuth client JSON", &["json"]).pick_file() {
                    match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|j| gdrive::parse_client_file(&j)) {
                        Ok((id, secret)) => {
                            d.client_id = id;
                            d.client_secret = secret;
                            d.error = None;
                        }
                        Err(e) => d.error = Some(format!("{e:#}")),
                    }
                }
            }
            let ready = !d.client_id.trim().is_empty() && !d.client_secret.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new(RichText::new("Sign in with Google").strong())).clicked() {
                d.error = None;
                let (id, secret) = (d.client_id.trim().to_string(), d.client_secret.trim().to_string());
                let url_slot = d.sign_in_url.clone();
                url_slot.lock().unwrap().clear();
                let browser = ctx.clone();
                d.sign_in = Some(Task::spawn(ctx.clone(), move |cancel, _| {
                    let open = |url: &str| {
                        *url_slot.lock().unwrap() = url.to_string();
                        browser.open_url(egui::OpenUrl::new_tab(url));
                        browser.request_repaint();
                    };
                    gdrive::sign_in(&id, &secret, &open, cancel).map(Some)
                }));
            }
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
