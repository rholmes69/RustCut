//! Auto-captions: speech-to-text with whisper.cpp, run locally.
//!
//! Two engines are supported, detected at runtime:
//! - FFmpeg's built-in `whisper` filter (FFmpeg 8+ built with whisper.cpp), which
//!   fits the existing FFmpeg-based pipeline with no extra dependencies;
//! - a standalone whisper.cpp `whisper-cli` executable (`RUSTCUT_WHISPER` to override).
//!
//! The result is a list of timed [`Segment`]s which [`apply_captions`] turns into
//! ordinary text clips on a dedicated caption track.

use crate::export;
use crate::media::ffmpeg::{self, atempo_chain};
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Engine {
    FfmpegFilter,
    WhisperCli(String),
}

impl Engine {
    pub fn label(&self) -> String {
        match self {
            Engine::FfmpegFilter => "FFmpeg whisper filter".into(),
            Engine::WhisperCli(bin) => format!("whisper.cpp ({bin})"),
        }
    }
}

/// Finds a usable speech-to-text engine.
pub fn detect_engine() -> Option<Engine> {
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-h", "filter=whisper"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("whisper.cpp")) {
        return Some(Engine::FfmpegFilter);
    }
    let candidates = match std::env::var("RUSTCUT_WHISPER") {
        Ok(bin) => vec![bin],
        Err(_) => vec!["whisper-cli".to_string(), "whisper-cpp".to_string()],
    };
    candidates.into_iter().find_map(|bin| {
        let ok = ffmpeg::command(&bin)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(Engine::WhisperCli(bin))
    })
}

// ── models ───────────────────────────────────────────────────────────────────

pub struct ModelInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { name: "tiny", label: "Tiny — fastest", size_mb: 75 },
    ModelInfo { name: "base", label: "Base — recommended", size_mb: 142 },
    ModelInfo { name: "small", label: "Small — more accurate", size_mb: 466 },
    ModelInfo { name: "medium", label: "Medium — most accurate, slow", size_mb: 1463 },
];

pub const DEFAULT_MODEL: &str = "base";

pub fn model_info(name: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.name == name)
}

pub fn model_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

/// Where downloaded models live (`RUSTCUT_MODELS_DIR` to override).
pub fn models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RUSTCUT_MODELS_DIR") {
        return PathBuf::from(d);
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join("RustCut")
            .join("models")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/RustCut/models")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("rustcut/models")
    }
}

/// The model file to use: `RUSTCUT_WHISPER_MODEL` if set, else the downloaded one.
pub fn model_path(name: &str) -> PathBuf {
    match std::env::var_os("RUSTCUT_WHISPER_MODEL") {
        Some(p) => PathBuf::from(p),
        None => models_dir().join(format!("ggml-{name}.bin")),
    }
}

pub fn is_downloaded(name: &str) -> bool {
    std::fs::metadata(model_path(name)).is_ok_and(|m| m.len() > 1_000_000)
}

/// Downloads a model with `curl` (bundled with Windows 10+, macOS and most Linux
/// distributions). Returns `Ok(None)` if cancelled.
pub fn download_model(name: &str, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<Option<PathBuf>> {
    let info = model_info(name).ok_or_else(|| anyhow!("unknown model `{name}`"))?;
    let dest = model_path(name);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let part = dest.with_extension("bin.part");
    let mut child = ffmpeg::command("curl")
        .args(["-L", "--fail", "-sS", "--retry", "2", "-o"])
        .arg(&part)
        .arg(model_url(name))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl to download the model")?;
    let expected = info.size_mb as f64 * 1_048_576.0;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Ok(None);
        }
        let got = std::fs::metadata(&part).map(|m| m.len() as f64).unwrap_or(0.0);
        progress((got / expected).clamp(0.0, 0.99) as f32);
        std::thread::sleep(Duration::from_millis(250));
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut e, &mut err);
        }
        let _ = std::fs::remove_file(&part);
        bail!("download failed: {}", err.trim());
    }
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if size < 1_000_000 {
        let _ = std::fs::remove_file(&part);
        bail!("download failed: file is only {size} bytes");
    }
    std::fs::rename(&part, &dest)?;
    progress(1.0);
    Ok(Some(dest))
}

// ── transcription ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Clip(Id),
    Timeline,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub model: PathBuf,
    /// ISO 639-1 code or "auto".
    pub language: String,
    /// Translate to English.
    pub translate: bool,
    /// Maximum characters per segment (short segments ≈ word timing, for editing).
    pub max_len: Option<u32>,
}

