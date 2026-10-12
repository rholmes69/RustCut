//! Real-time preview: keeps a decoder per visible clip and turns frames into textures.

pub mod audio;

use crate::media::decoder::{Frame, VideoStream};
use crate::media::ffmpeg;
use crate::model::{Adjust, Clip, FilterPreset, Id, MediaItem, MediaKind, Project, TrackKind};
use crate::render::filters;
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const PREVIEW_FPS: f64 = 30.0;
const PREVIEW_MAX: u32 = 960;
const PREWARM_SECS: f64 = 0.75;

#[derive(Clone)]
struct RawFrame {
    width: u32,
    height: u32,
    data: Arc<Vec<u8>>,
}

enum ImageState {
    Loading,
    Ready(RawFrame),
    Failed,
}

/// Identifies the source range a decoder was started for; any change restarts it.
#[derive(Clone, Copy, PartialEq)]
struct StreamKey {
    media: Id,
    source_in: u64,
    speed: u64,
    start: u64,
}

impl StreamKey {
    fn of(clip: &Clip, media: Id) -> Self {
        Self {
            media,
            source_in: clip.source_in.to_bits(),
            speed: clip.speed.to_bits(),
            start: clip.start.to_bits(),
        }
    }
}

struct Slot {
    key: StreamKey,
    stream: Option<VideoStream>,
    stream_start: f64,
    last_rx: Option<f64>,
    pending: Option<Frame>,
    raw: Option<RawFrame>,
    shown_time: Option<f64>,
    dirty: bool,
    texture: Option<TextureHandle>,
    look: Option<(FilterPreset, Adjust)>,
    last_spawn: Option<Instant>,
}

impl Slot {
    fn new(key: StreamKey) -> Self {
        Self {
            key,
            stream: None,
            stream_start: 0.0,
            last_rx: None,
            pending: None,
            raw: None,
            shown_time: None,
            dirty: false,
            texture: None,
            look: None,
            last_spawn: None,
        }
    }

    /// New source range: drop the decoder but keep the texture to avoid flicker.
    fn reset(&mut self, key: StreamKey) {
        let texture = self.texture.take();
        *self = Slot::new(key);
        self.texture = texture;
    }
}

/// Texture key for a clip's background plate.
pub fn plate_key(clip: Id) -> Id {
    clip | (1 << 62)
}

pub struct PreviewEngine {
    ctx: egui::Context,
    slots: HashMap<Id, Slot>,
    images: HashMap<Id, ImageState>,
    img_tx: Sender<(Id, Option<RawFrame>)>,
    img_rx: Receiver<(Id, Option<RawFrame>)>,
}

impl PreviewEngine {
    pub fn new(ctx: egui::Context) -> Self {
        let (img_tx, img_rx) = channel();
        Self { ctx, slots: HashMap::new(), images: HashMap::new(), img_tx, img_rx }
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.images.clear();
    }

    /// Advances decoders to time `t`; returns textures for every visible visual clip.
    pub fn update(&mut self, project: &Project, t: f64, playing: bool) -> HashMap<Id, TextureId> {
        while let Ok((id, frame)) = self.img_rx.try_recv() {
            self.images.insert(id, frame.map_or(ImageState::Failed, ImageState::Ready));
        }
        let mut wanted = HashSet::new();
        let mut out = HashMap::new();
        for (ti, track) in project.tracks.iter().enumerate() {
            if track.kind != TrackKind::Video || track.hidden {
                continue;
            }
            let transitions = project.track_transitions(ti);
            for clip in &track.clips {
                let Some(media) = clip.media_id().and_then(|m| project.media(m)) else { continue };
                if !media.has_video() {
                    continue;
                }
                // Transitions show a clip a little before its start / after its end.
                let lo = transitions.iter().find(|tr| tr.to == clip.id).map_or(clip.start, |tr| tr.start());
                let hi = transitions.iter().find(|tr| tr.from == clip.id).map_or(clip.end(), |tr| tr.end());
                let visible = t >= lo && t < hi;
                let upcoming = playing && media.kind == MediaKind::Video && lo > t && lo - t < PREWARM_SECS;
                if !visible && !upcoming {
                    continue;
                }
                wanted.insert(clip.id);
                if let Some(plate) = clip.background_plate.and_then(|m| project.media(m)) {
                    let key = plate_key(clip.id);
                    wanted.insert(key);
                    self.update_image(key, clip, plate);
                    if visible {
                        if let Some(tex) = self.slots.get(&key).and_then(|s| s.texture.as_ref()) {
                            out.insert(key, tex.id());
                        }
                    }
                }
                match media.kind {
                    MediaKind::Image => self.update_image(clip.id, clip, media),
                    _ => self.update_video(clip, media, t, playing, (lo, hi)),
                }
                if visible {
                    if let Some(tex) = self.slots.get(&clip.id).and_then(|s| s.texture.as_ref()) {
                        out.insert(clip.id, tex.id());
                    }
                }
            }
        }
        self.slots.retain(|id, _| wanted.contains(id));
        out
    }

