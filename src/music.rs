//! Music analysis for music videos: tempo, beats, bars (4/4) and song sections,
//! plus the timeline helpers built on them (beat snapping, beat montage, beat effects).
//!
//! Analysis: mono 11 025 Hz audio → 1024-point FFT every 256 samples → spectral-flux
//! onset envelope (≈ 43 frames/s) → tempo from the envelope's autocorrelation (weighted
//! towards 120 BPM) → beats by dynamic programming (Ellis 2007) → downbeats where the
//! bass onsets line up best → sections from changes in per-bar loudness.

use crate::media::ffmpeg;
use crate::model::{Clip, Ease, Id, Keyframe, MediaKind, Project, Prop, TrackKind};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::Stdio;

pub const RATE: usize = 11_025;
const FFT: usize = 1024;
const HOP: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// Source seconds.
    pub start: f64,
    pub end: f64,
    /// intro, verse, chorus, break or outro.
    pub label: String,
    /// Mean loudness relative to the loudest section (0..1).
    pub energy: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MusicAnalysis {
    pub bpm: f64,
    /// Beat times in source seconds.
    pub beats: Vec<f64>,
    /// Index into `beats` of the first downbeat (bar start).
    pub first_downbeat: usize,
    pub beats_per_bar: usize,
    pub sections: Vec<Section>,
    /// Loudness (0..1) at each beat, for energy-aware editing.
    #[serde(default)]
    pub beat_energy: Vec<f32>,
}

impl MusicAnalysis {
    pub fn is_downbeat(&self, i: usize) -> bool {
        i >= self.first_downbeat && (i - self.first_downbeat) % self.beats_per_bar.max(1) == 0
            || i < self.first_downbeat && (self.first_downbeat - i) % self.beats_per_bar.max(1) == 0
    }

    pub fn period(&self) -> f64 {
        60.0 / self.bpm.max(1.0)
    }

    pub fn section_at(&self, t: f64) -> Option<&Section> {
        self.sections.iter().find(|s| t >= s.start && t < s.end)
    }
}

// ── decoding & DSP ───────────────────────────────────────────────────────────

