//! Quality check for a lip-synced clip (free, local): how well the mouth follows the
//! vocal, whether the mouth looks clean, and a mouth contact sheet to look at.
//!
//! The sync score is the `/singing-scene` `fit` measure ported to Rust: an open mouth is
//! dark and a moving mouth changes between frames, so both are correlated with the
//! loudness of the vocal band (250–2500 Hz) at shifts of ±0.5 s. About 0.2 or more is in
//! sync; under about 0.08 the mouth doesn't follow the words. The best shift says whether
//! the mouth is early or late.

use crate::ai::faces::FaceBox;
use crate::media::ffmpeg;
use crate::model::{Id, Project};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

const FPS: f64 = 12.0;
const W: usize = 64;
const H: usize = 48;
const MAX_LAG: f64 = 0.5;

/// Above this the mouth follows the vocal well.
pub const GOOD: f32 = 0.2;
/// Under this the mouth doesn't follow the vocal.
pub const POOR: f32 = 0.08;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Fair,
    OutOfSync,
    /// Too little voice or mouth movement to measure (a held note, a still face).
    Unclear,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Good => "In sync",
            Verdict::Fair => "Roughly in sync",
            Verdict::OutOfSync => "Out of sync",
            Verdict::Unclear => "Can't tell (little movement or voice)",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub score: f32,
    /// Seconds the mouth trails the voice (negative = the mouth is early).
    pub lag: f64,
    pub verdict: Verdict,
    /// Mouth detail ÷ upper-face detail (rough): very low = soft, smeared mouth;
    /// very high = jagged or noisy teeth and lips.
    pub mouth_detail: f32,
    pub notes: Vec<String>,
    pub sheet: Option<PathBuf>,
    /// The face was known (detected or marked), so the lag is worth acting on. Without it
    /// the mouth crop is a guess and the numbers are noise.
    pub reliable: bool,
}

/// The mouth region: the lower part of the face box, or a central guess without one.
pub fn mouth_box(face: Option<FaceBox>) -> FaceBox {
    let f = face.unwrap_or(FaceBox { cx: 0.5, cy: 0.33, w: 0.22, h: 0.34 });
    FaceBox { cx: f.cx, cy: f.top() + f.h * 0.78, w: f.w * 0.7, h: f.h * 0.38 }
}

/// The eyes and brow: the face's upper part, as a detail reference.
fn upper_box(face: Option<FaceBox>) -> FaceBox {
    let f = face.unwrap_or(FaceBox { cx: 0.5, cy: 0.33, w: 0.22, h: 0.34 });
    FaceBox { cx: f.cx, cy: f.top() + f.h * 0.38, w: f.w * 0.8, h: f.h * 0.3 }
}

fn crop_expr(b: &FaceBox) -> String {
    let c = |v: f32| v.clamp(0.0, 1.0);
    let (w, h) = (c(b.w).max(0.02), c(b.h).max(0.02));
    format!("crop=iw*{w:.4}:ih*{h:.4}:iw*{:.4}:ih*{:.4}", c(b.left()).min(1.0 - w), c(b.top()).min(1.0 - h))
}

