//! Proxies: small, easy-to-seek copies of big videos used only by the live preview
//! (scrubbing and playback stay smooth); exports always read the originals.
//! Stored per user (`%LOCALAPPDATA%\RustCut\proxies`), keyed by file, size and date.

use crate::media::ffmpeg;
use crate::model::{MediaItem, MediaKind};
use anyhow::Result;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Proxy height (and the limit for "big"): the preview decodes at most 960 px anyway.
const PROXY_SHORT: u32 = 540;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn dir() -> PathBuf {
    crate::captions::models_dir().parent().map(Path::to_path_buf).unwrap_or_else(crate::util::config_dir).join("proxies")
}

/// Videos bigger than 720p (short side) get a proxy.
pub fn wanted(m: &MediaItem) -> bool {
    m.kind == MediaKind::Video && m.width.min(m.height) > 720
}

pub fn path_for(src: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    if let Ok(md) = std::fs::metadata(src) {
        md.len().hash(&mut h);
        md.modified().ok().hash(&mut h);
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().chars().take(40).collect::<String>()).unwrap_or_default();
    dir().join(format!("{stem}-{:016x}.mp4", h.finish()))
}

pub fn exists(m: &MediaItem) -> bool {
    path_for(&m.path).is_file()
}

/// The file the preview should decode: the proxy when enabled and ready.
pub fn for_preview(m: &MediaItem) -> PathBuf {
    if enabled() && wanted(m) {
        let p = path_for(&m.path);
        if p.is_file() {
            return p;
        }
    }
    m.path.clone()
}

/// Makes one proxy (silent, short GOP for fast seeking). `Ok(false)` if cancelled.
pub fn make(m: &MediaItem, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<bool> {
    let out = path_for(&m.path);
    if out.is_file() {
        return Ok(true);
    }
    std::fs::create_dir_all(dir())?;
    let (w, h) = if m.width <= m.height { (PROXY_SHORT, 0) } else { (0, PROXY_SHORT) };
    let scale = if w == 0 { format!("scale=-2:{h}") } else { format!("scale={w}:-2") };
    let tmp = out.with_extension("part.mp4");
    let mut args: Vec<String> = ["-hide_banner", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-i"].map(String::from).to_vec();
    args.push(m.path.to_string_lossy().into_owned());
    args.extend(["-vf", &scale, "-an", "-c:v", "libx264", "-preset", "veryfast", "-crf", "24", "-g", "12", "-pix_fmt", "yuv420p", "-movflags", "+faststart"].map(String::from));
    args.push(tmp.to_string_lossy().into_owned());
    if !ffmpeg::run_with_progress(&args, m.duration.max(0.1), cancel, &mut progress)? {
        let _ = std::fs::remove_file(&tmp);
        return Ok(false);
    }
    std::fs::rename(&tmp, &out)?;
    Ok(true)
}
