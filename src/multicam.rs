//! Multi-cam for music videos: line up performance takes against the master song by
//! their audio, then cut between them (live with number keys, or automatically on
//! beats, bars or sections) onto a "Multi-cam edit" track.

use crate::model::{Clip, Id, MediaKind, Project, TrackKind};
use crate::music::{self, Every};
use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Onset envelope used for syncing (≈ 43 frames/s).
pub fn sync_envelope(path: &Path) -> Result<(Vec<f32>, f64)> {
    let env = music::envelope(&music::decode_mono(path)?);
    // Combine onsets and loudness changes so both drums and vocals help.
    let mut v: Vec<f32> = env.onset.iter().zip(&env.rms).map(|(o, r)| o + 0.5 * r).collect();
    let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
    v.iter_mut().for_each(|x| *x -= mean);
    Ok((v, env.fps))
}

/// Where `take` starts inside `reference`, in seconds (negative if the take starts
/// before the song), and a confidence (peak sharpness, ~1 = unsure, >3 = solid).
pub fn find_offset(reference: &[f32], take: &[f32], fps: f64) -> (f64, f32) {
    let n = (reference.len() + take.len()).next_power_of_two();
    let (mut ar, mut ai) = (vec![0.0f32; n], vec![0.0f32; n]);
    let (mut br, mut bi) = (vec![0.0f32; n], vec![0.0f32; n]);
    ar[..reference.len()].copy_from_slice(reference);
    br[..take.len()].copy_from_slice(take);
    music::fft(&mut ar, &mut ai);
    music::fft(&mut br, &mut bi);
    // R · conj(T), then inverse FFT (via conj → FFT → conj).
    let (mut cr, mut ci) = (vec![0.0f32; n], vec![0.0f32; n]);
    for k in 0..n {
        cr[k] = ar[k] * br[k] + ai[k] * bi[k];
        ci[k] = -(ai[k] * br[k] - ar[k] * bi[k]);
    }
    music::fft(&mut cr, &mut ci);
    // cr[lag] (scaled) = Σ ref[t + lag] · take[t]; lags past n/2 are negative.
    let mut best = (0usize, f32::MIN);
    for (lag, &v) in cr.iter().enumerate() {
        if v > best.1 {
            best = (lag, v);
        }
    }
    let lag = if best.0 > n / 2 { best.0 as i64 - n as i64 } else { best.0 as i64 };
    // Confidence: peak vs the typical correlation magnitude away from the peak.
    let mut others: Vec<f32> = cr
        .iter()
        .enumerate()
        .filter(|(i, _)| (*i as i64 - best.0 as i64).abs() > (fps * 0.5) as i64)
        .map(|(_, v)| v.abs())
        .collect();
    others.sort_by(|a, b| b.total_cmp(a));
    let typical = others.get(others.len() / 100).copied().unwrap_or(1.0).max(1e-9);
    (lag as f64 / fps, best.1 / typical)
}

#[derive(Clone, Debug)]
pub struct Synced {
    pub media: Id,
    pub clip: Id,
    pub angle: u32,
    /// Timeline time where the take starts (before trimming to the song).
    pub offset: f64,
    pub confidence: f32,
}

/// Measures where each take starts in the song (song source seconds) and how sure we are.
/// Slow (decodes audio); safe to run on a worker thread.
pub fn measure_offsets(song: &Path, takes: &[std::path::PathBuf], report: &mut dyn FnMut(f32, &str)) -> Result<Vec<(f64, f32)>> {
    report(0.05, "Listening to the song…");
    let (reference, fps) = sync_envelope(song)?;
    let mut out = Vec::new();
    for (k, path) in takes.iter().enumerate() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        report(0.1 + 0.85 * k as f32 / takes.len().max(1) as f32, &format!("Syncing {name}…"));
        let (env, _) = sync_envelope(path).map_err(|e| anyhow!("{name}: {e}"))?;
        out.push(find_offset(&reference, &env, fps));
    }
    Ok(out)
}

