//! Motion graphics made with HyperFrames (HTML → video, https://github.com/heygen-com/hyperframes).
//!
//! A graphic is a small HyperFrames project written from a template (title card, lower-third
//! name tag, lyric line) and rendered with `npx hyperframes render --format mov` to ProRes 4444
//! with alpha, so it sits over the footage on an upper video track. Each graphic keeps its own
//! folder (`<project>/RustCut AI/motion graphics/<name>/`) with `graphic.json` (the settings, so
//! "Edit graphic…" can reopen it), `index.html` and the rendered `<name>-vN.mov` files.
//! Rendering is local and free; it needs Node.js (npx) and ≥ 1 GB free disk.

use crate::model::{Aspect, Clip, Id, Project, TrackKind};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The HyperFrames CLI version the templates are written and tested against.
pub const HYPERFRAMES_VERSION: &str = "0.9.52";
pub const SPEC_FILE: &str = "graphic.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    #[default]
    TitleCard,
    LowerThird,
    LyricLine,
}

impl Template {
    pub const ALL: [Template; 3] = [Template::TitleCard, Template::LowerThird, Template::LyricLine];

    pub fn label(self) -> &'static str {
        match self {
            Template::TitleCard => "Title card",
            Template::LowerThird => "Lower-third name tag",
            Template::LyricLine => "Lyric line (word by word)",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Template::TitleCard => "title_card",
            Template::LowerThird => "lower_third",
            Template::LyricLine => "lyric_line",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "title_card" | "title" => Some(Template::TitleCard),
            "lower_third" | "name_tag" | "lower" => Some(Template::LowerThird),
            "lyric_line" | "lyric" | "lyrics" => Some(Template::LyricLine),
            _ => None,
        }
    }

    /// What the two text fields mean for this template.
    pub fn field_labels(self) -> (&'static str, &'static str) {
        match self {
            Template::TitleCard => ("Title", "Subtitle / artist"),
            Template::LowerThird => ("Name", "Role / caption"),
            Template::LyricLine => ("Lyric line", "Second line (optional)"),
        }
    }

    pub fn default_font(self) -> &'static str {
        match self {
            Template::TitleCard => "League Gothic",
            Template::LowerThird | Template::LyricLine => "Montserrat",
        }
    }

    pub fn default_duration(self) -> f64 {
        match self {
            Template::TitleCard => 4.0,
            Template::LowerThird => 5.0,
            Template::LyricLine => 4.0,
        }
    }
}

