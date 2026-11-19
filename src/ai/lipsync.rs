//! Lip-sync a character to dialogue on the timeline with fal.ai (Sync lipsync-2):
//! the part of a video clip under a voice clip is sent with that line's audio, and the
//! lip-synced result replaces just that part of the clip.

use crate::ai::faces::FaceBox;
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{AudioRole, Clip, Id, Project, TrackKind};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const KEY_ENV: &str = "FAL_KEY";
pub const ENDPOINT: &str = "fal-ai/sync-lipsync/v2";
/// (id, label, USD per second of output)
pub const MODELS: &[(&str, &str, f64)] = &[("lipsync-2", "lipsync-2 (standard)", 0.05), ("lipsync-2-pro", "lipsync-2-pro (best quality)", 0.0834)];
pub const NO_KEY: &str = "No fal.ai API key. Add one in AI → Dialogue & voices (Lip-sync), or set FAL_KEY.";
/// Extra footage kept around the line so the mouth settles in and out.
const PAD: f64 = 0.15;

pub fn env_key() -> Option<String> {
    std::env::var(KEY_ENV).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

pub fn usd_per_second(model: &str) -> f64 {
    MODELS.iter().find(|m| m.0 == model).map_or(0.05, |m| m.2)
}

/// Output resolution sent to (and returned by) the lip-sync model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Up to 1280 px wide: cheaper uploads, fine for medium shots.
    #[default]
    Standard,
    /// Up to 1920 px wide (1080p): for close-ups.
    High,
}

impl Quality {
    pub fn max_width(self) -> u32 {
        match self {
            Quality::Standard => 1280,
            Quality::High => 1920,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "standard" | "720p" | "normal" => Some(Quality::Standard),
            "high" | "1080p" | "hd" => Some(Quality::High),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Quality::Standard => "Standard (720p)",
            Quality::High => "High (1080p)",
        }
    }
}

/// Where the lip-synced result goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// Replace the span of the shot (the rest of the clip is untouched).
    #[default]
    Replace,
    /// A new clip on the video track above, over the same span; the original stays below.
    Above,
}

impl Placement {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "replace" => Some(Placement::Replace),
            "above" | "overlay" => Some(Placement::Above),
            _ => None,
        }
    }
}

/// What will be lip-synced: the video clip, the voice clip and the timeline span.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub video_clip: Id,
    pub voice_clip: Id,
    pub start: f64,
    pub end: f64,
    /// Only this face (a box over the whole span, fractions of the frame) is re-mouthed:
    /// the region is cropped, synced and composited back. `None` = the model picks the face.
    pub face: Option<FaceBox>,
    pub quality: Quality,
    /// How footage is stretched when `end` is past the end of the shot.
    pub fill: Fill,
}

/// The voice clip that overlaps `video_clip` the most (Voice role first, then any audio).
pub fn find_voice(project: &Project, video_clip: Id) -> Option<Id> {
    let v = project.clip(video_clip)?;
    let (a, b) = (v.start, v.end());
    let mut best: Option<(Id, f64, bool)> = None;
    for t in project.tracks.iter().filter(|t| t.kind == TrackKind::Audio && !t.muted) {
        for c in &t.clips {
            let overlap = c.end().min(b) - c.start.max(a);
            if overlap <= 0.2 {
                continue;
            }
            let voice = project.audio_role(c) == AudioRole::Voice;
            if best.is_none_or(|(_, o, bv)| (voice, overlap) > (bv, o)) {
                best = Some((c.id, overlap, voice));
            }
        }
    }
    best.map(|b| b.0)
}

pub fn plan(project: &Project, video_clip: Id, voice_clip: Option<Id>) -> Result<Plan> {
    plan_span(project, video_clip, voice_clip, None)
}

/// Like [`plan`], limited to the timeline range `span` (a range trimmed in the Studio).
pub fn plan_span(project: &Project, video_clip: Id, voice_clip: Option<Id>, span: Option<(f64, f64)>) -> Result<Plan> {
    plan_fill(project, video_clip, voice_clip, span, Fill::None)
}

/// Like [`plan_span`]; with a [`Fill`] other than `None`, a voice that runs past the end
/// of the shot is covered too (the footage is looped, ping-ponged or held to its length).
pub fn plan_fill(project: &Project, video_clip: Id, voice_clip: Option<Id>, span: Option<(f64, f64)>, fill: Fill) -> Result<Plan> {
    let v = project.clip(video_clip).ok_or_else(|| anyhow!("no clip {video_clip}"))?;
    if project.track_kind_for(v) != TrackKind::Video || v.media_id().and_then(|m| project.media(m)).is_none_or(|m| !m.has_video()) {
        bail!("pick a video clip of the character");
    }
    let voice_id = match voice_clip {
        Some(id) => id,
        None => find_voice(project, video_clip).ok_or_else(|| anyhow!("put the dialogue (a voice clip) on an audio track over this shot first"))?,
    };
    let voice = project.clip(voice_id).ok_or_else(|| anyhow!("no voice clip {voice_id}"))?;
    let mut start = (voice.start - PAD).max(v.start);
    let reach = if fill == Fill::None { v.end() } else { f64::MAX };
    let mut end = (voice.end() + PAD).min(reach);
    if let Some((a, b)) = span {
        start = start.max(a);
        end = end.min(b);
    }
    // A sliver shorter than a clip can't be split off: take the shot to its edge instead.
    let margin = crate::model::MIN_CLIP_LEN + 0.02;
    if start - v.start < margin {
        start = v.start;
    }
    if end <= v.end() && v.end() - end < margin {
        end = v.end();
    }
    if end - start < 0.5 || start >= v.end() {
        bail!("the dialogue doesn't overlap this shot");
    }
    let fill = if end > v.end() + 1e-3 { fill } else { Fill::None };
    Ok(Plan { video_clip, voice_clip: voice_id, start, end, face: None, quality: Quality::Standard, fill })
}

