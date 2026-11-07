//! Reference shots: one Seedance 2.0 shot from a prompt that tags reference images
//! (`@Image1` … `@Image9`) — character sheets, a creature, a location. The images are
//! uploaded to fal storage in order, so `@ImageN` is the N-th file given.

use crate::ai::lipsync;
use crate::ai::performance::{usd_per_second, MODELS};
use anyhow::{bail, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Seedance 2.0 reference-to-video takes at most this many images.
pub const MAX_IMAGES: usize = 9;

pub struct RefShot {
    pub prompt: String,
    pub images: Vec<PathBuf>,
    /// Output length, 4–15 s.
    pub seconds: u32,
    pub model: String,
    /// "16:9", "9:16", …
    pub aspect: String,
}

/// "@Image 3", "@image3", "@IMAGE_3" → "@Image3" (the form the model expects).
pub fn normalize_tags(prompt: &str) -> String {
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(i) = rest.find('@') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let word = after.get(..5).unwrap_or("");
        if word.eq_ignore_ascii_case("image") {
            let tail = &after[5..];
            let gap = tail.len() - tail.trim_start_matches([' ', '_', '-']).len();
            let digits: String = tail[gap..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                out.push_str("@Image");
                out.push_str(&digits);
                rest = &tail[gap + digits.len()..];
                continue;
            }
        }
        out.push('@');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The image numbers a (normalised) prompt tags, in first-use order.
pub fn tags(prompt: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for part in prompt.split("@Image").skip(1) {
        let n: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = n.parse::<usize>() {
            if !found.contains(&n) {
                found.push(n);
            }
        }
    }
    found
}

impl RefShot {
    pub fn new(prompt: &str, images: Vec<PathBuf>, seconds: u32, model: &str, aspect: &str) -> Self {
        let model = match model {
            "" | "fast" => MODELS[0].0,
            "best" => MODELS[1].0,
            other => other,
        };
        RefShot { prompt: normalize_tags(prompt), images, seconds: seconds.clamp(4, 15), model: model.to_string(), aspect: aspect.to_string() }
    }

    pub fn estimate(&self) -> f64 {
        self.seconds as f64 * usd_per_second(&self.model)
    }

    /// Problems that would waste a paid run: missing files, too many images, tags that
    /// point past the last image, or images the prompt never mentions (a warning).
    pub fn check(&self) -> (Vec<String>, Vec<String>) {
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        if self.images.is_empty() {
            errors.push("give at least one reference image".into());
        }
        if self.images.len() > MAX_IMAGES {
            errors.push(format!("Seedance takes at most {MAX_IMAGES} images ({} given)", self.images.len()));
        }
        for p in &self.images {
            if !p.is_file() {
                errors.push(format!("image not found: {}", p.display()));
            }
        }
        let used = tags(&self.prompt);
        for &n in &used {
            if n == 0 || n > self.images.len() {
                errors.push(format!("the prompt tags @Image{n} but only {} image(s) are given", self.images.len()));
            }
        }
        for i in 1..=self.images.len() {
            if !used.contains(&i) {
                warnings.push(format!("@Image{i} ({}) isn't tagged in the prompt", self.images[i - 1].file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()));
            }
        }
        (errors, warnings)
    }

    /// Uploads the images, runs Seedance and downloads the video into `out_dir`.
    /// `Ok(None)` if cancelled.
    pub fn generate(&self, key: &str, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
        let (errors, _) = self.check();
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        progress(0.03, "Uploading the reference images…");
        let urls = self.images.iter().map(|p| lipsync::upload(key, p, mime(p))).collect::<Result<Vec<_>>>()?;
        let body = json!({
            "prompt": self.prompt,
            "image_urls": urls,
            "duration": self.seconds.to_string(),
            "resolution": "720p",
            "aspect_ratio": self.aspect,
            "generate_audio": false,
        });
        let stem = format!("reference-shot-{}", crate::ai::video_gen::slug(&self.prompt));
        let mut out = out_dir.join(format!("{stem}.mp4"));
        let mut k = 2;
        while out.exists() {
            out = out_dir.join(format!("{stem}-{k}.mp4"));
            k += 1;
        }
        let Some(path) = lipsync::fal_run(key, &self.model, body, &out, "Generating the shot…", cancel, progress)? else { return Ok(None) };
        let secs = self.seconds as f64;
        crate::economics::record(crate::economics::Spend {
            kind: "video",
            model: &self.model,
            usd: self.estimate(),
            units: secs,
            unit: "second",
            output: Some(&path),
            source: "estimate",
            note: "fal.ai Seedance 2.0 reference shot, list price per output second (720p)",
        });
        Ok(Some(path))
    }
}

fn mime(p: &Path) -> &'static str {
    match p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_loose_tags() {
        assert_eq!(normalize_tags("a dog @Image 3 at @image5, see @IMAGE_1 and me@x.com"), "a dog @Image3 at @Image5, see @Image1 and me@x.com");
        assert_eq!(tags("@Image3 then @Image1 and @Image3"), vec![3, 1]);
    }

    #[test]
    fn checks_tags_against_the_images() {
        let dir = std::env::temp_dir().join("rustcut-refshot-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let s = RefShot::new("her @Image 1 and a dog @Image3", vec![a.clone(), b.clone()], 20, "fast", "16:9");
        assert_eq!(s.seconds, 15);
        let (errors, warnings) = s.check();
        assert!(errors.iter().any(|e| e.contains("@Image3")), "{errors:?}");
        assert!(warnings.iter().any(|w| w.contains("@Image2")), "{warnings:?}");
        let ok = RefShot::new("@Image1 meets @Image2", vec![a, b], 5, "best", "16:9");
        assert_eq!(ok.check(), (vec![], vec![]));
        assert!((ok.estimate() - 5.0 * MODELS[1].2).abs() < 1e-9);
    }

    /// PAID, manual: `REF_IMAGES="a.png;b.png;dog.png;place.jpg" REF_PROMPT_FILE=prompt.txt (or REF_PROMPT="…")
    /// [REF_SECONDS=5 REF_MODEL=fast|best REF_ASPECT=16:9 REF_OUT=folder REF_DRY=1]
    /// cargo test --lib refshot_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn refshot_live() {
        let Ok(images) = std::env::var("REF_IMAGES") else { return };
        let images: Vec<PathBuf> = images.split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        let prompt = match std::env::var("REF_PROMPT_FILE") {
            Ok(f) => std::fs::read_to_string(f).unwrap(),
            Err(_) => std::env::var("REF_PROMPT").expect("REF_PROMPT or REF_PROMPT_FILE"),
        };
        let seconds = std::env::var("REF_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
        let shot = RefShot::new(&prompt, images, seconds, &std::env::var("REF_MODEL").unwrap_or_default(), &std::env::var("REF_ASPECT").unwrap_or_else(|_| "16:9".into()));
        for (i, p) in shot.images.iter().enumerate() {
            println!("@Image{} = {}", i + 1, p.display());
        }
        println!("prompt: {}\nmodel: {} · {} s · estimate ${:.2}", shot.prompt, shot.model, shot.seconds, shot.estimate());
        let (errors, warnings) = shot.check();
        for w in &warnings {
            println!("warning: {w}");
        }
        assert!(errors.is_empty(), "{errors:?}");
        if std::env::var("REF_DRY").is_ok() {
            return;
        }
        let key = crate::ai::openrouter::AiSettings::load().fal_key().expect("fal key in RustCut settings");
        let out = PathBuf::from(std::env::var("REF_OUT").expect("REF_OUT folder"));
        let cancel = AtomicBool::new(false);
        let path = shot.generate(&key, &out, &cancel, &mut |p, s| if (p * 100.0) as i32 % 20 == 0 { println!("  {s}") }).unwrap().unwrap();
        let info = crate::media::ffmpeg::probe(&path).unwrap();
        println!("video: {} ({:.2} s, {}x{})", path.display(), info.duration, info.width, info.height);
    }
}
