//! Reshoot one clip on the timeline from a prompt: draw a new still (keeping the
//! storyboard's style and cast, and optionally the current shot's look), animate it,
//! and swap it in for just that cut — other uses of the same storyboard shot stay.

use crate::ai::image_gen;
use crate::ai::openrouter::Client;
use crate::ai::storyboard::{self, Shot, Storyboard};
use crate::ai::video_gen::{self, Generated, VideoModel};
use crate::media::ffmpeg::ProbeInfo;
use crate::model::{Id, MediaKind, Project};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Where the clip came from, for pre-filling the prompt.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub storyboard: Option<Storyboard>,
    /// The storyboard shot this clip is a cut of.
    pub shot: Option<usize>,
    /// The clip is a lip-sync performance (its lip-sync should be redone after a reshoot).
    pub performance: bool,
}

impl Source {
    pub fn description(&self) -> String {
        self.shot_ref().map(|s| s.description.clone()).unwrap_or_default()
    }
    pub fn motion(&self) -> String {
        self.shot_ref().map(|s| s.motion.clone()).unwrap_or_default()
    }
    pub fn no_lead(&self) -> bool {
        self.shot_ref().is_some_and(|s| s.no_lead)
    }
    fn shot_ref(&self) -> Option<&Shot> {
        self.storyboard.as_ref().zip(self.shot).and_then(|(sb, i)| sb.shots.get(i))
    }
}

/// The storyboard shot number in a file name like `shot-16-video.mp4` or
/// `performance-shot-16-video-mp4-1.mp4`.
pub fn shot_number(name: &str) -> Option<usize> {
    let lower = name.to_lowercase();
    let at = lower.find("shot-")? + 5;
    let digits: String = lower[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&n| n >= 1)
}

pub fn find_source(project: &Project, clip: Id) -> Source {
    let Some(media) = project.clip(clip).and_then(|c| c.media_id()).and_then(|m| project.media(m)) else { return Source::default() };
    let name = media.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let performance = name.starts_with("performance-") || media.path.components().any(|c| c.as_os_str() == "lipsync");
    // Storyboard folders: this media's own, then any other media's in the project.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in std::iter::once(&media.path).chain(project.media.iter().map(|m| &m.path)) {
        if let Some(d) = p.parent() {
            if d.join("storyboard.json").is_file() && !dirs.iter().any(|x| x == d) {
                dirs.push(d.to_path_buf());
            }
        }
    }
    let mut fallback = None;
    for d in dirs {
        let Ok(sb) = Storyboard::load(&d.join("storyboard.json")) else { continue };
        if let Some(i) = sb.shots.iter().position(|s| s.video.as_deref() == Some(media.path.as_path())) {
            return Source { storyboard: Some(sb), shot: Some(i), performance };
        }
        if let Some(n) = shot_number(&name).filter(|&n| n <= sb.shots.len()) {
            return Source { storyboard: Some(sb), shot: Some(n - 1), performance };
        }
        fallback.get_or_insert(sb);
    }
    Source { storyboard: fallback, shot: None, performance }
}

/// A one-shot storyboard for the reshoot (keeps the original's style, cast and lead).
pub fn one_shot(project: &Project, src: &Source, description: &str, motion: &str, seconds: f64, no_lead: bool, dir: &Path) -> Storyboard {
    let shot = Shot { description: description.trim().to_string(), motion: motion.trim().to_string(), caption: String::new(), seconds, frame: None, approved: false, video: None, no_lead };
    match &src.storyboard {
        Some(sb) => {
            let mut sb = sb.clone();
            sb.shots = vec![shot];
            sb.slots.clear();
            sb.song_clip = None;
            sb.spent = 0.0;
            sb.dir = dir.to_path_buf();
            sb
        }
        None => Storyboard {
            brief: String::new(),
            aspect: project.aspect,
            title: "Reshoot".into(),
            style: String::new(),
            script: String::new(),
            music: String::new(),
            shots: vec![shot],
            dir: dir.to_path_buf(),
            spent: 0.0,
            slots: Vec::new(),
            song_clip: None,
            fresh_analysis: None,
            character: None,
            cast: Vec::new(),
        },
    }
}

/// A fresh folder for this reshoot, outside any storyboard folder (so re-assembling
/// the storyboard never removes it).
pub fn work_dir(project_path: Option<&Path>, clip: Id) -> PathBuf {
    let root = video_gen::output_dir(project_path).join("reshoots");
    let mut n = 1;
    loop {
        let d = root.join(format!("clip-{clip}-{n}"));
        if !d.exists() {
            return d;
        }
        n += 1;
    }
}

/// The current shot's middle frame, used as a look reference.
pub fn current_look(project: &Project, clip: Id, dir: &Path) -> Result<PathBuf> {
    let c = project.clip(clip).ok_or_else(|| anyhow!("the clip was removed"))?;
    let m = c.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("no media"))?;
    if m.kind == MediaKind::Image {
        return Ok(m.path.clone());
    }
    std::fs::create_dir_all(dir)?;
    let jpg = video_gen::frame_jpeg(&m.path, c.source_in + c.duration * c.speed * 0.5, 1280)?;
    let out = dir.join("current-look.jpg");
    std::fs::write(&out, jpg)?;
    Ok(out)
}

