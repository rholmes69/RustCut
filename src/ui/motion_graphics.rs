//! Motion graphics window (Edit → ✨ Motion graphics (HyperFrames)…): pick a template
//! (title card, lower-third name tag, lyric line), type the text, choose colour, font and
//! length, and RustCut renders it with HyperFrames to a transparent video placed at the
//! playhead above the footage. Right-click such a clip → ✏ Edit graphic… to change and
//! re-render it in place. Free and local; see `crate::motion_graphics`.

use crate::app::EditorApp;
use crate::model::Id;
use crate::motion_graphics::{self as mg, GraphicSpec, Template};
use crate::task::{Task, TaskState};
use crate::ui::widgets;
use egui::{Color32, Context, RichText, TextEdit};
use std::path::PathBuf;

enum Target {
    Add { start: f64 },
    Replace { clip: Id },
}

pub struct MotionGraphicsUi {
    pub open: bool,
    spec: GraphicSpec,
    /// Re-editing an existing graphic: (clip, its folder).
    editing: Option<(Id, PathBuf)>,
    job: Option<(Task<(PathBuf, GraphicSpec)>, Target, String)>,
}

impl Default for MotionGraphicsUi {
    fn default() -> Self {
        Self { open: false, spec: GraphicSpec::default(), editing: None, job: None }
    }
}

impl MotionGraphicsUi {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Sets the accent colour the next render uses (from the Brand tab).
    pub fn set_color(&mut self, rgb: [u8; 3]) {
        self.spec.color = rgb;
    }
}

/// Opens the window for a new graphic.
pub fn open_new(app: &mut EditorApp) {
    let g = &mut app.motion_graphics;
    if g.editing.take().is_some() {
        g.spec = GraphicSpec::default();
    }
    g.open = true;
}

/// Opens the window on the graphic clip `clip` (right-click → Edit graphic…).
pub fn open_edit(app: &mut EditorApp, clip: Id) {
    let folder = app.project.clip(clip).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).and_then(|m| mg::folder_of(&m.path));
    let Some(folder) = folder else {
        app.notify_error("This clip wasn't made in the Motion graphics window, so it can't be edited there");
        return;
    };
    match mg::load_spec(&folder) {
        Ok(spec) => {
            let g = &mut app.motion_graphics;
            g.spec = spec;
            g.editing = Some((clip, folder));
            g.open = true;
        }
        Err(e) => app.notify_error(format!("Couldn't read the graphic's settings: {e:#}")),
    }
}

/// Whether a clip is a graphic made here (shows "Edit graphic…" in its menu).
pub fn is_graphic(app: &EditorApp, clip: Id) -> bool {
    app.project.clip(clip).and_then(|c| c.media_id()).and_then(|m| app.project.media(m)).is_some_and(|m| mg::folder_of(&m.path).is_some())
}

