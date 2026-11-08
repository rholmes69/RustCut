//! Reverse and speed-ramp a clip by rendering a new file (what CapCut does too):
//! the result is an ordinary video, so preview, export, undo and the MCP tools need
//! nothing special. Results are cached by source, range and settings.
//!
//! - Reverse works in one-second chunks (FFmpeg's `reverse` keeps every frame in
//!   memory), each reversed, then joined last-to-first.
//! - A ramp maps source time to output time with a piecewise-constant speed curve
//!   (`setpts`), scaled so the clip keeps its length on the timeline (beat sync stays
//!   intact). Ramped clips are silent; slow parts can use optical-flow frames.

use crate::media::ffmpeg;
use anyhow::{bail, Context, Result};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Speed-ramp shapes (relative speed across the clip; scaled to keep its length).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ramp {
    /// Fast – slow – fast.
    Montage,
    /// Normal, a slow-motion moment in the middle, normal.
    Hero,
    /// Very fast into extreme slow motion and back out.
    Bullet,
    /// Starts fast, settles to normal.
    FlashIn,
    /// Normal, then speeds away.
    FlashOut,
}

impl Ramp {
    pub const ALL: [Ramp; 5] = [Ramp::Montage, Ramp::Hero, Ramp::Bullet, Ramp::FlashIn, Ramp::FlashOut];

    pub fn label(self) -> &'static str {
        match self {
            Ramp::Montage => "Montage (fast · slow · fast)",
            Ramp::Hero => "Hero moment (slow-mo in the middle)",
            Ramp::Bullet => "Bullet time (extreme slow-mo)",
            Ramp::FlashIn => "Flash in (fast → normal)",
            Ramp::FlashOut => "Flash out (normal → fast)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ramp::Montage => "montage",
            Ramp::Hero => "hero",
            Ramp::Bullet => "bullet",
            Ramp::FlashIn => "flash_in",
            Ramp::FlashOut => "flash_out",
        }
    }

    pub fn parse(s: &str) -> Option<Ramp> {
        let s = s.to_ascii_lowercase().replace([' ', '-'], "_");
        Ramp::ALL.into_iter().find(|r| r.name() == s || s.starts_with(r.name()))
    }

    /// Relative speed at `u` (0..1 through the source).
    pub fn shape(self, u: f64) -> f64 {
        let bump = |c: f64, w: f64| (-((u - c) / w).powi(2)).exp();
        match self {
            Ramp::Montage => 2.4 - 1.95 * bump(0.5, 0.18),
            Ramp::Hero => 1.0 - 0.7 * bump(0.5, 0.12),
            Ramp::Bullet => 3.0 - 2.88 * bump(0.5, 0.1),
            Ramp::FlashIn => 1.0 + 2.5 * (1.0 - u / 0.35).max(0.0).powi(2),
            Ramp::FlashOut => 1.0 + 2.5 * ((u - 0.65) / 0.35).max(0.0).powi(2),
        }
    }
}

/// Source segments of a ramp: `(seg_len, speed)` with the speeds scaled so the
/// output lasts `out_len` seconds.
pub fn ramp_segments(ramp: Ramp, span: f64, out_len: f64, n: usize) -> Vec<(f64, f64)> {
    let seg = span / n as f64;
    let shape: Vec<f64> = (0..n).map(|i| ramp.shape((i as f64 + 0.5) / n as f64).max(0.05)).collect();
    // out_len = Σ seg / (k·shape)  →  k = seg·Σ(1/shape) / out_len
    let k = seg * shape.iter().map(|s| 1.0 / s).sum::<f64>() / out_len.max(0.05);
    shape.into_iter().map(|s| (seg, s * k)).collect()
}

/// `setpts` expression (in seconds, of input time `T`) for the segments.
fn ramp_expr(segs: &[(f64, f64)]) -> String {
    let (mut t, mut o) = (0.0, 0.0);
    let mut parts = Vec::new();
    for (len, speed) in segs {
        parts.push((t, o, *speed));
        t += len;
        o += len / speed;
    }
    // Nested if(): the last segment is the fallback.
    let mut expr = {
        let (t0, o0, s) = parts.last().unwrap();
        format!("({o0:.6}+(T-{t0:.6})/{s:.6})")
    };
    for (i, (t0, o0, s)) in parts.iter().enumerate().rev().skip(1) {
        let t1 = parts[i + 1].0;
        expr = format!("if(lt(T,{t1:.6}),({o0:.6}+(T-{t0:.6})/{s:.6}),{expr})");
    }
    expr
}

fn cache_name(src: &Path, what: &str, a: f64, span: f64, extra: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    std::fs::metadata(src).ok().and_then(|m| m.modified().ok()).hash(&mut h);
    format!("{:.3}-{:.3}-{extra}", a, span).hash(&mut h);
    let stem = src.file_stem().map(|s| s.to_string_lossy().chars().take(40).collect::<String>()).unwrap_or_default();
    format!("{stem}-{what}-{:08x}.mp4", h.finish() as u32)
}

