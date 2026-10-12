//! AI video generation through OpenRouter's async video API (`/api/v1/videos`):
//! text-to-video B-roll, and image-to-video for extending or bridging shots.
//!
//! Jobs are submitted once (never retried — a duplicate would be billed twice),
//! polled until finished, then downloaded with the API key into a local folder.

use super::openrouter::Client;
use crate::media::ffmpeg::{self, ProbeInfo};
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A video model from `/api/v1/videos/models`.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoModel {
    pub id: String,
    pub name: String,
    pub durations: Vec<u32>,
    pub resolutions: Vec<String>,
    pub aspect_ratios: Vec<String>,
    pub first_frame: bool,
    pub last_frame: bool,
    /// `Some(true/false)` if the model can generate audio, `None` if unknown.
    pub audio: Option<bool>,
    pub pricing: Vec<(String, f64)>,
}

impl VideoModel {
    /// Generation models only (no editing/upscaling/avatar endpoints without durations).
    pub fn generates(&self) -> bool {
        !self.durations.is_empty()
    }

    pub fn default_resolution(&self) -> String {
        ["720p", "768p", "480p", "1080p"]
            .iter()
            .find(|r| self.resolutions.iter().any(|x| x == *r))
            .map(|r| r.to_string())
            .or_else(|| self.resolutions.first().cloned())
            .unwrap_or_else(|| "720p".into())
    }

    /// The supported duration closest to (and preferably not below) `wanted`.
    pub fn pick_duration(&self, wanted: f64) -> u32 {
        let mut d = self.durations.clone();
        d.sort_unstable();
        d.iter().copied().find(|x| *x as f64 >= wanted - 0.01).or_else(|| d.last().copied()).unwrap_or(5)
    }

