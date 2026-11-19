//! Face detection for lip-sync: a vision model (via OpenRouter) boxes every face in a few
//! frames of the shot, the user picks the one who sings or speaks (or marks it by hand),
//! and that face is followed from frame to frame. The box also drives the size check
//! ("the face is too small to sync well") and the quality recommendation.

use super::openrouter::{jpeg_part, text_part, Client, Usage};
use crate::model::{Clip, Id, Project};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// Frames sampled over the span (one request).
pub const MAX_FRAMES: usize = 12;
/// Below this face height (fraction of the frame) the sync barely reads.
pub const SMALL_FACE: f32 = 0.08;
/// Above this face height, recommend the pro lip-sync model and 1080p.
pub const CLOSE_UP: f32 = 0.25;

/// A box around a face: centre and size as fractions of the frame (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}

impl FaceBox {
    /// From two corner points (a manual drag), in fractions of the frame.
    pub fn from_corners(a: (f32, f32), b: (f32, f32)) -> Self {
        let (x0, x1) = (a.0.min(b.0).clamp(0.0, 1.0), a.0.max(b.0).clamp(0.0, 1.0));
        let (y0, y1) = (a.1.min(b.1).clamp(0.0, 1.0), a.1.max(b.1).clamp(0.0, 1.0));
        Self { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, w: x1 - x0, h: y1 - y0 }
    }

    pub fn left(&self) -> f32 {
        self.cx - self.w / 2.0
    }
    pub fn top(&self) -> f32 {
        self.cy - self.h / 2.0
    }
    pub fn right(&self) -> f32 {
        self.cx + self.w / 2.0
    }
    pub fn bottom(&self) -> f32 {
        self.cy + self.h / 2.0
    }

    /// The smallest box holding both.
    pub fn union(&self, o: &FaceBox) -> FaceBox {
        FaceBox::from_corners((self.left().min(o.left()), self.top().min(o.top())), (self.right().max(o.right()), self.bottom().max(o.bottom())))
    }

    fn distance(&self, o: &FaceBox) -> f32 {
        ((self.cx - o.cx).powi(2) + (self.cy - o.cy).powi(2)).sqrt() + (self.h - o.h).abs()
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left() && x <= self.right() && y >= self.top() && y <= self.bottom()
    }
}

/// The faces found in one sampled frame. `t` is seconds into the clip's source.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameFaces {
    pub t: f64,
    pub faces: Vec<FaceBox>,
}

#[derive(Clone, Debug, Default)]
pub struct Faces {
    pub frames: Vec<FrameFaces>,
    pub usage: Usage,
}

impl Faces {
    /// The frame with the most faces (ties: the middle one), for the picker.
    pub fn pick_frame(&self) -> Option<&FrameFaces> {
        let most = self.frames.iter().map(|f| f.faces.len()).max()?;
        let with: Vec<&FrameFaces> = self.frames.iter().filter(|f| f.faces.len() == most).collect();
        with.get(with.len() / 2).copied()
    }

    /// How many different people appear (the most faces seen in one frame).
    pub fn people(&self) -> usize {
        self.frames.iter().map(|f| f.faces.len()).max().unwrap_or(0)
    }
}

/// Warning when the face is too small for lip-sync to read.
pub fn size_hint(face_h: f32) -> Option<&'static str> {
    (face_h < SMALL_FACE).then_some("The face is small in this shot, so the lip-sync will barely show. A medium shot or close-up works best.")
}

/// Pro model and 1080p for close-ups, where the mouth fills enough pixels to show the difference.
pub fn recommend_pro(face_h: f32) -> bool {
    face_h >= CLOSE_UP
}

/// Follows the face nearest `chosen` through the frames (each frame's closest face to the
/// previous pick). Frames with no faces are skipped.
pub fn track(faces: &Faces, chosen: FaceBox) -> Vec<(f64, FaceBox)> {
    let mut last = chosen;
    let mut out = Vec::new();
    for f in &faces.frames {
        if let Some(b) = f.faces.iter().min_by(|a, b| a.distance(&last).total_cmp(&b.distance(&last))) {
            // A jump of more than half the frame is another person, not the same face moving.
            if b.distance(&last) < 0.5 {
                last = *b;
                out.push((f.t, *b));
            }
        }
    }
    out
}

