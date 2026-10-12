//! Auto-reframe: keeps the subject in frame when the canvas ratio differs from the
//! footage (e.g. 16:9 → 9:16).
//!
//! 1. Sample small JPEG frames from the clip.
//! 2. A vision model (via OpenRouter) locates the subject in each frame, in batches;
//!    the first batch names the subject so later batches track the same one.
//! 3. The detections become a camera path: missing frames are filled, a zero-lag
//!    smoother and a deadzone remove jitter (a camera operator holds still until the
//!    subject drifts), and the path is simplified.
//! 4. The path is written as position keyframes plus a "fill" scale, so it previews,
//!    exports and can be fine-tuned like any other keyframes.

use super::openrouter::{jpeg_part, text_part, Client, Usage};
use crate::media::ffmpeg;
use crate::model::*;
use crate::render::{fill_scale, layer_size};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

const BATCH: usize = 8;
const PARALLEL: usize = 4;
const MAX_FRAMES: usize = 120;
const FRAME_PX: u32 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Wide,
    Medium,
    Tight,
}

impl Framing {
    pub const ALL: [Framing; 3] = [Framing::Wide, Framing::Medium, Framing::Tight];

    pub fn label(self) -> &'static str {
        match self {
            Framing::Wide => "Wide (fill)",
            Framing::Medium => "Medium",
            Framing::Tight => "Tight",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Framing::Wide => "wide",
            Framing::Medium => "medium",
            Framing::Tight => "tight",
        }
    }

    pub fn parse(s: &str) -> Option<Framing> {
        Framing::ALL.into_iter().find(|f| f.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Extra zoom on top of the scale that fills the canvas.
    pub fn zoom(self) -> f32 {
        match self {
            Framing::Wide => 1.0,
            Framing::Medium => 1.15,
            Framing::Tight => 1.35,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReframeOptions {
    /// What to follow ("the woman in the red jacket"); empty = let the model decide.
    pub subject: String,
    pub framing: Framing,
    /// 0 = follow tightly … 1 = very steady camera.
    pub smoothness: f32,
}

impl Default for ReframeOptions {
    fn default() -> Self {
        Self { subject: String::new(), framing: Framing::Wide, smoothness: 0.6 }
    }
}

/// Where the subject is in one sampled frame (fractions of the frame, 0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    /// Clip-local time (seconds).
    pub t: f64,
    pub center: Option<(f32, f32)>,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub subject: String,
    pub detections: Vec<Detection>,
    pub usage: Usage,
}

impl Analysis {
    pub fn found(&self) -> usize {
        self.detections.iter().filter(|d| d.center.is_some()).count()
    }
}

/// The camera path to write onto a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub scale: f32,
    pub x: Vec<Keyframe>,
    pub y: Vec<Keyframe>,
}

// ── sampling ─────────────────────────────────────────────────────────────────

pub fn sample_interval(duration: f64) -> f64 {
    (duration / 40.0).clamp(0.5, 2.0).max(duration / MAX_FRAMES as f64)
}

/// Splits an MJPEG byte stream into individual JPEG files.
pub fn split_jpegs(data: &[u8]) -> Vec<Vec<u8>> {
    // 0xFF 0xD8 0xFF can only appear at the start of an image (entropy-coded data
    // byte-stuffs 0xFF), so it is a safe frame boundary.
    let starts: Vec<usize> = data.windows(3).enumerate().filter(|(_, w)| w == &[0xFF, 0xD8, 0xFF]).map(|(i, _)| i).collect();
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| data[s..starts.get(k + 1).copied().unwrap_or(data.len())].to_vec())
        .filter(|f| f.len() > 100)
        .collect()
}

/// Small JPEG frames from a clip, with their clip-local times.
pub fn sample_frames(project: &Project, clip: &Clip) -> Result<Vec<(f64, Vec<u8>)>> {
    let media = clip.media_id().and_then(|m| project.media(m)).context("this clip has no media")?;
    if !media.has_video() {
        bail!("auto-reframe needs a video or image clip");
    }
    let scale = format!("scale={FRAME_PX}:{FRAME_PX}:force_original_aspect_ratio=decrease");
    let mut cmd = ffmpeg::command(&ffmpeg::ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    let interval = sample_interval(clip.duration);
    if media.kind == MediaKind::Image {
        cmd.arg("-i").arg(&media.path).args(["-frames:v", "1", "-vf", &scale]);
    } else {
        cmd.args(["-ss", &format!("{:.3}", clip.source_in), "-t", &format!("{:.3}", clip.duration * clip.speed)])
            .arg("-i")
            .arg(&media.path)
            .args(["-an", "-vf", &format!("fps=1/{:.4},{scale}", interval * clip.speed)]);
    }
    let out = cmd
        .args(["-c:v", "mjpeg", "-q:v", "5", "-f", "image2pipe", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("could not run ffmpeg")?;
    if !out.status.success() {
        bail!("could not read frames: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let frames: Vec<(f64, Vec<u8>)> = split_jpegs(&out.stdout)
        .into_iter()
        .enumerate()
        .map(|(k, jpg)| (k as f64 * interval, jpg))
        .filter(|(t, _)| *t <= clip.duration + 1e-6)
        .take(MAX_FRAMES)
        .collect();
    if frames.is_empty() {
        bail!("could not read any frames from this clip");
    }
    Ok(frames)
}

// ── detection ────────────────────────────────────────────────────────────────

const SYSTEM: &str = "You locate subjects in video frames for automatic reframing (cropping a video to a different \
aspect ratio). Coordinates are fractions of the frame: x from the left edge (0) to the right edge (1), y from the top \
edge (0) to the bottom edge (1). Be precise and consistent from frame to frame.";

fn instructions(subject: &str) -> String {
    let what = if subject.trim().is_empty() {
        "the main subject: whoever or whatever a viewer would watch, usually the person speaking or the most important \
         moving object. If there are several people, keep tracking the same one."
            .to_string()
    } else {
        format!("this subject: {}.", subject.trim())
    };
    format!(
        "Track {what}\nFor every frame below, report: t (copy the frame's t), found (false if the subject is not \
         visible), and the centre (cx, cy) and size (w, h) of a tight box around the subject's most important part \
         (for a person: head and shoulders). Also give `subject`: a short description of exactly what you tracked, so \
         later frames can follow the same one."
    )
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "subject": { "type": "string" },
            "frames": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "t": { "type": "number" },
                        "found": { "type": "boolean" },
                        "cx": { "type": "number" },
                        "cy": { "type": "number" },
                        "w": { "type": "number" },
                        "h": { "type": "number" }
                    },
                    "required": ["t", "found", "cx", "cy", "w", "h"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["subject", "frames"],
        "additionalProperties": false
    })
}

fn batch_messages(batch: &[(f64, Vec<u8>)], subject: &str) -> Value {
    let mut content = vec![text_part(instructions(subject))];
    for (t, jpg) in batch {
        content.push(text_part(format!("Frame t={t:.2}")));
        content.push(jpeg_part(jpg));
    }
    json!([{ "role": "system", "content": SYSTEM }, { "role": "user", "content": content }])
}

/// Matches the model's answers to the frames that were sent (by order, else by nearest t).
fn parse_batch(reply: &Value, batch: &[(f64, Vec<u8>)]) -> (String, Vec<Detection>) {
    let subject = reply["subject"].as_str().unwrap_or("").trim().to_string();
    let answers = reply["frames"].as_array().cloned().unwrap_or_default();
    let detection = |a: &Value, t: f64| {
        let found = a["found"].as_bool().unwrap_or(false);
        let cx = a["cx"].as_f64().unwrap_or(f64::NAN);
        let cy = a["cy"].as_f64().unwrap_or(f64::NAN);
        // Some models answer in 0..100 or 0..1000 despite instructions.
        let norm = |v: f64| if v > 1.5 && v <= 100.0 { v / 100.0 } else if v > 100.0 { v / 1000.0 } else { v };
        let (cx, cy) = (norm(cx), norm(cy));
        let center = (found && cx.is_finite() && cy.is_finite()).then(|| (cx.clamp(0.0, 1.0) as f32, cy.clamp(0.0, 1.0) as f32));
        Detection { t, center }
    };
    let dets = if answers.len() == batch.len() {
        batch.iter().zip(&answers).map(|((t, _), a)| detection(a, *t)).collect()
    } else {
        batch
            .iter()
            .map(|(t, _)| {
                answers
                    .iter()
                    .filter(|a| a["t"].as_f64().is_some_and(|at| (at - t).abs() < 0.3))
                    .min_by(|a, b| {
                        let d = |x: &Value| (x["t"].as_f64().unwrap_or(f64::MAX) - t).abs();
                        d(a).total_cmp(&d(b))
                    })
                    .map_or(Detection { t: *t, center: None }, |a| detection(a, *t))
            })
            .collect()
    };
    (subject, dets)
}

/// The function that sends one batch to the model: messages in, (JSON reply, usage) out.
pub type ChatFn<'a> = dyn Fn(Value) -> Result<(Value, Usage)> + Sync + 'a;

/// Locates the subject in every frame. Returns `Ok(None)` if cancelled.
pub fn analyze(
    frames: &[(f64, Vec<u8>)],
    subject_hint: &str,
    chat: &ChatFn<'_>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<Option<Analysis>> {
    let batches: Vec<&[(f64, Vec<u8>)]> = frames.chunks(BATCH).collect();
    let total = batches.len();
    let mut usage = Usage::default();
    progress(0.05, &format!("Finding the subject (1/{total})…"));
    let (reply, u) = chat(batch_messages(batches[0], subject_hint))?;
    usage += u;
    let (named, mut detections) = parse_batch(&reply, batches[0]);
    // Follow the user's description if given, else what the model said it tracked.
    let subject = if subject_hint.trim().is_empty() { named.clone() } else { subject_hint.trim().to_string() };

    let mut done = 1;
    for group in batches[1..].chunks(PARALLEL) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let results: Vec<Result<(Value, Usage)>> = std::thread::scope(|s| {
            let handles: Vec<_> = group.iter().map(|b| s.spawn(|| chat(batch_messages(b, &subject)))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("worker panicked")))).collect()
        });
        for (b, r) in group.iter().zip(results) {
            let (reply, u) = r?;
            usage += u;
            detections.extend(parse_batch(&reply, b).1);
        }
        done += group.len();
        progress(0.05 + 0.9 * done as f32 / total as f32, &format!("Tracking the subject ({done}/{total})…"));
    }
    detections.sort_by(|a, b| a.t.total_cmp(&b.t));
    Ok(Some(Analysis { subject: if named.is_empty() { subject } else { named }, detections, usage }))
}

/// Samples and analyses one clip with OpenRouter.
pub fn analyze_clip(
    project: &Project,
    clip_id: Id,
    subject_hint: &str,
    client: &Client,
    model: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<Option<Analysis>> {
    let clip = project.clip(clip_id).cloned().ok_or_else(|| anyhow!("no clip with id {clip_id}"))?;
    progress(0.0, "Reading frames…");
    let frames = sample_frames(project, &clip)?;
    let chat = |messages: Value| client.chat_json(model, messages, "subject_boxes", schema());
    analyze(&frames, subject_hint, &chat, cancel, progress)
}

// ── camera path ──────────────────────────────────────────────────────────────

/// True if the clip can actually move inside the canvas (ratios differ or zoomed in).
pub fn has_room(project: &Project, clip: &Clip, framing: Framing) -> bool {
    let Some(media) = clip.media_id().and_then(|m| project.media(m)) else { return false };
    let (cw, ch) = project.aspect.size_for(1080);
    let scale = fill_scale(media.width as f64, media.height as f64, cw as f64, ch as f64) * framing.zoom() as f64;
    let (lw, lh) = layer_size(media.width as f64, media.height as f64, cw as f64, ch as f64, scale);
    lw - cw as f64 > 2.0 || lh - ch as f64 > 2.0
}

/// Centred moving average (no lag in the middle). At the ends the window is
/// clamped to the samples available, so the first and last frames are averaged too.
fn smooth(values: &[f32], half_window: usize) -> Vec<f32> {
    let n = values.len();
    (0..n)
        .map(|i| {
            let w = &values[i.saturating_sub(half_window)..=(i + half_window).min(n - 1)];
            w.iter().sum::<f32>() / w.len() as f32
        })
        .collect()
}

/// The camera only moves once the target leaves a band around its current position.
fn deadzone(values: &[f32], dz: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(values.len());
    let mut cam = values.first().copied().unwrap_or(0.0);
    for &v in values {
        if v > cam + dz {
            cam = v - dz;
        } else if v < cam - dz {
            cam = v + dz;
        }
        out.push(cam);
    }
    out
}

/// Ramer–Douglas–Peucker simplification of (t, v) points.
fn simplify(points: &[(f64, f32)], eps: f32) -> Vec<(f64, f32)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let (mut worst, mut idx) = (0.0f32, 0);
    for (i, p) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let k = ((p.0 - a.0) / (b.0 - a.0).max(1e-9)) as f32;
        let d = (p.1 - (a.1 + (b.1 - a.1) * k)).abs();
        if d > worst {
            worst = d;
            idx = i;
        }
    }
    if worst <= eps {
        return vec![a, b];
    }
    let mut left = simplify(&points[..=idx], eps);
    left.pop();
    left.extend(simplify(&points[idx..], eps));
    left
}