    /// The supported aspect ratio closest to `w:h`.
    pub fn pick_aspect(&self, w: f64, h: f64) -> String {
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
            .unwrap_or_else(|| "16:9".into())
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub fn parse_models(v: &Value) -> Vec<VideoModel> {
    let mut models: Vec<VideoModel> = v["data"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let frames = strings(&m["supported_frame_images"]);
            let pricing = m["pricing_skus"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| {
                            let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))?;
                            Some((k.clone(), n))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(VideoModel {
                name: m["name"].as_str().unwrap_or(&id).to_string(),
                durations: m["supported_durations"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d.as_u64().map(|d| d as u32)).collect())
                    .unwrap_or_default(),
                resolutions: strings(&m["supported_resolutions"]),
                aspect_ratios: strings(&m["supported_aspect_ratios"]),
                first_frame: frames.iter().any(|f| f == "first_frame"),
                last_frame: frames.iter().any(|f| f == "last_frame"),
                audio: m["generate_audio"].as_bool(),
                pricing,
                id,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

pub fn list_models(client: &Client) -> Result<Vec<VideoModel>> {
    Ok(parse_models(&client.get_json("/videos/models")?).into_iter().filter(VideoModel::generates).collect())
}

/// Approximate output size for a resolution label and aspect ratio.
pub fn pixel_size(resolution: &str, aspect: &str) -> (u32, u32) {
    let short: u32 = match resolution {
        "360p" => 360,
        "480p" => 480,
        "720p" => 720,
        "768p" => 768,
        "1080p" | "1K" => 1080,
        "2K" => 1440,
        "4K" => 2160,
        r => r.trim_end_matches('p').parse().unwrap_or(720),
    };
    let (a, b) = aspect
        .split_once(':')
        .and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)))
        .unwrap_or((16.0, 9.0));
    let long = (short as f64 * a.max(b) / a.min(b)).round() as u32;
    if a >= b { (long, short) } else { (short, long) }
}

/// Estimated price in USD and whether the estimate is rough (token-priced models).
pub fn estimate_cost(m: &VideoModel, duration: u32, resolution: &str, aspect: &str, audio: bool, image_input: bool) -> Option<(f64, bool)> {
    const RES: [&str; 8] = ["360p", "480p", "720p", "768p", "1080p", "1k", "2k", "4k"];
    let res = resolution.to_ascii_lowercase();
    let score = |key: &str| -> Option<i32> {
        let k = key.to_ascii_lowercase();
        let per_second = k.contains("second") || k.starts_with("duration");
        if !per_second || ["reference", "continuation", "upscale", "megapixel", "minimum"].iter().any(|x| k.contains(x)) {
            return None;
        }
        let mut s = 0;
        if k.contains(&res) {
            s += 3;
        } else if RES.iter().any(|r| k.contains(r)) {
            s -= 10;
        }
        match (k.contains("image_to_video"), k.contains("text_to_video"), image_input) {
            (true, _, true) | (_, true, false) => s += 2,
            (true, _, false) | (_, true, true) => s -= 10,
            _ => {}
        }
        match (k.contains("without_audio"), k.contains("with_audio"), audio) {
            (true, _, false) => s += 2,
            (_, true, true) if !k.contains("without_audio") => s += 2,
            (true, _, true) => s -= 10,
            (_, true, false) if !k.contains("without_audio") => s -= 10,
            _ => {}
        }
        Some(s)
    };
    let best = m.pricing.iter().filter_map(|(k, v)| score(k).map(|s| (s, k, *v))).max_by_key(|(s, _, _)| *s);
    let image_fee = m
        .pricing
        .iter()
        .find(|(k, _)| image_input && k.contains("image_input"))
        .map_or(0.0, |(k, v)| if k.contains("cents") { v / 100.0 } else { *v });
    if let Some((_, key, v)) = best {
        let per_second = if key.contains("cents") { v / 100.0 } else { v };
        return Some((per_second * duration as f64 + image_fee, false));
    }
    // Token pricing (e.g. Seedance): ~ width × height × 24 fps × seconds / 1024 tokens.
    let token_key = |k: &str| k.starts_with("video_tokens") && !k.contains("video_input");
    let pick = m
        .pricing
        .iter()
        .filter(|(k, _)| token_key(k))
        .max_by_key(|(k, _)| {
            let k = k.to_ascii_lowercase();
            (k.contains(&res) as i32) * 2 + (!audio && k.contains("without_audio")) as i32
                - (RES.iter().any(|r| k.contains(r)) && !k.contains(&res)) as i32 * 10
        })?;
    let (w, h) = pixel_size(resolution, aspect);
    let tokens = w as f64 * h as f64 * 24.0 * duration as f64 / 1024.0;
    Some((tokens * pick.1 + image_fee, true))
}

/// What to generate.
#[derive(Clone, Debug)]
pub struct GenRequest {
    pub model: String,
    pub prompt: String,
    pub duration: u32,
    pub resolution: String,
    pub aspect_ratio: String,
    pub generate_audio: bool,
    /// Image the video must start on (JPEG or PNG bytes, with MIME type).
    pub first_frame: Option<(Vec<u8>, &'static str)>,
    /// Image the video must end on.
    pub last_frame: Option<(Vec<u8>, &'static str)>,
}

impl GenRequest {
    pub fn body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "prompt": self.prompt,
            "duration": self.duration,
            "resolution": self.resolution,
            "aspect_ratio": self.aspect_ratio,
            "generate_audio": self.generate_audio,
        });
        let frame = |(bytes, mime): &(Vec<u8>, &str), kind: &str| {
            json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{}", crate::util::base64(bytes)) },
                "frame_type": kind,
            })
        };
        let frames: Vec<Value> = [(&self.first_frame, "first_frame"), (&self.last_frame, "last_frame")]
            .into_iter()
            .filter_map(|(f, kind)| f.as_ref().map(|f| frame(f, kind)))
            .collect();
        if !frames.is_empty() {
            body["frame_images"] = Value::Array(frames);
        }
        body
    }
}

#[derive(Clone, Debug)]
pub struct Generated {
    pub path: PathBuf,
    pub info: ProbeInfo,
    pub cost: Option<f64>,
    pub job_id: String,
}

