//! Continuity check: a frame from the middle of every shot on the main track goes
//! onto numbered contact sheets (numbers match the Timeline storyboard), and a
//! vision model — cloud through OpenRouter, or a local Ollama vision model — lists
//! places where the same character's outfit, hair or face changes between shots.
//! Character sheets / reference photos found next to the project are included.

use crate::ai::openrouter::{self, Client};
use crate::media::ffmpeg;
use crate::model::{Id, Project};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

const TILE_W: u32 = 320;
const TILE_H: u32 = 180;
const PER_SHEET: usize = 12;
/// One request holds at most this many sheets (120 shots).
const MAX_SHEETS: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    /// Shot numbers (1-based, as on the contact sheets / Timeline storyboard).
    pub shots: Vec<usize>,
    pub character: String,
    pub problem: String,
    /// "high", "medium" or "low".
    pub severity: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub issues: Vec<Issue>,
    pub summary: String,
    /// Clip id of each shot number (index 0 = shot 1).
    pub clips: Vec<Id>,
    pub sheets: Vec<PathBuf>,
    pub usd: f64,
}

pub enum Engine {
    Cloud { key: String, model: String },
    Local { url: String, model: String },
}

/// Character sheets and reference photos next to the project: (name, file).
pub fn find_references(project_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(project_dir) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| ["png", "jpg", "jpeg", "webp"].contains(&x.to_string_lossy().to_ascii_lowercase().as_str())))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().to_lowercase();
            let name = ["-character-sheet", "-reference", " character sheet", " reference"].iter().find_map(|s| stem.strip_suffix(s))?.to_string();
            let mut n = name.replace(['-', '_'], " ");
            if let Some(f) = n.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            Some((n, p))
        })
        .collect();
    out.sort();
    out.truncate(4);
    out
}

/// Main-track shots in storyboard order: (clip id, media path, source time).
fn shots(project: &Project) -> Vec<(Id, PathBuf, f64, u32, u32)> {
    crate::ui::clip_board::entries(project, true)
        .into_iter()
        .filter_map(|e| {
            let m = e.clip.media_id().and_then(|id| project.media(id))?;
            let t = if m.kind == crate::model::MediaKind::Image { 0.0 } else { e.clip.source_time(e.clip.start + e.clip.duration / 2.0).clamp(0.0, (m.duration - 0.05).max(0.0)) };
            Some((e.clip.id, m.path.clone(), t, m.width, m.height))
        })
        .collect()
}

pub fn shot_count(project: &Project) -> usize {
    shots(project).len()
}

/// Rough cost of a cloud check: (input tokens, output tokens).
pub fn token_estimate(shots: usize, refs: usize) -> (u64, u64) {
    let sheets = shots.div_ceil(PER_SHEET).min(MAX_SHEETS * 4) as u64;
    (sheets * 1300 + refs as u64 * 800 + 900, 1500)
}

/// Builds the numbered contact sheets (JPEG) in `dir`.
fn contact_sheets(list: &[(Id, PathBuf, f64, u32, u32)], dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Vec<PathBuf>>> {
    std::fs::create_dir_all(dir)?;
    for old in std::fs::read_dir(dir)?.flatten() {
        if old.file_name().to_string_lossy().starts_with("sheet-") {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let mut raw = Vec::with_capacity(list.len() * (TILE_W * TILE_H * 4) as usize);
    for (i, (_, path, t, w, h)) in list.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        progress(0.4 * i as f32 / list.len() as f32, &format!("Grabbing frame {} of {}…", i + 1, list.len()));
        let (fw, fh) = ffmpeg::fit_within((*w).max(1), (*h).max(1), TILE_W, TILE_H);
        let px = ffmpeg::extract_frame(path, *t, fw, fh).unwrap_or_else(|_| vec![0; (fw * fh * 4) as usize]);
        // Centre it on a black tile.
        let mut tile = vec![0u8; (TILE_W * TILE_H * 4) as usize];
        let (ox, oy) = ((TILE_W - fw) / 2, (TILE_H - fh) / 2);
        for y in 0..fh {
            let src = &px[(y * fw * 4) as usize..((y + 1) * fw * 4) as usize];
            let at = (((y + oy) * TILE_W + ox) * 4) as usize;
            tile[at..at + src.len()].copy_from_slice(src);
        }
        raw.extend_from_slice(&tile);
    }
    // A bold system font for the numbers when there is one (FFmpeg's default otherwise).
    let font = std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts").join("arialbd.ttf")).filter(|f| f.is_file());
    let font = font.map(|f| format!("fontfile='{}':", ffmpeg::filter_path(&f))).unwrap_or_default();
    let vf = format!("drawtext={font}text='%{{eif\\:n+1\\:d}}':x=8:y=6:fontsize=34:fontcolor=yellow:box=1:boxcolor=black@0.65:boxborderw=4,tile=4x3:padding=4:color=0x202020");
    let mut child = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{TILE_W}x{TILE_H}"), "-r", "1", "-i", "pipe:0", "-vf", &vf, "-q:v", "3"])
        .arg(dir.join("sheet-%02d.jpg"))
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run ffmpeg")?;
    child.stdin.take().context("no stdin")?.write_all(&raw)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("FFmpeg couldn't build the contact sheets: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut sheets: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sheet-"))).collect();
    sheets.sort();
    Ok(Some(sheets))
}