/// Puts each measured take on its own video track ("Cam 1", "Cam 2", …), muted and
/// trimmed to the song clip.
pub fn place_takes(project: &mut Project, song_clip: Id, measured: &[(Id, f64, f32)]) -> Result<Vec<Synced>> {
    let song = project.clip(song_clip).cloned().ok_or_else(|| anyhow!("the song clip was removed"))?;
    let mut out = Vec::new();
    let first_angle = project.tracks.iter().filter_map(|t| t.angle).max().unwrap_or(0) + 1;
    let song_speed = song.speed.max(1e-6);
    for (k, &(media, src_offset, confidence)) in measured.iter().enumerate() {
        let Some(item) = project.media(media).cloned() else { continue };
        let angle = first_angle + k as u32;
        // Take time 0 ↔ song source time `src_offset` ↔ this timeline time:
        let t0 = song.start + (src_offset - song.source_in) / song_speed;
        let (mut start, mut source_in) = (t0, 0.0);
        if start < song.start {
            source_in = song.start - start;
            start = song.start;
        }
        let end = (t0 + item.duration).min(song.end());
        if end - start < crate::model::MIN_CLIP_LEN {
            continue;
        }
        let Some(mut clip) = project.new_media_clip(item.id, start) else { continue };
        clip.start = start;
        clip.source_in = source_in;
        clip.duration = end - start;
        clip.volume = 0.0;
        let id = clip.id;
        let main = project.main_track();
        let ti = project.insert_track(TrackKind::Video, Some(main));
        project.tracks[ti].name = format!("Cam {angle} · {}", item.name);
        project.tracks[ti].angle = Some(angle);
        project.tracks[ti].clips.push(clip);
        out.push(Synced { media: item.id, clip: id, angle, offset: t0, confidence });
    }
    if out.is_empty() {
        bail!("none of the takes overlap the song");
    }
    Ok(out)
}

/// Song clip path plus take paths, validated.
pub fn sync_inputs(project: &Project, song_clip: Id, takes: &[Id]) -> Result<(std::path::PathBuf, Vec<std::path::PathBuf>)> {
    let song = project.clip(song_clip).ok_or_else(|| anyhow!("the song clip was removed"))?;
    let song_path = song.media_id().and_then(|m| project.media(m)).map(|m| m.path.clone()).ok_or_else(|| anyhow!("the song clip has no media"))?;
    let mut paths = Vec::new();
    for &m in takes {
        let item = project.media(m).ok_or_else(|| anyhow!("no media with id {m}"))?;
        if item.kind != MediaKind::Video || !item.has_audio {
            bail!("{} has no sound to sync with", item.name);
        }
        paths.push(item.path.clone());
    }
    if paths.is_empty() {
        bail!("pick at least one take");
    }
    Ok((song_path, paths))
}

/// Measures and places in one go (MCP / tests).
pub fn sync_takes(project: &mut Project, song_clip: Id, takes: &[Id], report: &mut dyn FnMut(f32, &str)) -> Result<Vec<Synced>> {
    let (song, paths) = sync_inputs(project, song_clip, takes)?;
    let offsets = measure_offsets(&song, &paths, report)?;
    let measured: Vec<(Id, f64, f32)> = takes.iter().zip(offsets).map(|(m, (o, c))| (*m, o, c)).collect();
    place_takes(project, song_clip, &measured)
}

/// The song clip to sync against: the beat clip, else the first clip on an audio track.
pub fn song_clip(project: &Project) -> Option<Id> {
    music::beat_clip(project)
        .map(|c| c.id)
        .or_else(|| project.tracks.iter().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| &t.clips).next().map(|c| c.id))
}