/// Fonts bundled with HyperFrames (they resolve by name without font files).
pub const FONTS: &[&str] = &["League Gothic", "Montserrat", "Oswald", "Archivo Black", "Playfair Display", "Poppins", "Bebas Neue"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphicSpec {
    pub template: Template,
    pub headline: String,
    #[serde(default)]
    pub sub: String,
    /// Accent colour (gold by default).
    pub color: [u8; 3],
    pub font: String,
    pub duration: f64,
    /// Canvas size; taken from the project's aspect when rendering.
    #[serde(default = "default_w")]
    pub width: u32,
    #[serde(default = "default_h")]
    pub height: u32,
    /// Render count, used for the next file name (renders never overwrite a file in use).
    #[serde(default)]
    pub version: u32,
}

fn default_w() -> u32 {
    1920
}
fn default_h() -> u32 {
    1080
}

impl Default for GraphicSpec {
    fn default() -> Self {
        Self { template: Template::TitleCard, headline: String::new(), sub: String::new(), color: [232, 181, 74], font: "League Gothic".into(), duration: 4.0, width: 1920, height: 1080, version: 0 }
    }
}

impl GraphicSpec {
    pub fn for_aspect(mut self, aspect: Aspect) -> Self {
        let (w, h) = aspect.size_for(1080);
        self.width = w;
        self.height = h;
        self
    }

    pub fn display_name(&self) -> String {
        let t = self.headline.trim();
        if t.is_empty() { self.template.label().to_string() } else { t.chars().take(40).collect() }
    }

    fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.color[0], self.color[1], self.color[2])
    }

    fn validate(&self) -> Result<()> {
        if self.headline.trim().is_empty() {
            bail!("the {} needs some text", self.template.label().to_lowercase());
        }
        if !(1.0..=30.0).contains(&self.duration) {
            bail!("length must be 1–30 seconds");
        }
        if !FONTS.contains(&self.font.as_str()) {
            bail!("font must be one of: {}", FONTS.join(", "));
        }
        Ok(())
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// The HyperFrames `index.html` for a graphic: transparent background, one paused GSAP
/// timeline registered as `main`, everything faded out just before the end.
pub fn html(spec: &GraphicSpec) -> String {
    let (w, h) = (spec.width, spec.height);
    // Layout is designed at 1920×1080; `s` scales it to the canvas's width-limited size.
    let s = (w as f64 / 1920.0).min(h as f64 / 1080.0).max(0.4) * if w < h { 1.5 } else { 1.0 };
    let d = spec.duration;
    let out_at = (d - 0.6).max(0.5);
    let accent = spec.hex();
    let font = &spec.font;
    let headline = esc(spec.headline.trim());
    let sub = esc(spec.sub.trim());
    let (style, body, script) = match spec.template {
        Template::TitleCard => {
            // Shrink long titles so they fit 85 % of the width (display faces ≈ 0.5 em per character).
            let chars = spec.headline.trim().chars().count().max(1) as f64;
            let size = (260.0 * s).min(0.85 * w as f64 / (chars * 0.5)).round();
            let sub_size = (44.0 * s).round();
            (
                format!(
                    r##"
      #card {{ position: absolute; inset: 0; }}
      #inner {{ position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: {gap}px; }}
      #shade {{ position: absolute; left: 15%; top: 25%; width: 70%; height: 50%; background: radial-gradient(ellipse at center, rgba(0,0,0,0.5) 0%, rgba(0,0,0,0) 68%); }}
      #headline {{ position: relative; font-family: "{font}", sans-serif; font-weight: 400; font-size: {size}px; line-height: 0.95; letter-spacing: 0.05em; text-align: center;
        background: linear-gradient(100deg, {accent} 0%, {accent} 38%, #fff6d8 50%, {accent} 62%, {accent} 100%); background-size: 300% 100%; background-position: 100% 0;
        -webkit-background-clip: text; background-clip: text; color: transparent; }}
      #rule {{ position: relative; width: {rule}px; height: {rh}px; background: linear-gradient(90deg, rgba(0,0,0,0), {accent}, rgba(0,0,0,0)); }}
      #sub {{ position: relative; font-family: "Montserrat", sans-serif; font-weight: 700; font-size: {sub_size}px; letter-spacing: 0.5em; padding-left: 0.5em; color: #fff; text-shadow: 0 3px 12px rgba(0,0,0,0.8); text-align: center; }}"##,
                    gap = (22.0 * s).round(),
                    rule = (760.0 * s).round(),
                    rh = (4.0 * s).max(2.0).round(),
                ),
                format!(
                    r##"<div id="card" class="clip" data-start="0" data-duration="{d}" data-track-index="0">
        <div id="inner">
          <div id="shade"></div>
          <h1 id="headline">{headline}</h1>
          {rule}
          {sub}
        </div>
      </div>"##,
                    rule = if sub.is_empty() { "" } else { r##"<div id="rule"></div>"## },
                    sub = if sub.is_empty() { String::new() } else { format!(r##"<div id="sub">{sub}</div>"##) },
                ),
                format!(
                    r##"
      tl.fromTo("#shade", {{ opacity: 0 }}, {{ opacity: 1, duration: 0.5, ease: "power2.out" }}, 0);
      tl.fromTo("#headline", {{ opacity: 0, scale: 1.18, filter: "blur(18px)" }}, {{ opacity: 1, scale: 1, filter: "blur(0px)", duration: 0.7, ease: "power4.out" }}, 0.15);
      if (document.querySelector("#rule")) tl.fromTo("#rule", {{ scaleX: 0 }}, {{ scaleX: 1, duration: 0.6, ease: "power3.inOut" }}, 0.55);
      if (document.querySelector("#sub")) tl.fromTo("#sub", {{ opacity: 0, scaleX: 1.25, y: 14 }}, {{ opacity: 1, scaleX: 1, y: 0, duration: 0.8, ease: "power3.out" }}, 0.75);
      tl.fromTo("#headline", {{ backgroundPosition: "100% 0" }}, {{ backgroundPosition: "0% 0", duration: {sheen}, ease: "power1.inOut" }}, 1.1);
      tl.fromTo("#inner", {{ scale: 1 }}, {{ scale: 1.04, duration: {push}, ease: "none" }}, 0);
      tl.fromTo("#inner", {{ opacity: 1 }}, {{ opacity: 0, duration: 0.45, ease: "power2.in", immediateRender: false }}, {out_at});"##,
                    sheen = ((d - 1.8).clamp(0.8, 2.4) * 100.0).round() / 100.0,
                    push = d - 0.05,
                ),
            )
        }
        Template::LowerThird => {
            let name_size = (68.0 * s).round();
            let role_size = (30.0 * s).round();
            (
                format!(
                    r##"
      #card {{ position: absolute; inset: 0; }}
      #tag {{ position: absolute; left: {left}px; bottom: {bottom}px; display: flex; align-items: stretch; gap: {gap}px; }}
      #bar {{ width: {bar}px; background: {accent}; transform-origin: 50% 100%; }}
      #panel {{ position: relative; padding: {pv}px {ph}px; }}
      #panel-bg {{ position: absolute; inset: 0; background: linear-gradient(90deg, rgba(10,10,14,0.78), rgba(10,10,14,0.35)); transform-origin: 0 50%; }}
      #headline {{ position: relative; font-family: "{font}", sans-serif; font-weight: 700; font-size: {name_size}px; line-height: 1.05; color: #fff; letter-spacing: 0.02em; white-space: nowrap; }}
      #sub {{ position: relative; margin-top: {mt}px; font-family: "Montserrat", sans-serif; font-weight: 700; font-size: {role_size}px; letter-spacing: 0.22em; text-transform: uppercase; color: {accent}; white-space: nowrap; }}"##,
                    left = (0.07 * w as f64).round(),
                    bottom = (0.13 * h as f64).round(),
                    gap = (14.0 * s).round(),
                    bar = (8.0 * s).max(4.0).round(),
                    pv = (16.0 * s).round(),
                    ph = (26.0 * s).round(),
                    mt = (6.0 * s).round(),
                ),
                format!(
                    r##"<div id="card" class="clip" data-start="0" data-duration="{d}" data-track-index="0">
        <div id="tag">
          <div id="bar"></div>
          <div id="panel">
            <div id="panel-bg"></div>
            <div id="headline">{headline}</div>
            {sub}
          </div>
        </div>
      </div>"##,
                    sub = if sub.is_empty() { String::new() } else { format!(r##"<div id="sub">{sub}</div>"##) },
                ),
                format!(
                    r##"
      tl.fromTo("#bar", {{ scaleY: 0 }}, {{ scaleY: 1, duration: 0.35, ease: "power3.out" }}, 0.1);
      tl.fromTo("#panel-bg", {{ scaleX: 0 }}, {{ scaleX: 1, duration: 0.5, ease: "power3.out" }}, 0.3);
      tl.fromTo("#headline", {{ opacity: 0, x: {dx} }}, {{ opacity: 1, x: 0, duration: 0.5, ease: "power3.out" }}, 0.45);
      if (document.querySelector("#sub")) tl.fromTo("#sub", {{ opacity: 0, x: {dx} }}, {{ opacity: 1, x: 0, duration: 0.5, ease: "power3.out" }}, 0.6);
      tl.fromTo("#tag", {{ opacity: 1, x: 0 }}, {{ opacity: 0, x: {dx_out}, duration: 0.45, ease: "power2.in", immediateRender: false }}, {out_at});"##,
                    dx = (-30.0 * s).round(),
                    dx_out = (-20.0 * s).round(),
                ),
            )
        }
        Template::LyricLine => {
            let words: Vec<String> = spec.headline.split_whitespace().map(esc).collect();
            let size = (84.0 * s).min(0.9 * w as f64 / (spec.headline.trim().chars().count().max(1) as f64 * 0.5)).max(28.0 * s).round();
            let spans: String = words.iter().enumerate().map(|(i, wd)| format!(r##"<span class="word" id="w{i}">{wd}</span>"##)).collect::<Vec<_>>().join(" ");
            // Words land over the first ~45 % of the clip (at most 0.35 s apart).
            let step = ((d * 0.45) / words.len().max(1) as f64).min(0.35);
            (
                format!(
                    r##"
      #card {{ position: absolute; inset: 0; }}
      #lyric {{ position: absolute; left: 6%; right: 6%; bottom: {bottom}px; text-align: center; }}
      #line {{ font-family: "{font}", sans-serif; font-weight: 700; font-size: {size}px; line-height: 1.15; color: #fff; text-shadow: 0 4px 18px rgba(0,0,0,0.85), 0 0 2px rgba(0,0,0,0.9); }}
      .word {{ display: inline-block; }}
      #sub {{ margin-top: {mt}px; font-family: "Montserrat", sans-serif; font-weight: 700; font-size: {sub_size}px; color: {accent}; text-shadow: 0 3px 12px rgba(0,0,0,0.85); }}"##,
                    bottom = (0.14 * h as f64).round(),
                    mt = (10.0 * s).round(),
                    sub_size = (size * 0.55).round(),
                ),
                format!(
                    r##"<div id="card" class="clip" data-start="0" data-duration="{d}" data-track-index="0">
        <div id="lyric">
          <div id="line">{spans}</div>
          {sub}
        </div>
      </div>"##,
                    sub = if sub.is_empty() { String::new() } else { format!(r##"<div id="sub">{sub}</div>"##) },
                ),
                format!(
                    r##"
      tl.fromTo(".word", {{ opacity: 0, y: {dy}, scale: 0.92 }}, {{ opacity: 1, y: 0, scale: 1, duration: 0.35, ease: "back.out(1.6)", stagger: {step:.3} }}, 0.1);
      tl.fromTo(".word", {{ color: "#ffffff" }}, {{ color: "{accent}", duration: 0.2, ease: "none", stagger: {step:.3} }}, 0.3);
      if (document.querySelector("#sub")) tl.fromTo("#sub", {{ opacity: 0, y: {dy} }}, {{ opacity: 1, y: 0, duration: 0.4, ease: "power3.out" }}, {sub_at:.2});
      tl.fromTo("#lyric", {{ opacity: 1 }}, {{ opacity: 0, duration: 0.45, ease: "power2.in", immediateRender: false }}, {out_at});"##,
                    dy = (26.0 * s).round(),
                    sub_at = 0.3 + step * words.len() as f64,
                ),
            )
        }
    };
    format!(
        r##"<!doctype html>
<!-- Made by RustCut ({template}). Edit the settings in RustCut (right-click the clip → Edit graphic…);
     changes made here by hand are overwritten on the next render from RustCut. -->
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width={w}, height={h}" />
    <script src="https://cdn.jsdelivr.net/npm/gsap@3.15.0/dist/gsap.min.js"></script>
    <style>
      * {{ margin: 0; padding: 0; box-sizing: border-box; }}
      html, body {{ margin: 0; width: {w}px; height: {h}px; overflow: hidden; background: transparent; }}
      #root {{ position: relative; width: 100%; height: 100%; }}{style}
    </style>
  </head>
  <body>
    <div id="root" data-composition-id="main" data-start="0" data-duration="{d}" data-width="{w}" data-height="{h}">
      {body}
    </div>
    <script>
      const tl = gsap.timeline({{ paused: true }});{script}
      window.__timelines["main"] = tl;
      tl.seek(0);
    </script>
  </body>
</html>
"##,
        template = spec.template.id(),
    )
}

/// Folder for a new graphic: `<base>/motion graphics/<slug>` (numbered if taken).
pub fn new_folder(base: &Path, spec: &GraphicSpec) -> PathBuf {
    let root = base.join("motion graphics");
    let slug = crate::ai::video_gen::slug(&format!("{} {}", spec.template.id().replace('_', " "), spec.headline));
    let slug = if slug.is_empty() { "graphic".to_string() } else { slug };
    let mut dir = root.join(&slug);
    let mut n = 2;
    while dir.exists() {
        dir = root.join(format!("{slug}-{n}"));
        n += 1;
    }
    dir
}

/// The graphic folder a media file was rendered into, if it is one.
pub fn folder_of(media_path: &Path) -> Option<PathBuf> {
    let dir = media_path.parent()?;
    dir.join(SPEC_FILE).is_file().then(|| dir.to_path_buf())
}

pub fn load_spec(dir: &Path) -> Result<GraphicSpec> {
    let text = std::fs::read_to_string(dir.join(SPEC_FILE)).with_context(|| format!("reading {}", dir.join(SPEC_FILE).display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn npx() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

/// Writes the HyperFrames project for `spec` into `dir` and renders it to a transparent
/// ProRes 4444 `.mov`. Bumps `spec.version`. Returns the rendered file, or `Ok(None)` when
/// cancelled. Takes ~20–40 s (the first run also downloads HyperFrames from npm).
pub fn render(dir: &Path, spec: &mut GraphicSpec, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    spec.validate()?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    spec.version += 1;
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "graphic".into());
    let out = dir.join(format!("{name}-v{}.mov", spec.version));
    std::fs::write(dir.join("index.html"), html(spec))?;
    std::fs::write(
        dir.join("hyperframes.json"),
        r#"{
  "$schema": "https://hyperframes.heygen.com/schema/hyperframes.json",
  "registry": "https://raw.githubusercontent.com/heygen-com/hyperframes/main/registry",
  "paths": { "blocks": "compositions", "components": "compositions/components", "assets": "assets" },
  "media": { "autoProxy": true },
  "authoringSkill": "hyperframes"
}
"#,
    )?;
    std::fs::write(dir.join(SPEC_FILE), serde_json::to_string_pretty(spec)?)?;

    let log_path = dir.join("render.log");
    let log = std::fs::File::create(&log_path)?;
    let mut cmd = crate::media::ffmpeg::command(npx());
    cmd.current_dir(dir)
        .args(["--yes", &format!("hyperframes@{HYPERFRAMES_VERSION}"), "render", "--format", "mov", "-o"])
        .arg(out.file_name().unwrap())
        .env("HYPERFRAMES_NO_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = cmd.spawn().map_err(|e| anyhow!("couldn't start HyperFrames (npx): {e}. Install Node.js from nodejs.org, then try again."))?;
    let started = Instant::now();
    let expected = 15.0 + spec.duration * 5.0;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if cancel.load(Ordering::Relaxed) {
            kill_tree(&mut child);
            return Ok(None);
        }
        let t = started.elapsed().as_secs_f64();
        let stage = if t < 8.0 { "Starting HyperFrames…" } else { "Rendering the graphic…" };
        progress((t / expected).min(0.95) as f32, stage);
        std::thread::sleep(Duration::from_millis(250));
    };
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    if !status.success() || !out.is_file() {
        if log.contains("Low disk space") {
            bail!("not enough free disk space: HyperFrames needs at least 1 GB free on the drive with {}", dir.display());
        }
        let tail: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).rev().take(6).collect();
        bail!("HyperFrames render failed ({status}): {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | "));
    }
    progress(1.0, "Done");
    Ok(Some(out))
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        // npx.cmd → node → Chrome: kill the whole tree, not just the cmd shell.
        let _ = crate::media::ffmpeg::command("taskkill").args(["/T", "/F", "/PID", &child.id().to_string()]).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Puts an overlay clip on the nearest video track above the main one with room at its
/// time, or on a new track just above the main track. Returns the track index.
pub fn place_overlay(project: &mut Project, clip: Clip) -> usize {
    let main = project.main_track();
    let (s, e) = (clip.start, clip.end());
    let target = (0..main).rev().find(|&i| {
        let t = &project.tracks[i];
        t.kind == TrackKind::Video && !t.locked && project.fits(i, s, e, None)
    });
    let ti = target.unwrap_or_else(|| project.insert_track(TrackKind::Video, Some(main)));
    project.tracks[ti].clips.push(clip);
    project.sort_track(ti);
    ti
}

/// Adds a rendered graphic: imports the file and places it at `start` above the footage.
/// Returns (media id, clip id).
pub fn add_to_project(project: &mut Project, file: &Path, start: f64) -> Result<(Id, Id)> {
    let info = crate::media::ffmpeg::probe(file)?;
    let media = project.add_media(info.into_media_item(file.to_path_buf()));
    let clip = project.new_media_clip(media, start.max(0.0)).ok_or_else(|| anyhow!("couldn't make a clip from {}", file.display()))?;
    let id = clip.id;
    place_overlay(project, clip);
    Ok((media, id))
}

/// Swaps a re-rendered graphic into clip `clip`, keeping its place; its length follows the
/// new render (as far as the next clip on its track allows).
pub fn replace_in_project(project: &mut Project, clip: Id, file: &Path) -> Result<Id> {
    let info = crate::media::ffmpeg::probe(file)?;
    let media = project.add_media(info.into_media_item(file.to_path_buf()));
    let len = project.media(media).and_then(|m| m.source_len()).unwrap_or(0.0);
    project.replace_media(&[clip], media);
    let (_, next) = project.neighbor_bounds(clip);
    if let Some(c) = project.clip_mut(clip) {
        c.source_in = 0.0;
        c.speed = 1.0;
        c.duration = len.min(next - c.start).max(crate::model::MIN_CLIP_LEN);
    }
    Ok(media)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(t: Template, headline: &str, sub: &str) -> GraphicSpec {
        GraphicSpec { template: t, headline: headline.into(), sub: sub.into(), duration: t.default_duration(), font: t.default_font().into(), ..Default::default() }
    }

    #[test]
    fn templates_follow_the_hyperframes_contract() {
        for t in Template::ALL {
            for sub in ["", "AISHYLA"] {
                let h = html(&spec(t, "Bond <Money> & \"cash\"", sub));
                assert!(h.contains(r#"data-composition-id="main""#) && h.contains(r#"window.__timelines["main"] = tl"#), "{t:?}");
                assert!(h.contains("background: transparent"));
                assert!(h.contains(r#"class="clip" data-start="0" data-duration=""#));
                assert!(h.contains("Bond &lt;Money&gt; &amp; &quot;cash&quot;") || t == Template::LyricLine);
                assert!(!h.contains("<Money>"), "text is escaped");
                assert!(!h.contains("Math.random") && !h.contains("Date.now") && !h.contains("repeat: -1"));
                assert_eq!(h.contains(r#"id="sub""#), !sub.is_empty(), "{t:?}");
                assert_eq!(Template::parse(t.id()), Some(t));
            }
        }
        let lyric = html(&spec(Template::LyricLine, "I been saving bond money", ""));
        assert_eq!(lyric.matches(r#"class="word""#).count(), 5);
        // Portrait canvases get their own size.
        let p = spec(Template::TitleCard, "BOND MONEY", "").for_aspect(Aspect::Portrait);
        assert_eq!((p.width, p.height), (1080, 1920));
        assert!(html(&p).contains(r#"data-width="1080" data-height="1920""#));
    }

    #[test]
    fn validates_and_names_folders() {
        assert!(spec(Template::TitleCard, "  ", "").validate().is_err());
        let mut s = spec(Template::TitleCard, "Bond Money", "");
        s.font = "Comic Sans".into();
        assert!(s.validate().is_err());
        let dir = std::env::temp_dir().join(format!("rustcut-mg-{}", std::process::id()));
        let s = spec(Template::LowerThird, "Pastor Holmes", "");
        let a = new_folder(&dir, &s);
        assert!(a.ends_with("motion graphics/lower-third-pastor-holmes") || a.to_string_lossy().contains("lower-third-pastor-holmes"), "{}", a.display());
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join(SPEC_FILE), serde_json::to_string(&s).unwrap()).unwrap();
        let b = new_folder(&dir, &s);
        assert!(b.to_string_lossy().ends_with("-2"));
        assert_eq!(folder_of(&a.join("x-v1.mov")).as_deref(), Some(a.as_path()));
        assert_eq!(load_spec(&a).unwrap(), s);
        assert!(folder_of(&b.join("x.mov")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_goes_above_the_main_track() {
        use crate::model::{MediaItem, MediaKind};
        let mut p = Project::default();
        let m = p.add_media(MediaItem { id: 0, path: "t.mov".into(), name: "t".into(), kind: MediaKind::Video, duration: 4.0, width: 1920, height: 1080, fps: 30.0, has_audio: false });
        let main_clip = p.new_media_clip(m, 0.0).unwrap();
        p.append_to_main(main_clip);
        let a = p.new_media_clip(m, 1.0).unwrap();
        let ti = place_overlay(&mut p, a);
        assert!(ti < p.main_track(), "new overlay track sits above main");
        assert_eq!(p.tracks[ti].kind, TrackKind::Video);
        // A second overlay at the same time gets yet another track; a later one reuses the first.
        let b = p.new_media_clip(m, 2.0).unwrap();
        let tb = place_overlay(&mut p, b);
        let main = p.main_track();
        assert_ne!(p.tracks[tb].id, p.tracks[main].id);
        let tracks = p.tracks.len();
        let c = p.new_media_clip(m, 20.0).unwrap();
        place_overlay(&mut p, c);
        assert_eq!(p.tracks.len(), tracks);
    }

    /// Manual, renders for real (needs Node + network on first run):
    /// `cargo test --lib motion_graphics_render_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn motion_graphics_render_live() {
        let base = std::env::temp_dir().join("rustcut-mg-live");
        for t in Template::ALL {
            let mut s = spec(t, if t == Template::LyricLine { "I been saving bond money" } else { "Pastor Holmes" }, "Bond Money");
            let dir = new_folder(&base, &s);
            let out = render(&dir, &mut s, &AtomicBool::new(false), &mut |p, st| eprint!("\r{p:.2} {st}   ")).unwrap().unwrap();
            let info = crate::media::ffmpeg::probe(&out).unwrap();
            println!("\n{t:?}: {} ({:?})", out.display(), info.duration);
        }
    }
}