/// Decodes the first audio stream to mono f32 at [`RATE`].
pub fn decode_mono(path: &Path) -> Result<Vec<f32>> {
    let mut child = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run ffmpeg")?;
    let mut bytes = Vec::new();
    BufReader::new(child.stdout.take().context("no stdout")?).read_to_end(&mut bytes)?;
    let _ = child.wait();
    if bytes.len() < RATE * 2 {
        bail!("{} has no usable audio (or it is shorter than a second)", path.display());
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

/// In-place iterative radix-2 FFT (`re.len()` must be a power of two).
pub fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// Per-frame features at `RATE / HOP` frames per second.
pub struct Envelope {
    pub fps: f64,
    /// Spectral flux (all bands), normalised to 0..1.
    pub onset: Vec<f32>,
    /// Spectral flux below ~150 Hz (kick drum), normalised.
    pub bass: Vec<f32>,
    /// RMS loudness, normalised.
    pub rms: Vec<f32>,
}

fn normalise(v: &mut [f32]) {
    let max = v.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        v.iter_mut().for_each(|x| *x /= max);
    }
}

pub fn envelope(samples: &[f32]) -> Envelope {
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
    let bass_bins = (150.0 * FFT as f32 / RATE as f32).ceil() as usize;
    let frames = samples.len().saturating_sub(FFT) / HOP + 1;
    let mut prev = vec![0.0f32; FFT / 2];
    let (mut onset, mut bass, mut rms) = (Vec::with_capacity(frames), Vec::with_capacity(frames), Vec::with_capacity(frames));
    let (mut re, mut im) = (vec![0.0f32; FFT], vec![0.0f32; FFT]);
    for f in 0..frames {
        let s = &samples[f * HOP..(f * HOP + FFT).min(samples.len())];
        let mut energy = 0.0;
        for i in 0..FFT {
            let x = s.get(i).copied().unwrap_or(0.0);
            energy += x * x;
            re[i] = x * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let (mut flux, mut bflux) = (0.0, 0.0);
        for k in 1..FFT / 2 {
            let mag = (1.0 + 100.0 * (re[k] * re[k] + im[k] * im[k]).sqrt()).ln();
            let d = (mag - prev[k]).max(0.0);
            flux += d;
            if k <= bass_bins {
                bflux += d;
            }
            prev[k] = mag;
        }
        onset.push(flux);
        bass.push(bflux);
        rms.push((energy / FFT as f32).sqrt());
    }
    // Remove the slowly varying part of the onset curve so tempo/beat picking sees peaks.
    let detrend = |v: &mut Vec<f32>| {
        let w = 16;
        let mean: Vec<f32> = (0..v.len())
            .map(|i| {
                let (a, b) = (i.saturating_sub(w), (i + w + 1).min(v.len()));
                v[a..b].iter().sum::<f32>() / (b - a) as f32
            })
            .collect();
        for (x, m) in v.iter_mut().zip(mean) {
            *x = (*x - m).max(0.0);
        }
    };
    detrend(&mut onset);
    detrend(&mut bass);
    normalise(&mut onset);
    normalise(&mut bass);
    normalise(&mut rms);
    Envelope { fps: RATE as f64 / HOP as f64, onset, bass, rms }
}

/// Tempo in BPM from the onset envelope.
pub fn estimate_bpm(onset: &[f32], fps: f64) -> f64 {
    let lag_of = |bpm: f64| 60.0 * fps / bpm;
    let (lo, hi) = (lag_of(190.0).floor() as usize, lag_of(65.0).ceil() as usize);
    if onset.len() < hi * 2 {
        return 120.0;
    }
    let ac = |lag: usize| -> f64 { onset.iter().zip(&onset[lag..]).map(|(a, b)| (*a * *b) as f64).sum::<f64>() / (onset.len() - lag) as f64 };
    let scores: Vec<(usize, f64)> = (lo..=hi)
        .map(|lag| {
            let bpm = 60.0 * fps / lag as f64;
            let w = (-0.5 * ((bpm / 120.0).log2() / 0.9).powi(2)).exp();
            // Reward lags whose double also lines up (true beat period rather than an off-beat).
            (lag, w * (ac(lag) + 0.5 * if lag * 2 < onset.len() { ac(lag * 2) } else { 0.0 }))
        })
        .collect();
    let best = scores.iter().cloned().max_by(|a, b| a.1.total_cmp(&b.1)).map_or(lo, |s| s.0);
    // Parabolic refinement around the best lag.
    let lag = if best > lo && best < hi {
        let (a, b, c) = (ac(best - 1), ac(best), ac(best + 1));
        let d = a - 2.0 * b + c;
        if d.abs() > 1e-12 { best as f64 + 0.5 * (a - c) / d } else { best as f64 }
    } else {
        best as f64
    };
    let bpm = 60.0 * fps / lag;
    (bpm * 10.0).round() / 10.0
}

/// Beat frames by dynamic programming over the onset envelope.
pub fn track_beats(onset: &[f32], period: f64) -> Vec<usize> {
    let n = onset.len();
    if n == 0 || period < 2.0 {
        return Vec::new();
    }
    let tightness = 100.0;
    let mut score = vec![0.0f64; n];
    let mut back = vec![usize::MAX; n];
    let (lo, hi) = ((period / 2.0).round() as usize, (period * 2.0).round() as usize);
    for t in 0..n {
        let mut best = 0.0;
        let mut arg = usize::MAX;
        if t >= lo {
            for prev in t.saturating_sub(hi)..=t - lo {
                let d = ((t - prev) as f64 / period).ln();
                let s = score[prev] - tightness * d * d;
                if arg == usize::MAX || s > best {
                    best = s;
                    arg = prev;
                }
            }
        }
        score[t] = onset[t] as f64 + if arg == usize::MAX { 0.0 } else { best.max(0.0) };
        back[t] = if best > 0.0 { arg } else { usize::MAX };
    }
    // Start from the best-scoring frame near the end.
    let tail = n.saturating_sub(period.ceil() as usize * 2);
    let mut t = (tail..n).max_by(|a, b| score[*a].total_cmp(&score[*b])).unwrap_or(n - 1);
    let mut beats = vec![t];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t);
    }
    beats.reverse();
    beats
}

