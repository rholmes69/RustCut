//! Schedule content window (File → 📅 Schedule content…, or from a YouTube upload): put
//! release days, premieres and posting slots on the user's Google Calendar with a reminder,
//! once or repeating, and see or remove what RustCut has scheduled. See `crate::calendar`.

use crate::app::EditorApp;
use crate::calendar::{self, Entry, Repeat, Scheduled};
use crate::google::SCOPE_CALENDAR;
use crate::localtime::{self, LocalTime};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Context, RichText, TextEdit};

#[derive(Default)]
pub struct CalendarUi {
    pub open: bool,
    entry: Entry,
    job: Option<Task<Vec<Scheduled>>>,
    list_job: Option<Task<Vec<Scheduled>>>,
    delete_job: Option<(String, Task<()>)>,
    /// Upcoming RustCut events, once loaded.
    upcoming: Option<Vec<Scheduled>>,
    /// The event waiting for a second click on Delete.
    confirm_delete: Option<String>,
    added: Option<Vec<Scheduled>>,
    error: Option<String>,
}

/// Opens the window with a fresh entry for the open project.
pub fn open(app: &mut EditorApp) {
    let c = &mut app.calendar;
    if c.entry.title.is_empty() {
        c.entry.title = format!("Post {}", app.project.name.trim());
    }
    c.entry.project = app.project.name.clone();
    if c.entry.link.is_empty() {
        if let Some((p, _)) = &app.youtube.last {
            c.entry.link = p.watch_url();
        }
    }
    open_window(app);
}

/// Opens the window with `entry` filled in (e.g. a YouTube premiere).
pub fn open_with(app: &mut EditorApp, entry: Entry) {
    app.calendar.entry = entry;
    app.calendar.added = None;
    open_window(app);
}

fn open_window(app: &mut EditorApp) {
    let c = &mut app.calendar;
    if c.entry.start.trim().is_empty() {
        let now = LocalTime::from_unix(localtime::now_unix());
        c.entry.start = LocalTime { hour: 18, minute: 0, ..now.plus_days(1) }.to_input();
    }
    c.open = true;
    c.upcoming = None; // reload when shown
}

