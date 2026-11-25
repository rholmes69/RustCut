//! Download a RustCut project zip from a Google Drive share link ("Anyone with the link").
//! No Google sign-in: private files can't be fetched this way.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Pulls the file id out of the usual Drive link shapes:
/// `/file/d/<id>/view`, `/open?id=<id>`, `/uc?id=<id>&export=download`, or a bare id.
pub fn file_id(link: &str) -> Option<String> {
    let link = link.trim();
    let ok = |s: &str| s.len() >= 10 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let take = |s: &str| s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>();
    if let Some(i) = link.find("/d/") {
        let id = take(&link[i + 3..]);
        if ok(&id) {
            return Some(id);
        }
    }
    if let Some(i) = link.find("id=") {
        let id = take(&link[i + 3..]);
        if ok(&id) {
            return Some(id);
        }
    }
    if !link.contains('/') && ok(link) {
        return Some(link.to_string());
    }
    None
}

/// Streams the file to `dest`, reporting progress 0..1. `Ok(false)` if cancelled.
pub fn download(id: &str, dest: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<bool> {
    // `confirm=t` skips Drive's "can't scan for viruses" page that big files get.
    let url = format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t");
    let resp = ureq::get(&url).call().map_err(|e| anyhow!("Could not reach Google Drive: {e}"))?;
    let ctype = resp.content_type().to_ascii_lowercase();
    if ctype.starts_with("text/html") {
        bail!("Google Drive sent a web page instead of the file. Set the file's sharing to \"Anyone with the link\" and check the link.");
    }
    let total: u64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    let (mut buf, mut got) = (vec![0u8; 1 << 20], 0u64);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        got += n as u64;
        let mb = got as f32 / 1_048_576.0;
        if total > 0 {
            progress(got as f32 / total as f32, &format!("Downloading… {mb:.0} MB"));
        } else {
            progress(0.0, &format!("Downloading… {mb:.0} MB"));
        }
    }
    out.flush()?;
    drop(out);
    let mut magic = [0u8; 2];
    std::fs::File::open(dest)?.read_exact(&mut magic).ok();
    if &magic != b"PK" {
        bail!("The downloaded file is not a zip. Is this a RustCut project zip (File → Export project as zip)?");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        let id = "1AbCdEfGhIjKlMnOpQrStUvWxYz_-0123";
        for l in [
            format!("https://drive.google.com/file/d/{id}/view?usp=sharing"),
            format!("https://drive.google.com/open?id={id}"),
            format!("https://drive.google.com/uc?export=download&id={id}"),
            id.to_string(),
        ] {
            assert_eq!(file_id(&l).as_deref(), Some(id), "{l}");
        }
        assert_eq!(file_id("https://example.com/page"), None);
        assert_eq!(file_id(""), None);
    }
}