/// Splits the song into sections at bar boundaries where loudness changes.
pub fn find_sections(bar_times: &[f64], bar_energy: &[f32], end: f64) -> Vec<Section> {
    let n = bar_energy.len();
    if n == 0 {
        return vec![Section { start: 0.0, end, label: "verse".into(), energy: 1.0 }];
    }
    let w = 4usize;
    let mean = |a: usize, b: usize| bar_energy[a..b].iter().sum::<f32>() / (b - a).max(1) as f32;
    let novelty: Vec<f32> = (0..n).map(|i| if i < w / 2 || i + w / 2 > n { 0.0 } else { (mean(i, (i + w).min(n)) - mean(i.saturating_sub(w), i)).abs() }).collect();
    let mut cuts = vec![0usize];
    let thresh = 0.07;
    for i in 1..n {
        let local_max = (i.saturating_sub(2)..(i + 3).min(n)).all(|j| novelty[j] <= novelty[i]);
        if local_max && novelty[i] > thresh && i - *cuts.last().unwrap() >= 4 && n - i >= 2 {
            // Prefer phrase boundaries (multiples of 4 bars) when one is adjacent.
            let snapped = [i, i - 1, i + 1].into_iter().find(|j| j % 4 == 0 && *j < n && *j - cuts.last().unwrap() >= 4).unwrap_or(i);
            cuts.push(snapped);
        }
    }
    let mut sections: Vec<Section> = cuts
        .iter()
        .enumerate()
        .map(|(k, &a)| {
            let b = cuts.get(k + 1).copied().unwrap_or(n);
            Section {
                start: if k == 0 { 0.0 } else { bar_times[a] },
                end: if b >= n { end } else { bar_times[b] },
                label: String::new(),
                energy: mean(a, b),
            }
        })
        .collect();
    let max = sections.iter().map(|s| s.energy).fold(0.0f32, f32::max).max(1e-6);
    sections.iter_mut().for_each(|s| s.energy /= max);
    let last = sections.len() - 1;
    for (k, s) in sections.iter_mut().enumerate() {
        s.label = if s.energy >= 0.8 {
            "chorus"
        } else if k == 0 && s.energy < 0.7 {
            "intro"
        } else if k == last && last > 0 && s.energy < 0.7 {
            "outro"
        } else if s.energy < 0.45 {
            "break"
        } else {
            "verse"
        }
        .into();
    }
    sections
}

/// Full analysis of decoded samples.
pub fn analyze_samples(samples: &[f32]) -> Result<MusicAnalysis> {
    let env = envelope(samples);
    if env.onset.len() < 100 {
        bail!("the audio is too short to find a beat");
    }
    let bpm = estimate_bpm(&env.onset, env.fps);
    let frames = track_beats(&env.onset, 60.0 * env.fps / bpm);
    if frames.len() < 4 {
        bail!("no steady beat found");
    }
    let beats: Vec<f64> = frames.iter().map(|&f| (f as f64 * HOP as f64 + FFT as f64 / 2.0) / RATE as f64).collect();
    // Downbeat: the phase (of 4) whose beats carry the most bass onsets.
    let bpb = 4;
    let first_downbeat = (0..bpb.min(frames.len()))
        .max_by(|&a, &b| {
            let s = |p: usize| frames.iter().skip(p).step_by(bpb).map(|&f| env.bass.get(f).copied().unwrap_or(0.0) as f64).sum::<f64>();
            s(a).total_cmp(&s(b))
        })
        .unwrap_or(0);
    let beat_energy: Vec<f32> = frames
        .iter()
        .enumerate()
        .map(|(k, &f)| {
            let next = frames.get(k + 1).copied().unwrap_or((f + 20).min(env.rms.len()));
            let seg = &env.rms[f.min(env.rms.len() - 1)..next.clamp(f + 1, env.rms.len())];
            seg.iter().sum::<f32>() / seg.len().max(1) as f32
        })
        .collect();
    let bar_idx: Vec<usize> = (first_downbeat..beats.len()).step_by(bpb).collect();
    let bar_times: Vec<f64> = bar_idx.iter().map(|&i| beats[i]).collect();
    let bar_energy: Vec<f32> = bar_idx
        .iter()
        .map(|&i| {
            let e = &beat_energy[i..(i + bpb).min(beat_energy.len())];
            e.iter().sum::<f32>() / e.len().max(1) as f32
        })
        .collect();
    let duration = samples.len() as f64 / RATE as f64;
    let sections = find_sections(&bar_times, &bar_energy, duration);
    Ok(MusicAnalysis { bpm, beats, first_downbeat, beats_per_bar: bpb, sections, beat_energy })
}

pub fn analyze(path: &Path) -> Result<MusicAnalysis> {
    analyze_samples(&decode_mono(path)?)
}

// ── timeline ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatMark {
    /// Timeline seconds.
    pub time: f64,
    pub downbeat: bool,
    pub energy: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SectionMark {
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub energy: f32,
}

/// The clip that drives the beat grid: the first analysed clip, preferring audio tracks.
pub fn beat_clip(project: &Project) -> Option<&Clip> {
    let analysed = |c: &&Clip| c.media_id().is_some_and(|m| project.music.contains_key(&m));
    let clips = || project.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (t.kind, c)));
    clips().filter(|(k, _)| *k == TrackKind::Audio).map(|(_, c)| c).find(analysed).or_else(|| clips().map(|(_, c)| c).find(analysed))
}

fn to_timeline(clip: &Clip, s: f64) -> Option<f64> {
    let t = clip.start + (s - clip.source_in) / clip.speed.max(1e-6);
    (t >= clip.start - 1e-6 && t <= clip.end() + 1e-6).then_some(t)
}

