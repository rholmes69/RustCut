//! File → ☁ Download project from Google Drive…: paste a share link to a project .zip
//! (made with File → Export project as zip), pick a folder, and it downloads, unpacks and opens.

use crate::app::EditorApp;
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Context, RichText};
use std::path::PathBuf;

#[derive(Default)]
pub struct GDriveUi {
    pub open: bool,
    link: String,
    task: Option<Task<PathBuf>>,
}

pub fn poll(app: &mut EditorApp) {
    let Some(task) = &app.gdrive.task else { return };
    let Some(state) = task.take_finished() else { return };
    app.gdrive.task = None;
    match state {
        TaskState::Done(path) => {
            app.gdrive.open = false;
            app.open_path(&path);
        }
        TaskState::Failed(e) => app.notify_error(format!("Drive download failed: {e}")),
        _ => app.notify("Drive download cancelled"),
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.gdrive.open {
        return;
    }
    let mut open = true;
    let mut start: Option<(String, PathBuf)> = None;
    egui::Window::new(RichText::new("☁ Download project from Google Drive").strong())
        .id(egui::Id::new("gdrive-download"))
        .open(&mut open)
        .default_width(460.0)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label(widgets::dim("Paste the share link of a RustCut project .zip. The file must be shared as \"Anyone with the link\"."));
            ui.add_space(4.0);
            if let Some((p, stage)) = app.gdrive.task.as_ref().and_then(|t| t.progress()) {
                ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
                if ui.button("Cancel").clicked() {
                    if let Some(t) = &app.gdrive.task {
                        t.cancel();
                    }
                }
                return;
            }
            ui.add(egui::TextEdit::singleline(&mut app.gdrive.link).desired_width(f32::INFINITY).hint_text("https://drive.google.com/file/d/…/view"));
            let id = crate::gdrive::file_id(&app.gdrive.link);
            if !app.gdrive.link.trim().is_empty() && id.is_none() {
                ui.label(RichText::new("That doesn't look like a Google Drive file link").color(theme::DANGER));
            }
            ui.add_space(4.0);
            if ui.add_enabled(id.is_some(), egui::Button::new("Choose folder and download…")).clicked() {
                if let (Some(id), Some(parent)) = (id, rfd::FileDialog::new().set_title("Folder to unpack the project into").pick_folder()) {
                    start = Some((id, parent));
                }
            }
        });
    if !open {
        if let Some(t) = &app.gdrive.task {
            t.cancel();
        }
        app.gdrive.open = false;
    }
    if let Some((id, parent)) = start {
        let mut dest = parent.join(format!("drive-{}", &id[..8]));
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("drive-{} ({n})", &id[..8]));
            n += 1;
        }
        app.gdrive.task = Some(Task::spawn(ctx.clone(), move |cancel, progress| {
            std::fs::create_dir_all(&parent)?;
            let zip = parent.join(format!(".drive-{}.zip.part", &id[..8]));
            let got = crate::gdrive::download(&id, &zip, cancel, progress);
            let result = match got {
                Ok(true) => {
                    progress(1.0, "Unpacking…");
                    crate::package::import_zip(&zip, &dest).map(Some)
                }
                Ok(false) => Ok(None),
                Err(e) => Err(e),
            };
            let _ = std::fs::remove_file(&zip);
            result
        }));
    }
}
