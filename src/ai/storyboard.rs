//! Prompt-to-video storyboard: a text model writes a script and shot list, the Image
//! API draws one frame per shot, the user approves/regenerates/swaps frames, and only
//! approved shots are animated (image-to-video) and assembled on the timeline with
//! captions, cross-dissolves, ducked music and −14 LUFS loudness.

use super::image_gen::{self, ImageModel, ImageRequest};
use super::openrouter::Client;
use super::video_gen::{self, GenRequest, Generated, VideoModel};
use crate::captions::{self, Layout, Segment};
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::{Aspect, AudioRole, Id, Project, Transition, TransitionKind};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const MAX_SHOTS: usize = 12;
pub const MIN_SHOT: f64 = 2.0;
pub const MAX_SHOT: f64 = 10.0;
/// Cross-dissolve between shots (seconds).
pub const DISSOLVE: f64 = 0.5;
/// Loudness target for the finished video.
pub const LOUDNESS: f32 = -14.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    /// What the frame shows (used for the storyboard image).
    pub description: String,
    /// How it moves (camera and subject motion, used for image-to-video).
    pub motion: String,
    /// On-screen caption / script line for this shot (may be empty).
    pub caption: String,
    /// Planned length in seconds.
    pub seconds: f64,
    #[serde(default)]
    pub frame: Option<PathBuf>,
    #[serde(default)]
    pub approved: bool,
    /// The animated clip made from the current frame (reused instead of paying again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<PathBuf>,
    /// The lead character is not in this shot: the character photo isn't used, and
    /// other people are drawn as described.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_lead: bool,
}

/// Music-video mode: where a shot plays on the timeline (cut on the bars).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub start: f64,
    pub end: f64,
    /// Index into `shots`.
    pub shot: usize,
    pub section: String,
    #[serde(default)]
    pub lyric: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Storyboard {
    pub brief: String,
    pub aspect: Aspect,
    pub title: String,
    /// Shared visual style, appended to every frame prompt for consistency.
    pub style: String,
    /// The whole script as prose (for reference / voiceover).
    pub script: String,
    pub music: String,
    pub shots: Vec<Shot>,
    /// Folder holding the frames, clips and `storyboard.json`.
    #[serde(default)]
    pub dir: PathBuf,
    /// Money spent on this storyboard so far (USD).
    #[serde(default)]
    pub spent: f64,
    /// Music-video mode: shots timed to the song (empty for a regular storyboard).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_clip: Option<Id>,
    /// Beat analysis made while planning, for the editor to keep.
    #[serde(skip)]
    pub fresh_analysis: Option<(Id, crate::music::MusicAnalysis)>,
    /// Photo of the lead character, passed to every frame so they look like that person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<PathBuf>,
    /// Named people with a photo or character sheet. A shot gets the photos of the people
    /// its description names, so several recurring characters keep their likeness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<CastMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CastMember {
    pub name: String,
    pub photo: PathBuf,
}

impl Storyboard {
    /// Cast members named in shot `i`'s description (whole-word, case-insensitive), at most 3.
    pub fn cast_in(&self, i: usize) -> Vec<&CastMember> {
        if self.shots[i].no_lead {
            return Vec::new();
        }
        let words: Vec<String> = self.shots[i].description.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).collect();
        self.cast
            .iter()
            .filter(|c| {
                let first = c.name.split_whitespace().next().unwrap_or(&c.name).to_lowercase();
                !first.is_empty() && words.contains(&first)
            })
            .take(3)
            .collect()
    }
}

impl Storyboard {
    pub fn total_seconds(&self) -> f64 {
        self.shots.iter().map(|s| s.seconds).sum()
    }

    pub fn approved(&self) -> Vec<usize> {
        (0..self.shots.len()).filter(|&i| self.shots[i].approved && self.shots[i].frame.is_some()).collect()
    }

