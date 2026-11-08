//! Colour grading on top of a clip's filter and adjustments: an optional 3D LUT
//! (`.cube`, blended by a strength) followed by per-channel gain and offset (set by
//! hand or by colour match). The preview applies it on the CPU; export bakes the
//! whole grade into one `.cube` for FFmpeg's `lut3d`, so both use the same numbers.

use crate::model::Grade;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Size of the cube baked for export.
const BAKE_SIZE: usize = 33;

pub struct Lut {
    size: usize,
    /// Red changes fastest, then green, then blue (the `.cube` order).
    data: Vec<[f32; 3]>,
}

impl Lut {
    /// Parses an Adobe/Resolve `.cube` 3D LUT.
    pub fn parse(text: &str) -> Result<Self> {
        let (mut size, mut lo, mut hi) = (0usize, [0.0f32; 3], [1.0f32; 3]);
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let first = parts.next().unwrap_or("");
            match first.to_ascii_uppercase().as_str() {
                "TITLE" => {}
                "LUT_1D_SIZE" => bail!("1D LUTs aren't supported — use a 3D .cube LUT"),
                "LUT_3D_SIZE" => size = parts.next().and_then(|s| s.parse().ok()).context("bad LUT_3D_SIZE")?,
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let v: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
                    if v.len() == 3 {
                        let t = if first.eq_ignore_ascii_case("DOMAIN_MIN") { &mut lo } else { &mut hi };
                        *t = [v[0], v[1], v[2]];
                    }
                }
                _ => {
                    let Ok(r) = first.parse::<f32>() else { continue };
                    let g: f32 = parts.next().and_then(|s| s.parse().ok()).context("bad LUT row")?;
                    let b: f32 = parts.next().and_then(|s| s.parse().ok()).context("bad LUT row")?;
                    data.push([r, g, b]);
                }
            }
        }
        if !(2..=256).contains(&size) {
            bail!("not a 3D .cube LUT (no LUT_3D_SIZE)");
        }
        if data.len() != size * size * size {
            bail!("the LUT should have {} rows but has {}", size * size * size, data.len());
        }
        // Outputs are normalised to 0..1 using the domain.
        if lo != [0.0; 3] || hi != [1.0; 3] {
            for c in &mut data {
                for i in 0..3 {
                    c[i] = (c[i] - lo[i]) / (hi[i] - lo[i]).max(1e-6);
                }
            }
        }
        Ok(Self { size, data })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("{}", path.display()))
    }

    /// Trilinear lookup of a 0..1 colour.
    pub fn sample(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.size - 1;
        let pos = c.map(|v| v.clamp(0.0, 1.0) * n as f32);
        let i0 = pos.map(|p| (p.floor() as usize).min(n));
        let i1 = i0.map(|i| (i + 1).min(n));
        let f = [pos[0] - i0[0] as f32, pos[1] - i0[1] as f32, pos[2] - i0[2] as f32];
        let at = |r: usize, g: usize, b: usize| self.data[r + self.size * (g + self.size * b)];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        let c00 = lerp(at(i0[0], i0[1], i0[2]), at(i1[0], i0[1], i0[2]), f[0]);
        let c10 = lerp(at(i0[0], i1[1], i0[2]), at(i1[0], i1[1], i0[2]), f[0]);
        let c01 = lerp(at(i0[0], i0[1], i1[2]), at(i1[0], i0[1], i1[2]), f[0]);
        let c11 = lerp(at(i0[0], i1[1], i1[2]), at(i1[0], i1[1], i1[2]), f[0]);
        lerp(lerp(c00, c10, f[1]), lerp(c01, c11, f[1]), f[2])
    }
}

/// LUTs are parsed once and shared (the preview applies them every frame).
pub fn cached(path: &Path) -> Result<Arc<Lut>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Lut>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(l) = cache.lock().unwrap().get(path) {
        return Ok(l.clone());
    }
    let lut = Arc::new(Lut::load(path)?);
    cache.lock().unwrap().insert(path.to_path_buf(), lut.clone());
    Ok(lut)
}