fn small_jpeg(path: &Path, max: u32) -> Result<Vec<u8>> {
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vf", &format!("scale='min({max},iw)':-2"), "-frames:v", "1", "-q:v", "3", "-f", "image2pipe", "-c:v", "mjpeg", "-"])
        .output()
        .context("could not run ffmpeg")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("couldn't read {}", path.display());
    }
    Ok(out.stdout)
}

fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["issues", "summary"],
        "properties": {
            "issues": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["shots", "character", "problem", "severity"],
                "properties": {
                    "shots": { "type": "array", "items": { "type": "integer" } },
                    "character": { "type": "string" },
                    "problem": { "type": "string" },
                    "severity": { "type": "string", "enum": ["high", "medium", "low"] }
                }
            } },
            "summary": { "type": "string" }
        }
    })
}

fn prompt(n_shots: usize, n_sheets: usize, refs: &[(String, PathBuf)], story: &str) -> String {
    let mut p = format!(
        "You are the continuity supervisor for a music video. Images 1–{n_sheets} are contact sheets of its {n_shots} shots in order; \
each tile has its shot number in yellow (top-left). "
    );
    if !refs.is_empty() {
        let list: Vec<String> = refs.iter().enumerate().map(|(i, (n, _))| format!("image {} = {n}", n_sheets + i + 1)).collect();
        p.push_str(&format!("The last images are the cast's reference looks: {}. ", list.join(", ")));
    }
    if !story.trim().is_empty() {
        p.push_str(&format!("The video's concept: {}\n", story.trim().chars().take(1500).collect::<String>()));
    }
    p.push_str(
        "Find continuity errors: the same character looking different between shots where they should look the same — a different face, \
skin tone, age or build; a different hairstyle or hair colour; outfit changes inside the same scene or setting; jewellery, tattoos or props \
that appear and disappear. Outfit changes between clearly different scenes or settings are fine. Also flag a character who doesn't match \
their reference look. Only report problems you can actually see; don't invent any. For each, give the shot numbers involved (include the \
shot that shows the correct look first), who, what changes, and a severity (high = obviously a different person or look; medium = noticeable \
change; low = small detail). Write a one-sentence summary. If everything is consistent, return no issues.",
    );
    p
}