pub fn poll(app: &mut EditorApp) {
    let c = &mut app.calendar;
    if let Some(state) = c.job.as_ref().and_then(|t| t.take_finished()) {
        c.job = None;
        match state {
            TaskState::Done(made) => {
                let n = made.len();
                let first = made.first().map(|s| LocalTime::from_unix(s.start).label()).unwrap_or_default();
                c.added = Some(made);
                c.upcoming = None;
                c.error = None;
                app.notify(if n == 1 { format!("Added to Google Calendar: {first}") } else { format!("Added {n} events to Google Calendar, from {first}") });
            }
            TaskState::Failed(e) => {
                c.error = Some(e);
                app.google.reload();
            }
            _ => {}
        }
    }
    let c = &mut app.calendar;
    if let Some(state) = c.list_job.as_ref().and_then(|t| t.take_finished()) {
        c.list_job = None;
        match state {
            TaskState::Done(list) => c.upcoming = Some(list),
            TaskState::Failed(e) => {
                c.upcoming = Some(Vec::new());
                c.error = Some(e);
            }
            _ => {}
        }
    }
    if let Some(state) = c.delete_job.as_ref().and_then(|(_, t)| t.take_finished()) {
        let (id, _) = c.delete_job.take().unwrap();
        match state {
            TaskState::Done(()) => {
                if let Some(list) = &mut c.upcoming {
                    list.retain(|s| s.id != id);
                }
            }
            TaskState::Failed(e) => c.error = Some(e),
            _ => {}
        }
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.calendar.open {
        return;
    }
    let mut open = true;
    egui::Window::new(RichText::new("📅 Schedule content").strong()).id(egui::Id::new("calendar")).open(&mut open).default_width(500.0).show(ctx, |ui| {
        let (c, g) = (&mut app.calendar, &mut app.google);
        if !g.can(SCOPE_CALENDAR) {
            crate::ui::google::connect_ui(ui, ctx, g, SCOPE_CALENDAR);
            return;
        }
        crate::ui::google::account_line(ui, g);
        if let Some(e) = &c.error {
            ui.colored_label(theme::DANGER, e);
        }
        if let Some(made) = &c.added {
            let first = made.first().cloned();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("✅ Added {} to your calendar", if made.len() == 1 { "the event".to_string() } else { format!("{} events", made.len()) })).color(theme::ACCENT));
                if let Some(f) = first.filter(|f| !f.link.is_empty()) {
                    if ui.small_button("Open").clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(f.link));
                    }
                }
            });
        }
        ui.separator();

        let busy = c.job.is_some();
        let e = &mut c.entry;
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("cal-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("What");
                ui.add(TextEdit::singleline(&mut e.title).desired_width(320.0).hint_text("Post the teaser, release day, premiere…"));
                ui.end_row();
                ui.label("Where");
                egui::ComboBox::from_id_salt("cal-platform").selected_text(e.platform.clone()).show_ui(ui, |ui| {
                    for (p, _) in calendar::PLATFORMS {
                        ui.selectable_value(&mut e.platform, p.to_string(), *p);
                    }
                });
                ui.end_row();
                ui.label("When");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut e.start).desired_width(140.0).hint_text("2026-11-25 18:00"));
                        match LocalTime::parse(&e.start) {
                            Ok(t) => ui.label(widgets::dim(t.label())),
                            Err(err) => ui.colored_label(theme::DANGER, err.to_string()),
                        };
                    });
                    ui.horizontal(|ui| {
                        let now = LocalTime::from_unix(localtime::now_unix());
                        if ui.small_button("Tomorrow 6 PM").clicked() {
                            e.start = LocalTime { hour: 18, minute: 0, ..now.plus_days(1) }.to_input();
                        }
                        if ui.small_button("Friday 6 PM").clicked() {
                            let wd = localtime::days_from_civil(now.year, now.month, now.day).rem_euclid(7); // 1 = Friday
                            let ahead = match (1 - wd).rem_euclid(7) {
                                0 => 7,
                                n => n,
                            };
                            e.start = LocalTime { hour: 18, minute: 0, ..now.plus_days(ahead) }.to_input();
                        }
                        if ui.small_button("+1 day").clicked() {
                            if let Ok(t) = LocalTime::parse(&e.start) {
                                e.start = t.plus_days(1).to_input();
                            }
                        }
                    });
                });
                ui.end_row();
                ui.label("Length");
                ui.add(egui::DragValue::new(&mut e.minutes).range(5..=1440).suffix(" min").speed(5));
                ui.end_row();
                ui.label("Repeat");
                ui.horizontal(|ui| {
                    for r in Repeat::ALL {
                        ui.radio_value(&mut e.repeat, r, r.label());
                    }
                    if e.repeat != Repeat::Once {
                        ui.add(egui::DragValue::new(&mut e.times).range(2..=52).suffix(" times"));
                    }
                });
                ui.end_row();
                ui.label("Reminder");
                let current = calendar::REMINDERS.iter().find(|(m, _)| *m == e.reminder).map(|(_, l)| *l).unwrap_or("Custom");
                egui::ComboBox::from_id_salt("cal-reminder").selected_text(current).show_ui(ui, |ui| {
                    for (m, l) in calendar::REMINDERS {
                        ui.selectable_value(&mut e.reminder, *m, *l);
                    }
                });
                ui.end_row();
                ui.label("Link");
                ui.add(TextEdit::singleline(&mut e.link).desired_width(320.0).hint_text("YouTube or Drive link (optional)"));
                ui.end_row();
                ui.label("Notes");
                ui.add(TextEdit::multiline(&mut e.notes).desired_rows(3).desired_width(320.0).hint_text("Caption, hashtags, which clip to post…"));
                ui.end_row();
            });
        });
        let problems = calendar::validate(e, localtime::now_unix());
        for p in &problems {
            ui.colored_label(theme::DANGER, format!("• {p}"));
        }
        ui.horizontal(|ui| {
            let n = calendar::occurrences(e).map(|o| o.len()).unwrap_or(1);
            let label = if n > 1 { format!("📅 Add {n} events to Google Calendar") } else { "📅 Add to Google Calendar".to_string() };
            if ui.add_enabled(problems.is_empty() && !busy, egui::Button::new(RichText::new(label).strong())).clicked() {
                c.error = None;
                c.added = None;
                let entry = e.clone();
                c.job = Some(Task::spawn(ctx.clone(), move |_, _| calendar::create(&entry).map(Some)));
            }
            if busy {
                ui.spinner();
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Coming up (scheduled from RustCut)").strong());
            if ui.small_button("↻").on_hover_text("Refresh").clicked() {
                c.upcoming = None;
            }
        });
        if c.upcoming.is_none() && c.list_job.is_none() {
            c.list_job = Some(Task::spawn(ctx.clone(), |_, _| calendar::upcoming(25).map(Some)));
        }
        match &c.upcoming {
            None => {
                ui.spinner();
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            Some(list) if list.is_empty() => {
                ui.label(widgets::dim("Nothing scheduled yet."));
            }
            Some(list) => {
                let mut delete: Option<String> = None;
                let mut open_link: Option<String> = None;
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for s in list {
                        ui.horizontal(|ui| {
                            ui.label(widgets::dim(LocalTime::from_unix(s.start).label()));
                            ui.label(&s.title);
                            if !s.platform.is_empty() {
                                ui.label(RichText::new(&s.platform).size(11.0).color(theme::TEXT_DIM));
                            }
                            if !s.link.is_empty() && ui.small_button("Open").clicked() {
                                open_link = Some(s.link.clone());
                            }
                            let deleting = c.delete_job.as_ref().is_some_and(|(id, _)| *id == s.id);
                            if deleting {
                                ui.spinner();
                            } else if c.confirm_delete.as_deref() == Some(s.id.as_str()) {
                                if ui.small_button(RichText::new("Delete — sure?").color(theme::DANGER)).clicked() {
                                    delete = Some(s.id.clone());
                                }
                            } else if ui.small_button("🗑").on_hover_text("Remove from the calendar").clicked() {
                                c.confirm_delete = Some(s.id.clone());
                            }
                        });
                    }
                });
                if let Some(url) = open_link {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
                if let Some(id) = delete {
                    c.confirm_delete = None;
                    let job_id = id.clone();
                    c.delete_job = Some((id, Task::spawn(ctx.clone(), move |_, _| calendar::delete(&job_id).map(Some))));
                }
            }
        }
        if c.list_job.is_some() || c.delete_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    });
    if !open {
        app.calendar.open = false;
    }
}
