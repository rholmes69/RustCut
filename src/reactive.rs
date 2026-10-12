//! Audio-reactive visuals for music videos: FFmpeg visualizer overlays (rendered to a
//! transparent .mov and placed as an ordinary clip) and keyframes that follow the
//! song's bass or loudness.

use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{Ease, Id, Keyframe, Project, Prop, TrackKind, Transform};
use crate::music;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visualizer {
    /// Spectrum bars.
    Bars,
    /// Filled waveform.
    Wave,
    /// Thin waveform line.
    Line,
    /// Stereo vectorscope (swirling Lissajous lines).
    Scope,
    /// Spectrum bars bent into a ring (mirrored left/right, bass at the top).
    Circle,
}

impl Visualizer {
    pub const ALL: [Visualizer; 5] = [Visualizer::Bars, Visualizer::Circle, Visualizer::Wave, Visualizer::Line, Visualizer::Scope];
    pub fn label(self) -> &'static str {
        match self {
            Visualizer::Bars => "Spectrum bars",
            Visualizer::Wave => "Waveform",
            Visualizer::Line => "Waveform line",
            Visualizer::Scope => "Vectorscope",
            Visualizer::Circle => "Circular spectrum",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Visualizer::Bars => "bars",
            Visualizer::Wave => "wave",
            Visualizer::Line => "line",
            Visualizer::Scope => "scope",
            Visualizer::Circle => "circle",
        }
    }
    pub fn parse(s: &str) -> Option<Visualizer> {
        Visualizer::ALL.into_iter().find(|v| v.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Output size for a canvas.
    pub fn size(self, cw: u32, ch: u32) -> (u32, u32) {
        let even = |v: u32| v.max(2) & !1;
        match self {
            Visualizer::Scope | Visualizer::Circle => {
                let s = even(cw.min(ch) * 2 / 3);
                (s, s)
            }
            _ => (even(cw), even(ch / 4)),
        }
    }

    /// FFmpeg filter turning `[0:a]` into an RGBA picture with a transparent background.
    pub fn filter(self, w: u32, h: u32, fps: u32, hex: &str) -> String {
        match self {
            Visualizer::Bars => format!(
                "[0:a]aformat=channel_layouts=mono,showfreqs=s={w}x{h}:mode=bar:ascale=log:fscale=log:win_size=2048:colors=0x{hex},fps={fps},format=rgba,colorkey=black:0.08:0.0[v]"
            ),
            Visualizer::Wave => format!("[0:a]aformat=channel_layouts=mono,showwaves=s={w}x{h}:mode=cline:rate={fps}:colors=0x{hex}:draw=full,format=rgba[v]"),
            Visualizer::Line => format!("[0:a]aformat=channel_layouts=mono,showwaves=s={w}x{h}:mode=line:rate={fps}:colors=0x{hex}:draw=full,format=rgba[v]"),
            // Bars into a strip, bent into a ring by `remap` with the pixel maps from
            // `ring_maps` (inputs 1 and 2), then the black background keyed out.
            // (Keying before `remap` makes FFmpeg fail.)
            Visualizer::Circle => format!(
                "[0:a]aformat=channel_layouts=mono,showfreqs=s={RING_W}x{RING_H}:mode=bar:ascale=log:fscale=log:win_size=2048:colors=0x{hex},fps={fps},format=rgba[strip];[1:v]format=gray16[xm];[2:v]format=gray16[ym];[strip][xm][ym]remap=fill=black,format=rgba,colorkey=black:0.08:0.0[v]"
            ),
            Visualizer::Scope => format!(
                "[0:a]avectorscope=s={w}x{h}:rate={fps}:draw=line:zoom=0.9:scale=lin:mode=lissajous_xy,format=rgba,colorkey=black:0.06:0.0,colorchannelmixer=rr=0:gg=0:bb=0,lutrgb=r=0x{r}:g=0x{g}:b=0x{b}[v]",
                r = &hex[0..2],
                g = &hex[2..4],
                b = &hex[4..6]
            ),
        }
    }
}

/// Strip the circular spectrum is drawn into before bending.
const RING_W: u32 = 720;
const RING_H: u32 = 200;

/// `remap` maps (16-bit PGM) bending a RING_W × RING_H strip into an `s`×`s` ring:
/// angle from the top → strip x (mirrored), distance from the centre → bar height.
/// Pixels off the ring point at the strip's top-right corner (treble at full level:
/// practically always background, keyed out afterwards).
pub fn ring_maps(s: u32) -> (Vec<u8>, Vec<u8>) {
    let header = format!("P5\n{s} {s}\n65535\n").into_bytes();
    let (mut xm, mut ym) = (header.clone(), header);
    let c = s as f64 / 2.0;
    let (r0, r1) = (c * 0.42, c * 0.98);
    for y in 0..s {
        for x in 0..s {
            let (dx, dy) = (x as f64 + 0.5 - c, y as f64 + 0.5 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let (sx, sy) = if r < r0 || r > r1 {
                ((RING_W - 1) as u16, 0u16)
            } else {
                let theta = dx.atan2(-dy).abs() / std::f64::consts::PI; // 0 at the top, 1 at the bottom
                let a = (r - r0) / (r1 - r0); // 0 = inner edge (no level), 1 = outer (full)
                let sx = (theta * (RING_W - 1) as f64).round() as u16;
                let sy = ((1.0 - a) * (RING_H - 1) as f64).round() as u16;
                (sx, sy)
            };
            xm.extend_from_slice(&sx.to_be_bytes());
            ym.extend_from_slice(&sy.to_be_bytes());
        }
    }
    (xm, ym)
}

/// `RRGGBB` for a colour.
pub fn hex(c: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Renders a visualizer for `source` from `from` for `duration` seconds to a
/// transparent QuickTime (.mov, Animation codec) at `out`.
pub fn render_visualizer(source: &Path, from: f64, duration: f64, kind: Visualizer, size: (u32, u32), fps: u32, color: [u8; 3], out: &Path) -> Result<ProbeInfo> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let graph = kind.filter(size.0, size.1, fps, &hex(color));
    let mut cmd = ffmpeg::command(&ffmpeg::ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{from:.3}"), "-t", &format!("{duration:.3}"), "-i"]).arg(source);
    let maps = if kind == Visualizer::Circle {
        let (xm, ym) = ring_maps(size.0);
        let stem = out.with_extension("");
        let (xp, yp) = (stem.with_extension("xmap.pgm"), stem.with_extension("ymap.pgm"));
        std::fs::write(&xp, xm)?;
        std::fs::write(&yp, ym)?;
        // Same rate and length as the strip, so `remap` never has to buffer map frames.
        let (rate, len) = (fps.to_string(), format!("{duration:.3}"));
        cmd.args(["-loop", "1", "-framerate", &rate, "-t", &len, "-i"]).arg(&xp);
        cmd.args(["-loop", "1", "-framerate", &rate, "-t", &len, "-i"]).arg(&yp);
        Some((xp, yp))
    } else {
        None
    };
    let result = cmd
        .args(["-filter_complex", &graph, "-map", "[v]", "-an", "-c:v", "qtrle", "-pix_fmt", "argb", "-t", &format!("{duration:.3}")])
        .arg(out)
        .output()
        .context("could not start ffmpeg");
    if let Some((xp, yp)) = maps {
        let _ = std::fs::remove_file(xp);
        let _ = std::fs::remove_file(yp);
    }
    let result = result?;
    if !result.status.success() {
        bail!("FFmpeg could not draw the visualizer: {}", String::from_utf8_lossy(&result.stderr).trim());
    }
    ffmpeg::probe(out)
}

/// Song clip's source range: (path, source start, timeline start, duration, speed).
pub fn song_range(project: &Project, song_clip: Id) -> Result<(PathBuf, f64, f64, f64, f64)> {
    let c = project.clip(song_clip).ok_or_else(|| anyhow!("the song clip was removed"))?;
    let path = c.media_id().and_then(|m| project.media(m)).map(|m| m.path.clone()).ok_or_else(|| anyhow!("the song clip has no media"))?;
    Ok((path, c.source_in, c.start, c.duration, c.speed))
}

/// Places a rendered visualizer over the song's span on an overlay video track.
pub fn place_visualizer(project: &mut Project, kind: Visualizer, path: &Path, info: ProbeInfo, start: f64, position_y: f32) -> (Id, Option<Id>) {
    let media = project.add_media(info.into_media_item(path.to_path_buf()));
    let Some(mut clip) = project.new_media_clip(media, start) else { return (media, None) };
    clip.start = start;
    clip.transform = match kind {
        Visualizer::Scope | Visualizer::Circle => Transform { y: position_y, scale: 1.0, ..Transform::default() },
        _ => Transform { y: position_y, ..Transform::default() },
    };
    let id = clip.id;
    let (s, e) = (clip.start, clip.end());
    let main = project.main_track();
    let free = (0..project.tracks.len()).find(|&i| i != main && project.tracks[i].kind == TrackKind::Video && project.tracks[i].angle.is_none() && !project.tracks[i].multicam && project.fits(i, s, e, None));
    let ti = free.unwrap_or_else(|| {
        let top = project.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap_or(main);
        project.insert_track(TrackKind::Video, Some(top))
    });
    project.tracks[ti].clips.push(clip);
    project.sort_track(ti);
    (media, Some(id))
}

// ── reactive keyframes ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// Kick drum / bass hits.
    Bass,
    /// Overall loudness.
    Loudness,
}

impl Band {
    pub fn parse(s: &str) -> Option<Band> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bass" | "kick" => Some(Band::Bass),
            "loudness" | "volume" | "rms" => Some(Band::Loudness),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum React {
    /// Scale throbs with the music.
    Throb,
    /// Position jitters on hits.
    Shake,
    /// Picture glows (brighter, more saturated) on the hits.
    Glow,
    /// Blurs on the hits and snaps back into focus.
    Blur,
    /// Opacity follows the music (dimmer when quiet).
    Fade,
    /// Rotation wobbles on hits.
    Wobble,
}

impl React {
    pub const ALL: [React; 6] = [React::Throb, React::Glow, React::Blur, React::Shake, React::Fade, React::Wobble];
    pub fn label(self) -> &'static str {
        match self {
            React::Throb => "Throb (scale)",
            React::Shake => "Shake (position)",
            React::Glow => "Glow (brightness)",
            React::Blur => "Blur",
            React::Fade => "Fade (opacity)",
            React::Wobble => "Wobble (rotation)",
        }
    }
    pub fn parse(s: &str) -> Option<React> {
        match s.trim().to_ascii_lowercase().as_str() {
            "throb" | "scale" | "pulse" => Some(React::Throb),
            "shake" | "jitter" => Some(React::Shake),
            "glow" | "bright" | "brightness" => Some(React::Glow),
            "blur" => Some(React::Blur),
            "fade" | "opacity" => Some(React::Fade),
            "wobble" | "rotate" | "rotation" => Some(React::Wobble),
            _ => None,
        }
    }
    fn prop(self) -> Prop {
        match self {
            React::Throb => Prop::Scale,
            React::Shake => Prop::X,
            React::Glow => Prop::Glow,
            React::Blur => Prop::Blur,
            React::Fade => Prop::Opacity,
            React::Wobble => Prop::Rotation,
        }
    }
}

pub const ENV_RATE: f64 = 20.0;

/// The song's envelope (0..1) at [`ENV_RATE`] per second of source time, with a fast
/// attack and a slower release so it reads as "hits".
pub fn envelope(path: &Path, band: Band) -> Result<Vec<f32>> {
    let env = music::envelope(&music::decode_mono(path)?);
    let raw = match band {
        Band::Bass => env.bass,
        Band::Loudness => env.rms,
    };
    // Resample frames (≈ 43/s) to ENV_RATE with max-pooling.
    let step = env.fps / ENV_RATE;
    let n = (raw.len() as f64 / step) as usize;
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = ((i as f64 * step) as usize, (((i + 1) as f64 * step) as usize).min(raw.len()));
            raw[a..b.max(a + 1).min(raw.len())].iter().cloned().fold(0.0, f32::max)
        })
        .collect();
    let (attack, release) = (0.8f32, 0.25f32);
    let mut level = 0.0f32;
    for v in out.iter_mut() {
        let k = if *v > level { attack } else { release };
        level += (*v - level) * k;
        *v = level;
    }
    let max = out.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    out.iter_mut().for_each(|v| *v /= max);
    Ok(out)
}

