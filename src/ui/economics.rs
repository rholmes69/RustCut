//! "Cost report" window: RustCut's actual AI spend vs commercial tool stacks, per clip
//! and per audio track, from the economics database.

use crate::app::EditorApp;
use crate::economics::{self, Cost, Report};
use crate::ui::{theme, widgets};
use egui::{Align2, Color32, Context, Grid, RichText, ScrollArea};

#[derive(Default)]
pub struct EconomicsUi {
    pub open: bool,
    report: Option<Result<Report, String>>,
    videos_per_month: f64,
    expanded: std::collections::BTreeSet<usize>,
}

fn money(v: f64) -> String {
    if v == 0.0 { "$0".into() } else if v < 0.01 { "<$0.01".into() } else { format!("${v:.2}") }
}

fn refresh(app: &mut EditorApp) {
    let r = economics::open().and_then(|c| {
        app.economics.videos_per_month = economics::videos_per_month(&c);
        economics::build_report(&c, &app.project, app.project_path.as_deref())
    });
    app.economics.report = Some(r.map_err(|e| format!("{e:#}")));
}

/// Storyboard folders used by this project whose spend predates the ledger.
fn unrecorded_storyboards(app: &EditorApp) -> Vec<std::path::PathBuf> {
    let Ok(conn) = economics::open() else { return Vec::new() };
    let mut dirs: Vec<std::path::PathBuf> = app.project.media.iter().filter_map(|m| m.path.parent().map(|p| p.to_path_buf())).filter(|d| d.join("storyboard.json").is_file()).collect();
    dirs.sort();
    dirs.dedup();
    dirs.into_iter()
        .filter(|d| {
            let key = economics::norm(&d.to_string_lossy());
            conn.query_row("SELECT COUNT(*) FROM spend WHERE output_path LIKE ?1 || '%'", [key], |r| r.get::<_, i64>(0)).unwrap_or(0) == 0
        })
        .collect()
}