    pub fn save(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join("storyboard.json");
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Storyboard> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn is_music_video(&self) -> bool {
        !self.slots.is_empty()
    }

    /// How many timeline slots use shot `i` (music-video mode).
    pub fn uses(&self, i: usize) -> usize {
        self.slots.iter().filter(|s| s.shot == i).count()
    }

    pub fn swap(&mut self, a: usize, b: usize) {
        if a < self.shots.len() && b < self.shots.len() {
            self.shots.swap(a, b);
            for s in &mut self.slots {
                if s.shot == a {
                    s.shot = b;
                } else if s.shot == b {
                    s.shot = a;
                }
            }
        }
    }

    /// Moves shot `from` to position `to`, keeping slot references right.
    pub fn move_shot(&mut self, from: usize, to: usize) {
        if from >= self.shots.len() || to >= self.shots.len() || from == to {
            return;
        }
        let shot = self.shots.remove(from);
        self.shots.insert(to, shot);
        let map = |i: usize| -> usize {
            if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            }
        };
        for s in &mut self.slots {
            s.shot = map(s.shot);
        }
    }

    /// Removes shot `i`; its slots borrow a neighbouring shot.
    pub fn remove_shot(&mut self, i: usize) {
        if i >= self.shots.len() || self.shots.len() == 1 {
            return;
        }
        self.shots.remove(i);
        let replacement = i.min(self.shots.len() - 1);
        for s in &mut self.slots {
            s.shot = if s.shot == i {
                replacement
            } else if s.shot > i {
                s.shot - 1
            } else {
                s.shot
            };
        }
    }
}

// ── 1. script and shot list ──────────────────────────────────────────────────

fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "style", "script", "music", "shots"],
        "properties": {
            "title": { "type": "string" },
            "style": { "type": "string", "description": "One sentence visual style shared by every shot: palette, lighting, lens, film look" },
            "script": { "type": "string", "description": "The full script / voiceover as prose" },
            "music": { "type": "string", "description": "Music mood and tempo, e.g. 'warm acoustic, 90 bpm'" },
            "shots": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["description", "motion", "caption", "seconds"],
                    "properties": {
                        "description": { "type": "string", "description": "A single still frame: subject, setting, composition, light. No text in the image." },
                        "motion": { "type": "string", "description": "Camera and subject motion over the shot, e.g. 'slow push-in, steam rises from the cup'" },
                        "caption": { "type": "string", "description": "Short on-screen line for this shot (max ~8 words), or empty" },
                        "seconds": { "type": "number" }
                    }
                }
            }
        }
    })
}

pub fn plan_messages(brief: &str, seconds: f64, aspect: Aspect) -> Value {
    let shots = (seconds / 4.0).round().clamp(3.0, MAX_SHOTS as f64) as usize;
    json!([
        { "role": "system", "content": "You are a creative director and storyboard artist for short social videos and ads. \
Write a tight script and a shot list that a text-to-image model can draw and an image-to-video model can animate. \
Each shot is ONE continuous moment with simple motion (no cuts inside a shot, no on-screen text in the frame itself). \
Keep characters, wardrobe, location and palette consistent across shots. Open with a hook, end with a clear call to action caption." },
        { "role": "user", "content": format!(
            "Brief: {brief}\nTotal length: about {seconds:.0} seconds. Canvas: {} ({}).\nWrite about {shots} shots of {MIN_SHOT:.0}–{MAX_SHOT:.0} seconds each; the seconds should add up to the total.",
            aspect.short_label(), if aspect.ratio().0 < aspect.ratio().1 { "vertical" } else { "horizontal" }
        ) }
    ])
}

/// Turns the model's JSON into a storyboard, cleaning up lengths and counts.
pub fn parse_plan(v: &Value, brief: &str, seconds: f64, aspect: Aspect) -> Result<Storyboard> {
    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let mut shots: Vec<Shot> = v["shots"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| {
            let description = x["description"].as_str()?.trim().to_string();
            if description.is_empty() {
                return None;
            }
            Some(Shot {
                description,
                motion: x["motion"].as_str().unwrap_or("").trim().to_string(),
                caption: x["caption"].as_str().unwrap_or("").trim().to_string(),
                seconds: x["seconds"].as_f64().unwrap_or(4.0),
                frame: None,
                approved: false,
                video: None,
                no_lead: false,
            })
        })
        .take(MAX_SHOTS)
        .collect();
    if shots.is_empty() {
        bail!("the model returned no shots; try rewording the brief");
    }
    // Scale lengths to the requested total, then clamp each shot.
    let sum: f64 = shots.iter().map(|s| s.seconds.max(0.1)).sum();
    for shot in &mut shots {
        shot.seconds = ((shot.seconds.max(0.1) * seconds / sum) * 2.0).round() / 2.0;
        shot.seconds = shot.seconds.clamp(MIN_SHOT, MAX_SHOT);
    }
    Ok(Storyboard {
        brief: brief.trim().to_string(),
        aspect,
        title: s("title"),
        style: s("style"),
        script: s("script"),
        music: s("music"),
        shots,
        dir: PathBuf::new(),
        spent: 0.0,
        slots: Vec::new(),
        song_clip: None,
        fresh_analysis: None,
        character: None,
        cast: Vec::new(),
    })
}

