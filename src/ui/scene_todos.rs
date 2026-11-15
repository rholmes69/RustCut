//! Scene to-do list: the new scenes still to make for this video, with notes, a status
//! (to do / in progress / done) and an optional spot on the timeline. Saved inside the
//! project's .rcproj JSON; "Export list…" / "Import list…" read and write it as its own
//! .json file. Edit → 📝 Scene to-do list…

use crate::app::EditorApp;
use crate::model::{Id, SceneStatus};
use crate::ui::{theme, widgets};
use egui::{Color32, Context, RichText, TextEdit};

#[derive(Default)]
pub struct SceneTodosUi {
    pub open: bool,
    new_title: String,
    new_notes: String,
    /// Pin the new scene to the playhead position.
    at_playhead: bool,
    hide_done: bool,
    /// Scene whose notes are expanded for editing.
    expanded: Option<Id>,
}

fn status_color(s: SceneStatus) -> Color32 {
    match s {
        SceneStatus::Todo => theme::TEXT_DIM,
        SceneStatus::InProgress => Color32::from_rgb(255, 184, 48),
        SceneStatus::Done => Color32::from_rgb(90, 200, 120),
    }
}

enum Action {
    Delete(Id),
    Move(Id, i32),
    Goto(f64),
    PinHere(Id),
    Unpin(Id),
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.scene_todos.open {
        return;
    }
    let mut open = true;
    let mut action = None;
    let mut add = false;
    let (mut export, mut import) = (false, false);
    egui::Window::new(RichText::new("📝 Scene to-do list").strong())
        .id(egui::Id::new("scene-todos"))
        .open(&mut open)
        .default_width(460.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            let (done, total) = app.project.scene_todo_progress();
            ui.horizontal(|ui| {
                if total == 0 {
                    ui.label(widgets::dim("New scenes you still want to make for this video. Saved with the project."));
                } else {
                    ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(160.0).text(format!("{done} of {total} done")));
                    ui.checkbox(&mut app.scene_todos.hide_done, "Hide done");
                }
            });
            ui.add_space(4.0);