/// Beats of the beat clip, in timeline time.
pub fn timeline_beats(project: &Project) -> Vec<BeatMark> {
    let Some(clip) = beat_clip(project) else { return Vec::new() };
    let Some(a) = clip.media_id().and_then(|m| project.music.get(&m)) else { return Vec::new() };
    a.beats
        .iter()
        .enumerate()
        .filter_map(|(i, &s)| {
            to_timeline(clip, s).map(|time| BeatMark { time, downbeat: a.is_downbeat(i), energy: a.beat_energy.get(i).copied().unwrap_or(1.0) })
        })
        .collect()
}

pub fn timeline_sections(project: &Project) -> Vec<SectionMark> {
    let Some(clip) = beat_clip(project) else { return Vec::new() };
    let Some(a) = clip.media_id().and_then(|m| project.music.get(&m)) else { return Vec::new() };
    a.sections
        .iter()
        .filter_map(|s| {
            let start = clip.start + ((s.start - clip.source_in) / clip.speed).max(0.0);
            let end = (clip.start + (s.end - clip.source_in) / clip.speed).min(clip.end());
            (end > start + 0.05).then(|| SectionMark { start, end, label: s.label.clone(), energy: s.energy })
        })
        .collect()
}

/// How often to cut or pulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Every {
    Beat,
    TwoBeats,
    Bar,
    TwoBars,
    /// Faster in loud sections (every 2 beats), slower in quiet ones (every bar or 2).
    Auto,
    /// At each section change (intro → verse → chorus…), and every 8 bars inside long sections.
    Section,
}

impl Every {
    pub const ALL: [Every; 5] = [Every::Auto, Every::Beat, Every::TwoBeats, Every::Bar, Every::TwoBars];
    pub fn label(self) -> &'static str {
        match self {
            Every::Beat => "Every beat",
            Every::TwoBeats => "Every 2 beats",
            Every::Bar => "Every bar",
            Every::TwoBars => "Every 2 bars",
            Every::Auto => "Auto (follows the energy)",
            Every::Section => "Every section (8 bars max)",
        }
    }
    pub fn parse(s: &str) -> Option<Every> {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "beat" | "every_beat" | "1" => Some(Every::Beat),
            "two_beats" | "2_beats" | "half_bar" => Some(Every::TwoBeats),
            "bar" | "every_bar" => Some(Every::Bar),
            "two_bars" | "2_bars" => Some(Every::TwoBars),
            "auto" => Some(Every::Auto),
            "section" | "sections" | "every_section" => Some(Every::Section),
            _ => None,
        }
    }
}

/// Cut points (timeline seconds) from the beat grid within `[from, to]`.
pub fn cut_points(project: &Project, from: f64, to: f64, every: Every) -> Vec<f64> {
    let beats = timeline_beats(project);
    let sections = timeline_sections(project);
    if every == Every::Section {
        return section_points(&beats, &sections, from, to);
    }
    let energy_at = |t: f64| sections.iter().find(|s| t >= s.start && t < s.end).map_or(0.6, |s| s.energy);
    let mut pts = Vec::new();
    let mut since = 0usize;
    let mut started = false;
    for b in beats.iter().filter(|b| b.time >= from - 1e-6 && b.time <= to + 1e-6) {
        // Start counting on a downbeat so cuts land on bar starts.
        if !started {
            if !b.downbeat && matches!(every, Every::Bar | Every::TwoBars | Every::Auto) && pts.is_empty() && b.time - from < 4.0 {
                continue;
            }
            started = true;
            pts.push(b.time);
            since = 0;
            continue;
        }
        since += 1;
        let need = match every {
            Every::Beat => 1,
            Every::TwoBeats => 2,
            Every::Bar => 4,
            Every::TwoBars => 8,
            Every::Section => unreachable!("handled above"),
            Every::Auto => {
                let e = energy_at(b.time);
                if e >= 0.8 { 2 } else if e >= 0.45 { 4 } else { 8 }
            }
        };
        if since >= need {
            pts.push(b.time);
            since = 0;
        }
    }
    pts
}

/// Section starts in `[from, to]`, plus a cut every 8 bars inside sections longer
/// than that (so a long intro doesn't sit on one angle).
fn section_points(beats: &[BeatMark], sections: &[SectionMark], from: f64, to: f64) -> Vec<f64> {
    let downbeats: Vec<f64> = beats.iter().filter(|b| b.downbeat).map(|b| b.time).collect();
    let mut pts = vec![from];
    for s in sections.iter().filter(|s| s.end > from + 1e-6 && s.start < to - 1e-6) {
        let start = s.start.max(from);
        if start > from + 0.05 {
            pts.push(start);
        }
        // Every 8th downbeat after the section start, while still inside it.
        let inside: Vec<f64> = downbeats.iter().copied().filter(|d| *d > start + 0.05 && *d < s.end.min(to) - 0.05).collect();
        for d in inside.iter().skip(7).step_by(8) {
            if s.end.min(to) - d >= 2.0 {
                pts.push(*d);
            }
        }
    }
    pts.sort_by(|a, b| a.total_cmp(b));
    pts.dedup_by(|b, a| (*b - *a).abs() < 0.05);
    pts
}