pub fn poll(app: &mut EditorApp) {
    let Some(state) = app.motion_graphics.job.as_ref().and_then(|(t, _, _)| t.take_finished()) else { return };
    let Some((_, target, label)) = app.motion_graphics.job.take() else { return };
    match state {
        TaskState::Done((path, spec)) => {
            let result = match target {
                Target::Add { start } => mg::add_to_project(&mut app.project, &path, start).map(|(media, clip)| {
                    // Further edits in the window now update this clip instead of adding another.
                    if let Some(dir) = path.parent() {
                        app.motion_graphics.editing = Some((clip, dir.to_path_buf()));
                    }
                    (media, clip, "added above the footage")
                }),
                Target::Replace { clip } => mg::replace_in_project(&mut app.project, clip, &path).map(|media| (media, clip, "updated")),
            };
            match result {
                Ok((media, clip, what)) => {
                    app.motion_graphics.spec = spec;
                    if let Some(item) = app.project.media(media).cloned() {
                        app.assets.ensure(&item);
                    }
                    app.selection = Some(clip);
                    app.notify(format!("✨ {label} {what}"));
                }
                Err(e) => app.notify_error(format!("{label}: rendered, but couldn't add it: {e:#}")),
            }
        }
        TaskState::Failed(e) => app.notify_error(format!("{label} failed: {e}")),
        TaskState::Cancelled | TaskState::Running { .. } => app.notify(format!("Stopped rendering {label}")),
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.motion_graphics.open {
        return;
    }
    let mut open = true;
    let mut render = false;
    let mut cancel = false;
    let mut new_one = false;
    let editing_clip = app.motion_graphics.editing.as_ref().map(|(c, _)| *c).filter(|&c| app.project.clip(c).is_some());
    let playhead = app.playhead;
    let brand_colors: Vec<(String, [u8; 3])> = app
        .brand_kit
        .kit
        .as_ref()
        .map(|k| k.info.colors.iter().filter_map(|c| Some((format!("{} (brand)", c.name), c.rgb()?))).collect())
        .unwrap_or_default();
    let title = if editing_clip.is_some() { "✨ Edit motion graphic" } else { "✨ Motion graphics (HyperFrames)" };
    egui::Window::new(RichText::new(title).strong()).id(egui::Id::new("motion-graphics")).open(&mut open).default_width(420.0).show(ctx, |ui| {
        let g = &mut app.motion_graphics;
        let busy = g.job.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("mg-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Template");
                let before = g.spec.template;
                egui::ComboBox::from_id_salt("mg-template").width(240.0).selected_text(before.label()).show_ui(ui, |ui| {
                    for t in Template::ALL {
                        ui.selectable_value(&mut g.spec.template, t, t.label());
                    }
                });
                if g.spec.template != before {
                    // Follow the new template's defaults unless the user changed them.
                    if g.spec.font == before.default_font() {
                        g.spec.font = g.spec.template.default_font().into();
                    }
                    if (g.spec.duration - before.default_duration()).abs() < 1e-6 {
                        g.spec.duration = g.spec.template.default_duration();
                    }
                }
                ui.end_row();

                let (l1, l2) = g.spec.template.field_labels();
                ui.label(l1);
                ui.add(TextEdit::singleline(&mut g.spec.headline).desired_width(260.0).hint_text(match g.spec.template {
                    Template::TitleCard => "BOND MONEY",
                    Template::LowerThird => "Pastor Holmes",
                    Template::LyricLine => "I been saving bond money",
                }));
                ui.end_row();
                ui.label(l2);
                ui.add(TextEdit::singleline(&mut g.spec.sub).desired_width(260.0).hint_text("optional"));
                ui.end_row();

                ui.label("Font");
                egui::ComboBox::from_id_salt("mg-font").width(240.0).selected_text(&g.spec.font).show_ui(ui, |ui| {
                    for f in mg::FONTS {
                        ui.selectable_value(&mut g.spec.font, f.to_string(), *f);
                    }
                });
                ui.end_row();
                ui.label("Accent colour");
                ui.horizontal(|ui| {
                    ui.color_edit_button_srgb(&mut g.spec.color);
                    let builtin = [("Gold", [232, 181, 74]), ("White", [245, 245, 245]), ("Rose", [236, 120, 160]), ("Cyan", [0, 205, 222])]
                        .map(|(n, c)| (n.to_string(), c));
                    // The brand kit's colours first, else the built-in swatches.
                    let swatches = if brand_colors.is_empty() { builtin.to_vec() } else { brand_colors.clone() };
                    for (name, c) in swatches {
                        let (r, gg, b) = (c[0], c[1], c[2]);
                        if ui.add(egui::Button::new(RichText::new("■").color(Color32::from_rgb(r, gg, b)))).on_hover_text(name).clicked() {
                            g.spec.color = c;
                        }
                    }
                });
                ui.end_row();
                ui.label("Length");
                ui.add(egui::Slider::new(&mut g.spec.duration, 1.0..=15.0).step_by(0.5).suffix(" s"));
                ui.end_row();
            });
        });
        ui.add_space(6.0);
        match editing_clip {
            Some(_) => {
                ui.label(widgets::dim("Re-renders this graphic and swaps it into its clip (same place on the timeline).").small());
            }
            None => {
                ui.label(widgets::dim(format!("Placed at the playhead ({}) on a video track above the footage.", widgets::format_short(playhead))).small());
            }
        }
        ui.label(widgets::dim("Rendered on your computer with HyperFrames: free, ~20–40 s. The first render downloads HyperFrames (needs Node.js). Needs 1 GB free disk.").small());
        ui.add_space(6.0);
        match g.job.as_ref().and_then(|(t, _, _)| t.progress()) {
            Some((p, stage)) => {
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(p).desired_width(260.0).text(stage));
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            None => {
                ui.horizontal(|ui| {
                    let ok = !g.spec.headline.trim().is_empty();
                    let label = if editing_clip.is_some() { "✨ Render & update clip" } else { "✨ Render & add at playhead" };
                    if ui.add_enabled(ok, egui::Button::new(RichText::new(label).strong())).clicked() {
                        render = true;
                    }
                    if editing_clip.is_some() && ui.button("New graphic").on_hover_text("Keep these settings but add a separate graphic").clicked() {
                        new_one = true;
                    }
                });
            }
        }
    });
    app.motion_graphics.open = open;
    if cancel {
        if let Some((t, _, _)) = &app.motion_graphics.job {
            t.cancel();
        }
    }
    if new_one {
        app.motion_graphics.editing = None;
    }
    if render {
        start_render(ctx, app, editing_clip);
    }
}

fn start_render(ctx: &Context, app: &mut EditorApp, editing_clip: Option<Id>) {
    let spec = app.motion_graphics.spec.clone().for_aspect(app.project.aspect);
    let (dir, target) = match (editing_clip, app.motion_graphics.editing.clone()) {
        (Some(clip), Some((_, dir))) => (dir, Target::Replace { clip }),
        _ => (mg::new_folder(&crate::ai::video_gen::output_dir(app.project_path.as_deref()), &spec), Target::Add { start: app.playhead }),
    };
    let label = format!("{} “{}”", spec.template.label(), spec.display_name());
    let task = Task::spawn(ctx.clone(), move |cancel, report| {
        let mut spec = spec;
        Ok(mg::render(&dir, &mut spec, cancel, report)?.map(|path| (path, spec)))
    });
    app.motion_graphics.job = Some((task, target, label));
}