/// Fills frames where the subject was not found by interpolating neighbours.
fn fill_missing(dets: &[Detection]) -> Option<Vec<(f64, (f32, f32))>> {
    let known: Vec<(f64, (f32, f32))> = dets.iter().filter_map(|d| d.center.map(|c| (d.t, c))).collect();
    if known.is_empty() {
        return None;
    }
    Some(
        dets.iter()
            .map(|d| {
                let c = d.center.unwrap_or_else(|| {
                    let prev = known.iter().rev().find(|k| k.0 <= d.t);
                    let next = known.iter().find(|k| k.0 >= d.t);
                    match (prev, next) {
                        (Some(p), Some(n)) if n.0 > p.0 => {
                            let k = ((d.t - p.0) / (n.0 - p.0)) as f32;
                            (p.1 .0 + (n.1 .0 - p.1 .0) * k, p.1 .1 + (n.1 .1 - p.1 .1) * k)
                        }
                        (Some(p), _) => p.1,
                        (_, Some(n)) => n.1,
                        _ => (0.5, 0.5),
                    }
                });
                (d.t, c)
            })
            .collect(),
    )
}

/// Turns detections into a camera path for a clip in a `canvas`-sized frame.
pub fn plan(media: (u32, u32), canvas: (u32, u32), duration: f64, dets: &[Detection], opts: &ReframeOptions) -> Result<Plan> {
    let (mw, mh) = (media.0 as f64, media.1 as f64);
    let (cw, ch) = (canvas.0 as f64, canvas.1 as f64);
    let samples = fill_missing(dets).ok_or_else(|| {
        anyhow!("the subject wasn't found in any frame — try describing it (e.g. \"the man in the blue shirt\")")
    })?;
    let scale = (fill_scale(mw, mh, cw, ch) as f32 * opts.framing.zoom()).max(0.01);
    let (lw, lh) = layer_size(mw, mh, cw, ch, scale as f64);
    // Limits that keep the layer covering the canvas (sub-pixel slack counts as none).
    let limit = |l: f64, c: f64| if l - c < 1.0 { 0.0 } else { ((l - c) / 2.0 / c) as f32 };
    let (max_x, max_y) = (limit(lw, cw), limit(lh, ch));
    // Offset that puts the subject's centre at the canvas centre, kept inside the frame.
    let xs: Vec<f32> =
        samples.iter().map(|(_, c)| (-(c.0 as f64 - 0.5) * lw / cw) as f32).map(|x| x.clamp(-max_x, max_x)).collect();
    let ys: Vec<f32> =
        samples.iter().map(|(_, c)| (-(c.1 as f64 - 0.5) * lh / ch) as f32).map(|y| y.clamp(-max_y, max_y)).collect();

    let s = opts.smoothness.clamp(0.0, 1.0);
    // Smoothness 0 follows exactly; 1 averages ~5 samples each side and ignores
    // drifts under 5% of the canvas.
    let half_window = (s * 5.0).round() as usize;
    let dz = 0.05 * s;
    let path = |vals: &[f32], limit: f32| -> Vec<Keyframe> {
        let v = deadzone(&smooth(vals, half_window), dz);
        let pts: Vec<(f64, f32)> = samples.iter().zip(&v).map(|((t, _), v)| (t.clamp(0.0, duration), v.clamp(-limit, limit))).collect();
        let simple = simplify(&pts, 0.003);
        if simple.iter().all(|p| (p.1 - simple[0].1).abs() < 0.002) {
            return vec![Keyframe { time: 0.0, value: simple[0].1, ease: Ease::Linear }];
        }
        let mut keys: Vec<Keyframe> = simple.iter().map(|&(t, v)| Keyframe { time: t, value: v, ease: Ease::Smooth }).collect();
        keys.dedup_by(|b, a| (b.time - a.time).abs() <= KEY_TOLERANCE);
        keys
    };
    Ok(Plan { scale, x: path(&xs, max_x), y: path(&ys, max_y) })
}