/// Runs the check. `story` is the video's concept (e.g. theme.txt) or empty.
pub fn run(project: &Project, project_dir: Option<&Path>, engine: &Engine, story: &str, work: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<Report>> {
    let list = shots(project);
    if list.len() < 2 {
        bail!("the main track needs at least two video or image shots");
    }
    let list: Vec<_> = list.into_iter().take(PER_SHEET * MAX_SHEETS).collect();
    let Some(sheets) = contact_sheets(&list, work, cancel, progress)? else { return Ok(None) };
    let refs = project_dir.map(find_references).unwrap_or_default();
    progress(0.45, "Asking the vision model…");
    // Cloud models see every sheet at once. A local model's context is small, so it
    // gets overlapping batches of smaller sheets and the findings are merged.
    let local = matches!(engine, Engine::Local { .. });
    let (batch, stride, width, ref_width) = if local { (3, 2, 1024, 512) } else { (sheets.len(), sheets.len(), 0, 768) };
    let mut ref_images = Vec::new();
    for (_, p) in &refs {
        ref_images.push(small_jpeg(p, ref_width)?);
    }
    let mut issues: Vec<Issue> = Vec::new();
    let mut summaries = Vec::new();
    let mut usd = 0.0;
    let groups: Vec<usize> = (0..sheets.len()).step_by(stride.max(1)).filter(|&g| g == 0 || g + 1 < sheets.len() || stride == sheets.len()).collect();
    for (gi, &g) in groups.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let part = &sheets[g..(g + batch).min(sheets.len())];
        let first = g * PER_SHEET + 1;
        let last = ((g + part.len()) * PER_SHEET).min(list.len());
        progress(0.45 + 0.55 * gi as f32 / groups.len() as f32, &format!("Asking the vision model ({} of {})…", gi + 1, groups.len()));
        let mut images = Vec::new();
        for s in part {
            images.push(if width > 0 { small_jpeg(s, width)? } else { std::fs::read(s)? });
        }
        images.extend(ref_images.iter().cloned());
        let text = prompt(list.len(), part.len(), &refs, story).replacen(
            &format!("are contact sheets of its {} shots in order", list.len()),
            &format!("are contact sheets of shots {first}–{last} (of {}) in order", list.len()),
            1,
        );
        let v = match engine {
            Engine::Cloud { key, model } => {
                let mut parts = vec![json!({ "type": "text", "text": text })];
                parts.extend(images.iter().map(|j| openrouter::jpeg_part(j)));
                let (v, usage) = Client::new(key.clone()).chat_json(model, json!([{ "role": "user", "content": parts }]), "continuity", schema())?;
                usd += usage.cost;
                v
            }
            Engine::Local { url, model } => {
                let body = json!({
                    "model": model, "stream": false, "think": false, "format": schema(),
                    "options": { "num_ctx": 12288, "num_predict": 2048, "temperature": 0.1 },
                    "messages": [{ "role": "user", "content": text, "images": images.iter().map(|j| crate::util::base64(j)).collect::<Vec<_>>() }],
                });
                let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(1200)).build();
                let r: Value = match agent.post(&format!("{}/api/chat", url.trim_end_matches('/'))).send_json(body) {
                    Ok(r) => r.into_json()?,
                    Err(ureq::Error::Status(c, r)) => bail!("Ollama: HTTP {c}: {}", r.into_string().unwrap_or_default().chars().take(300).collect::<String>()),
                    Err(e) => bail!("Ollama isn't reachable at {url} ({e})"),
                };
                openrouter::parse_json_reply(r["message"]["content"].as_str().unwrap_or("")).context("the local model's answer was cut off or not valid JSON")?
            }
        };
        let found: Vec<Issue> = serde_json::from_value(v["issues"].clone()).unwrap_or_default();
        for f in found {
            // The same finding from two overlapping batches is kept once.
            if !issues.iter().any(|i| i.shots == f.shots && i.character.eq_ignore_ascii_case(&f.character)) {
                issues.push(f);
            }
        }
        if let Some(s) = v["summary"].as_str().filter(|s| !s.trim().is_empty()) {
            summaries.push(s.trim().to_string());
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    // Keep only real shot numbers.
    for i in &mut issues {
        i.shots.retain(|s| (1..=list.len()).contains(s));
        i.shots.dedup();
    }
    issues.retain(|i| !i.shots.is_empty());
    let rank = |s: &str| match s {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    };
    issues.sort_by_key(|i| (rank(&i.severity), i.shots[0]));
    let summary = if summaries.len() <= 1 { summaries.concat() } else { summaries.join(" ") };
    Ok(Some(Report { issues, summary, clips: list.iter().map(|s| s.0).collect(), sheets, usd }))
}

/// A local Ollama model that can see (prefers the configured one).
pub fn local_vision_model(url: &str, preferred: &str) -> Option<String> {
    if crate::ai::copilot::ollama_has_vision(url, preferred) {
        return Some(preferred.to_string());
    }
    let models = crate::ai::local::Decider::models(url).ok()?;
    models.into_iter().find(|m| crate::ai::copilot::ollama_has_vision(url, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cast_references() {
        let dir = std::env::temp_dir().join("rustcut-continuity-refs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["aishyla-reference.png", "saivon-character-sheet.png", "preview-sheet.jpg", "notes.txt"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let refs: Vec<String> = find_references(&dir).into_iter().map(|r| r.0).collect();
        assert_eq!(refs, vec!["Aishyla", "Saivon"]);
        let p = prompt(30, 3, &find_references(&dir), "");
        assert!(p.contains("image 4 = Aishyla") && p.contains("image 5 = Saivon"), "{p}");
        let (inp, _) = token_estimate(85, 2);
        assert!(inp > 5000 && inp < 20000);
    }

    /// Manual (free with a local vision model): `CONT_PROJECT=song.rcproj CONT_OUT=dir CONT_LOCAL=qwen3-vl:8b-instruct
    /// cargo test --lib continuity_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn continuity_live() {
        let Ok(path) = std::env::var("CONT_PROJECT") else { return };
        let out = PathBuf::from(std::env::var("CONT_OUT").unwrap());
        let p = Project::load(Path::new(&path)).unwrap();
        let dir = Path::new(&path).parent().unwrap();
        let story = std::fs::read_to_string(dir.join("theme.txt")).unwrap_or_default();
        let s = crate::ai::openrouter::AiSettings::load();
        let engine = Engine::Local { url: s.ollama_url.clone(), model: std::env::var("CONT_LOCAL").unwrap() };
        let t0 = std::time::Instant::now();
        let r = run(&p, Some(dir), &engine, &story, &out, &AtomicBool::new(false), &mut |_, s| if s.starts_with("Asking") { println!("  {s}") }).unwrap().unwrap();
        println!("{} shots, {} sheets, refs {:?}, {:.0}s", r.clips.len(), r.sheets.len(), find_references(dir).iter().map(|r| &r.0).collect::<Vec<_>>(), t0.elapsed().as_secs_f64());
        println!("summary: {}", r.summary);
        for i in &r.issues {
            println!("  [{}] shots {:?} · {} · {}", i.severity, i.shots, i.character, i.problem);
        }
    }
}