    fn update_video(&mut self, clip: &Clip, media: &MediaItem, t: f64, playing: bool, range: (f64, f64)) {
        let key = StreamKey::of(clip, media.id);
        let slot = self.slots.entry(clip.id).or_insert_with(|| Slot::new(key));
        if slot.key != key {
            slot.reset(key);
        }
        let dt = 1.0 / PREVIEW_FPS;
        let target = t.max(range.0).min(range.1 - 1e-3);
        // May reach outside the clip (transition handles); clamp to the source.
        let source_time = (clip.source_in + (target - clip.start) * clip.speed).clamp(0.0, (media.duration - 0.05).max(0.0));

        let restart = match slot.stream {
            None => true,
            Some(_) => {
                let behind = target < slot.stream_start - 1e-6
                    || slot.shown_time.is_some_and(|s| target < s - dt * 0.5);
                let far_ahead = target > slot.last_rx.unwrap_or(slot.stream_start) + 1.0;
                behind || far_ahead
            }
        };
        if restart {
            // While scrubbing, don't spawn a new decoder for every mouse move.
            let throttled = !playing && slot.last_spawn.is_some_and(|i| i.elapsed() < Duration::from_millis(60));
            if throttled {
                self.ctx.request_repaint_after(Duration::from_millis(60));
            } else {
                let (w, h) = ffmpeg::fit_within(media.width, media.height, PREVIEW_MAX, PREVIEW_MAX);
                slot.stream = VideoStream::spawn(
                    &self.ctx,
                    &media.path,
                    source_time,
                    clip.speed,
                    target,
                    w,
                    h,
                    PREVIEW_FPS,
                )
                .ok();
                slot.stream_start = target;
                slot.last_rx = None;
                slot.pending = None;
                slot.shown_time = None;
                slot.last_spawn = Some(Instant::now());
            }
        }

        let Slot { stream, pending, last_rx, raw, shown_time, dirty, .. } = slot;
        if let Some(stream) = stream.as_ref() {
            let mut latest = None;
            loop {
                let f = match pending.take() {
                    Some(f) => f,
                    None => match stream.rx.try_recv() {
                        Ok(f) => {
                            *last_rx = Some(f.time);
                            f
                        }
                        Err(_) => break,
                    },
                };
                if f.time <= target + dt * 0.5 {
                    latest = Some(f);
                } else {
                    *pending = Some(f);
                    break;
                }
            }
            if let Some(f) = latest {
                *raw = Some(RawFrame { width: stream.width, height: stream.height, data: Arc::new(f.data) });
                *shown_time = Some(f.time);
                *dirty = true;
            }
        }
        Self::upload(&self.ctx, clip, slot);
    }

    fn update_image(&mut self, slot_id: Id, clip: &Clip, media: &MediaItem) {
        let tx = self.img_tx.clone();
        let ctx = self.ctx.clone();
        let state = self.images.entry(media.id).or_insert_with(|| {
            let path = media.path.clone();
            let id = media.id;
            let (w, h) = ffmpeg::fit_within(media.width, media.height, 1920, 1920);
            thread::spawn(move || {
                let frame = ffmpeg::extract_frame(&path, 0.0, w, h)
                    .ok()
                    .map(|data| RawFrame { width: w, height: h, data: Arc::new(data) });
                let _ = tx.send((id, frame));
                ctx.request_repaint();
            });
            ImageState::Loading
        });
        let frame = match state {
            ImageState::Ready(f) => Some(f.clone()),
            ImageState::Loading | ImageState::Failed => None,
        };
        let key = StreamKey { media: media.id, source_in: 0, speed: 0, start: 0 };
        let slot = self.slots.entry(slot_id).or_insert_with(|| Slot::new(key));
        if slot.key != key {
            slot.reset(key);
        }
        if slot.raw.is_none() {
            if let Some(f) = frame {
                slot.raw = Some(f);
                slot.dirty = true;
            }
        }
        Self::upload(&self.ctx, clip, slot);
    }

    fn upload(ctx: &egui::Context, clip: &Clip, slot: &mut Slot) {
        let Some(raw) = &slot.raw else { return };
        let look = (clip.filter, clip.adjust);
        if !slot.dirty && slot.look == Some(look) {
            return;
        }
        let size = [raw.width as usize, raw.height as usize];
        let image = if filters::is_identity(look.0, &look.1) {
            ColorImage::from_rgba_unmultiplied(size, &raw.data)
        } else {
            let mut data = (*raw.data).clone();
            filters::apply_cpu(&mut data, look.0, &look.1);
            ColorImage::from_rgba_unmultiplied(size, &data)
        };
        match &mut slot.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => slot.texture = Some(ctx.load_texture(format!("layer-{}", clip.id), image, TextureOptions::LINEAR)),
        }
        slot.dirty = false;
        slot.look = Some(look);
    }
}