fn save_as(app: &mut EditorApp, ext: &str, body: String) {
    let stem = app.project_path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "RustCut".into());
    let dir = app.project_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| crate::ai::video_gen::output_dir(None));
    if let Some(path) = rfd::FileDialog::new().set_directory(&dir).set_file_name(format!("{stem} cost report.{ext}")).add_filter(ext, &[ext]).save_file() {
        match std::fs::write(&path, body) {
            Ok(()) => {
                app.notify(format!("Saved {}", path.display()));
                if ext == "html" {
                    let _ = std::process::Command::new("explorer").arg(&path).spawn();
                }
            }
            Err(e) => app.notify_error(format!("Could not save: {e}")),
        }
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.economics.open {
        return;
    }
    if app.economics.report.is_none() {
        refresh(app);
    }
    let mut open = true;
    let mut do_refresh = false;
    let mut export: Option<&'static str> = None;
    let mut backfill = false;
    let screen = ctx.screen_rect();
    egui::Window::new(RichText::new("Cost report: RustCut vs commercial tools").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([screen.width().min(1180.0) - 40.0, screen.height().min(820.0) - 80.0])
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let report = match &app.economics.report {
                Some(Ok(r)) => r.clone(),
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("Could not build the report: {e}")).color(theme::DANGER));
                    return;
                }
                None => return,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::dim(format!("{} · prices as of {}", report.project, economics::PRICES_AS_OF)));
            });
            ui.horizontal(|ui| {
                ui.label("Subscriptions spread over");
                let r = ui.add(egui::DragValue::new(&mut app.economics.videos_per_month).range(1.0..=100.0).speed(0.2).suffix(" videos / month"));
                if r.changed() {
                    if let Ok(c) = economics::open() {
                        let _ = economics::set_videos_per_month(&c, app.economics.videos_per_month);
                    }
                    do_refresh = true;
                }
                if ui.button("↻ Refresh").clicked() {
                    do_refresh = true;
                }
                if ui.button("💾 Save HTML report…").clicked() {
                    export = Some("html");
                }
                if ui.button("💾 Save CSV…").clicked() {
                    export = Some("csv");
                }
                if ui.button("📂 Prices database").on_hover_text("Edit prices in economics.sqlite (e.g. with DB Browser for SQLite)").clicked() {
                    crate::ui::library::reveal(&economics::db_path());
                }
            });
            let missing = unrecorded_storyboards(app);
            if !missing.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("⚠ {} storyboard(s) in this project were paid for before cost tracking.", missing.len())).color(theme::DANGER));
                    if ui.button("Add estimated costs").on_hover_text("Spreads each storyboard's recorded total over its frames and shot videos (marked as estimates)").clicked() {
                        backfill = true;
                    }
                });
            }
            if report.estimated {
                ui.label(widgets::dim("Some RustCut amounts are estimates (spent before cost tracking).").small());
            }
            ui.add_space(4.0);
            // Summary cards.
            ui.horizontal_wrapped(|ui| {
                let card = |ui: &mut egui::Ui, title: &str, value: f64, sub: String, accent: Color32, note: Option<(String, Color32)>| {
                    egui::Frame::group(ui.style()).fill(theme::PANEL_ALT).show(ui, |ui| {
                        ui.set_width(200.0);
                        ui.label(widgets::dim(title));
                        ui.label(RichText::new(money(value)).size(22.0).strong().color(accent));
                        ui.label(widgets::dim(sub).small());
                        if let Some((t, c)) = note {
                            ui.label(RichText::new(t).color(c).small().strong());
                        }
                    });
                };
                card(ui, "RustCut (actual AI spend)", report.rustcut_total(), "no subscription".into(), theme::ACCENT, None);
                for (k, s) in report.stacks.iter().enumerate() {
                    let t = report.stack_total(k);
                    let diff = t - report.rustcut_total();
                    let note = if diff >= 0.0 { (format!("RustCut saves {}", money(diff)), Color32::from_rgb(95, 208, 143)) } else { (format!("RustCut costs {} more", money(-diff)), theme::DANGER) };
                    let sub = if s.monthly() > 0.0 { format!("${:.2}/mo subscriptions", s.monthly()) } else { "pay as you go".into() };
                    card(ui, &s.name, t, sub, theme::TEXT, Some(note));
                }
            });
            ui.separator();
            // Clip and audio table.
            let n = report.stacks.len();
            ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                Grid::new("cost-grid").striped(true).num_columns(5 + n).spacing([14.0, 4.0]).show(ui, |ui| {
                    for h in ["", "Time", "Clip / task", "Qty", "RustCut"] {
                        ui.label(RichText::new(h).small().strong());
                    }
                    for s in &report.stacks {
                        ui.label(RichText::new(&s.name).small().strong());
                    }
                    ui.end_row();
                    for (ri, row) in report.rows.iter().enumerate() {
                        let open_row = app.economics.expanded.contains(&ri);
                        if ui.small_button(if open_row { "▾" } else { "▸" }).clicked() {
                            if open_row {
                                app.economics.expanded.remove(&ri);
                            } else {
                                app.economics.expanded.insert(ri);
                            }
                        }
                        ui.label(widgets::format_short(row.start));
                        let icon = match row.scope.as_str() { "Audio" => "🎵", "Project" => "📁", _ => "🎬" };
                        ui.label(RichText::new(format!("{icon} {}", row.label)).strong()).on_hover_text(&row.track);
                        ui.label(widgets::dim(format!("{:.1} s", row.duration)));
                        ui.label(RichText::new(money(row.rustcut())).color(theme::ACCENT).strong());
                        for k in 0..n {
                            ui.label(money(row.stack(k)));
                        }
                        ui.end_row();
                        if open_row {
                            for it in &row.items {
                                ui.label("");
                                ui.label("");
                                let task = if it.rustcut_note.is_empty() { it.task.clone() } else { format!("{} ({})", it.task, it.rustcut_note) };
                                ui.label(widgets::dim(format!("   {task}")));
                                ui.label(widgets::dim(format!("{:.2} {}", it.qty, it.unit)));
                                ui.label(RichText::new(money(it.rustcut)).color(theme::ACCENT));
                                for c in &it.stacks {
                                    match c {
                                        Cost::Usd(v, label) => {
                                            ui.label(widgets::dim(money(*v))).on_hover_text(label);
                                        }
                                        Cost::NotOffered => {
                                            ui.label(widgets::dim("—")).on_hover_text("No equivalent in this stack");
                                        }
                                    }
                                }
                                ui.end_row();
                            }
                        }
                    }
                    ui.label("");
                    ui.label("");
                    ui.label(RichText::new("Total").strong());
                    ui.label("");
                    ui.label(RichText::new(money(report.rustcut_total())).color(theme::ACCENT).strong());
                    for k in 0..n {
                        ui.label(RichText::new(money(report.stack_total(k))).strong());
                    }
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(widgets::dim("Click ▸ to see each clip's tasks. Hover a commercial amount for the tool and rate used. \"—\" = no equivalent in that stack. Reused AI shots are split between the clips that use them.").small());
            });
        });
    if backfill {
        if let Ok(c) = economics::open() {
            let mut n = 0;
            for d in unrecorded_storyboards(app) {
                n += economics::backfill_storyboard(&c, &d.join("storyboard.json"), 0.069, 0.011).unwrap_or(0);
            }
            app.notify(format!("Added {n} estimated cost entries"));
        }
        do_refresh = true;
    }
    if do_refresh {
        refresh(app);
    }
    if let Some(ext) = export {
        if let Some(Ok(r)) = app.economics.report.clone() {
            if let Ok(c) = economics::open() {
                let _ = economics::save_report(&c, &r);
            }
            let body = if ext == "html" { economics::to_html(&r) } else { economics::to_csv(&r) };
            save_as(app, ext, body);
        }
    }
    if !open {
        app.economics.open = false;
        app.economics.report = None;
    }
}