/// Writes the script and shot list. Returns the storyboard (with `dir` set) and the cost.
pub fn plan(client: &Client, model: &str, brief: &str, seconds: f64, aspect: Aspect, out_root: &Path) -> Result<Storyboard> {
    if brief.trim().is_empty() {
        bail!("describe the video first");
    }
    let (v, usage) = client.chat_json(model, plan_messages(brief, seconds, aspect), "storyboard", schema())?;
    let mut sb = parse_plan(&v, brief, seconds, aspect)?;
    sb.spent += usage.cost;
    let name = if sb.title.is_empty() { brief } else { &sb.title };
    let mut dir = out_root.join(format!("storyboard-{}", video_gen::slug(name)));
    let mut n = 2;
    while dir.exists() {
        dir = out_root.join(format!("storyboard-{}-{n}", video_gen::slug(name)));
        n += 1;
    }
    sb.dir = dir;
    sb.save()?;
    Ok(sb)
}

// ── 2. storyboard frames ─────────────────────────────────────────────────────

pub fn frame_prompt(sb: &Storyboard, i: usize, with_reference: bool) -> String {
    frame_prompt_with(sb, i, with_reference, false)
}

/// `with_character`: the first reference image is a photo of the lead.
pub fn frame_prompt_with(sb: &Storyboard, i: usize, with_reference: bool, with_character: bool) -> String {
    let shot = &sb.shots[i];
    let mut p = format!(
        "A single cinematic film still for a {} video: {}. Style: {}. Photorealistic, high detail, natural composition for a {} frame. \
         No text, no captions, no logos, no watermark, no borders.",
        if sb.aspect.ratio().0 < sb.aspect.ratio().1 { "vertical" } else { "horizontal" },
        shot.description,
        if sb.style.is_empty() { "warm, polished commercial look" } else { &sb.style },
        sb.aspect.short_label()
    );
    if with_character {
        p += " The lead character is the person in the first reference image: whenever the lead appears, keep her face, features, skin tone and hair clearly recognisable as that person (wardrobe, pose and setting follow this shot).";
    }
    if with_reference && shot.no_lead && sb.character.is_some() {
        p += " The reference image is another shot of the same video: match only its colour palette, lighting and kind of location. \
              The woman in it is NOT in this shot; show only the people described here.";
        return p;
    }
    if with_reference {
        p += if with_character {
            " The second reference image is another shot of the same video: keep its wardrobe, locations and colour palette, but show this new moment."
        } else {
            " Keep the same characters, wardrobe, location and colour palette as the reference image (it is another shot of the same video), but show this new moment."
        };
    }
    p
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

/// Draws (or redraws) shot `i`'s frame. Another shot's frame, when given, is passed
/// as a reference so the look stays consistent. Returns the cost.
pub fn generate_frame(client: &Client, sb: &mut Storyboard, i: usize, model: &str, models: &[ImageModel], reference: Option<&Path>) -> Result<f64> {
    if i >= sb.shots.len() {
        bail!("there is no shot {}", i + 1);
    }
    let (cw, ch) = sb.aspect.size_for(1080);
    let m = models.iter().find(|m| m.id == model);
    let can_ref = m.is_none_or(|m| m.reference_input);
    let read = |p: &Path| std::fs::read(p).ok().map(|b| (b, mime_for(p)));
    let reference = reference.filter(|_| can_ref).and_then(read);
    if !sb.cast.is_empty() {
        let cast: Vec<(String, (Vec<u8>, &'static str))> = sb.cast_in(i).into_iter().filter(|_| can_ref).filter_map(|c| read(&c.photo).map(|b| (c.name.clone(), b))).collect();
        let names: Vec<String> = cast.iter().map(|c| c.0.clone()).collect();
        let req = ImageRequest {
            model: model.to_string(),
            prompt: cast_frame_prompt(sb, i, &names, reference.is_some()),
            aspect_ratio: m.and_then(|m| m.pick_aspect(cw as f64, ch as f64)),
            resolution: m.and_then(ImageModel::pick_resolution),
            transparent: false,
            references: cast.into_iter().map(|c| c.1).chain(reference).collect(),
        };
        return finish_frame(client, sb, i, &req);
    }
    let character = sb.character.as_deref().filter(|_| can_ref && !sb.shots[i].no_lead).and_then(read);
    let req = ImageRequest {
        model: model.to_string(),
        prompt: frame_prompt_with(sb, i, reference.is_some(), character.is_some()),
        aspect_ratio: m.and_then(|m| m.pick_aspect(cw as f64, ch as f64)),
        resolution: m.and_then(ImageModel::pick_resolution),
        transparent: false,
        references: character.into_iter().chain(reference).collect(),
    };
    finish_frame(client, sb, i, &req)
}

/// Prompt for a storyboard with a cast: one reference photo per person named in the shot,
/// then (optionally) another shot of the video for the look.
pub fn cast_frame_prompt(sb: &Storyboard, i: usize, names: &[String], with_style_ref: bool) -> String {
    let mut p = frame_prompt_with(sb, i, false, false);
    for (k, n) in names.iter().enumerate() {
        p += &format!(
            " Reference image {} shows {n} (it may be a character sheet with several views): {n} must look exactly like this person (face, features, skin tone, hair and build); wardrobe, pose and setting follow this shot.",
            k + 1
        );
    }
    let absent: Vec<&str> = sb.cast.iter().map(|c| c.name.as_str()).filter(|n| !names.iter().any(|x| x == n)).collect();
    if with_style_ref {
        p += &format!(
            " The last reference image is another shot of the same video: match only its colour palette, lighting and kind of locations{}.",
            if absent.is_empty() { String::new() } else { format!("; {} not in this shot unless described", absent.join(" and ")) }
        );
    }
    p
}

fn finish_frame(client: &Client, sb: &mut Storyboard, i: usize, req: &ImageRequest) -> Result<f64> {
    let img = image_gen::generate(client, req)?;
    let path = image_gen::save(&img, &sb.dir, &format!("shot-{:02}", i + 1))?;
    let cost = img.cost.unwrap_or(0.0);
    let shot = &mut sb.shots[i];
    shot.frame = Some(path);
    shot.approved = false;
    shot.video = None;
    sb.spent += cost;
    let _ = sb.save();
    Ok(cost)
}

/// The frame other shots use as their style reference: the first shot that has one,
/// other than `i` itself.
pub fn reference_for(sb: &Storyboard, i: usize) -> Option<PathBuf> {
    sb.shots.iter().enumerate().filter(|(j, _)| *j != i).find_map(|(_, s)| s.frame.clone())
}

/// Draws every shot that has no frame yet, the first one first (it becomes the
/// reference for the rest).
pub fn generate_missing_frames(
    client: &Client,
    sb: &mut Storyboard,
    model: &str,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f32, &str),
) -> Result<()> {
    let models = image_gen::list_models(client).unwrap_or_default();
    let todo: Vec<usize> = (0..sb.shots.len()).filter(|&i| sb.shots[i].frame.is_none()).collect();
    let mut failures = Vec::new();
    for (k, &i) in todo.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        report(k as f32 / todo.len() as f32, &format!("Drawing shot {} of {}…", i + 1, sb.shots.len()));
        let reference = reference_for(sb, i);
        if let Err(e) = generate_frame(client, sb, i, model, &models, reference.as_deref()) {
            failures.push(format!("shot {}: {e:#}", i + 1));
        }
    }
    if !failures.is_empty() && failures.len() == todo.len() {
        bail!("{}", failures.join("; "));
    }
    Ok(())
}

// ── 3. animation ─────────────────────────────────────────────────────────────

pub fn motion_prompt(sb: &Storyboard, i: usize) -> String {
    let s = &sb.shots[i];
    let motion = if s.motion.is_empty() { "gentle, natural camera movement" } else { &s.motion };
    format!("{motion}. {} Smooth, realistic motion, one continuous shot, no cuts, no text.", s.description)
}

/// Estimated video cost for the given shots with `m` (None if the model has no price).
pub fn estimate_video_cost(sb: &Storyboard, shots: &[usize], m: &VideoModel, audio: bool) -> Option<(f64, bool)> {
    let (cw, ch) = sb.aspect.size_for(1080);
    let aspect = m.pick_aspect(cw as f64, ch as f64);
    let res = m.default_resolution();
    let mut total = 0.0;
    let mut rough = false;
    for &i in shots {
        let (c, r) = video_gen::estimate_cost(m, m.pick_duration(sb.shots[i].seconds), &res, &aspect, audio, true)?;
        total += c;
        rough |= r;
    }
    Some((total, rough))
}

/// Animates shot `i` from its frame. `None` if cancelled.
pub fn animate_shot(
    client: &Client,
    sb: &Storyboard,
    i: usize,
    m: &VideoModel,
    audio: bool,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f32, &str),
) -> Result<Option<Generated>> {
    let frame = sb.shots[i].frame.as_ref().ok_or_else(|| anyhow!("shot {} has no frame yet", i + 1))?;
    if !m.first_frame {
        bail!("{} can't animate an image; pick a model with image-to-video support", m.id);
    }
    let (cw, ch) = sb.aspect.size_for(1080);
    let req = GenRequest {
        model: m.id.clone(),
        prompt: motion_prompt(sb, i),
        duration: m.pick_duration(sb.shots[i].seconds),
        resolution: m.default_resolution(),
        aspect_ratio: m.pick_aspect(cw as f64, ch as f64),
        generate_audio: audio && m.audio != Some(false),
        first_frame: Some((std::fs::read(frame)?, mime_for(frame))),
        last_frame: None,
    };
    let out = video_gen::generate(client, &req, &sb.dir, &format!("shot-{:02}-video", i + 1), cancel, report)?;
    if let Some(g) = &out {
        crate::economics::link(&g.path, frame);
    }
    Ok(out)
}