/// Audio to transcribe, plus how its timestamps map onto the timeline.
struct AudioPlan {
    inputs: Vec<String>,
    /// Either a simple `-af` chain (prefix) on input 0 or a complex graph with an `[aout]` label.
    simple_chain: Option<Vec<String>>,
    graph: Option<String>,
    duration: f64,
    offset: f64,
    range: (f64, f64),
}

fn plan_audio(project: &Project, source: Source, work: &Path) -> Result<AudioPlan> {
    match source {
        Source::Clip(id) => {
            let clip = project.clip(id).ok_or_else(|| anyhow!("no clip with id {id}"))?;
            let media = clip
                .media_id()
                .and_then(|m| project.media(m))
                .ok_or_else(|| anyhow!("this clip has no media"))?;
            if !media.has_sound() {
                bail!("this clip has no audio to transcribe");
            }
            let inputs = vec![
                "-ss".into(),
                format!("{:.3}", clip.source_in),
                "-t".into(),
                format!("{:.3}", clip.duration * clip.speed),
                "-i".into(),
                media.path.to_string_lossy().into_owned(),
            ];
            Ok(AudioPlan {
                inputs,
                simple_chain: Some(atempo_chain(clip.speed).into_iter().collect()),
                graph: None,
                duration: clip.duration,
                offset: clip.start,
                range: (clip.start, clip.end()),
            })
        }
        Source::Timeline => {
            let audible = project.tracks.iter().filter(|t| t.kind != TrackKind::Text && !t.muted).any(|t| {
                t.clips.iter().any(|c| {
                    c.volume > 0.001 && c.media_id().and_then(|m| project.media(m)).is_some_and(|m| m.has_sound())
                })
            });
            if !audible {
                bail!("nothing to transcribe: the timeline has no audible clips");
            }
            let g = export::build_graph(project, 360, project.fps, work, None, false, true)?;
            Ok(AudioPlan {
                inputs: g.inputs,
                simple_chain: None,
                graph: Some(g.filter),
                duration: g.duration,
                offset: 0.0,
                range: (0.0, g.duration),
            })
        }
    }
}

/// FFmpeg args that run `final_chain` on the planned audio, writing to `output` args.
fn ffmpeg_args(plan: &AudioPlan, final_chain: &str, output: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1"]
        .map(String::from)
        .to_vec();
    args.extend(plan.inputs.iter().cloned());
    if let Some(prefix) = &plan.simple_chain {
        let mut chain = prefix.clone();
        chain.push(final_chain.to_string());
        args.extend(["-vn".into(), "-af".into(), chain.join(",")]);
    } else if let Some(graph) = &plan.graph {
        args.extend([
            "-filter_complex".into(),
            format!("{graph};[aout]{final_chain}[aw]"),
            "-map".into(),
            "[aw]".into(),
        ]);
    }
    args.extend(output.iter().cloned());
    args
}

fn clean_language(lang: &str) -> String {
    let l = lang.trim().to_ascii_lowercase();
    if (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_lowercase()) {
        l
    } else {
        "auto".into()
    }
}