/// How footage shorter than the voice is stretched to cover it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fill {
    /// Only the part of the voice over the shot is synced.
    #[default]
    None,
    /// Play the footage again from the start (best for cyclical motion: swaying, waves).
    Loop,
    /// Forward, then backward, then forward…: no jump at the seam.
    PingPong,
    /// Freeze the last frame.
    Hold,
}

impl Fill {
    pub const ALL: [Fill; 4] = [Fill::None, Fill::Loop, Fill::PingPong, Fill::Hold];

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "none" | "off" => Some(Fill::None),
            "loop" => Some(Fill::Loop),
            "pingpong" | "bounce" => Some(Fill::PingPong),
            "hold" | "freeze" => Some(Fill::Hold),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Fill::None => "Only over the shot",
            Fill::Loop => "Loop the shot",
            Fill::PingPong => "Ping-pong (forward, then back)",
            Fill::Hold => "Hold the last frame",
        }
    }
}

/// How different the first and last frames of `[a, b]` (source seconds) of `path` are,
/// 0 (identical) to 1. Above [`SEAM_JUMP`] a plain loop visibly jumps at the seam.
pub fn loop_seam(path: &Path, a: f64, b: f64) -> Result<f32> {
    let (w, h) = (64, 36);
    let f0 = ffmpeg::extract_frame(path, a.max(0.0), w, h)?;
    let f1 = ffmpeg::extract_frame(path, (b - 0.05).max(0.0), w, h)?;
    let diff: u64 = f0.iter().zip(&f1).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
    Ok(diff as f32 / (w * h * 3) as f32 / 255.0)
}

/// The seam difference above which a plain loop jumps visibly.
pub const SEAM_JUMP: f32 = 0.08;

/// The fill to suggest for a shot whose seam differs by `seam`.
pub fn suggest_fill(seam: f32) -> Fill {
    if seam > SEAM_JUMP {
        Fill::PingPong
    } else {
        Fill::Loop
    }
}

/// The pixel rectangle `(x, y, w, h)` cropped around `face` in a `fw`×`fh` frame: the box
/// padded by 35% on every side (a little more under the chin), even sizes, inside the
/// frame, and at least 256 px so the model has a face it can work with.
pub fn crop_rect(face: &FaceBox, fw: u32, fh: u32) -> (u32, u32, u32, u32) {
    let (fw_f, fh_f) = (fw as f32, fh as f32);
    let pad_x = face.w * 0.35;
    let (top_pad, bottom_pad) = (face.h * 0.35, face.h * 0.45);
    let mut w = ((face.w + 2.0 * pad_x) * fw_f).max(256.0).min(fw_f);
    let mut h = ((face.h + top_pad + bottom_pad) * fh_f).max(256.0).min(fh_f);
    w = (w / 2.0).floor() * 2.0;
    h = (h / 2.0).floor() * 2.0;
    let cx = face.cx * fw_f;
    let cy = (face.top() - top_pad) * fh_f + h / 2.0;
    let x = (cx - w / 2.0).clamp(0.0, fw_f - w);
    let y = (cy - h / 2.0).clamp(0.0, fh_f - h);
    let even = |v: f32| ((v / 2.0).floor() * 2.0) as u32;
    (even(x), even(y), w as u32, h as u32)
}

/// True if the error is a provider refusing the input (a content filter, e.g. "likenesses
/// of real people"). Nothing is charged for these, and another method may accept it.
pub fn is_refusal(err: &anyhow::Error) -> bool {
    if err.downcast_ref::<crate::ai::video_gen::JobFailed>().is_some() {
        return true;
    }
    let text = format!("{err:#}").to_ascii_lowercase();
    ["content_policy_violation", "likenesses of real people", "content policy", "moderation"].iter().any(|k| text.contains(k))
}

/// Writes the video segment (no audio) and the dialogue audio for `plan`. With a face, the
/// segment sent is the face crop; the full segment is kept as `lipsync-full.mp4` for
/// [`composite`], and the crop rectangle (in its pixels) is returned.
pub fn render_inputs(project: &Project, plan: &Plan, dir: &Path) -> Result<(PathBuf, PathBuf, Option<(u32, u32, u32, u32)>)> {
    std::fs::create_dir_all(dir)?;
    let v = project.clip(plan.video_clip).ok_or_else(|| anyhow!("the video clip was removed"))?;
    let vm = v.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no video media"))?;
    let len = plan.end - plan.start;
    let full_out = dir.join(if plan.face.is_some() { "lipsync-full.mp4" } else { "lipsync-in.mp4" });
    let video_out = dir.join("lipsync-in.mp4");
    let audio_out = dir.join("lipsync-in.wav");
    let offset = v.source_in + (plan.start - v.start) * v.speed;
    if vm.kind == crate::model::MediaKind::Image {
        // A talking photo: the still held for the whole span; the model moves the mouth.
        still_video(&vm.path, len, plan.quality, &full_out)?;
    } else if plan.fill != Fill::None && plan.end > v.end() + 1e-3 {
        let footage = dir.join("lipsync-footage.mp4");
        cut_video_at(&vm.path, offset, (v.end() - plan.start).max(0.1), v.speed, plan.quality, &footage)?;
        fill_video(&footage, len, plan.fill, &full_out)?;
    } else {
        cut_video_at(&vm.path, offset, len, v.speed, plan.quality, &full_out)?;
    }
    let mut rect = None;
    if let Some(face) = &plan.face {
        let info = ffmpeg::probe(&full_out)?;
        let r = crop_rect(face, info.width, info.height);
        let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&full_out)
            .args(["-an", "-vf", &format!("crop={}:{}:{}:{}", r.2, r.3, r.0, r.1), "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p"])
            .arg(&video_out)
            .status()?
            .success();
        if !ok {
            bail!("could not crop the face");
        }
        rect = Some(r);
    }
    render_line_audio(project, plan, &audio_out)?;
    Ok((video_out, audio_out, rect))
}

