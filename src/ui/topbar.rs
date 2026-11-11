//! Top bar: brand, file menu, project name, undo/redo, canvas ratio, export.

use crate::app::EditorApp;
use crate::model::Aspect;
use crate::ui::{theme, widgets};
use egui::{Align, Context, Frame, Layout, Margin, RichText, TextEdit};

pub fn show(ctx: &Context, app: &mut EditorApp) {
    egui::TopBottomPanel::top("topbar")
        .exact_height(46.0)
        .frame(Frame::none().fill(theme::BG).inner_margin(Margin::symmetric(12.0, 6.0)))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("✂").size(18.0).color(theme::ACCENT));
                ui.label(RichText::new("RustCut").size(17.0).strong().color(theme::TEXT));
                ui.add_space(10.0);

                ui.menu_button("File", |ui| {
                    ui.set_min_width(220.0);
                    if ui.button("New project").clicked() {
                        app.new_project();
                        ui.close_menu();
                    }
                    if ui.button("Open project…            Ctrl+O").clicked() {
                        app.open_dialog();
                        ui.close_menu();
                    }
                    if ui.button("💲 Cost report (RustCut vs commercial tools)…").clicked() {
                        app.economics.open = true;
                        ui.close_menu();
                    }
                    let mut open_recent: Option<std::path::PathBuf> = None;
                    let mut clear_recent = false;
                    ui.menu_button("Open recent", |ui| {
                        ui.set_min_width(260.0);
                        if app.recent.is_empty() {
                            ui.label(widgets::dim("No recent projects yet"));
                        }
                        for (i, path) in app.recent.iter().enumerate() {
                            let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let exists = path.exists();
                            let label = if exists { format!("{}.  {name}", i + 1) } else { format!("{}.  {name}  (missing)", i + 1) };
                            let resp = ui.add(egui::Button::new(if exists { RichText::new(label) } else { widgets::dim(label) }));
                            if resp.on_hover_text(path.display().to_string()).clicked() {
                                open_recent = Some(path.clone());
                                ui.close_menu();
                            }
                        }
                        if !app.recent.is_empty() {
                            ui.separator();
                            if ui.button("Clear list").clicked() {
                                clear_recent = true;
                                ui.close_menu();
                            }
                        }
                    });
                    if let Some(path) = open_recent {
                        if path.exists() {
                            app.open_path(&path);
                        } else {
                            crate::recent::remove(&mut app.recent, &path);
                            app.notify_error(format!("{} no longer exists — removed from the recent list", path.display()));
                        }
                    }
                    if clear_recent {
                        crate::recent::clear(&mut app.recent);
                    }
                    if ui.button("Save                          Ctrl+S").clicked() {
                        app.save(false);
                        ui.close_menu();
                    }
                    if ui.button("Save as…          Ctrl+Shift+S").clicked() {
                        app.save(true);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Import media…           Ctrl+I").clicked() {
                        app.import_dialog();
                        ui.close_menu();
                    }
                    if ui.button("📦 Batch export (YouTube, Shorts, square…)…").clicked() {
                        crate::ui::export_queue::show(app);
                        ui.close_menu();
                    }
                    if ui.button("Export video…            Ctrl+E").clicked() {
                        app.open_export();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    ui.set_min_width(220.0);
                    if ui.button("⏱ Go to time…            Ctrl+G").on_hover_text("Type a time like 2:38.5 to move the playhead there").clicked() {
                        app.open_goto();
                        ui.close_menu();
                    }
                    if ui.button("🎞 Timeline storyboard…").clicked() {
                        app.clip_board.open = true;
                        ui.close_menu();
                    }
                    if ui.button("📊 Scopes (histogram, waveform, vectorscope)…").clicked() {
                        app.color.scopes_open = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.add_enabled(app.history.can_undo(), egui::Button::new("Undo                  Ctrl+Z")).clicked() {
                        app.undo();
                        ui.close_menu();
                    }
                    if ui.add_enabled(app.history.can_redo(), egui::Button::new("Redo         Ctrl+Shift+Z")).clicked() {
                        app.redo();
                        ui.close_menu();
                    }
                    ui.separator();
                    let has_sel = app.selection.is_some();
                    if ui.button("Split at playhead     Ctrl+B").clicked() {
                        app.split_at_playhead();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Duplicate               Ctrl+D")).clicked() {
                        app.duplicate_selection();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Delete                     Del")).clicked() {
                        app.delete_selection();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Ripple delete       Shift+Del")).on_hover_text("Delete and close the gap on the track").clicked() {
                        app.ripple_delete_selection();
                        ui.close_menu();
                    }
                    if ui.button("🧊 Freeze frame at playhead (2 s)").clicked() {
                        if let Err(e) = app.freeze_frame(None, 2.0) {
                            app.notify_error(e);
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.menu_button("⚡ Preview proxies", |ui| {
                        ui.set_min_width(260.0);
                        let (have, need) = crate::ui::retime::proxy_counts(app);
                        let mut on = crate::media::proxy::enabled();
                        if ui.checkbox(&mut on, "Use proxies for the preview").on_hover_text("Smaller copies of big videos for smooth scrubbing. Exports always use the originals.").changed() {
                            crate::media::proxy::set_enabled(on);
                        }
                        ui.label(crate::ui::widgets::dim(if need == 0 { "No videos above 720p in this project".to_string() } else { format!("{have} of {need} videos above 720p have a proxy") }).small());
                        if ui.add_enabled(have < need && !app.retime.making_proxies(), egui::Button::new("Make proxies")).clicked() {
                            let ctx = ui.ctx().clone();
                            crate::ui::retime::start_proxies(app, &ctx);
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.checkbox(&mut app.retime.smooth, "Smooth slow motion in speed ramps (optical flow, slower)");
                    });
                    ui.menu_button("🚩 Markers", |ui| {
                        ui.set_min_width(230.0);
                        if ui.button("Add marker at playhead          M").clicked() {
                            app.add_marker_at_playhead();
                            ui.close_menu();
                        }
                        if ui.button("Previous marker           Alt+Left").clicked() {
                            app.jump_marker(-1);
                            ui.close_menu();
                        }
                        if ui.button("Next marker                Alt+Right").clicked() {
                            app.jump_marker(1);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("🎵 Markers from song sections").clicked() {
                            app.markers_from_sections();
                            ui.close_menu();
                        }
                        if ui.add_enabled(!app.project.markers.is_empty(), egui::Button::new("📋 Copy YouTube chapters")).clicked() {
                            let ctx = ui.ctx().clone();
                            app.copy_chapters(&ctx);
                            ui.close_menu();
                        }
                        if ui.add_enabled(!app.project.markers.is_empty(), egui::Button::new("Clear all markers")).clicked() {
                            app.project.markers.clear();
                            ui.close_menu();
                        }
                    });
                });
                ui.menu_button("Music", |ui| crate::ui::music::menu(ui, app));
                ui.menu_button("AI", |ui| {
                    ui.set_min_width(240.0);
                    if ui.button(RichText::new("✨ Copilot — type or speak to edit       Ctrl+Space").strong()).clicked() {
                        crate::ui::copilot::show(app);
                        ui.close_menu();
                    }
                    if ui.button("🔍 Continuity check (outfit, hair, face between shots)…").clicked() {
                        let ctx = ui.ctx().clone();
                        crate::ui::review::open_continuity(app, &ctx);
                        ui.close_menu();
                    }
                    if ui.button("🎬 Teaser / trailer from the best part…").clicked() {
                        app.review.teaser_open = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(RichText::new("🪄 Make a video from a prompt (storyboard)…").strong()).clicked() {
                        app.storyboard.open = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("✂ Rough cut by prompt…").clicked() {
                        app.rough_cut.open = true;
                        ui.close_menu();
                    }
                    if ui.button("🗣 Dialogue & voices (ElevenLabs)…").clicked() {
                        app.voice.open = true;
                        ui.close_menu();
                    }
                    if ui.button("🔀 Smart transitions (local AI, free)…").clicked() {
                        app.local.transitions_open = true;
                        ui.close_menu();
                    }
                    if ui.button("🎬 Generate video (B-roll)…").clicked() {
                        app.video_gen.open = true;
                        ui.close_menu();
                    }
                    if ui.button("🖼 Generate image (thumbnail / title card / sticker)…").clicked() {
                        app.image_gen.open = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("✨ Auto-reframe all clips…").clicked() {
                        app.ai.reframe_all_open = true;
                        ui.close_menu();
                    }
                    let sel = app.selection.filter(|id| crate::ui::ai::reframable(app, *id));
                    if ui.add_enabled(sel.is_some() && !app.ai.busy(), egui::Button::new("✨ Auto-reframe selected clip")).clicked() {
                        if let Some(id) = sel {
                            let ctx = ui.ctx().clone();
                            crate::ui::ai::start_reframe(app, &ctx, vec![id]);
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("🔑 OpenRouter settings…").clicked() {
                        let ctx = ui.ctx().clone();
                        app.ai.open_settings(&ctx);
                        ui.close_menu();
                    }
                });

                ui.add_space(12.0);
                ui.add(
                    TextEdit::singleline(&mut app.project.name)
                        .desired_width(220.0)
                        .hint_text("Project name"),
                );
                if app.is_dirty() {
                    ui.label(widgets::dim("● unsaved")).on_hover_text("Ctrl+S to save");
                }

                ui.add_space(12.0);
                if ui
                    .add_enabled(app.history.can_undo(), egui::Button::new("↩").frame(false))
                    .on_hover_text("Undo (Ctrl+Z)")
                    .clicked()
                {
                    app.undo();
                }
                if ui
                    .add_enabled(app.history.can_redo(), egui::Button::new("↪").frame(false))
                    .on_hover_text("Redo (Ctrl+Shift+Z)")
                    .clicked()
                {
                    app.redo();
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::accent_button(ui, "⬆  Export").on_hover_text("Export video (Ctrl+E)").clicked() {
                        app.open_export();
                    }
                    ui.add_space(8.0);
                    if ui.button("✨ Copilot").on_hover_text("Type or speak to edit (Ctrl+Space talks)").clicked() {
                        if app.copilot.open {
                            app.copilot.open = false;
                        } else {
                            crate::ui::copilot::show(app);
                        }
                    }
                    ui.add_space(8.0);
                    egui::ComboBox::from_id_salt("aspect")
                        .selected_text(format!("{}", app.project.aspect.short_label()))
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for a in Aspect::ALL {
                                ui.selectable_value(&mut app.project.aspect, a, a.label());
                            }
                        })
                        .response
                        .on_hover_text("Canvas ratio");
                });
            });
        });
}
