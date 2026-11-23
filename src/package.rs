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

/// The project with its paths rewritten to zip-relative names, plus the files to pack
/// (source path, name inside the zip) in order.
fn collect(project: &Project) -> (Project, Vec<(PathBuf, String)>, ExportReport) {
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
    (p, order, report)
}

/// Total bytes of the media a project zip would hold (for progress bars).
pub fn media_bytes(project: &Project) -> u64 {
    collect(project).1.iter().filter_map(|(src, _)| std::fs::metadata(src).ok()).map(|m| m.len()).sum()
}

/// Writes `project` and its media (and LUTs, thumbnail) to `dest`.
pub fn export_zip(project: &Project, dest: &Path) -> Result<ExportReport> {
    let (p, order, mut report) = collect(project);

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

/// Writes the same package as [`export_zip`] to a writer that can't seek (an upload), without
/// a temporary file. Entries are stored (media is already compressed) with zip64 sizes and data
/// descriptors, which every unzip tool and [`import_zip`] read. `progress` gets the media bytes
/// written so far; when `keep_going` returns `false` it stops with an error.
pub fn stream_zip<W: Write>(project: &Project, out: W, progress: &mut dyn FnMut(u64), keep_going: &dyn Fn() -> bool) -> Result<ExportReport> {
    let (p, order, mut report) = collect(project);
    let mut zw = StreamZip { out: CountingWriter { inner: out, written: 0 }, entries: Vec::new() };
    zw.add(PROJECT_FILE, &mut serde_json::to_string_pretty(&p)?.as_bytes(), &mut |_| {}, keep_going)?;
    let mut done = 0u64;
    for (src, arc) in &order {
        let mut f = std::fs::File::open(src).with_context(|| format!("reading {}", src.display()))?;
        let base = done;
        let n = zw.add(arc, &mut f, &mut |n| progress(base + n), keep_going)?;
        done += n;
        report.files += 1;
    }
    zw.finish()?;
    Ok(report)
}

struct CountingWriter<W: Write> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct Entry {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
}

struct StreamZip<W: Write> {
    out: CountingWriter<W>,
    entries: Vec<Entry>,
}

const ZIP64_VERSION: u16 = 45;
/// Bit 3: sizes and CRC follow the data. Bit 11: names are UTF-8.
const FLAGS: u16 = (1 << 3) | (1 << 11);
/// 1980-01-01 00:00 in DOS time and date.
const DOS_TIME_DATE: [u8; 4] = [0, 0, 0x21, 0];

impl<W: Write> StreamZip<W> {
    fn add(&mut self, name: &str, data: &mut dyn Read, progress: &mut dyn FnMut(u64), keep_going: &dyn Fn() -> bool) -> Result<u64> {
        let offset = self.out.written;
        let o = &mut self.out;
        // Local header: sizes go in the zip64 extra (zeros here) and the descriptor.
        o.write_all(&0x04034b50u32.to_le_bytes())?;
        o.write_all(&ZIP64_VERSION.to_le_bytes())?;
        o.write_all(&FLAGS.to_le_bytes())?;
        o.write_all(&0u16.to_le_bytes())?; // stored
        o.write_all(&DOS_TIME_DATE)?;
        o.write_all(&0u32.to_le_bytes())?; // crc
        o.write_all(&u32::MAX.to_le_bytes())?;
        o.write_all(&u32::MAX.to_le_bytes())?;
        o.write_all(&(name.len() as u16).to_le_bytes())?;
        o.write_all(&20u16.to_le_bytes())?; // extra length
        o.write_all(name.as_bytes())?;
        o.write_all(&1u16.to_le_bytes())?;
        o.write_all(&16u16.to_le_bytes())?;
        o.write_all(&0u64.to_le_bytes())?;
        o.write_all(&0u64.to_le_bytes())?;

        let mut crc = crc32fast::Hasher::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if !keep_going() {
                bail!("cancelled");
            }
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            crc.update(&buf[..n]);
            o.write_all(&buf[..n])?;
            size += n as u64;
            progress(size);
        }
        let crc = crc.finalize();
        // Zip64 data descriptor.
        o.write_all(&0x08074b50u32.to_le_bytes())?;
        o.write_all(&crc.to_le_bytes())?;
        o.write_all(&size.to_le_bytes())?;
        o.write_all(&size.to_le_bytes())?;
        self.entries.push(Entry { name: name.to_string(), crc, size, offset });
        Ok(size)
    }

    fn finish(mut self) -> Result<()> {
        let cd_start = self.out.written;
        let o = &mut self.out;
        for e in &self.entries {
            o.write_all(&0x02014b50u32.to_le_bytes())?;
            o.write_all(&ZIP64_VERSION.to_le_bytes())?; // made by
            o.write_all(&ZIP64_VERSION.to_le_bytes())?; // needed
            o.write_all(&FLAGS.to_le_bytes())?;
            o.write_all(&0u16.to_le_bytes())?;
            o.write_all(&DOS_TIME_DATE)?;
            o.write_all(&e.crc.to_le_bytes())?;
            o.write_all(&u32::MAX.to_le_bytes())?;
            o.write_all(&u32::MAX.to_le_bytes())?;
            o.write_all(&(e.name.len() as u16).to_le_bytes())?;
            o.write_all(&28u16.to_le_bytes())?; // extra length
            o.write_all(&0u16.to_le_bytes())?; // comment
            o.write_all(&0u16.to_le_bytes())?; // disk
            o.write_all(&0u16.to_le_bytes())?; // internal attributes
            o.write_all(&0u32.to_le_bytes())?; // external attributes
            o.write_all(&u32::MAX.to_le_bytes())?; // offset: in the zip64 extra
            o.write_all(e.name.as_bytes())?;
            o.write_all(&1u16.to_le_bytes())?;
            o.write_all(&24u16.to_le_bytes())?;
            o.write_all(&e.size.to_le_bytes())?;
            o.write_all(&e.size.to_le_bytes())?;
            o.write_all(&e.offset.to_le_bytes())?;
        }
        let cd_end = o.written;
        let n = self.entries.len() as u64;
        // Zip64 end of central directory record, then its locator.
        o.write_all(&0x06064b50u32.to_le_bytes())?;
        o.write_all(&44u64.to_le_bytes())?;
        o.write_all(&ZIP64_VERSION.to_le_bytes())?;
        o.write_all(&ZIP64_VERSION.to_le_bytes())?;
        o.write_all(&0u32.to_le_bytes())?;
        o.write_all(&0u32.to_le_bytes())?;
        o.write_all(&n.to_le_bytes())?;
        o.write_all(&n.to_le_bytes())?;
        o.write_all(&(cd_end - cd_start).to_le_bytes())?;
        o.write_all(&cd_start.to_le_bytes())?;
        o.write_all(&0x07064b50u32.to_le_bytes())?;
        o.write_all(&0u32.to_le_bytes())?;
        o.write_all(&cd_end.to_le_bytes())?;
        o.write_all(&1u32.to_le_bytes())?;
        // Classic end record, pointing at the zip64 one.
        o.write_all(&0x06054b50u32.to_le_bytes())?;
        o.write_all(&0u16.to_le_bytes())?;
        o.write_all(&0u16.to_le_bytes())?;
        o.write_all(&u16::MAX.to_le_bytes())?;
        o.write_all(&u16::MAX.to_le_bytes())?;
        o.write_all(&u32::MAX.to_le_bytes())?;
        o.write_all(&u32::MAX.to_le_bytes())?;
        o.write_all(&0u16.to_le_bytes())?;
        o.flush()?;
        Ok(())
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

        // Unzipped by hand (e.g. Windows Explorer) and opened directly: the relative media
        // paths resolve against the project's folder, not the app's working directory.
        let manual = dir.join("by hand");
        let mut za = zip::ZipArchive::new(std::fs::File::open(&zip).unwrap()).unwrap();
        za.extract(&manual).unwrap();
        let q = Project::load(&manual.join(PROJECT_FILE)).unwrap();
        assert_eq!(q.media[0].path, manual.join("media").join("1-a.wav"));
        assert_eq!(std::fs::read(&q.media[0].path).unwrap(), b"hello audio");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streamed_zip_reads_back() {
        let dir = std::env::temp_dir().join(format!("rc-pkg-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("clip.mp4"), &big).unwrap();
        std::fs::write(dir.join("song é.wav"), b"hello audio").unwrap();
        let mut p = Project::default();
        p.name = "Streamed".into();
        p.media.push(MediaItem { id: 1, path: dir.join("clip.mp4"), name: "clip.mp4".into(), kind: MediaKind::Video, duration: 1.0, width: 2, height: 2, fps: 30.0, has_audio: false });
        p.media.push(MediaItem { id: 2, path: dir.join("song é.wav"), name: "song.wav".into(), kind: MediaKind::Audio, duration: 1.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let mut bytes = Vec::new();
        let mut last = 0;
        let r = stream_zip(&p, &mut bytes, &mut |n| last = n, &|| true).unwrap();
        assert_eq!(r.files, 2);
        assert_eq!(last, media_bytes(&p));
        let zip = dir.join("streamed.zip");
        std::fs::write(&zip, &bytes).unwrap();
        let proj = import_zip(&zip, &dir.join("unpacked")).unwrap();
        let q = Project::load(&proj).unwrap();
        assert_eq!(std::fs::read(&q.media[0].path).unwrap(), big);
        assert_eq!(std::fs::read(&q.media[1].path).unwrap(), b"hello audio");
        // Stopping early is an error, not a half-written "success".
        assert!(stream_zip(&p, std::io::sink(), &mut |_| {}, &|| false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