/// Writes a plan onto a clip: fill scale plus position keyframes (or static values).
pub fn apply_plan(clip: &mut Clip, plan: &Plan) {
    clip.keyframes.remove(&Prop::Scale);
    clip.transform.scale = plan.scale;
    for (prop, keys) in [(Prop::X, &plan.x), (Prop::Y, &plan.y)] {
        if keys.len() <= 1 {
            clip.keyframes.remove(&prop);
            prop.set_base(clip, keys.first().map_or(0.0, |k| k.value));
        } else {
            clip.keyframes.insert(prop, keys.clone());
        }
    }
}

/// Plans and applies an analysis to a clip in the project.
pub fn apply_analysis(project: &mut Project, clip_id: Id, analysis: &Analysis, opts: &ReframeOptions) -> Result<Plan> {
    let clip = project.clip(clip_id).cloned().ok_or_else(|| anyhow!("no clip with id {clip_id}"))?;
    let media = clip.media_id().and_then(|m| project.media(m)).context("clip has no media")?;
    let plan = plan((media.width, media.height), project.aspect.size_for(1080), clip.duration, &analysis.detections, opts)?;
    apply_plan(project.clip_mut(clip_id).expect("exists"), &plan);
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn det(t: f64, cx: f32) -> Detection {
        Detection { t, center: Some((cx, 0.4)) }
    }

    /// Runs against a real file when RUSTCUT_SAMPLE_VIDEO is set (FFmpeg required).
    #[test]
    fn samples_real_video_when_available() {
        let Ok(path) = std::env::var("RUSTCUT_SAMPLE_VIDEO") else { return };
        let info = ffmpeg::probe(std::path::Path::new(&path)).unwrap();
        let mut p = Project::default();
        let m = p.add_media(info.into_media_item(path.into()));
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.source_in = 1.0;
        c.duration = 5.0;
        let frames = sample_frames(&p, &c).unwrap();
        assert_eq!(frames.len(), 10, "0.5 s interval over 5 s (end exclusive)");
        assert!(frames.iter().all(|(_, j)| j.starts_with(&[0xFF, 0xD8, 0xFF]) && j.len() > 1000));
        assert_eq!(frames[3].0, 1.5);
    }

    #[test]
    fn splits_mjpeg() {
        let mut stream = Vec::new();
        for n in 0..3u8 {
            stream.extend([0xFF, 0xD8, 0xFF, 0xE0]);
            stream.extend(std::iter::repeat(n).take(200));
            stream.extend([0xFF, 0xD9]);
        }
        let frames = split_jpegs(&stream);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.starts_with(&[0xFF, 0xD8, 0xFF]) && f.ends_with(&[0xFF, 0xD9])));
    }

    #[test]
    fn interval_scales_with_length() {
        assert_eq!(sample_interval(5.0), 0.5);
        assert_eq!(sample_interval(60.0), 1.5);
        assert_eq!(sample_interval(600.0), 5.0); // capped at MAX_FRAMES
    }

    #[test]
    fn landscape_to_portrait_centres_the_subject() {
        // 1920×1080 into 1080×1920: fill scale makes the layer 3413 px wide.
        let opts = ReframeOptions { smoothness: 0.0, ..Default::default() };
        let p = plan((1920, 1080), (1080, 1920), 4.0, &[det(0.0, 0.75), det(4.0, 0.75)], &opts).unwrap();
        assert!((p.scale - 3.1605).abs() < 1e-3, "{}", p.scale);
        // Subject at 75% across → layer shifted left by 25% of its width.
        let lw = 1920.0 * (1080.0 / 1920.0) * p.scale as f64;
        let expect = (-(0.25) * lw / 1080.0) as f32;
        assert_eq!(p.x.len(), 1);
        assert!((p.x[0].value - expect).abs() < 0.02, "{:?} vs {expect}", p.x);
        // Height already fills the canvas: no vertical movement possible.
        assert_eq!(p.y[0].value, 0.0);
    }

    #[test]
    fn offsets_never_reveal_the_background() {
        let opts = ReframeOptions { smoothness: 0.0, ..Default::default() };
        let p = plan((1920, 1080), (1080, 1920), 2.0, &[det(0.0, 0.0), det(2.0, 1.0)], &opts).unwrap();
        let lw = 1920.0 * (1080.0 / 1920.0) * p.scale as f64;
        let max_x = ((lw - 1080.0) / 2.0 / 1080.0) as f32;
        assert!(p.x.iter().all(|k| k.value.abs() <= max_x + 1e-5), "{:?}", p.x);
        assert!((p.x.first().unwrap().value - max_x).abs() < 1e-4);
        assert!((p.x.last().unwrap().value + max_x).abs() < 1e-4);
    }

    #[test]
    fn jitter_is_smoothed_and_simplified() {
        let dets: Vec<Detection> =
            (0..40).map(|i| det(i as f64 * 0.5, 0.5 + if i % 2 == 0 { 0.02 } else { -0.02 })).collect();
        let p = plan((1920, 1080), (1080, 1920), 20.0, &dets, &ReframeOptions::default()).unwrap();
        // ±2% jitter inside the deadzone collapses to a steady shot.
        assert_eq!(p.x.len(), 1, "{:?}", p.x);
        // A real move is kept, with few keyframes.
        let dets: Vec<Detection> = (0..40).map(|i| det(i as f64 * 0.5, if i < 20 { 0.3 } else { 0.7 })).collect();
        let p = plan((1920, 1080), (1080, 1920), 20.0, &dets, &ReframeOptions::default()).unwrap();
        assert!(p.x.len() >= 2 && p.x.len() <= 8, "{:?}", p.x);
        assert!(p.x.first().unwrap().value > 0.0 && p.x.last().unwrap().value < 0.0);
    }

    #[test]
    fn missing_frames_are_filled_and_all_missing_is_an_error() {
        let dets = vec![det(0.0, 0.3), Detection { t: 1.0, center: None }, det(2.0, 0.7)];
        let filled = fill_missing(&dets).unwrap();
        assert!((filled[1].1 .0 - 0.5).abs() < 1e-6);
        let none = vec![Detection { t: 0.0, center: None }];
        assert!(plan((1920, 1080), (1080, 1920), 1.0, &none, &ReframeOptions::default()).is_err());
    }

    #[test]
    fn analyze_batches_and_tracks_one_subject() {
        let frames: Vec<(f64, Vec<u8>)> = (0..20).map(|i| (i as f64 * 0.5, vec![0xFF, 0xD8, 0xFF, i as u8])).collect();
        let seen_subjects = Mutex::new(Vec::new());
        let chat = |messages: Value| -> Result<(Value, Usage)> {
            let content = messages[1]["content"].as_array().unwrap().clone();
            seen_subjects.lock().unwrap().push(content[0]["text"].as_str().unwrap().to_string());
            let ts: Vec<f64> = content
                .iter()
                .filter_map(|p| p["text"].as_str()?.strip_prefix("Frame t=")?.parse().ok())
                .collect();
            let frames: Vec<Value> = ts
                .iter()
                .map(|t| json!({ "t": t, "found": *t != 1.0, "cx": 60.0, "cy": 0.4, "w": 0.2, "h": 0.3 }))
                .collect();
            Ok((json!({ "subject": "woman in a red jacket", "frames": frames }), Usage { prompt_tokens: 100, completion_tokens: 10, cost: 0.001 }))
        };
        let cancel = AtomicBool::new(false);
        let mut stages = Vec::new();
        let a = analyze(&frames, "", &chat, &cancel, &mut |_, s| stages.push(s.to_string())).unwrap().unwrap();
        assert_eq!(a.detections.len(), 20);
        assert_eq!(a.found(), 19);
        assert_eq!(a.subject, "woman in a red jacket");
        // 0..100 coordinates are normalised.
        assert!((a.detections[0].center.unwrap().0 - 0.6).abs() < 1e-6);
        assert!((a.usage.cost - 0.003).abs() < 1e-9); // 3 batches of 8
        // Later batches were told what to follow.
        let subjects = seen_subjects.lock().unwrap();
        assert!(subjects[1..].iter().all(|s| s.contains("woman in a red jacket")));
        assert!(stages.last().unwrap().contains("3/3"));
    }
}
