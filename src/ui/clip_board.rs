//! Timeline storyboard: every video and image clip on the timeline as a numbered tile in
//! play order, with a frame from the middle of the cut, its time, length and lyric line.
//! Click a tile to jump there; double-click (or 👁) to preview just that clip.

use crate::app::EditorApp;
use crate::media::ffmpeg;
use crate::model::{Clip, Id, MediaKind, Project, TrackKind};
use crate::ui::{theme, widgets};
use egui::{pos2, vec2, Align2, Color32, ColorImage, Context, FontId, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

const THUMB_W: u32 = 320;
const THUMB_H: u32 = 180;

/// A frame of a file: (media id, source time in ms).
type Key = (Id, i64);

struct Job {
    key: Key,
    path: PathBuf,
    time: f64,
    w: u32,
    h: u32,
}

#[derive(Default)]
pub struct ClipBoard {
    pub open: bool,
    main_only: bool,
    tile_w: f32,
    thumbs: HashMap<Key, TextureHandle>,
    requested: HashSet<Key>,
    worker: Option<(Sender<Job>, Receiver<(Key, u32, u32, Option<Vec<u8>>)>)>,
}

/// One tile: a visual clip in play order.
pub struct Entry {
    pub clip: Clip,
    pub track: String,
    pub main: bool,
    pub key: Key,
    pub lyric: String,
}

/// Every video/image clip on the (visible) video tracks, sorted by start time.
pub fn entries(project: &Project, main_only: bool) -> Vec<Entry> {
    let main = project.tracks.iter().rposition(|t| t.kind == TrackKind::Video);
    let mut out: Vec<Entry> = Vec::new();
    for (ti, t) in project.tracks.iter().enumerate() {
        if t.kind != TrackKind::Video || t.hidden || (main_only && Some(ti) != main) {
            continue;
        }
        for c in &t.clips {
            let Some(m) = c.media_id().and_then(|id| project.media(id)) else { continue };
            if m.kind == MediaKind::Audio {
                continue;
            }
            let time = if m.kind == MediaKind::Image { 0.0 } else { c.source_in + c.duration * c.speed / 2.0 };
            out.push(Entry { clip: c.clone(), track: t.name.clone(), main: Some(ti) == main, key: (m.id, (time * 1000.0).round() as i64), lyric: lyric_at(project, c.start + c.duration / 2.0) });
        }
    }
    out.sort_by(|a, b| a.clip.start.total_cmp(&b.clip.start).then(b.main.cmp(&a.main)));
    out
}

/// The text (lyric/caption) on screen at `t`, if any.
fn lyric_at(project: &Project, t: f64) -> String {
    project
        .tracks
        .iter()
        .filter(|tr| tr.kind == TrackKind::Text)
        .flat_map(|tr| tr.clips.iter())
        .filter(|c| c.start <= t && c.end() > t)
        .find_map(|c| c.text().map(|s| s.content.trim().replace('\n', " ")).filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

impl ClipBoard {
    fn request(&mut self, ctx: &Context, project: &Project, key: Key) {
        if self.thumbs.contains_key(&key) || !self.requested.insert(key) {
            return;
        }
        let Some(m) = project.media(key.0) else { return };
        if self.worker.is_none() {
            // One background thread decodes thumbnails in order, so a long timeline
            // never starts dozens of FFmpeg processes at once.
            let (job_tx, job_rx) = channel::<Job>();
            let (done_tx, done_rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                for j in job_rx {
                    let data = ffmpeg::extract_frame(&j.path, j.time, j.w, j.h).ok();
                    if done_tx.send((j.key, j.w, j.h, data)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            self.worker = Some((job_tx, done_rx));
        }
        let (w, h) = ffmpeg::fit_within(m.width, m.height, THUMB_W, THUMB_H);
        if let Some((tx, _)) = &self.worker {
            let _ = tx.send(Job { key, path: m.path.clone(), time: key.1 as f64 / 1000.0, w, h });
        }
    }

    fn poll(&mut self, ctx: &Context) {
        let Some((_, rx)) = &self.worker else { return };
        let done: Vec<_> = rx.try_iter().collect();
        for (key, w, h, data) in done {
            if let Some(data) = data {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
                self.thumbs.insert(key, ctx.load_texture(format!("board-{}-{}", key.0, key.1), img, TextureOptions::LINEAR));
            }
        }
    }
}

enum Act {
    Jump(Id),
    Preview(Id, String),
    Reshoot(Id),
    Replace(Id),
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.clip_board.open {
        return;
    }
    let mut board = std::mem::take(&mut app.clip_board);
    if board.tile_w < 100.0 {
        board.tile_w = 190.0;
    }
    board.poll(ctx);
    let list = entries(&app.project, board.main_only);
    let now = app.playhead;
    let mut act = None;
    let mut open = true;
    egui::Window::new(RichText::new("🎞 Timeline storyboard").strong())
        .open(&mut open)
        .resizable(true)
        .default_size([980.0, 640.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let total: f64 = list.iter().filter(|e| e.main).map(|e| e.clip.duration).sum();
                ui.label(format!("{} clips · {}", list.len(), widgets::format_time(total)));
                ui.separator();
                ui.checkbox(&mut board.main_only, "Main track only");
                ui.separator();
                ui.label(widgets::dim("Size").small());
                ui.add(egui::Slider::new(&mut board.tile_w, 120.0..=360.0).show_value(false));
            });
            ui.label(widgets::dim("Click a clip to jump to it · double-click or 👁 to preview it · right-click for more").small());
            ui.separator();
            if list.is_empty() {
                ui.add_space(20.0);
                ui.label(widgets::dim("No video or image clips on the timeline yet."));
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 12.0);
                    for (i, e) in list.iter().enumerate() {
                        board.request(ctx, &app.project, e.key);
                        let name = app.project.media(e.key.0).map(|m| m.name.clone()).unwrap_or_default();
                        let title = format!("Clip {} · {}", i + 1, widgets::format_time(e.clip.start));
                        let current = e.clip.start <= now && e.clip.end() > now && e.main;
                        let selected = app.selection == Some(e.clip.id);
                        let resp = tile(ui, &board, e, i + 1, &name, board.tile_w, current, selected);
                        let eye = Rect::from_center_size(resp.rect.left_top() + vec2(board.tile_w - 16.0, 16.0), vec2(24.0, 24.0));
                        if resp.double_clicked() || (resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| eye.contains(p))) {
                            act = Some(Act::Preview(e.clip.id, title.clone()));
                        } else if resp.clicked() {
                            act = Some(Act::Jump(e.clip.id));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("👁 Preview").clicked() {
                                act = Some(Act::Preview(e.clip.id, title.clone()));
                                ui.close_menu();
                            }
                            if ui.button("📍 Show in timeline").clicked() {
                                act = Some(Act::Jump(e.clip.id));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("🔁 Replace clip…").clicked() {
                                act = Some(Act::Replace(e.clip.id));
                                ui.close_menu();
                            }
                            if ui.button("🎬 Reshoot this clip (AI)…").clicked() {
                                act = Some(Act::Reshoot(e.clip.id));
                                ui.close_menu();
                            }
                        });
                    }
                });
            });
        });
    board.open = open;
    app.clip_board = board;
    match act {
        Some(Act::Jump(id)) => crate::ui::media_preview::show_in_timeline(app, id),
        Some(Act::Preview(id, title)) => crate::ui::media_preview::open_clip(ctx, app, id, title),
        Some(Act::Replace(id)) => crate::ui::replace::open(app, id),
        Some(Act::Reshoot(id)) => crate::ui::reshoot::open(app, id),
        None => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn tile(ui: &mut egui::Ui, board: &ClipBoard, e: &Entry, n: usize, name: &str, w: f32, current: bool, selected: bool) -> egui::Response {
    let th = (w * 9.0 / 16.0).round();
    let lines = if e.lyric.is_empty() { 2.0 } else { 3.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, th + 6.0 + 16.0 * lines), Sense::click());
    let painter = ui.painter_at(rect);
    let thumb = Rect::from_min_size(rect.min, vec2(w, th));
    painter.rect_filled(thumb, 5.0, Color32::BLACK);
    match board.thumbs.get(&e.key) {
        Some(tex) => {
            // Fit (letterbox) so the whole frame shows.
            let ts = tex.size_vec2();
            let s = (thumb.width() / ts.x).min(thumb.height() / ts.y);
            let r = Rect::from_center_size(thumb.center(), ts * s);
            painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.text(thumb.center(), Align2::CENTER_CENTER, "…", FontId::proportional(20.0), theme::TEXT_DIM);
        }
    }
    // Number badge and length.
    let badge = painter.layout_no_wrap(format!("{n}"), FontId::proportional(13.0), Color32::BLACK);
    let br = Rect::from_min_size(thumb.left_top() + vec2(6.0, 6.0), badge.size() + vec2(10.0, 4.0));
    painter.rect_filled(br, 4.0, theme::ACCENT);
    painter.galley(br.min + vec2(5.0, 2.0), badge, Color32::BLACK);
    let len = painter.layout_no_wrap(format!("{:.1} s", e.clip.duration), FontId::proportional(11.0), Color32::WHITE);
    let lr = Rect::from_min_size(thumb.right_bottom() - len.size() - vec2(12.0, 8.0), len.size() + vec2(6.0, 3.0));
    painter.rect_filled(lr, 3.0, Color32::from_black_alpha(180));
    painter.galley(lr.min + vec2(3.0, 1.5), len, Color32::WHITE);
    if resp.hovered() {
        let eye = thumb.left_top() + vec2(w - 16.0, 16.0);
        painter.circle_filled(eye, 12.0, theme::ACCENT);
        painter.text(eye, Align2::CENTER_CENTER, "👁", FontId::proportional(13.0), Color32::BLACK);
    }
    let stroke = if selected {
        Stroke::new(2.5_f32, theme::ACCENT)
    } else if current {
        Stroke::new(2.0_f32, Color32::WHITE)
    } else if resp.hovered() {
        Stroke::new(1.0_f32, theme::TEXT_DIM)
    } else {
        Stroke::new(1.0_f32, theme::BORDER)
    };
    painter.rect_stroke(thumb, 5.0, stroke);

    let text = painter.with_clip_rect(Rect::from_min_max(pos2(rect.left(), thumb.bottom()), rect.right_bottom()));
    let mut y = thumb.bottom() + 4.0;
    let when = if e.main { widgets::format_time(e.clip.start) } else { format!("{} · {}", widgets::format_time(e.clip.start), e.track) };
    text.text(pos2(rect.left() + 1.0, y), Align2::LEFT_TOP, when, FontId::proportional(12.5), theme::TEXT);
    y += 16.0;
    text.text(pos2(rect.left() + 1.0, y), Align2::LEFT_TOP, name, FontId::proportional(11.0), theme::TEXT_DIM);
    if !e.lyric.is_empty() {
        y += 16.0;
        text.text(pos2(rect.left() + 1.0, y), Align2::LEFT_TOP, format!("“{}”", e.lyric), FontId::proportional(11.0), theme::ACCENT);
    }
    resp.on_hover_text(format!("{name}\n{} – {} ({:.2} s){}", widgets::format_time(e.clip.start), widgets::format_time(e.clip.end()), e.clip.duration, if e.lyric.is_empty() { String::new() } else { format!("\n“{}”", e.lyric) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, TextStyle};

    #[test]
    fn lists_visual_clips_in_play_order_with_lyrics() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "a.mp4".into(), name: "a.mp4".into(), kind: MediaKind::Video, duration: 10.0, width: 1920, height: 1080, fps: 30.0, has_audio: false });
        let main = p.main_track();
        for (start, src) in [(4.0, 6.0), (0.0, 2.0)] {
            let mut c = p.new_media_clip(v, start).unwrap();
            c.duration = 4.0;
            c.source_in = src;
            p.tracks[main].clips.push(c);
        }
        let ti = p.insert_track(TrackKind::Text, None);
        let mut t = p.new_text_clip(TextStyle { content: "Thought I knew you".into(), ..Default::default() }, 5.0);
        t.duration = 2.0;
        p.tracks[ti].clips.push(t);
        let list = entries(&p, false);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].clip.start, 0.0);
        assert_eq!(list[0].key, (v, 4000), "a frame from the middle of the cut");
        assert_eq!(list[1].lyric, "Thought I knew you");
        assert!(list[0].lyric.is_empty());
    }
}
