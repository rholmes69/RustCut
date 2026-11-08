//! Media library preview: plays a video (with its sound) or shows an image in a window,
//! without touching the timeline. Opened from a library item's right-click menu.

use crate::app::EditorApp;
use crate::media::decoder::VideoStream;
use crate::media::ffmpeg;
use crate::model::{Id, MediaItem, MediaKind};
use crate::playback::audio::AudioEngine;
use crate::ui::{theme, widgets};
use egui::{Button, ColorImage, Context, Key, Modifiers, RichText, TextureHandle, TextureOptions, Vec2};
use std::time::Instant;

/// Largest decode size: plenty for a preview window, cheap to stream.
const MAX_W: u32 = 1280;
const MAX_H: u32 = 720;

#[derive(Default)]
pub struct MediaPreview {
    media: Option<Id>,
    tex: Option<TextureHandle>,
    pos: f64,
    /// While playing: when playback (re)started and the position then.
    anchor: Option<(Instant, f64)>,
    stream: Option<VideoStream>,
    sink: Option<rodio::Sink>,
    /// Playing when the user grabbed the scrubber: resume after it's released.
    resume: bool,
    error: Option<String>,
}

impl MediaPreview {
    pub fn is_open(&self) -> bool {
        self.media.is_some()
    }

    fn playing(&self) -> bool {
        self.anchor.is_some()
    }

    fn stop(&mut self) {
        self.anchor = None;
        self.stream = None;
        if let Some(s) = self.sink.take() {
            s.stop();
        }
    }

    /// (Re)starts decoding at `self.pos`. Paused, only one frame is taken (the still).
    fn start(&mut self, ctx: &Context, audio: &mut AudioEngine, m: &MediaItem, play: bool) {
        self.stop();
        let len = m.duration.max(0.0);
        if play && self.pos >= len - 0.05 {
            self.pos = 0.0;
        }
        if m.kind == MediaKind::Video {
            let (w, h) = ffmpeg::fit_within(m.width, m.height, MAX_W, MAX_H);
            let fps = if m.fps > 1.0 { m.fps.min(60.0) } else { 30.0 };
            match VideoStream::spawn(ctx, &m.path, self.pos, 1.0, self.pos, w, h, fps) {
                Ok(s) => self.stream = Some(s),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if play {
            if m.has_sound() {
                self.sink = audio.play_file(&m.path, self.pos);
            }
            self.anchor = Some((Instant::now(), self.pos));
        }
    }

    fn show_rgba(&mut self, ctx: &Context, w: u32, h: u32, data: &[u8]) {
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], data);
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("media-preview", img, TextureOptions::LINEAR)),
        }
    }
}

/// Opens the preview for a library item; videos and audio start playing right away.
pub fn open(ctx: &Context, app: &mut EditorApp, id: Id) {
    let Some(m) = app.project.media(id).cloned() else { return };
    app.pause();
    let mut pv = MediaPreview { media: Some(id), ..Default::default() };
    if !m.path.exists() {
        pv.error = Some(format!("File not found: {}", m.path.display()));
    } else if m.kind == MediaKind::Image {
        let (w, h) = ffmpeg::fit_within(m.width, m.height, 1920, 1080);
        match ffmpeg::extract_frame(&m.path, 0.0, w, h) {
            Ok(data) => pv.show_rgba(ctx, w, h, &data),
            Err(e) => pv.error = Some(e.to_string()),
        }
    } else {
        pv.start(ctx, &mut app.audio, &m, true);
    }
    app.media_preview.stop();
    app.media_preview = pv;
}