/// Drops keyframes that linear interpolation already reproduces within `tol`.
pub fn simplify(keys: &[Keyframe], tol: f32) -> Vec<Keyframe> {
    if keys.len() <= 2 {
        return keys.to_vec();
    }
    let mut keep = vec![false; keys.len()];
    keep[0] = true;
    keep[keys.len() - 1] = true;
    fn rdp(keys: &[Keyframe], lo: usize, hi: usize, tol: f32, keep: &mut [bool]) {
        if hi <= lo + 1 {
            return;
        }
        let (a, b) = (keys[lo], keys[hi]);
        let mut worst = (lo, 0.0f32);
        for (i, k) in keys.iter().enumerate().take(hi).skip(lo + 1) {
            let f = ((k.time - a.time) / (b.time - a.time).max(1e-9)) as f32;
            let d = (k.value - (a.value + (b.value - a.value) * f)).abs();
            if d > worst.1 {
                worst = (i, d);
            }
        }
        if worst.1 > tol {
            keep[worst.0] = true;
            rdp(keys, lo, worst.0, tol, keep);
            rdp(keys, worst.0, hi, tol, keep);
        }
    }
    rdp(keys, 0, keys.len() - 1, tol, &mut keep);
    keys.iter().zip(keep).filter(|(_, k)| *k).map(|(k, _)| *k).collect()
}