            // ── add a scene ──
            let s = &mut app.scene_todos;
            let title = ui.add(TextEdit::singleline(&mut s.new_title).hint_text("New scene, e.g. Choir solo in the rain").desired_width(f32::INFINITY));
            let enter = title.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add(TextEdit::multiline(&mut s.new_notes).hint_text("Notes: shot, cast, outfit, lyric line… (optional)").desired_rows(2).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.checkbox(&mut s.at_playhead, format!("At playhead ({})", widgets::format_short(app.playhead)));
                if widgets::accent_button(ui, "➕ Add scene").clicked() || (enter && !s.new_title.trim().is_empty()) {
                    add = true;
                }
            });
            ui.separator();

            // ── the list ──
            egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 34.0).show(ui, |ui| {
                let count = app.project.scene_todos.len();
                let hide_done = app.scene_todos.hide_done;
                let mut expanded = app.scene_todos.expanded;
                for (i, scene) in app.project.scene_todos.iter_mut().enumerate() {
                    if hide_done && scene.status == SceneStatus::Done {
                        continue;
                    }
                    let id = scene.id;
                    egui::Frame::none().fill(theme::PANEL_ALT).rounding(6.0).inner_margin(6.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let mut checked = scene.status == SceneStatus::Done;
                            if ui.checkbox(&mut checked, "").on_hover_text("Done").changed() {
                                scene.status = if checked { SceneStatus::Done } else { SceneStatus::Todo };
                            }
                            ui.label(widgets::dim(format!("{}.", i + 1)));
                            let edit = TextEdit::singleline(&mut scene.title).desired_width(ui.available_width() - 150.0);
                            ui.add(if scene.status == SceneStatus::Done { edit.text_color(theme::TEXT_DIM) } else { edit });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                                    action = Some(Action::Delete(id));
                                }
                                if ui.add_enabled(i + 1 < count, egui::Button::new("⬇").small()).on_hover_text("Move down").clicked() {
                                    action = Some(Action::Move(id, 1));
                                }
                                if ui.add_enabled(i > 0, egui::Button::new("⬆").small()).on_hover_text("Move up").clicked() {
                                    action = Some(Action::Move(id, -1));
                                }
                                let open_notes = expanded == Some(id);
                                let notes_icon = if scene.notes.is_empty() { "🗒" } else { "📄" };
                                if ui.selectable_label(open_notes, notes_icon).on_hover_text("Notes").clicked() {
                                    expanded = if open_notes { None } else { Some(id) };
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("scene-status", id))
                                .width(110.0)
                                .selected_text(RichText::new(scene.status.label()).color(status_color(scene.status)))
                                .show_ui(ui, |ui| {
                                    for st in SceneStatus::ALL {
                                        ui.selectable_value(&mut scene.status, st, RichText::new(st.label()).color(status_color(st)));
                                    }
                                });
                            match scene.at {
                                Some(t) => {
                                    if ui.small_button(format!("⏵ {}", widgets::format_short(t))).on_hover_text("Go to this spot on the timeline").clicked() {
                                        action = Some(Action::Goto(t));
                                    }
                                    if ui.small_button("✖").on_hover_text("Unpin from the timeline").clicked() {
                                        action = Some(Action::Unpin(id));
                                    }
                                }
                                None => {
                                    if ui.small_button("📍 Pin to playhead").clicked() {
                                        action = Some(Action::PinHere(id));
                                    }
                                }
                            }
                            if expanded != Some(id) && !scene.notes.is_empty() {
                                let first = scene.notes.lines().next().unwrap_or_default();
                                ui.label(widgets::dim(first).small()).on_hover_text(&scene.notes);
                            }
                        });
                        if expanded == Some(id) {
                            ui.add(TextEdit::multiline(&mut scene.notes).hint_text("Notes").desired_rows(3).desired_width(f32::INFINITY));
                        }
                    });
                    ui.add_space(3.0);
                }
                app.scene_todos.expanded = expanded;
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(total > 0, egui::Button::new("💾 Export list…")).on_hover_text("Save the list as a .json file").clicked() {
                    export = true;
                }
                if ui.button("📂 Import list…").on_hover_text("Add the scenes from a .json file").clicked() {
                    import = true;
                }
            });
        });
    app.scene_todos.open = open;

    if add {
        let s = &mut app.scene_todos;
        let at = s.at_playhead.then_some(app.playhead);
        let (title, notes) = (std::mem::take(&mut s.new_title), std::mem::take(&mut s.new_notes));
        app.project.add_scene_todo(title, notes, at);
    }
    match action {
        Some(Action::Delete(id)) => {
            if let Some(s) = app.project.remove_scene_todo(id) {
                app.notify(format!("Deleted “{}” — Ctrl+Z brings it back", s.title));
            }
        }
        Some(Action::Move(id, d)) => {
            app.project.move_scene_todo(id, d);
        }
        Some(Action::Goto(t)) => {
            app.seek(t);
            app.timeline.reveal = Some(t);
        }
        Some(Action::PinHere(id)) => {
            let t = app.playhead;
            if let Some(s) = app.project.scene_todo_mut(id) {
                s.at = Some(t);
            }
        }
        Some(Action::Unpin(id)) => {
            if let Some(s) = app.project.scene_todo_mut(id) {
                s.at = None;
            }
        }
        None => {}
    }
    if export {
        let name = format!("{} - scene todos.json", app.project.name);
        let mut dialog = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name(name);
        if let Some(dir) = app.project_path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.save_file() {
            match app.project.export_scene_todos(&path) {
                Ok(()) => app.notify(format!("Saved the scene list to {}", path.display())),
                Err(e) => app.notify_error(format!("Couldn't save the scene list: {e:#}")),
            }
        }
    }
    if import {
        let mut dialog = rfd::FileDialog::new().add_filter("JSON", &["json"]);
        if let Some(dir) = app.project_path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            match app.project.import_scene_todos(&path) {
                Ok(n) => app.notify(format!("Added {n} scenes from {}", path.display())),
                Err(e) => app.notify_error(format!("Couldn't import the scene list: {e:#}")),
            }
        }
    }
}