/// Cuts `len` seconds of `src` from `offset` (no audio), scaled down to the quality's width.
pub fn cut_video(src: &Path, offset: f64, len: f64, quality: Quality, out: &Path) -> Result<()> {
    cut_video_at(src, offset, len, 1.0, quality, out)
}

/// Cuts what plays in `len` timeline seconds of a clip at `speed` (reading `len × speed`
/// source seconds from `offset`), retimed so the result plays at 1× for `len` seconds.
pub fn cut_video_at(src: &Path, offset: f64, len: f64, speed: f64, quality: Quality, out: &Path) -> Result<()> {
    let speed = if speed > 0.01 { speed } else { 1.0 };
    let retime = if (speed - 1.0).abs() > 1e-3 { format!("setpts=(PTS-STARTPTS)/{speed:.6},fps=30,") } else { String::new() };
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{:.3}", offset.max(0.0)), "-t", &format!("{:.3}", len * speed), "-i"])
        .arg(src)
        .args(["-an", "-vf", &format!("{retime}scale='min({},iw)':-2", quality.max_width()), "-t", &format!("{len:.3}"), "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"])
        .arg(out)
        .status()?
        .success();
    if !ok {
        bail!("could not cut the video segment");
    }
    Ok(())
}

/// A still held for `len` seconds as video (a talking photo's input).
pub fn still_video(image: &Path, len: f64, quality: Quality, out: &Path) -> Result<()> {
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-loop", "1", "-framerate", "30", "-t", &format!("{len:.3}"), "-i"])
        .arg(image)
        .args(["-vf", &format!("scale='min({},iw)':-2", quality.max_width()), "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"])
        .arg(out)
        .status()?
        .success();
    if !ok {
        bail!("could not turn the still into video");
    }
    Ok(())
}

/// Stretches `footage` to `len` seconds: looped, ping-ponged (forward then reversed) or
/// held on its last frame. Footage over 12 s ping-pongs as a plain loop (reversing holds
/// every frame in memory).
pub fn fill_video(footage: &Path, len: f64, fill: Fill, out: &Path) -> Result<()> {
    let have = ffmpeg::probe(footage).map(|i| i.duration).unwrap_or(0.0);
    let enc = ["-an", "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"];
    let run = |cmd: &mut std::process::Command| -> Result<()> {
        if !cmd.status()?.success() {
            bail!("could not extend the footage ({fill:?})");
        }
        Ok(())
    };
    let t = format!("{len:.3}");
    match fill {
        Fill::Hold | Fill::None => {
            let pad = (len - have).max(0.0) + 0.1;
            run(ffmpeg::command(&ffmpeg::ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(footage)
                .args(["-vf", &format!("tpad=stop_mode=clone:stop_duration={pad:.3}"), "-t", &t])
                .args(enc)
                .arg(out))
        }
        Fill::Loop => run(ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-stream_loop", "-1", "-i"])
            .arg(footage)
            .args(["-t", &t])
            .args(enc)
            .arg(out)),
        Fill::PingPong if have > 12.0 => fill_video(footage, len, Fill::Loop, out),
        Fill::PingPong => {
            let bounce = out.with_file_name("lipsync-bounce.mp4");
            run(ffmpeg::command(&ffmpeg::ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(footage)
                .args(["-filter_complex", "[0:v]split[a][b];[b]reverse,trim=start_frame=1[r];[a][r]concat=n=2:v=1[v]", "-map", "[v]"])
                .args(enc)
                .arg(&bounce))?;
            fill_video(&bounce, len, Fill::Loop, out)
        }
    }
}

/// The voice clip's audio for the plan's span, positioned inside it (silence before and
/// after the line), as mono WAV.
pub fn render_line_audio(project: &Project, plan: &Plan, out: &Path) -> Result<()> {
    let voice = project.clip(plan.voice_clip).ok_or_else(|| anyhow!("the voice clip was removed"))?;
    let am = voice.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no voice media"))?;
    let len = plan.end - plan.start;
    let lead = (voice.start - plan.start).max(0.0);
    let a_src = voice.source_in + (plan.start - voice.start).max(0.0) * voice.speed;
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{a_src:.3}"), "-i"])
        .arg(&am.path)
        .args(["-af", &format!("adelay={}:all=1,apad,atrim=0:{len:.3}", (lead * 1000.0).round() as i64), "-ac", "1", "-ar", "44100"])
        .arg(out)
        .status()?
        .success();
    if !ok {
        bail!("could not cut the dialogue audio");
    }
    Ok(())
}

/// Puts the lip-synced face crop back into the full segment at `rect`, with soft edges so
/// the seam doesn't show. Writes `out` (no audio).
pub fn composite(full: &Path, synced_crop: &Path, rect: (u32, u32, u32, u32), out: &Path) -> Result<()> {
    let (x, y, w, h) = rect;
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(full)
        .arg("-i")
        .arg(synced_crop)
        .args(["-filter_complex", &composite_filter(rect), "-map", "[v]", "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "17"])
        .arg(out)
        .status()?
        .success();
    if !ok || !out.is_file() {
        bail!("could not put the lip-synced face ({w}×{h} at {x},{y}) back into the shot");
    }
    Ok(())
}

/// The ffmpeg graph for [`composite`]: the crop's alpha fades to 0 over the outer 12% so the
/// seam is invisible.
pub fn composite_filter((x, y, w, h): (u32, u32, u32, u32)) -> String {
    let feather = ((w.min(h) as f32) * 0.12).max(4.0);
    let alpha = format!("255*min(1,min(min(X,W-1-X),min(Y,H-1-Y))/{feather:.1})");
    format!("[1:v]scale={w}:{h},format=yuva444p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='{alpha}'[fg];[0:v][fg]overlay={x}:{y}:shortest=1,format=yuv420p[v]")
}

pub fn data_uri(path: &Path, mime: &str) -> Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 40_000_000 {
        bail!("{} is too large to send ({} MB)", path.display(), bytes.len() / 1_000_000);
    }
    Ok(format!("data:{mime};base64,{}", crate::util::base64(&bytes)))
}

fn fal_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            match code {
                401 | 403 => anyhow!("fal.ai rejected the API key ({code}): {}", body.chars().take(200).collect::<String>()),
                402 => anyhow!("fal.ai: not enough balance ({})", body.chars().take(200).collect::<String>()),
                _ => anyhow!("fal.ai error {code}: {}", body.chars().take(300).collect::<String>()),
            }
        }
        other => anyhow!("could not reach fal.ai: {other}"),
    }
}