/// Writes keyframes that follow `env` (song envelope over source time, mapped through
/// the song clip) onto each clip. Returns the number of keyframes written.
pub fn apply_reactive(project: &mut Project, clips: &[Id], song_clip: Id, env: &[f32], effect: React, amount: f32) -> usize {
    let Some(song) = project.clip(song_clip).cloned() else { return 0 };
    let amount = amount.clamp(0.05, 1.0);
    let mut written = 0;
    for &id in clips {
        let Some(c) = project.clip(id).cloned() else { continue };
        let base = effect.prop().base(&c);
        let mut keys = Vec::new();
        let steps = (c.duration * ENV_RATE).ceil() as usize;
        for i in 0..=steps {
            let rel = (i as f64 / ENV_RATE).min(c.duration);
            let t = c.start + rel;
            let src = song.source_in + (t - song.start) * song.speed;
            let v = if t < song.start || t > song.end() { 0.0 } else { env.get((src * ENV_RATE) as usize).copied().unwrap_or(0.0) };
            let value = match effect {
                React::Throb => base * (1.0 + 0.15 * amount * v),
                React::Shake => base + 0.03 * amount * v * if i % 2 == 0 { 1.0 } else { -1.0 },
                React::Glow => (base + 0.9 * amount * v).min(1.0),
                React::Blur => base + 18.0 * amount * v,
                React::Fade => base * (1.0 - 0.7 * amount * (1.0 - v)),
                React::Wobble => base + 4.0 * amount * v * if i % 2 == 0 { 1.0 } else { -1.0 },
            };
            keys.push(Keyframe { time: rel, value, ease: Ease::Linear });
        }
        let tol = match effect {
            React::Throb => 0.004 * base.abs().max(0.1),
            React::Shake => 0.002,
            React::Glow => 0.02,
            React::Blur => 0.4,
            React::Fade => 0.02,
            React::Wobble => 0.25,
        };
        // Shake/wobble alternate every sample; keep them all (they're the effect).
        let keys = if matches!(effect, React::Shake | React::Wobble) { simplify_pairs(&keys, base, tol) } else { simplify(&keys, tol) };
        written += keys.len();
        if let Some(clip) = project.clip_mut(id) {
            clip.keyframes.insert(effect.prop(), keys);
        }
    }
    written
}