/// Submits a job, waits for it and downloads the result into `dir`. `Ok(None)` if
/// cancelled (the job may still finish and be billed on OpenRouter).
pub fn generate(
    client: &Client,
    req: &GenRequest,
    dir: &Path,
    file_stem: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<Option<Generated>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    progress(0.02, "Submitting…");
    let job = client.post_once("/videos", &req.body())?;
    let id = job["id"].as_str().ok_or_else(|| anyhow!("OpenRouter did not return a job id: {job}"))?.to_string();
    let started = Instant::now();
    let expected = 60.0 + 8.0 * req.duration as f32;
    let deadline = Duration::from_secs(25 * 60);
    let (urls, cost) = loop {
        for _ in 0..8 {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let status = client.get_json(&format!("/videos/{id}"))?;
        let state = status["status"].as_str().unwrap_or("unknown");
        let secs = started.elapsed().as_secs_f32();
        match state {
            "completed" => {
                let urls = strings(&status["unsigned_urls"]);
                break (urls, status["usage"]["cost"].as_f64());
            }
            "failed" | "cancelled" | "expired" => {
                let err = status["error"].as_str().unwrap_or("no details");
                bail!(
                    "{} {state}: {err}. Models differ in what they accept (e.g. photos of realistic people); try another model.",
                    req.model
                );
            }
            "pending" => progress(0.05, &format!("Queued… {secs:.0}s")),
            _ => progress((0.08 + 0.82 * secs / expected).min(0.9), &format!("Generating… {secs:.0}s")),
        }
        if started.elapsed() > deadline {
            bail!("gave up after 25 minutes (job {id} may still finish on OpenRouter)");
        }
    };
    progress(0.93, "Downloading…");
    let url = urls.first().cloned().unwrap_or_else(|| format!("/videos/{id}/content?index=0"));
    let path = unique_path(dir, file_stem, "mp4");
    client.download(&url, &path)?;
    let info = ffmpeg::probe(&path).context("the downloaded video could not be read")?;
    progress(1.0, "Done");
    crate::economics::record(crate::economics::Spend {
        kind: "video",
        model: &req.model,
        usd: cost.unwrap_or(0.0),
        units: info.duration,
        unit: "second",
        output: Some(&path),
        note: if cost.is_none() { "provider reported no cost" } else { "" },
        ..Default::default()
    });
    Ok(Some(Generated { path, info, cost, job_id: id }))
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let base = dir.join(format!("{stem}.{ext}"));
    if !base.exists() {
        return base;
    }
    (2..).map(|n| dir.join(format!("{stem}-{n}.{ext}"))).find(|p| !p.exists()).expect("unbounded")
}

/// A filesystem-friendly name from a prompt.
pub fn slug(text: &str) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() { "clip".into() } else { words.join("-") }
}

/// Where generated clips are saved: next to the project if it has been saved,
/// otherwise in Videos/RustCut AI.
pub fn output_dir(project_path: Option<&Path>) -> PathBuf {
    if let Some(dir) = project_path.and_then(Path::parent) {
        return dir.join("RustCut AI");
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join("Videos").join("RustCut AI"))
        .unwrap_or_else(|| PathBuf::from("RustCut AI"))
}

/// A JPEG of a media file at `time` (seconds into the source), longest side ≤ `max_side`.
pub fn frame_jpeg(path: &Path, time: f64, max_side: u32) -> Result<Vec<u8>> {
    for t in [time, time - 0.3, time - 1.0, 0.0] {
        let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", t.max(0.0)), "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={max_side}:{max_side}:force_original_aspect_ratio=decrease"),
                "-c:v",
                "mjpeg",
                "-q:v",
                "3",
                "-f",
                "image2pipe",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .context("could not run ffmpeg")?;
        if out.stdout.len() > 1000 {
            return Ok(out.stdout);
        }
    }
    bail!("could not read a frame from {}", path.display())
}

/// The last visible frame of a clip (source time just before its out point).
pub fn clip_last_frame(project: &Project, clip: &Clip) -> Result<Vec<u8>> {
    let media = clip.media_id().and_then(|m| project.media(m)).context("clip has no media")?;
    let t = if media.kind == MediaKind::Image { 0.0 } else { clip.source_in + clip.duration * clip.speed - 0.1 };
    frame_jpeg(&media.path, t, 1280)
}

/// The first visible frame of a clip.
pub fn clip_first_frame(project: &Project, clip: &Clip) -> Result<Vec<u8>> {
    let media = clip.media_id().and_then(|m| project.media(m)).context("clip has no media")?;
    let t = if media.kind == MediaKind::Image { 0.0 } else { clip.source_in };
    frame_jpeg(&media.path, t, 1280)
}

/// Where a generated clip goes once it has downloaded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Media library only.
    Library,
    /// At a timeline time, on the main track if free, else an overlay track.
    At(f64),
    /// Appended to the end of the main track.
    End,
    /// Right after a clip on its track, pushing later clips along.
    After(Id),
}