/// Space plays/pauses and Esc closes while the preview is open. Returns true if the
/// preview is open (the editor's own shortcuts are skipped then).
pub fn keys(ctx: &Context, app: &mut EditorApp) -> bool {
    let Some(m) = app.media_preview.media.and_then(|id| app.project.media(id)).cloned() else { return false };
    let (space, esc) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Space), i.consume_key(Modifiers::NONE, Key::Escape)));
    if esc {
        app.media_preview.stop();
        app.media_preview = MediaPreview::default();
    } else if space && m.kind != MediaKind::Image {
        let play = !app.media_preview.playing();
        app.media_preview.start(ctx, &mut app.audio, &m, play);
    }
    true
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    let Some(id) = app.media_preview.media else { return };
    let Some(m) = app.project.media(id).cloned() else {
        app.media_preview.stop();
        app.media_preview = MediaPreview::default();
        return;
    };
    let mut pv = std::mem::take(&mut app.media_preview);
    let len = m.duration.max(0.0);

    // Clock.
    if let Some((t0, p0)) = pv.anchor {
        pv.pos = p0 + t0.elapsed().as_secs_f64();
        if pv.pos >= len {
            pv.pos = len;
            pv.stop();
        } else {
            ctx.request_repaint();
        }
    }
    // Newest decoded frame that is due (paused: the first frame, then stop decoding).
    let mut frame = None;
    if let Some(s) = &pv.stream {
        if pv.playing() {
            while let Ok(f) = s.rx.try_recv() {
                let due = f.time <= pv.pos + 0.02;
                frame = Some((s.width, s.height, f));
                if !due {
                    break;
                }
            }
        } else if let Ok(f) = s.rx.try_recv() {
            frame = Some((s.width, s.height, f));
        }
    }
    if let Some((w, h, f)) = frame {
        pv.show_rgba(ctx, w, h, &f.data);
        if !pv.playing() {
            pv.stream = None;
        }
    }

    let mut open = true;
    let mut add = false;
    let title = match m.kind {
        MediaKind::Image => "🖼",
        MediaKind::Video => "🎬",
        MediaKind::Audio => "🎵",
    };
    egui::Window::new(RichText::new(format!("{title}  {}", m.name)).strong())
        .id(egui::Id::new("media-preview"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(820.0)
        .show(ctx, |ui| {
            // Picture, fitted to the window width and at most ~65% of the screen height.
            let aspect = if m.width > 0 && m.height > 0 { m.height as f32 / m.width as f32 } else { 9.0 / 16.0 };
            let max_h = ctx.screen_rect().height() * 0.65;
            let mut size = Vec2::new(ui.available_width(), ui.available_width() * aspect);
            if size.y > max_h {
                size = Vec2::new(max_h / aspect, max_h);
            }
            ui.vertical_centered(|ui| match (&pv.tex, &pv.error, m.kind) {
                (_, Some(e), _) => {
                    ui.add_space(20.0);
                    ui.label(RichText::new(e).color(theme::DANGER));
                    ui.add_space(20.0);
                }
                (_, None, MediaKind::Audio) => {
                    ui.add_space(24.0);
                    ui.label(RichText::new("🎵").size(48.0).color(theme::TEXT_DIM));
                    ui.add_space(24.0);
                }
                (Some(tex), None, _) => {
                    ui.add(egui::Image::new((tex.id(), size)));
                }
                (None, None, _) => {
                    let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().rect_filled(r, 4.0, egui::Color32::BLACK);
                    ui.put(r, egui::Spinner::new());
                }
            });
            ui.add_space(6.0);

            if m.kind != MediaKind::Image && pv.error.is_none() {
                ui.horizontal(|ui| {
                    let icon = if pv.playing() { "⏸" } else { "▶" };
                    if ui.add_sized([34.0, 24.0], Button::new(RichText::new(icon).size(15.0))).on_hover_text("Play / pause (Space)").clicked() {
                        let play = !pv.playing();
                        pv.start(ctx, &mut app.audio, &m, play);
                    }
                    let time = format!("{} / {}", widgets::format_time(pv.pos), widgets::format_time(len));
                    ui.spacing_mut().slider_width = (ui.available_width() - 110.0).max(80.0);
                    let mut pos = pv.pos;
                    let r = ui.add(egui::Slider::new(&mut pos, 0.0..=len.max(0.01)).show_value(false));
                    if r.drag_started() {
                        pv.resume = pv.playing();
                        pv.stop();
                    }
                    if r.changed() {
                        pv.pos = pos;
                    }
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        let play = std::mem::take(&mut pv.resume) || (pv.playing() && !r.dragged());
                        pv.start(ctx, &mut app.audio, &m, play);
                    }
                    ui.label(widgets::dim(time).small());
                });
            }
            ui.horizontal(|ui| {
                let info = match m.kind {
                    MediaKind::Image => format!("Image · {}×{}", m.width, m.height),
                    MediaKind::Audio => format!("Audio · {}", widgets::format_time(len)),
                    MediaKind::Video => format!("Video · {}×{} · {:.2} fps{}", m.width, m.height, m.fps, if m.has_audio { " · with audio" } else { " · no sound" }),
                };
                ui.label(widgets::dim(info).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(m.path.exists(), Button::new("➕ Add to timeline")).clicked() {
                        add = true;
                    }
                });
            });
        });

    if !open || add {
        pv.stop();
        pv = MediaPreview::default();
    }
    app.media_preview = pv;
    if add {
        app.add_media_to_timeline(id);
    }
}
