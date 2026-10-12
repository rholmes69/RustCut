//! Rough cut by prompt: a video-understanding model reads the timeline's transcript
//! (whisper, ≈ word-level timestamps) and/or watches a tiny copy of the timeline,
//! then returns the ranges to keep. RustCut snaps the cuts to speech boundaries and
//! ripple-cuts everything else across all tracks in one undoable step.

use super::openrouter::{mp4_part, text_part, Client, Usage};
use crate::captions::{self, Engine, Segment};
use crate::export;
use crate::media::ffmpeg;
use crate::model::Project;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

/// Uploads above this are skipped (the transcript alone is used).
pub const MAX_PROXY_BYTES: u64 = 18 * 1024 * 1024;
/// Gemini-class models: ~258 tokens per sampled frame (1 fps) + ~32 per second of audio.
const VIDEO_TOKENS_PER_SECOND: f64 = 290.0;

#[derive(Clone, Debug, Default)]
pub struct RoughCutRequest {
    pub prompt: String,
    pub target_seconds: Option<f64>,
    pub use_transcript: bool,
    pub use_video: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeepRange {
    pub start: f64,
    pub end: f64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    /// Time on the original (uncut) timeline.
    pub time: f64,
    pub title: String,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub keep: Vec<KeepRange>,
    pub summary: String,
    pub titles: Vec<String>,
    pub description: String,
    pub chapters: Vec<Chapter>,
    pub usage: Usage,
    pub transcript_segments: usize,
    pub sent_video: bool,
    /// Timeline ranges with no picture (black frames / no video clip).
    pub no_picture: Vec<(f64, f64)>,
    /// Original timeline length.
    pub source_duration: f64,
}

impl Plan {
    /// Seconds of the kept ranges that have no picture.
    pub fn kept_without_picture(&self) -> f64 {
        self.keep
            .iter()
            .map(|k| self.no_picture.iter().map(|(a, b)| (b.min(k.end) - a.max(k.start)).max(0.0)).sum::<f64>())
            .sum()
    }

    pub fn kept_seconds(&self) -> f64 {
        self.keep.iter().map(|k| k.end - k.start).sum()
    }

    /// Ranges that will be cut (complement of `keep`).
    pub fn removed(&self) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut cursor = 0.0;
        for k in &self.keep {
            if k.start > cursor + 1e-6 {
                out.push((cursor, k.start));
            }
            cursor = cursor.max(k.end);
        }
        if self.source_duration > cursor + 1e-6 {
            out.push((cursor, self.source_duration));
        }
        out
    }

    /// Where an original-timeline time lands after the cut (removed times snap to
    /// the next kept moment).
    pub fn map_time(&self, t: f64) -> f64 {
        let mut shift = 0.0;
        for (a, b) in self.removed() {
            if t >= b {
                shift += b - a;
            } else if t > a {
                return a - shift;
            }
        }
        (t - shift).max(0.0)
    }

