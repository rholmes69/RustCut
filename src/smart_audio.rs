//! Smart audio: auto-ducking, noise removal and loudness.
//!
//! - **Auto-ducking** finds where voice clips are speaking (from their waveform
//!   peaks) and writes volume keyframes onto music clips, so the dips are visible,
//!   editable, undoable and previewed like any other keyframes.
//! - **Noise removal** uses FFmpeg's `afftdn`, tuned with the clip's measured noise floor.
//! - **Loudness** normalises the export mix with `loudnorm` and measures it with `ebur128`.

use crate::export;
use crate::media::ffmpeg::{self, PEAKS_PER_SEC};
use crate::model::*;
use anyhow::{bail, Context, Result};
use std::process::Stdio;
use std::sync::Arc;

// ── auto-ducking ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct DuckOptions {
    /// How far music is lowered (dB, negative).
    pub amount_db: f32,
    /// Voice threshold on normalised waveform peaks (0..1).
    pub sensitivity: f32,
    /// Seconds to fade down before speech starts.
    pub attack: f64,
    /// Seconds to fade back up after speech ends.
    pub release: f64,
    /// Pauses shorter than this keep the music down.
    pub hold: f64,
}

impl DuckOptions {
    pub fn from_mix(mix: &AudioMix) -> Self {
        Self { amount_db: mix.duck_db, sensitivity: mix.duck_sensitivity, attack: 0.25, release: 0.6, hold: 0.8 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DuckReport {
    /// Timeline ranges where voice was detected.
    pub speech: Vec<(f64, f64)>,
    pub voice_clips: usize,
    pub music_clips: usize,
}

/// Clips whose sound is heard in the mix (not muted, has audio, not silent).
fn audible_clips(project: &Project) -> impl Iterator<Item = &Clip> {
    project.tracks.iter().filter(|t| t.kind != TrackKind::Text && !t.muted).flat_map(|t| &t.clips).filter(|c| {
        (c.volume > 0.001 || c.is_animated(Prop::Volume))
            && c.media_id().and_then(|m| project.media(m)).is_some_and(|m| m.has_sound())
    })
}

pub fn voice_clips(project: &Project) -> Vec<&Clip> {
    audible_clips(project).filter(|c| project.audio_role(c) == AudioRole::Voice).collect()
}

pub fn music_clips(project: &Project) -> Vec<&Clip> {
    audible_clips(project).filter(|c| project.audio_role(c) == AudioRole::Music).collect()
}

/// Timeline ranges where any voice clip is speaking. `peaks_for` returns a media
/// item's normalised waveform peaks (see [`ffmpeg::waveform`]).
pub fn speech_segments(
    project: &Project,
    peaks_for: &dyn Fn(Id) -> Option<Arc<Vec<f32>>>,
    threshold: f32,
    hold: f64,
) -> Vec<(f64, f64)> {
    let rate = PEAKS_PER_SEC;
    let n = (project.duration() * rate).ceil() as usize + 1;
    let mut active = vec![false; n];
    for clip in voice_clips(project) {
        let Some(peaks) = clip.media_id().and_then(peaks_for) else { continue };
        let i0 = (clip.start * rate).floor() as usize;
        let i1 = ((clip.end() * rate).ceil() as usize).min(n);
        for (i, slot) in active.iter_mut().enumerate().take(i1).skip(i0) {
            let t = i as f64 / rate;
            let src = clip.source_in + (t - clip.start) * clip.speed;
            let j = (src * rate) as usize;
            if peaks.get(j).is_some_and(|p| *p >= threshold) {
                *slot = true;
            }
        }
    }
    // Runs of activity → segments; bridge short pauses, drop blips.
    let mut segs: Vec<(f64, f64)> = Vec::new();
    let mut start = None;
    for (i, &a) in active.iter().chain(std::iter::once(&false)).enumerate() {
        match (a, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                segs.push((s as f64 / rate, i as f64 / rate));
                start = None;
            }
            _ => {}
        }
    }
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for s in segs {
        match merged.last_mut() {
            Some(last) if s.0 - last.1 < hold => last.1 = s.1,
            _ => merged.push(s),
        }
    }
    merged.retain(|(a, b)| b - a >= 0.15);
    merged
}

/// Volume keyframes (clip-local) that duck `base` volume during `speech`.
fn duck_keys(clip: &Clip, speech: &[(f64, f64)], opts: &DuckOptions) -> Option<Vec<Keyframe>> {
    let base = clip.volume;
    let low = base * 10f32.powf(opts.amount_db / 20.0);
    // Merge segments whose ramps would overlap.
    let mut segs: Vec<(f64, f64)> = Vec::new();
    for &(s, e) in speech {
        match segs.last_mut() {
            Some(last) if s - opts.attack <= last.1 + opts.release => last.1 = last.1.max(e),
            _ => segs.push((s, e)),
        }
    }
    let (lo, hi) = (clip.start, clip.end());
    let segs: Vec<_> = segs.into_iter().filter(|(s, e)| e + opts.release > lo && s - opts.attack < hi).collect();
    if segs.is_empty() {
        return None;
    }
    let mut points = Vec::new();
    for (s, e) in segs {
        points.push(Keyframe { time: s - opts.attack - lo, value: base, ease: Ease::Smooth });
        points.push(Keyframe { time: s - lo, value: low, ease: Ease::Linear });
        points.push(Keyframe { time: e - lo, value: low, ease: Ease::Smooth });
        points.push(Keyframe { time: e + opts.release - lo, value: base, ease: Ease::Linear });
    }
    // Keep what falls inside the clip, with exact values at its edges.
    let mut keys: Vec<Keyframe> = Vec::new();
    for edge in [0.0, clip.duration] {
        if !points.iter().any(|k| (k.time - edge).abs() <= KEY_TOLERANCE) {
            let i = points.iter().rposition(|k| k.time < edge);
            let ease = i.map_or(Ease::Linear, |i| points[i].ease);
            keys.push(Keyframe { time: edge, value: interpolate(&points, edge), ease });
        }
    }
    keys.extend(points.into_iter().filter(|k| k.time >= -KEY_TOLERANCE && k.time <= clip.duration + KEY_TOLERANCE));
    keys.iter_mut().for_each(|k| k.time = k.time.clamp(0.0, clip.duration));
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    keys.dedup_by(|b, a| (b.time - a.time).abs() <= KEY_TOLERANCE);
    Some(keys)
}

/// Lowers music under voice by writing volume keyframes onto music clips
/// (replacing their existing volume keyframes).
pub fn auto_duck(
    project: &mut Project,
    peaks_for: &dyn Fn(Id) -> Option<Arc<Vec<f32>>>,
    opts: &DuckOptions,
) -> std::result::Result<DuckReport, String> {
    let voice = voice_clips(project).len();
    let music: Vec<Id> = music_clips(project).iter().map(|c| c.id).collect();
    if voice == 0 {
        return Err("No voice clips: set a clip's audio role to Voice (video sound is Voice by default).".into());
    }
    if music.is_empty() {
        return Err("No music clips: set a clip's audio role to Music (imported audio files are Music by default).".into());
    }
    let speech = speech_segments(project, peaks_for, opts.sensitivity, opts.hold);
    for id in &music {
        if let Some(clip) = project.clip_mut(*id) {
            match duck_keys(clip, &speech, opts) {
                Some(keys) => {
                    clip.keyframes.insert(Prop::Volume, keys);
                }
                None => {
                    clip.keyframes.remove(&Prop::Volume);
                }
            }
        }
    }
    Ok(DuckReport { speech, voice_clips: voice, music_clips: music.len() })
}

/// Removes volume keyframes from music clips (undoes auto-ducking). Returns how many clips changed.
pub fn clear_ducking(project: &mut Project) -> usize {
    let music: Vec<Id> = music_clips(project).iter().map(|c| c.id).collect();
    let mut n = 0;
    for id in music {
        if let Some(c) = project.clip_mut(id) {
            if c.keyframes.remove(&Prop::Volume).is_some() {
                n += 1;
            }
        }
    }
    n
}

// ── noise removal ────────────────────────────────────────────────────────────

/// The `afftdn` filter for a clip's noise settings.
pub fn denoise_filter(d: &Denoise) -> String {
    format!("afftdn=nr={}:nf={:.1}", d.level.reduction_db(), (d.floor_db + 3.0).clamp(-80.0, -20.0))
}

/// Measures the noise floor (dB) of a clip's source range with `astats`.
pub fn measure_noise_floor(project: &Project, clip: &Clip) -> Result<f32> {
    let media = clip.media_id().and_then(|m| project.media(m)).context("clip has no media")?;
    if !media.has_sound() {
        bail!("this clip has no audio");
    }
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-nostats", "-ss", &format!("{:.3}", clip.source_in), "-t"])
        .arg(format!("{:.3}", clip.duration * clip.speed))
        .arg("-i")
        .arg(&media.path)
        .args(["-vn", "-af", "astats=measure_overall=Noise_floor:measure_perchannel=none", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not run ffmpeg")?;
    let log = String::from_utf8_lossy(&out.stderr);
    log.lines()
        .filter_map(|l| l.split("Noise floor dB:").nth(1))
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .find(|v| v.is_finite())
        .ok_or_else(|| anyhow::anyhow!("could not measure the noise floor"))
}

// ── loudness ─────────────────────────────────────────────────────────────────

/// Fixed master gain followed by a −1.5 dBTP limiter.
pub fn master_filter(gain_db: f32) -> String {
    format!("volume={gain_db:.2}dB,alimiter=limit=0.841:level=false:attack=5:release=50,aresample=48000")
}

/// Gain (dB) that brings the mix to `target` LUFS through [`master_filter`]. The
/// limiter eats some of the gain on peaky material, so this measures and corrects
/// (secant steps) until within 0.2 LU.
pub fn normalization_gain(project: &Project, target: f32) -> Result<f32> {
    let first = measure_with_gain(project, None)?.integrated;
    if first < -60.0 {
        return Ok(0.0); // silence: nothing sensible to normalise
    }
    let mut gain = (target - first).clamp(-30.0, 20.0);
    let mut prev: Option<(f32, f32)> = Some((0.0, first));
    for _ in 0..4 {
        let measured = measure_with_gain(project, Some(gain))?.integrated;
        let err = target - measured;
        if err.abs() < 0.2 {
            break;
        }
        let slope = match prev {
            Some((g0, l0)) if (gain - g0).abs() > 0.05 => ((measured - l0) / (gain - g0)).clamp(0.2, 1.2),
            _ => 1.0,
        };
        prev = Some((gain, measured));
        gain = (gain + err / slope).clamp(-30.0, 20.0);
    }
    Ok(gain)
}

/// The filter that normalises the final mix to `target` LUFS.
pub fn loudnorm_filter(target: f32) -> String {
    format!("loudnorm=I={target:.1}:TP=-1.5:LRA=11,aresample=48000")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, LUFS.
    pub integrated: f32,
    /// Loudness range, LU.
    pub range: f32,
    /// True peak, dBFS.
    pub peak: f32,
}

fn parse_ebur128(log: &str) -> Option<Loudness> {
    // The summary is printed last; take the final value of each field.
    let field = |name: &str| {
        log.lines()
            .filter_map(|l| l.trim_start().strip_prefix(name))
            .filter_map(|v| v.split_whitespace().next()?.parse::<f32>().ok())
            .last()
    };
    Some(Loudness { integrated: field("I:")?, range: field("LRA:").unwrap_or(0.0), peak: field("Peak:").unwrap_or(f32::NAN) })
}

/// Measures the timeline mix as it would export *before* normalisation.
pub fn measure_loudness(project: &Project) -> Result<Loudness> {
    measure_with_gain(project, None)
}

/// Measures the mix with an optional master gain (+ limiter) applied.
fn measure_with_gain(project: &Project, gain_db: Option<f32>) -> Result<Loudness> {
    let mut p = project.clone();
    p.audio.loudness = None;
    p.audio.master_gain_db = gain_db;
    let work = ffmpeg::work_dir("loudness")?;
    let result = (|| -> Result<Loudness> {
        let g = export::build_graph(&p, 360, p.fps, &work, None, false, true)?;
        let label = g.audio_label.context("no audio")?;
        let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-nostats"])
            .args(&g.inputs)
            .args(export::filter_complex_args(format!("{};[{label}]ebur128=peak=true:framelog=quiet[m]", g.filter), &work)?)
            .args(["-map", "[m]", "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("could not run ffmpeg")?;
        let log = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!("FFmpeg failed: {}", log.lines().rev().take(3).collect::<Vec<_>>().join(" | "));
        }
        parse_ebur128(&log).context("could not read the loudness summary")
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(p: &mut Project, kind: MediaKind, name: &str) -> Id {
        p.add_media(MediaItem {
            id: 0,
            path: name.into(),
            name: name.into(),
            kind,
            duration: 20.0,
            width: 1280,
            height: 720,
            fps: 30.0,
            has_audio: true,
        })
    }

    /// Voice on the main track (speaking 2–4 s and 4.5–6 s and 12–13 s), music under it.
    fn project() -> (Project, Id, Id, Arc<Vec<f32>>) {
        let mut p = Project::default();
        let v = media(&mut p, MediaKind::Video, "talk.mp4");
        let m = media(&mut p, MediaKind::Audio, "song.mp3");
        let mut vc = p.new_media_clip(v, 0.0).unwrap();
        vc.duration = 15.0;
        p.append_to_main(vc);
        let mut mc = p.new_media_clip(m, 0.0).unwrap();
        mc.duration = 15.0;
        mc.volume = 0.8;
        let music = mc.id;
        p.place_clip(mc, None);
        let mut peaks = vec![0.02f32; 2000];
        for (a, b) in [(2.0, 4.0), (4.5, 6.0), (12.0, 13.0)] {
            for i in (a * 100.0) as usize..(b * 100.0) as usize {
                peaks[i] = if i % 7 == 0 { 0.05 } else { 0.6 }; // speech is spiky
            }
        }
        (p, v, music, Arc::new(peaks))
    }

    #[test]
    fn roles_default_sensibly() {
        let (p, _, music, _) = project();
        assert_eq!(voice_clips(&p).len(), 1);
        assert_eq!(music_clips(&p)[0].id, music);
    }

    #[test]
    fn detects_speech_and_bridges_pauses() {
        let (p, v, _, peaks) = project();
        let peaks_for = |id: Id| (id == v).then(|| peaks.clone());
        let segs = speech_segments(&p, &peaks_for, 0.12, 0.8);
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert!((segs[0].0 - 2.0).abs() < 0.02 && (segs[0].1 - 6.0).abs() < 0.02);
        assert!((segs[1].0 - 12.0).abs() < 0.02);
    }

    #[test]
    fn ducks_music_with_keyframes() {
        let (mut p, v, music, peaks) = project();
        let peaks_for = |id: Id| (id == v).then(|| peaks.clone());
        let opts = DuckOptions { amount_db: -12.0, sensitivity: 0.12, attack: 0.25, release: 0.6, hold: 0.8 };
        let report = auto_duck(&mut p, &peaks_for, &opts).unwrap();
        assert_eq!((report.voice_clips, report.music_clips, report.speech.len()), (1, 1, 2));
        let c = p.clip(music).unwrap();
        let low = 0.8 * 10f32.powf(-0.6);
        assert!((c.volume_at(1.0) - 0.8).abs() < 1e-4);
        assert!((c.volume_at(3.0) - low).abs() < 1e-4);
        assert!((c.volume_at(9.0) - 0.8).abs() < 1e-4);
        assert!((c.volume_at(12.5) - low).abs() < 1e-4);
        assert!(c.volume_at(1.9) < 0.8 && c.volume_at(1.9) > low); // ramping down
        // Static volume is untouched, so clearing restores it.
        assert_eq!(clear_ducking(&mut p), 1);
        assert_eq!(p.clip(music).unwrap().volume_at(3.0), 0.8);
    }

    #[test]
    fn ducking_needs_voice_and_music() {
        let (mut p, v, music, peaks) = project();
        let peaks_for = |id: Id| (id == v).then(|| peaks.clone());
        let opts = DuckOptions::from_mix(&AudioMix::default());
        p.clip_mut(music).unwrap().audio_role = AudioRole::Effect;
        assert!(auto_duck(&mut p, &peaks_for, &opts).unwrap_err().contains("No music"));
    }

    #[test]
    fn clip_starting_mid_speech_starts_low() {
        let (mut p, v, music, peaks) = project();
        let c = p.clip_mut(music).unwrap();
        c.start = 3.0;
        c.duration = 2.0;
        let peaks_for = |id: Id| (id == v).then(|| peaks.clone());
        auto_duck(&mut p, &peaks_for, &DuckOptions::from_mix(&AudioMix::default())).unwrap();
        let c = p.clip(music).unwrap();
        let keys = &c.keyframes[&Prop::Volume];
        assert_eq!(keys[0].time, 0.0);
        assert!(keys[0].value < 0.3);
        assert!(keys.iter().all(|k| k.time >= 0.0 && k.time <= c.duration));
    }

    #[test]
    fn filters_and_parsing() {
        let d = Denoise { level: NoiseLevel::Medium, floor_db: -33.1 };
        assert_eq!(denoise_filter(&d), "afftdn=nr=24:nf=-30.1");
        assert_eq!(denoise_filter(&Denoise { level: NoiseLevel::Light, floor_db: -10.0 }), "afftdn=nr=12:nf=-20.0");
        assert_eq!(loudnorm_filter(-14.0), "loudnorm=I=-14.0:TP=-1.5:LRA=11,aresample=48000");
        assert!(master_filter(6.5).starts_with("volume=6.50dB,alimiter=limit=0.841"));
        let log = "[Parsed_ebur128_0] Summary:\n\n  Integrated loudness:\n    I:         -22.2 LUFS\n    Threshold: -32.9 LUFS\n\n  Loudness range:\n    LRA:         3.1 LU\n\n  True peak:\n    Peak:      -15.9 dBFS\n";
        assert_eq!(parse_ebur128(log), Some(Loudness { integrated: -22.2, range: 3.1, peak: -15.9 }));
    }
}