/// The grade applied to one 0..1 colour.
pub fn apply(c: [f32; 3], g: &Grade, lut: Option<&Lut>) -> [f32; 3] {
    let mut c = c;
    if let Some(l) = lut {
        let m = g.lut_mix.clamp(0.0, 1.0);
        let o = l.sample(c);
        c = [c[0] + (o[0] - c[0]) * m, c[1] + (o[1] - c[1]) * m, c[2] + (o[2] - c[2]) * m];
    }
    [0, 1, 2].map(|i| (c[i] * g.gain[i] + g.offset[i]).clamp(0.0, 1.0))
}

fn lut_for(g: &Grade) -> Option<Arc<Lut>> {
    g.lut.as_deref().and_then(|p| cached(p).ok())
}

/// Applies the grade to an RGBA buffer in place (after the filter and adjustments).
pub fn apply_cpu(rgba: &mut [u8], g: &Grade) {
    if g.is_identity() {
        return;
    }
    let lut = lut_for(g);
    // A 256-entry table per channel is enough when there's no LUT (gain/offset only).
    if lut.is_none() {
        let table: [[u8; 256]; 3] = [0, 1, 2].map(|i| std::array::from_fn(|v| ((v as f32 / 255.0 * g.gain[i] + g.offset[i]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
        for px in rgba.chunks_exact_mut(4) {
            for i in 0..3 {
                px[i] = table[i][px[i] as usize];
            }
        }
        return;
    }
    for px in rgba.chunks_exact_mut(4) {
        let c = apply([px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0], g, lut.as_deref());
        for i in 0..3 {
            px[i] = (c[i] * 255.0 + 0.5) as u8;
        }
    }
}

/// Writes the whole grade as a `.cube` file.
pub fn bake_cube(g: &Grade, path: &Path) -> Result<()> {
    let lut = match &g.lut {
        Some(p) => Some(cached(p)?),
        None => None,
    };
    let n = BAKE_SIZE;
    let mut out = String::with_capacity(n * n * n * 28 + 64);
    out.push_str(&format!("TITLE \"RustCut grade\"\nLUT_3D_SIZE {n}\n"));
    let step = 1.0 / (n - 1) as f32;
    for b in 0..n {
        for gg in 0..n {
            for r in 0..n {
                let c = apply([r as f32 * step, gg as f32 * step, b as f32 * step], g, lut.as_deref());
                out.push_str(&format!("{:.6} {:.6} {:.6}\n", c[0], c[1], c[2]));
            }
        }
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

/// The FFmpeg filter for a grade (`None` = nothing to do). Bakes a cube into `work_dir`.
pub fn ffmpeg_filter(g: &Grade, work_dir: &Path, tag: &str) -> Result<Option<String>> {
    if g.is_identity() {
        return Ok(None);
    }
    let cube = work_dir.join(format!("grade-{tag}.cube"));
    bake_cube(g, &cube)?;
    Ok(Some(format!("lut3d=file='{}':interp=trilinear", crate::media::ffmpeg::filter_path(&cube))))
}

// ── built-in looks ──────────────────────────────────────────────────────────

fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// Gentle S-curve (contrast) of strength `k`.
fn s_curve(v: f32, k: f32) -> f32 {
    let s = v * v * (3.0 - 2.0 * v);
    v + (s - v) * k
}

fn saturate(c: [f32; 3], s: f32) -> [f32; 3] {
    let l = luma(c);
    c.map(|v| l + (v - l) * s)
}

type Look = fn([f32; 3]) -> [f32; 3];

/// Looks shipped as generated `.cube` files (name, description, colour function).
pub const BUILTIN: &[(&str, &str, Look)] = &[
    ("Teal & Orange", "Cinematic: teal shadows, warm skin and highlights", |c| {
        let l = luma(c);
        let c = [c[0] + 0.10 * l - 0.06 * (1.0 - l), c[1] + 0.02 * l + 0.03 * (1.0 - l), c[2] - 0.08 * l + 0.08 * (1.0 - l)];
        saturate(c.map(|v| s_curve(v.clamp(0.0, 1.0), 0.35)), 1.08)
    }),
    ("Golden Hour", "Warm, soft sunset light", |c| {
        let c = [c[0] * 1.08 + 0.02, c[1] * 1.0 + 0.01, c[2] * 0.85];
        saturate(c.map(|v| s_curve(v.clamp(0.0, 1.0), 0.15)), 1.05)
    }),
    ("Moody Blue", "Cold, desaturated night look with deep blacks", |c| {
        let c = saturate(c, 0.72);
        [c[0] * 0.9, c[1] * 0.97, c[2] * 1.1 + 0.02].map(|v| s_curve((v - 0.03).clamp(0.0, 1.0), 0.4))
    }),
    ("Bleach Bypass", "Gritty: low saturation, hard contrast", |c| {
        let l = luma(c);
        let c = [0, 1, 2].map(|i| c[i] * 0.55 + l * 0.45);
        c.map(|v| s_curve(v, 0.7))
    }),
    ("Faded Film", "Lifted blacks, soft highlights, slightly warm", |c| {
        let c = saturate(c, 0.85);
        [c[0] * 0.9 + 0.08, c[1] * 0.88 + 0.07, c[2] * 0.84 + 0.06]
    }),
    ("Noir", "High-contrast black and white", |c| {
        let l = s_curve(luma(c), 0.8);
        [l, l, l]
    }),
];

/// The `.cube` file for a built-in look, written on first use.
pub fn builtin_path(name: &str) -> Result<PathBuf> {
    let (_, _, f) = BUILTIN.iter().find(|b| b.0 == name).with_context(|| format!("no built-in look `{name}`"))?;
    let dir = crate::util::config_dir().join("luts");
    let path = dir.join(format!("{}.cube", name.replace(['&', ' '], "_")));
    if !path.is_file() {
        std::fs::create_dir_all(&dir)?;
        let n = BAKE_SIZE;
        let step = 1.0 / (n - 1) as f32;
        let mut out = format!("TITLE \"{name}\"\nLUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let c = f([r as f32 * step, g as f32 * step, b as f32 * step]).map(|v| v.clamp(0.0, 1.0));
                    out.push_str(&format!("{:.6} {:.6} {:.6}\n", c[0], c[1], c[2]));
                }
            }
        }
        std::fs::write(&path, out)?;
    }
    Ok(path)
}

/// The built-in look a LUT path belongs to, if any.
pub fn builtin_name(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    BUILTIN.iter().map(|b| b.0).find(|n| n.replace(['&', ' '], "_") == stem && path.parent().is_some_and(|d| d.ends_with("luts")))
}

/// Per-channel mean and standard deviation of an RGBA frame (0..1), skipping
/// near-black and blown-out pixels (letterbox bars, clipped highlights).
pub fn stats(rgba: &[u8]) -> Option<([f32; 3], [f32; 3])> {
    let (mut sum, mut sq, mut n) = ([0f64; 3], [0f64; 3], 0usize);
    for px in rgba.chunks_exact(4) {
        let c = [px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0];
        let l = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        if !(0.02..=0.98).contains(&l) {
            continue;
        }
        for i in 0..3 {
            sum[i] += c[i];
            sq[i] += c[i] * c[i];
        }
        n += 1;
    }
    if n < 50 {
        return None;
    }
    let mean = sum.map(|s| s / n as f64);
    let sd = [0, 1, 2].map(|i| (sq[i] / n as f64 - mean[i] * mean[i]).max(0.0).sqrt());
    Some((mean.map(|v| v as f32), sd.map(|v| v as f32)))
}

/// Gain and offset that move `src`'s colour statistics toward `reference`'s
/// (mean/variance transfer per channel). `strength` 0..1 blends from no change;
/// extreme corrections are limited so a bad reference can't wreck a shot.
pub fn match_colour(src: &[u8], reference: &[u8], strength: f32) -> Option<([f32; 3], [f32; 3])> {
    let (ms, ss) = stats(src)?;
    let (mr, sr) = stats(reference)?;
    let s = strength.clamp(0.0, 1.0);
    let mut gain = [1.0; 3];
    let mut offset = [0.0; 3];
    for i in 0..3 {
        let g = if ss[i] > 0.01 { (sr[i] / ss[i]).clamp(0.6, 1.6) } else { 1.0 };
        let o = (mr[i] - g * ms[i]).clamp(-0.25, 0.25);
        gain[i] = 1.0 + (g - 1.0) * s;
        offset[i] = o * s;
    }
    Some((gain, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_cube(n: usize) -> String {
        let mut s = format!("LUT_3D_SIZE {n}\n");
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let f = |v: usize| v as f32 / (n - 1) as f32;
                    s.push_str(&format!("{} {} {}\n", f(r), f(g), f(b)));
                }
            }
        }
        s
    }

    #[test]
    fn parses_and_samples_a_cube() {
        let lut = Lut::parse(&identity_cube(5)).unwrap();
        let c = lut.sample([0.3, 0.6, 0.9]);
        assert!((c[0] - 0.3).abs() < 1e-5 && (c[1] - 0.6).abs() < 1e-5 && (c[2] - 0.9).abs() < 1e-5);
        assert!(Lut::parse("LUT_1D_SIZE 4\n0 0 0").is_err());
        assert!(Lut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn baked_cube_matches_the_cpu_grade() {
        let dir = std::env::temp_dir().join("rustcut-grade-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("id.cube");
        std::fs::write(&src, identity_cube(9)).unwrap();
        let g = Grade { lut: Some(src), lut_mix: 0.5, gain: [1.2, 1.0, 0.8], offset: [0.0, 0.05, 0.0] };
        let baked = dir.join("baked.cube");
        bake_cube(&g, &baked).unwrap();
        let b = Lut::load(&baked).unwrap();
        let lut = cached(g.lut.as_ref().unwrap()).unwrap();
        let c = [0.5, 0.25, 0.75];
        let (x, y) = (apply(c, &g, Some(&lut)), b.sample(c));
        for i in 0..3 {
            assert!((x[i] - y[i]).abs() < 0.01, "{x:?} vs {y:?}");
        }
        let mut px = vec![128, 128, 128, 255];
        apply_cpu(&mut px, &Grade { gain: [2.0, 1.0, 1.0], ..Default::default() });
        assert_eq!(px, vec![255, 128, 128, 255]);
    }

    /// The preview (CPU) and the export (FFmpeg lut3d on the baked cube) agree.
    #[test]
    fn export_matches_preview() {
        use crate::model::{MediaItem, MediaKind, Project};
        let dir = std::env::temp_dir().join("rustcut-grade-parity");
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("bars.png");
        let ok = crate::media::ffmpeg::command(&crate::media::ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "smptehdbars=size=320x180", "-frames:v", "1"])
            .arg(&img)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("ffmpeg not available; skipped");
            return;
        }
        let mut p = Project { aspect: crate::model::Aspect::Landscape, ..Default::default() };
        let m = p.add_media(MediaItem { id: 0, path: img.clone(), name: "bars".into(), kind: MediaKind::Image, duration: 0.0, width: 320, height: 180, fps: 0.0, has_audio: false });
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = 2.0;
        c.grade = Grade { lut: Some(builtin_path("Teal & Orange").unwrap()), lut_mix: 0.8, gain: [1.05, 1.0, 0.95], offset: [0.0, 0.01, 0.0] };
        let grade = c.grade.clone();
        let main = p.main_track();
        p.tracks[main].clips.push(c);
        let (w, h, exported) = crate::export::render_frame_rgba(&p, 1.0, 180).unwrap();
        assert_eq!((w, h), (320, 180));
        let mut preview = crate::media::ffmpeg::extract_frame(&img, 0.0, w, h).unwrap();
        apply_cpu(&mut preview, &grade);
        let diff: f64 = exported.iter().zip(&preview).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (*a as f64 - *b as f64).abs()).sum::<f64>() / (w * h * 3) as f64;
        let plain = crate::media::ffmpeg::extract_frame(&img, 0.0, w, h).unwrap();
        let change: f64 = plain.iter().zip(&preview).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (*a as f64 - *b as f64).abs()).sum::<f64>() / (w * h * 3) as f64;
        println!("mean |export - preview| = {diff:.2} levels (the grade itself changes pixels by {change:.1})");
        assert!(change > 5.0, "the grade should visibly change the picture");
        assert!(diff < 4.0, "export and preview differ by {diff:.2} levels on average");
    }

    #[test]
    fn colour_match_moves_toward_the_reference() {
        let frame = |r: u8, g: u8, b: u8| -> Vec<u8> { (0..400).flat_map(|i| [r.saturating_add((i % 40) as u8), g.saturating_add((i % 40) as u8), b, 255]).collect() };
        let warm = frame(180, 120, 80);
        let cool = frame(90, 120, 170);
        let (gain, offset) = match_colour(&cool, &warm, 1.0).unwrap();
        let g = Grade { gain, offset, ..Default::default() };
        let mut out = cool.clone();
        apply_cpu(&mut out, &g);
        let (m_out, _) = stats(&out).unwrap();
        let (m_ref, _) = stats(&warm).unwrap();
        let (m_src, _) = stats(&cool).unwrap();
        // Every channel moves toward the reference; big shifts stop at the safety cap.
        for i in 0..3 {
            assert!((m_out[i] - m_ref[i]).abs() <= (m_src[i] - m_ref[i]).abs() + 1e-3, "channel {i}: {m_out:?} vs {m_ref:?}");
        }
        assert!(m_out[0] - m_src[0] > 0.2 && m_src[2] - m_out[2] > 0.2, "{m_src:?} -> {m_out:?}");
        assert!(match_colour(&cool, &warm, 0.0).unwrap().0 == [1.0; 3]);
    }

    /// Manual: `GRADE_PROJECT=song.rcproj GRADE_OUT=dir GRADE_REF=60 GRADE_TARGET=120 cargo test --lib grade_live -- --ignored --nocapture`
    /// renders a shot plain, with Teal & Orange, and colour-matched to the shot at GRADE_REF (nothing is saved).
    #[test]
    #[ignore]
    fn grade_live() {
        let Ok(path) = std::env::var("GRADE_PROJECT") else { return };
        let out = PathBuf::from(std::env::var("GRADE_OUT").unwrap());
        let num = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (tr, tt) = (num("GRADE_REF", 60.0), num("GRADE_TARGET", 120.0));
        let p = crate::model::Project::load(Path::new(&path)).unwrap();
        let main = p.main_track_id().unwrap();
        let at = |p: &crate::model::Project, t: f64| p.tracks.iter().find(|x| x.id == main).unwrap().clips.iter().find(|c| c.is_active(t)).unwrap().id;
        let (rid, tid) = (at(&p, tr), at(&p, tt));
        let png = |p: &crate::model::Project, t: f64, name: &str| std::fs::write(out.join(name), crate::export::render_frame_png(p, t, 360).unwrap()).unwrap();
        png(&p, tr, "1-reference.png");
        png(&p, tt, "2-target-before.png");
        let small = |p: &crate::model::Project, id| {
            let c = p.clip(id).unwrap();
            let m = p.media(c.media_id().unwrap()).unwrap();
            let (w, h) = crate::media::ffmpeg::fit_within(m.width, m.height, 192, 192);
            let mut px = crate::media::ffmpeg::extract_frame(&m.path, c.source_time(c.start + c.duration / 2.0), w, h).unwrap();
            crate::render::filters::apply_cpu(&mut px, c.filter, &c.adjust);
            px
        };
        let (gain, offset) = match_colour(&small(&p, tid), &small(&p, rid), 0.8).unwrap();
        println!("reference clip {rid} at {tr}s, target clip {tid} at {tt}s → gain {gain:.3?} offset {offset:.3?}");
        let mut m = p.clone();
        m.clip_mut(tid).unwrap().grade = Grade { gain, offset, ..Default::default() };
        png(&m, tt, "3-target-matched.png");
        let mut l = p.clone();
        l.clip_mut(tid).unwrap().grade.lut = Some(builtin_path("Teal & Orange").unwrap());
        png(&l, tt, "4-target-teal-orange.png");
        println!("wrote {}", out.display());
    }
}