/// Angle tracks: (angle, track index), sorted by angle.
pub fn angles(project: &Project) -> Vec<(u32, usize)> {
    let mut v: Vec<(u32, usize)> = project.tracks.iter().enumerate().filter_map(|(i, t)| t.angle.map(|a| (a, i))).collect();
    v.sort();
    v
}

fn angle_clip_at(project: &Project, angle: u32, t: f64) -> Option<&Clip> {
    let (_, ti) = angles(project).into_iter().find(|(a, _)| *a == angle)?;
    project.tracks[ti].clips.iter().find(|c| t >= c.start - 1e-6 && t < c.end() - 1e-6)
}

/// Rebuilds the "Multi-cam edit" track from switch points `(time, angle)` over
/// `[from, to]`. Angle tracks are hidden so only the edit shows. Returns clip count.
pub fn build_edit(project: &mut Project, switches: &[(f64, u32)], from: f64, to: f64) -> Result<usize> {
    if angles(project).is_empty() {
        bail!("sync some takes first (Multi-cam → Sync takes)");
    }
    let mut sw: Vec<(f64, u32)> = switches.iter().copied().filter(|(t, _)| *t < to - 1e-6).collect();
    sw.sort_by(|a, b| a.0.total_cmp(&b.0));
    if sw.first().is_none_or(|s| s.0 > from + 1e-6) {
        let first = sw.first().map_or_else(|| angles(project)[0].0, |s| s.1);
        sw.insert(0, (from, first));
    }
    let mut clips = Vec::new();
    for (k, &(a, angle)) in sw.iter().enumerate() {
        let a = a.max(from);
        let b = sw.get(k + 1).map_or(to, |s| s.0).min(to);
        // Within [a, b) the chosen angle may have gaps: copy whatever pieces exist.
        let pieces: Vec<Clip> = {
            let Some((_, ti)) = angles(project).into_iter().find(|(x, _)| *x == angle) else { continue };
            project.tracks[ti].clips.iter().filter(|c| c.end() > a + 1e-6 && c.start < b - 1e-6).cloned().collect()
        };
        for src in pieces {
            let (s, e) = (a.max(src.start), b.min(src.end()));
            if e - s < crate::model::MIN_CLIP_LEN {
                continue;
            }
            let mut c = src.clone();
            c.id = project.new_id();
            c.source_in = src.source_in + (s - src.start) * src.speed;
            c.start = s;
            c.duration = e - s;
            c.keyframes.clear();
            c.transition_in = None;
            clips.push(c);
        }
    }
    if clips.is_empty() {
        bail!("the chosen angles have no footage in that range");
    }
    let edit = match project.tracks.iter().position(|t| t.multicam) {
        Some(i) => i,
        None => {
            let top_video = project.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap_or(0);
            let i = project.insert_track(TrackKind::Video, Some(top_video));
            project.tracks[i].name = "Multi-cam edit".into();
            project.tracks[i].multicam = true;
            i
        }
    };
    project.tracks[edit].clips.retain(|c| c.end() <= from + 1e-6 || c.start >= to - 1e-6);
    let n = clips.len();
    project.tracks[edit].clips.extend(clips);
    project.sort_track(edit);
    for (_, ti) in angles(project) {
        project.tracks[ti].hidden = true;
    }
    Ok(n)
}

/// Automatic angle switching on the beat grid (or every few seconds without one).
/// Never repeats the same angle twice in a row and only picks angles with footage there.
pub fn auto_switches(project: &Project, from: f64, to: f64, every: Every) -> Vec<(f64, u32)> {
    let all: Vec<u32> = angles(project).iter().map(|a| a.0).collect();
    if all.is_empty() {
        return Vec::new();
    }
    let mut pts = music::cut_points(project, from, to, every);
    if pts.len() < 2 {
        pts = (0..).map(|k| from + k as f64 * 4.0).take_while(|t| *t < to).collect();
    }
    let mut out: Vec<(f64, u32)> = Vec::new();
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    for t in pts {
        let prev = out.last().map(|s| s.1);
        let mut avail: Vec<u32> = all.iter().copied().filter(|a| Some(*a) != prev && angle_clip_at(project, *a, t + 0.01).is_some()).collect();
        if avail.is_empty() {
            if let Some(p) = prev.filter(|p| angle_clip_at(project, *p, t + 0.01).is_some()) {
                avail.push(p);
            } else {
                continue;
            }
        }
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let pick = avail[(seed % avail.len() as u64) as usize];
        if prev != Some(pick) {
            out.push((t, pick));
        }
    }
    out
}