/// Draws (or redraws) the reshoot still. Returns its path and cost.
pub fn draw(client: &Client, sb: &mut Storyboard, model: &str, look: Option<&Path>) -> Result<(PathBuf, f64)> {
    if sb.shots[0].description.is_empty() {
        bail!("describe what the new shot should show");
    }
    std::fs::create_dir_all(&sb.dir)?;
    let models = image_gen::list_models(client).unwrap_or_default();
    let cost = storyboard::generate_frame(client, sb, 0, model, &models, look)?;
    let frame = sb.shots[0].frame.clone().ok_or_else(|| anyhow!("no image came back"))?;
    Ok((frame, cost))
}

/// Animates the drawn still. `Ok(None)` if cancelled.
pub fn animate(client: &Client, sb: &Storyboard, m: &VideoModel, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<Generated>> {
    storyboard::animate_shot(client, sb, 0, m, false, cancel, report)
}

pub fn estimate_animation(sb: &Storyboard, m: &VideoModel) -> Option<f64> {
    storyboard::estimate_video_cost(sb, &[0], m, false).map(|(c, _)| c)
}

/// Swaps the new footage in for `clip`: same place, length (or shorter if the
/// generated clip is), look and volume.
pub fn apply(project: &mut Project, clip: Id, path: &Path, info: ProbeInfo) -> Result<Id> {
    let (ti, _) = project.locate(clip).ok_or_else(|| anyhow!("the clip was removed"))?;
    let old = project.remove_clip(clip).ok_or_else(|| anyhow!("the clip was removed"))?;
    let len = info.duration.max(0.1);
    let media = project.add_media(info.into_media_item(path.to_path_buf()));
    let mut c = project.new_media_clip(media, old.start).ok_or_else(|| anyhow!("could not use the new footage"))?;
    c.start = old.start;
    c.source_in = 0.0;
    c.duration = old.duration.min(len);
    c.transform = old.transform;
    c.filter = old.filter;
    c.adjust = old.adjust;
    c.opacity = old.opacity;
    c.transition_in = old.transition_in;
    c.volume = old.volume;
    let id = c.id;
    project.tracks[ti].clips.push(c);
    project.sort_track(ti);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MediaItem;

    #[test]
    fn reads_shot_numbers_from_file_names() {
        assert_eq!(shot_number("shot-16-video.mp4"), Some(16));
        assert_eq!(shot_number("performance-shot-16-video-mp4-1.mp4"), Some(16));
        assert_eq!(shot_number("shot-03.png"), Some(3));
        assert_eq!(shot_number("broll.mp4"), None);
    }

    #[test]
    fn finds_the_storyboard_shot_and_prefills() {
        let dir = std::env::temp_dir().join(format!("rc-reshoot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("shot-02-video.mp4");
        let mut p = Project::default();
        let mut sb = one_shot(&p, &Source::default(), "Aishyla on the rooftop", "slow crane up", 4.0, false, &dir);
        sb.shots.push(Shot { description: "Rain street, Aishyla sings".into(), motion: "orbit".into(), caption: String::new(), seconds: 4.0, frame: None, approved: true, video: Some(video.clone()), no_lead: false });
        sb.style = "moody neon".into();
        sb.save().unwrap();
        let m = p.add_media(MediaItem { id: 0, path: video, name: "shot-02-video.mp4".into(), kind: MediaKind::Video, duration: 6.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.append_to_main(c);
        let src = find_source(&p, id);
        assert_eq!(src.shot, Some(1));
        assert_eq!(src.description(), "Rain street, Aishyla sings");
        assert!(!src.performance);
        let one = one_shot(&p, &src, "Close-up, she sings in the rain", "push-in", 3.7, false, &dir.join("r1"));
        assert_eq!(one.shots.len(), 1);
        assert_eq!(one.style, "moody neon", "keeps the storyboard's look");
        assert!(one.slots.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swaps_only_that_clip() {
        let mut p = Project::default();
        let v = p.add_media(MediaItem { id: 0, path: "shot-16-video.mp4".into(), name: "shot-16-video.mp4".into(), kind: MediaKind::Video, duration: 6.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        let mut ids = Vec::new();
        for _ in 0..3 {
            let mut c = p.new_media_clip(v, 0.0).unwrap();
            c.duration = 3.7;
            c.volume = 0.0;
            ids.push(c.id);
            p.append_to_main(c);
        }
        let info = ProbeInfo { kind: MediaKind::Video, duration: 5.0, width: 1280, height: 720, fps: 24.0, has_audio: false };
        let new = apply(&mut p, ids[1], Path::new("reshoot.mp4"), info).unwrap();
        let main = p.main_track();
        let clips = &p.tracks[main].clips;
        assert_eq!(clips.len(), 3);
        assert_eq!(clips[1].id, new);
        assert!((clips[1].start - 3.7).abs() < 1e-9 && (clips[1].duration - 3.7).abs() < 1e-9);
        assert_eq!(clips[1].volume, 0.0);
        assert_eq!(clips[0].media_id(), Some(v), "other cuts of the shot are untouched");
        assert_eq!(clips[2].media_id(), Some(v));
    }
}