/// Grey `W`×`H` frames of `region` at `FPS`, over `[t0, t0 + len]` of the video file.
pub fn region_frames(video: &Path, t0: f64, len: f64, region: &FaceBox) -> Result<Vec<Vec<u8>>> {
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", t0.max(0.0)), "-t", &format!("{len:.3}"), "-i"])
        .arg(video)
        .args(["-vf", &format!("fps={FPS},{},scale={W}:{H},format=gray", crop_expr(region)), "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to run ffmpeg")?;
    Ok(out.stdout.chunks_exact(W * H).map(<[u8]>::to_vec).collect())
}

/// Loudness of the vocal band (250–2500 Hz) at `FPS`, over `[t0, t0 + len]` of the audio
/// file. Seconds before the file starts (`t0 < 0`) are silence.
pub fn vocal_envelope(audio: &Path, t0: f64, len: f64) -> Result<Vec<f32>> {
    const SR: usize = 4800;
    let lead = (-t0).max(0.0);
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", t0.max(0.0)), "-t", &format!("{:.3}", (len - lead).max(0.0)), "-i"])
        .arg(audio)
        .args(["-vn", "-ac", "1", "-af", "highpass=f=250,lowpass=f=2500", "-ar", &SR.to_string(), "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to run ffmpeg")?;
    let samples: Vec<f32> = out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    let hop = (SR as f64 / FPS) as usize;
    let n = (len * FPS).round() as usize;
    let lead_frames = (lead * FPS).round() as usize;
    Ok((0..n)
        .map(|i| {
            if i < lead_frames {
                return 0.0;
            }
            let s = (i - lead_frames) * hop;
            let seg = samples.get(s..(s + hop).min(samples.len())).unwrap_or(&[]);
            if seg.is_empty() {
                0.0
            } else {
                (seg.iter().map(|x| x * x).sum::<f32>() / seg.len() as f32).sqrt()
            }
        })
        .collect())
}

fn corr(xs: &[f32], ys: &[f32]) -> f32 {
    let n = xs.len().min(ys.len());
    if n < 3 {
        return 0.0;
    }
    let (ax, ay) = (xs[..n].iter().sum::<f32>() / n as f32, ys[..n].iter().sum::<f32>() / n as f32);
    let (mut sxy, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (dx, dy) = (xs[i] - ax, ys[i] - ay);
        sxy += dx * dy;
        sx += dx * dx;
        sy += dy * dy;
    }
    if sx <= 1e-12 || sy <= 1e-12 {
        0.0
    } else {
        sxy / (sx * sy).sqrt()
    }
}

/// Openness (share of dark pixels) and motion (mean change from the previous frame) per frame.
pub fn mouth_signals(frames: &[Vec<u8>]) -> (Vec<f32>, Vec<f32>) {
    let px = (W * H) as f32;
    let dark = frames.iter().map(|f| f.iter().filter(|&&p| p < 60).count() as f32 / px).collect();
    let motion = std::iter::once(0.0)
        .chain(frames.windows(2).map(|w| w[0].iter().zip(&w[1]).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as f32).sum::<f32>() / px))
        .collect();
    (dark, motion)
}

/// The best (score, lag): `lag` > 0 means the mouth moves after the voice.
pub fn best_lag(dark: &[f32], motion: &[f32], env: &[f32]) -> (f32, f64) {
    let max = (MAX_LAG * FPS) as i64;
    let n = dark.len().min(env.len()) as i64;
    let mut best = (f32::MIN, 0.0);
    for k in -max..=max {
        // Mouth frame i + k against voice frame i.
        let (a, b) = (0.max(-k), n.min(n - k));
        if b - a < (2.0 * FPS) as i64 {
            continue;
        }
        let m: Vec<f32> = (a..b).map(|i| dark[(i + k) as usize]).collect();
        let mo: Vec<f32> = (a..b).map(|i| motion[(i + k) as usize]).collect();
        let e: Vec<f32> = (a..b).map(|i| env[i as usize]).collect();
        let s = corr(&m, &e) + corr(&mo, &e);
        // Prefer the smaller shift when scores are close.
        if s > best.0 + 0.01 || (s > best.0 - 0.01 && (k as f64).abs() < (best.1 * FPS).abs()) {
            best = (s.max(best.0), k as f64 / FPS);
        }
    }
    if best.0 == f32::MIN {
        (0.0, 0.0)
    } else {
        best
    }
}

/// Mean absolute gradient: how much fine detail a grey frame has.
fn detail(f: &[u8]) -> f32 {
    let mut sum = 0.0;
    for y in 0..H - 1 {
        for x in 0..W - 1 {
            let p = f[y * W + x] as f32;
            sum += (p - f[y * W + x + 1] as f32).abs() + (p - f[(y + 1) * W + x] as f32).abs();
        }
    }
    sum / ((W - 1) * (H - 1)) as f32
}

pub fn verdict(score: f32, voice_level: f32, movement: f32) -> Verdict {
    if voice_level < 0.004 || movement < 0.15 {
        Verdict::Unclear
    } else if score >= GOOD {
        Verdict::Good
    } else if score >= POOR {
        Verdict::Fair
    } else {
        Verdict::OutOfSync
    }
}

/// Writes a grid of mouth crops (2 per second) for a person to look at.
pub fn contact_sheet(video: &Path, t0: f64, len: f64, face: Option<FaceBox>, out: &Path) -> Result<()> {
    let f = face.unwrap_or(FaceBox { cx: 0.5, cy: 0.33, w: 0.22, h: 0.34 });
    let region = FaceBox { cx: f.cx, cy: f.cy + f.h * 0.1, w: (f.w * 1.2).min(1.0), h: (f.h * 1.1).min(1.0) };
    let n = ((len * 2.0).ceil() as usize).clamp(1, 24);
    let cols = n.min(8);
    let rows = n.div_ceil(cols);
    let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{:.3}", t0.max(0.0)), "-t", &format!("{len:.3}"), "-i"])
        .arg(video)
        .args(["-vf", &format!("fps=2,{},scale=180:-2,tile={cols}x{rows}:padding=2", crop_expr(&region)), "-frames:v", "1"])
        .arg(out)
        .status()?
        .success();
    if !ok || !out.is_file() {
        bail!("could not make the mouth contact sheet");
    }
    Ok(())
}

/// Checks a lip-synced clip against the voice under it. `sheet_dir` gets the contact sheet.
pub fn check_clip(project: &Project, clip: Id, voice: Option<Id>, face: Option<FaceBox>, sheet_dir: Option<&Path>) -> Result<Check> {
    let c = project.clip(clip).ok_or_else(|| anyhow!("no clip {clip}"))?;
    let m = c.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("the clip has no media"))?;
    let voice = voice.or_else(|| crate::ai::lipsync::find_voice(project, clip)).ok_or_else(|| anyhow!("no audio under this clip to check against"))?;
    let a = project.clip(voice).ok_or_else(|| anyhow!("no audio clip {voice}"))?;
    let am = a.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("the audio clip has no media"))?;
    let (s, e) = (c.start.max(a.start), c.end().min(a.end()));
    if e - s < 2.0 {
        bail!("the clip and the audio overlap for less than 2 s");
    }
    let len = e - s;
    let v_src = c.source_in + (s - c.start) * c.speed;
    let a_src = a.source_in + (s - a.start) * a.speed;
    let frames = region_frames(&m.path, v_src, len * c.speed, &mouth_box(face))?;
    if frames.len() < (2.0 * FPS) as usize {
        bail!("couldn't read enough frames of the clip");
    }
    let env = vocal_envelope(&am.path, a_src, len)?;
    let (dark, motion) = mouth_signals(&frames);
    let (score, lag) = best_lag(&dark, &motion, &env);
    let voice_level = env.iter().sum::<f32>() / env.len().max(1) as f32;
    let movement = motion.iter().sum::<f32>() / motion.len().max(1) as f32;
    let verdict = verdict(score, voice_level, movement);
    let upper = region_frames(&m.path, v_src, len * c.speed, &upper_box(face)).unwrap_or_default();
    let mouth_d = frames.iter().map(|f| detail(f)).sum::<f32>() / frames.len() as f32;
    let upper_d = if upper.is_empty() { mouth_d } else { upper.iter().map(|f| detail(f)).sum::<f32>() / upper.len() as f32 };
    let mouth_detail = if upper_d > 0.01 { mouth_d / upper_d } else { 1.0 };
    let mut notes = Vec::new();
    let reliable = face.is_some();
    if reliable && verdict != Verdict::Unclear && lag.abs() >= 0.15 {
        notes.push(format!("The mouth is {:.2} s {} the voice. Nudge it by {:+.2} s.", lag.abs(), if lag > 0.0 { "behind" } else { "ahead of" }, lag));
    }
    if mouth_detail < 0.45 {
        notes.push("The mouth looks soft or smeared next to the eyes (rough check). Try the pro lip-sync model, or Animate + sync with a calmer source.".into());
    } else if mouth_detail > 2.2 {
        notes.push("The mouth has much more edge detail than the eyes: teeth may look jagged (rough check). Look at the contact sheet.".into());
    }
    if face.is_none() {
        notes.push("No face box: the mouth crop is a guess, so the score and timing are unreliable. Detect or mark the face, or judge by the contact sheet.".into());
    }
    if reliable {
        notes.push("The score is rough: judge by the contact sheet. If the camera pushes in, the face box drifts off the mouth in later frames.".into());
    }
    let sheet = match sheet_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            let out = dir.join(format!("mouth-check-clip-{clip}.png"));
            contact_sheet(&m.path, v_src, len * c.speed, face, &out).ok().map(|_| out)
        }
        None => None,
    };
    Ok(Check { score, lag, verdict, mouth_detail, notes, sheet, reliable })
}