impl Storyboard {
    /// Of `shots`, those that still need animating (no video yet for the current frame).
    pub fn to_animate(&self, shots: &[usize]) -> Vec<usize> {
        shots.iter().copied().filter(|&i| !self.shots[i].video.as_ref().is_some_and(|v| v.exists())).collect()
    }
}

/// Animates the approved `shots` that have no video yet, reuses the ones that do,
/// and records new videos in the storyboard. Returns (all clips, errors, reused count).
pub fn produce_shots(
    client: &Client,
    sb: &mut Storyboard,
    shots: &[usize],
    m: &VideoModel,
    audio: bool,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f32, &str),
) -> (Vec<(usize, Generated)>, Vec<String>, usize) {
    let todo = sb.to_animate(shots);
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut reused = 0;
    for &i in shots.iter().filter(|i| !todo.contains(i)) {
        let path = sb.shots[i].video.clone().unwrap();
        match crate::media::ffmpeg::probe(&path) {
            Ok(info) => {
                out.push((i, Generated { path, info, cost: None, job_id: String::new() }));
                reused += 1;
            }
            Err(e) => errors.push(format!("shot {}: {e:#}", i + 1)),
        }
    }
    if !todo.is_empty() {
        let (rendered, errs) = animate_many(client, sb, &todo, m, audio, cancel, report);
        for (i, g) in &rendered {
            sb.shots[*i].video = Some(g.path.clone());
        }
        out.extend(rendered);
        errors.extend(errs);
    }
    out.sort_by_key(|(i, _)| *i);
    (out, errors, reused)
}