    /// YouTube-style chapter list on the cut timeline (first chapter at 0:00).
    pub fn chapters_text(&self) -> String {
        let mut rows: Vec<(f64, String)> = Vec::new();
        for c in &self.chapters {
            let t = self.map_time(c.time);
            if rows.last().is_none_or(|(lt, _)| t - lt >= 10.0) {
                rows.push((t, c.title.clone()));
            }
        }
        if let Some(first) = rows.first_mut() {
            first.0 = 0.0;
        }
        rows.iter()
            .map(|(t, title)| {
                let s = t.round() as u64;
                if s >= 3600 {
                    format!("{}:{:02}:{:02} {title}", s / 3600, (s / 60) % 60, s % 60)
                } else {
                    format!("{}:{:02} {title}", s / 60, s % 60)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ripple-cuts the project to the kept ranges. Returns seconds removed.
    pub fn apply(&self, project: &mut Project) -> f64 {
        let keep: Vec<(f64, f64)> = self.keep.iter().map(|k| (k.start, k.end)).collect();
        project.keep_ranges(&keep)
    }
}

/// Rough token/cost estimate before running (USD, using per-million prices).
pub fn estimate_cost(duration: f64, use_video: bool, prompt_per_m: f64, completion_per_m: f64) -> f64 {
    let transcript_tokens = duration * 4.0;
    let video_tokens = if use_video { duration * VIDEO_TOKENS_PER_SECOND } else { 0.0 };
    let input = 1500.0 + transcript_tokens + video_tokens;
    let output = 1500.0;
    (input * prompt_per_m + output * completion_per_m) / 1e6
}

/// Pauses between transcript segments longer than `min` seconds.
pub fn pauses(segments: &[Segment], duration: f64, min: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut prev = 0.0;
    for s in segments {
        if s.start - prev >= min {
            out.push((prev, s.start));
        }
        prev = prev.max(s.end);
    }
    if duration - prev >= min {
        out.push((prev, duration));
    }
    out
}

const SYSTEM: &str = "You are a professional video editor making a rough cut. You get the timeline's speech transcript \
with timestamps in seconds and/or a low-resolution copy of the video with its audio. Decide which time ranges to KEEP so \
the result follows the user's instructions.\nRules:\n- Times are seconds on the ORIGINAL timeline.\n- Ranges must be \
chronological and must not overlap.\n- Cut at natural boundaries: between sentences or words, never in the middle of a \
word.\n- Remove filler words (um, uh, erm, you know…) and long pauses when the user asks, or when it clearly improves \
pacing.\n- If a target length is given, the kept total should be within ±10% of it.\n- Keep enough context around each \
moment that it makes sense on its own.\n- Use the video for visual moments (reactions, action, laughter, scene changes), \
not just speech.\nAlso write: a one-paragraph summary of the edit, three title suggestions, a YouTube description under \
120 words, and chapters (time on the original timeline + a short title) for the kept content.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "keep": { "type": "array", "items": { "type": "object", "properties": {
                "start": { "type": "number" }, "end": { "type": "number" }, "reason": { "type": "string" }
            }, "required": ["start", "end", "reason"], "additionalProperties": false } },
            "summary": { "type": "string" },
            "titles": { "type": "array", "items": { "type": "string" } },
            "description": { "type": "string" },
            "chapters": { "type": "array", "items": { "type": "object", "properties": {
                "time": { "type": "number" }, "title": { "type": "string" }
            }, "required": ["time", "title"], "additionalProperties": false } }
        },
        "required": ["keep", "summary", "titles", "description", "chapters"],
        "additionalProperties": false
    })
}

fn user_text(req: &RoughCutRequest, duration: f64, transcript: &[Segment], no_picture: &[(f64, f64)]) -> String {
    let mut t = format!(
        "Instructions: {}\nTarget length: {}\nTimeline length: {duration:.2} s\n",
        req.prompt.trim(),
        req.target_seconds.map_or("not specified".to_string(), |s| format!("{s:.0} s")),
    );
    if transcript.is_empty() {
        t += "\nNo transcript available: judge from the video and its audio.\n";
    } else {
        t += "\nTranscript ([start-end] text):\n";
        for s in transcript {
            t += &format!("[{:.2}-{:.2}] {}\n", s.start, s.end, s.text.replace('\n', " "));
        }
        let gaps = pauses(transcript, duration, 0.7);
        if !no_picture.is_empty() {
            t += "\nNO PICTURE (black screen, audio only) during: ";
            t += &no_picture.iter().map(|(a, b)| format!("[{a:.2}-{b:.2}]")).collect::<Vec<_>>().join(" ");
            t += "\nDo NOT keep these ranges unless the user explicitly asks for audio-only moments.\n";
        }
        if !gaps.is_empty() {
            t += "\nSilences longer than 0.7 s: ";
            t += &gaps.iter().map(|(a, b)| format!("[{a:.2}-{b:.2}]")).collect::<Vec<_>>().join(" ");
            t += "\n";
        }
    }
    t
}

