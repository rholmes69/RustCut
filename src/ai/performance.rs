//! Performance shots ("method 1" lip-sync): instead of patching the mouth of existing
//! footage, the shot is re-generated from a still of the singer with Seedance 2.0,
//! driven by the matching piece of the song and its lyrics. Long spans are cut into
//! overlapping segments (≤ 14 s, 1 s overlap, preferring lyric line boundaries) so
//! each one stays inside the model's 15 s audio limit and syncs tightly.

use crate::ai::lipsync::{self, data_uri};
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{Clip, Id, MediaKind, Project, TrackKind};
use anyhow::{anyhow, bail, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// (fal endpoint, label, USD per second of output at 720p)
pub const MODELS: &[(&str, &str, f64)] = &[
    ("bytedance/seedance-2.0/fast/reference-to-video", "Seedance 2.0 Fast", 0.2419),
    ("bytedance/seedance-2.0/reference-to-video", "Seedance 2.0 (best)", 0.3034),
];
pub const DEFAULT_DIRECTION: &str = "sings the song with expressive movements and emotion";
/// Longest audio segment sent at once (the model takes ≤ 15 s of audio).
pub const MAX_SEGMENT: f64 = 14.0;
/// Each segment starts this much before the previous one ends, for a smooth lead-in.
pub const OVERLAP: f64 = 1.0;

pub fn usd_per_second(model: &str) -> f64 {
    MODELS.iter().find(|m| m.0 == model).map_or(0.3034, |m| m.2)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub lyrics: String,
}

impl Segment {
    /// The generated clip's length: a little longer than the audio, within 4–15 s.
    pub fn video_seconds(&self) -> u32 {
        ((self.end - self.start).ceil() as u32 + 1).clamp(4, 15)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub video_clip: Id,
    pub audio_clip: Id,
    pub segments: Vec<Segment>,
}

impl Plan {
    pub fn start(&self) -> f64 {
        self.segments.first().map_or(0.0, |s| s.start)
    }
    pub fn end(&self) -> f64 {
        self.segments.last().map_or(0.0, |s| s.end)
    }
    pub fn estimate(&self, model: &str) -> f64 {
        self.segments.iter().map(|s| s.video_seconds() as f64).sum::<f64>() * usd_per_second(model)
    }
}

/// Splits `[start, end]` into segments of at most `max` seconds that overlap by
/// `overlap`, cutting at the latest boundary in `cuts` that keeps a segment ≥ half `max`.
pub fn split_segments(start: f64, end: f64, max: f64, overlap: f64, cuts: &[f64]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut s = start;
    while end - s > max + 1e-6 {
        let limit = s + max;
        let e = cuts.iter().copied().filter(|&c| c >= s + max * 0.5 && c <= limit).fold(limit, |best, c| if (best - limit).abs() < 1e-9 { c } else { best.max(c) });
        out.push((s, e));
        s = e - overlap;
    }
    out.push((s, end));
    out
}

fn text_clips(project: &Project) -> impl Iterator<Item = &Clip> {
    project.tracks.iter().filter(|t| t.kind == TrackKind::Text).flat_map(|t| t.clips.iter())
}

/// The lyric lines (text clips) mostly inside `[a, b]`, in order.
pub fn lyrics_in(project: &Project, a: f64, b: f64) -> String {
    let mut lines: Vec<(f64, String)> = text_clips(project)
        .filter_map(|c| {
            let overlap = c.end().min(b) - c.start.max(a);
            let text = c.text()?.content.trim().replace('\n', " ");
            (overlap > c.duration * 0.5 && !text.is_empty()).then_some((c.start, text))
        })
        .collect();
    lines.sort_by(|x, y| x.0.total_cmp(&y.0));
    lines.dedup_by(|x, y| x.1 == y.1);
    lines.into_iter().map(|l| l.1).collect::<Vec<_>>().join(" / ")
}

pub fn plan(project: &Project, video_clip: Id, audio_clip: Option<Id>) -> Result<Plan> {
    let v = project.clip(video_clip).ok_or_else(|| anyhow!("no clip {video_clip}"))?;
    let media = v.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("pick a video or image clip of the singer"))?;
    if project.track_kind_for(v) != TrackKind::Video || media.kind == MediaKind::Audio {
        bail!("pick a video or image clip of the singer");
    }
    let audio_id = match audio_clip {
        Some(id) => id,
        None => lipsync::find_voice(project, video_clip).ok_or_else(|| anyhow!("put the song (or the vocal) on an audio track under this shot first"))?,
    };
    let a = project.clip(audio_id).ok_or_else(|| anyhow!("no audio clip {audio_id}"))?;
    let (start, end) = (v.start.max(a.start), v.end().min(a.end()));
    if end - start < 1.0 {
        bail!("the song doesn't overlap this shot");
    }
    let cuts: Vec<f64> = text_clips(project).map(|c| c.start).filter(|&t| t > start && t < end).collect();
    let segments = split_segments(start, end, MAX_SEGMENT, OVERLAP, &cuts).into_iter().map(|(s, e)| Segment { start: s, end: e, lyrics: lyrics_in(project, s, e) }).collect();
    Ok(Plan { video_clip, audio_clip: audio_id, segments })
}

/// The prompt for one segment, following the tutorial: who performs, the lyrics in the
/// audio, one continuous shot, no subtitles.
pub fn prompt(direction: &str, lyrics: &str, has_sheet: bool) -> String {
    let who = if has_sheet { "The performer in @Image1 (same person as the character sheet @Image2)" } else { "The performer in @Image1" };
    let direction = if direction.trim().is_empty() { DEFAULT_DIRECTION } else { direction.trim() };
    let mut p = format!("{who} {direction}, lip-syncing precisely to the vocals in @Audio1.");
    if !lyrics.is_empty() {
        p.push_str(&format!(" The lyrics being sung are: \"{lyrics}\"."));
    }
    p.push_str(" Keep the framing, outfit, face and setting from @Image1. A single continuous shot with no cuts and no change of camera angle. No subtitles, no captions, no on-screen text.");
    p
}

/// A still of the singer: the image itself, or the video's frame at the start of the span.
pub fn render_still(project: &Project, plan: &Plan, dir: &Path) -> Result<PathBuf> {
    let v = project.clip(plan.video_clip).ok_or_else(|| anyhow!("the shot was removed"))?;
    let m = v.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no media"))?;
    if m.kind == MediaKind::Image {
        return Ok(m.path.clone());
    }
    let out = dir.join("performance-still.png");
    let t = v.source_in + (plan.start() - v.start) * v.speed;
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
        .arg(&m.path)
        .args(["-frames:v", "1"])
        .arg(&out)
        .status()?
        .success();
    if !ok || !out.is_file() {
        bail!("could not take a still from the shot");
    }
    Ok(out)
}

pub fn render_audio(project: &Project, audio_clip: Id, seg: &Segment, out: &Path) -> Result<()> {
    let a = project.clip(audio_clip).ok_or_else(|| anyhow!("the audio clip was removed"))?;
    let m = a.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no audio media"))?;
    let src = a.source_in + (seg.start - a.start) * a.speed;
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{src:.3}"), "-t", &format!("{:.3}", seg.end - seg.start), "-i"])
        .arg(&m.path)
        .args(["-vn", "-ac", "2", "-ar", "44100", "-b:a", "192k"])
        .arg(out)
        .status()?
        .success();
    if !ok {
        bail!("could not cut the song segment");
    }
    Ok(())
}

pub struct Options<'a> {
    pub model: &'a str,
    pub direction: &'a str,
    /// Optional character sheet sent as a second reference for a consistent face.
    pub sheet: Option<&'a Path>,
}