/// Animates several shots in parallel. Returns the finished clips and per-shot errors.
pub fn animate_many(
    client: &Client,
    sb: &Storyboard,
    shots: &[usize],
    m: &VideoModel,
    audio: bool,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f32, &str),
) -> (Vec<(usize, Generated)>, Vec<String>) {
    let progress = std::sync::Mutex::new(vec![0.0f32; shots.len()]);
    let results: std::sync::Mutex<Vec<(usize, Result<Option<Generated>>)>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (k, &i) in shots.iter().enumerate() {
            let (progress, results) = (&progress, &results);
            s.spawn(move || {
                let mut rep = |p: f32, _: &str| progress.lock().unwrap()[k] = p;
                let r = animate_shot(client, sb, i, m, audio, cancel, &mut rep);
                progress.lock().unwrap()[k] = 1.0;
                results.lock().unwrap().push((i, r));
            });
        }
        loop {
            let done = results.lock().unwrap().len();
            let avg = progress.lock().unwrap().iter().sum::<f32>() / shots.len().max(1) as f32;
            report(avg * 0.95, &format!("Animating shots: {done} of {} done…", shots.len()));
            if done == shots.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    let mut rendered = Vec::new();
    let mut errors = Vec::new();
    for (i, r) in results.into_inner().unwrap() {
        match r {
            Ok(Some(g)) => rendered.push((i, g)),
            Ok(None) => errors.push(format!("shot {} stopped", i + 1)),
            Err(e) => errors.push(format!("shot {}: {e:#}", i + 1)),
        }
    }
    rendered.sort_by_key(|(i, _)| *i);
    (rendered, errors)
}

// ── 4. assembly ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssembleReport {
    pub clips: Vec<Id>,
    pub captions: usize,
    pub music: Option<Id>,
    pub ducked: bool,
    pub duration: f64,
}