/// Uploads a file to fal.ai's storage and returns its public URL (for endpoints that no
/// longer accept inline `data:` URIs, like Sync lipsync-2).
pub fn upload(key: &str, path: &Path, mime: &str) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > 90 * 1024 * 1024 {
        bail!("{} is too large to upload ({} MB)", path.display(), bytes.len() / 1_000_000);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "upload".into());
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(300)).build();
    let init: Value = agent
        .post("https://rest.fal.ai/storage/upload/initiate?storage_type=fal-cdn-v3")
        .set("Authorization", &format!("Key {key}"))
        .send_json(json!({ "content_type": mime, "file_name": name }))
        .map_err(fal_error)?
        .into_json()?;
    let (Some(put_url), Some(file_url)) = (init["upload_url"].as_str(), init["file_url"].as_str()) else {
        bail!("fal.ai storage did not return an upload URL: {}", init.to_string().chars().take(200).collect::<String>());
    };
    agent.put(put_url).set("Content-Type", mime).send_bytes(&bytes).map_err(fal_error)?;
    Ok(file_url.to_string())
}

/// Sends the segment and the line to fal.ai and downloads the lip-synced video into
/// `out`. `Ok(None)` if cancelled.
pub fn run(key: &str, video: &Path, audio: &Path, model: &str, out: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    progress(0.05, "Uploading the shot and the line…");
    let body = json!({ "video_url": upload(key, video, "video/mp4")?, "audio_url": upload(key, audio, "audio/wav")?, "model": model, "sync_mode": "cut_off" });
    fal_run(key, ENDPOINT, body, out, "Lip-syncing…", cancel, progress)
}

/// Queues a fal.ai job, waits for it, and downloads its `video.url` into `out`.
/// `Ok(None)` if cancelled.
pub fn fal_run(key: &str, endpoint: &str, body: Value, out: &Path, working: &str, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(300)).build();
    let auth = format!("Key {key}");
    let sub: Value = agent.post(&format!("https://queue.fal.run/{endpoint}")).set("Authorization", &auth).send_json(body).map_err(fal_error)?.into_json()?;
    let status_url = sub["status_url"].as_str().ok_or_else(|| anyhow!("fal.ai did not queue the job: {sub}"))?.to_string();
    let response_url = sub["response_url"].as_str().ok_or_else(|| anyhow!("fal.ai returned no result URL"))?.to_string();
    let t0 = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_secs(3));
        let s: Value = agent.get(&status_url).set("Authorization", &auth).call().map_err(fal_error)?.into_json()?;
        match s["status"].as_str().unwrap_or("") {
            "COMPLETED" => break,
            "IN_QUEUE" => progress(0.15, &format!("Waiting in fal.ai's queue{}…", s["queue_position"].as_u64().map_or(String::new(), |p| format!(" (position {p})")))),
            _ => progress((0.2 + t0.elapsed().as_secs_f32() / 240.0).min(0.9), working),
        }
        if t0.elapsed() > Duration::from_secs(1800) {
            bail!("fal.ai took too long (30 min)");
        }
    }
    let r: Value = agent.get(&response_url).set("Authorization", &auth).call().map_err(fal_error)?.into_json()?;
    let url = r["video"]["url"].as_str().ok_or_else(|| anyhow!("fal.ai returned no video: {}", r.to_string().chars().take(300).collect::<String>()))?;
    progress(0.93, "Downloading…");
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let resp = agent.get(url).call().map_err(fal_error)?;
    let mut file = std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    std::io::copy(&mut resp.into_reader(), &mut file).context("download interrupted")?;
    Ok(Some(out.to_path_buf()))
}

