//! Lip-sync Studio: one flow from a shot (a timeline clip, a video file or a still) and a
//! voice (a timeline clip, an audio file or a text-to-speech line) to a lip-synced clip on
//! the timeline, with a recommended method and a cost estimate before anything is spent.
//!
//! Methods:
//! - **Fix the mouth** — Sync lipsync-2 on the existing footage (`lipsync`).
//! - **Performance** — Seedance 2.0 re-generates the shot from a still and the song (`performance`).
//! - **Animate + sync** — an image-to-video model animates the still with a steady face
//!   (`video_gen`, on OpenRouter), then lipsync-2-pro drives the mouth. The fallback when
//!   Seedance refuses a likeness, and the cheaper way to cover long spans from a still.
//!
//! Sources and voices that aren't on the timeline yet are placed first ([`place_source`],
//! [`place_voice`]), so every method works on ordinary clips.

use crate::ai::faces::FaceBox;
use crate::ai::lipsync::{self, Placement, Quality};
use crate::ai::{performance, video_gen};
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{AudioRole, Id, MediaKind, Project, TrackKind};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The default image-to-video model for Animate + sync (the user's pick on Bond Money v2).
pub const DEFAULT_VIDEO_MODEL: &str = "alibaba/wan-3.0";
/// The lip-sync model used after animating (sharper teeth and lips on big mouths).
pub const ANIMATE_LIPSYNC_MODEL: &str = "lipsync-2-pro";
/// How far into the animated clip the synced span starts: the first second is often a
/// still-looking settle.
pub const ANIMATE_LEAD_IN: f64 = 1.0;

/// (id, label, approximate USD per generated second, min seconds, max seconds).
pub const ANIMATE_MODELS: &[(&str, &str, f64, u32, u32)] = &[
    ("alibaba/wan-3.0", "Wan 3.0 (most singing, steady face)", 0.06, 4, 15),
    ("minimax/hailuo-3-max", "Hailuo 3 Max (steadiest face, least movement)", 0.08, 6, 10),
    ("kwaivgi/kling-v3.0-std", "Kling v3.0 Std", 0.085, 5, 15),
];

/// The motion prompt that kept the mouth clean for lipsync-2 on Bond Money v2: a soft,
/// barely open mouth (the lip-sync model draws the singing), no tears, the face toward the
/// camera, and a push-in that stops before an extreme close-up.
pub const STEADY_TEMPLATE: &str = "Continuous single take, no cuts. Exactly the same face, hairstyle and outfit throughout. \
Her lips stay soft and relaxed, only gently parted, never opened wide, teeth barely showing. Dry cheeks, no tears, no crying, \
no grimace: a calm, soulful expression with softly knitted brows. The emotion is in the body: shoulders rise and fall with each \
breath, a gentle sway, natural hand gestures. The head stays mostly steady and turned toward the camera so the mouth is always \
clearly visible. Steady camera with a very slow gentle push-in that stops at a medium close-up, never an extreme close-up.";

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Clip(Id),
    /// A video file to place on the timeline first.
    File(PathBuf),
    /// An image of the singer or speaker to place on the timeline first.
    Still(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub enum VoiceSrc {
    Clip(Id),
    File(PathBuf),
    /// Generated with ElevenLabs (reused if the same line was made before).
    Tts { voice: String, text: String, model: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    FixMouth,
    Performance,
    AnimateSync,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::FixMouth, Method::Performance, Method::AnimateSync];

    pub fn label(self) -> &'static str {
        match self {
            Method::FixMouth => "Fix the mouth",
            Method::Performance => "Performance shot (Seedance)",
            Method::AnimateSync => "Animate + sync",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Method::FixMouth => "Keeps the footage and re-draws only the mouth (Sync lipsync-2). Cheapest; best for dialogue or a shot you already like.",
            Method::Performance => "Re-generates the shot from a still with Seedance 2.0, driven by the song and lyrics. The most emotion, but it refuses photo-real likenesses.",
            Method::AnimateSync => "Animates the still with a steady face (Wan, Hailuo or Kling), then lipsync-2-pro sings it. Works on likenesses Seedance refuses and on long spans.",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "fix_mouth" | "fix" | "lipsync" => Some(Method::FixMouth),
            "performance" | "seedance" => Some(Method::Performance),
            "animate_sync" | "animate" => Some(Method::AnimateSync),
            _ => None,
        }
    }
}

/// Everything the Studio needs to plan and run one lip-sync.
#[derive(Clone, Debug)]
pub struct Request {
    pub video_clip: Id,
    pub voice_clip: Id,
    /// A trimmed timeline range; `None` = wherever the shot and the voice overlap.
    pub span: Option<(f64, f64)>,
    pub method: Method,
    pub quality: Quality,
    pub placement: Placement,
    pub face: Option<FaceBox>,
    pub direction: String,
    pub video_model: String,
    pub lipsync_model: String,
    pub performance_model: String,
    pub sheet: Option<PathBuf>,
    /// How a voice longer than the shot is covered (Fix the mouth).
    pub fill: lipsync::Fill,
}

impl Request {
    pub fn new(video_clip: Id, voice_clip: Id, method: Method) -> Self {
        Self {
            video_clip,
            voice_clip,
            span: None,
            method,
            quality: Quality::Standard,
            placement: Placement::Replace,
            face: None,
            direction: String::new(),
            video_model: DEFAULT_VIDEO_MODEL.into(),
            lipsync_model: lipsync::MODELS[0].0.into(),
            performance_model: performance::MODELS[0].0.into(),
            sheet: None,
            fill: lipsync::Fill::None,
        }
    }
}

/// The recommended method: a still or a long span needs a new shot (Animate + sync covers
/// both and isn't refused); dialogue on footage just needs the mouth fixed; a sung span
/// on footage gets the most expressive option.
pub fn recommend(project: &Project, video_clip: Id, voice_clip: Id, span: Option<(f64, f64)>) -> Method {
    let Some(v) = project.clip(video_clip) else { return Method::FixMouth };
    let is_still = v.media_id().and_then(|m| project.media(m)).is_some_and(|m| m.kind == MediaKind::Image);
    let voice = project.clip(voice_clip);
    let (a, b) = span.unwrap_or_else(|| voice.map_or((v.start, v.end()), |c| (v.start.max(c.start), v.end().min(c.end()))));
    if is_still || b - a > performance::MAX_SEGMENT {
        return Method::AnimateSync;
    }
    if voice.is_some_and(|c| project.audio_role(c) == AudioRole::Voice) {
        return Method::FixMouth;
    }
    Method::Performance
}

pub fn animate_model(id: &str) -> (&'static str, &'static str, f64, u32, u32) {
    ANIMATE_MODELS.iter().copied().find(|m| m.0 == id).unwrap_or(("", "custom model", 0.08, 4, 15))
}

/// Seconds to generate for a span: the span plus the lead-in and a little tail, within
/// the model's limits.
pub fn animate_seconds(model: &str, span: f64) -> u32 {
    let (_, _, _, min, max) = animate_model(model);
    ((span + ANIMATE_LEAD_IN).ceil() as u32 + 1).clamp(min, max)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Estimate {
    pub usd: f64,
    /// The timeline span that gets lip-synced.
    pub start: f64,
    pub end: f64,
    pub parts: Vec<(String, f64)>,
    pub warnings: Vec<String>,
}

/// The span every method works on: where the shot and the voice overlap (plus the
/// mouth-settle padding), inside the trimmed range.
pub fn span(project: &Project, req: &Request) -> Result<(f64, f64)> {
    let mut p = lipsync::plan_fill(project, req.video_clip, Some(req.voice_clip), req.span, req.fill);
    if let Err(e) = &p {
        // A still (or a sped-up shot) can't be "fixed" but can still be animated or performed.
        if req.method != Method::FixMouth && e.to_string().contains("speed") {
            let v = project.clip(req.video_clip).ok_or_else(|| anyhow!("no clip {}", req.video_clip))?;
            let a = project.clip(req.voice_clip).ok_or_else(|| anyhow!("no voice clip {}", req.voice_clip))?;
            let (mut s, mut e) = (v.start.max(a.start), v.end().min(a.end()));
            if let Some((x, y)) = req.span {
                s = s.max(x);
                e = e.min(y);
            }
            p = Ok(lipsync::Plan { video_clip: req.video_clip, voice_clip: req.voice_clip, start: s, end: e, face: None, quality: req.quality, fill: lipsync::Fill::None });
        }
    }
    let p = p?;
    if p.end - p.start < 0.5 {
        bail!("the voice doesn't overlap this shot");
    }
    Ok((p.start, p.end))
}

pub fn performance_plan(project: &Project, req: &Request) -> Result<performance::Plan> {
    let mut plan = performance::plan(project, req.video_clip, Some(req.voice_clip))?;
    if let Some((a, b)) = req.span {
        let (s, e) = (plan.start().max(a), plan.end().min(b));
        if e - s < 1.0 {
            bail!("the trimmed range is too short");
        }
        let cuts: Vec<f64> = project.tracks.iter().filter(|t| t.kind == TrackKind::Text).flat_map(|t| t.clips.iter().map(|c| c.start)).filter(|&t| t > s && t < e).collect();
        plan.segments = performance::split_segments(s, e, performance::MAX_SEGMENT, performance::OVERLAP, &cuts)
            .into_iter()
            .map(|(x, y)| performance::Segment { start: x, end: y, lyrics: performance::lyrics_in(project, x, y) })
            .collect();
    }
    Ok(plan)
}

pub fn fix_plan(project: &Project, req: &Request) -> Result<lipsync::Plan> {
    let mut plan = lipsync::plan_fill(project, req.video_clip, Some(req.voice_clip), req.span, req.fill)?;
    plan.face = req.face;
    plan.quality = req.quality;
    Ok(plan)
}

/// What a request will cost and cover, with a breakdown. Free.
pub fn estimate(project: &Project, req: &Request) -> Result<Estimate> {
    let mut est = Estimate::default();
    match req.method {
        Method::FixMouth => {
            let p = fix_plan(project, req)?;
            let usd = (p.end - p.start) * lipsync::usd_per_second(&req.lipsync_model);
            est.parts.push((format!("{} for {:.1} s", req.lipsync_model, p.end - p.start), usd));
            (est.start, est.end) = (p.start, p.end);
        }
        Method::Performance => {
            let p = performance_plan(project, req)?;
            let usd = p.estimate(&req.performance_model);
            est.parts.push((format!("Seedance, {} segment(s)", p.segments.len()), usd));
            (est.start, est.end) = (p.start(), p.end());
            est.warnings.push("Seedance refuses photo-real likenesses (free). If it does, the Studio offers Animate + sync.".into());
        }
        Method::AnimateSync => {
            let (s, e) = span(project, req)?;
            let secs = animate_seconds(&req.video_model, e - s);
            let (_, label, per_sec, _, max) = animate_model(&req.video_model);
            est.parts.push((format!("{label}: {secs} s of video"), secs as f64 * per_sec));
            est.parts.push((format!("{ANIMATE_LIPSYNC_MODEL} for {:.1} s", e - s), (e - s) * lipsync::usd_per_second(ANIMATE_LIPSYNC_MODEL)));
            if e - s + ANIMATE_LEAD_IN > max as f64 {
                est.warnings.push(format!("The span is longer than {max} s of video; the last {:.1} s will hold on the final frame. Trim it or split the scene.", e - s + ANIMATE_LEAD_IN - max as f64));
            }
            (est.start, est.end) = (s, e);
        }
    }
    if req.face.is_some_and(|f| crate::ai::faces::size_hint(f.h).is_some()) {
        est.warnings.push(crate::ai::faces::size_hint(0.0).unwrap_or_default().to_string());
    }
    est.usd = est.parts.iter().map(|p| p.1).sum();
    Ok(est)
}

/// Puts a file or still on the timeline at `start` for `len` seconds, on the video track
/// nearest the main one that has room. Returns the clip.
pub fn place_source(project: &mut Project, source: &Source, start: f64, len: f64) -> Result<Id> {
    let path = match source {
        Source::Clip(id) => return project.clip(*id).map(|_| *id).ok_or_else(|| anyhow!("no clip {id}")),
        Source::File(p) | Source::Still(p) => p,
    };
    let info = ffmpeg::probe(path).with_context(|| format!("reading {}", path.display()))?;
    if info.kind == MediaKind::Audio {
        bail!("{} has no picture", path.display());
    }
    let media = project.add_media(info.into_media_item(path.clone()));
    let mut clip = project.new_media_clip(media, start).ok_or_else(|| anyhow!("could not use {}", path.display()))?;
    clip.start = start.max(0.0);
    clip.duration = if project.media(media).is_some_and(|m| m.kind == MediaKind::Image) { len.max(0.5) } else { clip.duration.min(len.max(0.5)) };
    clip.volume = 0.0;
    let id = clip.id;
    let main = project.main_track();
    project.place_clip(clip, Some(main));
    Ok(id)
}

/// Puts an audio file on the timeline at `start` as a Voice clip (music ducked under it).
pub fn place_voice(project: &mut Project, path: &Path, start: f64) -> Result<Id> {
    let info = ffmpeg::probe(path).with_context(|| format!("reading {}", path.display()))?;
    let media = project.add_media(info.into_media_item(path.to_path_buf()));
    let mut clip = project.new_media_clip(media, start).ok_or_else(|| anyhow!("could not use {}", path.display()))?;
    clip.start = start.max(0.0);
    clip.audio_role = AudioRole::Voice;
    let id = clip.id;
    project.place_clip(clip, None);
    let paths: Vec<(Id, PathBuf)> = project.media.iter().map(|m| (m.id, m.path.clone())).collect();
    let peaks_for = |m: Id| paths.iter().find(|(x, _)| *x == m).and_then(|(_, p)| ffmpeg::waveform(p).ok()).map(std::sync::Arc::new);
    let opts = crate::smart_audio::DuckOptions::from_mix(&project.audio);
    let _ = crate::smart_audio::auto_duck(project, &peaks_for, &opts);
    Ok(id)
}

/// The finished work of a run, to apply on the main thread.
pub enum Outcome {
    /// One lip-synced file over `plan`'s span (Fix the mouth, Animate + sync).
    Synced { plan: lipsync::Plan, path: PathBuf, info: ProbeInfo, animated: Option<PathBuf> },
    /// Seedance clips, one per segment.
    Performed { plan: performance::Plan, results: Vec<(PathBuf, ProbeInfo)> },
    /// The provider refused the input (not charged); try `suggested` instead.
    Refused { reason: String, suggested: Method },
}

/// Runs the request in the background. `Ok(None)` if cancelled.
pub fn run(project: &Project, req: &Request, fal_key: &str, openrouter: Option<&crate::ai::openrouter::Client>, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Outcome>> {
    match req.method {
        Method::FixMouth => {
            let plan = fix_plan(project, req)?;
            let Some((path, info)) = lipsync::lip_sync(project, &plan, fal_key, &req.lipsync_model, out_dir, cancel, progress)? else { return Ok(None) };
            Ok(Some(Outcome::Synced { plan, path, info, animated: None }))
        }
        Method::Performance => {
            let plan = performance_plan(project, req)?;
            let opts = performance::Options { model: &req.performance_model, direction: &req.direction, sheet: req.sheet.as_deref() };
            match performance::perform(project, &plan, fal_key, &opts, out_dir, cancel, progress) {
                Ok(Some(results)) => Ok(Some(Outcome::Performed { plan, results })),
                Ok(None) => Ok(None),
                Err(e) if lipsync::is_refusal(&e) => Ok(Some(Outcome::Refused { reason: format!("{e:#}").chars().take(300).collect(), suggested: Method::AnimateSync })),
                Err(e) => Err(e),
            }
        }
        Method::AnimateSync => {
            let client = openrouter.ok_or_else(|| anyhow!("Animate + sync needs an OpenRouter key (AI → AI settings)"))?;
            animate_sync(project, req, fal_key, client, out_dir, cancel, progress)
        }
    }
}

/// The still Animate + sync starts from: the image itself, or the shot's frame at the
/// start of the span.
fn still_for(project: &Project, req: &Request, start: f64) -> Result<(Vec<u8>, &'static str)> {
    let v = project.clip(req.video_clip).ok_or_else(|| anyhow!("the shot was removed"))?;
    let m = v.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("the shot has no media"))?;
    if m.kind == MediaKind::Image {
        let bytes = std::fs::read(&m.path).with_context(|| format!("reading {}", m.path.display()))?;
        let png = m.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
        return Ok((bytes, if png { "image/png" } else { "image/jpeg" }));
    }
    let t = v.source_in + (start - v.start) * v.speed;
    Ok((video_gen::frame_jpeg(&m.path, t, 1920)?, "image/jpeg"))
}

pub fn animate_prompt(direction: &str) -> String {
    let d = direction.trim();
    if d.is_empty() {
        STEADY_TEMPLATE.to_string()
    } else {
        format!("{STEADY_TEMPLATE} {d}")
    }
}

fn animate_sync(project: &Project, req: &Request, fal_key: &str, client: &crate::ai::openrouter::Client, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Outcome>> {
    let (start, end) = span(project, req)?;
    let len = end - start;
    let secs = animate_seconds(&req.video_model, len);
    let first_frame = still_for(project, req, start)?;
    let gen = video_gen::GenRequest {
        model: req.video_model.clone(),
        prompt: animate_prompt(&req.direction),
        duration: secs,
        // The animation stays 720p (what every model accepts); lip-sync quality is separate.
        resolution: "720p".into(),
        aspect_ratio: project.aspect.short_label().to_string(),
        generate_audio: false,
        first_frame: Some(first_frame),
        last_frame: None,
    };
    let name = project.clip(req.video_clip).and_then(|c| c.media_id()).and_then(|m| project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| "shot".into());
    let stem = format!("animated-{}", video_gen::slug(&name));
    let mut sub = |p: f32, s: &str| progress(p * 0.6, &format!("Animating: {s}"));
    let generated = match video_gen::generate(client, &gen, out_dir, &stem, cancel, &mut sub) {
        Ok(Some(g)) => g,
        Ok(None) => return Ok(None),
        Err(e) if lipsync::is_refusal(&e) => return Ok(Some(Outcome::Refused { reason: format!("{e:#}").chars().take(300).collect(), suggested: Method::FixMouth })),
        Err(e) => return Err(e),
    };
    // Lip-sync the animated clip over the span, starting after its lead-in.
    let work = ffmpeg::work_dir("animate-sync")?;
    let result = (|| -> Result<Option<Outcome>> {
        let offset = if generated.info.duration >= len + ANIMATE_LEAD_IN { ANIMATE_LEAD_IN } else { (generated.info.duration - len).max(0.0) };
        let video = work.join("animated-in.mp4");
        lipsync::cut_video(&generated.path, offset, len.min(generated.info.duration), req.quality, &video)?;
        let plan = lipsync::Plan { video_clip: req.video_clip, voice_clip: req.voice_clip, start, end, face: None, quality: req.quality, fill: lipsync::Fill::None };
        let audio = work.join("animated-in.wav");
        lipsync::render_line_audio(project, &plan, &audio)?;
        let out = unique(out_dir, &format!("lipsync-{}", video_gen::slug(&name)));
        let mut sub = |p: f32, s: &str| progress(0.6 + p * 0.4, &format!("Lip-syncing: {s}"));
        let Some(path) = lipsync::run(fal_key, &video, &audio, ANIMATE_LIPSYNC_MODEL, &out, cancel, &mut sub)? else { return Ok(None) };
        crate::economics::record(crate::economics::Spend {
            kind: "lipsync",
            model: ANIMATE_LIPSYNC_MODEL,
            usd: len * lipsync::usd_per_second(ANIMATE_LIPSYNC_MODEL),
            units: len,
            unit: "second",
            output: Some(&path),
            source: "estimate",
            note: "fal.ai list price per second (Animate + sync)",
        });
        let info = ffmpeg::probe(&path)?;
        Ok(Some(Outcome::Synced { plan, path, info, animated: Some(generated.path.clone()) }))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn unique(dir: &Path, stem: &str) -> PathBuf {
    let base = dir.join(format!("{stem}.mp4"));
    if !base.exists() {
        return base;
    }
    (2..).map(|n| dir.join(format!("{stem}-{n}.mp4"))).find(|p| !p.exists()).expect("unbounded")
}

/// Applies a finished run to the project. Returns the new clip ids.
pub fn apply(project: &mut Project, req: &Request, outcome: Outcome) -> Result<Vec<Id>> {
    match outcome {
        Outcome::Synced { plan, path, info, animated } => {
            if let Some(a) = animated {
                if let Ok(i) = ffmpeg::probe(&a) {
                    project.add_media(i.into_media_item(a));
                }
            }
            Ok(vec![lipsync::apply_with(project, &plan, &path, info, req.placement)?])
        }
        Outcome::Performed { plan, results } => {
            let original = project.clip(plan.video_clip).and_then(|c| c.media_id());
            let ids = performance::apply(project, &plan, results)?;
            if let Some(&first) = ids.first() {
                if let Some(m) = project.clip(first).and_then(|c| c.media_id()) {
                    project.record_take(plan.video_clip, first, original, m);
                }
            }
            Ok(ids)
        }
        Outcome::Refused { reason, .. } => bail!("refused: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaItem;

    fn item(name: &str, kind: MediaKind, duration: f64) -> MediaItem {
        let (w, h) = if kind == MediaKind::Audio { (0, 0) } else { (1280, 720) };
        MediaItem { id: 0, path: name.into(), name: name.into(), kind, duration, width: w, height: h, fps: 24.0, has_audio: kind == MediaKind::Audio }
    }

    /// A shot (video or still) at 0–`len` with a song (Music role) or a line (Voice) under it.
    fn project(still: bool, len: f64, voice_role: AudioRole) -> (Project, Id, Id) {
        let mut p = Project::default();
        let v = p.add_media(item(if still { "still.png" } else { "shot.mp4" }, if still { MediaKind::Image } else { MediaKind::Video }, if still { 0.0 } else { 30.0 }));
        let a = p.add_media(item("song.mp3", MediaKind::Audio, 200.0));
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.duration = len;
        let vid = vc.id;
        p.append_to_main(vc);
        let mut ac = p.new_media_clip(a, 0.0).unwrap();
        ac.duration = len;
        ac.audio_role = voice_role;
        let aid = ac.id;
        p.place_clip(ac, None);
        (p, vid, aid)
    }

    #[test]
    fn recommends_by_source_and_voice() {
        let (p, v, a) = project(true, 6.0, AudioRole::Music);
        assert_eq!(recommend(&p, v, a, None), Method::AnimateSync, "a still");
        let (p, v, a) = project(false, 20.0, AudioRole::Music);
        assert_eq!(recommend(&p, v, a, None), Method::AnimateSync, "longer than one Seedance segment");
        assert_eq!(recommend(&p, v, a, Some((0.0, 10.0))), Method::Performance, "trimmed to fit");
        let (p, v, a) = project(false, 6.0, AudioRole::Voice);
        assert_eq!(recommend(&p, v, a, None), Method::FixMouth, "dialogue");
        let (p, v, a) = project(false, 6.0, AudioRole::Music);
        assert_eq!(recommend(&p, v, a, None), Method::Performance, "a sung span");
    }

    #[test]
    fn seconds_snap_to_the_model() {
        assert_eq!(animate_seconds("alibaba/wan-3.0", 10.59), 13);
        assert_eq!(animate_seconds("minimax/hailuo-3-max", 10.59), 10);
        assert_eq!(animate_seconds("minimax/hailuo-3-max", 2.0), 6);
        assert_eq!(animate_seconds("alibaba/wan-3.0", 30.0), 15);
    }

    #[test]
    fn estimates_sum_their_parts() {
        let (p, v, a) = project(true, 10.0, AudioRole::Music);
        let req = Request::new(v, a, Method::AnimateSync);
        let est = estimate(&p, &req).unwrap();
        assert_eq!(est.parts.len(), 2);
        assert!((est.usd - est.parts.iter().map(|x| x.1).sum::<f64>()).abs() < 1e-9);
        assert!((est.end - est.start - 10.0).abs() < 1e-6);
        // 12 s of Wan ($0.72) + 10 s of lipsync-2-pro ($0.83).
        assert!((est.usd - (12.0 * 0.06 + 10.0 * 0.0834)).abs() < 1e-6, "{}", est.usd);
        let (p, v, a) = project(false, 6.0, AudioRole::Voice);
        let mut req = Request::new(v, a, Method::FixMouth);
        req.lipsync_model = "lipsync-2-pro".into();
        let est = estimate(&p, &req).unwrap();
        assert!((est.usd - 6.0 * 0.0834).abs() < 1e-6);
        req.span = Some((1.0, 4.0));
        assert!((estimate(&p, &req).unwrap().usd - 3.0 * 0.0834).abs() < 1e-6, "a trimmed span costs less");
    }

    #[test]
    fn performance_plan_respects_the_trim() {
        let (p, v, a) = project(false, 20.0, AudioRole::Music);
        let mut req = Request::new(v, a, Method::Performance);
        assert_eq!(performance_plan(&p, &req).unwrap().segments.len(), 2);
        req.span = Some((2.0, 12.0));
        let plan = performance_plan(&p, &req).unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert_eq!((plan.start(), plan.end()), (2.0, 12.0));
    }

    #[test]
    fn a_trimmed_fix_keeps_the_face_and_quality() {
        let (p, v, a) = project(false, 6.0, AudioRole::Voice);
        let mut req = Request::new(v, a, Method::FixMouth);
        req.face = Some(FaceBox { cx: 0.5, cy: 0.4, w: 0.2, h: 0.3 });
        req.quality = Quality::High;
        let plan = fix_plan(&p, &req).unwrap();
        assert_eq!(plan.face, req.face);
        assert_eq!(plan.quality, Quality::High);
    }

    #[test]
    fn method_names_parse() {
        assert_eq!(Method::parse("animate_sync"), Some(Method::AnimateSync));
        assert_eq!(Method::parse("Fix-mouth"), Some(Method::FixMouth));
        assert_eq!(Method::parse("seedance"), Some(Method::Performance));
        assert!(animate_prompt("she smiles").starts_with("Continuous single take") && animate_prompt("she smiles").ends_with("she smiles"));
    }
}