/// Generates every segment. Returns the files in segment order, or `Ok(None)` if cancelled.
pub fn perform(project: &Project, plan: &Plan, key: &str, opts: &Options, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Vec<(PathBuf, ProbeInfo)>>> {
    let work = ffmpeg::work_dir("performance")?;
    let result = (|| -> Result<Option<Vec<(PathBuf, ProbeInfo)>>> {
        let still = render_still(project, plan, &work)?;
        let mime = |p: &Path| if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg")) { "image/jpeg" } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("webp")) { "image/webp" } else { "image/png" };
        let mut images = vec![data_uri(&still, mime(&still))?];
        if let Some(sheet) = opts.sheet {
            images.push(data_uri(sheet, mime(sheet))?);
        }
        let name = project.clip(plan.video_clip).and_then(|c| c.media_id()).and_then(|m| project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| "shot".into());
        let stem = format!("performance-{}", crate::ai::video_gen::slug(&name));
        let n = plan.segments.len();
        let mut done = Vec::new();
        for (i, seg) in plan.segments.iter().enumerate() {
            let audio = work.join(format!("segment-{i}.mp3"));
            render_audio(project, plan.audio_clip, seg, &audio)?;
            let body = json!({
                "prompt": prompt(opts.direction, &seg.lyrics, opts.sheet.is_some()),
                "image_urls": images,
                "audio_urls": [data_uri(&audio, "audio/mpeg")?],
                "duration": seg.video_seconds().to_string(),
                "resolution": "720p",
                "aspect_ratio": project.aspect.short_label(),
                "generate_audio": false,
            });
            let mut out = out_dir.join(format!("{stem}-{}.mp4", i + 1));
            let mut k = 2;
            while out.exists() {
                out = out_dir.join(format!("{stem}-{}-{k}.mp4", i + 1));
                k += 1;
            }
            let label = format!("Performing segment {} of {n}…", i + 1);
            let base = i as f32 / n as f32;
            let mut sub = |p: f32, s: &str| progress(base + p / n as f32, if s.starts_with("Waiting") { s } else { &label });
            let Some(path) = lipsync::fal_run(key, opts.model, body, &out, &label, cancel, &mut sub)? else { return Ok(None) };
            let info = ffmpeg::probe(&path)?;
            let secs = seg.video_seconds() as f64;
            crate::economics::record(crate::economics::Spend {
                kind: "lipsync",
                model: opts.model,
                usd: secs * usd_per_second(opts.model),
                units: secs,
                unit: "second",
                output: Some(&path),
                source: "estimate",
                note: "fal.ai Seedance 2.0 list price per output second (720p)",
            });
            done.push((path, info));
        }
        Ok(Some(done))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Replaces the planned span of the shot with the performance clips: each segment
/// plays until the next one starts (the overlap is the next clip's lead-in), with its
/// own sound muted so the song on the timeline is what's heard.
pub fn apply(project: &mut Project, plan: &Plan, results: Vec<(PathBuf, ProbeInfo)>) -> Result<Vec<Id>> {
    if results.len() != plan.segments.len() {
        bail!("expected {} performance clips, got {}", plan.segments.len(), results.len());
    }
    let original = project.clip(plan.video_clip).cloned().ok_or_else(|| anyhow!("the shot was removed"))?;
    let (ti, _) = project.locate(plan.video_clip).ok_or_else(|| anyhow!("the shot was removed"))?;
    let (start, end) = (plan.start(), plan.end());
    let mut middle = plan.video_clip;
    if start > original.start + 1e-3 {
        middle = project.split_clip(middle, start).ok_or_else(|| anyhow!("could not split the shot"))?;
    }
    if end < original.end() - 1e-3 {
        project.split_clip(middle, end).ok_or_else(|| anyhow!("could not split the shot"))?;
    }
    let old = project.remove_clip(middle).ok_or_else(|| anyhow!("could not replace the shot"))?;
    let mut ids = Vec::new();
    for (i, (path, info)) in results.into_iter().enumerate() {
        let seg = &plan.segments[i];
        let until = plan.segments.get(i + 1).map_or(seg.end, |n| n.start);
        let len = info.duration.max(0.1);
        let media = project.add_media(info.into_media_item(path));
        let mut c = project.new_media_clip(media, seg.start).ok_or_else(|| anyhow!("could not use a performance clip"))?;
        c.start = seg.start;
        c.source_in = 0.0;
        c.duration = (until - seg.start).min(len);
        c.filter = old.filter;
        c.adjust = old.adjust;
        c.opacity = old.opacity;
        if i == 0 {
            c.transition_in = old.transition_in;
        }
        c.volume = 0.0;
        ids.push(c.id);
        project.tracks[ti].clips.push(c);
    }
    project.sort_track(ti);
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioRole, MediaItem, TextStyle};

    /// PAID, manual: `PERF_PROJECT=… PERF_CLIP=120 [PERF_MODEL=fast|best PERF_SHEET=… PERF_DIRECTION=… PERF_DRY=1] cargo test --lib performance_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn performance_live() {
        let Ok(path) = std::env::var("PERF_PROJECT") else { return };
        let path = PathBuf::from(path);
        let clip: Id = std::env::var("PERF_CLIP").unwrap().parse().unwrap();
        let model = if std::env::var("PERF_MODEL").as_deref() == Ok("best") { MODELS[1].0 } else { MODELS[0].0 };
        let mut p = Project::load(&path).unwrap();
        let plan = plan(&p, clip, None).unwrap();
        for s in &plan.segments {
            println!("segment {:.2}–{:.2} ({} s video): {}", s.start, s.end, s.video_seconds(), s.lyrics);
        }
        println!("estimate ${:.2} with {model}", plan.estimate(model));
        println!("prompt: {}", prompt(&std::env::var("PERF_DIRECTION").unwrap_or_default(), &plan.segments[0].lyrics, std::env::var("PERF_SHEET").is_ok()));
        if std::env::var("PERF_DRY").is_ok() {
            return;
        }
        let key = crate::ai::openrouter::AiSettings::load().fal_key().expect("fal key in RustCut settings");
        let sheet = std::env::var("PERF_SHEET").ok().map(PathBuf::from);
        let direction = std::env::var("PERF_DIRECTION").unwrap_or_default();
        let opts = Options { model, direction: &direction, sheet: sheet.as_deref() };
        let dir = crate::ai::video_gen::output_dir(Some(&path)).join("lipsync");
        let t = std::time::Instant::now();
        let results = perform(&p, &plan, &key, &opts, &dir, &AtomicBool::new(false), &mut |f, s| println!("  {:3.0}% {s}", f * 100.0)).unwrap().unwrap();
        for (f, i) in &results {
            println!("got {} ({:.2} s, {}x{}) in {:?}", f.display(), i.duration, i.width, i.height, t.elapsed());
        }
        std::fs::copy(&path, path.with_extension("before-performance.rcproj")).unwrap();
        let ids = apply(&mut p, &plan, results).unwrap();
        p.save(&path).unwrap();
        println!("placed clips {ids:?}; project saved (backup: {})", path.with_extension("before-performance.rcproj").display());
    }

    #[test]
    fn segments_overlap_and_prefer_line_starts() {
        assert_eq!(split_segments(0.0, 10.0, 14.0, 1.0, &[]), vec![(0.0, 10.0)]);
        // 30 s with no lyrics: 0–14, 13–27, 26–30.
        assert_eq!(split_segments(0.0, 30.0, 14.0, 1.0, &[]), vec![(0.0, 14.0), (13.0, 27.0), (26.0, 30.0)]);
        // A line starting at 11 s is used as the first cut.
        let s = split_segments(0.0, 24.0, 14.0, 1.0, &[3.0, 11.0, 15.0]);
        assert_eq!(s[0], (0.0, 11.0));
        assert_eq!(s[1].0, 10.0);
        assert!(s.iter().all(|(a, b)| b - a <= 14.0 + 1e-9));
    }

    #[test]
    fn video_is_a_little_longer_than_the_audio() {
        let seg = |a, b| Segment { start: a, end: b, lyrics: String::new() };
        assert_eq!(seg(0.0, 6.0).video_seconds(), 7);
        assert_eq!(seg(0.0, 1.5).video_seconds(), 4);
        assert_eq!(seg(0.0, 14.0).video_seconds(), 15);
    }

    fn project() -> (Project, Id) {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 30.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "song.mp3".into(), name: "song.mp3".into(), kind: MediaKind::Audio, duration: 200.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let vc = p.new_media_clip(v, 0.0).unwrap();
        let vid = vc.id;
        p.append_to_main(vc); // 0–30 s
        let mut ac = p.new_media_clip(a, 0.0).unwrap();
        ac.audio_role = AudioRole::Music;
        p.place_clip(ac, None);
        for (t, line) in [(2.0, "No. I ain't paying that bond money."), (6.0, "No. I ain't paying that bond money."), (18.0, "You gotta learn the lesson")] {
            let mut style = TextStyle::default();
            style.content = line.into();
            let mut c = p.new_text_clip(style, t);
            c.start = t;
            c.duration = 3.5;
            p.place_clip(c, None);
        }
        (p, vid)
    }

    #[test]
    fn plans_segments_with_their_lyrics() {
        let (p, vid) = project();
        let plan = plan(&p, vid, None).unwrap();
        assert_eq!(plan.segments.len(), 3, "{:?}", plan.segments);
        assert!(plan.segments[0].lyrics.contains("bond money"), "{:?}", plan.segments[0]);
        assert!(!plan.segments[0].lyrics.contains(" / No."), "repeated lines are not doubled");
        assert!(plan.estimate(MODELS[0].0) > 0.0);
        let pr = prompt("", &plan.segments[0].lyrics, true);
        assert!(pr.contains("@Audio1") && pr.contains("@Image2") && pr.contains("No subtitles"));
    }

    #[test]
    fn performance_clips_replace_the_span_back_to_back() {
        let (mut p, vid) = project();
        let plan = plan(&p, vid, None).unwrap();
        let info = |d: f64| ProbeInfo { kind: MediaKind::Video, duration: d, width: 1280, height: 720, fps: 24.0, has_audio: false };
        let results: Vec<_> = plan.segments.iter().enumerate().map(|(i, s)| (PathBuf::from(format!("perf-{i}.mp4")), info(s.video_seconds() as f64))).collect();
        let ids = apply(&mut p, &plan, results).unwrap();
        assert_eq!(ids.len(), 3);
        let main = p.main_track();
        let clips = &p.tracks[main].clips;
        assert_eq!(clips.len(), 3);
        for w in clips.windows(2) {
            assert!((w[0].end() - w[1].start).abs() < 1e-9, "no gaps or overlaps");
        }
        assert!((clips[2].end() - 30.0).abs() < 1e-9);
        assert!(clips.iter().all(|c| c.volume == 0.0));
    }
}
