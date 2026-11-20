//! Project packages: a `.zip` holding the project file and every media file it uses, so a
//! project can be moved to another computer. Layout: `project.rcproj` plus `media/<n>-<name>`;
//! paths inside `project.rcproj` are relative to the zip root.

use crate::model::Project;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

pub const PROJECT_FILE: &str = "project.rcproj";

#[derive(Debug, Default)]
pub struct ExportReport {
    pub files: usize,
    /// Media files the project lists but that are not on disk (left out of the zip).
    pub missing: Vec<PathBuf>,
}

fn safe_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' }).collect();
    if s.trim().is_empty() { "file".into() } else { s }
}

/// Writes `project` and its media (and LUTs, thumbnail) to `dest`.
pub fn export_zip(project: &Project, dest: &Path) -> Result<ExportReport> {
    let mut p = project.clone();
    let mut report = ExportReport::default();
    let mut names: HashMap<PathBuf, String> = HashMap::new();
    let mut order: Vec<(PathBuf, String)> = Vec::new();
    let mut add = |path: &Path, report: &mut ExportReport| -> Option<String> {
        if let Some(n) = names.get(path) {
            return Some(n.clone());
        }
        if !path.is_file() {
            if !report.missing.contains(&path.to_path_buf()) {
                report.missing.push(path.to_path_buf());
            }
            return None;
        }
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let arc = format!("media/{}-{}", order.len() + 1, safe_name(&file));
        names.insert(path.to_path_buf(), arc.clone());
        order.push((path.to_path_buf(), arc.clone()));
        Some(arc)
    };
    for m in &mut p.media {
        if let Some(a) = add(&m.path, &mut report) {
            m.path = PathBuf::from(a);
        }
    }
    if let Some(t) = p.thumbnail.clone() {
        if let Some(a) = add(&t, &mut report) {
            p.thumbnail = Some(PathBuf::from(a));
        }
    }
    for track in &mut p.tracks {
        for clip in &mut track.clips {
            if let Some(l) = clip.grade.lut.clone() {
                if let Some(a) = add(&l, &mut report) {
                    clip.grade.lut = Some(PathBuf::from(a));
                }
            }
        }
    }

    let tmp = dest.with_extension("zip.part");
    let result = (|| -> Result<()> {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?);
        let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true);
        // Video, audio and images are already compressed: storing them is much faster.
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored).large_file(true);
        zw.start_file(PROJECT_FILE, deflate)?;
        zw.write_all(serde_json::to_string_pretty(&p)?.as_bytes())?;
        for (src, arc) in &order {
            zw.start_file(arc, stored)?;
            let mut f = std::fs::File::open(src).with_context(|| format!("reading {}", src.display()))?;
            std::io::copy(&mut f, &mut zw)?;
            report.files += 1;
        }
        zw.finish()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(dest);
            std::fs::rename(&tmp, dest).with_context(|| format!("writing {}", dest.display()))?;
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Unpacks `zip_path` into `dest_dir` (created; must be new or empty) and returns the path of
/// the extracted project, whose media paths now point at the extracted files.
pub fn import_zip(zip_path: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let mut za = zip::ZipArchive::new(std::fs::File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?)
        .context("not a valid zip file")?;
    if za.by_name(PROJECT_FILE).is_err() {
        bail!("not a RustCut project zip (no {PROJECT_FILE} inside)");
    }
    if dest_dir.exists() && std::fs::read_dir(dest_dir)?.next().is_some() {
        bail!("{} already exists and is not empty", dest_dir.display());
    }
    std::fs::create_dir_all(dest_dir)?;
    for i in 0..za.len() {
        let mut f = za.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let rel = f.enclosed_name().ok_or_else(|| anyhow!("unsafe path in zip: {}", f.name()))?;
        let out = dest_dir.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut w = std::fs::File::create(&out).with_context(|| format!("writing {}", out.display()))?;
        std::io::copy(&mut f.by_ref(), &mut w)?;
    }
    let proj_path = dest_dir.join(PROJECT_FILE);
    let mut p = Project::load(&proj_path)?;
    let abs = |rel: &Path| -> PathBuf { if rel.is_relative() { dest_dir.join(rel) } else { rel.to_path_buf() } };
    for m in &mut p.media {
        m.path = abs(&m.path);
    }
    if let Some(t) = p.thumbnail.clone() {
        p.thumbnail = Some(abs(&t));
    }
    for track in &mut p.tracks {
        for clip in &mut track.clips {
            if let Some(l) = clip.grade.lut.clone() {
                clip.grade.lut = Some(abs(&l));
            }
        }
    }
    let name = safe_name(&p.name);
    let final_path = dest_dir.join(format!("{name}.rcproj"));
    p.save(&final_path)?;
    let _ = std::fs::remove_file(&proj_path);
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    #[test]
    fn zip_round_trip() {
        let dir = std::env::temp_dir().join(format!("rc-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("a.wav");
        std::fs::write(&media, b"hello audio").unwrap();
        let mut p = Project::default();
        p.name = "My Song!".into();
        p.media.push(MediaItem { id: 1, path: media.clone(), name: "a.wav".into(), kind: MediaKind::Audio, duration: 1.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        p.media.push(MediaItem { id: 2, path: dir.join("gone.png"), name: "gone.png".into(), kind: MediaKind::Image, duration: 0.0, width: 1, height: 1, fps: 0.0, has_audio: false });
        let zip = dir.join("out.zip");
        let r = export_zip(&p, &zip).unwrap();
        assert_eq!(r.files, 1);
        assert_eq!(r.missing.len(), 1);
        let dest = dir.join("unpacked");
        let proj = import_zip(&zip, &dest).unwrap();
        let q = Project::load(&proj).unwrap();
        assert_eq!(std::fs::read(&q.media[0].path).unwrap(), b"hello audio");
        assert!(q.media[0].path.starts_with(&dest));
        assert!(import_zip(&zip, &dest).is_err(), "non-empty destination is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
