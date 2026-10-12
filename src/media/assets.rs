//! Background import, thumbnails and waveforms.

use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{FilterPreset, Id, MediaItem};
use crate::render::filters;
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

const THUMB_W: u32 = 256;
const THUMB_H: u32 = 144;

enum AssetEvent {
    Probed { path: PathBuf, result: Result<ProbeInfo, String> },
    Thumbnail { media: Id, path: PathBuf, width: u32, height: u32, rgba: Vec<u8> },
    Waveform { media: Id, path: PathBuf, peaks: Vec<f32> },
}

struct RawThumb {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

pub struct AssetManager {
    ctx: egui::Context,
    tx: Sender<AssetEvent>,
    rx: Receiver<AssetEvent>,
    pub thumbs: HashMap<Id, TextureHandle>,
    raw_thumbs: HashMap<Id, RawThumb>,
    pub waveforms: HashMap<Id, Arc<Vec<f32>>>,
    filter_thumbs: HashMap<(Id, FilterPreset), TextureHandle>,
    requested: HashSet<Id>,
    /// Which file each cached id belongs to (ids repeat across projects).
    paths: HashMap<Id, PathBuf>,
    pub importing: usize,
}

impl AssetManager {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            ctx,
            tx,
            rx,
            thumbs: HashMap::new(),
            raw_thumbs: HashMap::new(),
            waveforms: HashMap::new(),
            filter_thumbs: HashMap::new(),
            requested: HashSet::new(),
            paths: HashMap::new(),
            importing: 0,
        }
    }

    /// Probes a file in the background; the result is returned from [`poll`](Self::poll).
    pub fn import(&mut self, path: PathBuf) {
        self.importing += 1;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let result = ffmpeg::probe(&path).map_err(|e| e.to_string());
            let _ = tx.send(AssetEvent::Probed { path, result });
            ctx.request_repaint();
        });
    }

    /// Makes sure thumbnail and waveform jobs have been started for a media item.
    pub fn ensure(&mut self, media: &MediaItem) {
        if self.paths.get(&media.id) != Some(&media.path) {
            // Same id, different file (another project): drop the stale cache.
            self.thumbs.remove(&media.id);
            self.raw_thumbs.remove(&media.id);
            self.waveforms.remove(&media.id);
            self.filter_thumbs.retain(|(m, _), _| *m != media.id);
            self.requested.remove(&media.id);
            self.paths.insert(media.id, media.path.clone());
        }
        if !self.requested.insert(media.id) || !media.path.exists() {
            return;
        }
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let m = media.clone();
        thread::spawn(move || {
            if m.has_video() {
                let (w, h) = ffmpeg::fit_within(m.width, m.height, THUMB_W, THUMB_H);
                let t = if m.duration > 0.0 { (m.duration * 0.1).min(1.0) } else { 0.0 };
                if let Ok(rgba) = ffmpeg::extract_frame(&m.path, t, w, h) {
                    let _ = tx.send(AssetEvent::Thumbnail { media: m.id, path: m.path.clone(), width: w, height: h, rgba });
                    ctx.request_repaint();
                }
            }
            if m.has_sound() {
                if let Ok(peaks) = ffmpeg::waveform(&m.path) {
                    let _ = tx.send(AssetEvent::Waveform { media: m.id, path: m.path.clone(), peaks });
                    ctx.request_repaint();
                }
            }
        });
    }

    /// Drains finished jobs; returns finished imports.
    pub fn poll(&mut self) -> Vec<(PathBuf, Result<ProbeInfo, String>)> {
        let mut imports = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AssetEvent::Probed { path, result } => {
                    self.importing = self.importing.saturating_sub(1);
                    imports.push((path, result));
                }
                AssetEvent::Thumbnail { media, path, .. } | AssetEvent::Waveform { media, path, .. } if self.paths.get(&media) != Some(&path) => {
                    // Finished for a file this id no longer refers to.
                }
                AssetEvent::Thumbnail { media, width, height, rgba, .. } => {
                    let img = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                    let tex = self.ctx.load_texture(format!("thumb-{media}"), img, TextureOptions::LINEAR);
                    self.thumbs.insert(media, tex);
                    self.raw_thumbs.insert(media, RawThumb { width, height, rgba });
                    self.filter_thumbs.retain(|(m, _), _| *m != media);
                }
                AssetEvent::Waveform { media, peaks, .. } => {
                    self.waveforms.insert(media, Arc::new(peaks));
                }
            }
        }
        imports
    }

    /// A thumbnail of `media` (or a built-in sample picture) with `preset` applied.
    pub fn filter_thumb(&mut self, media: Option<Id>, preset: FilterPreset) -> TextureId {
        let key_media = media.filter(|m| self.raw_thumbs.contains_key(m)).unwrap_or(0);
        let key = (key_media, preset);
        if let Some(t) = self.filter_thumbs.get(&key) {
            return t.id();
        }
        let (w, h, mut rgba) = match self.raw_thumbs.get(&key_media) {
            Some(r) => (r.width, r.height, r.rgba.clone()),
            None => sample_picture(),
        };
        filters::apply_cpu(&mut rgba, preset, &Default::default());
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let tex = self.ctx.load_texture(format!("filter-{key_media}-{preset:?}"), img, TextureOptions::LINEAR);
        let id = tex.id();
        self.filter_thumbs.insert(key, tex);
        id
    }
}

/// A colourful landscape-ish gradient used to preview filters when there is no media.
fn sample_picture() -> (u32, u32, Vec<u8>) {
    let (w, h) = (160u32, 90u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let (r, g, b) = if fy < 0.55 {
                // Sky: warm horizon to blue.
                let k = fy / 0.55;
                (255.0 * (1.0 - k) + 70.0 * k, 170.0 * (1.0 - k) + 140.0 * k, 90.0 * (1.0 - k) + 230.0 * k)
            } else {
                // Hills.
                let k = (fy - 0.55) / 0.45;
                let wave = ((fx * 9.0).sin() * 0.5 + 0.5) * 40.0;
                (40.0 + wave, 140.0 - 60.0 * k + wave, 60.0)
            };
            rgba.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    (w, h, rgba)
}