/// Transcribes speech and returns segments in timeline time. `Ok(None)` if cancelled.
pub fn transcribe(
    project: &Project,
    source: Source,
    opts: &Options,
    engine: &Engine,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32, &str),
) -> Result<Option<Vec<Segment>>> {
    if !opts.model.is_file() {
        bail!("speech model not found at {}", opts.model.display());
    }
    let work = ffmpeg::work_dir("captions")?;
    let result = (|| -> Result<Option<Vec<Segment>>> {
        let plan = plan_audio(project, source, &work)?;
        let lang = clean_language(&opts.language);
        let srt_path = work.join("captions.srt");
        match engine {
            Engine::FfmpegFilter => {
                let whisper = format!(
                    "aresample=16000,whisper=model='{}':language={lang}:translate={}:queue=20:destination='{}':format=srt{}",
                    ffmpeg::filter_path(&opts.model),
                    opts.translate as u8,
                    ffmpeg::filter_path(&srt_path),
                    opts.max_len.map_or(String::new(), |n| format!(":max_len={n}")),
                );
                let args = ffmpeg_args(&plan, &whisper, &["-f".into(), "null".into(), "-".into()]);
                progress(0.0, "Transcribing…");
                let done = ffmpeg::run_with_progress(&args, plan.duration, cancel, |p| progress(p, "Transcribing…"))?;
                if !done {
                    return Ok(None);
                }
            }
            Engine::WhisperCli(bin) => {
                let wav = work.join("audio.wav");
                let args = ffmpeg_args(
                    &plan,
                    "aresample=16000",
                    &["-ac".into(), "1".into(), "-c:a".into(), "pcm_s16le".into(), wav.to_string_lossy().into_owned()],
                );
                progress(0.0, "Extracting audio…");
                if !ffmpeg::run_with_progress(&args, plan.duration, cancel, |p| progress(p * 0.2, "Extracting audio…"))? {
                    return Ok(None);
                }
                let mut cmd = ffmpeg::command(bin);
                cmd.arg("-m").arg(&opts.model).arg("-f").arg(&wav).args(["-l", &lang, "-osrt", "-pp", "-of"]);
                cmd.arg(work.join("captions"));
                if opts.translate {
                    cmd.arg("-tr");
                }
                if let Some(n) = opts.max_len {
                    cmd.args(["-ml", &n.to_string(), "-sow"]);
                }
                let mut child = cmd
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("could not run {bin}"))?;
                let stderr = child.stderr.take().context("no stderr")?;
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if cancel.load(Ordering::Relaxed) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(None);
                    }
                    if let Some(p) = line.split("progress =").nth(1) {
                        if let Ok(p) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                            progress(0.2 + 0.8 * p / 100.0, "Transcribing…");
                        }
                    }
                }
                if !child.wait()?.success() {
                    bail!("{bin} failed");
                }
            }
        }
        let text = std::fs::read_to_string(&srt_path).unwrap_or_default();
        let (lo, hi) = plan.range;
        let segments = parse_srt(&text)
            .into_iter()
            .filter_map(|s| {
                let start = (plan.offset + s.start).clamp(lo, hi);
                let end = (plan.offset + s.end).clamp(lo, hi);
                let text = clean_text(&s.text)?;
                (end - start >= 0.05).then_some(Segment { start, end, text })
            })
            .collect();
        Ok(Some(segments))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Drops non-speech markers like `[BLANK_AUDIO]` or `(music)` and tidies whitespace.
fn clean_text(text: &str) -> Option<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim();
    let bracketed = |o: char, c: char| t.starts_with(o) && t.ends_with(c) && !t[1..t.len() - 1].contains(c);
    if t.is_empty() || bracketed('[', ']') || bracketed('(', ')') || bracketed('*', '*') {
        return None;
    }
    if !t.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(t.to_string())
}

// ── SRT ──────────────────────────────────────────────────────────────────────

fn parse_timestamp(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    Some(h * 3600.0 + m * 60.0 + sec)
}

pub fn parse_srt(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r')).peekable();
    while let Some(line) = lines.next() {
        let Some((a, b)) = line.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_timestamp(a), parse_timestamp(b.split_whitespace().next().unwrap_or(""))) else {
            continue;
        };
        let mut body = Vec::new();
        while let Some(l) = lines.peek() {
            if l.trim().is_empty() {
                break;
            }
            body.push(lines.next().unwrap_or_default().trim().to_string());
        }
        out.push(Segment { start, end, text: body.join("\n") });
    }
    out
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

pub fn to_srt(segments: &[Segment]) -> String {
    segments
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}\n{} --> {}\n{}\n", i + 1, srt_time(s.start), srt_time(s.end), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── shaping captions ─────────────────────────────────────────────────────────

/// Splits long segments into captions of at most `max_words` words, sharing the
/// segment's time in proportion to text length.
pub fn chunk_segments(segments: &[Segment], max_words: usize) -> Vec<Segment> {
    let max_words = max_words.max(1);
    let mut out = Vec::new();
    for s in segments {
        let words: Vec<&str> = s.text.split_whitespace().collect();
        if words.len() <= max_words {
            out.push(s.clone());
            continue;
        }
        let n = words.len().div_ceil(max_words);
        let per = words.len().div_ceil(n);
        let chunks: Vec<String> = words.chunks(per).map(|c| c.join(" ")).collect();
        let total: usize = chunks.iter().map(|c| c.chars().count() + 1).sum();
        let mut t = s.start;
        for (i, c) in chunks.iter().enumerate() {
            let share = (c.chars().count() + 1) as f64 / total as f64;
            let end = if i + 1 == chunks.len() { s.end } else { t + (s.end - s.start) * share };
            out.push(Segment { start: t, end, text: c.clone() });
            t = end;
        }
    }
    out
}