/// Turns the model's reply into a clean plan: clamped, sorted, merged, snapped to
/// speech boundaries, with tiny fragments dropped.
pub fn parse_plan(reply: &Value, duration: f64, transcript: &[Segment]) -> Result<Plan> {
    let mut keep: Vec<KeepRange> = reply["keep"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|k| {
            let (s, e) = (k["start"].as_f64()?, k["end"].as_f64()?);
            let (s, e) = (s.clamp(0.0, duration), e.clamp(0.0, duration));
            (e - s > 0.05).then(|| KeepRange { start: s, end: e, reason: k["reason"].as_str().unwrap_or("").to_string() })
        })
        .collect();
    // Snap to speech: starts to the nearest segment start, ends to the nearest end.
    for k in &mut keep {
        if let Some(seg) = transcript.iter().filter(|s| (s.start - k.start).abs() <= 0.35).min_by(|a, b| {
            (a.start - k.start).abs().total_cmp(&(b.start - k.start).abs())
        }) {
            k.start = (seg.start - 0.05).max(0.0);
        }
        if let Some(seg) = transcript.iter().filter(|s| (s.end - k.end).abs() <= 0.35).min_by(|a, b| {
            (a.end - k.end).abs().total_cmp(&(b.end - k.end).abs())
        }) {
            k.end = (seg.end + 0.1).min(duration);
        }
    }
    keep.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<KeepRange> = Vec::new();
    for k in keep {
        match merged.last_mut() {
            Some(last) if k.start <= last.end + 0.15 => {
                last.end = last.end.max(k.end);
                if !k.reason.is_empty() && !last.reason.contains(&k.reason) {
                    last.reason = format!("{}; {}", last.reason, k.reason);
                }
            }
            _ => merged.push(k),
        }
    }
    merged.retain(|k| k.end - k.start >= 0.3);
    if merged.is_empty() {
        bail!("the model didn't keep anything — try rephrasing the instructions");
    }
    let strings = |v: &Value| -> Vec<String> {
        v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    };
    let mut chapters: Vec<Chapter> = reply["chapters"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| Some(Chapter { time: c["time"].as_f64()?.clamp(0.0, duration), title: c["title"].as_str()?.trim().to_string() }))
        .filter(|c| !c.title.is_empty())
        .collect();
    chapters.sort_by(|a, b| a.time.total_cmp(&b.time));
    Ok(Plan {
        keep: merged,
        summary: reply["summary"].as_str().unwrap_or("").trim().to_string(),
        titles: strings(&reply["titles"]),
        description: reply["description"].as_str().unwrap_or("").trim().to_string(),
        chapters,
        usage: Usage::default(),
        transcript_segments: transcript.len(),
        sent_video: false,
        no_picture: Vec::new(),
        source_duration: duration,
    })
}