/// Snaps live switch points to the nearest beat (within 0.25 s) when a grid exists.
pub fn snap_to_beats(project: &Project, switches: &mut [(f64, u32)]) {
    let beats = music::timeline_beats(project);
    if beats.is_empty() {
        return;
    }
    for s in switches.iter_mut() {
        if let Some(b) = beats.iter().min_by(|a, b| (a.time - s.0).abs().total_cmp(&(b.time - s.0).abs())) {
            if (b.time - s.0).abs() <= 0.25 {
                s.0 = b.time;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaItem;

    #[test]
    fn finds_the_offset_of_a_take() {
        // A random-ish onset pattern; the take is the reference from 7.0 s on.
        let fps = 43.0;
        let mut seed: u32 = 12345;
        let reference: Vec<f32> = (0..(60.0 * fps) as usize)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                if seed >> 28 == 0 { 1.0 } else { 0.0 }
            })
            .collect();
        let start = (7.0 * fps) as usize;
        let take: Vec<f32> = reference[start..start + (20.0 * fps) as usize].to_vec();
        let (off, conf) = find_offset(&reference, &take, fps);
        assert!((off - 7.0).abs() < 0.03, "offset {off}");
        assert!(conf > 3.0, "confidence {conf}");
        // A take that starts before the song (negative offset).
        let mut early = vec![0.0f32; (2.0 * fps) as usize];
        early.extend_from_slice(&reference[..(10.0 * fps) as usize]);
        let (off, _) = find_offset(&reference, &early, fps);
        assert!((off + 2.0).abs() < 0.03, "offset {off}");
    }

    fn project_with_angles() -> Project {
        let mut p = Project::default();
        for (k, name) in ["a", "b", "c"].iter().enumerate() {
            let m = p.add_media(MediaItem {
                id: 0, path: format!("{name}.mp4").into(), name: name.to_string(), kind: MediaKind::Video,
                duration: 40.0, width: 1920, height: 1080, fps: 30.0, has_audio: true,
            });
            let mut c = p.new_media_clip(m, 0.0).unwrap();
            c.start = k as f64; // staggered starts
            c.duration = 30.0;
            let main = p.main_track();
            let ti = p.insert_track(TrackKind::Video, Some(main));
            p.tracks[ti].angle = Some(k as u32 + 1);
            p.tracks[ti].clips.push(c);
        }
        p
    }


    /// Real FFmpeg round trip: a song file, and a "phone take" of it (starts 7.5 s in,
    /// with background noise and a video track) is placed 7.5 s into the song.
    #[test]
    fn syncs_a_real_take() {
        use crate::media::ffmpeg;
        let dir = std::env::temp_dir().join(format!("rc-mc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (song, take) = (dir.join("song.wav"), dir.join("take.mp4"));
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        // An irregular drum-like pattern: noise bursts gated by incommensurate sines (no repeats).
        let expr = "random(0)*0.8*gt(sin(t*5.1)*sin(t*2.3+1)*sin(t*0.77+2),0.25)";
        if !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("aevalsrc='{expr}':s=22050:d=40"), song.to_str().unwrap()]) {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        assert!(ok(&[
            "-y", "-loglevel", "error", "-ss", "7.5", "-t", "20", "-i", song.to_str().unwrap(),
            "-f", "lavfi", "-i", "anoisesrc=a=0.05:d=20", "-f", "lavfi", "-i", "color=c=green:s=320x240:d=20",
            "-filter_complex", "[0:a][1:a]amix=inputs=2:duration=first[a]", "-map", "2:v", "-map", "[a]",
            "-shortest", "-pix_fmt", "yuv420p", take.to_str().unwrap(),
        ]));
        let mut p = Project::default();
        let s = p.add_media(ffmpeg::probe(&song).unwrap().into_media_item(song.clone()));
        let t = p.add_media(ffmpeg::probe(&take).unwrap().into_media_item(take.clone()));
        let mut sc = p.new_media_clip(s, 0.0).unwrap();
        sc.start = 1.0;
        let song_clip = sc.id;
        p.place_clip(sc, None);
        let synced = sync_takes(&mut p, song_clip, &[t], &mut |_, _| {}).unwrap();
        assert_eq!(synced.len(), 1);
        assert!((synced[0].offset - 8.5).abs() < 0.05, "offset {} conf {}", synced[0].offset, synced[0].confidence);
        let c = p.clip(synced[0].clip).unwrap();
        assert!((c.start - 8.5).abs() < 0.05 && c.volume == 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// Manual: `RUSTCUT_SONG=path cargo test --lib real_song_take -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_song_take() {
        use crate::media::ffmpeg;
        let Ok(song) = std::env::var("RUSTCUT_SONG") else { return };
        let dir = std::env::temp_dir().join(format!("rc-mc2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let take = dir.join("take.mp4");
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        assert!(ok(&[
            "-y", "-loglevel", "error", "-ss", "30", "-t", "20", "-i", &song,
            "-f", "lavfi", "-i", "anoisesrc=a=0.08:d=20", "-f", "lavfi", "-i", "color=c=green:s=320x240:d=20",
            "-filter_complex", "[0:a]highpass=f=300,lowpass=f=4000[s];[s][1:a]amix=inputs=2:duration=first[a]",
            "-map", "2:v", "-map", "[a]", "-shortest", "-pix_fmt", "yuv420p", take.to_str().unwrap(),
        ]));
        let (r, fps) = sync_envelope(Path::new(&song)).unwrap();
        let (t, _) = sync_envelope(&take).unwrap();
        let (off, conf) = find_offset(&r, &t, fps);
        println!("offset {off:.3} (expected 30.000), confidence {conf:.1}");
        assert!((off - 30.0).abs() < 0.05);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_an_edit_from_switch_points() {
        let mut p = project_with_angles();
        let n = build_edit(&mut p, &[(2.0, 1), (5.0, 2), (9.0, 3), (12.0, 1)], 2.0, 15.0).unwrap();
        assert_eq!(n, 4);
        let edit = p.tracks.iter().find(|t| t.multicam).unwrap();
        let c = &edit.clips;
        assert_eq!((c[0].start, c[0].duration), (2.0, 3.0));
        // Angle 2 starts at timeline 1.0, so at 5.0 its source is at 4.0.
        assert!((c[1].source_in - 4.0).abs() < 1e-9);
        assert!((c[3].end() - 15.0).abs() < 1e-9);
        assert!(p.tracks.iter().filter(|t| t.angle.is_some()).all(|t| t.hidden));
        // Rebuilding replaces the range instead of stacking.
        build_edit(&mut p, &[(2.0, 3)], 2.0, 15.0).unwrap();
        assert_eq!(p.tracks.iter().find(|t| t.multicam).unwrap().clips.len(), 1);
    }

    #[test]
    fn auto_switching_never_repeats_an_angle() {
        let p = project_with_angles();
        let sw = auto_switches(&p, 3.0, 30.0, Every::Bar);
        assert!(sw.len() >= 5);
        assert!(sw.windows(2).all(|w| w[0].1 != w[1].1));
        assert_eq!(sw[0].0, 3.0);
    }
}