/// For alternating effects: keep samples whose offset from `base` is noticeable, and
/// rest points between quiet stretches.
fn simplify_pairs(keys: &[Keyframe], base: f32, tol: f32) -> Vec<Keyframe> {
    let mut out: Vec<Keyframe> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        let loud = (k.value - base).abs() > tol;
        let edge = i == 0 || i == keys.len() - 1;
        if loud || edge {
            out.push(*k);
        } else if out.last().is_some_and(|l| (l.value - base).abs() > tol) {
            out.push(Keyframe { value: base, ..*k });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    #[test]
    fn ring_maps_bend_the_strip() {
        let s = 100u32;
        let (xm, ym) = ring_maps(s);
        let header = format!("P5\n{s} {s}\n65535\n").len();
        let at = |m: &[u8], x: u32, y: u32| { let i = header + ((y * s + x) * 2) as usize; u16::from_be_bytes([m[i], m[i + 1]]) };
        // Centre is off the ring.
        assert_eq!((at(&xm, 50, 50), at(&ym, 50, 50)), (RING_W as u16 - 1, 0));
        // Top of the ring = bass (x≈0); bottom = treble (x≈max); outer edge = full level (y small).
        assert!(at(&xm, 50, 3) < 10);
        assert!(at(&xm, 50, 97) > RING_W as u16 - 10);
        assert!(at(&ym, 50, 3) < at(&ym, 50, 25), "outer edge maps to tall bars");
        // Mirrored left/right.
        assert_eq!(at(&xm, 20, 50), at(&xm, 79, 50));
    }


    /// Manual visual check: `RUSTCUT_OUT=dir RUSTCUT_SONG=path cargo test --lib reactive_frames -- --ignored`
    #[test]
    #[ignore]
    fn reactive_frames() {
        let (Ok(dir), Ok(song)) = (std::env::var("RUSTCUT_OUT"), std::env::var("RUSTCUT_SONG")) else { return };
        let dir = Path::new(&dir);
        // Circular spectrum from the song.
        let mov = dir.join("circle.mov");
        render_visualizer(Path::new(&song), 90.0, 2.0, Visualizer::Circle, (480, 480), 24, [255, 60, 200], &mov).unwrap();
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        assert!(ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=0x181828:s=480x480:d=2", "-i", mov.to_str().unwrap(),
            "-filter_complex", "[0][1]overlay=format=auto", "-ss", "1", "-frames:v", "1", "-update", "1", dir.join("circle.png").to_str().unwrap()]));
        // Blur and glow through the export path.
        let img = dir.join("src.png");
        assert!(ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=s=640x360:d=1", "-frames:v", "1", img.to_str().unwrap()]));
        for (name, blur, glow) in [("plain", 0.0, 0.0), ("blur", 14.0, 0.0), ("glow", 0.0, 0.8)] {
            let mut p = Project::default();
            let m = p.add_media(ffmpeg::probe(&img).unwrap().into_media_item(img.clone()));
            let mut c = p.new_media_clip(m, 0.0).unwrap();
            c.duration = 2.0;
            c.blur = blur;
            c.glow = glow;
            p.append_to_main(c);
            std::fs::write(dir.join(format!("fx-{name}.png")), crate::export::render_frame_png(&p, 0.5, 360).unwrap()).unwrap();
        }
        // Animated blur (keyframes → per-frame commands).
        let mut p = Project::default();
        let m = p.add_media(ffmpeg::probe(&img).unwrap().into_media_item(img.clone()));
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = 2.0;
        c.keyframes.insert(Prop::Blur, vec![Keyframe { time: 0.0, value: 0.0, ease: Ease::Linear }, Keyframe { time: 1.0, value: 20.0, ease: Ease::Linear }]);
        c.keyframes.insert(Prop::Glow, vec![Keyframe { time: 0.0, value: 0.0, ease: Ease::Linear }, Keyframe { time: 1.0, value: 1.0, ease: Ease::Linear }]);
        p.append_to_main(c);
        std::fs::write(dir.join("fx-animated-0.1.png"), crate::export::render_frame_png(&p, 0.1, 360).unwrap()).unwrap();
        std::fs::write(dir.join("fx-animated-0.9.png"), crate::export::render_frame_png(&p, 0.9, 360).unwrap()).unwrap();
    }

    #[test]
    fn simplify_keeps_shape() {
        let keys: Vec<Keyframe> = (0..100).map(|i| Keyframe { time: i as f64 * 0.05, value: if (40..45).contains(&i) { 1.2 } else { 1.0 }, ease: Ease::Linear }).collect();
        let s = simplify(&keys, 0.01);
        assert!(s.len() <= 8, "{}", s.len());
        assert!(s.iter().any(|k| (k.value - 1.2).abs() < 1e-6));
    }

    #[test]
    fn reactive_keys_follow_the_envelope() {
        let mut p = Project::default();
        let song = p.add_media(MediaItem { id: 0, path: "s.mp3".into(), name: "s".into(), kind: MediaKind::Audio, duration: 10.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut sc = p.new_media_clip(song, 0.0).unwrap();
        sc.start = 1.0;
        let sid = sc.id;
        p.place_clip(sc, None);
        let img = p.add_media(MediaItem { id: 0, path: "i.png".into(), name: "i".into(), kind: MediaKind::Image, duration: 0.0, width: 100, height: 100, fps: 0.0, has_audio: false });
        let mut c = p.new_media_clip(img, 0.0).unwrap();
        c.duration = 6.0;
        let cid = c.id;
        p.append_to_main(c);
        // Loud only between song seconds 2 and 3 (timeline 3–4).
        let env: Vec<f32> = (0..200).map(|i| if (40..60).contains(&i) { 1.0 } else { 0.0 }).collect();
        let n = apply_reactive(&mut p, &[cid], sid, &env, React::Throb, 1.0);
        assert!(n >= 3 && n < 30, "{n}");
        let clip = p.clip(cid).unwrap();
        assert!((clip.value_at(Prop::Scale, 3.5) - 1.15).abs() < 1e-3);
        assert!((clip.value_at(Prop::Scale, 1.5) - 1.0).abs() < 1e-3);
        assert!((clip.value_at(Prop::Scale, 0.5) - 1.0).abs() < 1e-3, "before the song starts");
    }

    /// Real FFmpeg: each visualizer renders a transparent clip of the right length.
    #[test]
    fn visualizers_render_with_transparency() {
        let dir = std::env::temp_dir().join(format!("rc-viz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.wav");
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        if !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=f=220:d=6,aformat=channel_layouts=stereo", song.to_str().unwrap()]) {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        for kind in Visualizer::ALL {
            let out = dir.join(format!("{}.mov", kind.name()));
            let size = kind.size(640, 360);
            let info = render_visualizer(&song, 1.0, 3.0, kind, size, 24, [0, 205, 222], &out).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert!((info.duration - 3.0).abs() < 0.2, "{kind:?} {}", info.duration);
            assert_eq!((info.width, info.height), size);
            let rgba = ffmpeg::extract_frame(&out, 1.0, 64, 64 * size.1 / size.0).unwrap();
            let transparent = rgba.chunks_exact(4).filter(|p| p[3] < 10).count();
            assert!(transparent > rgba.len() / 4 / 4, "{kind:?}: background should be transparent ({transparent})");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