/// Renders a tiny copy of the timeline (240p, 2 fps, mono audio) for the model.
pub fn render_proxy(project: &Project, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<Option<Vec<u8>>> {
    let work = ffmpeg::work_dir("roughcut")?;
    let result = (|| -> Result<Option<Vec<u8>>> {
        let g = export::build_graph(project, 240, 2, &work, None, true, true)?;
        let out = work.join("proxy.mp4");
        let mut args: Vec<String> = ["-hide_banner", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1"].map(String::from).to_vec();
        args.extend(g.inputs);
        args.extend(export::filter_complex_args(g.filter, &work)?);
        args.extend([
            "-map".into(),
            format!("[{}]", g.video_label),
            "-map".into(),
            format!("[{}]", g.audio_label.unwrap_or_default()),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "34".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-r".into(),
            "2".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "32k".into(),
            "-ac".into(),
            "1".into(),
            "-t".into(),
            format!("{:.3}", g.duration),
            out.to_string_lossy().into_owned(),
        ]);
        if !ffmpeg::run_with_progress(&args, g.duration, cancel, progress)? {
            return Ok(None);
        }
        Ok(Some(std::fs::read(&out)?))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// The function that sends the request: messages in, (JSON reply, usage) out.
pub type ChatFn<'a> = dyn Fn(Value) -> Result<(Value, Usage)> + 'a;

/// Builds the plan from already-gathered inputs (transcript and/or proxy video).
pub fn plan_from_inputs(
    req: &RoughCutRequest,
    duration: f64,
    transcript: &[Segment],
    proxy: Option<&[u8]>,
    no_picture: &[(f64, f64)],
    chat: &ChatFn<'_>,
) -> Result<Plan> {
    let mut content = vec![text_part(user_text(req, duration, transcript, no_picture))];
    if let Some(mp4) = proxy {
        content.push(mp4_part(mp4));
    }
    let messages = json!([{ "role": "system", "content": SYSTEM }, { "role": "user", "content": content }]);
    let (reply, usage) = chat(messages)?;
    let mut plan = parse_plan(&reply, duration, transcript)?;
    plan.usage = usage;
    plan.sent_video = proxy.is_some();
    plan.no_picture = no_picture.to_vec();
    Ok(plan)
}

/// Black (or empty) stretches of the rendered timeline, ≥ 1 s. Renders a tiny
/// video-only copy and runs FFmpeg's `blackdetect` on it.
pub fn detect_no_picture(project: &Project, cancel: &AtomicBool) -> Result<Vec<(f64, f64)>> {
    let work = ffmpeg::work_dir("blackdetect")?;
    let result = (|| -> Result<Vec<(f64, f64)>> {
        let g = export::build_graph(project, 160, 2, &work, None, true, false)?;
        let mut child = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-nostats"])
            .args(&g.inputs)
            .args(export::filter_complex_args(format!("{};[{}]blackdetect=d=1:pix_th=0.10[bd]", g.filter, g.video_label), &work)?)
            .args(["-map", "[bd]", "-f", "null", "-"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut log = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let mut buf = [0u8; 8192];
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = child.kill();
                    break;
                }
                match e.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => log.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        }
        let _ = child.wait();
        Ok(parse_blackdetect(&log, g.duration))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Quiet ranges of the timeline mix (FFmpeg `silencedetect`, below −38 dB for ≥ 0.5 s).
pub fn detect_silence(project: &Project, cancel: &AtomicBool) -> Result<Vec<(f64, f64)>> {
    let work = ffmpeg::work_dir("silence")?;
    let result = (|| -> Result<Vec<(f64, f64)>> {
        let g = export::build_graph(project, 160, 2, &work, None, false, true)?;
        let label = g.audio_label.clone().ok_or_else(|| anyhow!("the timeline has no sound"))?;
        let mut child = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-nostats"])
            .args(&g.inputs)
            .args(export::filter_complex_args(format!("{};[{label}]silencedetect=n=-38dB:d=0.5[sd]", g.filter), &work)?)
            .args(["-map", "[sd]", "-f", "null", "-"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut log = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let mut buf = [0u8; 8192];
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = child.kill();
                    break;
                }
                match e.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => log.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        }
        let _ = child.wait();
        Ok(parse_silencedetect(&log, g.duration))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Parses `silence_start: …` / `silence_end: …` lines (an unfinished silence runs to the end).
pub fn parse_silencedetect(log: &str, duration: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = line.split("silence_start:").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|x| x.parse::<f64>().ok()) {
            start = Some(v.max(0.0));
        } else if let Some(v) = line.split("silence_end:").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|x| x.parse::<f64>().ok()) {
            if let Some(a) = start.take() {
                out.push((a, v.min(duration)));
            }
        }
    }
    if let Some(a) = start {
        out.push((a, duration));
    }
    out
}

/// Parses `black_start:… black_end:…` lines.
pub fn parse_blackdetect(log: &str, duration: f64) -> Vec<(f64, f64)> {
    let field = |line: &str, key: &str| -> Option<f64> {
        line.split(key).nth(1)?.split_whitespace().next()?.parse().ok()
    };
    log.lines()
        .filter_map(|l| Some((field(l, "black_start:")?, field(l, "black_end:")?.min(duration))))
        .filter(|(a, b)| b - a >= 1.0)
        .collect()
}

/// Speech engine + model to use for the transcript, if available.
pub fn speech_setup() -> Option<(Engine, std::path::PathBuf)> {
    let engine = captions::detect_engine()?;
    let model = ["base", "small", "tiny", "medium"].into_iter().find(|m| captions::is_downloaded(m))?;
    Some((engine, captions::model_path(model)))
}

/// Full pipeline: transcript → proxy → model → plan. `Ok(None)` if cancelled.
pub fn analyze(
    project: &Project,
    req: &RoughCutRequest,
    client: &Client,
    model: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<Option<Plan>> {
    let duration = project.duration();
    if duration < 1.0 {
        bail!("the timeline is empty");
    }
    if !req.use_transcript && !req.use_video {
        bail!("turn on the transcript, the video, or both");
    }
    let mut transcript = Vec::new();
    if req.use_transcript {
        let (engine, model_path) = speech_setup()
            .ok_or_else(|| anyhow!("no speech model: download one in the Captions tab, or turn off “Use transcript”"))?;
        let opts = captions::Options { model: model_path, language: "auto".into(), translate: false, max_len: Some(16) };
        match captions::transcribe(project, captions::Source::Timeline, &opts, &engine, cancel, |p, _| {
            progress(0.05 + 0.4 * p, "Transcribing speech…")
        }) {
            Ok(Some(segs)) => transcript = segs,
            Ok(None) => return Ok(None),
            // No audible clips is fine when the video is also sent.
            Err(e) if req.use_video => progress(0.45, &format!("No transcript ({e}); using the video only…")),
            Err(e) => return Err(e),
        }
    }
    let mut proxy = None;
    if req.use_video {
        progress(0.45, "Preparing a small copy of the video…");
        let Some(bytes) = render_proxy(project, cancel, &mut |p| progress(0.45 + 0.25 * p, "Preparing a small copy of the video…"))?
        else {
            return Ok(None);
        };
        if bytes.len() as u64 <= MAX_PROXY_BYTES {
            proxy = Some(bytes);
        } else if transcript.is_empty() {
            bail!("the timeline is too long to send as video ({} MB); turn on the transcript", bytes.len() / 1_048_576);
        }
    }
    progress(0.7, "Checking for black or empty stretches…");
    let no_picture = detect_no_picture(project, cancel)?;
    progress(0.75, "The editor model is watching…");
    let chat = |messages: Value| client.chat_json(model, messages, "rough_cut", schema());
    let plan = plan_from_inputs(req, duration, &transcript, proxy.as_deref(), &no_picture, &chat)?;
    progress(1.0, "Done");
    Ok(Some(plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: f64, e: f64, t: &str) -> Segment {
        Segment { start: s, end: e, text: t.into() }
    }

    fn transcript() -> Vec<Segment> {
        vec![
            seg(0.5, 2.0, "Hey guys welcome back"),
            seg(2.1, 2.4, "um"),
            seg(4.0, 6.0, "today we're going to Lagos"),
            seg(6.2, 8.9, "and it was hilarious"),
            seg(12.0, 14.0, "anyway that's it"),
        ]
    }

    #[test]
    fn plan_is_cleaned_snapped_and_merged() {
        let reply = json!({
            "keep": [
                { "start": 4.2, "end": 6.1, "reason": "intro to trip" },
                { "start": 0.4, "end": 2.05, "reason": "greeting" },
                { "start": 6.15, "end": 8.8, "reason": "funny" },
                { "start": 13.0, "end": 13.1, "reason": "tiny" },
                { "start": -5.0, "end": 0.2, "reason": "clamped then too short" }
            ],
            "summary": "Kept the greeting and the funny trip story.",
            "titles": ["Lagos!", "We went to Lagos"],
            "description": "A short trip.",
            "chapters": [{ "time": 0.0, "title": "Hello" }, { "time": 6.2, "title": "Lagos" }]
        });
        let p = parse_plan(&reply, 15.0, &transcript()).unwrap();
        let ranges: Vec<(f64, f64)> = p.keep.iter().map(|k| ((k.start * 100.0).round() / 100.0, (k.end * 100.0).round() / 100.0)).collect();
        // Snapped to speech (start-0.05 / end+0.1), and the trip ranges merged.
        assert_eq!(ranges, vec![(0.45, 2.1), (3.95, 9.0)]);
        assert!(p.keep[1].reason.contains("intro to trip") && p.keep[1].reason.contains("funny"));
        assert!((p.kept_seconds() - 6.7).abs() < 1e-9);
        assert_eq!(p.titles.len(), 2);
    }

    #[test]
    fn removed_ranges_and_time_mapping() {
        let p = Plan {
            keep: vec![
                KeepRange { start: 2.0, end: 5.0, reason: String::new() },
                KeepRange { start: 10.0, end: 12.0, reason: String::new() },
            ],
            source_duration: 20.0,
            chapters: vec![Chapter { time: 2.5, title: "Start".into() }, Chapter { time: 10.5, title: "Middle".into() }, Chapter { time: 11.0, title: "Too close".into() }],
            ..Default::default()
        };
        assert_eq!(p.removed(), vec![(0.0, 2.0), (5.0, 10.0), (12.0, 20.0)]);
        assert_eq!(p.map_time(3.0), 1.0);
        assert_eq!(p.map_time(7.0), 3.0); // inside a cut → next kept moment
        assert_eq!(p.map_time(11.0), 4.0);
        // Chapters closer than 10 s after the cut are merged (YouTube's minimum).
        assert_eq!(p.chapters_text(), "0:00 Start");
        let long = Plan {
            keep: vec![
                KeepRange { start: 0.0, end: 40.0, reason: String::new() },
                KeepRange { start: 100.0, end: 160.0, reason: String::new() },
            ],
            source_duration: 200.0,
            chapters: vec![Chapter { time: 5.0, title: "Intro".into() }, Chapter { time: 120.0, title: "The market".into() }],
            ..Default::default()
        };
        assert_eq!(long.chapters_text(), "0:00 Intro\n1:00 The market");
    }

    #[test]
    fn applies_as_ripple_cut() {
        use crate::model::*;
        let mut proj = Project::default();
        let m = proj.add_media(MediaItem {
            id: 0, path: "v.mp4".into(), name: "v.mp4".into(), kind: MediaKind::Video,
            duration: 20.0, width: 1280, height: 720, fps: 30.0, has_audio: true,
        });
        let mut c = proj.new_media_clip(m, 0.0).unwrap();
        c.duration = 20.0;
        proj.append_to_main(c);
        let p = Plan {
            keep: vec![KeepRange { start: 2.0, end: 5.0, reason: String::new() }, KeepRange { start: 10.0, end: 12.0, reason: String::new() }],
            source_duration: 20.0,
            ..Default::default()
        };
        assert_eq!(p.apply(&mut proj), 15.0);
        assert_eq!(proj.duration(), 5.0);
    }

    /// Local-only check (no network): transcript granularity and proxy size.
    /// Runs when RUSTCUT_SAMPLE_VIDEO points at a file with speech.
    #[test]
    fn local_inputs_when_available() {
        use crate::model::*;
        let Ok(path) = std::env::var("RUSTCUT_SAMPLE_VIDEO") else { return };
        let info = crate::media::ffmpeg::probe(std::path::Path::new(&path)).unwrap();
        let mut proj = Project::default();
        let m = proj.add_media(info.into_media_item(path.into()));
        let c = proj.new_media_clip(m, 0.0).unwrap();
        proj.append_to_main(c);
        let cancel = AtomicBool::new(false);
        let (engine, model) = speech_setup().expect("speech model installed");
        let opts = captions::Options { model, language: "auto".into(), translate: false, max_len: Some(16) };
        let segs = captions::transcribe(&proj, captions::Source::Timeline, &opts, &engine, &cancel, |_, _| {}).unwrap().unwrap();
        let longest = segs.iter().map(|s| s.text.chars().count()).max().unwrap_or(0);
        eprintln!("segments: {} (longest {} chars): {:?}", segs.len(), longest, segs.iter().take(8).map(|s| format!("[{:.2}-{:.2}] {}", s.start, s.end, s.text)).collect::<Vec<_>>());
        assert!(segs.len() >= 10, "short segments expected");
        let bytes = render_proxy(&proj, &cancel, &mut |_| {}).unwrap().unwrap();
        eprintln!("proxy: {} KB for {:.1} s", bytes.len() / 1024, proj.duration());
        assert!(bytes.len() > 10_000 && (bytes.len() as u64) < MAX_PROXY_BYTES);
    }

    #[test]
    fn blackdetect_parsing() {
        let log = "[Parsed_blackdetect_1 @ 000] black_start:35.1 black_end:332.366667 black_duration:297.27\n\
                   [Parsed_blackdetect_1 @ 000] black_start:3 black_end:3.5 black_duration:0.5\n";
        assert_eq!(parse_blackdetect(log, 332.4), vec![(35.1, 332.366667)]);
    }

    #[test]
    fn pauses_and_prompt() {
        let t = transcript();
        assert_eq!(pauses(&t, 15.0, 0.7), vec![(2.4, 4.0), (8.9, 12.0), (14.0, 15.0)]);
        let req = RoughCutRequest { prompt: "Keep the funny bits".into(), target_seconds: Some(60.0), use_transcript: true, use_video: false };
        let text = user_text(&req, 15.0, &t, &[(10.0, 15.0)]);
        assert!(text.contains("NO PICTURE (black screen, audio only) during: [10.00-15.00]"));
        assert!(text.contains("Target length: 60 s"));
        assert!(text.contains("[2.10-2.40] um"));
        assert!(text.contains("Silences longer than 0.7 s: [2.40-4.00]"));
    }

    #[test]
    fn plan_from_inputs_sends_video_part() {
        let chat = |messages: Value| -> Result<(Value, Usage)> {
            let content = messages[1]["content"].as_array().unwrap();
            assert_eq!(content[1]["type"], "video_url");
            assert!(content[1]["video_url"]["url"].as_str().unwrap().starts_with("data:video/mp4;base64,"));
            Ok((
                json!({ "keep": [{ "start": 1.0, "end": 3.0, "reason": "r" }], "summary": "s", "titles": [], "description": "", "chapters": [] }),
                Usage { prompt_tokens: 10, completion_tokens: 5, cost: 0.01 },
            ))
        };
        let req = RoughCutRequest { prompt: "x".into(), use_video: true, ..Default::default() };
        let plan = plan_from_inputs(&req, 10.0, &[], Some(&[0, 0, 0, 0x20]), &[(2.0, 10.0)], &chat).unwrap();
        // The kept 1–3 s range overlaps the black 2–10 s stretch by one second.
        assert!((plan.kept_without_picture() - 1.0).abs() < 1e-9);
        assert!(plan.sent_video);
        assert_eq!(plan.usage.cost, 0.01);
        assert!((estimate_cost(300.0, true, 0.75, 3.75) - 0.072).abs() < 0.01);
    }
}
