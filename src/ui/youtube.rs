//! YouTube window (File → ▶ Upload to YouTube…, or the button after an export): everything
//! YouTube's upload form asks for, filled in from the project, checked before upload, then
//! uploaded with progress. See `crate::youtube`; the sign-in panel is `crate::ui::google`.

use crate::app::EditorApp;
use crate::google::SCOPE_YOUTUBE;
use crate::localtime::{self, LocalTime};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use crate::youtube::{self, Details, Published, Visibility};
use egui::{Context, RichText, ScrollArea, TextEdit};
use std::path::PathBuf;

#[derive(Default)]
pub struct YouTubeUi {
    pub open: bool,
    video: Option<PathBuf>,
    details: Details,
    /// The project the details were filled in from (refilled when another project is open).
    filled_for: Option<String>,
    ai_job: Option<Task<Details>>,
    job: Option<Task<Published>>,
    /// Kept to offer a calendar entry for a scheduled premiere.
    pub last: Option<(Published, Details)>,
    error: Option<String>,
}

impl YouTubeUi {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

/// Opens the form for `video` (or the last export), filled in from the project.
pub fn open_for(app: &mut EditorApp, video: Option<PathBuf>) {
    let video = video.or_else(|| app.last_export.clone().filter(|p| p.is_file()));
    let y = &mut app.youtube;
    if video.is_some() {
        y.video = video;
    }
    if y.filled_for.as_deref() != Some(app.project.name.as_str()) {
        y.details = youtube::suggest(&app.project, &y.details);
        y.filled_for = Some(app.project.name.clone());
    }
    y.open = true;
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.youtube.ai_job.as_ref().and_then(|t| t.take_finished()) {
        app.youtube.ai_job = None;
        match state {
            TaskState::Done(d) => {
                // Keep anything the user changed outside title, description and tags.
                let y = &mut app.youtube;
                y.details.title = d.title;
                y.details.description = d.description;
                y.details.tags = d.tags;
            }
            TaskState::Failed(e) => app.youtube.error = Some(format!("Write with AI failed: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.youtube.job.as_ref().and_then(|t| t.take_finished()) {
        app.youtube.job = None;
        match state {
            TaskState::Done(p) => {
                app.notify(format!("Uploaded \"{}\" to YouTube > {}", p.title, p.watch_url()));
                let d = app.youtube.details.clone();
                app.youtube.last = Some((p, d));
            }
            TaskState::Failed(e) => {
                app.notify_error(format!("YouTube upload failed: {e}"));
                app.youtube.error = Some(e);
                app.google.reload();
            }
            _ => app.notify("Cancelled the YouTube upload"),
        }
    }
}

fn counter(ui: &mut egui::Ui, n: usize, max: usize) {
    let color = if n > max { theme::DANGER } else { theme::TEXT_DIM };
    ui.label(RichText::new(format!("{n}/{max}")).size(11.0).color(color));
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.youtube.open {
        return;
    }
    let mut open = true;
    let mut refill = false;
    let mut to_calendar: Option<crate::calendar::Entry> = None;
    let mut pick_video = false;
    let project = app.project.clone();
    egui::Window::new(RichText::new("▶ Upload to YouTube").strong()).id(egui::Id::new("youtube")).open(&mut open).default_width(560.0).default_height(680.0).show(ctx, |ui| {
        let (y, g) = (&mut app.youtube, &mut app.google);
        if !g.can(SCOPE_YOUTUBE) {
            crate::ui::google::connect_ui(ui, ctx, g, SCOPE_YOUTUBE);
            return;
        }
        crate::ui::google::account_line(ui, g);

        if let Some(e) = &y.error {
            ui.colored_label(theme::DANGER, e);
        }

        // Running or finished upload.
        if let Some(job) = &y.job {
            ui.label(RichText::new(format!("Uploading \"{}\"", y.details.title)).strong());
            if let Some((p, stage)) = job.progress() {
                ui.add(egui::ProgressBar::new(p).show_percentage());
                ui.label(widgets::dim(stage));
            }
            if ui.button("Cancel upload").clicked() {
                job.cancel();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        let mut another = false;
        if let Some((p, d)) = &y.last {
            ui.label(RichText::new(format!("✅ \"{}\" is on YouTube", p.title)).strong().color(theme::ACCENT));
            if d.visibility == Visibility::Scheduled {
                if let Ok(t) = LocalTime::parse(&d.publish_at) {
                    ui.label(format!("It goes public {}.", t.label()));
                }
            }
            ui.label(widgets::dim("YouTube is processing it now; HD versions can take a few minutes more."));
            for w in &p.warnings {
                ui.colored_label(theme::DANGER, w);
            }
            ui.horizontal(|ui| {
                if ui.button("▶ Watch").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(p.watch_url()));
                }
                if ui.button("🛠 Open in YouTube Studio").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(p.studio_url()));
                }
                if ui.button("📋 Copy link").clicked() {
                    ctx.copy_text(p.watch_url());
                }
                if ui.button("Upload another").clicked() {
                    another = true;
                }
            });
            ui.horizontal(|ui| {
                let premiere = d.visibility == Visibility::Scheduled;
                let label = if premiere { "📅 Add the premiere to my calendar" } else { "📅 Schedule posts about it" };
                if ui.button(label).on_hover_text("Opens Schedule content with this video's link filled in").clicked() {
                    to_calendar = Some(calendar_entry(p, d, &project.name));
                }
            });
        }
        if another {
            y.last = None;
        }
        if y.last.is_some() {
            return;
        }

        ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.75).show(ui, |ui| {
            // The file.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Video").strong());
                match &y.video {
                    Some(p) => {
                        let size = std::fs::metadata(p).map(|m| crate::google::size_label(m.len())).unwrap_or_else(|_| "missing".into());
                        ui.label(format!("{} · {size}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).on_hover_text(p.display().to_string());
                    }
                    None => {
                        ui.label(widgets::dim("none chosen"));
                    }
                }
                if ui.button("Choose…").clicked() {
                    pick_video = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.button("↺ Fill in from project").on_hover_text("Title, description with chapters and lyrics, tags, category, AI disclosure and thumbnail from the project (free)").clicked() {
                    refill = true;
                }
                let busy = y.ai_job.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new(if busy { "✨ Writing…" } else { "✨ Write with AI (≈ 1¢)" }))
                    .on_hover_text("The editor model drafts a catchier title, an opening, hashtags and search tags, keeping the chapters and lyrics")
                    .clicked()
                {
                    y.error = None;
                    let (p, d) = (project.clone(), y.details.clone());
                    y.ai_job = Some(Task::spawn(ctx.clone(), move |_, _| youtube::write_with_ai(&p, &d).map(Some)));
                }
                if busy {
                    ui.spinner();
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            });
            ui.separator();

            let d = &mut y.details;
            ui.label(RichText::new("Details").strong());
            ui.horizontal(|ui| {
                ui.label("Title (required)");
                counter(ui, d.title.trim().chars().count(), youtube::TITLE_MAX);
            });
            ui.add(TextEdit::singleline(&mut d.title).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label("Description");
                counter(ui, d.description.len(), youtube::DESCRIPTION_MAX_BYTES);
            });
            ui.add(TextEdit::multiline(&mut d.description).desired_rows(8).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label("Tags (comma-separated)");
                counter(ui, youtube::tags_length(&youtube::tag_list(&d.tags)), youtube::TAGS_MAX);
            });
            ui.add(TextEdit::multiline(&mut d.tags).desired_rows(2).desired_width(f32::INFINITY));
            egui::Grid::new("yt-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Category");
                let current = youtube::CATEGORIES.iter().find(|(id, _)| *id == d.category).map(|(_, n)| *n).unwrap_or("Entertainment");
                egui::ComboBox::from_id_salt("yt-cat").selected_text(current).show_ui(ui, |ui| {
                    for (id, name) in youtube::CATEGORIES {
                        ui.selectable_value(&mut d.category, id.to_string(), *name);
                    }
                });
                ui.end_row();
                ui.label("Language");
                let current = youtube::LANGUAGES.iter().find(|(id, _)| *id == d.language).map(|(_, n)| n.to_string()).unwrap_or_else(|| d.language.clone());
                egui::ComboBox::from_id_salt("yt-lang").selected_text(current).show_ui(ui, |ui| {
                    for (id, name) in youtube::LANGUAGES {
                        ui.selectable_value(&mut d.language, id.to_string(), *name);
                    }
                });
                ui.end_row();
                ui.label("Thumbnail");
                ui.horizontal(|ui| {
                    match &d.thumbnail {
                        Some(t) => {
                            ui.label(t.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).on_hover_text(t.display().to_string());
                        }
                        None => {
                            ui.label(widgets::dim("YouTube picks one"));
                        }
                    }
                    if ui.button("Choose…").on_hover_text("JPG or PNG, 1280×720 recommended, under 2 MB").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Image", &["jpg", "jpeg", "png"]).pick_file() {
                            d.thumbnail = Some(p);
                        }
                    }
                    if d.thumbnail.is_some() && ui.small_button("✕").on_hover_text("No custom thumbnail").clicked() {
                        d.thumbnail = None;
                    }
                });
                ui.end_row();
            });
            ui.separator();