/// Moves what the clip shows by `lag` seconds (positive = show later footage earlier, for
/// a mouth that trails the voice), keeping the clip in place. Returns what was done.
pub fn nudge(project: &mut Project, clip: Id, lag: f64) -> Result<String> {
    let c = project.clip_mut(clip).ok_or_else(|| anyhow!("no clip {clip}"))?;
    if lag > 0.0 {
        if c.duration - lag < crate::model::MIN_CLIP_LEN {
            bail!("the clip is too short to nudge");
        }
        c.source_in += lag * c.speed;
        c.duration -= lag;
        Ok(format!("showing the footage {lag:.2} s earlier; the clip ends {lag:.2} s sooner"))
    } else {
        let back = -lag * c.speed;
        if c.source_in + 1e-6 < back {
            bail!("there's no footage before the clip's first frame to show; move the voice {:.2} s earlier instead", -lag);
        }
        c.source_in -= back;
        Ok(format!("showing the footage {:.2} s later", -lag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_lag_of_a_late_mouth() {
        // A voice with syllables every ~0.5 s, and a mouth that follows it 2 frames late.
        let n = 120;
        // Irregular syllables (a repeating rhythm would match at several shifts).
        let mut seed = 7u32;
        let env: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                if (seed >> 16) % 3 == 0 { 0.1 } else { 0.8 }
            })
            .collect();
        let dark: Vec<f32> = (0..n).map(|i| if i >= 2 { env[i - 2] * 0.5 } else { 0.0 }).collect();
        let motion: Vec<f32> = std::iter::once(0.0).chain(dark.windows(2).map(|w| (w[1] - w[0]).abs())).collect();
        let (score, lag) = best_lag(&dark, &motion, &env);
        assert!(score > GOOD, "{score}");
        assert!((lag - 2.0 / FPS).abs() < 1e-9, "{lag}");
        // A still mouth gives nothing to correlate.
        let still = vec![0.3; n];
        assert!(best_lag(&still, &vec![0.0; n], &env).0.abs() < 1e-3);
    }

    #[test]
    fn verdict_thresholds() {
        assert_eq!(verdict(0.3, 0.05, 2.0), Verdict::Good);
        assert_eq!(verdict(0.1, 0.05, 2.0), Verdict::Fair);
        assert_eq!(verdict(0.02, 0.05, 2.0), Verdict::OutOfSync);
        assert_eq!(verdict(0.3, 0.0, 2.0), Verdict::Unclear);
    }

    #[test]
    fn mouth_is_the_lower_face() {
        let f = FaceBox { cx: 0.5, cy: 0.4, w: 0.2, h: 0.3 };
        let m = mouth_box(Some(f));
        assert!(m.cy > f.cy && m.bottom() <= f.bottom() + 0.06 && m.w < f.w);
        assert!(crop_expr(&m).starts_with("crop=iw*"));
    }

    #[test]
    fn nudges_both_ways() {
        let mut p = Project::default();
        let mut c = crate::model::Clip::new(5, crate::model::ClipSource::Media(1), 10.0, 4.0);
        c.source_in = 0.0;
        p.tracks[0].clips.push(c);
        assert!(nudge(&mut p, 5, -0.1).is_err(), "nothing before the first frame");
        nudge(&mut p, 5, 0.2).unwrap();
        let c = p.clip(5).unwrap();
        assert!((c.source_in - 0.2).abs() < 1e-9 && (c.duration - 3.8).abs() < 1e-9 && c.start == 10.0);
        nudge(&mut p, 5, -0.1).unwrap();
        assert!((p.clip(5).unwrap().source_in - 0.1).abs() < 1e-9);
    }
}
