//! Application state, editing commands and the per-frame update loop.

use crate::history::History;
use crate::media::assets::AssetManager;
use crate::media::ffmpeg;
use crate::model::*;
use crate::playback::audio::AudioEngine;
use crate::playback::PreviewEngine;
use crate::ui::export_dialog::ExportDialog;
use crate::ui::preview::PreviewUi;
use crate::ui::timeline::TimelineState;
use crate::ui::{self, theme};
use egui::{Align2, Color32, Context, FontId, Id as EguiId, Key, LayerId, Modifiers, Order};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PROJECT_EXT: &str = "rcproj";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeftTab {
    Media,
    Audio,
    Text,
    Filters,
    Animation,
    Captions,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct EditorApp {
    pub project: Project,
    pub history: History,
    pub project_path: Option<PathBuf>,
    saved: Project,
    pub selection: Option<Id>,
    pub selected_media: Option<Id>,
    pub playhead: f64,
    pub playing: bool,
    play_anchor: (Instant, f64),
    pub preview: PreviewEngine,
    pub preview_ui: PreviewUi,
    pub audio: AudioEngine,
    pub assets: AssetManager,
    pub timeline: TimelineState,
    pub left_tab: LeftTab,
    /// Media item being dragged from the library onto the timeline.
    pub drag_media: Option<Id>,
    pub export: ExportDialog,
    pub anim_len: f64,
    pub captions: ui::captions::CaptionsUi,
    pub smart: ui::smart_audio::SmartAudioUi,
    pub ai: ui::ai::AiUi,
    pub video_gen: ui::video_gen::VideoGenUi,
    pub image_gen: ui::image_gen::ImageGenUi,
    pub storyboard: ui::storyboard::StoryboardUi,
    pub music: ui::music::MusicUi,
    pub local: ui::local_ai::LocalAiUi,
    pub economics: ui::economics::EconomicsUi,
    pub voice: ui::voice::VoiceUi,
    pub rough_cut: ui::rough_cut::RoughCutUi,
    pub toast: Option<Toast>,
    pub ffmpeg_ok: bool,
    last_title: String,
    last_autosave: Instant,
    /// Recently opened/saved projects, most recent first (File → Open recent).
    pub recent: Vec<PathBuf>,
    /// Work recovered after a crash, waiting for the user to restore or discard it.
    restore_offer: Option<(Project, std::time::SystemTime)>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let project = Project::default();
        let mut app = Self {
            history: History::new(&project),
            saved: project.clone(),
            project,
            project_path: None,
            selection: None,
            selected_media: None,
            playhead: 0.0,
            playing: false,
            play_anchor: (Instant::now(), 0.0),
            preview: PreviewEngine::new(ctx.clone()),
            preview_ui: PreviewUi::default(),
            audio: AudioEngine::new(),
            assets: AssetManager::new(ctx),
            timeline: TimelineState::default(),
            left_tab: LeftTab::Media,
            drag_media: None,
            export: ExportDialog::default(),
            anim_len: 0.5,
            captions: Default::default(),
            smart: Default::default(),
            ai: Default::default(),
            video_gen: Default::default(),
            image_gen: Default::default(),
            storyboard: Default::default(),
            music: Default::default(),
            local: Default::default(),
            economics: Default::default(),
            voice: Default::default(),
            rough_cut: Default::default(),
            toast: None,
            ffmpeg_ok: ffmpeg::is_available(),
            last_title: String::new(),
            last_autosave: Instant::now(),
            recent: crate::recent::load(),
            restore_offer: None,
        };
        if !app.ffmpeg_ok {
            app.notify_error("FFmpeg was not found on PATH. Install it (e.g. `winget install ffmpeg`) to import, preview and export.");
        }
        app.restore_offer = recover_session();
        if let Some(path) = initial {
            app.import_paths(vec![path]);
        }
        app
    }

    // ── notifications ────────────────────────────────────────────────────────

    pub fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now() });
    }

    pub fn notify_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        log_error(&text);
        self.toast = Some(Toast { text, error: true, at: Instant::now() });
    }

    // ── playback ─────────────────────────────────────────────────────────────

    pub fn frame_dt(&self) -> f64 {
        1.0 / self.project.fps.max(1) as f64
    }

    pub fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn play(&mut self) {
        let duration = self.project.duration();
        if duration <= 0.0 {
            return;
        }
        if self.playhead >= duration - 1e-3 {
            self.playhead = 0.0;
        }
        self.playing = true;
        self.play_anchor = (Instant::now(), self.playhead);
        self.audio.start(&self.project, self.playhead);
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.audio.stop();
    }

    pub fn seek(&mut self, t: f64) {
        self.playhead = t.clamp(0.0, self.project.duration().max(0.0));
        if self.playing {
            self.play_anchor = (Instant::now(), self.playhead);
            self.audio.start(&self.project, self.playhead);
        }
    }

    fn advance_playback(&mut self, ctx: &Context) {
        if !self.playing {
            return;
        }
        let t = self.play_anchor.1 + self.play_anchor.0.elapsed().as_secs_f64();
        let duration = self.project.duration();
        if t >= duration {
            self.playhead = duration;
            self.pause();
        } else {
            self.playhead = t;
            self.audio.tick(&self.project, t);
            ctx.request_repaint();
        }
    }

    // ── media ────────────────────────────────────────────────────────────────

    pub fn import_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Import media")
            .add_filter("Media", ffmpeg::MEDIA_EXTS)
            .add_filter("All files", &["*"])
            .pick_files()
        {
            self.import_paths(files);
        }
    }

    pub fn import_paths(&mut self, paths: Vec<PathBuf>) {
        if !self.ffmpeg_ok {
            self.notify_error("FFmpeg is required to import media.");
            return;
        }
        for path in paths {
            let is_project = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXT));
            if is_project {
                self.open_path(&path);
            } else if path.is_file() {
                self.assets.import(path);
            }
        }
    }

    fn poll_background(&mut self) {
        for (path, result) in self.assets.poll() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match result {
                Ok(info) => {
                    let kind = info.kind;
                    let id = self.project.add_media(info.into_media_item(path));
                    self.selected_media = Some(id);
                    if kind == MediaKind::Audio && self.left_tab == LeftTab::Media {
                        self.notify("Audio imported — find it in the Audio tab");
                    }
                }
                Err(e) => self.notify_error(format!("Could not import {name}: {e}")),
            }
        }
    }

    /// Adds media at the playhead (an empty main track starts at 0): video and images
    /// on the main track when it's free there, otherwise on a track above; audio on an
    /// audio track. The timeline scrolls to show the new clip.
    pub fn add_media_to_timeline(&mut self, media_id: Id) {
        let Some(kind) = self.project.media(media_id).map(|m| m.kind) else { return };
        let Some(mut clip) = self.project.new_media_clip(media_id, self.playhead) else { return };
        let id = clip.id;
        clip.start = self.playhead;
        if kind == MediaKind::Audio {
            self.project.place_clip(clip, None);
        } else {
            let main = self.project.main_track();
            if self.project.tracks[main].clips.is_empty() {
                clip.start = 0.0;
            }
            self.project.place_clip(clip, Some(main));
        }
        self.selection = Some(id);
        if let Some(start) = self.project.clip(id).map(|c| c.start) {
            self.reveal(start);
        }
    }

    /// Moves the playhead to `t` and scrolls the timeline so it's visible.
    pub fn reveal(&mut self, t: f64) {
        self.seek(t);
        self.timeline.reveal = Some(t);
    }

    pub fn add_text(&mut self, style: TextStyle) {
        let clip = self.project.new_text_clip(style, self.playhead);
        let id = clip.id;
        self.project.place_clip(clip, None);
        self.selection = Some(id);
    }

    // ── editing ──────────────────────────────────────────────────────────────

    fn clip_at_playhead(&self) -> Option<Id> {
        let t = self.playhead;
        let usable = |c: &Clip| c.is_active(t);
        if let Some(id) = self.selection {
            if self.project.clip(id).is_some_and(usable) {
                return Some(id);
            }
        }
        let main = self.project.main_track_id();
        let tracks = &self.project.tracks;
        tracks
            .iter()
            .filter(|tr| Some(tr.id) == main && !tr.locked)
            .chain(tracks.iter().filter(|tr| !tr.locked))
            .find_map(|tr| tr.clips.iter().find(|c| usable(c)).map(|c| c.id))
    }

    pub fn split_at_playhead(&mut self) {
        match self.clip_at_playhead() {
            Some(id) => match self.project.split_clip(id, self.playhead) {
                Some(right) => self.selection = Some(right),
                None => self.notify("Too close to the clip edge to split"),
            },
            None => self.notify("Move the playhead over a clip to split it"),
        }
    }

    pub fn delete_selection(&mut self) {
        if let Some(id) = self.selection.take() {
            self.project.remove_clip(id);
            self.project.cleanup_tracks();
        }
    }

    pub fn duplicate_selection(&mut self) {
        if let Some(id) = self.selection {
            self.selection = self.project.duplicate_clip(id).or(Some(id));
        }
    }

    pub fn undo(&mut self) {
        if self.history.undo(&mut self.project) {
            self.after_history_change();
        }
    }

    pub fn redo(&mut self) {
        if self.history.redo(&mut self.project) {
            self.after_history_change();
        }
    }

    fn after_history_change(&mut self) {
        self.timeline.drag = None;
        if self.playing {
            self.audio.start(&self.project, self.playhead);
        }
    }

    // ── files ────────────────────────────────────────────────────────────────

    pub fn is_dirty(&self) -> bool {
        self.project != self.saved
    }

    pub fn new_project(&mut self) {
        self.pause();
        crate::economics::set_current_project(None);
        self.project = Project::default();
        self.project_path = None;
        self.saved = self.project.clone();
        self.selection = None;
        self.playhead = 0.0;
        self.preview.clear();
        self.notify("New project — Ctrl+Z restores the previous one");
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open project")
            .add_filter("RustCut project", &[PROJECT_EXT])
            .pick_file()
        {
            self.open_path(&path);
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        match Project::load(path) {
            Ok(project) => {
                self.pause();
                self.project = project;
                self.history.reset(&self.project);
                self.saved = self.project.clone();
                self.project_path = Some(path.to_path_buf());
                crate::economics::set_current_project(Some(path));
                crate::recent::push(&mut self.recent, path);
                self.selection = None;
                self.playhead = 0.0;
                self.preview.clear();
                let missing = self.project.media.iter().filter(|m| !m.path.exists()).count();
                if missing > 0 {
                    self.notify_error(format!("Opened project — {missing} media file(s) are missing"));
                } else {
                    self.notify(format!("Opened {}", path.display()));
                }
            }
            Err(e) => self.notify_error(format!("Could not open project: {e:#}")),
        }
    }

    pub fn save(&mut self, save_as: bool) {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .set_title("Save project")
                .add_filter("RustCut project", &[PROJECT_EXT])
                .set_file_name(format!("{}.{PROJECT_EXT}", sanitize(&self.project.name)))
                .save_file(),
        };
        let Some(path) = path else { return };
        match self.project.save(&path) {
            Ok(()) => {
                self.saved = self.project.clone();
                self.project_path = Some(path.clone());
                crate::economics::set_current_project(Some(&path));
                crate::recent::push(&mut self.recent, &path);
                self.notify(format!("Saved {}", path.display()));
            }
            Err(e) => self.notify_error(format!("Save failed: {e:#}")),
        }
    }

    pub fn open_export(&mut self) {
        if self.project.duration() <= 0.0 {
            self.notify("Add something to the timeline before exporting");
            return;
        }
        self.pause();
        self.export.open(&self.project, self.project_path.as_deref());
    }

    // ── per-frame plumbing ───────────────────────────────────────────────────

    fn handle_dropped_files(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.import_paths(dropped);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(LayerId::new(Order::Foreground, EguiId::new("file-drop")));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
            painter.rect_stroke(screen.shrink(18.0), 14.0, egui::Stroke::new(2.0_f32, theme::ACCENT));
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "Drop to import",
                FontId::proportional(28.0),
                theme::TEXT,
            );
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        #[derive(PartialEq)]
        enum A {
            Undo,
            Redo,
            Save,
            SaveAs,
            Open,
            Import,
            Export,
            Split,
            Duplicate,
            Delete,
            Play,
            Step(f64),
            Home,
            End,
        }
        let mut actions = Vec::new();
        let dt = self.frame_dt();
        ctx.input_mut(|i| {
            let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
            // Shift variants first: `consume_key` ignores extra Shift.
            if i.consume_key(cmd_shift, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y) {
                actions.push(A::Redo);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Z) {
                actions.push(A::Undo);
            }
            if i.consume_key(cmd_shift, Key::S) {
                actions.push(A::SaveAs);
            }
            if i.consume_key(Modifiers::COMMAND, Key::S) {
                actions.push(A::Save);
            }
            if i.consume_key(Modifiers::COMMAND, Key::O) {
                actions.push(A::Open);
            }
            if i.consume_key(Modifiers::COMMAND, Key::I) {
                actions.push(A::Import);
            }
            if i.consume_key(Modifiers::COMMAND, Key::E) {
                actions.push(A::Export);
            }
            if i.consume_key(Modifiers::COMMAND, Key::B) {
                actions.push(A::Split);
            }
            if i.consume_key(Modifiers::COMMAND, Key::D) {
                actions.push(A::Duplicate);
            }
            if i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace) {
                actions.push(A::Delete);
            }
            if i.consume_key(Modifiers::NONE, Key::Space) {
                actions.push(A::Play);
            }
            if i.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                actions.push(A::Step(-1.0));
            }
            if i.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                actions.push(A::Step(1.0));
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                actions.push(A::Step(-dt));
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                actions.push(A::Step(dt));
            }
            if i.consume_key(Modifiers::NONE, Key::Home) {
                actions.push(A::Home);
            }
            if i.consume_key(Modifiers::NONE, Key::End) {
                actions.push(A::End);
            }
        });
        for a in actions {
            match a {
                A::Undo => self.undo(),
                A::Redo => self.redo(),
                A::Save => self.save(false),
                A::SaveAs => self.save(true),
                A::Open => self.open_dialog(),
                A::Import => self.import_dialog(),
                A::Export => self.open_export(),
                A::Split => self.split_at_playhead(),
                A::Duplicate => self.duplicate_selection(),
                A::Delete => self.delete_selection(),
                A::Play => self.toggle_play(),
                A::Step(d) => self.seek(self.playhead + d),
                A::Home => {
                    self.seek(0.0);
                    self.timeline.scroll_x = 0.0;
                }
                A::End => {
                    let end = self.project.duration();
                    self.seek(end);
                    self.timeline.reveal = Some(end);
                }
            }
        }
    }

    fn draw_drag_ghost(&self, ctx: &Context) {
        let Some(id) = self.drag_media else { return };
        let (Some(media), Some(pos)) = (self.project.media(id), ctx.input(|i| i.pointer.latest_pos())) else { return };
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, EguiId::new("drag-ghost")));
        let galley = painter.layout_no_wrap(media.name.clone(), FontId::proportional(12.0), theme::TEXT);
        let rect = egui::Rect::from_min_size(pos + egui::vec2(14.0, 10.0), galley.size() + egui::vec2(16.0, 10.0));
        painter.rect_filled(rect, 6.0, theme::ACCENT_DIM);
        painter.galley(rect.min + egui::vec2(8.0, 5.0), galley, theme::TEXT);
    }

    fn draw_toast(&mut self, ctx: &Context) {
        let Some(toast) = &self.toast else { return };
        let life = if toast.error { 6.0 } else { 3.0 };
        let age = toast.at.elapsed().as_secs_f32();
        if age > life {
            self.toast = None;
            return;
        }
        let alpha = ((life - age) / 0.4).clamp(0.0, 1.0);
        egui::Area::new(EguiId::new("toast"))
            .anchor(Align2::CENTER_TOP, [0.0, 56.0])
            .order(Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                let fill = if toast.error { theme::DANGER } else { theme::SURFACE_HOVER };
                egui::Frame::none()
                    .fill(fill.gamma_multiply(alpha))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.label(egui::RichText::new(&toast.text).color(theme::TEXT.gamma_multiply(alpha)));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn finish_frame(&mut self, ctx: &Context) {
        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_media = None;
        }
        if let Some(id) = self.selection {
            if self.project.clip(id).is_none() {
                self.selection = None;
            }
        }
        // Auto-commit an undo step once an interaction has finished.
        let interacting = ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input();
        if !interacting {
            self.history.commit(&self.project);
        }
        // Autosave unsaved work every 30 s so a crash never loses it.
        if !interacting && self.is_dirty() && self.restore_offer.is_none() && self.last_autosave.elapsed() > Duration::from_secs(30) {
            self.last_autosave = Instant::now();
            let _ = std::fs::create_dir_all(crate::util::config_dir());
            let _ = self.project.save(&autosave_path());
        }
        let dirty = if self.is_dirty() { " •" } else { "" };
        let title = format!("{}{dirty} — RustCut", self.project.name);
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

/// Appends an error shown to the user to `errors.log` (error toasts vanish after a
/// few seconds; this keeps them readable). The file is trimmed to its last ~200 KB.
pub fn log_error(text: &str) {
    use std::io::Write;
    let dir = crate::util::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("errors.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 400_000) {
        if let Ok(old) = std::fs::read_to_string(&path) {
            // Keep roughly the last 200 KB, starting at a line boundary.
            let mut cut = old.len().saturating_sub(200_000);
            while !old.is_char_boundary(cut) {
                cut += 1;
            }
            let start = old[cut..].find('\n').map_or(old.len(), |i| cut + i + 1);
            let _ = std::fs::write(&path, &old[start..]);
        }
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[unix {secs} · {h:02}:{m:02}:{s:02} UTC] {}", text.replace('\n', " | "));
    }
}

fn autosave_path() -> PathBuf {
    crate::util::config_dir().join("autosave.rcproj")
}

fn session_lock_path() -> PathBuf {
    crate::util::config_dir().join("session.lock")
}

/// If the previous session did not exit cleanly, returns its autosaved project.
/// Also marks this session as running.
fn recover_session() -> Option<(Project, std::time::SystemTime)> {
    let crashed = session_lock_path().exists();
    let _ = std::fs::create_dir_all(crate::util::config_dir());
    let _ = std::fs::write(session_lock_path(), std::process::id().to_string());
    if !crashed {
        return None;
    }
    let when = std::fs::metadata(autosave_path()).and_then(|m| m.modified()).ok()?;
    let project = Project::load(&autosave_path()).ok()?;
    (project.clip_count() > 0 || !project.media.is_empty()).then_some((project, when))
}

impl EditorApp {
    fn restore_window(&mut self, ctx: &Context) {
        let Some((project, when)) = &self.restore_offer else { return };
        let ago = when.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        let ago = if ago < 120 { format!("{ago} seconds") } else if ago < 7200 { format!("{} minutes", ago / 60) } else { format!("{} hours", ago / 3600) };
        let summary = format!("“{}” — {} clips, {} media, autosaved {ago} ago.", project.name, project.clip_count(), project.media.len());
        let mut choice = None;
        egui::Window::new(egui::RichText::new("Restore unsaved work?").strong())
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("RustCut didn't close properly last time. Your work was autosaved:");
                ui.label(egui::RichText::new(summary).strong());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui::widgets::accent_button(ui, "Restore").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(false);
                    }
                });
            });
        match choice {
            Some(true) => {
                if let Some((project, _)) = self.restore_offer.take() {
                    self.pause();
                    self.project = project;
                    self.selection = None;
                    self.playhead = 0.0;
                    self.preview.clear();
                    self.notify("Restored — press Ctrl+S to save it");
                }
            }
            Some(false) => {
                self.restore_offer = None;
                let _ = std::fs::remove_file(autosave_path());
            }
            None => {}
        }
    }
}