/// Breaks a caption into two balanced lines when it is longer than `max_chars`.
pub fn wrap_caption(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars || text.contains('\n') {
        return text.to_string();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let total = text.chars().count();
    let mut best = (usize::MAX, 1);
    let mut acc = 0;
    for i in 1..words.len() {
        acc += words[i - 1].chars().count() + 1;
        let diff = (acc as i64 * 2 - total as i64).unsigned_abs() as usize;
        if diff < best.0 {
            best = (diff, i);
        }
    }
    format!("{}\n{}", words[..best.1].join(" "), words[best.1..].join(" "))
}

/// Caption looks offered in the UI and over MCP.
pub fn caption_styles() -> Vec<(&'static str, TextStyle)> {
    vec![
        (
            "Clean",
            TextStyle {
                content: String::new(),
                font_size: 60.0,
                bold: true,
                color: [255, 255, 255, 255],
                background: None,
                outline: Some([0, 0, 0, 255]),
                align_to: None,
            },
        ),
        (
            "Boxed",
            TextStyle {
                content: String::new(),
                font_size: 48.0,
                bold: false,
                color: [255, 255, 255, 255],
                background: Some([0, 0, 0, 180]),
                outline: None,
                align_to: None,
            },
        ),
        (
            "Pop",
            TextStyle {
                content: String::new(),
                font_size: 84.0,
                bold: true,
                color: [255, 224, 40, 255],
                background: None,
                outline: Some([0, 0, 0, 255]),
                align_to: None,
            },
        ),
        (
            "Minimal",
            TextStyle {
                content: String::new(),
                font_size: 44.0,
                bold: false,
                color: [255, 255, 255, 240],
                background: None,
                outline: Some([0, 0, 0, 200]),
                align_to: None,
            },
        ),
    ]
}

pub fn caption_style(name: &str) -> Option<TextStyle> {
    caption_styles().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, s)| s)
}

/// How captions are laid out on the timeline.
#[derive(Clone, Debug)]
pub struct Layout {
    pub style: TextStyle,
    pub max_words: usize,
    /// Vertical centre offset (fraction of canvas height).
    pub y: f32,
    /// Remove earlier auto-caption tracks first.
    pub replace: bool,
}

/// Adds captions as text clips on a new caption track at the top.
/// Returns the track index and the number of clips created.
pub fn apply_captions(project: &mut Project, segments: &[Segment], layout: &Layout) -> (usize, usize) {
    if layout.replace {
        project.tracks.retain(|t| !t.captions);
        project.main_track();
    }
    let ti = project.insert_track(TrackKind::Text, None);
    let count = project.tracks.iter().filter(|t| t.captions).count();
    project.tracks[ti].captions = true;
    project.tracks[ti].name = if count == 0 { "Captions".into() } else { format!("Captions {}", count + 1) };

    let max_chars = if layout.max_words > 6 { 32 } else { usize::MAX };
    let mut prev_end: f64 = 0.0;
    let mut clips = Vec::new();
    for s in chunk_segments(segments, layout.max_words) {
        let start = s.start.max(prev_end);
        let end = s.end.max(start);
        if end - start < MIN_CLIP_LEN {
            continue;
        }
        let style = TextStyle { content: wrap_caption(&s.text, max_chars), ..layout.style.clone() };
        let mut clip = project.new_text_clip(style, start);
        clip.duration = end - start;
        clip.transform.y = layout.y;
        prev_end = end;
        clips.push(clip);
    }
    let n = clips.len();
    project.tracks[ti].clips = clips;
    if n == 0 {
        project.tracks.remove(ti);
        return (project.tracks.len(), 0);
    }
    (ti, n)
}

/// Applies a caption style to every clip on caption tracks, keeping their text.
pub fn restyle_captions(project: &mut Project, style: &TextStyle, y: f32) -> usize {
    let mut n = 0;
    for track in project.tracks.iter_mut().filter(|t| t.captions) {
        for clip in &mut track.clips {
            if let Some(t) = clip.text_mut() {
                let content = std::mem::take(&mut t.content);
                *t = TextStyle { content, ..style.clone() };
                clip.transform.y = y;
                n += 1;
            }
        }
    }
    n
}

