//! AI images through OpenRouter's Image API (`POST /api/v1/images`): thumbnails,
//! title cards, lower-third backdrops, transparent stickers and background plates
//! (outpainting a clip's frame to the canvas shape).

use super::openrouter::Client;
use crate::media::ffmpeg::ProbeInfo;
use crate::model::{Clip, Id, MediaKind, Project, TrackKind, Transform};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-3.1-flash-image";
/// Used when a transparent background is needed and the chosen model can't do it.
pub const DEFAULT_TRANSPARENT_MODEL: &str = "openai/gpt-image-1-mini";

#[derive(Clone, Debug, PartialEq)]
pub struct ImageModel {
    pub id: String,
    pub name: String,
    pub reference_input: bool,
    pub aspect_ratios: Vec<String>,
    pub resolutions: Vec<String>,
    pub transparent: bool,
}

impl ImageModel {
    /// The supported ratio closest to `w:h` ("auto" if the model lists no ratios).
    pub fn pick_aspect(&self, w: f64, h: f64) -> Option<String> {
        let target = (w / h.max(1e-9)).ln();
        self.aspect_ratios
            .iter()
            .filter_map(|a| {
                let (x, y) = a.split_once(':')?;
                let r = (x.parse::<f64>().ok()? / y.parse::<f64>().ok()?).ln();
                Some((a.clone(), (r - target).abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|a| a.0)
    }

    pub fn pick_resolution(&self) -> Option<String> {
        ["1K", "1.5K", "2K", "768", "512"].iter().find(|r| self.resolutions.iter().any(|x| x == *r)).map(|r| r.to_string())
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub fn parse_models(v: &Value) -> Vec<ImageModel> {
    let mut out: Vec<ImageModel> = v["data"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let sp = &m["supported_parameters"];
            Some(ImageModel {
                name: m["name"].as_str().unwrap_or(&id).to_string(),
                reference_input: m["architecture"]["input_modalities"].as_array().is_some_and(|i| i.iter().any(|x| x == "image")),
                aspect_ratios: strings(&sp["aspect_ratio"]["values"]).into_iter().filter(|a| a != "auto").collect(),
                resolutions: strings(&sp["resolution"]["values"]),
                transparent: strings(&sp["background"]["values"]).iter().any(|b| b == "transparent"),
                id,
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn list_models(client: &Client) -> Result<Vec<ImageModel>> {
    Ok(parse_models(&client.get_json("/images/models")?))
}

#[derive(Clone, Debug)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub transparent: bool,
    /// Reference images: (bytes, MIME type).
    pub references: Vec<(Vec<u8>, &'static str)>,
}

impl ImageRequest {
    pub fn body(&self) -> Value {
        let mut b = json!({ "model": self.model, "prompt": self.prompt, "n": 1 });
        if let Some(a) = &self.aspect_ratio {
            b["aspect_ratio"] = json!(a);
        }
        if let Some(r) = &self.resolution {
            b["resolution"] = json!(r);
        }
        if self.transparent {
            b["background"] = json!("transparent");
            b["output_format"] = json!("png");
        }
        if !self.references.is_empty() {
            b["input_references"] = Value::Array(
                self.references
                    .iter()
                    .map(|(bytes, mime)| {
                        json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{}", crate::util::base64(bytes)) } })
                    })
                    .collect(),
            );
        }
        b
    }
}

#[derive(Clone, Debug)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    pub cost: Option<f64>,
    /// Model that made it (for the cost ledger).
    pub model: String,
}

fn decode_base64(s: &str) -> Result<Vec<u8>> {
    let s = s.split_once("base64,").map_or(s, |(_, d)| d);
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => bail!("invalid base64 in image data"),
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

/// Generates one image. Not retried automatically (it is billed).
pub fn generate(client: &Client, req: &ImageRequest) -> Result<GeneratedImage> {
    let v = client.post_once("/images", &req.body())?;
    let item = v["data"].as_array().and_then(|a| a.first()).ok_or_else(|| {
        anyhow!("{} returned no image ({}). Some models refuse certain content; try another model.", req.model, v["error"]["message"].as_str().unwrap_or("no details"))
    })?;
    let b64 = item["b64_json"].as_str().or_else(|| item["url"].as_str()).ok_or_else(|| anyhow!("the image response had no data"))?;
    let bytes = decode_base64(b64)?;
    if bytes.len() < 100 {
        bail!("the generated image is empty");
    }
    let extension = match item["media_type"].as_str().unwrap_or("") {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ if bytes.starts_with(&[0xFF, 0xD8]) => "jpg",
        _ if bytes.starts_with(b"RIFF") => "webp",
        _ => "png",
    };
    Ok(GeneratedImage { bytes, extension, cost: v["usage"]["cost"].as_f64(), model: req.model.clone() })
}

/// Saves an image under `dir` with a unique name; returns the path.
pub fn save(img: &GeneratedImage, dir: &Path, stem: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut path = dir.join(format!("{stem}.{}", img.extension));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{}", img.extension));
        n += 1;
    }
    std::fs::write(&path, &img.bytes)?;
    if let Some(usd) = img.cost {
        crate::economics::record(crate::economics::Spend { kind: "image", model: &img.model, usd, units: 1.0, unit: "image", output: Some(&path), ..Default::default() });
    }
    Ok(path)
}

// ── prompts ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbStyle {
    Bold,
    Clean,
    Dramatic,
}

impl ThumbStyle {
    pub const ALL: [ThumbStyle; 3] = [ThumbStyle::Bold, ThumbStyle::Clean, ThumbStyle::Dramatic];
    pub fn label(self) -> &'static str {
        match self {
            ThumbStyle::Bold => "Bold YouTube",
            ThumbStyle::Clean => "Clean & minimal",
            ThumbStyle::Dramatic => "Dramatic / cinematic",
        }
    }
    pub fn parse(s: &str) -> Option<ThumbStyle> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bold" | "youtube" => Some(ThumbStyle::Bold),
            "clean" | "minimal" => Some(ThumbStyle::Clean),
            "dramatic" | "cinematic" => Some(ThumbStyle::Dramatic),
            _ => None,
        }
    }
    fn direction(self) -> &'static str {
        match self {
            ThumbStyle::Bold => "punchy YouTube thumbnail: vivid saturated colours, high contrast, the subject large and expressive, huge bold headline text with a thick outline",
            ThumbStyle::Clean => "clean, modern thumbnail: generous space, soft light, one elegant bold headline, minimal clutter",
            ThumbStyle::Dramatic => "cinematic, dramatic thumbnail: moody lighting, strong depth, a bold headline with a subtle glow",
        }
    }
}

pub fn thumbnail_prompt(headline: &str, style: ThumbStyle, extra: &str) -> String {
    let mut p = format!(
        "Create a {} using the reference image as the main photo; keep the people and scene recognisable. ",
        style.direction()
    );
    if headline.trim().is_empty() {
        p += "Do not add any text. ";
    } else {
        p += &format!("Add this headline, spelled exactly: \"{}\". Make it readable at small sizes. ", headline.trim());
    }
    if !extra.trim().is_empty() {
        p += extra.trim();
    }
    p
}

pub fn outpaint_prompt(aspect: &str) -> String {
    format!(
        "Extend this image to fill a {aspect} frame. Keep the original picture unchanged and centred; continue the \
         scene naturally into the new areas (same lighting, perspective and style), as a seamless background plate. \
         The original may contain overlaid graphics (titles, captions, logos, stickers, buttons such as Subscribe): \
         never copy, continue or repeat any of them in the new areas; fill those with the scene's environment only \
         (walls, sky, floor, fabric, bokeh). No text, no borders, no frames, no blur bands."
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StillKind {
    TitleCard,
    LowerThird,
    Sticker,
}

impl StillKind {
    pub const ALL: [StillKind; 3] = [StillKind::TitleCard, StillKind::LowerThird, StillKind::Sticker];
    pub fn label(self) -> &'static str {
        match self {
            StillKind::TitleCard => "Title card",
            StillKind::LowerThird => "Lower-third backdrop",
            StillKind::Sticker => "Sticker",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            StillKind::TitleCard => "title_card",
            StillKind::LowerThird => "lower_third",
            StillKind::Sticker => "sticker",
        }
    }
    pub fn parse(s: &str) -> Option<StillKind> {
        StillKind::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s.trim()))
    }
    pub fn transparent(self) -> bool {
        self != StillKind::TitleCard
    }
    /// Seconds on the timeline.
    pub fn default_duration(self) -> f64 {
        match self {
            StillKind::TitleCard => 3.0,
            StillKind::LowerThird => 5.0,
            StillKind::Sticker => 3.0,
        }
    }
    pub fn prompt(self, what: &str) -> String {
        let what = what.trim();
        match self {
            StillKind::TitleCard => format!("A full-frame video title card: {what}. Bold, legible typography as the focus, polished design, no watermark."),
            StillKind::LowerThird => format!(
                "A lower-third graphic backdrop for a video: {what}. A clean horizontal bar or shape occupying the lower part, \
                 fully transparent background everywhere else, no text unless asked, no watermark."
            ),
            StillKind::Sticker => format!("A die-cut sticker: {what}. Isolated subject with a thin white outline, fully transparent background, no text unless asked."),
        }
    }
}

// ── jobs (shared by the editor and the MCP server) ───────────────────────────

/// One image to make.
#[derive(Clone, Debug)]
pub enum Job {
    /// A thumbnail from the composited frame at `time`.
    Thumbnail { time: f64, headline: String, style: ThumbStyle, extra: String },
    /// A title card, lower-third backdrop or sticker from a prompt.
    Still { kind: StillKind, prompt: String },
    /// A canvas-shaped background plate outpainted from a clip's middle frame.
    Plate { clip: Id },
}

impl Job {
    pub fn label(&self) -> String {
        match self {
            Job::Thumbnail { headline, .. } if headline.trim().is_empty() => "Thumbnail".into(),
            Job::Thumbnail { headline, .. } => format!("Thumbnail “{}”", headline.trim()),
            Job::Still { kind, prompt } => format!("{} “{}”", kind.label(), super::video_gen::slug(prompt).replace('-', " ")),
            Job::Plate { .. } => "Background fill".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Output {
    pub path: PathBuf,
    pub info: ProbeInfo,
    pub cost: Option<f64>,
    pub model: String,
}

/// Builds the request for `job` (frames are grabbed here) without sending it.
pub fn build_request(project: &Project, job: &Job, model: &str, models: &[ImageModel]) -> Result<(ImageRequest, String)> {
    let (cw, ch) = project.aspect.size_for(1080);
    let (cw, ch) = (cw as f64, ch as f64);
    let find = |id: &str| models.iter().find(|m| m.id == id).cloned();
    let mut chosen = find(model);
    let (prompt, aspect_of, transparent, references, stem) = match job {
        Job::Thumbnail { time, headline, style, extra } => {
            let png = crate::export::render_frame_png(project, *time, 720)?;
            // YouTube thumbnails are 16:9; a vertical project gets a vertical one.
            let shape = if cw >= ch { (16.0, 9.0) } else { (cw, ch) };
            let stem = format!("thumbnail-{}", super::video_gen::slug(if headline.trim().is_empty() { "frame" } else { headline }));
            (thumbnail_prompt(headline, *style, extra), shape, false, vec![(png, "image/png")], stem)
        }
        Job::Still { kind, prompt } => {
            if prompt.trim().is_empty() {
                bail!("describe the {} first", kind.label().to_lowercase());
            }
            let shape = match kind {
                StillKind::TitleCard => (cw, ch),
                StillKind::LowerThird => (4.0, 1.0),
                StillKind::Sticker => (1.0, 1.0),
            };
            (kind.prompt(prompt), shape, kind.transparent(), Vec::new(), format!("{}-{}", kind.name().replace('_', "-"), super::video_gen::slug(prompt)))
        }
        Job::Plate { clip } => {
            let clip = project.clip(*clip).ok_or_else(|| anyhow!("clip was removed"))?;
            let media = clip.media_id().and_then(|m| project.media(m)).ok_or_else(|| anyhow!("only video and image clips can get a background"))?;
            let t = if media.kind == MediaKind::Image { 0.0 } else { clip.source_in + clip.duration * clip.speed / 2.0 };
            let jpg = super::video_gen::frame_jpeg(&media.path, t, 1280)?;
            let stem = format!("background-{}", super::video_gen::slug(&media.name));
            (String::new(), (cw, ch), false, vec![(jpg, "image/jpeg")], stem)
        }
    };
    if transparent && !chosen.as_ref().is_some_and(|m| m.transparent) {
        chosen = find(DEFAULT_TRANSPARENT_MODEL).or_else(|| models.iter().find(|m| m.transparent).cloned());
        if chosen.is_none() && !models.is_empty() {
            bail!("no image model on OpenRouter offers transparent backgrounds right now");
        }
    }
    if !references.is_empty() && chosen.as_ref().is_some_and(|m| !m.reference_input) {
        bail!("{model} can't take a reference image; pick a model that edits images (e.g. {DEFAULT_IMAGE_MODEL})");
    }
    let model_id = chosen.as_ref().map_or_else(|| model.to_string(), |m| m.id.clone());
    let aspect_ratio = chosen.as_ref().and_then(|m| m.pick_aspect(aspect_of.0, aspect_of.1));
    let prompt = if prompt.is_empty() { outpaint_prompt(aspect_ratio.as_deref().unwrap_or("vertical")) } else { prompt };
    let req = ImageRequest {
        model: model_id,
        prompt,
        aspect_ratio,
        resolution: chosen.as_ref().and_then(ImageModel::pick_resolution),
        transparent,
        references,
    };
    Ok((req, stem))
}

/// Makes the image for `job` and saves it under `dir`.
pub fn run(client: &Client, project: &Project, job: &Job, model: &str, dir: &Path) -> Result<Output> {
    let models = list_models(client).unwrap_or_default();
    let (req, stem) = build_request(project, job, model, &models)?;
    let img = generate(client, &req)?;
    let path = save(&img, dir, &stem)?;
    let info = crate::media::ffmpeg::probe(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Output { path, info, cost: img.cost, model: req.model })
}

/// Whether a visual clip leaves part of the canvas uncovered (so a plate would show).
pub fn needs_plate(project: &Project, clip: &Clip) -> bool {
    let Some(media) = clip.media_id().and_then(|m| project.media(m)) else { return false };
    if media.kind == MediaKind::Audio || media.width == 0 || media.height == 0 {
        return false;
    }
    let (cw, ch) = project.aspect.size_for(1080);
    let (w, h) = crate::render::layer_size(media.width as f64, media.height as f64, cw as f64, ch as f64, clip.transform.scale as f64);
    let (x, y) = (clip.transform.x.abs() as f64 * cw as f64, clip.transform.y.abs() as f64 * ch as f64);
    w + 2.0 * x < cw as f64 - 2.0 || h + 2.0 * y < ch as f64 - 2.0 || clip.transform.rotation.abs() > 0.5
}

/// Adds the generated plate to the library and puts it behind `clip`.
pub fn apply_plate(project: &mut Project, clip: Id, out: &Output) -> Option<Id> {
    project.clip(clip)?;
    let media = project.add_media(out.info.clone().into_media_item(out.path.clone()));
    project.clip_mut(clip)?.background_plate = Some(media);
    Some(media)
}

/// Adds a still to the library and drops it on an overlay video track (never the
/// main track) at `start`. Returns (media id, clip id).
pub fn place_still(project: &mut Project, kind: StillKind, out: &Output, start: f64) -> (Id, Option<Id>) {
    let media = project.add_media(out.info.clone().into_media_item(out.path.clone()));
    let Some(mut clip) = project.new_media_clip(media, start) else { return (media, None) };
    clip.duration = kind.default_duration();
    clip.transform = match kind {
        StillKind::TitleCard => Transform::default(),
        StillKind::LowerThird => Transform { y: 0.32, scale: 0.9, ..Transform::default() },
        StillKind::Sticker => Transform { x: 0.25, y: -0.2, scale: 0.35, ..Transform::default() },
    };
    let id = clip.id;
    let (s, e) = (clip.start, clip.end());
    let main = project.main_track();
    let overlay = (0..project.tracks.len())
        .rev()
        .find(|&i| i != main && project.tracks[i].kind == TrackKind::Video && !project.tracks[i].locked && project.fits(i, s, e, None));
    let ti = match overlay {
        Some(i) => i,
        None => {
            let main = project.main_track();
            project.insert_track(TrackKind::Video, Some(main))
        }
    };
    project.tracks[ti].clips.push(clip);
    project.sort_track(ti);
    (media, Some(id))
}

/// Copies the project's thumbnail next to an exported video as `<stem>-thumbnail.<ext>`.
pub fn copy_thumbnail_next_to(thumbnail: &Path, video: &Path) -> Result<PathBuf> {
    let ext = thumbnail.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    let dest = video.with_file_name(format!("{stem}-thumbnail.{ext}"));
    std::fs::copy(thumbnail, &dest).with_context(|| format!("copying the thumbnail to {}", dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: u32, h: u32) -> ProbeInfo {
        ProbeInfo { kind: MediaKind::Image, duration: 0.0, width: w, height: h, fps: 0.0, has_audio: false }
    }

    #[test]
    fn stills_go_on_an_overlay_track() {
        let mut p = Project::default();
        let out = Output { path: "C:/x/sticker.png".into(), info: info(1024, 1024), cost: None, model: "m".into() };
        let (_, clip) = place_still(&mut p, StillKind::Sticker, &out, 2.0);
        let clip = clip.unwrap();
        let main = p.main_track();
        let (ti, _) = p.locate(clip).unwrap();
        assert_ne!(ti, main, "stills must not land on the main track");
        let c = p.clip(clip).unwrap();
        assert_eq!((c.start, c.duration), (2.0, 3.0));
        assert!((c.transform.scale - 0.35).abs() < 1e-6);
        // A second, overlapping still gets another overlay track; a later one reuses the first.
        let (_, b) = place_still(&mut p, StillKind::LowerThird, &out, 3.0);
        assert_ne!(p.locate(b.unwrap()).unwrap().0, p.locate(clip).unwrap().0);
        let (_, c3) = place_still(&mut p, StillKind::TitleCard, &out, 20.0);
        assert_ne!(p.locate(c3.unwrap()).unwrap().0, p.main_track());
    }

    #[test]
    fn plate_needed_only_when_canvas_is_uncovered() {
        let mut p = Project::default();
        let m = p.add_media(crate::model::MediaItem {
            id: 0, path: "a.mp4".into(), name: "a".into(), kind: MediaKind::Video,
            duration: 10.0, width: 1920, height: 1080, fps: 30.0, has_audio: false,
        });
        let c = p.new_media_clip(m, 0.0).unwrap();
        p.append_to_main(c.clone());
        p.aspect = crate::model::Aspect::Portrait;
        assert!(needs_plate(&p, p.clip(c.id).unwrap()));
        p.aspect = crate::model::Aspect::Landscape;
        assert!(!needs_plate(&p, p.clip(c.id).unwrap()));
        let out = Output { path: "C:/x/bg.png".into(), info: info(1080, 1920), cost: None, model: "m".into() };
        let plate = apply_plate(&mut p, c.id, &out).unwrap();
        assert_eq!(p.clip(c.id).unwrap().background_plate, Some(plate));
    }

    #[test]
    fn transparent_still_falls_back_to_a_transparent_model() {
        let models = parse_models(&json!({ "data": [
            { "id": "google/gemini-3.1-flash-image", "architecture": { "input_modalities": ["text", "image"] },
              "supported_parameters": { "aspect_ratio": { "values": ["1:1", "16:9", "4:1"] } } },
            { "id": "openai/gpt-image-1-mini", "architecture": { "input_modalities": ["text", "image"] },
              "supported_parameters": { "aspect_ratio": { "values": ["1:1", "3:2"] }, "background": { "values": ["auto", "transparent"] } } }
        ]}));
        let p = Project::default();
        let job = Job::Still { kind: StillKind::Sticker, prompt: "a red heart".into() };
        let (req, stem) = build_request(&p, &job, DEFAULT_IMAGE_MODEL, &models).unwrap();
        assert_eq!(req.model, DEFAULT_TRANSPARENT_MODEL);
        assert!(req.transparent);
        assert_eq!(req.aspect_ratio.as_deref(), Some("1:1"));
        assert!(stem.starts_with("sticker-"));
        let job = Job::Still { kind: StillKind::LowerThird, prompt: "news bar".into() };
        let (req, _) = build_request(&p, &job, "openai/gpt-image-1-mini", &models).unwrap();
        assert_eq!(req.model, "openai/gpt-image-1-mini");
        let job = Job::Still { kind: StillKind::TitleCard, prompt: "Day 1".into() };
        let (req, _) = build_request(&p, &job, DEFAULT_IMAGE_MODEL, &models).unwrap();
        assert_eq!((req.model.as_str(), req.transparent, req.aspect_ratio.as_deref()), (DEFAULT_IMAGE_MODEL, false, Some("16:9")));
    }


    /// Real FFmpeg render: a landscape clip in a 9:16 canvas with a solid-red plate
    /// shows red in the bars (top) and the clip in the middle.
    #[test]
    fn plate_renders_in_the_bars() {
        use crate::media::ffmpeg;
        let dir = std::env::temp_dir().join(format!("rc-plate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (clip_path, plate_path) = (dir.join("clip.mp4"), dir.join("plate.png"));
        let ok = |args: &[&str]| ffmpeg::command(&ffmpeg::ffmpeg_bin()).args(args).output().is_ok_and(|o| o.status.success());
        if !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=blue:s=1920x1080:d=2", "-pix_fmt", "yuv420p", clip_path.to_str().unwrap()])
            || !ok(&["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=red:s=1080x1920", "-frames:v", "1", plate_path.to_str().unwrap()])
        {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let mut p = Project::default();
        p.aspect = crate::model::Aspect::Portrait;
        let m = p.add_media(ffmpeg::probe(&clip_path).unwrap().into_media_item(clip_path.clone()));
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.append_to_main(c);
        let out = Output { path: plate_path.clone(), info: ffmpeg::probe(&plate_path).unwrap(), cost: None, model: "m".into() };
        apply_plate(&mut p, id, &out).unwrap();
        let png = crate::export::render_frame_png(&p, 1.0, 360).unwrap();
        let frame = dir.join("frame.png");
        std::fs::write(&frame, png).unwrap();
        let rgba = ffmpeg::extract_frame(&frame, 0.0, 36, 64).unwrap();
        let px = |x: usize, y: usize| { let i = (y * 36 + x) * 4; (rgba[i], rgba[i + 1], rgba[i + 2]) };
        let (top, mid) = (px(18, 3), px(18, 32));
        assert!(top.0 > 180 && top.2 < 80, "bars should show the red plate, got {top:?}");
        assert!(mid.2 > 180 && mid.0 < 80, "the clip should be on top, got {mid:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumbnail_copied_next_to_video() {
        let dir = std::env::temp_dir().join(format!("rc-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let thumb = dir.join("t.png");
        std::fs::write(&thumb, b"png").unwrap();
        let dest = copy_thumbnail_next_to(&thumb, &dir.join("My Vlog.mp4")).unwrap();
        assert_eq!(dest.file_name().unwrap(), "My Vlog-thumbnail.png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_models_and_picks_ratios() {
        let v = json!({ "data": [
            { "id": "google/gemini-3.1-flash-image", "architecture": { "input_modalities": ["image", "text"] },
              "supported_parameters": { "resolution": { "values": ["512", "1K", "2K"] },
                                        "aspect_ratio": { "values": ["1:1", "9:16", "16:9", "4:5"] } } },
            { "id": "openai/gpt-image-1-mini", "architecture": { "input_modalities": ["image", "text"] },
              "supported_parameters": { "aspect_ratio": { "values": ["1:1", "3:2", "2:3", "auto"] },
                                        "background": { "values": ["auto", "transparent", "opaque"] } } }
        ]});
        let m = parse_models(&v);
        let gem = &m[0];
        assert!(gem.reference_input && !gem.transparent);
        assert_eq!(gem.pick_aspect(1080.0, 1920.0).as_deref(), Some("9:16"));
        assert_eq!(gem.pick_aspect(1080.0, 1350.0).as_deref(), Some("4:5"));
        assert_eq!(gem.pick_resolution().as_deref(), Some("1K"));
        let gpt = &m[1];
        assert!(gpt.transparent);
        assert!(!gpt.aspect_ratios.contains(&"auto".to_string()));
        assert_eq!(gpt.pick_aspect(1920.0, 1080.0).as_deref(), Some("3:2"));
    }

    #[test]
    fn request_body() {
        let req = ImageRequest {
            model: "m".into(),
            prompt: "p".into(),
            aspect_ratio: Some("16:9".into()),
            resolution: None,
            transparent: true,
            references: vec![(vec![0xFF, 0xD8, 0xFF], "image/jpeg")],
        };
        let b = req.body();
        assert_eq!(b["background"], "transparent");
        assert_eq!(b["output_format"], "png");
        assert_eq!(b["input_references"][0]["image_url"]["url"], "data:image/jpeg;base64,/9j/");
        assert!(b.get("resolution").is_none());
    }

    #[test]
    fn base64_roundtrip() {
        for data in [&b"hello world"[..], b"\x89PNG\r\n\x1a\n\x00\x01", b"ab"] {
            assert_eq!(decode_base64(&crate::util::base64(data)).unwrap(), data);
        }
        assert_eq!(decode_base64("data:image/png;base64,aGk=").unwrap(), b"hi");
    }

    /// PAID, manual: `LOGO_REF=ref.jpg LOGO_OUT=dir cargo test --lib make_logos -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn make_logos() {
        let (Ok(reference), Ok(out)) = (std::env::var("LOGO_REF"), std::env::var("LOGO_OUT")) else { return };
        let settings = crate::ai::openrouter::AiSettings::load();
        let client = crate::ai::openrouter::Client::from_settings(&settings).unwrap();
        let bytes = std::fs::read(&reference).unwrap();
        let style = "Match the visual style of the reference logo exactly: polished, highly detailed 3D gold metal with a brushed, slightly textured surface, \
an ornate gold crest frame with baroque scrollwork and a gold crown on top, warm highlights and deep shadows. \
Do NOT copy the reference's words. Real transparent background (alpha), no checkerboard pattern, no background colour, no extra text.";
        let jobs = [
            ("rustcut-icon", format!("A square application icon for 'RustCut', a video editing app. {style} Inside the crest: a bold, chunky 3D gold monogram of the letters 'RC' \
with a small gold play-button triangle cut into the C. Large, simple, bold shapes that stay readable at 32 pixels; centred, filling about 90% of the square.")),
            ("rustcut-logo", format!("The full logo for 'RustCut', a video editing app. {style} Inside the crest: the word 'RUSTCUT' in huge, bold 3D gold serif capital letters \
(spelled exactly R-U-S-T-C-U-T), and below it smaller gold capitals 'VIDEO EDITOR'. Centred, filling the frame.")),
        ];
        let mut total = 0.0;
        for (stem, prompt) in jobs {
            let req = ImageRequest { model: DEFAULT_TRANSPARENT_MODEL.into(), prompt, aspect_ratio: Some("1:1".into()), resolution: None, transparent: true, references: vec![(bytes.clone(), "image/jpeg")] };
            let img = generate(&client, &req).unwrap();
            total += img.cost.unwrap_or(0.0);
            let path = save(&img, Path::new(&out), stem).unwrap();
            println!("{stem}: {} (${:.3})", path.display(), img.cost.unwrap_or(0.0));
        }
        println!("total ${total:.3}");
    }

    #[test]
    fn prompts() {
        assert!(thumbnail_prompt("I DELETED HIM", ThumbStyle::Bold, "").contains("\"I DELETED HIM\""));
        assert!(thumbnail_prompt("", ThumbStyle::Clean, "").contains("Do not add any text"));
        assert!(outpaint_prompt("9:16").contains("9:16 frame"));
        assert!(outpaint_prompt("16:9").contains("never copy, continue or repeat"));
        assert!(StillKind::Sticker.transparent() && !StillKind::TitleCard.transparent());
        assert_eq!(StillKind::parse("lower_third"), Some(StillKind::LowerThird));
    }
}