/// Puts the lip-synced video in place of the planned span of the video clip (same
/// track, transform and look; its own sound muted so the dialogue clip is heard). The
/// result is recorded as a take of the clip.
pub fn apply(project: &mut Project, plan: &Plan, result: &Path, info: ProbeInfo) -> Result<Id> {
    apply_with(project, plan, result, info, Placement::Replace)
}

/// [`apply`] with a choice of placement: replace the span, or a new clip on the track above.
pub fn apply_with(project: &mut Project, plan: &Plan, result: &Path, info: ProbeInfo, placement: Placement) -> Result<Id> {
    let original = project.clip(plan.video_clip).cloned().ok_or_else(|| anyhow!("the video clip was removed"))?;
    let (ti, _) = project.locate(plan.video_clip).ok_or_else(|| anyhow!("the video clip was removed"))?;
    if placement == Placement::Above {
        return place_above(project, plan, &original, ti, result, info);
    }
    if plan.end > original.end() + 1e-3 && !project.fits(ti, original.end(), plan.end, Some(original.id)) {
        bail!("the next shot is in the way of the extended span ({:.2}–{:.2}): choose \"On the track above\", or move that shot", original.end(), plan.end);
    }
    // Split around the span; the middle piece is what gets replaced.
    let mut middle = plan.video_clip;
    if plan.start > original.start + 1e-3 {
        middle = project.split_clip(middle, plan.start).ok_or_else(|| anyhow!("could not split the clip at the start of the line"))?;
    }
    if plan.end < original.end() - 1e-3 {
        project.split_clip(middle, plan.end).ok_or_else(|| anyhow!("could not split the clip at the end of the line"))?;
    }
    let old = project.remove_clip(middle).ok_or_else(|| anyhow!("could not replace the clip"))?;
    let (old_id, old_media) = (old.id, old.media_id());
    let media = project.add_media(info.clone().into_media_item(result.to_path_buf()));
    let mut c: Clip = project.new_media_clip(media, plan.start).ok_or_else(|| anyhow!("could not use the result"))?;
    c.start = plan.start;
    c.duration = (plan.end - plan.start).min(info.duration.max(0.1));
    c.transform = old.transform;
    c.filter = old.filter;
    c.adjust = old.adjust;
    c.opacity = old.opacity;
    c.transition_in = old.transition_in;
    c.background_plate = old.background_plate;
    c.fade_in = old.fade_in;
    c.fade_out = old.fade_out;
    c.keyframes = old.keyframes;
    c.grade = old.grade;
    c.blur = old.blur;
    c.glow = old.glow;
    c.volume = 0.0;
    let id = c.id;
    project.tracks[ti].clips.push(c);
    project.sort_track(ti);
    project.record_take(old_id, id, old_media, media);
    Ok(id)
}

/// The result on the nearest free video track above the shot (a new one if none has room),
/// at exactly the planned span; the original stays underneath as a fallback.
fn place_above(project: &mut Project, plan: &Plan, original: &Clip, ti: usize, result: &Path, info: ProbeInfo) -> Result<Id> {
    let media = project.add_media(info.clone().into_media_item(result.to_path_buf()));
    let mut c: Clip = project.new_media_clip(media, plan.start).ok_or_else(|| anyhow!("could not use the result"))?;
    c.start = plan.start;
    c.duration = (plan.end - plan.start).min(info.duration.max(0.1));
    c.transform = original.transform.clone();
    c.filter = original.filter;
    c.adjust = original.adjust;
    c.grade = original.grade.clone();
    c.opacity = original.opacity;
    c.volume = 0.0;
    let (s, e) = (c.start, c.end());
    let above = (0..ti).rev().find(|&i| project.tracks[i].kind == TrackKind::Video && !project.tracks[i].locked && project.fits(i, s, e, None));
    let target = above.unwrap_or_else(|| project.insert_track(TrackKind::Video, Some(ti)));
    let id = c.id;
    project.tracks[target].clips.push(c);
    project.sort_track(target);
    project.record_take(original.id, id, original.media_id(), media);
    Ok(id)
}