/// Caption clips as SRT segments (caption tracks, or every text track if there are none).
pub fn timeline_segments(project: &Project) -> Vec<Segment> {
    let has_captions = project.tracks.iter().any(|t| t.captions && !t.clips.is_empty());
    let mut segs: Vec<Segment> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Text && (t.captions || !has_captions))
        .flat_map(|t| &t.clips)
        .filter_map(|c| c.text().map(|s| Segment { start: c.start, end: c.end(), text: s.content.clone() }))
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    segs.sort_by(|a, b| a.start.total_cmp(&b.start));
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\r\n00:00:00,000 --> 00:00:02,500\r\n Hello there.\r\n\r\n2\r\n00:00:02,500 --> 00:00:05,000\r\n[BLANK_AUDIO]\r\n\r\n3\r\n00:00:05.000 --> 00:00:09,000\r\nThis is a much longer sentence that needs splitting\r\nacross lines\r\n";

    #[test]
    fn parses_srt() {
        let s = parse_srt(SRT);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], Segment { start: 0.0, end: 2.5, text: "Hello there.".into() });
        assert_eq!(s[2].start, 5.0);
        assert!(s[2].text.contains('\n'));
        assert_eq!(parse_timestamp("01:02:03,450"), Some(3723.45));
    }

    #[test]
    fn srt_roundtrip() {
        let segs = vec![
            Segment { start: 0.0, end: 1.25, text: "One".into() },
            Segment { start: 3661.5, end: 3662.0, text: "Two\nlines".into() },
        ];
        let text = to_srt(&segs);
        assert!(text.contains("01:01:01,500 --> 01:01:02,000"));
        assert_eq!(parse_srt(&text), segs);
    }

    #[test]
    fn cleans_markers() {
        assert_eq!(clean_text("[BLANK_AUDIO]"), None);
        assert_eq!(clean_text(" (music) "), None);
        assert_eq!(clean_text("♪ ♪"), None);
        assert_eq!(clean_text("  Hi   there "), Some("Hi there".into()));
        assert_eq!(clean_text("[laughs] okay"), Some("[laughs] okay".into()));
    }

    #[test]
    fn chunks_by_words() {
        let segs = vec![Segment { start: 10.0, end: 16.0, text: "one two three four five six seven".into() }];
        let c = chunk_segments(&segs, 3);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].start, 10.0);
        assert_eq!(c.last().unwrap().end, 16.0);
        assert!(c.windows(2).all(|w| (w[0].end - w[1].start).abs() < 1e-9));
        assert!(c.iter().all(|s| s.text.split_whitespace().count() <= 3));
        assert_eq!(chunk_segments(&segs, 10).len(), 1);
    }

    #[test]
    fn wraps_long_captions() {
        assert_eq!(wrap_caption("short one", 32), "short one");
        let w = wrap_caption("this caption is definitely longer than the limit", 32);
        let lines: Vec<&str> = w.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!((lines[0].len() as i64 - lines[1].len() as i64).abs() < 12);
    }

    #[test]
    fn applies_and_replaces_captions() {
        let mut p = Project::default();
        let layout = Layout { style: caption_style("pop").unwrap(), max_words: 2, y: 0.3, replace: true };
        let segs = vec![
            Segment { start: 0.0, end: 2.0, text: "hello big world".into() },
            Segment { start: 1.9, end: 3.0, text: "again".into() },
        ];
        let (ti, n) = apply_captions(&mut p, &segs, &layout);
        assert_eq!((ti, n), (0, 3));
        let t = &p.tracks[0];
        assert!(t.captions && t.kind == TrackKind::Text && t.name == "Captions");
        // Overlaps are trimmed so captions never stack.
        assert!(t.clips.windows(2).all(|w| w[0].end() <= w[1].start + 1e-9));
        assert_eq!(t.clips[0].transform.y, 0.3);
        assert_eq!(t.clips[0].text().unwrap().font_size, 84.0);

        apply_captions(&mut p, &segs[..1], &layout);
        assert_eq!(p.tracks.iter().filter(|t| t.captions).count(), 1);
        assert_eq!(timeline_segments(&p).len(), 2);

        let n = restyle_captions(&mut p, &caption_style("boxed").unwrap(), -0.3);
        assert_eq!(n, 2);
        assert!(p.tracks[0].clips[0].text().unwrap().background.is_some());
        assert_eq!(p.tracks[0].clips[0].text().unwrap().content, "hello big");
    }

    #[test]
    fn languages() {
        assert_eq!(clean_language("EN"), "en");
        assert_eq!(clean_language("auto"), "auto");
        assert_eq!(clean_language("x; rm -rf"), "auto");
    }
}