fn encode_args() -> Vec<String> {
    ["-c:v", "libx264", "-crf", "16", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-movflags", "+faststart"].map(String::from).to_vec()
}

/// Reverses `src[a .. a+span]` into `dir`. Returns the file (cached) or `None` if cancelled.
pub fn reverse(src: &Path, a: f64, span: f64, with_audio: bool, dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let out = dir.join(cache_name(src, "reversed", a, span, &with_audio.to_string()));
    if out.is_file() {
        return Ok(Some(out));
    }
    let work = ffmpeg::work_dir("reverse")?;
    let result = (|| -> Result<Option<PathBuf>> {
        const CHUNK: f64 = 1.0;
        let n = (span / CHUNK).ceil().max(1.0) as usize;
        let mut parts = Vec::new();
        for i in 0..n {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            progress(i as f32 / n as f32 * 0.95, &format!("Reversing second {} of {n}…", i + 1));
            let start = a + i as f64 * CHUNK;
            let len = CHUNK.min(a + span - start);
            if len < 0.02 {
                break;
            }
            let part = work.join(format!("p{i:05}.mp4"));
            let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-ss"].map(String::from).to_vec();
            args.extend([format!("{start:.4}"), "-t".into(), format!("{len:.4}"), "-i".into(), src.to_string_lossy().into_owned()]);
            args.extend(["-vf".into(), "reverse".into()]);
            if with_audio {
                args.extend(["-af".into(), "areverse".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
            } else {
                args.push("-an".into());
            }
            args.extend(encode_args());
            args.push(part.to_string_lossy().into_owned());
            let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(&args).status().context("could not run ffmpeg")?.success();
            if !ok {
                bail!("FFmpeg couldn't reverse {start:.1}–{:.1} s of {}", start + len, src.display());
            }
            parts.push(part);
        }
        // Join last-to-first.
        let list = work.join("list.txt");
        let body: String = parts.iter().rev().map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\\', "/").replace('\'', "'\\''"))).collect();
        std::fs::write(&list, body)?;
        progress(0.97, "Joining…");
        let tmp = out.with_extension("part.mp4");
        let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&tmp)
            .status()
            .context("could not run ffmpeg")?
            .success();
        if !ok {
            bail!("FFmpeg couldn't join the reversed pieces");
        }
        std::fs::rename(&tmp, &out)?;
        Ok(Some(out))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Speed-ramps `src[a .. a+span]` into a silent clip lasting `out_len` seconds.
/// `smooth` = optical-flow in-between frames for the slow parts (much slower to render).
#[allow(clippy::too_many_arguments)]
pub fn ramp(src: &Path, a: f64, span: f64, out_len: f64, ramp: Ramp, fps: u32, smooth: bool, dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let out = dir.join(cache_name(src, ramp.name(), a, span, &format!("{out_len:.3}-{fps}-{smooth}")));
    if out.is_file() {
        return Ok(Some(out));
    }
    let segs = ramp_segments(ramp, span, out_len, 24);
    let mut vf = format!("setpts=PTS-STARTPTS,setpts='({})/TB'", ramp_expr(&segs));
    if smooth {
        vf.push_str(&format!(",minterpolate=fps={fps}:mi_mode=mci:mc_mode=aobmc:vsbmc=1"));
    } else {
        vf.push_str(&format!(",fps={fps}"));
    }
    let tmp = out.with_extension("part.mp4");
    let mut args: Vec<String> = ["-hide_banner", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-ss"].map(String::from).to_vec();
    args.extend([format!("{a:.4}"), "-t".into(), format!("{span:.4}"), "-i".into(), src.to_string_lossy().into_owned()]);
    args.extend(["-vf".into(), vf, "-an".into(), "-t".into(), format!("{out_len:.4}")]);
    args.extend(encode_args());
    args.push(tmp.to_string_lossy().into_owned());
    progress(0.0, "Rendering the speed ramp…");
    if !ffmpeg::run_with_progress(&args, out_len, cancel, |p| progress(p, "Rendering the speed ramp…"))? {
        let _ = std::fs::remove_file(&tmp);
        return Ok(None);
    }
    std::fs::rename(&tmp, &out)?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_keep_the_clip_length() {
        for r in Ramp::ALL {
            let segs = ramp_segments(r, 6.0, 4.0, 24);
            let out: f64 = segs.iter().map(|(l, s)| l / s).sum();
            let src: f64 = segs.iter().map(|(l, _)| l).sum();
            assert!((out - 4.0).abs() < 1e-9 && (src - 6.0).abs() < 1e-9, "{r:?}");
        }
        let hero = ramp_segments(Ramp::Hero, 4.0, 4.0, 24);
        assert!(hero[12].1 < 0.5 && hero[0].1 > 1.0, "slow in the middle, faster at the ends");
        assert_eq!(Ramp::parse("Bullet time"), Some(Ramp::Bullet));
        assert_eq!(Ramp::parse("flash-in"), Some(Ramp::FlashIn));
    }

    #[test]
    fn ramp_expression_is_continuous() {
        let segs = vec![(1.0, 2.0), (1.0, 0.5), (1.0, 1.0)];
        let e = ramp_expr(&segs);
        assert!(e.starts_with("if(lt(T,1.000000),(0.000000+(T-0.000000)/2.000000),if(lt(T,2.000000),(0.500000+"), "{e}");
        assert!(e.contains("(2.500000+(T-2.000000)/1.000000)"), "{e}");
    }

    /// Reverse and ramp a generated clip and check the files (needs FFmpeg).
    #[test]
    fn renders_reverse_and_ramp() {
        let dir = std::env::temp_dir().join("rustcut-retime-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let ok = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24:duration=3", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let cancel = AtomicBool::new(false);
        let rev = reverse(&src, 0.5, 2.0, false, &dir, &cancel, &mut |_, _| {}).unwrap().unwrap();
        let info = ffmpeg::probe(&rev).unwrap();
        assert!((info.duration - 2.0).abs() < 0.15, "reversed length {}", info.duration);
        // The first frame of the reversed clip is the last frame of the range.
        let first = ffmpeg::extract_frame(&rev, 0.0, 64, 36).unwrap();
        let last = ffmpeg::extract_frame(&src, 2.45, 64, 36).unwrap();
        let start = ffmpeg::extract_frame(&src, 0.5, 64, 36).unwrap();
        let d = |x: &[u8], y: &[u8]| x.iter().zip(y).map(|(a, b)| (*a as i32 - *b as i32).abs() as u64).sum::<u64>();
        assert!(d(&first, &last) < d(&first, &start), "reversed clip should start where the range ends");
        let r = ramp(&src, 0.0, 3.0, 2.5, Ramp::Hero, 24, false, &dir, &cancel, &mut |_, _| {}).unwrap().unwrap();
        let info = ffmpeg::probe(&r).unwrap();
        assert!((info.duration - 2.5).abs() < 0.15, "ramped length {}", info.duration);
    }

    /// Manual: `P4_PROJECT=song.rcproj P4_OUT=dir P4_T=200 cargo test --lib phase4_live -- --ignored --nocapture`
    /// reverses and Hero-ramps the main-track shot at P4_T, makes its proxy and renders a 9:16 frame (nothing is saved).
    #[test]
    #[ignore]
    fn phase4_live() {
        let Ok(path) = std::env::var("P4_PROJECT") else { return };
        let out = PathBuf::from(std::env::var("P4_OUT").unwrap());
        let t: f64 = std::env::var("P4_T").ok().and_then(|v| v.parse().ok()).unwrap_or(200.0);
        let p = crate::model::Project::load(Path::new(&path)).unwrap();
        let main = p.main_track_id().unwrap();
        let c = p.tracks.iter().find(|x| x.id == main).unwrap().clips.iter().find(|c| c.is_active(t)).unwrap().clone();
        let m = p.media(c.media_id().unwrap()).unwrap().clone();
        let span = c.duration * c.speed;
        println!("clip {} · {} · {}x{} · source {:.2}+{:.2}s", c.id, m.name, m.width, m.height, c.source_in, span);
        let cancel = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        let rev = reverse(&m.path, c.source_in, span, false, &out, &cancel, &mut |_, _| {}).unwrap().unwrap();
        println!("reverse: {:.1}s → {} ({:.2}s)", t0.elapsed().as_secs_f64(), rev.display(), ffmpeg::probe(&rev).unwrap().duration);
        let t0 = std::time::Instant::now();
        let r = ramp(&m.path, c.source_in, span, c.duration, Ramp::Hero, 30, false, &out, &cancel, &mut |_, _| {}).unwrap().unwrap();
        println!("hero ramp: {:.1}s → {:.2}s long (clip is {:.2}s)", t0.elapsed().as_secs_f64(), ffmpeg::probe(&r).unwrap().duration, c.duration);
        let t0 = std::time::Instant::now();
        let mut mm = m.clone();
        mm.path = m.path.clone();
        if crate::media::proxy::wanted(&mm) {
            crate::media::proxy::make(&mm, &cancel, |_| {}).unwrap();
            let pp = crate::media::proxy::path_for(&m.path);
            println!("proxy: {:.1}s → {} KB vs original {} KB", t0.elapsed().as_secs_f64(), std::fs::metadata(&pp).unwrap().len() / 1024, std::fs::metadata(&m.path).unwrap().len() / 1024);
        } else {
            println!("no proxy needed for {}x{}", m.width, m.height);
        }
        let v = crate::ui::export_queue::reshaped(&p, crate::model::Aspect::Portrait);
        std::fs::write(out.join("portrait.png"), crate::export::render_frame_png(&v, t, 480).unwrap()).unwrap();
        std::fs::write(out.join("landscape.png"), crate::export::render_frame_png(&p, t, 480).unwrap()).unwrap();
    }
}