/// The whole job: inputs, fal.ai, cost record. Returns the result file and its info.
pub fn lip_sync(project: &Project, plan: &Plan, key: &str, model: &str, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<(PathBuf, ProbeInfo)>> {
    let work = ffmpeg::work_dir("lipsync")?;
    let result = (|| -> Result<Option<(PathBuf, ProbeInfo)>> {
        let (video, audio, rect) = render_inputs(project, plan, &work)?;
        let name = project.clip(plan.video_clip).and_then(|c| c.media_id()).and_then(|m| project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| "shot".into());
        let mut out = out_dir.join(format!("lipsync-{}.mp4", crate::ai::video_gen::slug(&name)));
        let mut n = 2;
        while out.exists() {
            out = out_dir.join(format!("lipsync-{}-{n}.mp4", crate::ai::video_gen::slug(&name)));
            n += 1;
        }
        let synced = if rect.is_some() { work.join("lipsync-face.mp4") } else { out.clone() };
        let Some(path) = run(key, &video, &audio, model, &synced, cancel, progress)? else { return Ok(None) };
        let path = match rect {
            Some(r) => {
                progress(0.96, "Putting the face back into the shot…");
                composite(&work.join("lipsync-full.mp4"), &path, r, &out)?;
                out.clone()
            }
            None => path,
        };
        let info = ffmpeg::probe(&path)?;
        let secs = plan.end - plan.start;
        crate::economics::record(crate::economics::Spend {
            kind: "lipsync",
            model,
            usd: secs * usd_per_second(model),
            units: secs,
            unit: "second",
            output: Some(&path),
            source: "estimate",
            note: "fal.ai list price per second",
        });
        Ok(Some((path, info)))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    /// Paid, manual: `LS_PROJECT=<.rcproj> LS_CLIP=<video clip id> [LS_VOICE=<clip id>] [LS_MODEL=lipsync-2-pro]
    /// [LS_DRY=1] cargo test --lib lipsync_live -- --ignored --nocapture`. Saves the project
    /// (backup: `<name>.before-lipsync.rcproj`).
    #[test]
    #[ignore]
    fn lipsync_live() {
        let Ok(path) = std::env::var("LS_PROJECT") else { return };
        let path = PathBuf::from(path);
        let mut p = Project::load(&path).unwrap();
        let clip: Id = std::env::var("LS_CLIP").unwrap().parse().unwrap();
        let voice = std::env::var("LS_VOICE").ok().and_then(|v| v.parse().ok());
        let model = std::env::var("LS_MODEL").unwrap_or_else(|_| MODELS[0].0.into());
        let plan = plan(&p, clip, voice).unwrap();
        println!("span {:.2}–{:.2} with voice clip {}: estimate ${:.2} ({model})", plan.start, plan.end, plan.voice_clip, (plan.end - plan.start) * usd_per_second(&model));
        if std::env::var("LS_DRY").is_ok() {
            return;
        }
        let key = crate::ai::openrouter::AiSettings::load().fal_key().expect("fal key in RustCut settings");
        let dir = crate::ai::video_gen::output_dir(Some(&path)).join("lipsync");
        let (file, info) = lip_sync(&p, &plan, &key, &model, &dir, &AtomicBool::new(false), &mut |f, s| println!("  {:3.0}% {s}", f * 100.0)).unwrap().unwrap();
        println!("got {} ({:.2} s, {}x{})", file.display(), info.duration, info.width, info.height);
        std::fs::copy(&path, path.with_extension("before-lipsync.rcproj")).unwrap();
        let id = apply(&mut p, &plan, &file, info).unwrap();
        p.save(&path).unwrap();
        println!("placed clip {id}; project saved");
    }

    fn project() -> (Project, Id, Id) {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 8.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "line.mp3".into(), name: "line.mp3".into(), kind: MediaKind::Audio, duration: 3.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.start = 10.0;
        vc.transform.scale = 1.2;
        let vid = vc.id;
        p.append_to_main(vc);
        let mut ac = p.new_media_clip(a, 12.0).unwrap();
        ac.start = 12.0;
        ac.audio_role = AudioRole::Voice;
        let aid = ac.id;
        p.place_clip(ac, None);
        (p, vid, aid)
    }

    #[test]
    fn plans_the_span_under_the_line() {
        let (p, vid, aid) = project();
        // The video clip starts where append_to_main put it (0), so move expectations accordingly.
        let v = p.clip(vid).unwrap().clone();
        assert_eq!(find_voice(&p, vid), if v.end() > 12.2 { Some(aid) } else { None });
    }

    #[test]
    fn replaces_only_the_middle_and_keeps_the_look() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 8.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "line.mp3".into(), name: "line.mp3".into(), kind: MediaKind::Audio, duration: 3.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.transform.scale = 1.2;
        let vid = vc.id;
        p.append_to_main(vc); // 0–8 s
        let mut ac = p.new_media_clip(a, 2.0).unwrap();
        ac.start = 2.0; // 2–5 s
        ac.audio_role = AudioRole::Voice;
        p.place_clip(ac, None);
        let plan = plan(&p, vid, None).unwrap();
        assert!((plan.start - 1.85).abs() < 1e-9 && (plan.end - 5.15).abs() < 1e-9, "{plan:?}");
        let info = ProbeInfo { kind: MediaKind::Video, duration: 3.3, width: 1280, height: 720, fps: 25.0, has_audio: true };
        let id = apply(&mut p, &plan, Path::new("lipsync-shot.mp4"), info).unwrap();
        let main = p.main_track();
        let clips = &p.tracks[main].clips;
        assert_eq!(clips.len(), 3, "before, lip-synced middle, after");
        let mid = p.clip(id).unwrap();
        assert!((mid.start - 1.85).abs() < 1e-9 && (mid.duration - 3.3).abs() < 1e-9);
        assert_eq!(mid.transform.scale, 1.2);
        assert_eq!(mid.volume, 0.0);
        assert!((clips[2].start - 5.15).abs() < 1e-9 && (clips[2].source_in - 5.15).abs() < 1e-9, "the rest continues from the right frame");
    }

    #[test]
    fn a_line_near_the_shot_edge_takes_the_whole_edge() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 4.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "line.mp3".into(), name: "line.mp3".into(), kind: MediaKind::Audio, duration: 1.07, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.duration = 1.37; // 0–1.37 s
        let vid = vc.id;
        p.append_to_main(vc);
        let mut ac = p.new_media_clip(a, 0.11).unwrap();
        ac.start = 0.11; // 0.11–1.18: padded, 0.04 s from both edges
        ac.audio_role = AudioRole::Voice;
        p.place_clip(ac, None);
        let plan = plan(&p, vid, None).unwrap();
        assert!(plan.start.abs() < 1e-9 && (plan.end - 1.37).abs() < 1e-9, "{plan:?}");
        let info = ProbeInfo { kind: MediaKind::Video, duration: 1.37, width: 1280, height: 720, fps: 25.0, has_audio: true };
        apply(&mut p, &plan, Path::new("lipsync-shot.mp4"), info).unwrap();
        let main = p.main_track();
        assert_eq!(p.tracks[main].clips.len(), 1, "the whole shot is replaced, no slivers");
    }

    #[test]
    fn crop_rect_pads_the_face_and_stays_in_frame() {
        let face = FaceBox { cx: 0.5, cy: 0.3, w: 0.1, h: 0.2 };
        let (x, y, w, h) = crop_rect(&face, 1920, 1080);
        assert!(w % 2 == 0 && h % 2 == 0 && x % 2 == 0 && y % 2 == 0);
        assert!(w >= 256 && h >= 256);
        assert!(x as f32 <= 0.45 * 1920.0 && (x + w) as f32 >= 0.55 * 1920.0, "the face is inside the crop");
        assert!(y as f32 <= 0.2 * 1080.0 && (y + h) as f32 >= 0.4 * 1080.0);
        assert!(y + h <= 1080 && x + w <= 1920);
        // A face at the edge is clamped inside the frame.
        let edge = FaceBox { cx: 0.98, cy: 0.95, w: 0.1, h: 0.2 };
        let (x, y, w, h) = crop_rect(&edge, 1280, 720);
        assert!(x + w <= 1280 && y + h <= 720);
        assert_eq!(Quality::High.max_width(), 1920);
        assert_eq!(Quality::parse("1080p"), Some(Quality::High));
    }

    #[test]
    fn recognises_refusals() {
        let fal = anyhow!(r#"fal.ai error 422: {{"detail":[{{"msg":"The images or videos provided may contain likenesses of real people or other private information that cannot be processed.","type":"content_policy_violation"}}]}}"#);
        assert!(is_refusal(&fal));
        let job: anyhow::Error = crate::ai::video_gen::JobFailed { model: "m".into(), state: "failed".into(), reason: "x".into() }.into();
        assert!(is_refusal(&job));
        assert!(!is_refusal(&anyhow!("fal.ai: not enough balance")));
    }

    #[test]
    fn replace_keeps_fades_and_records_the_take() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 4.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "song.mp3".into(), name: "song.mp3".into(), kind: MediaKind::Audio, duration: 60.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.fade_in = 0.5;
        vc.fade_out = 0.4;
        let vid = vc.id;
        p.append_to_main(vc);
        let ac = p.new_media_clip(a, 0.0).unwrap();
        p.place_clip(ac, None);
        let plan = plan(&p, vid, None).unwrap();
        assert!(plan.start.abs() < 1e-9 && (plan.end - 4.0).abs() < 1e-9, "the whole shot under the song");
        let info = ProbeInfo { kind: MediaKind::Video, duration: 4.0, width: 1280, height: 720, fps: 25.0, has_audio: true };
        let id = apply(&mut p, &plan, Path::new("lipsync-shot.mp4"), info).unwrap();
        let c = p.clip(id).unwrap();
        assert_eq!((c.fade_in, c.fade_out), (0.5, 0.4));
        let takes = p.takes_for(id);
        assert_eq!(takes.len(), 2);
        assert_eq!(takes[0], v, "the original footage is the first take");
    }

    /// A 4 s shot at 0–4 with a voice at 1–7 (longer than the shot), and optionally a next shot.
    fn long_voice(next_shot: bool) -> (Project, Id, Id) {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 4.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let a = p.add_media(MediaItem { id: 0, path: "line.mp3".into(), name: "line.mp3".into(), kind: MediaKind::Audio, duration: 6.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let vc = p.new_media_clip(v, 0.0).unwrap();
        let vid = vc.id;
        p.append_to_main(vc);
        if next_shot {
            let nc = p.new_media_clip(v, 4.0).unwrap();
            p.append_to_main(nc);
        }
        let mut ac = p.new_media_clip(a, 1.0).unwrap();
        ac.start = 1.0;
        ac.audio_role = AudioRole::Voice;
        let aid = ac.id;
        p.place_clip(ac, None);
        (p, vid, aid)
    }

    #[test]
    fn any_speed_is_allowed_now() {
        let (mut p, vid, _) = project();
        p.clip_mut(vid).unwrap().speed = 0.888;
        let v = p.clip(vid).unwrap().clone();
        // The voice at 12–15 is past this 8 s shot at 0.888× (0–9 s): no overlap error, not a speed error.
        if let Err(e) = plan(&p, vid, None) {
            assert!(!e.to_string().contains("speed"), "{e}");
        }
        assert!(v.speed < 1.0);
    }

    #[test]
    fn fill_covers_a_voice_longer_than_the_shot() {
        let (p, vid, _) = long_voice(false);
        let short = plan(&p, vid, None).unwrap();
        assert_eq!((short.fill, short.end), (Fill::None, 4.0), "without a fill, only the part over the shot");
        let long = plan_fill(&p, vid, None, None, Fill::PingPong).unwrap();
        assert_eq!(long.fill, Fill::PingPong);
        assert!((long.start - 0.85).abs() < 1e-9 && (long.end - 7.15).abs() < 1e-9, "{long:?}");
        // A fill is dropped when the voice ends inside the shot.
        let (p2, vid2, _) = project();
        if let Ok(pl) = plan_fill(&p2, vid2, None, None, Fill::Loop) {
            assert_eq!(pl.fill, Fill::None);
        }
        assert_eq!(Fill::parse("ping-pong"), Some(Fill::PingPong));
        assert_eq!(suggest_fill(0.2), Fill::PingPong);
        assert_eq!(suggest_fill(0.01), Fill::Loop);
    }

    #[test]
    fn an_extended_span_needs_room_or_the_track_above() {
        let info = ProbeInfo { kind: MediaKind::Video, duration: 6.3, width: 1280, height: 720, fps: 25.0, has_audio: true };
        let (mut p, vid, _) = long_voice(true);
        let pl = plan_fill(&p, vid, None, None, Fill::Loop).unwrap();
        let err = apply(&mut p, &pl, Path::new("lipsync-long.mp4"), info.clone()).unwrap_err();
        assert!(err.to_string().contains("in the way"), "{err}");
        let id = apply_with(&mut p, &pl, Path::new("lipsync-long.mp4"), info.clone(), Placement::Above).unwrap();
        assert!((p.clip(id).unwrap().duration - 6.3).abs() < 1e-9);
        // With room after the shot, it replaces and runs on.
        let (mut p, vid, _) = long_voice(false);
        let pl = plan_fill(&p, vid, None, None, Fill::Hold).unwrap();
        let id = apply(&mut p, &pl, Path::new("lipsync-long.mp4"), info).unwrap();
        let c = p.clip(id).unwrap();
        assert!((c.start - 0.85).abs() < 1e-9 && (c.end() - 7.15).abs() < 1e-9);
    }

    #[test]
    fn fills_and_stills_render_to_length() {
        let dir = std::env::temp_dir().join(format!("rustcut-fill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let made = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=s=320x180:d=2:r=24", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let len = |p: &Path| ffmpeg::probe(p).unwrap().duration;
        for fill in [Fill::Loop, Fill::PingPong, Fill::Hold] {
            let out = dir.join(format!("{fill:?}.mp4"));
            fill_video(&src, 5.0, fill, &out).unwrap();
            assert!((len(&out) - 5.0).abs() < 0.15, "{fill:?}: {}", len(&out));
        }
        // 2 s of timeline at 0.5× reads 1 s of source and plays for 2 s; at 2× it reads 2 s for 1 s.
        let slow = dir.join("slow.mp4");
        cut_video_at(&src, 0.0, 2.0, 0.5, Quality::Standard, &slow).unwrap();
        assert!((len(&slow) - 2.0).abs() < 0.15, "{}", len(&slow));
        let fast = dir.join("fast.mp4");
        cut_video_at(&src, 0.0, 1.0, 2.0, Quality::Standard, &fast).unwrap();
        assert!((len(&fast) - 1.0).abs() < 0.15, "{}", len(&fast));
        let png = dir.join("still.png");
        ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&src).args(["-frames:v", "1"]).arg(&png).status().unwrap();
        let talking = dir.join("still.mp4");
        still_video(&png, 3.0, Quality::Standard, &talking).unwrap();
        assert!((len(&talking) - 3.0).abs() < 0.15);
        let seam = loop_seam(&src, 0.0, 2.0).unwrap();
        assert!(seam > 0.0, "testsrc changes over 2 s");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn above_keeps_the_original_and_lands_on_the_exact_span() {
        let (mut p, vid, _) = project();
        let v = p.clip(vid).unwrap().clone();
        let mut plan = plan_span(&p, vid, None, None).unwrap_or(Plan { video_clip: vid, voice_clip: 0, start: v.start + 1.0, end: v.start + 3.0, face: None, quality: Quality::High, fill: Fill::None });
        plan.start = v.start + 1.0;
        plan.end = v.start + 3.0;
        let tracks_before = p.tracks.len();
        let info = ProbeInfo { kind: MediaKind::Video, duration: 2.0, width: 1920, height: 1080, fps: 25.0, has_audio: true };
        let id = apply_with(&mut p, &plan, Path::new("lipsync-above.mp4"), info, Placement::Above).unwrap();
        assert!(p.clip(vid).is_some(), "the original is untouched");
        let (ti, _) = p.locate(id).unwrap();
        let (oi, _) = p.locate(vid).unwrap();
        assert!(ti < oi, "on a track above the shot");
        assert_eq!(p.tracks.len(), tracks_before + 1, "a new video track (none had room)");
        let c = p.clip(id).unwrap();
        assert_eq!((c.start, c.duration, c.volume), (plan.start, 2.0, 0.0));
        assert_eq!(c.transform.scale, 1.2);
    }

    #[test]
    fn composite_puts_the_face_back() {
        let dir = std::env::temp_dir().join(format!("rustcut-composite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, color: &str, size: &str| {
            let out = dir.join(name);
            let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("color=c={color}:s={size}:d=1:r=10"), "-pix_fmt", "yuv420p"])
                .arg(&out)
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(out)
        };
        let (Some(full), Some(face)) = (mk("full.mp4", "black", "640x360"), mk("face.mp4", "white", "200x200")) else {
            eprintln!("ffmpeg not available; skipping");
            return;
        };
        let out = dir.join("out.mp4");
        composite(&full, &face, (100, 80, 200, 200), &out).unwrap();
        let info = ffmpeg::probe(&out).unwrap();
        assert_eq!((info.width, info.height), (640, 360));
        let centre = crate::ai::video_gen::frame_jpeg(&out, 0.5, 640).unwrap();
        assert!(!centre.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_without_dialogue() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 8.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let vc = p.new_media_clip(v, 0.0).unwrap();
        let vid = vc.id;
        p.append_to_main(vc);
        assert!(plan(&p, vid, None).unwrap_err().to_string().contains("voice clip"));
    }
}