/// Puts the animated shots on the timeline (after anything already there), in
/// storyboard order, with cross-dissolves, the shots' captions, optional music
/// (ducked under any voice) and the −14 LUFS loudness target.
pub fn assemble(
    project: &mut Project,
    sb: &Storyboard,
    rendered: &[(usize, Generated)],
    music: Option<(PathBuf, ProbeInfo)>,
    peaks_for: &dyn Fn(&Path) -> Option<Arc<Vec<f32>>>,
) -> AssembleReport {
    let mut report = AssembleReport::default();
    let mut ordered: Vec<&(usize, Generated)> = rendered.iter().collect();
    ordered.sort_by_key(|(i, _)| *i);
    if ordered.is_empty() {
        return report;
    }
    project.aspect = sb.aspect;
    let main = project.main_track();
    let t0 = project.tracks[main].end();
    let mut segments = Vec::new();
    for (i, gen) in ordered {
        let media = project.add_media(gen.info.clone().into_media_item(gen.path.clone()));
        let Some(mut clip) = project.new_media_clip(media, 0.0) else { continue };
        let want = sb.shots[*i].seconds;
        if gen.info.duration > want + 0.05 {
            clip.duration = want;
        }
        let id = clip.id;
        project.append_to_main(clip);
        if let Some(c) = project.clip(id) {
            let caption = sb.shots[*i].caption.trim();
            if !caption.is_empty() {
                segments.push(Segment { start: c.start, end: c.end(), text: caption.to_string() });
            }
        }
        if !report.clips.is_empty() {
            let _ = project.set_transition(id, Some(Transition { kind: TransitionKind::Dissolve, duration: DISSOLVE }));
        }
        report.clips.push(id);
    }
    let end = project.tracks[main].end();
    report.duration = end - t0;

    if !segments.is_empty() {
        let style = captions::caption_styles().into_iter().next().map(|(_, s)| s).unwrap_or_default();
        let y = if sb.aspect.ratio().0 < sb.aspect.ratio().1 { 0.30 } else { 0.36 };
        let (_, n) = captions::apply_captions(project, &segments, &Layout { style, max_words: 12, y, replace: false });
        report.captions = n;
    }

    if let Some((path, info)) = music {
        let media = project.add_media(info.into_media_item(path));
        if let Some(mut clip) = project.new_media_clip(media, t0) {
            clip.start = t0;
            clip.duration = clip.duration.min(report.duration).max(crate::model::MIN_CLIP_LEN);
            clip.audio_role = AudioRole::Music;
            clip.volume = 0.8;
            clip.fade_in = 0.5_f64.min(clip.duration / 4.0);
            clip.fade_out = 1.5_f64.min(clip.duration / 3.0);
            report.music = Some(clip.id);
            project.place_clip(clip, None);
        }
        let opts = crate::smart_audio::DuckOptions::from_mix(&project.audio);
        let paths: Vec<(Id, PathBuf)> = project.media.iter().map(|m| (m.id, m.path.clone())).collect();
        let by_id = |id: Id| paths.iter().find(|(m, _)| *m == id).and_then(|(_, p)| peaks_for(p));
        report.ducked = crate::smart_audio::auto_duck(project, &by_id, &opts).is_ok();
    }
    project.audio.loudness = Some(LOUDNESS);
    report
}