/// Builds a montage of `media` (videos/images) cut on the beat over `[from, to]`.
/// Clips go on the main track if it is free there, else on a new video track above it.
/// Their own sound is muted (the song is the soundtrack). Returns the new clip ids.
pub fn beat_montage(project: &mut Project, media: &[Id], from: f64, to: f64, every: Every, shuffle: bool) -> Result<Vec<Id>> {
    let usable: Vec<Id> = media.iter().copied().filter(|m| project.media(*m).is_some_and(|x| x.kind != MediaKind::Audio)).collect();
    if usable.is_empty() {
        bail!("pick some video clips or images for the montage");
    }
    let mut pts = cut_points(project, from, to, every);
    if pts.len() < 2 {
        bail!("no beats in that range — detect beats on the song first");
    }
    if *pts.last().unwrap() < to - 0.05 {
        pts.push(to);
    }
    // Simple deterministic shuffle so it's reproducible.
    let mut order: Vec<Id> = usable.clone();
    if shuffle {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15 ^ order.len() as u64;
        for i in (1..order.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(i, (seed % (i as u64 + 1)) as usize);
        }
    }
    let main = project.main_track();
    let (start, end) = (pts[0], *pts.last().unwrap());
    let ti = if project.fits(main, start, end, None) { main } else { project.insert_track(TrackKind::Video, Some(main)) };
    // Each source is used in successive, non-overlapping pieces.
    let mut cursor: std::collections::HashMap<Id, f64> = std::collections::HashMap::new();
    let mut ids = Vec::new();
    let mut k = 0usize;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = b - a;
        if len < crate::model::MIN_CLIP_LEN {
            continue;
        }
        // Next source that still has `len` seconds left (wrapping around to its start).
        let mut chosen = None;
        for tries in 0..order.len() {
            let m = order[(k + tries) % order.len()];
            let item = project.media(m).cloned().unwrap();
            let src_len = item.source_len().unwrap_or(f64::INFINITY);
            if src_len + 1e-6 < len {
                continue;
            }
            let mut at = *cursor.get(&m).unwrap_or(&if src_len.is_finite() && src_len > len * 3.0 { (src_len * 0.05).min(2.0) } else { 0.0 });
            if at + len > src_len {
                at = 0.0;
            }
            cursor.insert(m, at + len);
            chosen = Some((m, at, tries));
            break;
        }
        let Some((m, at, tries)) = chosen else { continue };
        k += tries + 1;
        let Some(mut clip) = project.new_media_clip(m, a) else { continue };
        clip.start = a;
        clip.duration = len;
        if project.media(m).is_some_and(|x| x.kind == MediaKind::Video) {
            clip.source_in = at;
        }
        clip.volume = 0.0;
        ids.push(clip.id);
        project.tracks[ti].clips.push(clip);
    }
    project.sort_track(ti);
    if ids.is_empty() {
        bail!("the clips are shorter than one cut; pick a slower cut rate");
    }
    Ok(ids)
}

/// Per-beat animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeatEffect {
    /// Zoom punch: scale jumps up on the beat and eases back.
    Pulse,
    /// Quick sideways jolt on the beat.
    Shake,
    /// Brief dip in opacity on the beat (a flicker).
    Flicker,
    /// Each beat zooms in a little further (a stepped push-in), reset every 4 bars.
    StepZoom,
}

impl BeatEffect {
    pub const ALL: [BeatEffect; 4] = [BeatEffect::Pulse, BeatEffect::Shake, BeatEffect::Flicker, BeatEffect::StepZoom];
    pub fn label(self) -> &'static str {
        match self {
            BeatEffect::Pulse => "Zoom punch",
            BeatEffect::Shake => "Shake",
            BeatEffect::Flicker => "Flicker",
            BeatEffect::StepZoom => "Step zoom",
        }
    }
    pub fn parse(s: &str) -> Option<BeatEffect> {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "pulse" | "zoom_punch" | "punch" => Some(BeatEffect::Pulse),
            "shake" => Some(BeatEffect::Shake),
            "flicker" | "flash" | "blink" => Some(BeatEffect::Flicker),
            "step_zoom" | "stepzoom" => Some(BeatEffect::StepZoom),
            _ => None,
        }
    }
    fn prop(self) -> Prop {
        match self {
            BeatEffect::Pulse | BeatEffect::StepZoom => Prop::Scale,
            BeatEffect::Shake => Prop::X,
            BeatEffect::Flicker => Prop::Opacity,
        }
    }
}