            ui.label(RichText::new("Audience (required)").strong());
            ui.label("Is this video made for kids?");
            ui.horizontal(|ui| {
                if ui.radio(d.made_for_kids == Some(true), "Yes, it's made for kids").clicked() {
                    d.made_for_kids = Some(true);
                }
                if ui.radio(d.made_for_kids == Some(false), "No, it's not made for kids").clicked() {
                    d.made_for_kids = Some(false);
                }
            });
            ui.label(widgets::dim("Required by US law (COPPA). Made-for-kids videos have comments and personalised ads turned off."));
            ui.add_space(4.0);
            ui.label(RichText::new("Altered content").strong());
            ui.checkbox(&mut d.synthetic, "Contains realistic altered or synthetic content (e.g. made with AI)")
                .on_hover_text("YouTube asks creators to disclose realistic people, places or events that were made or changed with AI. Ticked automatically when the project uses RustCut's AI shots.");
            ui.separator();

            ui.label(RichText::new("Visibility").strong());
            ui.horizontal(|ui| {
                for v in Visibility::ALL {
                    ui.radio_value(&mut d.visibility, v, v.label());
                }
            });
            if d.visibility == Visibility::Scheduled {
                if d.publish_at.trim().is_empty() {
                    let tomorrow = LocalTime::from_unix(localtime::now_unix()).plus_days(1);
                    d.publish_at = LocalTime { hour: 18, minute: 0, ..tomorrow }.to_input();
                }
                ui.horizontal(|ui| {
                    ui.label("Goes public at");
                    ui.add(TextEdit::singleline(&mut d.publish_at).desired_width(140.0).hint_text("2026-11-25 18:00"));
                    match LocalTime::parse(&d.publish_at) {
                        Ok(t) => ui.label(widgets::dim(format!("{} (your time)", t.label()))),
                        Err(e) => ui.colored_label(theme::DANGER, e.to_string()),
                    };
                });
                ui.horizontal(|ui| {
                    let now = LocalTime::from_unix(localtime::now_unix());
                    if ui.small_button("Tomorrow 6 PM").clicked() {
                        d.publish_at = LocalTime { hour: 18, minute: 0, ..now.plus_days(1) }.to_input();
                    }
                    if ui.small_button("Friday 6 PM").clicked() {
                        let wd = localtime::days_from_civil(now.year, now.month, now.day).rem_euclid(7); // 1 = Friday
                        let ahead = match (1 - wd).rem_euclid(7) {
                            0 => 7,
                            n => n,
                        };
                        d.publish_at = LocalTime { hour: 18, minute: 0, ..now.plus_days(ahead) }.to_input();
                    }
                    if ui.small_button("+1 week").clicked() {
                        if let Ok(t) = LocalTime::parse(&d.publish_at) {
                            d.publish_at = t.plus_days(7).to_input();
                        }
                    }
                });
                ui.label(widgets::dim("Uploaded as Private now; YouTube makes it public at that time."));
            }
            ui.collapsing("More options", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Licence");
                    ui.radio_value(&mut d.creative_commons, false, "Standard YouTube licence");
                    ui.radio_value(&mut d.creative_commons, true, "Creative Commons – Attribution");
                });
                ui.checkbox(&mut d.embeddable, "Allow embedding on other sites");
                ui.add_enabled(d.visibility == Visibility::Public, egui::Checkbox::new(&mut d.notify_subscribers, "Notify subscribers"))
                    .on_disabled_hover_text("Only for videos published right away as Public");
                ui.checkbox(&mut d.show_likes, "Show how many viewers like this video");
            });
            ui.add_space(4.0);
            ui.label(widgets::dim(
                "Note: YouTube keeps uploads from a Google Cloud project that hasn't passed its API audit as Private. If that happens, RustCut tells you after the upload.",
            ));
        });

        ui.separator();
        let problems = youtube::validate(&y.details, y.video.as_deref(), localtime::now_unix());
        for p in &problems {
            ui.colored_label(theme::DANGER, format!("• {p}"));
        }
        if ui.add_enabled(problems.is_empty(), egui::Button::new(RichText::new("⬆ Upload to YouTube").strong())).clicked() {
            y.error = None;
            let (video, details) = (y.video.clone().unwrap(), y.details.clone());
            y.job = Some(Task::spawn(ctx.clone(), move |cancel, report| youtube::upload(&video, &details, cancel, report)));
        }
    });
    if !open {
        app.youtube.open = false;
    }
    if let Some(entry) = to_calendar {
        crate::ui::calendar::open_with(app, entry);
    }
    if refill {
        app.youtube.details = youtube::suggest(&app.project, &app.youtube.details);
        app.youtube.filled_for = Some(app.project.name.clone());
    }
    if pick_video {
        if let Some(p) = crate::ui::gdrive::pick_video(app, "Choose the video to upload to YouTube") {
            app.youtube.video = Some(p);
        }
    }
}