/// One box covering the chosen face through the whole span (what gets cropped).
pub fn span_box(tracked: &[(f64, FaceBox)], chosen: FaceBox) -> FaceBox {
    tracked.iter().fold(chosen, |acc, (_, b)| acc.union(b))
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "frames": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "t": { "type": "number" },
                        "faces": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "cx": { "type": "number" }, "cy": { "type": "number" },
                                    "w": { "type": "number" }, "h": { "type": "number" }
                                },
                                "required": ["cx", "cy", "w", "h"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["t", "faces"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["frames"],
        "additionalProperties": false
    })
}

const INSTRUCTIONS: &str = "For every frame below, list every human face you can see. For each face give a tight box \
around the face (forehead to chin, ear to ear): its centre (cx, cy) and size (w, h) as fractions of the frame width and \
height, from 0 to 1, with (0, 0) at the top-left. Copy each frame's t. A frame with no faces gets an empty list.";

/// Some models answer in 0–100 or 0–1000 despite the instructions.
fn norm(v: f64) -> f32 {
    let v = if v > 100.0 { v / 1000.0 } else if v > 1.5 { v / 100.0 } else { v };
    v.clamp(0.0, 1.0) as f32
}

/// Matches the reply to the frames sent (by order, else by nearest t).
pub fn parse(reply: &Value, times: &[f64]) -> Vec<FrameFaces> {
    let answers = reply["frames"].as_array().cloned().unwrap_or_default();
    let boxes = |a: &Value| -> Vec<FaceBox> {
        a["faces"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| {
                        let g = |k: &str| f[k].as_f64().filter(|v| v.is_finite());
                        let b = FaceBox { cx: norm(g("cx")?), cy: norm(g("cy")?), w: norm(g("w")?), h: norm(g("h")?) };
                        (b.w > 0.005 && b.h > 0.005).then_some(b)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    times
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let a = if answers.len() == times.len() {
                answers.get(i)
            } else {
                answers.iter().filter(|a| a["t"].as_f64().is_some_and(|at| (at - t).abs() < 0.3)).min_by(|a, b| {
                    let d = |x: &Value| (x["t"].as_f64().unwrap_or(f64::MAX) - t).abs();
                    d(a).total_cmp(&d(b))
                })
            };
            FrameFaces { t, faces: a.map(boxes).unwrap_or_default() }
        })
        .collect()
}

/// Source times to sample over `[a, b]` of the timeline, for `clip`.
pub fn sample_times(clip: &Clip, a: f64, b: f64) -> Vec<f64> {
    let (a, b) = (a.max(clip.start), b.min(clip.end()));
    let len = (b - a).max(0.0);
    let n = ((len / 0.5).ceil() as usize).clamp(1, MAX_FRAMES);
    (0..n).map(|i| clip.source_in + (a - clip.start + len * (i as f64 + 0.5) / n as f64) * clip.speed).collect()
}

/// Samples frames of `clip_id` over the timeline span `[a, b]`.
pub fn sample(project: &Project, clip_id: Id, a: f64, b: f64) -> Result<Vec<(f64, Vec<u8>)>> {
    let clip = project.clip(clip_id).ok_or_else(|| anyhow!("no clip {clip_id}"))?;
    let media = clip.media_id().and_then(|m| project.media(m)).context("this clip has no media")?;
    if !media.has_video() {
        return Err(anyhow!("pick a video or image clip"));
    }
    let times = if media.kind == crate::model::MediaKind::Image { vec![0.0] } else { sample_times(clip, a, b) };
    times.into_iter().map(|t| Ok((t, crate::ai::video_gen::frame_jpeg(&media.path, t, 768)?))).collect()
}

/// Boxes every face in the sampled frames with one vision-model request (about a cent).
pub fn detect(frames: &[(f64, Vec<u8>)], client: &Client, model: &str, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Faces>> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    progress(0.2, "Finding faces…");
    let mut content = vec![text_part(INSTRUCTIONS.to_string())];
    for (t, jpg) in frames {
        content.push(text_part(format!("Frame t={t:.2}")));
        content.push(jpeg_part(jpg));
    }
    let messages = json!([{ "role": "user", "content": content }]);
    let (reply, usage) = client.chat_json(model, messages, "faces", schema())?;
    let times: Vec<f64> = frames.iter().map(|f| f.0).collect();
    progress(1.0, "Done");
    Ok(Some(Faces { frames: parse(&reply, &times), usage }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises_boxes() {
        let reply = json!({ "frames": [
            { "t": 1.0, "faces": [{ "cx": 0.5, "cy": 0.3, "w": 0.1, "h": 0.2 }] },
            { "t": 1.5, "faces": [{ "cx": 50, "cy": 30, "w": 10, "h": 20 }, { "cx": 800, "cy": 300, "w": 100, "h": 200 }] },
            { "t": 2.0, "faces": [] }
        ]});
        let f = parse(&reply, &[1.0, 1.5, 2.0]);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].faces[0], FaceBox { cx: 0.5, cy: 0.3, w: 0.1, h: 0.2 });
        assert_eq!(f[1].faces[0], f[0].faces[0], "0–100 answers are scaled");
        assert!((f[1].faces[1].cx - 0.8).abs() < 1e-6, "0–1000 answers are scaled");
        assert!(f[2].faces.is_empty());
        let faces = Faces { frames: f, usage: Usage::default() };
        assert_eq!(faces.people(), 2);
        assert_eq!(faces.pick_frame().unwrap().t, 1.5);
    }

    #[test]
    fn follows_the_chosen_face_not_the_other_one() {
        let left = |cx: f32| FaceBox { cx, cy: 0.4, w: 0.1, h: 0.2 };
        let right = |cx: f32| FaceBox { cx, cy: 0.4, w: 0.1, h: 0.2 };
        let faces = Faces {
            frames: vec![
                FrameFaces { t: 0.0, faces: vec![left(0.25), right(0.75)] },
                FrameFaces { t: 0.5, faces: vec![right(0.72), left(0.28)] },
                FrameFaces { t: 1.0, faces: vec![left(0.31)] },
                FrameFaces { t: 1.5, faces: vec![right(0.9)] },
            ],
            usage: Usage::default(),
        };
        let t = track(&faces, left(0.25));
        assert_eq!(t.iter().map(|(_, b)| b.cx).collect::<Vec<_>>(), vec![0.25, 0.28, 0.31], "the right-hand singer alone is skipped");
        let span = span_box(&t, left(0.25));
        assert!((span.left() - 0.20).abs() < 1e-6 && (span.right() - 0.36).abs() < 1e-6);
    }

    #[test]
    fn size_thresholds_and_manual_box() {
        assert!(size_hint(0.05).is_some());
        assert!(size_hint(0.12).is_none());
        assert!(recommend_pro(0.3) && !recommend_pro(0.15));
        let b = FaceBox::from_corners((0.6, 0.5), (0.4, 0.1));
        assert!((b.cx - 0.5).abs() < 1e-6 && (b.cy - 0.3).abs() < 1e-6 && (b.w - 0.2).abs() < 1e-6 && (b.h - 0.4).abs() < 1e-6);
        assert!(b.contains(0.5, 0.3) && !b.contains(0.7, 0.3));
    }

    #[test]
    fn samples_inside_the_span() {
        let mut c = Clip::new(1, crate::model::ClipSource::Media(1), 10.0, 6.0);
        c.source_in = 2.0;
        let t = sample_times(&c, 12.0, 14.0);
        assert_eq!(t.len(), 4);
        assert!(t.iter().all(|&s| s > 4.0 && s < 6.0), "{t:?}");
        assert_eq!(sample_times(&c, 0.0, 100.0).len(), MAX_FRAMES);
    }
}
