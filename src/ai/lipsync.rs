//! Lip-sync a character to dialogue on the timeline with fal.ai (Sync lipsync-2):
//! the part of a video clip under a voice clip is sent with that line's audio, and the
//! lip-synced result replaces just that part of the clip.

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

/// What will be lip-synced: the video clip, the voice clip and the timeline span.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub video_clip: Id,
    pub voice_clip: Id,
    pub start: f64,
    pub end: f64,
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
    let v = project.clip(video_clip).ok_or_else(|| anyhow!("no clip {video_clip}"))?;
    if project.track_kind_for(v) != TrackKind::Video || v.media_id().and_then(|m| project.media(m)).is_none_or(|m| !m.has_video()) {
        bail!("pick a video clip of the character");
    }
    let voice_id = match voice_clip {
        Some(id) => id,
        None => find_voice(project, video_clip).ok_or_else(|| anyhow!("put the dialogue (a voice clip) on an audio track over this shot first"))?,
    };
    let voice = project.clip(voice_id).ok_or_else(|| anyhow!("no voice clip {voice_id}"))?;
    let start = (voice.start - PAD).max(v.start);
    let end = (voice.end() + PAD).min(v.end());
    if end - start < 0.5 {
        bail!("the dialogue doesn't overlap this shot");
    }
    if (v.speed - 1.0).abs() > 1e-3 {
        bail!("set this clip's speed to 1× before lip-syncing");
    }
    Ok(Plan { video_clip, voice_clip: voice_id, start, end })
}

/// Writes the video segment (no audio) and the dialogue audio for `plan`.
pub fn render_inputs(project: &Project, plan: &Plan, dir: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let v = project.clip(plan.video_clip).ok_or_else(|| anyhow!("the video clip was removed"))?;
    let vm = v.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no video media"))?;
    let voice = project.clip(plan.voice_clip).ok_or_else(|| anyhow!("the voice clip was removed"))?;
    let am = voice.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no voice media"))?;
    let len = plan.end - plan.start;
    let video_out = dir.join("lipsync-in.mp4");
    let audio_out = dir.join("lipsync-in.wav");
    let src = v.source_in + (plan.start - v.start);
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{src:.3}"), "-t", &format!("{len:.3}"), "-i"])
        .arg(&vm.path)
        .args(["-an", "-vf", "scale='min(1280,iw)':-2", "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"])
        .arg(&video_out)
        .status()?
        .success();
    if !ok {
        bail!("could not cut the video segment");
    }
    // The line, positioned inside the segment (silence before/after it).
    let lead = (voice.start - plan.start).max(0.0);
    let a_src = voice.source_in + (plan.start - voice.start).max(0.0) * voice.speed;
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{a_src:.3}"), "-i"])
        .arg(&am.path)
        .args(["-af", &format!("adelay={}:all=1,apad,atrim=0:{len:.3}", (lead * 1000.0).round() as i64), "-ac", "1", "-ar", "44100"])
        .arg(&audio_out)
        .status()?
        .success();
    if !ok {
        bail!("could not cut the dialogue audio");
    }
    Ok((video_out, audio_out))
}

fn data_uri(path: &Path, mime: &str) -> Result<String> {
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

/// Sends the segment and the line to fal.ai and downloads the lip-synced video into
/// `out`. `Ok(None)` if cancelled.
pub fn run(key: &str, video: &Path, audio: &Path, model: &str, out: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(300)).build();
    let auth = format!("Key {key}");
    progress(0.05, "Uploading the shot and the line…");
    let body = json!({ "video_url": data_uri(video, "video/mp4")?, "audio_url": data_uri(audio, "audio/wav")?, "model": model, "sync_mode": "cut_off" });
    let sub: Value = agent.post(&format!("https://queue.fal.run/{ENDPOINT}")).set("Authorization", &auth).send_json(body).map_err(fal_error)?.into_json()?;
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
            _ => progress((0.2 + t0.elapsed().as_secs_f32() / 240.0).min(0.9), "Lip-syncing…"),
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
/// track, transform and look; its own sound muted so the dialogue clip is heard).
pub fn apply(project: &mut Project, plan: &Plan, result: &Path, info: ProbeInfo) -> Result<Id> {
    let original = project.clip(plan.video_clip).cloned().ok_or_else(|| anyhow!("the video clip was removed"))?;
    let (ti, _) = project.locate(plan.video_clip).ok_or_else(|| anyhow!("the video clip was removed"))?;
    // Split around the span; the middle piece is what gets replaced.
    let mut middle = plan.video_clip;
    if plan.start > original.start + 1e-3 {
        middle = project.split_clip(middle, plan.start).ok_or_else(|| anyhow!("could not split the clip at the start of the line"))?;
    }
    if plan.end < original.end() - 1e-3 {
        project.split_clip(middle, plan.end).ok_or_else(|| anyhow!("could not split the clip at the end of the line"))?;
    }
    let old = project.remove_clip(middle).ok_or_else(|| anyhow!("could not replace the clip"))?;
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
    c.volume = 0.0;
    let id = c.id;
    project.tracks[ti].clips.push(c);
    project.sort_track(ti);
    Ok(id)
}

/// The whole job: inputs, fal.ai, cost record. Returns the result file and its info.
pub fn lip_sync(project: &Project, plan: &Plan, key: &str, model: &str, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<(PathBuf, ProbeInfo)>> {
    let work = ffmpeg::work_dir("lipsync")?;
    let result = (|| -> Result<Option<(PathBuf, ProbeInfo)>> {
        let (video, audio) = render_inputs(project, plan, &work)?;
        let name = project.clip(plan.video_clip).and_then(|c| c.media_id()).and_then(|m| project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| "shot".into());
        let mut out = out_dir.join(format!("lipsync-{}.mp4", crate::ai::video_gen::slug(&name)));
        let mut n = 2;
        while out.exists() {
            out = out_dir.join(format!("lipsync-{}-{n}.mp4", crate::ai::video_gen::slug(&name)));
            n += 1;
        }
        let Some(path) = run(key, &video, &audio, model, &out, cancel, progress)? else { return Ok(None) };
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
    fn refuses_without_dialogue() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot.mp4".into(), name: "shot.mp4".into(), kind: MediaKind::Video, duration: 8.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let vc = p.new_media_clip(v, 0.0).unwrap();
        let vid = vc.id;
        p.append_to_main(vc);
        assert!(plan(&p, vid, None).unwrap_err().to_string().contains("voice clip"));
    }
}