/// A calendar entry for an uploaded video: the premiere itself when it's scheduled, otherwise
/// a "share it" slot tomorrow evening.
pub fn calendar_entry(p: &Published, d: &Details, project: &str) -> crate::calendar::Entry {
    let mut e = crate::calendar::Entry { link: p.watch_url(), project: project.to_string(), platform: "YouTube".into(), ..Default::default() };
    match LocalTime::parse(&d.publish_at).ok().filter(|_| d.visibility == Visibility::Scheduled) {
        Some(t) => {
            e.title = format!("Premiere: {}", p.title);
            e.platform = "Release / premiere".into();
            e.start = t.to_input();
            e.reminder = Some(60);
            e.notes = "Goes public on YouTube. Share the link when it's live.".into();
        }
        None => {
            e.title = format!("Share \"{}\"", p.title);
            let now = LocalTime::from_unix(localtime::now_unix());
            e.start = LocalTime { hour: 18, minute: 0, ..now.plus_days(1) }.to_input();
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premiere_goes_on_the_calendar() {
        let p = Published { id: "abc".into(), title: "Falling Deeper".into(), status: "uploaded".into(), warnings: vec![] };
        let d = Details { visibility: Visibility::Scheduled, publish_at: "2030-05-01 18:00".into(), ..Default::default() };
        let e = calendar_entry(&p, &d, "Falling Deeper - lyric video");
        assert_eq!(e.title, "Premiere: Falling Deeper");
        assert_eq!(e.start, "2030-05-01 18:00");
        assert_eq!(e.link, "https://youtu.be/abc");
        assert!(crate::calendar::validate(&e, 1_795_000_000).is_empty());
        let e = calendar_entry(&p, &Details::default(), "x");
        assert!(e.title.starts_with("Share") && e.platform == "YouTube");
    }
}