/// Writes beat-synced keyframes onto each clip (replacing that property's keyframes).
/// `amount` is 0..1 (strength). Returns the number of keyframes written.
pub fn apply_beat_effect(project: &mut Project, clips: &[Id], effect: BeatEffect, every: Every, amount: f32) -> usize {
    let amount = amount.clamp(0.05, 1.0);
    let beats = timeline_beats(project);
    let period = beat_clip(project).and_then(|c| c.media_id()).and_then(|m| project.music.get(&m)).map_or(0.5, |a| a.period() / beat_clip(project).map_or(1.0, |c| c.speed));
    let mut written = 0;
    for &id in clips {
        let Some(c) = project.clip(id).cloned() else { continue };
        let hits = cut_points_from(&beats, c.start, c.end(), every);
        if hits.is_empty() {
            continue;
        }
        let base = effect.prop().base(&c);
        let rel = |t: f64| t - c.start;
        let decay = (period * 0.6).clamp(0.08, 0.35);
        let mut keys = vec![Keyframe { time: 0.0, value: base, ease: Ease::Linear }];
        let mut step = 0;
        for (n, &t) in hits.iter().enumerate() {
            let t0 = rel(t);
            let next = hits.get(n + 1).map_or(c.duration, |&x| rel(x));
            let d = decay.min((next - t0) * 0.8);
            match effect {
                BeatEffect::Pulse => {
                    let peak = base * (1.0 + 0.12 * amount);
                    keys.push(Keyframe { time: (t0 - 0.001).max(0.0), value: base, ease: Ease::Linear });
                    keys.push(Keyframe { time: t0, value: peak, ease: Ease::EaseOut });
                    keys.push(Keyframe { time: t0 + d, value: base, ease: Ease::Linear });
                }
                BeatEffect::Shake => {
                    let dir = if n % 2 == 0 { 1.0 } else { -1.0 };
                    keys.push(Keyframe { time: (t0 - 0.001).max(0.0), value: base, ease: Ease::Linear });
                    keys.push(Keyframe { time: t0, value: base + dir * 0.025 * amount, ease: Ease::EaseOut });
                    keys.push(Keyframe { time: t0 + d.min(0.15), value: base, ease: Ease::Linear });
                }
                BeatEffect::Flicker => {
                    keys.push(Keyframe { time: (t0 - 0.001).max(0.0), value: base, ease: Ease::Linear });
                    keys.push(Keyframe { time: t0, value: base * (1.0 - 0.7 * amount), ease: Ease::EaseOut });
                    keys.push(Keyframe { time: t0 + d.min(0.12), value: base, ease: Ease::Linear });
                }
                BeatEffect::StepZoom => {
                    if n > 0 && n % 16 == 0 {
                        step = 0;
                    }
                    step += 1;
                    let v = base * (1.0 + 0.03 * amount * step as f32);
                    keys.push(Keyframe { time: t0, value: v, ease: Ease::Hold });
                }
            }
        }
        keys.retain(|k| k.time <= c.duration + 1e-6);
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        keys.dedup_by(|b, a| (b.time - a.time).abs() < 1e-4);
        written += keys.len();
        if let Some(clip) = project.clip_mut(id) {
            clip.keyframes.insert(effect.prop(), keys);
        }
    }
    written
}