/// Adds a generated file to the project and places it. Returns (media id, clip id).
pub fn place_generated(project: &mut Project, gen: &Generated, placement: Placement) -> (Id, Option<Id>) {
    let media = project.add_media(gen.info.clone().into_media_item(gen.path.clone()));
    let clip = match placement {
        Placement::Library => None,
        Placement::At(t) => project.new_media_clip(media, t).map(|c| {
            let id = c.id;
            let main = project.main_track();
            project.place_clip(c, Some(main));
            id
        }),
        Placement::End => project.new_media_clip(media, 0.0).map(|c| {
            let id = c.id;
            project.append_to_main(c);
            id
        }),
        Placement::After(anchor) => match (project.locate(anchor), project.clip(anchor).map(Clip::end)) {
            (Some((ti, _)), Some(end)) => project.new_media_clip(media, end).map(|c| project.ripple_insert(ti, end, c)),
            _ => project.new_media_clip(media, 0.0).map(|c| {
                let id = c.id;
                project.append_to_main(c);
                id
            }),
        },
    };
    (media, clip)
}

pub const EXTEND_PROMPT: &str = "Continue this shot seamlessly: same scene, subject, lighting, colours and camera movement. \
Natural, realistic motion; no cuts, no text.";
pub const BRIDGE_PROMPT: &str = "A smooth, continuous shot that starts exactly on the first frame and ends exactly on the last \
frame, with natural motion between them; same style and lighting; no cuts, no text.";

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<VideoModel> {
        parse_models(&json!({ "data": [
            { "id": "google/veo-3.1-lite", "name": "Veo 3.1 Lite", "supported_durations": [8, 4, 6],
              "supported_resolutions": ["720p", "1080p"], "supported_aspect_ratios": ["16:9", "9:16"],
              "supported_frame_images": ["first_frame", "last_frame"], "generate_audio": true,
              "pricing_skus": { "duration_seconds_with_audio": "0.08", "duration_seconds_without_audio": "0.05",
                                "duration_seconds_with_audio_720p": "0.05", "duration_seconds_without_audio_720p": "0.03" } },
            { "id": "x-ai/grok-imagine-video-1.5-lite", "supported_durations": [1, 2, 3, 4, 5],
              "supported_resolutions": ["480p", "720p", "1080p"], "supported_aspect_ratios": ["16:9", "9:16", "1:1", "4:3", "3:4"],
              "supported_frame_images": ["first_frame"], "generate_audio": null,
              "pricing_skus": { "cents_per_image_input": "1", "cents_per_video_output_second_480p": "2",
                                "cents_per_video_output_second_720p": "3", "cents_per_video_output_second_1080p": "14" } },
            { "id": "bytedance/seedance-2.0-mini", "supported_durations": [4, 5, 6],
              "supported_resolutions": ["480p", "720p"], "supported_aspect_ratios": ["16:9", "9:16"],
              "supported_frame_images": ["first_frame", "last_frame"], "generate_audio": true,
              "pricing_skus": { "video_tokens": "0.0000035", "video_tokens_without_audio": "0.0000035", "video_tokens_with_video_input": "0.0000021" } },
            { "id": "alibaba/wan-2.6", "supported_durations": [5, 10], "supported_resolutions": ["720p", "1080p"],
              "supported_aspect_ratios": ["16:9", "9:16"], "supported_frame_images": ["first_frame"],
              "pricing_skus": { "text_to_video_duration_seconds_720p": "0.08", "image_to_video_duration_seconds_720p": "0.10" } },
            { "id": "black-forest-labs/flux-video-edit", "supported_durations": [], "pricing_skus": { "cents_per_second_output": "3" } }
        ]}))
    }

    fn get<'a>(m: &'a [VideoModel], id: &str) -> &'a VideoModel {
        m.iter().find(|x| x.id == id).unwrap()
    }

    #[test]
    fn parses_capabilities() {
        let m = models();
        let veo = get(&m, "google/veo-3.1-lite");
        assert!(veo.first_frame && veo.last_frame && veo.generates());
        assert_eq!(veo.audio, Some(true));
        assert!(!get(&m, "black-forest-labs/flux-video-edit").generates());
        assert_eq!(veo.pick_duration(5.0), 6);
        assert_eq!(veo.pick_duration(20.0), 8);
        assert_eq!(veo.pick_aspect(1080.0, 1920.0), "9:16");
        assert_eq!(get(&m, "x-ai/grok-imagine-video-1.5-lite").pick_aspect(1080.0, 1350.0), "3:4");
        assert_eq!(veo.default_resolution(), "720p");
    }

    #[test]
    fn estimates_costs_across_pricing_styles() {
        let m = models();
        let (c, rough) = estimate_cost(get(&m, "google/veo-3.1-lite"), 8, "720p", "16:9", false, false).unwrap();
        assert!((c - 0.24).abs() < 1e-9 && !rough, "{c}");
        let (c, _) = estimate_cost(get(&m, "google/veo-3.1-lite"), 8, "720p", "16:9", true, true).unwrap();
        assert!((c - 0.40).abs() < 1e-9, "{c}");
        let (c, _) = estimate_cost(get(&m, "x-ai/grok-imagine-video-1.5-lite"), 5, "480p", "16:9", false, true).unwrap();
        assert!((c - 0.11).abs() < 1e-9, "{c}"); // 5 s × 2¢ + 1¢ image
        let (c, _) = estimate_cost(get(&m, "alibaba/wan-2.6"), 5, "720p", "16:9", false, true).unwrap();
        assert!((c - 0.50).abs() < 1e-9, "{c}"); // image-to-video price
        let (c, rough) = estimate_cost(get(&m, "bytedance/seedance-2.0-mini"), 5, "720p", "16:9", false, false).unwrap();
        assert!(rough && c > 0.2 && c < 0.6, "{c}");
    }

    #[test]
    fn request_body_with_frames() {
        let req = GenRequest {
            model: "google/veo-3.1-lite".into(),
            prompt: "waves".into(),
            duration: 4,
            resolution: "720p".into(),
            aspect_ratio: "9:16".into(),
            generate_audio: false,
            first_frame: Some((vec![0xFF, 0xD8, 0xFF], "image/jpeg")),
            last_frame: Some((vec![1, 2, 3], "image/png")),
        };
        let b = req.body();
        assert_eq!(b["duration"], 4);
        assert_eq!(b["frame_images"][0]["frame_type"], "first_frame");
        assert_eq!(b["frame_images"][0]["image_url"]["url"], "data:image/jpeg;base64,/9j/");
        assert_eq!(b["frame_images"][1]["frame_type"], "last_frame");
        let plain = GenRequest { first_frame: None, last_frame: None, ..req };
        assert!(plain.body().get("frame_images").is_none());
    }

    #[test]
    fn placement_ripples_after_anchor() {
        let mut p = Project::default();
        let m = p.add_media(MediaItem {
            id: 0, path: "a.mp4".into(), name: "a.mp4".into(), kind: MediaKind::Video,
            duration: 4.0, width: 1280, height: 720, fps: 30.0, has_audio: false,
        });
        let a = p.new_media_clip(m, 0.0).unwrap();
        let b = p.new_media_clip(m, 0.0).unwrap();
        let (aid, bid) = (a.id, b.id);
        p.append_to_main(a);
        p.append_to_main(b);
        let gen = Generated {
            path: "gen.mp4".into(),
            info: ProbeInfo { kind: MediaKind::Video, duration: 6.0, width: 1280, height: 720, fps: 24.0, has_audio: false },
            cost: Some(0.18),
            job_id: "gen-vid-1".into(),
        };
        let (_, clip) = place_generated(&mut p, &gen, Placement::After(aid));
        let c = p.clip(clip.unwrap()).unwrap();
        assert_eq!((c.start, c.duration), (4.0, 6.0));
        assert_eq!(p.clip(bid).unwrap().start, 10.0);
        let (_, none) = place_generated(&mut p, &gen, Placement::Library);
        assert!(none.is_none());
        assert_eq!(p.media.len(), 3);
    }

    #[test]
    fn helpers() {
        assert_eq!(slug("Drone shot over a misty forest, golden hour!"), "drone-shot-over-a-misty-forest");
        assert_eq!(slug("!!!"), "clip");
        assert_eq!(pixel_size("720p", "9:16"), (720, 1280));
        assert_eq!(pixel_size("1080p", "16:9"), (1920, 1080));
        let dir = std::env::temp_dir().join("rustcut-test-unique");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a", "mp4").file_name().unwrap(), "a-2.mp4");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
