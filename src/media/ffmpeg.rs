//! Thin wrappers around the `ffmpeg` / `ffprobe` command-line tools.

use crate::model::{MediaItem, MediaKind};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::process::{Command, Stdio};

/// Waveform resolution produced by [`waveform`].
pub const PEAKS_PER_SEC: f64 = 100.0;

pub const MEDIA_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "gif", "mpg", "mpeg", "wmv", "flv", "png", "jpg",
    "jpeg", "bmp", "webp", "tif", "tiff", "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma",
];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff"];

pub fn ffmpeg_bin() -> String {
    std::env::var("RUSTCUT_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

pub fn ffprobe_bin() -> String {
    std::env::var("RUSTCUT_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string())
}

/// A `Command` with stdin closed and (on Windows) no console window popping up.
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Escapes a path for use inside a quoted filter option (`opt='…'`).
pub fn filter_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:").replace('\'', "")
}

/// A fresh, unique scratch directory under the system temp dir.
pub fn work_dir(tag: &str) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rustcut-{tag}-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Runs `ffmpeg args…` (which must include `-progress pipe:1`), reporting progress
/// (0..1) against `total` seconds of output. Returns `Ok(false)` if cancelled.
pub fn run_with_progress(
    args: &[String],
    total: f64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<bool> {
    let mut child = command(&ffmpeg_bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ffmpeg — is it installed and on PATH?")?;
    let mut stderr = child.stderr.take().context("no stderr")?;
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stdout = child.stdout.take().context("no stdout")?;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        let value = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="));
        if let Some(us) = value.and_then(|v| v.trim().parse::<f64>().ok()) {
            if total > 0.0 {
                on_progress(((us / 1e6) / total).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let status = child.wait()?;
    let err = err_thread.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    if !status.success() {
        let tail: String = err.trim().chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("FFmpeg failed ({status}):\n{tail}");
    }
    Ok(true)
}

pub fn is_available() -> bool {
    command(&ffmpeg_bin())
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub kind: MediaKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
}

pub fn probe(path: &Path) -> Result<ProbeInfo> {
    let out = command(&ffprobe_bin())
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run ffprobe — is FFmpeg installed and on PATH?")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: Value = serde_json::from_slice(&out.stdout).context("unreadable ffprobe output")?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0
    });
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let num = |v: &Value| -> Option<f64> {
        v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    };
    let duration = num(&v["format"]["duration"])
        .or_else(|| video.and_then(|s| num(&s["duration"])))
        .unwrap_or(0.0);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let Some(video) = video else {
        if has_audio {
            return Ok(ProbeInfo { kind: MediaKind::Audio, duration, width: 0, height: 0, fps: 0.0, has_audio });
        }
        bail!("no audio or video streams found");
    };

    let mut width = video["width"].as_u64().unwrap_or(0) as u32;
    let mut height = video["height"].as_u64().unwrap_or(0) as u32;
    let rotation = video["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()))
        .or_else(|| num(&video["tags"]["rotate"]))
        .unwrap_or(0.0);
    if (rotation.abs().round() as i64) % 180 == 90 {
        std::mem::swap(&mut width, &mut height);
    }
    let fps = video["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .or_else(|| video["r_frame_rate"].as_str().and_then(parse_rate))
        .unwrap_or(30.0);
    let is_image = IMAGE_EXTS.contains(&ext.as_str()) || (duration <= 0.0 && !has_audio);
    let kind = if is_image { MediaKind::Image } else { MediaKind::Video };
    Ok(ProbeInfo {
        kind,
        duration: if is_image { 0.0 } else { duration },
        width,
        height,
        fps,
        has_audio: has_audio && !is_image,
    })
}

impl ProbeInfo {
    /// A media item (id 0, assigned by `Project::add_media`) for a probed file.
    pub fn into_media_item(self, path: std::path::PathBuf) -> MediaItem {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        MediaItem {
            id: 0,
            path,
            name,
            kind: self.kind,
            duration: self.duration,
            width: self.width,
            height: self.height,
            fps: self.fps,
            has_audio: self.has_audio,
        }
    }
}

fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

/// Fits `w×h` inside `max_w×max_h` without upscaling; result is even and ≥ 2.
pub fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (640, 360);
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(w as f64 * s), even(h as f64 * s))
}

/// Decodes a single RGBA frame of exactly `w×h` at `time` seconds.
pub fn extract_frame(path: &Path, time: f64, w: u32, h: u32) -> Result<Vec<u8>> {
    let out = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", time.max(0.0)), "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={w}:{h}:flags=bilinear"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to run ffmpeg")?;
    let need = (w * h * 4) as usize;
    if out.stdout.len() >= need {
        let mut data = out.stdout;
        data.truncate(need);
        Ok(data)
    } else if time > 0.0 {
        extract_frame(path, 0.0, w, h)
    } else {
        bail!("could not decode a frame")
    }
}

/// Peak amplitude (0..1) every 1/[`PEAKS_PER_SEC`] seconds of the first audio stream.
pub fn waveform(path: &Path) -> Result<Vec<f32>> {
    const RATE: usize = 8000;
    let mut child = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run ffmpeg")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut reader = BufReader::with_capacity(1 << 16, stdout);
    let per_peak = RATE / PEAKS_PER_SEC as usize;
    let mut buf = vec![0u8; per_peak * 2];
    let mut peaks = Vec::new();
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => filled += n,
            }
        }
        if filled < 2 {
            break;
        }
        let peak = buf[..filled & !1]
            .chunks_exact(2)
            .map(|b| (i16::from_le_bytes([b[0], b[1]]) as i32).unsigned_abs())
            .max()
            .unwrap_or(0);
        peaks.push(peak as f32 / 32768.0);
        if filled < buf.len() {
            break;
        }
    }
    let _ = child.wait();
    // Normalise so quiet recordings are still visible.
    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        let gain = (0.95 / max).min(4.0);
        peaks.iter_mut().for_each(|p| *p = (*p * gain).min(1.0));
    }
    Ok(peaks)
}

/// `atempo` filter chain for a speed factor (each stage limited to 0.5..2).
pub fn atempo_chain(speed: f64) -> Option<String> {
    if (speed - 1.0).abs() < 1e-3 {
        return None;
    }
    let mut s = speed;
    let mut parts = Vec::new();
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s /= 0.5;
    }
    parts.push(format!("atempo={s:.5}"));
    Some(parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates() {
        assert!((parse_rate("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("25"), Some(25.0));
    }

    #[test]
    fn fitting() {
        assert_eq!(fit_within(1920, 1080, 960, 960), (960, 540));
        assert_eq!(fit_within(1080, 1920, 960, 960), (540, 960));
        assert_eq!(fit_within(320, 240, 960, 960), (320, 240));
    }

    #[test]
    fn atempo() {
        assert_eq!(atempo_chain(1.0), None);
        assert_eq!(atempo_chain(4.0).unwrap(), "atempo=2.0,atempo=2.00000");
        assert_eq!(atempo_chain(0.25).unwrap(), "atempo=0.5,atempo=0.50000");
    }
}
