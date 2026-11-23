//! Uploads to the user's Google Drive: exported videos and project packages (a project zip is
//! streamed straight to Drive, so it needs no free disk space). Only the `drive.file` scope is
//! used: RustCut can see and change only the files it uploaded, never the rest of the Drive.
//! Sign-in and the upload protocol are in `crate::google`.

use crate::google::{self, api_error, enc, Account, Resumable, SCOPE_DRIVE};
use crate::model::Project;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::google::size_label;

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";
/// Uploads go into this folder at the top of My Drive.
pub const FOLDER_NAME: &str = "RustCut";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// The RustCut folder's id, made on first use. With the `drive.file` scope only folders this
/// app made are visible, so a user's own folder called RustCut is never touched.
fn ensure_folder(a: &mut Account, token: &str) -> Result<String> {
    let auth = format!("Bearer {token}");
    if !a.folder_id.is_empty() {
        let ok = google::agent()
            .get(&format!("{API}/files/{}?fields=id,trashed", enc(&a.folder_id)))
            .set("Authorization", &auth)
            .call()
            .ok()
            .and_then(|r| r.into_json::<Value>().ok())
            .map(|v| v["trashed"].as_bool() == Some(false))
            .unwrap_or(false);
        if ok {
            return Ok(a.folder_id.clone());
        }
    }
    let q = format!("name = '{FOLDER_NAME}' and mimeType = '{FOLDER_MIME}' and trashed = false and 'root' in parents");
    let found: Value = google::agent()
        .get(&format!("{API}/files?q={}&fields=files(id)&spaces=drive", enc(&q)))
        .set("Authorization", &auth)
        .call()
        .map_err(api_error)?
        .into_json()?;
    let id = match found["files"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let made: Value = google::agent()
                .post(&format!("{API}/files?fields=id"))
                .set("Authorization", &auth)
                .send_json(serde_json::json!({ "name": FOLDER_NAME, "mimeType": FOLDER_MIME }))
                .map_err(api_error)?
                .into_json()?;
            made["id"].as_str().ok_or_else(|| anyhow!("Drive didn't return the new folder's id"))?.to_string()
        }
    };
    a.folder_id = id.clone();
    let mut saved = google::load();
    saved.folder_id = id.clone();
    let _ = google::save(&saved);
    Ok(id)
}

#[derive(Clone, Debug)]
pub struct Uploaded {
    pub name: String,
    pub size: u64,
    /// The file's page on drive.google.com.
    pub link: String,
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// Starts a Drive upload of `name` into the RustCut folder.
fn start<'a>(a: &'a mut Account, name: &str, mime: &str, size: Option<u64>, cancel: &'a AtomicBool, progress: &'a mut dyn FnMut(u64)) -> Result<Resumable<'a>> {
    let token = google::access_token(a)?;
    let folder = ensure_folder(a, &token)?;
    let meta = serde_json::json!({ "name": name, "parents": [folder] });
    Resumable::start(a, token, &format!("{UPLOAD_API}?uploadType=resumable&fields=id,name,size,webViewLink"), &meta, mime, size, cancel, progress)
}

fn uploaded(v: &Value, fallback_name: &str, size: u64) -> Uploaded {
    Uploaded {
        name: v["name"].as_str().unwrap_or(fallback_name).to_string(),
        size,
        link: v["webViewLink"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://drive.google.com/file/d/{}/view", v["id"].as_str().unwrap_or_default())),
    }
}

/// Uploads a file (e.g. an exported video) to the RustCut folder. `Ok(None)` when cancelled.
pub fn upload_file(path: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<Uploaded>> {
    let mut a = google::load();
    let size = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "video.mp4".into());
    report(0.0, "Connecting to Google Drive…");
    let mut progress = |n: u64| report(if size > 0 { n as f32 / size as f32 } else { 1.0 }, &format!("Uploading {} of {}", size_label(n), size_label(size)));
    let mut up = start(&mut a, &name, mime_for(path), Some(size), cancel, &mut progress)?;
    let mut f = std::fs::File::open(path)?;
    if let Err(e) = std::io::copy(&mut f, &mut up) {
        up.abort();
        return if cancel.load(Ordering::Relaxed) { Ok(None) } else { Err(e.into()) };
    }
    match up.finish() {
        Ok(v) => Ok(Some(uploaded(&v, &name, size))),
        Err(_) if cancel.load(Ordering::Relaxed) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Packs the project and all its media as a zip straight into the RustCut folder, with no
/// local temporary file. Returns the upload and how many media files were missing.
pub fn upload_project_zip(project: &Project, name: &str, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<(Uploaded, usize)>> {
    let mut a = google::load();
    let total = crate::package::media_bytes(project).max(1);
    report(0.0, "Connecting to Google Drive…");
    let sent = std::cell::Cell::new(0u64);
    let mut upload_progress = |n: u64| sent.set(n);
    let mut up = start(&mut a, name, "application/zip", None, cancel, &mut upload_progress)?;
    let mut packed = |n: u64| report((n as f32 / total as f32).min(0.999), &format!("Packing and uploading {} of {}", size_label(n), size_label(total)));
    let keep_going = || !cancel.load(Ordering::Relaxed);
    let pack = match crate::package::stream_zip(project, &mut up, &mut packed, &keep_going) {
        Ok(r) => r,
        Err(e) => {
            up.abort();
            return if cancel.load(Ordering::Relaxed) { Ok(None) } else { Err(e) };
        }
    };
    let v = match up.finish() {
        Ok(v) => v,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(None),
        Err(e) => return Err(e),
    };
    let size = v["size"].as_str().and_then(|n| n.parse().ok()).unwrap_or(sent.get());
    Ok(Some((uploaded(&v, name, size), pack.missing.len())))
}

/// True when the saved account may upload to Drive.
pub fn ready() -> bool {
    google::load().can(SCOPE_DRIVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types() {
        assert_eq!(mime_for(Path::new("a/B.MP4")), "video/mp4");
        assert_eq!(mime_for(Path::new("x.zip")), "application/zip");
        assert_eq!(mime_for(Path::new("x")), "application/octet-stream");
    }

    /// Real upload with the saved sign-in: `GDRIVE_LIVE=<file> cargo test --lib gdrive_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gdrive_live() {
        let path = std::path::PathBuf::from(std::env::var("GDRIVE_LIVE").expect("GDRIVE_LIVE=<file to upload>"));
        let cancel = AtomicBool::new(false);
        let r = upload_file(&path, &cancel, &mut |p, s| eprintln!("{:3.0}% {s}", p * 100.0)).unwrap().unwrap();
        eprintln!("uploaded {} ({}) > {}", r.name, size_label(r.size), r.link);
    }
}