/// Assembles either kind of storyboard: music videos cut on their slots, others in order.
pub fn assemble_any(
    project: &mut Project,
    sb: &Storyboard,
    rendered: &[(usize, Generated)],
    music: Option<(PathBuf, ProbeInfo)>,
    peaks_for: &dyn Fn(&Path) -> Option<Arc<Vec<f32>>>,
) -> AssembleReport {
    if sb.is_music_video() {
        super::music_video::assemble(project, sb, rendered)
    } else {
        assemble(project, sb, rendered, music, peaks_for)
    }
}

/// Waveform peaks loaded with FFmpeg, for ducking outside the editor (MCP, workers).
pub fn ffmpeg_peaks(path: &Path) -> Option<Arc<Vec<f32>>> {
    ffmpeg::waveform(path).ok().map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ffmpeg::ProbeInfo;
    use crate::model::{MediaKind, TrackKind};

    fn plan_json() -> Value {
        json!({
            "title": "Morning at Ember Coffee",
            "style": "warm golden light, shallow depth of field, 35mm film look",
            "script": "Your morning, slowed down.",
            "music": "warm acoustic, 90 bpm",
            "shots": [
                { "description": "Barista pours latte art", "motion": "slow push-in", "caption": "Slow mornings", "seconds": 6 },
                { "description": "Steam over a cup by the window", "motion": "steam rises", "caption": "", "seconds": 6 },
                { "description": "", "motion": "", "caption": "dropped", "seconds": 3 },
                { "description": "Friends laughing at a table", "motion": "handheld drift", "caption": "Visit Ember today", "seconds": 18 }
            ]
        })
    }

    #[test]
    fn plan_is_cleaned_up() {
        let sb = parse_plan(&plan_json(), "coffee ad", 30.0, Aspect::Portrait).unwrap();
        assert_eq!(sb.shots.len(), 3, "empty shots are dropped");
        assert!(sb.shots.iter().all(|s| (MIN_SHOT..=MAX_SHOT).contains(&s.seconds)));
        assert_eq!(sb.shots[2].seconds, MAX_SHOT, "long shots are clamped");
        assert!(frame_prompt(&sb, 0, true).contains("same characters"));
        assert!(frame_prompt(&sb, 0, false).contains("35mm film look"));
        assert!(plan_messages("x", 30.0, Aspect::Portrait)[1]["content"].as_str().unwrap().contains("vertical"));
    }

    fn generated(path: &str, secs: f64) -> Generated {
        Generated {
            path: path.into(),
            info: ProbeInfo { kind: MediaKind::Video, duration: secs, width: 720, height: 1280, fps: 24.0, has_audio: false },
            cost: Some(0.1),
            job_id: String::new(),
        }
    }

    #[test]
    fn assembles_in_order_with_dissolves_captions_music_and_loudness() {
        let mut sb = parse_plan(&plan_json(), "coffee ad", 30.0, Aspect::Portrait).unwrap();
        sb.shots[0].seconds = 4.0;
        let mut p = Project::default();
        // Rendered out of order; shot 1 (index 1) was not approved.
        let rendered = vec![(2, generated("C:/sb/s3.mp4", 8.0)), (0, generated("C:/sb/s1.mp4", 6.0))];
        let music = ProbeInfo { kind: MediaKind::Audio, duration: 120.0, width: 0, height: 0, fps: 0.0, has_audio: true };
        let r = assemble(&mut p, &sb, &rendered, Some(("C:/sb/music.mp3".into(), music)), &|_| None);
        assert_eq!(p.aspect, Aspect::Portrait);
        assert_eq!(r.clips.len(), 2);
        let a = p.clip(r.clips[0]).unwrap();
        let b = p.clip(r.clips[1]).unwrap();
        assert!(a.media_id().and_then(|m| p.media(m)).unwrap().path.ends_with("s1.mp4"), "storyboard order");
        assert_eq!(a.duration, 4.0, "trimmed to the planned length");
        assert_eq!(b.start, a.end());
        assert!(b.transition_in.is_some_and(|t| t.kind == TransitionKind::Dissolve));
        assert!(a.transition_in.is_none());
        assert_eq!(r.captions, 2);
        assert!(p.tracks.iter().any(|t| t.captions && t.kind == TrackKind::Text));
        let m = p.clip(r.music.unwrap()).unwrap();
        assert_eq!(m.audio_role, AudioRole::Music);
        assert!((m.duration - r.duration).abs() < 1e-9, "music trimmed to the video");
        assert_eq!(p.audio.loudness, Some(LOUDNESS));
    }


    /// Real FFmpeg: assemble two synthetic "generated" shots plus a tone as music and export.
    #[test]
    fn assembled_storyboard_exports() {
        let dir = std::env::temp_dir().join(format!("rc-sb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        let mut rendered = Vec::new();
        for (i, c) in ["red", "blue"].iter().enumerate() {
            let p = dir.join(format!("s{i}.mp4"));
            if !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c={c}:s=720x1280:d=4:r=24"), "-pix_fmt", "yuv420p", p.to_str().unwrap()]) {
                eprintln!("ffmpeg not available; skipping");
                return;
            }
            rendered.push((i, Generated { info: ffmpeg::probe(&p).unwrap(), path: p, cost: None, job_id: String::new() }));
        }
        let music = dir.join("m.wav");
        assert!(ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=f=440:d=20", music.to_str().unwrap()]));
        let mut sb = parse_plan(&plan_json(), "coffee ad", 30.0, Aspect::Portrait).unwrap();
        sb.shots.truncate(2);
        sb.shots[0].seconds = 3.0;
        let mut p = Project::default();
        let info = ffmpeg::probe(&music).unwrap();
        let r = assemble(&mut p, &sb, &rendered, Some((music.clone(), info)), &ffmpeg_peaks);
        assert_eq!(r.clips.len(), 2);
        let out = dir.join("out.mp4");
        let settings = crate::export::ExportSettings { path: out.clone(), short_side: 360, fps: 24, crf: 30, preset: "ultrafast".into() };
        crate::export::run(&p, &settings, &AtomicBool::new(false), |_| {}).unwrap();
        let got = ffmpeg::probe(&out).unwrap();
        assert!((got.duration - r.duration).abs() < 0.3, "{} vs {}", got.duration, r.duration);
        assert!(got.has_audio);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cast_members_follow_the_shot_description() {
        let mut sb = parse_plan(&plan_json(), "ad", 30.0, Aspect::Landscape).unwrap();
        sb.cast = vec![CastMember { name: "Aidan B.".into(), photo: "a.png".into() }, CastMember { name: "Aishyla".into(), photo: "b.png".into() }];
        sb.shots[0].description = "Aidan signs a contract while Aishyla watches".into();
        sb.shots[1].description = "Aishyla's phone glows on a marble table".into();
        sb.shots[2].description = "Wide shot of the glass tower at dawn; Aidanson Ltd sign".into();
        let names = |i: usize| sb.cast_in(i).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(0), vec!["Aidan B.", "Aishyla"]);
        assert_eq!(names(1), vec!["Aishyla"], "possessive still counts");
        assert!(names(2).is_empty(), "whole words only");
        let p = cast_frame_prompt(&sb, 0, &names(0), true);
        assert!(p.contains("Reference image 1 shows Aidan B.") && p.contains("Reference image 2 shows Aishyla") && p.contains("last reference image"));
        sb.shots[0].no_lead = true;
        assert!(sb.cast_in(0).is_empty());
    }

    #[test]
    fn storyboard_roundtrips_and_reference_is_another_shot() {
        let mut sb = parse_plan(&plan_json(), "coffee ad", 30.0, Aspect::Portrait).unwrap();
        sb.shots[1].frame = Some("b.png".into());
        assert_eq!(reference_for(&sb, 0), Some("b.png".into()));
        assert_eq!(reference_for(&sb, 1), None);
        sb.shots[1].approved = true;
        assert_eq!(sb.approved(), vec![1]);
        sb.swap(0, 1);
        assert_eq!(sb.approved(), vec![0]);
        let json = serde_json::to_string(&sb).unwrap();
        assert_eq!(serde_json::from_str::<Storyboard>(&json).unwrap(), sb);
    }
}