impl eframe::App for EditorApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Clean exit: no recovery needed next time.
        let _ = std::fs::remove_file(session_lock_path());
        let _ = std::fs::remove_file(autosave_path());
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll_background();
        ui::captions::poll(self);
        ui::smart_audio::poll(self);
        ui::ai::poll(self);
        ui::video_gen::poll(self);
        ui::image_gen::poll(self);
        ui::storyboard::poll(self);
        ui::music::poll(self);
        ui::local_ai::poll(self);
        ui::voice::poll(self);
        ui::rough_cut::poll(self);
        self.handle_shortcuts(ctx);
        self.advance_playback(ctx);

        ui::topbar::show(ctx, self);
        ui::timeline::show(ctx, self);
        ui::library::show(ctx, self);
        ui::inspector::show(ctx, self);
        ui::preview::show(ctx, self);
        ui::export_dialog::show(ctx, self);
        ui::ai::windows(ctx, self);
        ui::video_gen::window(ctx, self);
        ui::image_gen::window(ctx, self);
        ui::storyboard::window(ctx, self);
        ui::music::window(ctx, self);
        ui::local_ai::transitions_window(ctx, self);
        ui::economics::window(ctx, self);
        ui::voice::window(ctx, self);
        ui::rough_cut::window(ctx, self);

        self.handle_dropped_files(ctx);
        self.draw_drag_ghost(ctx);
        self.draw_toast(ctx);
        self.restore_window(ctx);
        self.finish_frame(ctx);
        if self.assets.importing > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() { "project".into() } else { s }
}

pub fn default_output_path(project: &Project, project_path: Option<&Path>) -> PathBuf {
    let dir = project_path
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join("Videos"))
                .filter(|p| p.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("{}.mp4", sanitize(&project.name)))
}