fn cut_points_from(beats: &[BeatMark], from: f64, to: f64, every: Every) -> Vec<f64> {
    let step = match every {
        Every::Beat | Every::Auto => 1,
        Every::Section => 16,
        Every::TwoBeats => 2,
        Every::Bar => 4,
        Every::TwoBars => 8,
    };
    let inside: Vec<&BeatMark> = beats.iter().filter(|b| b.time >= from - 1e-6 && b.time < to - 0.02).collect();
    let first = if step >= 4 { inside.iter().position(|b| b.downbeat).unwrap_or(0) } else { 0 };
    inside.iter().skip(first).step_by(step).map(|b| b.time).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaItem;

    /// A synthetic track: kick on every beat (louder on beat 1 of each bar), quiet first
    /// 8 bars then a loud section.
    fn click_track(bpm: f64, secs: f64, loud_after: f64) -> Vec<f32> {
        let n = (secs * RATE as f64) as usize;
        let mut s = vec![0.0f32; n];
        let period = 60.0 / bpm;
        let mut k = 0;
        loop {
            let t0 = 0.5 + k as f64 * period;
            if t0 >= secs - 0.2 {
                break;
            }
            let gain = if t0 >= loud_after { 0.9 } else { 0.3 } * if k % 4 == 0 { 1.0 } else { 0.6 };
            let start = (t0 * RATE as f64) as usize;
            for i in 0..(0.12 * RATE as f64) as usize {
                if start + i < n {
                    let t = i as f32 / RATE as f32;
                    let freq = if k % 4 == 0 { 60.0 } else { 400.0 };
                    s[start + i] += gain * (2.0 * std::f32::consts::PI * freq * t).sin() * (-t * 30.0).exp();
                }
            }
            k += 1;
        }
        // A quiet pad so loudness differs between sections.
        for (i, x) in s.iter_mut().enumerate() {
            let t = i as f64 / RATE as f64;
            let pad = if t >= loud_after { 0.25 } else { 0.05 };
            *x += pad * (2.0 * std::f32::consts::PI * 220.0 * t as f32).sin();
        }
        s
    }


    /// Manual check on a real song: `RUSTCUT_SONG=path cargo test --lib real_song -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_song() {
        let Ok(path) = std::env::var("RUSTCUT_SONG") else { return };
        let t = std::time::Instant::now();
        let a = analyze(Path::new(&path)).unwrap();
        println!("{:.1} BPM, {} beats, first downbeat {:.2}s, took {:?}", a.bpm, a.beats.len(), a.beats[a.first_downbeat], t.elapsed());
        for s in &a.sections {
            println!("  {:>6.1}–{:>6.1}  {:<7} energy {:.2}", s.start, s.end, s.label, s.energy);
        }
        println!("first beats: {:?}", &a.beats[..8.min(a.beats.len())].iter().map(|b| (b * 100.0).round() / 100.0).collect::<Vec<_>>());
    }


    /// Real FFmpeg: hundreds of beat keyframes (pulse + shake) still export.
    #[test]
    fn many_beat_keyframes_export() {
        let dir = std::env::temp_dir().join(format!("rc-beatfx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("img.png");
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        if !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=640x360:d=1", "-frames:v", "1", img.to_str().unwrap()]) {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let mut p = Project::default();
        let song = p.add_media(MediaItem { id: 0, path: "song.mp3".into(), name: "song".into(), kind: MediaKind::Audio, duration: 60.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let period = 60.0 / 144.0;
        let beats: Vec<f64> = (0..144).map(|i| i as f64 * period).collect();
        let n = beats.len();
        p.music.insert(song, MusicAnalysis { bpm: 144.0, beats, first_downbeat: 0, beats_per_bar: 4, sections: vec![], beat_energy: vec![1.0; n] });
        let sc = p.new_media_clip(song, 0.0).unwrap();
        p.place_clip(sc, None);
        let m = p.add_media(ffmpeg::probe(&img).unwrap().into_media_item(img.clone()));
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = 60.0;
        let id = c.id;
        p.append_to_main(c);
        let k1 = apply_beat_effect(&mut p, &[id], BeatEffect::Pulse, Every::Beat, 0.8);
        let k2 = apply_beat_effect(&mut p, &[id], BeatEffect::Shake, Every::Beat, 0.8);
        assert!(k1 + k2 > 800, "{k1} + {k2}");
        // Drop the (fake) song so the export only needs the image.
        p.tracks.retain(|t| t.kind != TrackKind::Audio);
        let keys_backup = p.clip(id).unwrap().keyframes.clone();
        p.music.clear();
        assert_eq!(p.clip(id).unwrap().keyframes, keys_backup);
        let out = dir.join("out.mp4");
        let settings = crate::export::ExportSettings { path: out.clone(), short_side: 240, fps: 24, crf: 35, preset: "ultrafast".into() };
        crate::export::run(&p, &settings, &std::sync::atomic::AtomicBool::new(false), |_| {}).unwrap();
        let got = ffmpeg::probe(&out).unwrap();
        assert!((got.duration - 60.0).abs() < 0.5, "{}", got.duration);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fft_finds_a_sine() {
        let n = 256;
        let mut re: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 8.0 * i as f32 / n as f32).sin()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let peak = (0..n / 2).max_by(|a, b| mags[*a].total_cmp(&mags[*b])).unwrap();
        assert_eq!(peak, 8);
    }

    #[test]
    fn finds_tempo_beats_downbeats_and_sections() {
        let bpm = 128.0;
        let samples = click_track(bpm, 40.0, 20.0);
        let a = analyze_samples(&samples).unwrap();
        assert!((a.bpm - bpm).abs() < 2.0, "bpm {}", a.bpm);
        let period = 60.0 / bpm;
        // Beats line up with the clicks (within ~35 ms).
        let mut on = 0;
        for b in &a.beats {
            let k = ((b - 0.5) / period).round();
            if (b - (0.5 + k * period)).abs() < 0.035 {
                on += 1;
            }
        }
        assert!(on as f64 >= a.beats.len() as f64 * 0.9, "{on}/{} beats on the grid", a.beats.len());
        // The first downbeat is a bar start (k % 4 == 0).
        let db = a.beats[a.first_downbeat];
        let k = ((db - 0.5) / period).round() as i64;
        assert_eq!(k % 4, 0, "downbeat at {db}");
        // A quiet part followed by a loud one.
        assert!(a.sections.len() >= 2, "{:?}", a.sections);
        let loud = a.section_at(30.0).unwrap();
        assert_eq!(loud.label, "chorus");
        assert!(a.section_at(5.0).unwrap().energy < 0.7);
    }

    fn project_with_song() -> (Project, Id) {
        let mut p = Project::default();
        let song = p.add_media(MediaItem {
            id: 0, path: "song.mp3".into(), name: "song".into(), kind: MediaKind::Audio,
            duration: 60.0, width: 0, height: 0, fps: 0.0, has_audio: true,
        });
        let beats: Vec<f64> = (0..120).map(|i| i as f64 * 0.5).collect();
        p.music.insert(song, MusicAnalysis {
            bpm: 120.0,
            beats,
            first_downbeat: 0,
            beats_per_bar: 4,
            sections: vec![
                Section { start: 0.0, end: 16.0, label: "intro".into(), energy: 0.3 },
                Section { start: 16.0, end: 60.0, label: "chorus".into(), energy: 1.0 },
            ],
            beat_energy: vec![1.0; 120],
        });
        let mut c = p.new_media_clip(song, 0.0).unwrap();
        c.start = 2.0; // song starts 2 s into the timeline
        p.place_clip(c, None);
        (p, song)
    }

    #[test]
    fn beats_follow_the_song_clip() {
        let (p, _) = project_with_song();
        let b = timeline_beats(&p);
        assert_eq!(b[0].time, 2.0);
        assert!(b[0].downbeat && !b[1].downbeat && b[4].downbeat);
        assert_eq!(timeline_sections(&p)[1].start, 18.0);
        let bars = cut_points(&p, 2.0, 12.0, Every::Bar);
        assert_eq!(bars, vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0]);
        // Sections: the intro start, the chorus start (18 s), then every 8 bars (16 s) inside the long chorus.
        let secs = cut_points(&p, 2.0, 62.0, Every::Section);
        assert_eq!(secs, vec![2.0, 18.0, 34.0, 50.0]);
        // Auto: slow (2 bars) in the quiet intro, every 2 beats in the chorus.
        let auto = cut_points(&p, 2.0, 22.0, Every::Auto);
        assert_eq!(&auto[..3], &[2.0, 6.0, 10.0]);
        assert!(auto.windows(2).any(|w| (w[1] - w[0] - 1.0).abs() < 1e-9));
    }

    #[test]
    fn montage_cuts_on_the_beat_and_mutes_clips() {
        let (mut p, _) = project_with_song();
        let v = |p: &mut Project, name: &str| p.add_media(MediaItem {
            id: 0, path: format!("{name}.mp4").into(), name: name.into(), kind: MediaKind::Video,
            duration: 30.0, width: 1920, height: 1080, fps: 30.0, has_audio: true,
        });
        let (a, b) = (v(&mut p, "a"), v(&mut p, "b"));
        let ids = beat_montage(&mut p, &[a, b], 2.0, 10.0, Every::Bar, false).unwrap();
        assert_eq!(ids.len(), 4);
        let clips: Vec<Clip> = ids.iter().map(|i| p.clip(*i).unwrap().clone()).collect();
        assert_eq!(clips[0].start, 2.0);
        assert!(clips.windows(2).all(|w| (w[0].end() - w[1].start).abs() < 1e-9), "back to back");
        assert!(clips.iter().all(|c| c.volume == 0.0 && (c.duration - 2.0).abs() < 1e-9));
        assert_ne!(clips[0].media_id(), clips[1].media_id(), "alternates sources");
        // Same source used twice → different, non-overlapping parts.
        assert!(clips[2].source_in >= clips[0].source_in + 2.0 - 1e-9);
    }

    #[test]
    fn beat_effects_write_keyframes_on_beats() {
        let (mut p, _) = project_with_song();
        let img = p.add_media(MediaItem {
            id: 0, path: "x.png".into(), name: "x".into(), kind: MediaKind::Image,
            duration: 0.0, width: 100, height: 100, fps: 0.0, has_audio: false,
        });
        let mut c = p.new_media_clip(img, 2.0).unwrap();
        c.start = 2.0;
        c.duration = 4.0;
        let id = c.id;
        p.place_clip(c, None);
        let n = apply_beat_effect(&mut p, &[id], BeatEffect::Pulse, Every::Beat, 1.0);
        assert!(n > 8);
        let clip = p.clip(id).unwrap();
        // Peak exactly on the beat at 3.0 s (timeline), back to rest before the next beat.
        assert!((clip.value_at(Prop::Scale, 3.0) - 1.12).abs() < 1e-3);
        assert!((clip.value_at(Prop::Scale, 3.45) - 1.0).abs() < 1e-3);
    }
}
