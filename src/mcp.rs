//! Model Context Protocol server: lets AI agents build, inspect and render
//! RustCut projects.
//!
//! Transport is MCP "stdio": newline-delimited JSON-RPC 2.0 on stdin/stdout.
//! Only protocol messages are written to stdout; diagnostics go to stderr.
//!
//! The server holds one project in memory (with undo/redo). Agents edit it with
//! tools, look at the result with `render_frame`, then `save_project` (so it can
//! be opened in the RustCut desktop app) and/or `export_video`.

use crate::captions;
use crate::export::{self, ExportSettings};
use crate::smart_audio;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use crate::history::History;
use crate::media::ffmpeg;
use crate::model::*;
use crate::render::text;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "RustCut is a CapCut-style video editor. This server edits one project in memory.\n\
Workflow: import_media → add_clip / add_text → update_clip (transform, speed, filters, fades) → \
render_frame to check the result visually → save_project and/or export_video.\n\
Conventions: times are seconds on the timeline. Clip position x/y is the offset of the layer centre \
from the canvas centre as a fraction of canvas width/height (0,0 = centred; y=0.35 = lower third). \
scale multiplies the 'fit inside canvas' size. Tracks are listed top to bottom; upper tracks draw on top. \
The bottom video track is the main track. Every edit can be undone with `undo`. \
get_project returns all ids you need.";

struct ToolError(String);

pub struct Server<W: Write> {
    project: Project,
    path: Option<PathBuf>,
    history: History,
    out: W,
    /// Waveform peaks per media item, computed on demand for auto-ducking.
    peaks: HashMap<Id, Arc<Vec<f32>>>,
    /// The prompt-to-video storyboard being worked on (storyboard_* tools).
    storyboard: Option<crate::ai::storyboard::Storyboard>,
}

fn write_msg<W: Write>(out: &mut W, msg: &Value) -> std::io::Result<()> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    out.write_all(s.as_bytes())?;
    out.flush()
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

impl<W: Write> Server<W> {
    pub fn new(out: W) -> Self {
        let project = Project::default();
        Self { history: History::new(&project), project, path: None, out, peaks: HashMap::new(), storyboard: None }
    }

    /// Serves requests until stdin closes.
    pub fn serve(&mut self, input: impl BufRead) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle_line(&line) {
                write_msg(&mut self.out, &resp)?;
            }
        }
        Ok(())
    }

    /// Handles one JSON-RPC message; returns the response for requests.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        // Some Windows shells prefix piped input with a UTF-8 byte-order mark.
        let line = line.trim_start_matches('\u{feff}');
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(rpc_error(Value::Null, -32700, format!("Parse error: {e}"))),
        };
        if !msg.is_object() {
            return Some(rpc_error(Value::Null, -32600, "Invalid request (batches are not supported)"));
        }
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str);
        match (id, method) {
            (Some(id), Some(method)) => {
                let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                Some(self.handle_request(id, method, &params))
            }
            // Notifications (initialized, cancelled, …) and client responses need no reply.
            (None, Some(_)) | (Some(_), None) => None,
            (None, None) => Some(rpc_error(Value::Null, -32600, "Invalid request")),
        }
    }

    fn handle_request(&mut self, id: Value, method: &str, params: &Value) -> Value {
        let result = match method {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().unwrap_or(LATEST_PROTOCOL);
                let version = if SUPPORTED_PROTOCOLS.contains(&requested) { requested } else { LATEST_PROTOCOL };
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "rustcut", "title": "RustCut video editor", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tool_definitions() }),
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return rpc_error(id, -32602, "Missing tool name");
                };
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let progress = params["_meta"].get("progressToken").cloned();
                match self.call_tool(name, &args, progress) {
                    Ok(content) => json!({ "content": content, "isError": false }),
                    Err(ToolError(e)) if e.starts_with("unknown tool") => return rpc_error(id, -32602, e),
                    Err(ToolError(e)) => json!({ "content": [text(e)], "isError": true }),
                }
            }
            _ => return rpc_error(id, -32601, format!("Method not found: {method}")),
        };
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    }

    fn call_tool(&mut self, name: &str, a: &Value, progress: Option<Value>) -> std::result::Result<Vec<Value>, ToolError> {
        let result = match name {
            "get_project" => Ok(text_json(&self.summary())),
            "new_project" => self.new_project(a),
            "open_project" => self.open_project(a),
            "save_project" => self.save_project(a),
            "update_project" => self.update_project(a),
            "import_media" => self.import_media(a),
            "remove_media" => self.remove_media(a),
            "add_clip" => self.add_clip(a),
            "add_text" => self.add_text(a),
            "update_clip" => self.update_clip(a),
            "split_clip" => self.split_clip(a),
            "delete_clip" => self.delete_clip(a),
            "duplicate_clip" => self.duplicate_clip(a),
            "update_track" => self.update_track(a),
            "undo" => Ok(vec![text(if self.history.undo(&mut self.project) { "Undone." } else { "Nothing to undo." })]),
            "redo" => Ok(vec![text(if self.history.redo(&mut self.project) { "Redone." } else { "Nothing to redo." })]),
            "render_frame" => self.render_frame(a),
            "export_video" => self.export_video(a, progress),
            "auto_caption" => self.auto_caption(a, progress),
            "download_caption_model" => self.download_caption_model(a, progress),
            "export_srt" => self.export_srt(a),
            "set_keyframes" => self.set_keyframes(a),
            "clear_keyframes" => self.clear_keyframes(a),
            "apply_motion_preset" => self.apply_motion_preset(a),
            "set_transition" => self.set_transition(a),
            "auto_duck" => self.auto_duck(a),
            "set_clip_audio" => self.set_clip_audio(a),
            "measure_loudness" => self.measure_loudness(),
            "auto_reframe" => self.auto_reframe(a, progress),
            "list_video_models" => self.list_video_models(),
            "generate_video" => self.generate_video(a, progress),
            "extend_clip" => self.extend_clip(a, progress),
            "rough_cut" => self.rough_cut(a, progress),
            "auto_transitions" => self.auto_transitions(a, progress),
            "cost_report" => self.cost_report(a),
            "list_voices" => self.list_voices(),
            "speak_line" => self.speak_line(a),
            "clone_voice" => self.clone_voice(a),
            "lip_sync" => self.lip_sync(a),
            "performance_shot" => self.performance_shot(a, progress),
            "reshoot_clip" => self.reshoot_clip(a, progress),
            "set_price" => self.set_price(a),
            "generate_thumbnail" => self.generate_thumbnail(a, progress),
            "generate_still" => self.generate_still(a, progress),
            "background_fill" => self.background_fill(a, progress),
            "storyboard_plan" => self.storyboard_plan(a, progress),
            "storyboard_update_shot" => self.storyboard_update_shot(a, progress),
            "storyboard_produce" => self.storyboard_produce(a, progress),
            "detect_beats" => self.detect_beats(a),
            "beat_montage" => self.beat_montage(a),
            "beat_effect" => self.beat_effect(a),
            "multicam_sync" => self.multicam_sync(a, progress),
            "multicam_switch" => self.multicam_switch(a),
            "add_lyrics" => self.add_lyrics(a, progress),
            "export_lrc" => self.export_lrc(a),
            "add_visualizer" => self.add_visualizer(a),
            "audio_react" => self.audio_react(a),
            "music_video_plan" => self.music_video_plan(a, progress),
            "best_section" => self.best_section(a),
            _ => return Err(ToolError(format!("unknown tool: {name}"))),
        };
        self.history.commit(&self.project);
        result.map_err(|e| ToolError(format!("{e:#}")))
    }

    // ── summaries ────────────────────────────────────────────────────────────

    fn clip_json(&self, c: &Clip) -> Value {
        let mut v = serde_json::to_value(c).unwrap_or_default();
        v["end"] = json!(round3(c.end()));
        if c.has_keyframes() {
            // Agents work in timeline time; show keyframes that way too.
            let kf: serde_json::Map<String, Value> = c
                .keyframes
                .iter()
                .map(|(p, keys)| {
                    let list = keys
                        .iter()
                        .map(|k| json!({ "time": round3(c.start + k.time), "value": k.value, "ease": k.ease }))
                        .collect();
                    (p.name().to_string(), Value::Array(list))
                })
                .collect();
            v["keyframes"] = Value::Object(kf);
        }
        if let Some((ti, _)) = self.project.locate(c.id) {
            v["track_index"] = json!(ti);
        }
        if let Some(m) = c.media_id().and_then(|m| self.project.media(m)) {
            v["media_name"] = json!(m.name);
        }
        if c.transition_in.is_some() {
            let active = self
                .project
                .locate(c.id)
                .and_then(|(ti, _)| self.project.track_transitions(ti).into_iter().find(|t| t.to == c.id));
            v["transition_in_effective"] = match active {
                Some(t) => json!({ "start": round3(t.start()), "end": round3(t.end()), "duration": round3(t.duration) }),
                None => json!("inactive: no clip directly before this one"),
            };
        }
        v
    }

    fn clip_json_by_id(&self, id: Id) -> Value {
        self.project.clip(id).map(|c| self.clip_json(c)).unwrap_or(Value::Null)
    }

    fn summary(&self) -> Value {
        let p = &self.project;
        let main = p.main_track_id();
        let (w, h) = p.aspect.size_for(1080);
        json!({
            "name": p.name,
            "aspect": p.aspect.short_label(),
            "canvas_1080p": [w, h],
            "fps": p.fps,
            "duration": round3(p.duration()),
            "path": self.path,
            "media": p.media.iter().map(|m| json!({
                "id": m.id,
                "name": m.name,
                "kind": format!("{:?}", m.kind),
                "duration": round3(m.duration),
                "width": m.width,
                "height": m.height,
                "has_audio": m.has_sound(),
                "path": m.path,
                "missing": !m.path.exists(),
            })).collect::<Vec<_>>(),
            "tracks": p.tracks.iter().enumerate().map(|(i, t)| json!({
                "index": i,
                "id": t.id,
                "kind": format!("{:?}", t.kind),
                "name": t.name,
                "main": Some(t.id) == main,
                "muted": t.muted,
                "hidden": t.hidden,
                "locked": t.locked,
                "clips": t.clips.iter().map(|c| self.clip_json(c)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    // ── project ──────────────────────────────────────────────────────────────

    fn new_project(&mut self, a: &Value) -> Result<Vec<Value>> {
        let mut p = Project::default();
        if let Some(n) = opt_str(a, "name")? {
            p.name = n.to_string();
        }
        if let Some(s) = opt_str(a, "aspect")? {
            p.aspect = parse_aspect(s)?;
        }
        if let Some(f) = opt_f64(a, "fps")? {
            p.fps = parse_fps(f)?;
        }
        self.project = p;
        self.path = None;
        crate::economics::set_current_project(None);
        Ok(text_json(&self.summary()))
    }

    fn open_project(&mut self, a: &Value) -> Result<Vec<Value>> {
        let path = PathBuf::from(req_str(a, "path")?);
        self.project = Project::load(&path)?;
        self.history.reset(&self.project);
        crate::economics::set_current_project(Some(&path));
        self.path = Some(path);
        Ok(text_json(&self.summary()))
    }

    fn save_project(&mut self, a: &Value) -> Result<Vec<Value>> {
        let path = match opt_str(a, "path")? {
            Some(p) => PathBuf::from(p),
            None => self.path.clone().ok_or_else(|| anyhow!("no path given and the project has never been saved"))?,
        };
        self.project.save(&path)?;
        self.path = Some(path.clone());
        crate::economics::set_current_project(Some(&path));
        Ok(vec![text(format!("Saved to {}. Open it in the RustCut app to keep editing.", path.display()))])
    }

    fn update_project(&mut self, a: &Value) -> Result<Vec<Value>> {
        if let Some(n) = opt_str(a, "name")? {
            self.project.name = n.to_string();
        }
        if let Some(s) = opt_str(a, "aspect")? {
            self.project.aspect = parse_aspect(s)?;
        }
        match a.get("loudness_target") {
            None => {}
            Some(Value::Null) => self.project.audio.loudness = None,
            Some(Value::String(s)) if s.eq_ignore_ascii_case("off") => self.project.audio.loudness = None,
            Some(v) => {
                let t = v.as_f64().ok_or_else(|| anyhow!("`loudness_target` must be a LUFS number like -14, or \"off\""))?;
                if !(-40.0..=-5.0).contains(&t) {
                    bail!("loudness_target must be between -40 and -5 LUFS");
                }
                self.project.audio.loudness = Some(t as f32);
            }
        }
        if let Some(f) = opt_f64(a, "fps")? {
            self.project.fps = parse_fps(f)?;
        }
        Ok(text_json(&json!({
            "name": self.project.name,
            "aspect": self.project.aspect.short_label(),
            "fps": self.project.fps,
            "loudness_target": self.project.audio.loudness,
        })))
    }

    // ── media ────────────────────────────────────────────────────────────────

    fn import_media(&mut self, a: &Value) -> Result<Vec<Value>> {
        let paths = a["paths"].as_array().ok_or_else(|| anyhow!("`paths` must be an array of file paths"))?;
        if paths.is_empty() {
            bail!("`paths` is empty");
        }
        let mut imported = Vec::new();
        let mut errors = Vec::new();
        for p in paths {
            let Some(p) = p.as_str() else {
                errors.push(json!({ "path": p, "error": "not a string" }));
                continue;
            };
            let path = PathBuf::from(p);
            if !path.is_file() {
                errors.push(json!({ "path": p, "error": "file not found" }));
                continue;
            }
            match ffmpeg::probe(&path) {
                Ok(info) => {
                    let id = self.project.add_media(info.into_media_item(path));
                    let m = self.project.media(id).expect("just added");
                    imported.push(json!({
                        "media_id": id,
                        "name": m.name,
                        "kind": format!("{:?}", m.kind),
                        "duration": round3(m.duration),
                        "width": m.width,
                        "height": m.height,
                        "has_audio": m.has_sound(),
                    }));
                }
                Err(e) => errors.push(json!({ "path": p, "error": format!("{e:#}") })),
            }
        }
        if imported.is_empty() {
            bail!("nothing imported: {}", serde_json::to_string(&errors)?);
        }
        Ok(text_json(&json!({ "imported": imported, "errors": errors })))
    }

    fn remove_media(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "media_id")?;
        if self.project.media(id).is_none() {
            bail!("no media with id {id}");
        }
        self.project.remove_media(id);
        Ok(vec![text(format!("Removed media {id} and every clip that used it."))])
    }

    // ── clips ────────────────────────────────────────────────────────────────

    fn add_clip(&mut self, a: &Value) -> Result<Vec<Value>> {
        let mid = req_id(a, "media_id")?;
        let media = self.project.media(mid).cloned().ok_or_else(|| anyhow!("no media with id {mid}"))?;
        let start = opt_f64(a, "start")?;
        let track = opt_track(a, &self.project)?;
        let mut clip = self.project.new_media_clip(mid, start.unwrap_or(0.0).max(0.0)).expect("media exists");
        if let Some(si) = opt_f64(a, "source_in")? {
            if let Some(len) = media.source_len() {
                if si < 0.0 || si > len - MIN_CLIP_LEN {
                    bail!("source_in must be between 0 and {:.3}", len - MIN_CLIP_LEN);
                }
                clip.source_in = si;
            }
        }
        if let Some(d) = opt_f64(a, "duration")? {
            clip.duration = d.max(MIN_CLIP_LEN);
        }
        if let Some(max) = self.project.max_duration(&clip) {
            clip.duration = clip.duration.min(max);
        }
        let id = clip.id;
        if start.is_none() && track.is_none() && media.kind != MediaKind::Audio {
            self.project.append_to_main(clip);
        } else {
            self.project.place_clip(clip, track);
        }
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(id) })))
    }

    fn add_text(&mut self, a: &Value) -> Result<Vec<Value>> {
        let mut style = match opt_str(a, "preset")? {
            Some(name) => text::presets()
                .into_iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, s)| s)
                .ok_or_else(|| {
                    let names: Vec<_> = text::presets().into_iter().map(|(n, _)| n).collect();
                    anyhow!("unknown preset `{name}`; use one of {names:?}")
                })?,
            None => TextStyle::default(),
        };
        style.content = req_str(a, "content")?.to_string();
        apply_text_args(&mut style, a)?;
        let mut clip = self.project.new_text_clip(style, opt_f64(a, "start")?.unwrap_or(0.0));
        if let Some(d) = opt_f64(a, "duration")? {
            clip.duration = d.max(MIN_CLIP_LEN);
        }
        if let Some(x) = opt_f64(a, "x")? {
            clip.transform.x = x as f32;
        }
        if let Some(y) = opt_f64(a, "y")? {
            clip.transform.y = y as f32;
        }
        if let Some(f) = opt_f64(a, "fade_in")? {
            clip.fade_in = f.max(0.0);
        }
        if let Some(f) = opt_f64(a, "fade_out")? {
            clip.fade_out = f.max(0.0);
        }
        let id = clip.id;
        let track = opt_track(a, &self.project)?;
        self.project.place_clip(clip, track);
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(id) })))
    }

    fn update_clip(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let orig = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        let media = orig.media_id().and_then(|m| self.project.media(m)).cloned();
        let mut c = orig.clone();

        let f32_field = |k: &str| opt_f64(a, k).map(|v| v.map(|v| v as f32));
        if let Some(v) = f32_field("x")? {
            c.transform.x = v;
        }
        if let Some(v) = f32_field("y")? {
            c.transform.y = v;
        }
        if let Some(v) = f32_field("scale")? {
            if v <= 0.0 {
                bail!("scale must be > 0");
            }
            c.transform.scale = v;
        }
        if let Some(v) = f32_field("rotation")? {
            c.transform.rotation = v;
        }
        if let Some(v) = f32_field("opacity")? {
            c.opacity = v.clamp(0.0, 1.0);
        }
        if let Some(v) = f32_field("blur")? {
            c.blur = v.clamp(0.0, 100.0);
        }
        if let Some(v) = f32_field("glow")? {
            c.glow = v.clamp(0.0, 1.0);
        }
        if let Some(v) = f32_field("volume")? {
            c.volume = v.clamp(0.0, 4.0);
        }
        if let Some(v) = opt_f64(a, "fade_in")? {
            c.fade_in = v.max(0.0);
        }
        if let Some(v) = opt_f64(a, "fade_out")? {
            c.fade_out = v.max(0.0);
        }
        if let Some(f) = opt_str(a, "filter")? {
            c.filter = parse_filter(f)?;
        }
        if let Some(v) = f32_field("brightness")? {
            c.adjust.brightness = v.clamp(-1.0, 1.0);
        }
        if let Some(v) = f32_field("contrast")? {
            c.adjust.contrast = v.clamp(0.0, 4.0);
        }
        if let Some(v) = f32_field("saturation")? {
            c.adjust.saturation = v.clamp(0.0, 3.0);
        }
        let text_keys = ["content", "font_size", "color", "background", "outline", "bold"];
        if text_keys.iter().any(|k| a.get(*k).is_some()) {
            let style = c.text_mut().ok_or_else(|| anyhow!("{text_keys:?} only apply to text clips"))?;
            if let Some(s) = opt_str(a, "content")? {
                style.content = s.to_string();
            }
            apply_text_args(style, a)?;
        }
        if let Some(si) = opt_f64(a, "source_in")? {
            match media.as_ref().and_then(|m| m.source_len()) {
                Some(len) if si >= 0.0 && si <= len - MIN_CLIP_LEN => c.source_in = si,
                Some(len) => bail!("source_in must be between 0 and {:.3}", len - MIN_CLIP_LEN),
                None => bail!("source_in only applies to video and audio clips"),
            }
        }
        if let Some(d) = opt_f64(a, "duration")? {
            c.duration = d.max(MIN_CLIP_LEN);
        }
        let speed = opt_f64(a, "speed")?;
        if let Some(s) = speed {
            if media.as_ref().and_then(|m| m.source_len()).is_none() {
                bail!("speed only applies to video and audio clips");
            }
            if !(0.1..=10.0).contains(&s) {
                bail!("speed must be between 0.1 and 10");
            }
        }

        *self.project.clip_mut(id).expect("exists") = c;
        if let Some(s) = speed {
            self.project.set_clip_speed(id, s);
        }
        let max = self.project.clip(id).and_then(|c| self.project.max_duration(c));
        if let (Some(max), Some(c)) = (max, self.project.clip_mut(id)) {
            c.duration = c.duration.min(max);
        }

        let requested_track = opt_track(a, &self.project)?;
        let start = opt_f64(a, "start")?;
        if start.is_some() || requested_track.is_some() {
            let (cur, _) = self.project.locate(id).expect("exists");
            let target = requested_track.unwrap_or(cur);
            let kind = self.project.track_kind_for(&orig);
            if self.project.tracks[target].kind != kind {
                bail!("track {target} is a {:?} track; this clip needs a {kind:?} track", self.project.tracks[target].kind);
            }
            let s = start.unwrap_or_else(|| self.project.clip(id).map_or(0.0, |c| c.start));
            self.project.move_clip_live(id, target, s);
        }
        self.project.resolve_overlap(id);
        self.project.cleanup_tracks();
        let mut out = json!({ "clip": self.clip_json_by_id(id) });
        let shadowed: Vec<&str> = Prop::ALL
            .into_iter()
            .filter(|p| a.get(p.name()).is_some() && orig.is_animated(*p))
            .map(Prop::name)
            .collect();
        if !shadowed.is_empty() {
            out["warning"] = json!(format!(
                "{shadowed:?} are keyframed, so their static values are ignored; use set_keyframes or clear_keyframes"
            ));
        }
        if let (Some(req), Some((now, _))) = (requested_track, self.project.locate(id)) {
            if req != now {
                out["note"] = json!("The clip overlapped another clip on the requested track, so it was placed on a different track.");
            }
        }
        Ok(text_json(&out))
    }

    fn split_clip(&mut self, a: &Value) -> Result<Vec<Value>> {
        let t = req_f64(a, "time")?;
        let ids: Vec<Id> = match opt_f64(a, "clip_id")? {
            Some(_) => vec![req_id(a, "clip_id")?],
            None => self
                .project
                .tracks
                .iter()
                .filter(|tr| !tr.locked)
                .flat_map(|tr| tr.clips.iter().filter(|c| c.is_active(t)).map(|c| c.id))
                .collect(),
        };
        if ids.is_empty() {
            bail!("no clip is under time {t}");
        }
        let mut splits = Vec::new();
        for id in ids {
            if self.project.clip(id).is_none() {
                bail!("no clip with id {id}");
            }
            match self.project.split_clip(id, t) {
                Some(right) => splits.push(json!({ "left_clip_id": id, "right_clip_id": right })),
                None => splits.push(json!({ "clip_id": id, "error": "time is outside the clip or within 0.1 s of an edge" })),
            }
        }
        Ok(text_json(&json!({ "splits": splits })))
    }

    fn delete_clip(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let (ti, _) = self.project.locate(id).ok_or_else(|| anyhow!("no clip with id {id}"))?;
        let removed = self.project.remove_clip(id).expect("exists");
        let ripple = opt_bool(a, "ripple")?.unwrap_or(false);
        if ripple {
            for c in self.project.tracks[ti].clips.iter_mut().filter(|c| c.start >= removed.start - 1e-6) {
                c.start = (c.start - removed.duration).max(0.0);
            }
        }
        self.project.cleanup_tracks();
        Ok(vec![text(format!(
            "Deleted clip {id}{}.",
            if ripple { " and closed the gap" } else { "" }
        ))])
    }

    fn duplicate_clip(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let new = self.project.duplicate_clip(id).ok_or_else(|| anyhow!("no clip with id {id}"))?;
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(new) })))
    }

    fn update_track(&mut self, a: &Value) -> Result<Vec<Value>> {
        let i = opt_track(a, &self.project)?.ok_or_else(|| anyhow!("`track_index` is required"))?;
        let name = opt_str(a, "name")?.map(str::to_string);
        let (muted, hidden, locked) = (opt_bool(a, "muted")?, opt_bool(a, "hidden")?, opt_bool(a, "locked")?);
        let t = &mut self.project.tracks[i];
        if let Some(n) = name {
            t.name = n;
        }
        if let Some(v) = muted {
            t.muted = v;
        }
        if let Some(v) = hidden {
            t.hidden = v;
        }
        if let Some(v) = locked {
            t.locked = v;
        }
        Ok(text_json(&json!({ "index": i, "name": t.name, "muted": t.muted, "hidden": t.hidden, "locked": t.locked })))
    }

    // ── rendering ────────────────────────────────────────────────────────────

    fn render_frame(&mut self, a: &Value) -> Result<Vec<Value>> {
        let t = opt_f64(a, "time")?.unwrap_or(0.0).max(0.0);
        let side = opt_f64(a, "resolution")?.unwrap_or(360.0).clamp(120.0, 1080.0) as u32 & !1;
        let png = export::render_frame_png(&self.project, t, side)?;
        let (w, h) = self.project.aspect.size_for(side);
        let active: Vec<Id> =
            self.project.tracks.iter().flat_map(|tr| &tr.clips).filter(|c| c.is_active(t)).map(|c| c.id).collect();
        Ok(vec![
            json!({ "type": "image", "data": crate::util::base64(&png), "mimeType": "image/png" }),
            text(format!("Frame at {t:.3}s ({w}×{h}). Clips visible: {active:?}")),
        ])
    }

    fn export_video(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        let path = PathBuf::from(req_str(a, "output")?);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            if !dir.is_dir() {
                bail!("output folder does not exist: {}", dir.display());
            }
        }
        let short_side = opt_f64(a, "resolution")?.unwrap_or(1080.0).clamp(144.0, 2160.0) as u32 & !1;
        let fps = match opt_f64(a, "fps")? {
            Some(f) => parse_fps(f)?,
            None => self.project.fps,
        };
        let (crf, preset) = match opt_str(a, "quality")?.unwrap_or("standard").to_ascii_lowercase().as_str() {
            "draft" => (28, "veryfast"),
            "standard" => (21, "medium"),
            "high" => (17, "slow"),
            q => bail!("unknown quality `{q}`; use draft, standard or high"),
        };
        let settings = ExportSettings { path: path.clone(), short_side, fps, crf, preset: preset.into() };
        let started = Instant::now();
        let cancel = AtomicBool::new(false);
        let mut notify = progress_notifier(&mut self.out, progress);
        export::run(&self.project, &settings, &cancel, |p| notify(p, "Rendering…"))?;
        let (w, h) = self.project.aspect.size_for(short_side);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let gaps: Vec<Value> = self.project.picture_gaps(1.0).iter().map(|(a, b)| json!([round3(*a), round3(*b)])).collect();
        let thumbnail = self
            .project
            .thumbnail
            .as_ref()
            .filter(|t| t.exists())
            .and_then(|t| crate::ai::image_gen::copy_thumbnail_next_to(t, &path).ok());
        Ok(text_json(&json!({
            "path": path,
            "thumbnail": thumbnail,
            "width": w,
            "height": h,
            "fps": fps,
            "duration": round3(self.project.duration()),
            "size_bytes": size,
            "elapsed_seconds": round3(started.elapsed().as_secs_f64()),
            "no_video_ranges": gaps,
        })))
    }

    // ── captions ─────────────────────────────────────────────────────────────

    fn caption_model(a: &Value) -> Result<&'static captions::ModelInfo> {
        let name = opt_str(a, "model")?.unwrap_or(captions::DEFAULT_MODEL);
        captions::model_info(name).ok_or_else(|| {
            let names: Vec<_> = captions::MODELS.iter().map(|m| m.name).collect();
            anyhow!("unknown model `{name}`; use one of {names:?}")
        })
    }

    fn download_caption_model(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        let info = Self::caption_model(a)?;
        if captions::is_downloaded(info.name) {
            return Ok(vec![text(format!(
                "The `{}` model is already downloaded ({}).",
                info.name,
                captions::model_path(info.name).display()
            ))]);
        }
        let cancel = AtomicBool::new(false);
        let mut notify = progress_notifier(&mut self.out, progress);
        let path = captions::download_model(info.name, &cancel, |p| notify(p, "Downloading…"))?
            .ok_or_else(|| anyhow!("download cancelled"))?;
        Ok(vec![text(format!("Downloaded the `{}` model to {}.", info.name, path.display()))])
    }

    fn auto_caption(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        let engine = captions::detect_engine().ok_or_else(|| {
            anyhow!("no speech engine: needs FFmpeg 8+ built with whisper, or whisper.cpp's whisper-cli on PATH")
        })?;
        let info = Self::caption_model(a)?;
        if !captions::is_downloaded(info.name) {
            bail!(
                "the `{}` speech model is not downloaded yet ({} MB); call download_caption_model first",
                info.name,
                info.size_mb
            );
        }
        let source = match a.get("clip_id") {
            None | Some(Value::Null) => captions::Source::Timeline,
            Some(_) => captions::Source::Clip(req_id(a, "clip_id")?),
        };
        let style_name = opt_str(a, "style")?.unwrap_or("Clean");
        let style = captions::caption_style(style_name).ok_or_else(|| {
            let names: Vec<_> = captions::caption_styles().into_iter().map(|s| s.0).collect();
            anyhow!("unknown style `{style_name}`; use one of {names:?}")
        })?;
        let layout = captions::Layout {
            style,
            max_words: opt_f64(a, "max_words")?.unwrap_or(6.0).clamp(1.0, 20.0) as usize,
            y: opt_f64(a, "y")?.unwrap_or(0.33) as f32,
            replace: opt_bool(a, "replace")?.unwrap_or(true),
        };
        let opts = captions::Options {
            model: captions::model_path(info.name),
            language: opt_str(a, "language")?.unwrap_or("auto").to_string(),
            translate: opt_bool(a, "translate")?.unwrap_or(false),
            max_len: None,
        };
        let cancel = AtomicBool::new(false);
        let mut notify = progress_notifier(&mut self.out, progress);
        let segments = captions::transcribe(&self.project, source, &opts, &engine, &cancel, |p, s| notify(p, s))?
            .unwrap_or_default();
        if segments.is_empty() {
            return Ok(vec![text("No speech detected; nothing was added.")]);
        }
        let (ti, n) = captions::apply_captions(&mut self.project, &segments, &layout);
        let first: Vec<Value> = self.project.tracks[ti]
            .clips
            .iter()
            .take(8)
            .map(|c| json!({ "id": c.id, "start": round3(c.start), "end": round3(c.end()), "text": c.text().map(|t| t.content.clone()) }))
            .collect();
        Ok(text_json(&json!({
            "captions_added": n,
            "track_index": ti,
            "engine": engine.label(),
            "model": info.name,
            "first_captions": first,
            "hint": "Captions are text clips: fix wording with update_clip(content=…), restyle by passing a new style with replace=true, or export_srt.",
        })))
    }

    // ── keyframes ────────────────────────────────────────────────────────────

    fn check_prop(&self, clip: &Clip, p: Prop) -> Result<()> {
        let kind = self.project.track_kind_for(clip);
        let has_sound = clip.media_id().and_then(|m| self.project.media(m)).is_some_and(|m| m.has_sound());
        let ok = match p {
            Prop::Volume => has_sound,
            Prop::Rotation | Prop::Blur | Prop::Glow => kind == TrackKind::Video,
            _ => kind != TrackKind::Audio,
        };
        if !ok {
            bail!("`{}` cannot be animated on this {kind:?} clip", p.name());
        }
        Ok(())
    }

    fn set_keyframes(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let clip = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        let prop = Prop::parse(req_str(a, "property")?)
            .ok_or_else(|| anyhow!("unknown property; use x, y, scale, rotation, opacity or volume"))?;
        self.check_prop(&clip, prop)?;
        let list = a["keyframes"].as_array().ok_or_else(|| anyhow!("`keyframes` must be an array of {{time, value, ease}}"))?;
        if list.is_empty() {
            bail!("`keyframes` is empty");
        }
        let mut c = clip.clone();
        if opt_bool(a, "replace")?.unwrap_or(false) {
            c.keyframes.remove(&prop);
        }
        for k in list {
            let time = req_f64(k, "time")?;
            let value = req_f64(k, "value")? as f32;
            if time < c.start - 0.01 || time > c.end() + 0.01 {
                bail!("keyframe time {time} is outside the clip ({:.3}–{:.3})", c.start, c.end());
            }
            let local = (time - c.start).clamp(0.0, c.duration);
            c.set_key(prop, local, value);
            if let Some(e) = opt_str(k, "ease")? {
                let ease = Ease::parse(e).ok_or_else(|| anyhow!("unknown ease `{e}`; use linear, smooth, ease_in, ease_out or hold"))?;
                let i = c.key_index(prop, local).expect("just set");
                c.keyframes.get_mut(&prop).expect("present")[i].ease = ease;
            }
        }
        *self.project.clip_mut(id).expect("exists") = c;
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(id) })))
    }

    fn clear_keyframes(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let c = self.project.clip_mut(id).ok_or_else(|| anyhow!("no clip with id {id}"))?;
        let at = c.start;
        let props: Vec<Prop> = match opt_str(a, "property")? {
            Some(s) => vec![Prop::parse(s).ok_or_else(|| anyhow!("unknown property `{s}`"))?],
            None => Prop::ALL.to_vec(),
        };
        let time = opt_f64(a, "keep_value_at")?.unwrap_or(at);
        for p in props {
            c.clear_keys(p, time);
        }
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(id) })))
    }

    fn apply_motion_preset(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let name = req_str(a, "preset")?;
        let preset = MotionPreset::parse(name).ok_or_else(|| {
            let names: Vec<_> = MotionPreset::ALL.iter().map(|p| p.name()).collect();
            anyhow!("unknown preset `{name}`; use one of {names:?}")
        })?;
        let clip = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        self.check_prop(&clip, if preset.uses_rotation() { Prop::Rotation } else { Prop::Scale })?;
        preset.apply(self.project.clip_mut(id).expect("exists"));
        Ok(text_json(&json!({ "clip": self.clip_json_by_id(id) })))
    }

    fn set_transition(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        if self.project.clip(id).is_none() {
            bail!("no clip with id {id}");
        }
        let to = match opt_str(a, "at")?.unwrap_or("start") {
            "start" => id,
            "end" => self.project.next_adjacent(id).ok_or_else(|| anyhow!("no clip directly after clip {id} on its track"))?,
            other => bail!("`at` must be start or end, not `{other}`"),
        };
        let kind = req_str(a, "kind")?;
        let tr = if kind.eq_ignore_ascii_case("none") {
            None
        } else {
            let k = TransitionKind::parse(kind).ok_or_else(|| {
                let names: Vec<_> = TransitionKind::ALL.iter().map(|k| k.name()).collect();
                anyhow!("unknown transition `{kind}`; use none or one of {names:?}")
            })?;
            Some(Transition { kind: k, duration: opt_f64(a, "duration")?.unwrap_or(0.5) })
        };
        self.project.set_transition(to, tr).map_err(|e| anyhow!(e))?;
        Ok(text_json(&json!({ "incoming_clip": self.clip_json_by_id(to) })))
    }

    // ── smart audio ──────────────────────────────────────────────────────────

    fn auto_duck(&mut self, a: &Value) -> Result<Vec<Value>> {
        if opt_bool(a, "remove")?.unwrap_or(false) {
            let n = smart_audio::clear_ducking(&mut self.project);
            return Ok(vec![text(format!("Removed ducking from {n} music clip(s)."))]);
        }
        if let Some(db) = opt_f64(a, "amount_db")? {
            self.project.audio.duck_db = (-(db.abs()) as f32).clamp(-40.0, -1.0);
        }
        if let Some(s) = opt_f64(a, "sensitivity")? {
            // 0 = only loud speech … 1 = very quiet speech.
            self.project.audio.duck_sensitivity = (1.0 - s.clamp(0.0, 1.0) as f32).clamp(0.03, 0.5);
        }
        // Make sure every voice clip's waveform is available.
        let voice_media: Vec<MediaItem> = smart_audio::voice_clips(&self.project)
            .iter()
            .filter_map(|c| c.media_id().and_then(|m| self.project.media(m)).cloned())
            .collect();
        for m in voice_media {
            if !self.peaks.contains_key(&m.id) {
                let peaks = ffmpeg::waveform(&m.path)?;
                self.peaks.insert(m.id, Arc::new(peaks));
            }
        }
        let peaks = self.peaks.clone();
        let opts = smart_audio::DuckOptions::from_mix(&self.project.audio);
        let report = smart_audio::auto_duck(&mut self.project, &|id| peaks.get(&id).cloned(), &opts).map_err(|e| anyhow!(e))?;
        Ok(text_json(&json!({
            "voice_clips": report.voice_clips,
            "music_clips": report.music_clips,
            "amount_db": self.project.audio.duck_db,
            "speech_passages": report.speech.iter().map(|(s, e)| json!([round3(*s), round3(*e)])).collect::<Vec<_>>(),
            "hint": "Music clips now have volume keyframes; inspect with get_project, tweak with set_keyframes(property=volume), or call auto_duck(remove=true).",
        })))
    }

    fn set_clip_audio(&mut self, a: &Value) -> Result<Vec<Value>> {
        let id = req_id(a, "clip_id")?;
        let clip = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        if !clip.media_id().and_then(|m| self.project.media(m)).is_some_and(|m| m.has_sound()) {
            bail!("clip {id} has no audio");
        }
        if let Some(r) = opt_str(a, "role")? {
            let role = AudioRole::parse(r).ok_or_else(|| anyhow!("role must be auto, voice, music or effect"))?;
            self.project.clip_mut(id).expect("exists").audio_role = role;
        }
        if let Some(n) = opt_str(a, "noise_reduction")? {
            let denoise = if n.eq_ignore_ascii_case("off") {
                None
            } else {
                let level = NoiseLevel::parse(n).ok_or_else(|| anyhow!("noise_reduction must be off, light, medium or strong"))?;
                let floor_db = match clip.denoise {
                    Some(d) => d.floor_db,
                    None => smart_audio::measure_noise_floor(&self.project, &clip)?,
                };
                Some(Denoise { level, floor_db })
            };
            self.project.clip_mut(id).expect("exists").denoise = denoise;
        }
        let c = self.project.clip(id).expect("exists");
        Ok(text_json(&json!({
            "clip_id": id,
            "role": self.project.audio_role(c).name(),
            "role_setting": c.audio_role.name(),
            "noise_reduction": c.denoise.map(|d| json!({ "level": d.level.name(), "noise_floor_db": d.floor_db })),
        })))
    }

    fn measure_loudness(&mut self) -> Result<Vec<Value>> {
        let l = smart_audio::measure_loudness(&self.project)?;
        let mut out = json!({
            "integrated_lufs": (l.integrated * 10.0).round() / 10.0,
            "loudness_range_lu": (l.range * 10.0).round() / 10.0,
            "true_peak_dbfs": (l.peak * 10.0).round() / 10.0,
            "note": "Measured before loudness normalisation.",
        });
        if let Some(t) = self.project.audio.loudness {
            out["export_target_lufs"] = json!(t);
            out["export_gain_db"] = json!(((t - l.integrated) * 10.0).round() / 10.0);
        }
        Ok(text_json(&out))
    }

    // ── AI ───────────────────────────────────────────────────────────────────

    fn auto_reframe(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::{openrouter, reframe};
        let settings = openrouter::AiSettings::load();
        let client = openrouter::Client::from_settings(&settings).map_err(|_| {
            anyhow!("no OpenRouter API key: set OPENROUTER_API_KEY in the MCP server's environment (the `env` block in .mcp.json) or save a key in RustCut → AI → OpenRouter settings")
        })?;
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.vision_model);
        if let Some(s) = opt_str(a, "aspect")? {
            self.project.aspect = parse_aspect(s)?;
        }
        let opts = reframe::ReframeOptions {
            subject: opt_str(a, "subject")?.unwrap_or("").to_string(),
            framing: match opt_str(a, "framing")? {
                Some(f) => reframe::Framing::parse(f).ok_or_else(|| anyhow!("framing must be wide, medium or tight"))?,
                None => reframe::Framing::Wide,
            },
            smoothness: opt_f64(a, "smoothness")?.unwrap_or(0.6).clamp(0.0, 1.0) as f32,
        };
        let candidates: Vec<Id> = match a.get("clip_id") {
            Some(v) if !v.is_null() => vec![req_id(a, "clip_id")?],
            _ => self.project.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter().map(|c| c.id)).collect(),
        };
        let mut results = Vec::new();
        let mut skipped = Vec::new();
        let mut total_cost = 0.0;
        let cancel = AtomicBool::new(false);
        let mut notify = progress_notifier(&mut self.out, progress);
        let n = candidates.len();
        for (i, id) in candidates.iter().copied().enumerate() {
            let Some(clip) = self.project.clip(id).cloned() else { bail!("no clip with id {id}") };
            let visual = clip.media_id().and_then(|m| self.project.media(m)).is_some_and(|m| m.kind != MediaKind::Audio);
            if !visual || self.project.track_kind_for(&clip) != TrackKind::Video {
                skipped.push(json!({ "clip_id": id, "reason": "not a video or image clip" }));
                continue;
            }
            if !reframe::has_room(&self.project, &clip, opts.framing) {
                skipped.push(json!({ "clip_id": id, "reason": "already fits the canvas (change aspect or use framing=medium/tight)" }));
                continue;
            }
            let mut report = |p: f32, s: &str| notify((i as f32 + p) / n as f32, s);
            match reframe::analyze_clip(&self.project, id, &opts.subject, &client, &model, &cancel, &mut report)? {
                Some(analysis) => {
                    total_cost += analysis.usage.cost;
                    let plan = reframe::apply_analysis(&mut self.project, id, &analysis, &opts)?;
                    results.push(json!({
                        "clip_id": id,
                        "subject": analysis.subject,
                        "frames": analysis.detections.len(),
                        "found": analysis.found(),
                        "scale": plan.scale,
                        "x_keyframes": plan.x.len(),
                        "y_keyframes": plan.y.len(),
                        "cost_usd": analysis.usage.cost,
                    }));
                }
                None => bail!("cancelled"),
            }
        }
        if results.is_empty() && !skipped.is_empty() {
            bail!("nothing reframed: {}", serde_json::to_string(&skipped)?);
        }
        Ok(text_json(&json!({
            "aspect": self.project.aspect.short_label(),
            "model": model,
            "reframed": results,
            "skipped": skipped,
            "total_cost_usd": total_cost,
            "hint": "Check with render_frame; fine-tune with set_keyframes(property=x|y) or undo.",
        })))
    }

    fn run_image(&mut self, a: &Value, job: crate::ai::image_gen::Job, progress: Option<Value>) -> Result<crate::ai::image_gen::Output> {
        let (settings, client) = self.ai_client()?;
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.image_model);
        let dir = crate::ai::video_gen::output_dir(self.path.as_deref());
        let mut notify = progress_notifier(&mut self.out, progress);
        notify(0.1, "Generating image…");
        crate::ai::image_gen::run(&client, &self.project, &job, &model, &dir)
    }

    fn generate_thumbnail(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::image_gen::{Job, ThumbStyle};
        let style = match opt_str(a, "style")? {
            Some(s) => ThumbStyle::parse(s).ok_or_else(|| anyhow!("style must be bold, clean or dramatic"))?,
            None => ThumbStyle::Bold,
        };
        let time = opt_f64(a, "time")?.unwrap_or(0.0).clamp(0.0, self.project.duration().max(0.0));
        let job = Job::Thumbnail {
            time,
            headline: opt_str(a, "headline")?.unwrap_or("").to_string(),
            style,
            extra: opt_str(a, "prompt")?.unwrap_or("").to_string(),
        };
        let out = self.run_image(a, job, progress)?;
        self.project.thumbnail = Some(out.path.clone());
        Ok(text_json(&json!({
            "file": out.path,
            "size": [out.info.width, out.info.height],
            "model": out.model,
            "cost_usd": out.cost,
            "hint": "Saved as the project's thumbnail; export_video copies it next to the video as <name>-thumbnail.png.",
        })))
    }

    fn generate_still(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::image_gen::{self, Job, StillKind};
        let kind_s = req_str(a, "kind")?;
        let kind = StillKind::parse(kind_s).ok_or_else(|| anyhow!("kind must be title_card, lower_third or sticker, not `{kind_s}`"))?;
        let prompt = req_str(a, "prompt")?.trim().to_string();
        let start = opt_f64(a, "start")?.unwrap_or(0.0).max(0.0);
        let place = opt_bool(a, "place")?.unwrap_or(true);
        let out = self.run_image(a, Job::Still { kind, prompt }, progress)?;
        let (media, clip) = if place {
            image_gen::place_still(&mut self.project, kind, &out, start)
        } else {
            (self.project.add_media(out.info.clone().into_media_item(out.path.clone())), None)
        };
        Ok(text_json(&json!({
            "file": out.path,
            "media_id": media,
            "clip": clip.map(|c| self.clip_json_by_id(c)),
            "transparent": kind.transparent(),
            "model": out.model,
            "cost_usd": out.cost,
        })))
    }

    fn background_fill(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::image_gen::{self, Job};
        let id = req_id(a, "clip_id")?;
        let clip = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        if opt_bool(a, "remove")?.unwrap_or(false) {
            self.project.clip_mut(id).unwrap().background_plate = None;
            return Ok(vec![text("Background fill removed.")]);
        }
        if !image_gen::needs_plate(&self.project, &clip) && !opt_bool(a, "force")?.unwrap_or(false) {
            bail!("clip {id} already covers the whole canvas, so a background would not show (pass force=true to make one anyway)");
        }
        let out = self.run_image(a, Job::Plate { clip: id }, progress)?;
        let media = image_gen::apply_plate(&mut self.project, id, &out).ok_or_else(|| anyhow!("clip {id} was removed"))?;
        Ok(text_json(&json!({
            "file": out.path,
            "plate_media_id": media,
            "clip_id": id,
            "model": out.model,
            "cost_usd": out.cost,
            "hint": "Check with render_frame. Remove with background_fill(clip_id, remove=true).",
        })))
    }

    fn storyboard_json(&self) -> Value {
        let Some(sb) = &self.storyboard else { return json!(null) };
        json!({
            "title": sb.title,
            "aspect": sb.aspect.short_label(),
            "style": sb.style,
            "script": sb.script,
            "music_suggestion": sb.music,
            "total_seconds": sb.total_seconds(),
            "folder": sb.dir,
            "spent_usd": (sb.spent * 1000.0).round() / 1000.0,
            "shots": sb.shots.iter().enumerate().map(|(i, s)| json!({
                "shot": i + 1,
                "seconds": s.seconds,
                "description": s.description,
                "motion": s.motion,
                "caption": s.caption,
                "frame": s.frame,
                "approved": s.approved,
            })).collect::<Vec<_>>(),
        })
    }

    fn storyboard_draw(&mut self, client: &crate::ai::openrouter::Client, model: &str, progress: Option<Value>) -> Result<()> {
        let mut sb = self.storyboard.take().ok_or_else(|| anyhow!("no storyboard yet: call storyboard_plan"))?;
        let cancel = AtomicBool::new(false);
        let r = {
            let mut notify = progress_notifier(&mut self.out, progress);
            crate::ai::storyboard::generate_missing_frames(client, &mut sb, model, &cancel, &mut |p, s| notify(p, s))
        };
        self.storyboard = Some(sb);
        r
    }

    fn storyboard_plan(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::storyboard::{self, Storyboard};
        let (settings, client) = self.ai_client()?;
        if let Some(path) = opt_str(a, "open")? {
            self.storyboard = Some(Storyboard::load(Path::new(path))?);
        } else {
            let prompt = req_str(a, "prompt")?;
            let seconds = opt_f64(a, "seconds")?.unwrap_or(30.0).clamp(6.0, 120.0);
            let aspect = match opt_str(a, "aspect")? {
                Some(s) => parse_aspect(s)?,
                None => Aspect::Portrait,
            };
            let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.edit_model.clone());
            let root = crate::ai::video_gen::output_dir(self.path.as_deref());
            self.storyboard = Some(storyboard::plan(&client, &model, prompt, seconds, aspect, &root)?);
        }
        if let (Some(img), Some(sb)) = (opt_str(a, "character_image")?, self.storyboard.as_mut()) {
            if !Path::new(img).is_file() {
                bail!("character_image not found: {img}");
            }
            sb.character = Some(PathBuf::from(img));
            let _ = sb.save();
        }
        if opt_bool(a, "draw_frames")?.unwrap_or(true) {
            self.storyboard_draw(&client, &settings.image_model, progress)?;
        }
        let mut out = self.storyboard_json();
        out["next"] = json!("Look at the frames, then storyboard_update_shot (approve / regenerate / edit / swap image / move / remove) and storyboard_produce to animate the approved shots.");
        Ok(text_json(&out))
    }

    fn storyboard_update_shot(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        let n = self.storyboard.as_ref().ok_or_else(|| anyhow!("no storyboard yet: call storyboard_plan"))?.shots.len();
        let shot = req_f64(a, "shot")? as usize;
        if shot == 0 || shot > n {
            bail!("shot must be 1–{n}");
        }
        let i = shot - 1;
        let mut regenerate = opt_bool(a, "regenerate")?.unwrap_or(false);
        {
            let sb = self.storyboard.as_mut().unwrap();
            if opt_bool(a, "remove")?.unwrap_or(false) {
                if n == 1 {
                    bail!("can't remove the only shot");
                }
                sb.remove_shot(i);
                let _ = sb.save();
                return Ok(text_json(&self.storyboard_json()));
            }
            let s = &mut sb.shots[i];
            if let Some(d) = opt_str(a, "description")? {
                s.description = d.to_string();
                s.video = None;
            }
            if let Some(c) = opt_str(a, "caption")? {
                s.caption = c.to_string();
            }
            if let Some(m) = opt_str(a, "motion")? {
                s.motion = m.to_string();
                s.video = None;
            }
            if let Some(lead) = opt_bool(a, "lead")? {
                s.no_lead = !lead;
            }
            if let Some(x) = opt_f64(a, "seconds")? {
                s.seconds = x.clamp(crate::ai::storyboard::MIN_SHOT, crate::ai::storyboard::MAX_SHOT);
            }
            if let Some(img) = opt_str(a, "image_path")? {
                let src = Path::new(img);
                let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
                let dest = sb.dir.join(format!("shot-{shot:02}-own.{ext}"));
                std::fs::create_dir_all(&sb.dir)?;
                std::fs::copy(src, &dest).with_context(|| format!("copying {img}"))?;
                let s = &mut sb.shots[i];
                s.frame = Some(dest);
                s.approved = true;
                s.video = None;
                regenerate = false;
            }
            if regenerate {
                sb.shots[i].frame = None;
            }
            if let Some(ap) = opt_bool(a, "approved")? {
                if ap && sb.shots[i].frame.is_none() && !regenerate {
                    bail!("shot {shot} has no frame to approve yet");
                }
                sb.shots[i].approved = ap;
            }
            if let Some(to) = opt_f64(a, "move_to")? {
                let to = (to as usize).clamp(1, n) - 1;
                sb.move_shot(i, to);
            }
            let _ = sb.save();
        }
        if regenerate {
            let (settings, client) = self.ai_client()?;
            let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.image_model);
            self.storyboard_draw(&client, &model, progress)?;
        }
        Ok(text_json(&self.storyboard_json()))
    }

    fn storyboard_produce(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::storyboard;
        let (settings, client) = self.ai_client()?;
        let mut sb = self.storyboard.clone().ok_or_else(|| anyhow!("no storyboard yet: call storyboard_plan"))?;
        let shots: Vec<usize> = match a.get("shots").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_u64).map(|x| x as usize).filter(|&x| x >= 1 && x <= sb.shots.len()).map(|x| x - 1).collect(),
            None => sb.approved(),
        };
        if shots.is_empty() {
            bail!("no shots to animate: approve some with storyboard_update_shot, or pass `shots`");
        }
        if let Some(i) = shots.iter().find(|&&i| sb.shots[i].frame.is_none()) {
            bail!("shot {} has no frame yet (regenerate it first)", i + 1);
        }
        let model_id = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.video_model);
        let m = self.video_model(&client, &model_id)?;
        let audio = opt_bool(a, "generate_audio")?.unwrap_or(false);
        let music = match opt_str(a, "music_path")? {
            Some(p) => Some((PathBuf::from(p), crate::media::ffmpeg::probe(Path::new(p)).with_context(|| format!("reading {p}"))?)),
            None => None,
        };
        let cancel = AtomicBool::new(false);
        let (rendered, errors, reused) = {
            let mut notify = progress_notifier(&mut self.out, progress);
            storyboard::produce_shots(&client, &mut sb, &shots, &m, audio, &cancel, &mut |p, s| notify(p, s))
        };
        if rendered.is_empty() {
            bail!("no shot could be animated: {}", errors.join("; "));
        }
        let cost: f64 = rendered.iter().filter_map(|(_, g)| g.cost).sum();
        sb.spent += cost;
        let _ = sb.save();
        let report = storyboard::assemble_any(&mut self.project, &sb, &rendered, music, &storyboard::ffmpeg_peaks);
        self.storyboard = Some(sb);
        Ok(text_json(&json!({
            "clips": report.clips.iter().map(|c| self.clip_json_by_id(*c)).collect::<Vec<_>>(),
            "duration": round3(report.duration),
            "captions": report.captions,
            "music_clip": report.music,
            "ducked": report.ducked,
            "loudness_lufs": storyboard::LOUDNESS,
            "video_cost_usd": (cost * 1000.0).round() / 1000.0,
            "storyboard_total_usd": self.storyboard.as_ref().map(|s| (s.spent * 1000.0).round() / 1000.0),
            "failed_shots": errors,
            "reused_shots": reused,
            "hint": "Check with render_frame, then export_video.",
        })))
    }

    fn detect_beats(&mut self, a: &Value) -> Result<Vec<Value>> {
        let media = match (opt_f64(a, "clip_id")?, opt_f64(a, "media_id")?) {
            (Some(c), _) => self.project.clip(c as Id).and_then(|c| c.media_id()).ok_or_else(|| anyhow!("no media clip with id {c}"))?,
            (None, Some(m)) => m as Id,
            (None, None) => crate::music::beat_clip(&self.project)
                .and_then(|c| c.media_id())
                .or_else(|| self.project.tracks.iter().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| &t.clips).find_map(|c| c.media_id()))
                .ok_or_else(|| anyhow!("give clip_id (the song clip) — no audio clip found"))?,
        };
        let path = self.project.media(media).map(|m| m.path.clone()).ok_or_else(|| anyhow!("no media with id {media}"))?;
        let analysis = crate::music::analyze(&path)?;
        self.project.music.insert(media, analysis.clone());
        let beats = crate::music::timeline_beats(&self.project);
        let sections = crate::music::timeline_sections(&self.project);
        Ok(text_json(&json!({
            "media_id": media,
            "bpm": analysis.bpm,
            "beats": analysis.beats.len(),
            "bars": analysis.beats.len() / analysis.beats_per_bar.max(1),
            "first_downbeat_timeline": beats.iter().find(|b| b.downbeat).map(|b| round3(b.time)),
            "sections": sections.iter().map(|s| json!({ "label": s.label, "start": round3(s.start), "end": round3(s.end), "energy": (s.energy * 100.0).round() / 100.0 })).collect::<Vec<_>>(),
            "hint": "Beats are on the timeline now (snapping). Next: beat_montage, beat_effect.",
        })))
    }

    fn beat_montage(&mut self, a: &Value) -> Result<Vec<Value>> {
        let song = crate::music::beat_clip(&self.project).map(|c| (c.start, c.end())).ok_or_else(|| anyhow!("detect beats on the song first (detect_beats)"))?;
        let media: Vec<Id> = match a.get("media_ids").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_u64).map(|x| x as Id).collect(),
            None => self.project.media.iter().filter(|m| m.kind != MediaKind::Audio).map(|m| m.id).collect(),
        };
        let every = match opt_str(a, "every")? {
            Some(s) => crate::music::Every::parse(s).ok_or_else(|| anyhow!("every must be auto, beat, two_beats, bar or two_bars"))?,
            None => crate::music::Every::Auto,
        };
        let from = opt_f64(a, "from")?.unwrap_or(song.0).max(song.0);
        let to = opt_f64(a, "to")?.unwrap_or(song.1).min(song.1);
        let ids = crate::music::beat_montage(&mut self.project, &media, from, to, every, opt_bool(a, "shuffle")?.unwrap_or(false))?;
        Ok(text_json(&json!({
            "clips": ids.len(),
            "first_clip_id": ids.first(),
            "from": round3(from),
            "to": round3(to),
            "hint": "Clips are muted so the song is the soundtrack. Add punch with beat_effect.",
        })))
    }

    fn beat_effect(&mut self, a: &Value) -> Result<Vec<Value>> {
        let ids: Vec<Id> = a.get("clip_ids").and_then(Value::as_array).ok_or_else(|| anyhow!("clip_ids is required"))?.iter().filter_map(Value::as_u64).map(|x| x as Id).collect();
        let effect = crate::music::BeatEffect::parse(req_str(a, "effect")?).ok_or_else(|| anyhow!("effect must be pulse, shake, flicker or step_zoom"))?;
        let every = match opt_str(a, "every")? {
            Some(s) => crate::music::Every::parse(s).ok_or_else(|| anyhow!("every must be beat, two_beats, bar or two_bars"))?,
            None => crate::music::Every::Beat,
        };
        let n = crate::music::apply_beat_effect(&mut self.project, &ids, effect, every, opt_f64(a, "amount")?.unwrap_or(0.6) as f32);
        if n == 0 {
            bail!("no beats fall inside those clips (detect_beats first, and check the clip times)");
        }
        Ok(text_json(&json!({ "keyframes": n, "clips": ids.len(), "effect": effect.label() })))
    }

    fn multicam_sync(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        let takes: Vec<Id> = a.get("media_ids").and_then(Value::as_array).ok_or_else(|| anyhow!("media_ids is required"))?.iter().filter_map(Value::as_u64).map(|x| x as Id).collect();
        let song = match opt_f64(a, "song_clip_id")? {
            Some(c) => c as Id,
            None => crate::multicam::song_clip(&self.project).ok_or_else(|| anyhow!("put the song on an audio track (or pass song_clip_id)"))?,
        };
        let synced = {
            let mut notify = progress_notifier(&mut self.out, progress);
            let mut project = std::mem::take(&mut self.project);
            let r = crate::multicam::sync_takes(&mut project, song, &takes, &mut |p, s| notify(p, s));
            self.project = project;
            r?
        };
        Ok(text_json(&json!({
            "takes": synced.iter().map(|s| json!({
                "angle": s.angle,
                "media_id": s.media,
                "clip_id": s.clip,
                "starts_at": round3(s.offset),
                "confidence": (s.confidence * 10.0).round() / 10.0,
                "sure": s.confidence >= 1.5,
            })).collect::<Vec<_>>(),
            "hint": "Each take is on its own muted 'Cam N' track. Next: multicam_switch (auto on beats/bars, or manual switch points).",
        })))
    }

    fn multicam_switch(&mut self, a: &Value) -> Result<Vec<Value>> {
        let song = crate::multicam::song_clip(&self.project).and_then(|c| self.project.clip(c)).map(|c| (c.start, c.end()));
        let (s0, e0) = song.unwrap_or((0.0, self.project.duration()));
        let from = opt_f64(a, "from")?.unwrap_or(s0);
        let to = opt_f64(a, "to")?.unwrap_or(e0);
        let switches: Vec<(f64, u32)> = match a.get("switches").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(|x| Some((x.get("time")?.as_f64()?, x.get("angle")?.as_u64()? as u32)))
                .collect(),
            None => {
                let every = match opt_str(a, "every")? {
                    Some(s) => crate::music::Every::parse(s).ok_or_else(|| anyhow!("every must be auto, beat, two_beats, bar, two_bars or section"))?,
                    None => crate::music::Every::Bar,
                };
                crate::multicam::auto_switches(&self.project, from, to, every)
            }
        };
        let n = crate::multicam::build_edit(&mut self.project, &switches, from, to)?;
        Ok(text_json(&json!({ "shots": n, "switches": switches.len(), "from": round3(from), "to": round3(to) })))
    }

    fn add_lyrics(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::lyrics::{self, Input, LyricStyle};
        let song = match opt_f64(a, "song_clip_id")? {
            Some(c) => c as Id,
            None => crate::multicam::song_clip(&self.project).ok_or_else(|| anyhow!("put the song on an audio track (or pass song_clip_id)"))?,
        };
        let text = match (opt_str(a, "lyrics")?, opt_str(a, "lrc_path")?) {
            (Some(t), _) => Some(t.to_string()),
            (None, Some(p)) => Some(std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?),
            _ => None,
        };
        let input = text.map_or(Input::Transcribe, Input::Text);
        let style = match opt_str(a, "style")? {
            Some(s) => LyricStyle::parse(s).ok_or_else(|| anyhow!("style must be karaoke, lines, big_centered, pop_words or build_up"))?,
            None => LyricStyle::Line,
        };
        let y = match opt_str(a, "position")?.unwrap_or("bottom") {
            "top" => -0.30,
            "middle" | "center" => 0.0,
            _ => 0.30,
        };
        let cancel = AtomicBool::new(false);
        let lines = {
            let mut notify = progress_notifier(&mut self.out, progress);
            let language = opt_str(a, "language")?.unwrap_or("auto").to_string();
            lyrics::timed_lines(&self.project, song, &input, &language, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?
        };
        let n = lyrics::apply_lyrics(&mut self.project, &lines, style, y, opt_bool(a, "beat_snap")?.unwrap_or(true));
        Ok(text_json(&json!({
            "lines": lines.len(),
            "clips": n,
            "first_lines": lines.iter().take(6).map(|l| json!({ "start": round3(l.start), "end": round3(l.end), "text": l.text })).collect::<Vec<_>>(),
            "hint": "Lyrics are text clips on the 'Lyrics' caption track; fix any line with update_clip. export_lrc saves an .lrc file.",
        })))
    }

    fn export_lrc(&mut self, a: &Value) -> Result<Vec<Value>> {
        let path = PathBuf::from(req_str(a, "path")?);
        let mut lines = crate::lyrics::timeline_lines(&self.project);
        if lines.is_empty() {
            bail!("no lyrics on the timeline (add_lyrics first)");
        }
        if let Some(c) = crate::multicam::song_clip(&self.project).and_then(|c| self.project.clip(c)) {
            crate::lyrics::to_song_time(&mut lines, c.start, c.source_in, c.speed);
        }
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        std::fs::write(&path, crate::lyrics::to_lrc(&lines, &title))?;
        Ok(text_json(&json!({ "path": path, "lines": lines.len() })))
    }

    fn song_or(&self, a: &Value) -> Result<Id> {
        match opt_f64(a, "song_clip_id")? {
            Some(c) => Ok(c as Id),
            None => crate::multicam::song_clip(&self.project).ok_or_else(|| anyhow!("put the song on an audio track (or pass song_clip_id)")),
        }
    }

    fn add_visualizer(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::reactive::{self, Visualizer};
        let song = self.song_or(a)?;
        let kind = match opt_str(a, "kind")? {
            Some(s) => Visualizer::parse(s).ok_or_else(|| anyhow!("kind must be bars, circle, wave, line or scope"))?,
            None => Visualizer::Bars,
        };
        let color = match opt_str(a, "color")? {
            Some(h) => {
                let h = h.trim_start_matches('#');
                let v = u32::from_str_radix(h, 16).map_err(|_| anyhow!("color must be a hex colour like #00CDDE"))?;
                [(v >> 16) as u8, (v >> 8) as u8, v as u8]
            }
            None => [0, 205, 222],
        };
        let y = match opt_str(a, "position")?.unwrap_or("bottom") {
            "top" => -0.36,
            "middle" | "center" => 0.0,
            _ => 0.36,
        };
        let (path, from, start, dur, speed) = reactive::song_range(&self.project, song)?;
        let (cw, ch) = self.project.aspect.size_for(1080);
        let size = kind.size(cw, ch);
        let dir = crate::ai::video_gen::output_dir(self.path.as_deref());
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut out = dir.join(format!("visualizer-{}-{}.mov", kind.name(), crate::ai::video_gen::slug(&stem)));
        let mut n = 2;
        while out.exists() {
            out = dir.join(format!("visualizer-{}-{}-{n}.mov", kind.name(), crate::ai::video_gen::slug(&stem)));
            n += 1;
        }
        let info = reactive::render_visualizer(&path, from, dur * speed, kind, size, self.project.fps, color, &out)?;
        let (media, clip) = reactive::place_visualizer(&mut self.project, kind, &out, info, start, y);
        Ok(text_json(&json!({ "file": out, "media_id": media, "clip": clip.map(|c| self.clip_json_by_id(c)) })))
    }

    fn audio_react(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::reactive::{self, Band, React};
        let song = self.song_or(a)?;
        let ids: Vec<Id> = a.get("clip_ids").and_then(Value::as_array).ok_or_else(|| anyhow!("clip_ids is required"))?.iter().filter_map(Value::as_u64).map(|x| x as Id).collect();
        let effect = React::parse(req_str(a, "effect")?).ok_or_else(|| anyhow!("effect must be throb, glow, blur, shake, fade or wobble"))?;
        let band = match opt_str(a, "follow")? {
            Some(s) => Band::parse(s).ok_or_else(|| anyhow!("follow must be bass or loudness"))?,
            None => Band::Bass,
        };
        let (path, ..) = reactive::song_range(&self.project, song)?;
        let env = reactive::envelope(&path, band)?;
        let n = reactive::apply_reactive(&mut self.project, &ids, song, &env, effect, opt_f64(a, "amount")?.unwrap_or(0.6) as f32);
        Ok(text_json(&json!({ "keyframes": n, "clips": ids.len(), "effect": effect.label() })))
    }

    fn music_video_plan(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::music_video;
        let (settings, client) = self.ai_client()?;
        let song = self.song_or(a)?;
        let theme = req_str(a, "theme")?.to_string();
        if let Some(s) = opt_str(a, "aspect")? {
            self.project.aspect = parse_aspect(s)?;
        }
        let media = self.project.clip(song).and_then(|c| c.media_id()).ok_or_else(|| anyhow!("the song clip has no media"))?;
        if !self.project.music.contains_key(&media) {
            let path = self.project.media(media).map(|m| m.path.clone()).unwrap_or_default();
            let analysis = crate::music::analyze(&path)?;
            self.project.music.insert(media, analysis);
        }
        let clip = self.project.clip(song).cloned().ok_or_else(|| anyhow!("no song clip"))?;
        let range = match opt_str(a, "cover")?.unwrap_or("whole") {
            "best30" | "best_30" | "best" => music_video::best_window(&self.project, 30.0).unwrap_or((clip.start, clip.end())),
            _ => (clip.start, clip.end()),
        };
        let max_shots = opt_f64(a, "max_shots")?.unwrap_or(10.0).clamp(3.0, 24.0) as usize;
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.edit_model.clone());
        let root = crate::ai::video_gen::output_dir(self.path.as_deref());
        let cast: Vec<crate::ai::storyboard::CastMember> = a
            .get("cast")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|c| Some(crate::ai::storyboard::CastMember { name: c.get("name")?.as_str()?.to_string(), photo: c.get("photo")?.as_str()?.into() })).collect())
            .unwrap_or_default();
        for c in &cast {
            if !c.photo.is_file() {
                bail!("cast photo not found for {}: {}", c.name, c.photo.display());
            }
        }
        let mut sb = music_video::plan_cast(&client, &model, &theme, &self.project, song, range, max_shots, &root, &cast)?;
        sb.fresh_analysis = None;
        if let Some(img) = opt_str(a, "character_image")? {
            if !Path::new(img).is_file() {
                bail!("character_image not found: {img}");
            }
            sb.character = Some(PathBuf::from(img));
            let _ = sb.save();
        }
        self.storyboard = Some(sb);
        if opt_bool(a, "draw_frames")?.unwrap_or(true) {
            self.storyboard_draw(&client, &settings.image_model, progress)?;
        }
        let mut out = self.storyboard_json();
        if let Some(sb) = &self.storyboard {
            out["cuts"] = json!(sb.slots.len());
            out["range"] = json!([round3(range.0), round3(range.1)]);
            out["slots"] = json!(sb.slots.iter().map(|s| json!({ "start": round3(s.start), "end": round3(s.end), "shot": s.shot + 1, "section": s.section })).collect::<Vec<_>>());
        }
        out["next"] = json!("Review frames with storyboard_update_shot (approve/regenerate), then storyboard_produce to animate the approved shots and cut them on the bars.");
        Ok(text_json(&out))
    }

    fn best_section(&mut self, a: &Value) -> Result<Vec<Value>> {
        let seconds = opt_f64(a, "seconds")?.unwrap_or(30.0).clamp(5.0, 180.0);
        if crate::music::beat_clip(&self.project).is_none() {
            bail!("detect beats on the song first (detect_beats)");
        }
        let (s, e) = crate::ai::music_video::best_window(&self.project, seconds).ok_or_else(|| anyhow!("no song clip"))?;
        let applied = opt_bool(a, "apply")?.unwrap_or(false);
        let len = if applied { Some(crate::ai::music_video::apply_window(&mut self.project, s, e)) } else { None };
        Ok(text_json(&json!({
            "start": round3(s),
            "end": round3(e),
            "applied": applied,
            "new_duration": len.map(round3),
            "hint": if applied { "Trimmed (ripple) with song fades; undo restores the full song." } else { "Pass apply=true to trim the project to this window." },
        })))
    }

    fn auto_transitions(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::local;
        let decider = self.local_decider(a)?;
        let track = opt_f64(a, "track_index")?.map(|t| t as usize);
        if let Some(t) = track {
            if t >= self.project.tracks.len() {
                bail!("there is no track {t}");
            }
        }
        let cuts = local::describe_cuts(&self.project, track, opt_str(a, "style")?.unwrap_or(""));
        if cuts.is_empty() {
            bail!("no cuts between touching clips on visible video tracks");
        }
        let cancel = AtomicBool::new(false);
        let choices = {
            let mut notify = progress_notifier(&mut self.out, progress);
            local::choose_transitions(&cuts, &decider, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?
        };
        let apply = opt_bool(a, "apply")?.unwrap_or(true);
        let tally = if apply { local::apply_transitions(&mut self.project, &choices) } else { [0; 6] };
        Ok(text_json(&json!({
            "applied": apply,
            "model": decider.model,
            "cuts": cuts.iter().zip(&choices).map(|(c, (_, ch))| json!({ "time": round3(c.time), "clip_id": c.to, "choice": local::TRANSITION_OPTIONS[*ch] })).collect::<Vec<_>>(),
            "tally": local::TRANSITION_OPTIONS.iter().zip(tally).filter(|(_, n)| *n > 0).map(|(o, n)| json!({ "transition": o, "count": n })).collect::<Vec<_>>(),
        })))
    }

    fn cost_report(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::economics;
        let conn = economics::open()?;
        if let Some(n) = opt_f64(a, "videos_per_month")? {
            economics::set_videos_per_month(&conn, n)?;
        }
        let r = economics::build_report(&conn, &self.project, self.path.as_deref())?;
        economics::save_report(&conn, &r)?;
        let saved = match (opt_str(a, "format")?.unwrap_or("summary"), opt_str(a, "output")?) {
            ("html", Some(p)) => {
                std::fs::write(p, economics::to_html(&r))?;
                Some(p.to_string())
            }
            ("csv", Some(p)) => {
                std::fs::write(p, economics::to_csv(&r))?;
                Some(p.to_string())
            }
            (f @ ("html" | "csv"), None) => bail!("give `output` (a file path) for format {f}"),
            _ => None,
        };
        let detail = opt_bool(a, "detail")?.unwrap_or(false);
        Ok(text_json(&json!({
            "project": r.project,
            "duration": round3(r.duration),
            "rustcut_usd": (r.rustcut_total() * 1000.0).round() / 1000.0,
            "stacks": r.stacks.iter().enumerate().map(|(k, s)| json!({
                "name": s.name, "usd": (r.stack_total(k) * 100.0).round() / 100.0, "monthly_subscriptions": s.monthly(),
                "rustcut_saves": ((r.stack_total(k) - r.rustcut_total()) * 100.0).round() / 100.0,
            })).collect::<Vec<_>>(),
            "videos_per_month": r.videos_per_month,
            "estimated": r.estimated,
            "rows": r.rows.iter().map(|row| {
                let mut j = json!({ "scope": row.scope, "track": row.track, "start": round3(row.start), "duration": round3(row.duration), "clip": row.label,
                    "rustcut_usd": (row.rustcut() * 10000.0).round() / 10000.0,
                    "stacks_usd": (0..r.stacks.len()).map(|k| (row.stack(k) * 10000.0).round() / 10000.0).collect::<Vec<_>>() });
                if detail {
                    j["items"] = json!(row.items.iter().map(|i| json!({ "task": i.task, "qty": i.qty, "unit": i.unit, "rustcut_usd": i.rustcut,
                        "stacks_usd": i.stacks.iter().map(|c| match c { economics::Cost::Usd(v, _) => json!(v), economics::Cost::NotOffered => json!(null) }).collect::<Vec<_>>() })).collect::<Vec<_>>());
                }
                j
            }).collect::<Vec<_>>(),
            "saved": saved,
        })))
    }

    fn set_price(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::economics;
        let conn = economics::open()?;
        let tool = req_str(a, "tool")?;
        if let Some(m) = opt_f64(a, "monthly_usd")? {
            economics::set_subscription(&conn, tool, m)?;
        }
        if let Some(task) = opt_str(a, "task")? {
            let usd = opt_f64(a, "usd_per_unit")?.ok_or_else(|| anyhow!("usd_per_unit is required with task"))?;
            economics::set_rate(&conn, tool, task, opt_str(a, "unit")?.unwrap_or("unit"), usd, opt_str(a, "note")?.unwrap_or(""))?;
        }
        let stacks = economics::stacks(&conn)?;
        Ok(text_json(&json!({
            "stacks": stacks.iter().map(|s| json!({ "name": s.name, "subscriptions": s.subscriptions, "rates": s.rates.iter().map(|(t, (u, tool, n))| json!({ "task": t, "usd": u, "tool": tool, "note": n })).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        })))
    }

    fn elevenlabs(&self) -> Result<(crate::ai::openrouter::AiSettings, crate::ai::elevenlabs::Client)> {
        let settings = crate::ai::openrouter::AiSettings::load();
        let key = settings.elevenlabs_key().ok_or_else(|| anyhow!(crate::ai::elevenlabs::NO_KEY))?;
        Ok((settings, crate::ai::elevenlabs::Client::new(key)))
    }

    fn list_voices(&mut self) -> Result<Vec<Value>> {
        let (settings, client) = self.elevenlabs()?;
        let voices = client.voices()?;
        let sub = client.subscription().ok();
        Ok(text_json(&json!({
            "characters": settings.voices.iter().map(|p| json!({ "name": p.name, "voice_id": p.voice_id })).collect::<Vec<_>>(),
            "account_voices": voices.iter().map(|v| json!({ "name": v.name, "voice_id": v.voice_id, "category": v.category })).collect::<Vec<_>>(),
            "credits": sub.map(|s| json!({ "tier": s.tier, "used": s.used, "limit": s.limit })),
        })))
    }

    fn speak_line(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::ai::elevenlabs::{self, VoiceProfile, VoiceSettings};
        let (settings, client) = self.elevenlabs()?;
        let who = req_str(a, "voice")?;
        let profile = match settings.voice(who) {
            Some(p) => p,
            None => {
                // An account voice by name or id.
                let v = client.voices()?.into_iter().find(|v| v.voice_id == who || v.name.eq_ignore_ascii_case(who)).ok_or_else(|| anyhow!("no voice or character named {who} (see list_voices)"))?;
                VoiceProfile { name: v.name, voice_id: v.voice_id }
            }
        };
        let text = req_str(a, "text")?;
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.tts_model.clone());
        let s = VoiceSettings {
            stability: opt_f64(a, "stability")?.unwrap_or(0.5) as f32,
            similarity: opt_f64(a, "likeness")?.unwrap_or(0.8) as f32,
            style: opt_f64(a, "style")?.unwrap_or(0.0) as f32,
        };
        let dir = crate::ai::video_gen::output_dir(self.path.as_deref()).join("voice");
        let path = elevenlabs::speak_to_file(&client, &profile, text, &model, s, &dir, settings.tts_usd_per_char)?;
        let info = crate::media::ffmpeg::probe(&path)?;
        let duration = info.duration;
        let media = self.project.add_media(info.into_media_item(path.clone()));
        let mut clip_json = json!(null);
        if opt_bool(a, "place")?.unwrap_or(true) {
            let start = opt_f64(a, "start")?.unwrap_or(0.0).max(0.0);
            if let Some(mut clip) = self.project.new_media_clip(media, start) {
                clip.start = start;
                clip.audio_role = AudioRole::Voice;
                let id = clip.id;
                self.project.place_clip(clip, None);
                if opt_bool(a, "duck")?.unwrap_or(true) {
                    let paths: Vec<(Id, PathBuf)> = self.project.media.iter().map(|m| (m.id, m.path.clone())).collect();
                    let peaks_for = |m: Id| paths.iter().find(|(x, _)| *x == m).and_then(|(_, p)| crate::media::ffmpeg::waveform(p).ok()).map(Arc::new);
                    let opts = smart_audio::DuckOptions::from_mix(&self.project.audio);
                    let _ = smart_audio::auto_duck(&mut self.project, &peaks_for, &opts);
                }
                clip_json = self.clip_json_by_id(id);
            }
        }
        Ok(text_json(&json!({
            "file": path, "media_id": media, "duration": round3(duration), "voice": profile.name, "model": model,
            "characters": text.chars().count(), "estimated_usd": (text.chars().count() as f64 * settings.tts_usd_per_char * 10000.0).round() / 10000.0,
            "clip": clip_json,
        })))
    }

    fn clone_voice(&mut self, a: &Value) -> Result<Vec<Value>> {
        let (mut settings, client) = self.elevenlabs()?;
        let name = req_str(a, "name")?.trim().to_string();
        let files: Vec<PathBuf> = a.get("files").and_then(Value::as_array).ok_or_else(|| anyhow!("files is required"))?.iter().filter_map(Value::as_str).map(PathBuf::from).collect();
        for f in &files {
            if !f.is_file() {
                bail!("file not found: {}", f.display());
            }
        }
        let id = client.clone_voice(&name, opt_str(a, "description")?.unwrap_or("Created with RustCut"), &files)?;
        settings.voices.retain(|p| !p.name.eq_ignore_ascii_case(&name));
        settings.voices.push(crate::ai::elevenlabs::VoiceProfile { name: name.clone(), voice_id: id.clone() });
        settings.save()?;
        Ok(text_json(&json!({ "name": name, "voice_id": id, "hint": "Saved as a character; use speak_line with voice = this name." })))
    }

    fn lip_sync(&mut self, a: &Value) -> Result<Vec<Value>> {
        use crate::ai::lipsync;
        let settings = crate::ai::openrouter::AiSettings::load();
        let key = settings.fal_key().ok_or_else(|| anyhow!(lipsync::NO_KEY))?;
        let video = req_id(a, "video_clip_id")?;
        let voice = a.get("voice_clip_id").and_then(Value::as_u64);
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.lipsync_model.clone());
        let plan = lipsync::plan(&self.project, video, voice)?;
        let dir = crate::ai::video_gen::output_dir(self.path.as_deref()).join("lipsync");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let (path, info) = lipsync::lip_sync(&self.project, &plan, &key, &model, &dir, &cancel, &mut |_, _| {})?.ok_or_else(|| anyhow!("cancelled"))?;
        let id = lipsync::apply(&mut self.project, &plan, &path, info)?;
        let secs = plan.end - plan.start;
        Ok(text_json(&json!({
            "file": path, "start": round3(plan.start), "end": round3(plan.end), "model": model,
            "estimated_usd": (secs * lipsync::usd_per_second(&model) * 100.0).round() / 100.0,
            "clip": self.clip_json_by_id(id),
        })))
    }

    fn performance_shot(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::performance;
        let settings = crate::ai::openrouter::AiSettings::load();
        let key = settings.fal_key().ok_or_else(|| anyhow!(crate::ai::lipsync::NO_KEY))?;
        let video = req_id(a, "clip_id")?;
        let audio = a.get("audio_clip_id").and_then(Value::as_u64);
        let model = match opt_str(a, "model")? {
            Some("fast") | None => performance::MODELS[0].0.to_string(),
            Some("best") => performance::MODELS[1].0.to_string(),
            Some(other) => other.to_string(),
        };
        let plan = performance::plan(&self.project, video, audio)?;
        if opt_bool(a, "dry_run")?.unwrap_or(false) {
            return Ok(text_json(&json!({
                "start": round3(plan.start()), "end": round3(plan.end()), "model": model, "estimated_usd": (plan.estimate(&model) * 100.0).round() / 100.0,
                "segments": plan.segments.iter().map(|s| json!({ "start": round3(s.start), "end": round3(s.end), "video_seconds": s.video_seconds(), "lyrics": s.lyrics })).collect::<Vec<_>>(),
            })));
        }
        let sheet = opt_str(a, "character_sheet")?.map(PathBuf::from);
        let opts = performance::Options { model: &model, direction: opt_str(a, "direction")?.unwrap_or(""), sheet: sheet.as_deref() };
        let dir = crate::ai::video_gen::output_dir(self.path.as_deref()).join("lipsync");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let results = {
            let mut notify = progress_notifier(&mut self.out, progress);
            performance::perform(&self.project, &plan, &key, &opts, &dir, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?
        };
        let ids = performance::apply(&mut self.project, &plan, results)?;
        Ok(text_json(&json!({
            "start": round3(plan.start()), "end": round3(plan.end()), "model": model,
            "estimated_usd": (plan.estimate(&model) * 100.0).round() / 100.0,
            "clips": ids.iter().map(|&id| self.clip_json_by_id(id)).collect::<Vec<_>>(),
        })))
    }

    fn reshoot_clip(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::reshoot;
        let clip = req_id(a, "clip_id")?;
        let c = self.project.clip(clip).cloned().ok_or_else(|| anyhow!("no clip {clip}"))?;
        let src = reshoot::find_source(&self.project, clip);
        let description = opt_str(a, "description")?.map(str::to_string).unwrap_or_else(|| src.description());
        let motion = opt_str(a, "motion")?.map(str::to_string).unwrap_or_else(|| src.motion());
        let lead = opt_bool(a, "lead")?.unwrap_or(!src.no_lead());
        let (settings, client) = self.ai_client()?;
        let still = opt_str(a, "still")?.map(PathBuf::from);
        let dir = match still.as_ref().and_then(|s| s.parent()) {
            Some(d) => d.to_path_buf(),
            None => reshoot::work_dir(self.path.as_deref(), clip),
        };
        let mut sb = reshoot::one_shot(&self.project, &src, &description, &motion, c.duration, !lead, &dir);
        let mut spent = 0.0;
        match still {
            Some(s) => {
                if !s.is_file() {
                    bail!("still not found: {}", s.display());
                }
                sb.shots[0].frame = Some(s);
            }
            None => {
                let look = if opt_bool(a, "keep_look")?.unwrap_or(true) { reshoot::current_look(&self.project, clip, &dir).ok() } else { None };
                let (_, cost) = reshoot::draw(&client, &mut sb, &settings.image_model, look.as_deref())?;
                spent += cost;
            }
        }
        let frame = sb.shots[0].frame.clone();
        let shot_info = json!({ "storyboard_shot": src.shot.map(|i| i + 1), "uses_of_shot": src.storyboard.as_ref().zip(src.shot).map(|(sb, i)| sb.uses(i)), "was_lip_synced": src.performance });
        if opt_bool(a, "still_only")?.unwrap_or(false) {
            return Ok(text_json(&json!({ "still": frame, "description": description, "motion": motion, "cost_usd": (spent * 1000.0).round() / 1000.0, "source": shot_info,
                "hint": "Review the still, then call reshoot_clip again with still = this path to animate and replace the clip." })));
        }
        let models = crate::ai::video_gen::list_models(&client)?;
        let wanted = opt_str(a, "video_model")?.map(str::to_string).unwrap_or(settings.video_model.clone());
        let m = models.iter().find(|m| m.id == wanted && m.first_frame).or_else(|| models.iter().find(|m| m.first_frame)).cloned().ok_or_else(|| anyhow!("no image-to-video model available"))?;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let g = {
            let mut notify = progress_notifier(&mut self.out, progress);
            reshoot::animate(&client, &sb, &m, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?
        };
        spent += g.cost.unwrap_or(0.0);
        let id = reshoot::apply(&mut self.project, clip, &g.path, g.info.clone())?;
        Ok(text_json(&json!({ "still": frame, "video": g.path, "video_model": m.id, "cost_usd": (spent * 1000.0).round() / 1000.0, "source": shot_info,
            "clip": self.clip_json_by_id(id),
            "hint": if src.performance { "This clip was lip-synced: run performance_shot on the new clip id to redo it." } else { "" } })))
    }

    fn ai_client(&self) -> Result<(crate::ai::openrouter::AiSettings, crate::ai::openrouter::Client)> {
        let settings = crate::ai::openrouter::AiSettings::load();
        let client = crate::ai::openrouter::Client::from_settings(&settings).map_err(|_| {
            anyhow!("no OpenRouter API key: set OPENROUTER_API_KEY in the MCP server's environment (the `env` block in .mcp.json) or save a key in RustCut → AI → OpenRouter settings")
        })?;
        Ok((settings, client))
    }

    fn video_model(&self, client: &crate::ai::openrouter::Client, id: &str) -> Result<crate::ai::video_gen::VideoModel> {
        let models = crate::ai::video_gen::list_models(client)?;
        models.into_iter().find(|m| m.id == id).ok_or_else(|| anyhow!("unknown video model `{id}`; call list_video_models"))
    }

    fn list_video_models(&mut self) -> Result<Vec<Value>> {
        use crate::ai::video_gen;
        let (settings, client) = self.ai_client()?;
        let models = video_gen::list_models(&client)?;
        let rows: Vec<Value> = models
            .iter()
            .map(|m| {
                let d = m.pick_duration(5.0);
                let res = m.default_resolution();
                json!({
                    "id": m.id,
                    "durations": m.durations,
                    "resolutions": m.resolutions,
                    "aspect_ratios": m.aspect_ratios,
                    "first_frame": m.first_frame,
                    "last_frame": m.last_frame,
                    "audio": m.audio,
                    "example_cost": video_gen::estimate_cost(m, d, &res, "16:9", false, false)
                        .map(|(c, rough)| json!({ "usd": (c * 100.0).round() / 100.0, "for": format!("{d}s {res}"), "approximate": rough })),
                })
            })
            .collect();
        Ok(text_json(&json!({ "default_model": settings.video_model, "models": rows })))
    }

    fn placement(&self, a: &Value) -> Result<crate::ai::video_gen::Placement> {
        use crate::ai::video_gen::Placement;
        Ok(match opt_str(a, "place")?.unwrap_or("library") {
            "library" => Placement::Library,
            "end" => Placement::End,
            "at" => Placement::At(opt_f64(a, "start")?.unwrap_or(0.0).max(0.0)),
            other => bail!("place must be library, at or end, not `{other}`"),
        })
    }

    fn run_generation(
        &mut self,
        client: &crate::ai::openrouter::Client,
        req: crate::ai::video_gen::GenRequest,
        label: &str,
        placement: crate::ai::video_gen::Placement,
        progress: Option<Value>,
    ) -> Result<Vec<Value>> {
        use crate::ai::video_gen;
        let dir = video_gen::output_dir(self.path.as_deref());
        // B-roll is named after its prompt; extensions after the clip they extend.
        let stem = format!("ai-{}", video_gen::slug(if label == "b-roll" { &req.prompt } else { label }));
        let cancel = AtomicBool::new(false);
        let gen = {
            let mut notify = progress_notifier(&mut self.out, progress);
            video_gen::generate(client, &req, &dir, &stem, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?
        };
        let (media, clip) = video_gen::place_generated(&mut self.project, &gen, placement);
        Ok(text_json(&json!({
            "file": gen.path,
            "media_id": media,
            "clip": clip.map(|c| self.clip_json_by_id(c)),
            "duration": round3(gen.info.duration),
            "size": [gen.info.width, gen.info.height],
            "cost_usd": gen.cost,
            "job_id": gen.job_id,
        })))
    }

    fn generate_video(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::video_gen::GenRequest;
        let (settings, client) = self.ai_client()?;
        let model_id = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.video_model);
        let m = self.video_model(&client, &model_id)?;
        let prompt = opt_str(a, "prompt")?.unwrap_or("").trim().to_string();
        let start_time = opt_f64(a, "start_frame_time")?;
        if prompt.is_empty() && start_time.is_none() {
            bail!("give a `prompt` (and/or `start_frame_time` to start from the frame at that timeline time)");
        }
        if start_time.is_some() && !m.first_frame {
            bail!("{} can't start from an image; pick a model with first_frame support (list_video_models)", m.id);
        }
        let (cw, ch) = self.project.aspect.size_for(1080);
        let aspect = match opt_str(a, "aspect")? {
            Some(x) if m.aspect_ratios.iter().any(|r| r == x) => x.to_string(),
            Some(x) => bail!("{} supports aspect ratios {:?}, not {x}", m.id, m.aspect_ratios),
            None => m.pick_aspect(cw as f64, ch as f64),
        };
        let duration = m.pick_duration(opt_f64(a, "duration")?.unwrap_or(5.0));
        let resolution = match opt_str(a, "resolution")? {
            Some(r) if m.resolutions.iter().any(|x| x == r) => r.to_string(),
            Some(r) => bail!("{} supports resolutions {:?}, not {r}", m.id, m.resolutions),
            None => m.default_resolution(),
        };
        let first_frame = match start_time {
            Some(t) => Some((crate::export::render_frame_png(&self.project, t.max(0.0), 720)?, "image/png")),
            None => None,
        };
        let req = GenRequest {
            model: m.id.clone(),
            prompt,
            duration,
            resolution,
            aspect_ratio: aspect,
            generate_audio: opt_bool(a, "audio")?.unwrap_or(false) && m.audio != Some(false),
            first_frame,
            last_frame: None,
        };
        let placement = self.placement(a)?;
        self.run_generation(&client, req, "b-roll", placement, progress)
    }

    fn extend_clip(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::video_gen::{self, GenRequest, Placement};
        let (settings, client) = self.ai_client()?;
        let id = req_id(a, "clip_id")?;
        let clip = self.project.clip(id).cloned().ok_or_else(|| anyhow!("no clip with id {id}"))?;
        let media = clip
            .media_id()
            .and_then(|m| self.project.media(m))
            .cloned()
            .ok_or_else(|| anyhow!("clip {id} has no media"))?;
        if !media.has_video() {
            bail!("clip {id} is not a video or image clip");
        }
        let model_id = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.video_model);
        let m = self.video_model(&client, &model_id)?;
        if !m.first_frame {
            bail!("{} can't start from an image; pick a model with first_frame support (list_video_models)", m.id);
        }
        let bridge = opt_bool(a, "blend_into_next")?.unwrap_or(false);
        let next = self.project.next_adjacent(id);
        if bridge && (next.is_none() || !m.last_frame) {
            bail!("blend_into_next needs a clip directly after this one and a model with last_frame support");
        }
        let base = if bridge { video_gen::BRIDGE_PROMPT } else { video_gen::EXTEND_PROMPT };
        let prompt = match opt_str(a, "prompt")? {
            Some(p) if !p.trim().is_empty() => format!("{base} {}", p.trim()),
            _ => base.to_string(),
        };
        let first = video_gen::clip_last_frame(&self.project, &clip)?;
        let last = match (bridge, next) {
            (true, Some(n)) => {
                let nc = self.project.clip(n).cloned().expect("adjacent clip exists");
                Some((video_gen::clip_first_frame(&self.project, &nc)?, "image/jpeg"))
            }
            _ => None,
        };
        let req = GenRequest {
            model: m.id.clone(),
            prompt,
            duration: m.pick_duration(opt_f64(a, "duration")?.unwrap_or(4.0)),
            resolution: m.default_resolution(),
            aspect_ratio: m.pick_aspect(media.width.max(1) as f64, media.height.max(1) as f64),
            generate_audio: false,
            first_frame: Some((first, "image/jpeg")),
            last_frame: last,
        };
        self.run_generation(&client, req, &format!("extend {}", media.name), Placement::After(id), progress)
    }

    fn local_decider(&self, a: &Value) -> Result<crate::ai::local::Decider> {
        let settings = crate::ai::openrouter::AiSettings::load();
        let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.local_model);
        Ok(crate::ai::local::Decider::new(&settings.ollama_url, &model))
    }

    fn rough_cut(&mut self, a: &Value, progress: Option<Value>) -> Result<Vec<Value>> {
        use crate::ai::rough_cut::{self, RoughCutRequest};
        let local = match opt_str(a, "engine")?.unwrap_or("openrouter") {
            "local" | "ollama" => true,
            "openrouter" | "cloud" => false,
            other => bail!("engine must be openrouter or local, not `{other}`"),
        };
        let cancel = AtomicBool::new(false);
        let (plan, model) = if local {
            let decider = self.local_decider(a)?;
            let req = RoughCutRequest { prompt: req_str(a, "prompt")?.to_string(), target_seconds: opt_f64(a, "target_seconds")?, use_transcript: true, use_video: false };
            let mut notify = progress_notifier(&mut self.out, progress);
            let plan = crate::ai::local::rough_cut(&self.project, &req, &decider, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?;
            (plan, decider.model)
        } else {
            let (settings, client) = self.ai_client()?;
            let model = opt_str(a, "model")?.map(str::to_string).unwrap_or(settings.edit_model);
            let req = RoughCutRequest {
                prompt: req_str(a, "prompt")?.to_string(),
                target_seconds: opt_f64(a, "target_seconds")?,
                use_transcript: opt_bool(a, "use_transcript")?.unwrap_or(rough_cut::speech_setup().is_some()),
                use_video: opt_bool(a, "use_video")?.unwrap_or(true),
            };
            let mut notify = progress_notifier(&mut self.out, progress);
            let plan = rough_cut::analyze(&self.project, &req, &client, &model, &cancel, &mut |p, s| notify(p, s))?.ok_or_else(|| anyhow!("cancelled"))?;
            (plan, model)
        };
        let apply = opt_bool(a, "apply")?.unwrap_or(true);
        let before = self.project.duration();
        let removed = if apply { plan.apply(&mut self.project) } else { 0.0 };
        Ok(text_json(&json!({
            "applied": apply,
            "duration_before": round3(before),
            "duration_after": round3(if apply { self.project.duration() } else { plan.kept_seconds() }),
            "removed_seconds": round3(removed),
            "keep": plan.keep.iter().map(|k| json!({ "start": round3(k.start), "end": round3(k.end), "reason": k.reason })).collect::<Vec<_>>(),
            "summary": plan.summary,
            "titles": plan.titles,
            "description": plan.description,
            "chapters": plan.chapters_text(),
            "used_transcript_segments": plan.transcript_segments,
            "used_video": plan.sent_video,
            "no_picture_ranges": plan.no_picture.iter().map(|(a, b)| json!([round3(*a), round3(*b)])).collect::<Vec<_>>(),
            "kept_without_picture_seconds": round3(plan.kept_without_picture()),
            "model": model,
            "cost_usd": plan.usage.cost,
            "hint": if apply { "One undo reverts the whole cut." } else { "Call again with apply=true to cut, or adjust the prompt." },
        })))
    }

    fn export_srt(&mut self, a: &Value) -> Result<Vec<Value>> {
        let path = PathBuf::from(req_str(a, "output")?);
        let segments = captions::timeline_segments(&self.project);
        if segments.is_empty() {
            bail!("there are no captions or text clips to export");
        }
        std::fs::write(&path, captions::to_srt(&segments))?;
        Ok(vec![text(format!("Wrote {} subtitles to {}.", segments.len(), path.display()))])
    }
}

/// Sends MCP progress notifications (whole percents) when the client asked for them.
fn progress_notifier<W: Write>(out: &mut W, token: Option<Value>) -> impl FnMut(f32, &str) + '_ {
    let mut last = -1i64;
    move |p: f32, stage: &str| {
        let pct = (p * 100.0).round() as i64;
        if let (Some(token), true) = (&token, pct != last) {
            last = pct;
            let _ = write_msg(
                out,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": { "progressToken": token, "progress": pct, "total": 100, "message": format!("{stage} {pct}%") },
                }),
            );
        }
    }
}

// ── argument helpers ─────────────────────────────────────────────────────────

fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

fn text_json(v: &Value) -> Vec<Value> {
    vec![text(serde_json::to_string_pretty(v).unwrap_or_default())]
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn opt_f64(a: &Value, k: &str) -> Result<Option<f64>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|f| f.is_finite()).map(Some).ok_or_else(|| anyhow!("`{k}` must be a number")),
    }
}

fn req_f64(a: &Value, k: &str) -> Result<f64> {
    opt_f64(a, k)?.ok_or_else(|| anyhow!("`{k}` is required"))
}

fn opt_str<'a>(a: &'a Value, k: &str) -> Result<Option<&'a str>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| anyhow!("`{k}` must be a string")),
    }
}

fn req_str<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    opt_str(a, k)?.ok_or_else(|| anyhow!("`{k}` is required"))
}

fn opt_bool(a: &Value, k: &str) -> Result<Option<bool>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| anyhow!("`{k}` must be true or false")),
    }
}

fn req_id(a: &Value, k: &str) -> Result<Id> {
    a.get(k).and_then(Value::as_u64).ok_or_else(|| anyhow!("`{k}` must be a non-negative integer id"))
}

fn opt_track(a: &Value, p: &Project) -> Result<Option<usize>> {
    match a.get("track_index") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let i = v.as_u64().ok_or_else(|| anyhow!("`track_index` must be a non-negative integer"))? as usize;
            if i >= p.tracks.len() {
                bail!("track_index {i} does not exist (there are {} tracks)", p.tracks.len());
            }
            Ok(Some(i))
        }
    }
}

fn parse_aspect(s: &str) -> Result<Aspect> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "16:9" | "landscape" | "youtube" => Aspect::Landscape,
        "9:16" | "portrait" | "vertical" | "tiktok" | "reels" | "shorts" => Aspect::Portrait,
        "1:1" | "square" => Aspect::Square,
        "4:3" | "classic" => Aspect::Classic,
        "4:5" | "instagram" => Aspect::Social,
        _ => bail!("unknown aspect `{s}`; use 16:9, 9:16, 1:1, 4:3 or 4:5"),
    })
}

fn parse_fps(f: f64) -> Result<u32> {
    if !(1.0..=120.0).contains(&f) {
        bail!("fps must be between 1 and 120");
    }
    Ok(f.round() as u32)
}

fn parse_filter(s: &str) -> Result<FilterPreset> {
    let key: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    Ok(match key.as_str() {
        "none" | "" => FilterPreset::None,
        "bw" | "blackwhite" | "blackandwhite" | "mono" | "monochrome" | "grayscale" | "greyscale" => FilterPreset::BlackWhite,
        "vintage" | "sepia" => FilterPreset::Vintage,
        "warm" => FilterPreset::Warm,
        "cool" => FilterPreset::Cool,
        "vivid" => FilterPreset::Vivid,
        "matte" => FilterPreset::Matte,
        _ => bail!("unknown filter `{s}`; use none, bw, vintage, warm, cool, vivid or matte"),
    })
}

/// Parses `#RRGGBB` or `#RRGGBBAA`.
fn parse_color(s: &str) -> Result<[u8; 4]> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16);
    let bad = || anyhow!("colour `{s}` must look like #RRGGBB or #RRGGBBAA");
    match h.len() {
        6 | 8 if h.is_ascii() => {
            let a = if h.len() == 8 { byte(6).map_err(|_| bad())? } else { 255 };
            Ok([byte(0).map_err(|_| bad())?, byte(2).map_err(|_| bad())?, byte(4).map_err(|_| bad())?, a])
        }
        _ => Err(bad()),
    }
}

/// `null` / "none" removes an optional colour.
fn opt_color(a: &Value, k: &str) -> Result<Option<Option<[u8; 4]>>> {
    match a.get(k) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("none") => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(parse_color(s)?))),
        Some(_) => bail!("`{k}` must be a colour string or null"),
    }
}

fn apply_text_args(style: &mut TextStyle, a: &Value) -> Result<()> {
    if let Some(v) = opt_f64(a, "font_size")? {
        style.font_size = v.clamp(4.0, 600.0) as f32;
    }
    if let Some(v) = opt_bool(a, "bold")? {
        style.bold = v;
    }
    if let Some(Some(c)) = opt_color(a, "color")? {
        style.color = c;
    }
    if let Some(c) = opt_color(a, "background")? {
        style.background = c;
    }
    if let Some(c) = opt_color(a, "outline")? {
        style.outline = c;
    }
    Ok(())
}


// ── tool catalogue ───────────────────────────────────────────────────────────

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str], annotations: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": annotations,
    })
}

fn tool_definitions() -> Vec<Value> {
    let ro = json!({ "readOnlyHint": true, "openWorldHint": false });
    let edit = json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false });
    let destructive = json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false });
    let num = |d: &str| json!({ "type": "number", "description": d });
    let int = |d: &str| json!({ "type": "integer", "minimum": 0, "description": d });
    let string = |d: &str| json!({ "type": "string", "description": d });
    let boolean = |d: &str| json!({ "type": "boolean", "description": d });
    let color = |d: &str| json!({ "type": ["string", "null"], "description": d });
    let aspect = json!({ "type": "string", "enum": ["16:9", "9:16", "1:1", "4:3", "4:5"], "description": "Canvas ratio" });
    let filter = json!({ "type": "string", "enum": ["none", "bw", "vintage", "warm", "cool", "vivid", "matte"], "description": "Colour filter preset" });
    let track = int("Target track index from get_project (optional)");
    let text_props = |mut base: Value| {
        let m = base.as_object_mut().expect("object");
        m.insert("font_size".into(), num("Font size in px relative to a 1080 px short side (default 72)"));
        m.insert("bold".into(), boolean("Bold font"));
        m.insert("color".into(), color("Text colour, #RRGGBB or #RRGGBBAA"));
        m.insert("background".into(), color("Background box colour (#RRGGBBAA), or null for none"));
        m.insert("outline".into(), color("Outline colour, or null for none"));
        base
    };

    vec![
        tool("get_project", "Get project", "Returns the whole project: settings, media (with ids), tracks and clips (with ids, timing and every property).", json!({}), &[], ro.clone()),
        tool("new_project", "New project", "Replaces the in-memory project with an empty one (undoable).", json!({ "name": string("Project name"), "aspect": aspect, "fps": num("Frames per second (default 30)") }), &[], destructive.clone()),
        tool("open_project", "Open project", "Loads a .rcproj project file.", json!({ "path": string("Absolute path to a .rcproj file") }), &["path"], destructive.clone()),
        tool("save_project", "Save project", "Saves the project as .rcproj (openable in the RustCut desktop app). Without `path`, saves to the last used path.", json!({ "path": string("Absolute output path ending in .rcproj") }), &[], edit.clone()),
        tool("update_project", "Update project settings", "Changes the project name, canvas ratio, frame rate or export loudness target.", json!({
            "name": string("Project name"),
            "aspect": aspect,
            "fps": num("Frames per second"),
            "loudness_target": { "type": ["number", "string", "null"], "description": "Normalise the export to this integrated loudness in LUFS: -14 (YouTube/TikTok/Spotify), -16 (podcasts/Apple), -23 (broadcast); \"off\" or null to disable" },
        }), &[], edit.clone()),
        tool("import_media", "Import media", "Adds video, image or audio files to the media library and returns their media ids, durations and sizes. Does not put them on the timeline (use add_clip).", json!({ "paths": { "type": "array", "items": { "type": "string" }, "description": "Absolute file paths" } }), &["paths"], edit.clone()),
        tool("remove_media", "Remove media", "Removes a media item and every clip that uses it.", json!({ "media_id": int("Media id") }), &["media_id"], destructive.clone()),
        tool("add_clip", "Add media clip", "Places a media item on the timeline. With no `start`/`track_index`, video and images are appended to the end of the main track; audio goes on an audio track at 0. If the spot is taken, a free or new track of the right kind is used.", json!({
            "media_id": int("Media id from import_media"),
            "start": num("Timeline position in seconds"),
            "duration": num("Timeline length in seconds (defaults to the whole source; images 5 s)"),
            "source_in": num("Offset into the source file in seconds (trim the beginning)"),
            "track_index": track.clone(),
        }), &["media_id"], edit.clone()),
        tool("add_text", "Add text", "Adds a text overlay. Presets: Default, Title, Subtitle, Caption, Neon, Lower third.", text_props(json!({
            "content": string("The text; may contain newlines"),
            "start": num("Timeline position in seconds (default 0)"),
            "duration": num("Length in seconds (default 3)"),
            "preset": string("Style preset name"),
            "x": num("Horizontal centre offset as a fraction of canvas width (0 = centred)"),
            "y": num("Vertical centre offset as a fraction of canvas height (0 = centred, 0.35 = lower third)"),
            "fade_in": num("Fade-in seconds"),
            "fade_out": num("Fade-out seconds"),
            "track_index": track.clone(),
        })), &["content"], edit.clone()),
        tool("update_clip", "Update clip", "Changes any property of a clip. Only the fields you pass are changed. Moving onto an occupied spot puts the clip on another track.", text_props(json!({
            "clip_id": int("Clip id"),
            "start": num("New timeline position (seconds)"),
            "duration": num("New timeline length (seconds); limited by the source length"),
            "source_in": num("Offset into the source (seconds)"),
            "track_index": track.clone(),
            "speed": num("Playback speed 0.1–10 (changes the timeline length)"),
            "x": num("Horizontal centre offset, fraction of canvas width"),
            "y": num("Vertical centre offset, fraction of canvas height"),
            "scale": num("Size multiplier on the fit-to-canvas size (1 = fit)"),
            "rotation": num("Clockwise rotation in degrees"),
            "opacity": num("0–1"),
            "blur": num("Blur radius in px at 1080p (0 = sharp; video/image clips)"),
            "glow": num("Glow 0–1: brighter, more saturated (video/image clips)"),
            "volume": num("0–4 (1 = original)"),
            "fade_in": num("Fade-in seconds (video, text and audio)"),
            "fade_out": num("Fade-out seconds"),
            "filter": filter,
            "brightness": num("-1–1 (0 = unchanged)"),
            "contrast": num("0–4 (1 = unchanged)"),
            "saturation": num("0–3 (1 = unchanged, 0 = greyscale)"),
            "content": string("Text content (text clips only)"),
        })), &["clip_id"], edit.clone()),
        tool("split_clip", "Split", "Splits a clip at a timeline time. Without `clip_id`, splits every clip under that time on unlocked tracks. Returns the ids of both halves.", json!({ "time": num("Timeline time in seconds"), "clip_id": int("Clip to split (optional)") }), &["time"], edit.clone()),
        tool("delete_clip", "Delete clip", "Deletes a clip. With `ripple`, later clips on the same track move left to close the gap.", json!({ "clip_id": int("Clip id"), "ripple": boolean("Close the gap (default false)") }), &["clip_id"], destructive.clone()),
        tool("duplicate_clip", "Duplicate clip", "Copies a clip and places the copy right after it.", json!({ "clip_id": int("Clip id") }), &["clip_id"], edit.clone()),
        tool("update_track", "Update track", "Renames, mutes, hides or locks a track.", json!({ "track_index": track, "name": string("Track name"), "muted": boolean("Mute audio"), "hidden": boolean("Hide video/text"), "locked": boolean("Prevent edits") }), &["track_index"], edit.clone()),
        tool("undo", "Undo", "Undoes the last edit.", json!({}), &[], edit.clone()),
        tool("redo", "Redo", "Redoes the last undone edit.", json!({}), &[], edit.clone()),
        tool("render_frame", "Render frame", "Renders the composited frame at a timeline time and returns it as a PNG image, exactly as it will look in the export. Use it to check layout, text and filters.", json!({ "time": num("Timeline time in seconds (default 0)"), "resolution": num("Short side in pixels, 120–1080 (default 360)") }), &[], ro.clone()),
        tool("export_video", "Export video", "Renders the timeline to an MP4 (H.264/AAC). Blocks until finished and reports progress if a progress token is given. Overwrites an existing file.", json!({
            "output": string("Absolute output path ending in .mp4"),
            "resolution": num("Short side in pixels: 480, 720, 1080 (default), 1440, 2160"),
            "fps": num("Frames per second (defaults to the project's)"),
            "quality": { "type": "string", "enum": ["draft", "standard", "high"], "description": "Encoder quality (default standard)" },
        }), &["output"], destructive.clone()),
        tool("download_caption_model", "Download caption model", "Downloads a whisper.cpp speech model used by auto_caption (one-time, from huggingface.co). Sizes: tiny 75 MB, base 142 MB (default), small 466 MB, medium 1463 MB.", json!({
            "model": { "type": "string", "enum": ["tiny", "base", "small", "medium"], "description": "Model size (default base)" },
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("auto_caption", "Auto-caption", "Transcribes speech locally with whisper.cpp and adds timed captions as text clips on a 'Captions' track at the top. Transcribes the whole timeline mix, or one clip with `clip_id`. Needs the model from download_caption_model.", json!({
            "clip_id": int("Transcribe only this clip (default: whole timeline)"),
            "model": { "type": "string", "enum": ["tiny", "base", "small", "medium"], "description": "Speech model (default base)" },
            "language": string("Spoken language as an ISO 639-1 code like en, es, fr, or auto (default)"),
            "translate": boolean("Translate the captions to English"),
            "style": { "type": "string", "enum": ["Clean", "Boxed", "Pop", "Minimal"], "description": "Caption look (default Clean)" },
            "max_words": num("Maximum words per caption, 1–20 (default 6; 2–3 for punchy short-form)"),
            "y": num("Vertical position as a fraction of canvas height from the centre (default 0.33 = lower area; negative = top)"),
            "replace": boolean("Replace earlier auto-captions (default true)"),
        }), &[], edit.clone()),
        tool("export_srt", "Export SRT", "Writes the captions (or all text clips if there are no auto-captions) as a .srt subtitle file.", json!({
            "output": string("Absolute output path ending in .srt"),
        }), &["output"], edit.clone()),
        tool("set_keyframes", "Set keyframes", "Animates a clip property. Keyframe times are TIMELINE seconds within the clip; values use the same units as update_clip (x/y fractions of the canvas, scale multiplier, rotation degrees, opacity 0–1, volume 0–4). `ease` shapes the motion from that keyframe to the next. Merges with existing keyframes unless replace=true. Check the result with render_frame.", json!({
            "clip_id": int("Clip id"),
            "property": { "type": "string", "enum": ["x", "y", "scale", "rotation", "opacity", "volume", "blur", "glow"], "description": "Property to animate" },
            "keyframes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "time": { "type": "number", "description": "Timeline time in seconds" },
                        "value": { "type": "number" },
                        "ease": { "type": "string", "enum": ["linear", "smooth", "ease_in", "ease_out", "hold"] },
                    },
                    "required": ["time", "value"],
                },
                "description": "Keyframes to add or update",
            },
            "replace": boolean("Remove the property's existing keyframes first (default false)"),
        }), &["clip_id", "property", "keyframes"], edit.clone()),
        tool("clear_keyframes", "Clear keyframes", "Removes keyframes from a clip (one property, or all). The property keeps the value it had at `keep_value_at` (default: the clip start).", json!({
            "clip_id": int("Clip id"),
            "property": { "type": "string", "enum": ["x", "y", "scale", "rotation", "opacity", "volume", "blur", "glow"], "description": "Property (default: all)" },
            "keep_value_at": num("Timeline time whose animated value becomes the static value"),
        }), &["clip_id"], edit.clone()),
        tool("auto_duck", "Auto-duck music", "Lowers music under speech: detects where Voice clips are speaking and writes volume keyframes onto Music clips (replacing their volume keyframes). Roles default to: imported audio files = music, a video's own sound = voice; change with set_clip_audio.", json!({
            "amount_db": num("How far to lower the music in dB (default 12)"),
            "sensitivity": num("0–1: higher catches quieter speech (default 0.88)"),
            "remove": boolean("Remove ducking instead (clears music volume keyframes)"),
        }), &[], edit.clone()),
        tool("set_clip_audio", "Clip audio settings", "Sets a clip's audio role for auto-ducking (auto, voice, music, effect) and/or noise removal (off, light, medium, strong). Noise removal measures the clip's background noise first, then removes steady noise such as hiss, hum and fans.", json!({
            "clip_id": int("Clip id"),
            "role": { "type": "string", "enum": ["auto", "voice", "music", "effect"] },
            "noise_reduction": { "type": "string", "enum": ["off", "light", "medium", "strong"] },
        }), &["clip_id"], edit.clone()),
        tool("auto_reframe", "Auto-reframe (AI)", "Keeps the subject in frame when the canvas ratio differs from the footage (e.g. 16:9 → 9:16). A vision model on OpenRouter locates the subject in sampled frames; RustCut writes a smooth camera path as position keyframes plus a fill scale. Costs roughly 1–2 cents per minute of video with the default model. Needs an OpenRouter API key.", json!({
            "clip_id": int("Clip to reframe (default: every video/image clip on video tracks)"),
            "aspect": { "type": "string", "enum": ["16:9", "9:16", "1:1", "4:3", "4:5"], "description": "Switch the canvas to this ratio first" },
            "subject": string("Who/what to keep in frame, e.g. 'the woman in the red jacket' (default: the main subject)"),
            "framing": { "type": "string", "enum": ["wide", "medium", "tight"], "description": "wide = just fill the canvas (default); medium/tight zoom in further" },
            "smoothness": num("0 = follow closely … 1 = very steady camera (default 0.6)"),
            "model": string("OpenRouter model id (default: the one in RustCut's AI settings)"),
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("list_video_models", "List video models", "Lists OpenRouter video generation models with supported durations, resolutions, aspect ratios, whether they can start/end on an image, and an example price.", json!({}), &[], json!({ "readOnlyHint": true, "openWorldHint": true })),
        tool("generate_video", "Generate video (AI)", "Generates a video clip from a text prompt (B-roll) with an OpenRouter video model and adds it to the project. Optionally starts from the frame at a timeline time. Takes 30 s to several minutes and is BILLED to the user's OpenRouter account (typically $0.10–$1 per clip; check list_video_models first). Duration, resolution and aspect snap to what the model supports.", json!({
            "prompt": string("What the shot shows, e.g. 'slow drone shot over a misty pine forest at golden hour'"),
            "model": string("Video model id (default: RustCut's AI setting)"),
            "duration": num("Seconds (snapped to the model's supported durations; default 5)"),
            "resolution": string("e.g. 720p (default: the model's 720p or lowest)"),
            "aspect": string("e.g. 16:9 or 9:16 (default: closest to the canvas)"),
            "audio": boolean("Generate sound (costs more; default false)"),
            "start_frame_time": num("Start the video on the composited frame at this timeline time (image-to-video)"),
            "place": { "type": "string", "enum": ["library", "at", "end"], "description": "library (default), at (timeline time `start`), or end of the main track" },
            "start": num("Timeline time for place=at"),
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("extend_clip", "Extend clip (AI)", "Generates new footage that continues from a clip's last frame and inserts it right after the clip (later clips move along). With blend_into_next, the next clip's first frame is used as the end frame to bridge the two shots. BILLED to the user's OpenRouter account.", json!({
            "clip_id": int("Video or image clip to extend"),
            "duration": num("Seconds to add (snapped to the model's durations; default 4)"),
            "prompt": string("Optional direction, e.g. 'the camera keeps pushing in'"),
            "blend_into_next": boolean("End on the next clip's first frame (needs a model with last_frame)"),
            "model": string("Video model id (default: RustCut's AI setting)"),
        }), &["clip_id"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("generate_thumbnail", "Generate thumbnail (AI)", "Makes a YouTube-style thumbnail from the composited frame at a timeline time plus a headline, using an OpenRouter image model with the frame as the reference image. Saves a PNG in 'RustCut AI' and sets it as the project's thumbnail (export_video copies it next to the video). BILLED: usually a few cents.", json!({
            "headline": string("Text on the thumbnail, spelled exactly (empty = no text)"),
            "time": num("Timeline time of the frame to use (default 0)"),
            "style": { "type": "string", "enum": ["bold", "clean", "dramatic"], "description": "Look (default bold)" },
            "prompt": string("Extra direction, e.g. 'add a red arrow pointing at the cake'"),
            "model": string("Image model id that accepts reference images (default: RustCut's AI setting)"),
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("generate_still", "Generate still image (AI)", "Generates a title card, lower-third backdrop or transparent sticker from a prompt and drops it on an overlay video track at `start` (an ordinary image clip: transform, keyframes, filters and transitions apply). Lower thirds and stickers get a transparent background. BILLED: usually a few cents.", json!({
            "kind": { "type": "string", "enum": ["title_card", "lower_third", "sticker"] },
            "prompt": string("What it shows, e.g. 'Day 1 in Lisbon, warm sunset colours'"),
            "start": num("Timeline time to place it (default 0)"),
            "place": boolean("Put it on the timeline (default true); false = media library only"),
            "model": string("Image model id (default: RustCut's AI setting; transparent kinds fall back to a model that supports transparency)"),
        }), &["kind", "prompt"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("background_fill", "Background fill (AI)", "For a clip that doesn't fill the canvas (e.g. a landscape clip in a 9:16 project), outpaints its middle frame into a canvas-shaped background plate drawn behind the clip instead of black bars. BILLED: usually a few cents. remove=true takes it away.", json!({
            "clip_id": int("Video or image clip"),
            "remove": boolean("Remove the existing background instead"),
            "force": boolean("Generate even if the clip already covers the canvas"),
            "model": string("Image model id that accepts reference images (default: RustCut's AI setting)"),
        }), &["clip_id"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("storyboard_plan", "Storyboard: plan (AI)", "Prompt-to-video step 1–2: a text model writes a script and shot list (JSON) for a brief like '30-second ad for a coffee shop, warm and cozy', then the Image API draws one storyboard frame per shot (first frame is the style reference for the rest). Nothing is animated yet. BILLED: planning < $0.01, ≈ $0.07 per frame. Pass `open` to reload a saved storyboard.json instead.", json!({
            "prompt": string("The brief"),
            "seconds": num("Total length (default 30)"),
            "aspect": { "type": "string", "enum": ["16:9", "9:16", "1:1", "4:3", "4:5"], "description": "Canvas (default 9:16)" },
            "draw_frames": boolean("Draw the storyboard frames now (default true)"),
            "model": string("Text model for the script (default: RustCut's editing model)"),
            "open": string("Path of a saved storyboard.json to continue instead of planning"),
            "character_image": string("Photo of the lead character; every frame keeps their likeness (only for people who agreed)"),
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("storyboard_update_shot", "Storyboard: edit shot", "Approves, edits, regenerates, swaps, moves or removes one storyboard shot (1-based). Regenerating redraws only that frame (≈ $0.07). image_path swaps in your own picture (auto-approved).", json!({
            "shot": int("Shot number, 1-based"),
            "approved": boolean("Approve (true) or unapprove (false)"),
            "description": string("New frame description"),
            "caption": string("New on-screen caption (empty = none)"),
            "motion": string("New motion direction for image-to-video"),
            "lead": boolean("Whether the lead character (character_image) appears in this shot; false for other people, then regenerate"),
            "seconds": num("New length (2–10 s)"),
            "regenerate": boolean("Redraw the frame (BILLED)"),
            "image_path": string("Use this image file as the frame instead"),
            "move_to": int("Move the shot to this position"),
            "remove": boolean("Delete the shot"),
            "model": string("Image model for regenerate"),
        }), &["shot"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("storyboard_produce", "Storyboard: animate & assemble (AI)", "Prompt-to-video step 3: animates the approved shots (or `shots`) with an image-to-video model in parallel, then appends them to the timeline in storyboard order with cross-dissolves, the shots' captions on a caption track, optional music (ducked under any voice) and a −14 LUFS loudness target. BILLED at video prices (check list_video_models; typically $0.10–$0.50 per shot).", json!({
            "shots": { "type": "array", "items": { "type": "integer" }, "description": "Shot numbers to animate (default: the approved ones)" },
            "model": string("Video model id with first_frame support (default: RustCut's AI setting)"),
            "generate_audio": boolean("Generate sound for each shot (costs more)"),
            "music_path": string("Music file to lay under the video"),
        }), &[], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("detect_beats", "Detect beats", "Analyses a song (on this computer, free): tempo (BPM), beats, bars (4/4) and sections (intro/verse/chorus/break/outro by loudness). The beat grid then drives snapping, beat_montage and beat_effect.", json!({
            "clip_id": int("The song clip (default: the first clip on an audio track)"),
            "media_id": int("Or a media id"),
        }), &[], edit.clone()),
        tool("beat_montage", "Beat montage", "Cuts video clips/photos on the beat over the song: every beat, 2 beats, bar, 2 bars, or auto (faster in loud sections). Uses successive parts of each source; clips are muted. Goes on the main track if free there, else a new video track.", json!({
            "media_ids": { "type": "array", "items": { "type": "integer" }, "description": "Videos/images to use (default: all)" },
            "every": { "type": "string", "enum": ["auto", "beat", "two_beats", "bar", "two_bars"] },
            "shuffle": boolean("Shuffle the order"),
            "from": num("Start (timeline s, default: song start)"),
            "to": num("End (timeline s, default: song end)"),
        }), &[], edit.clone()),
        tool("beat_effect", "Beat effect", "Writes beat-synced keyframes on clips: pulse (zoom punch), shake, flicker (opacity dip) or step_zoom (zooms further each beat). Replaces that property's keyframes.", json!({
            "clip_ids": { "type": "array", "items": { "type": "integer" } },
            "effect": { "type": "string", "enum": ["pulse", "shake", "flicker", "step_zoom"] },
            "every": { "type": "string", "enum": ["beat", "two_beats", "bar", "two_bars"] },
            "amount": num("Strength 0.1–1 (default 0.6)"),
        }), &["clip_ids", "effect"], edit.clone()),
        tool("multicam_sync", "Multi-cam: sync takes", "Lines up performance takes (videos with sound) against the song clip by their audio (cross-correlation, free/local) and puts each on its own muted 'Cam N' video track, trimmed to the song.", json!({
            "media_ids": { "type": "array", "items": { "type": "integer" }, "description": "Take media ids" },
            "song_clip_id": int("The master song clip (default: the beat clip or first audio-track clip)"),
        }), &["media_ids"], edit.clone()),
        tool("multicam_switch", "Multi-cam: switch angles", "Builds the 'Multi-cam edit' track by cutting between synced angles: automatically on the beat grid (every), or from explicit switch points [{time, angle}]. Angle tracks are hidden. Re-running replaces the edit in that range.", json!({
            "every": { "type": "string", "enum": ["auto", "beat", "two_beats", "bar", "two_bars", "section"], "description": "Auto-switch rate (default bar); section = at each song section change, max 8 bars" },
            "switches": { "type": "array", "items": { "type": "object", "properties": { "time": { "type": "number" }, "angle": { "type": "integer" } } }, "description": "Manual switch points" },
            "from": num("Start (default: song start)"),
            "to": num("End (default: song end)"),
        }), &[], edit.clone()),
        tool("add_lyrics", "Add lyrics", "Adds timed lyrics as caption clips on a 'Lyrics' track. Give `lyrics` (plain text is aligned to the vocals with speech recognition; .lrc text with [mm:ss.xx] stamps is used as-is), or `lrc_path`, or nothing to transcribe the vocals. Styles: karaoke (white line, each word lights up as sung), lines, big_centered, pop_words (one big word at a time, optionally on the beat), build_up. Free/local; needs a speech model unless the lyrics are timed.", json!({
            "lyrics": string("Lyrics text (plain or .lrc)"),
            "lrc_path": string("Path to an .lrc or .txt file"),
            "style": { "type": "string", "enum": ["karaoke", "lines", "big_centered", "pop_words", "build_up"], "description": "karaoke = white line, words light up as sung (default lines)" },
            "beat_snap": boolean("pop_words: land each word on the nearest beat when beats are detected (default true)"),
            "position": { "type": "string", "enum": ["top", "middle", "bottom"] },
            "language": string("Vocals language code, e.g. en (default auto)"),
            "song_clip_id": int("The song clip (default: the beat clip or first audio-track clip)"),
        }), &[], edit.clone()),
        tool("export_lrc", "Export .lrc", "Saves the timeline's lyrics as an .lrc file (times relative to the song).", json!({ "path": string("Absolute output path ending in .lrc") }), &["path"], ro.clone()),
        tool("add_visualizer", "Add audio visualizer", "Renders the song as a transparent visualizer overlay (bars = spectrum, circle = circular spectrum, wave, line, scope = vectorscope) with FFmpeg and places it over the song's span on an overlay track. Free/local.", json!({
            "kind": { "type": "string", "enum": ["bars", "circle", "wave", "line", "scope"] },
            "color": string("Hex colour, e.g. #00CDDE"),
            "position": { "type": "string", "enum": ["top", "middle", "bottom"] },
            "song_clip_id": int("The song clip (default: the beat clip or first audio-track clip)"),
        }), &[], edit.clone()),
        tool("audio_react", "React to the music", "Animates clips with the song's bass (kick) or loudness: throb (scale), glow (brightness), blur, shake (position), fade (opacity) or wobble (rotation). Written as simplified keyframes. Free/local.", json!({
            "clip_ids": { "type": "array", "items": { "type": "integer" } },
            "effect": { "type": "string", "enum": ["throb", "glow", "blur", "shake", "fade", "wobble"] },
            "follow": { "type": "string", "enum": ["bass", "loudness"] },
            "amount": num("Strength 0.1–1 (default 0.6)"),
            "song_clip_id": int("The song clip (default: the beat clip or first audio-track clip)"),
        }), &["clip_ids", "effect"], edit.clone()),
        tool("music_video_plan", "Music video: plan (AI)", "Plans an AI music video for the song on the timeline: shots are cut on the bars (2 bars in loud sections, longer in quiet ones), repeated sections reuse earlier shots, and a text model writes each unique shot from your theme, the song's sections and lyrics (Lyrics track). Then draws the storyboard frames. Continue with storyboard_update_shot and storyboard_produce. BILLED: planning < $0.01, ≈ $0.07 per frame.", json!({
            "theme": string("The video's idea, e.g. 'neon city at night, a lonely dancer in a red jacket'"),
            "cover": { "type": "string", "enum": ["whole", "best30"], "description": "Whole song (default) or its best 30 s" },
            "max_shots": num("Unique shots to generate (3–24, default 10)"),
            "aspect": { "type": "string", "enum": ["16:9", "9:16", "1:1", "4:3", "4:5"] },
            "draw_frames": boolean("Draw the frames now (default true)"),
            "model": string("Text model (default: RustCut's editing model)"),
            "song_clip_id": int("The song clip (default: the beat clip or first audio-track clip)"),
            "character_image": string("Photo of the lead character; every frame keeps their likeness (only for people who agreed)"),
            "cast": { "type": "array", "items": { "type": "object", "properties": { "name": { "type": "string" }, "photo": { "type": "string" } } }, "description": "Several recurring people [{name, photo}]: shots naming a person get their photo (only for people who agreed)" },
        }), &["theme"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("best_section", "Best section of the song", "Finds the most energetic window (default 30 s, starting on a downbeat, preferring a chorus) for a TikTok/Reels cut. With apply=true, ripple-trims the project to it and fades the song in/out.", json!({
            "seconds": num("Window length (default 30)"),
            "apply": boolean("Trim the project to it (default false)"),
        }), &[], edit.clone()),
        tool("cost_report", "Cost report", "Side-by-side production cost report for the open project: RustCut's actual AI spend (from the cost ledger) vs commercial tool stacks (Adobe, CapCut Pro, Runway + Descript, pay-as-you-go APIs), per video clip, per audio clip and for the project. Subscriptions are spread over videos_per_month. Saves the report in the economics database; format html/csv writes a file.", json!({
            "format": { "type": "string", "enum": ["summary", "html", "csv"] },
            "output": string("File path for html/csv"),
            "detail": boolean("Include every task line per clip (default false)"),
            "videos_per_month": num("How many videos the subscriptions are shared across (saved; default 4)"),
        }), &[], edit.clone()),
        tool("set_price", "Set commercial price", "Updates a commercial tool's monthly subscription and/or per-use rate in the economics database (tasks: ai_video_second, ai_image, transcription_minute, noise_removal_minute, mastering_track, beat_detection, visualizer_render, multicam_sync). Returns the stacks.", json!({
            "tool": string("Tool name, e.g. 'Runway Pro'"),
            "monthly_usd": num("New monthly subscription price"),
            "task": string("Task to price"),
            "usd_per_unit": num("Price per unit for the task"),
            "unit": string("Unit, e.g. second, image, minute"),
            "note": string("Where the price comes from"),
        }), &["tool"], edit.clone()),
        tool("list_voices", "List voices", "Lists the ElevenLabs voices in the user's account (cloned voices first), the character → voice map saved in RustCut, and credits used.", json!({}), &[], json!({ "readOnlyHint": true, "openWorldHint": true })),
        tool("speak_line", "Speak a line (ElevenLabs)", "Generates dialogue in a character's (or account voice's) voice with ElevenLabs, saves it as MP3 and places it on an audio track at `start` as a Voice clip, lowering the music under it. Uses the user's ElevenLabs credits (≈ 1 credit per character).", json!({
            "text": string("What the character says (v3 model supports tags like [whispers], [sighs])"),
            "voice": string("Character name (e.g. Aishyla) or ElevenLabs voice name/id"),
            "start": num("Timeline time to place it (default 0)"),
            "model": { "type": "string", "enum": ["eleven_multilingual_v2", "eleven_v3", "eleven_turbo_v2_5", "eleven_flash_v2_5"] },
            "stability": num("0–1, lower = more emotional (default 0.5)"),
            "likeness": num("0–1 similarity boost (default 0.8)"),
            "style": num("0–1 style exaggeration (default 0)"),
            "place": boolean("Put it on the timeline (default true)"),
            "duck": boolean("Lower music under it (default true)"),
        }), &["text", "voice"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("clone_voice", "Clone a voice (ElevenLabs)", "Creates an instant voice clone in the user's ElevenLabs account from clean speech clips (only for voices the user has the rights to) and saves it as a character.", json!({
            "name": string("Character / voice name, e.g. 'Aidan B.'"),
            "files": { "type": "array", "items": { "type": "string" }, "description": "Reference audio files (clean speech)" },
            "description": string("Voice description"),
        }), &["name", "files"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("lip_sync", "Lip-sync to dialogue (fal.ai)", "Makes the character in a video clip mouth the dialogue in the voice clip over it (fal.ai Sync lipsync-2). Only the part of the clip under the line (plus 0.15 s) is re-animated and replaced; the rest of the clip is untouched. Paid: about $0.05 per second (pro ≈ $0.083/s).", json!({
            "video_clip_id": int("The character's video clip"),
            "voice_clip_id": int("The dialogue clip (default: the voice clip overlapping the shot most)"),
            "model": { "type": "string", "enum": ["lipsync-2", "lipsync-2-pro"] },
        }), &["video_clip_id"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("performance_shot", "Performance shot (Seedance 2.0 lip-sync)", "Re-generates a shot of the singer as a performance of the song: a still of the shot plus the matching song audio and lyrics go to Seedance 2.0 on fal.ai in overlapping segments of ≤14 s, and the results replace the shot on the timeline. Best lip-sync method for music videos. Paid: ≈ $0.24/s (fast) or $0.30/s (best) of generated video; use dry_run first to see segments and cost.", json!({
            "clip_id": int("The singer's video or image clip"),
            "audio_clip_id": int("The song/vocal clip (default: the audio overlapping the shot most)"),
            "model": { "type": "string", "enum": ["fast", "best"] },
            "direction": string("How they perform, e.g. 'sings with eyes closed, swaying slowly'"),
            "character_sheet": string("Optional character sheet image path for a consistent face"),
            "dry_run": boolean("Only plan: return segments, lyrics and estimated cost"),
        }), &["clip_id"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("reshoot_clip", "Reshoot one clip (AI)", "Re-generates just one cut on the timeline from a prompt: draws a new still (keeping the storyboard's style and cast, and the current shot's look unless keep_look=false), animates it and swaps it in at the same place and length. Other cuts of the same storyboard shot are untouched. Description/motion default to the storyboard shot's. Use still_only=true first to review the still (≈ $0.07), then call again with still=<path> to animate (≈ $0.14+ depending on model).", json!({
            "clip_id": int("The clip to reshoot"),
            "description": string("What the new shot shows (default: the storyboard shot's description)"),
            "motion": string("Camera/subject motion (default: the storyboard shot's motion)"),
            "keep_look": boolean("Use the current shot's frame as a look reference (default true)"),
            "lead": boolean("The lead/cast appear in the shot (default: as in the storyboard)"),
            "still_only": boolean("Only draw the still and return its path"),
            "still": string("Animate this already-drawn still instead of drawing a new one"),
            "video_model": string("Image-to-video model id (default: the one in AI settings)"),
        }), &["clip_id"], json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true })),
        tool("rough_cut", "Rough cut by prompt (AI)", "Makes a rough cut from instructions like '60-second highlight, keep the funny bits, cut ums and long pauses'. A video-understanding model reads the whisper transcript (≈ word timing) and watches a tiny copy of the timeline, then RustCut keeps the chosen ranges and ripple-cuts everything else across all tracks (one undo). Also returns title ideas, a YouTube description and chapters. Billed to the user's OpenRouter account (typically a few cents per minute).", json!({
            "prompt": string("What the edit should be"),
            "target_seconds": num("Desired final length in seconds (optional)"),
            "use_transcript": boolean("Use the speech transcript (default: true if a speech model is installed)"),
            "use_video": boolean("Send a low-res copy of the timeline for visual moments (default true)"),
            "apply": boolean("Apply the cut (default true); false returns the plan only"),
            "engine": { "type": "string", "enum": ["openrouter", "local"], "description": "openrouter (default, paid, reads + watches) or local (free, offline: a local Ollama model keeps/cuts/trims each transcript sentence; needs a speech model)" },
            "model": string("Model id: OpenRouter model with video input, or for engine=local an Ollama model (default: RustCut's settings)"),
        }), &["prompt"], json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true })),
        tool("auto_transitions", "Smart transitions (local AI)", "For every cut between touching clips on visible video tracks, a local Ollama decision model (free, offline) picks hard cut, quick/slow dissolve, fade through black, slide or zoom from the song section and energy, beat timing and the clips, and applies it (one undo).", json!({
            "style": string("Optional style, e.g. 'moody, mostly hard cuts'"),
            "track_index": int("Only this track (default: all visible video tracks)"),
            "apply": boolean("Apply (default true); false only returns the choices"),
            "model": string("Ollama model (default: RustCut's local model setting)"),
        }), &[], edit.clone()),
        tool("measure_loudness", "Measure loudness", "Measures the integrated loudness (LUFS), loudness range and true peak of the timeline mix before normalisation.", json!({}), &[], ro.clone()),
        tool("set_transition", "Set transition", "Adds, changes or removes the transition at a cut between two touching clips on a video track. It is centred on the cut and uses footage just past each clip's edges (holding a frame if there is none); the timeline length does not change. Clips must touch (no gap).", json!({
            "clip_id": int("A clip on either side of the cut"),
            "at": { "type": "string", "enum": ["start", "end"], "description": "The cut at the clip's start (default) or end" },
            "kind": { "type": "string", "enum": ["none", "dissolve", "dip_black", "dip_white", "slide_left", "slide_right", "slide_up", "slide_down", "wipe_left", "wipe_right", "wipe_up", "wipe_down", "zoom_in"], "description": "Transition style, or none to remove" },
            "duration": num("Seconds, 0.1–3 (default 0.5); shortened automatically to fit short clips"),
        }), &["clip_id", "kind"], edit.clone()),
        tool("apply_motion_preset", "Apply motion preset", "Adds ready-made keyframe motion to a video, image or text clip: ken_burns (slow zoom and drift), punch_in (quick zoom), slide_in_left, pop_in, spin_in (not for text).", json!({
            "clip_id": int("Clip id"),
            "preset": { "type": "string", "enum": ["ken_burns", "punch_in", "slide_in_left", "pop_in", "spin_in"] },
        }), &["clip_id", "preset"], edit.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(s: &mut Server<Vec<u8>>, id: i64, method: &str, params: Value) -> Value {
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        s.handle_line(&line).expect("response")
    }

    fn tool_call(s: &mut Server<Vec<u8>>, name: &str, args: Value) -> (bool, String) {
        let r = call(s, 1, "tools/call", json!({ "name": name, "arguments": args }));
        let res = &r["result"];
        (res["isError"].as_bool().unwrap(), res["content"][0]["text"].as_str().unwrap_or("").to_string())
    }

    #[test]
    fn handshake_and_listing() {
        let mut s = Server::new(Vec::new());
        let r = call(&mut s, 1, "initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }));
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "rustcut");
        let r = call(&mut s, 2, "initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(r["result"]["protocolVersion"], LATEST_PROTOCOL);
        assert!(s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let r = call(&mut s, 3, "tools/list", json!({}));
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 59);
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
        assert_eq!(call(&mut s, 4, "ping", json!({}))["result"], json!({}));
    }

    #[test]
    fn protocol_errors() {
        let mut s = Server::new(Vec::new());
        assert_eq!(s.handle_line("{not json").unwrap()["error"]["code"], -32700);
        assert_eq!(call(&mut s, 1, "bogus/method", json!({}))["error"]["code"], -32601);
        assert_eq!(call(&mut s, 2, "tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
        assert_eq!(s.handle_line("[1,2]").unwrap()["error"]["code"], -32600);
        let bom = "\u{feff}{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}";
        assert_eq!(s.handle_line(bom).unwrap()["result"], json!({}));
    }

    #[test]
    fn text_editing_flow() {
        let mut s = Server::new(Vec::new());
        let (err, out) = tool_call(&mut s, "add_text", json!({ "content": "Hi", "preset": "title", "start": 1.0, "y": 0.3, "color": "#FF0000" }));
        assert!(!err, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        let id = v["clip"]["id"].as_u64().unwrap();
        assert_eq!(v["clip"]["source"]["Text"]["color"], json!([255, 0, 0, 255]));
        assert_eq!(v["clip"]["source"]["Text"]["font_size"], json!(120.0));

        let (err, _) = tool_call(&mut s, "update_clip", json!({ "clip_id": id, "content": "Bye", "background": "#00000080", "duration": 5 }));
        assert!(!err);
        let style = s.project.clip(id).unwrap().text().unwrap().clone();
        assert_eq!(style.content, "Bye");
        assert_eq!(style.background, Some([0, 0, 0, 128]));
        assert_eq!(s.project.clip(id).unwrap().duration, 5.0);

        // Tool-level errors are reported in the result, not as JSON-RPC errors.
        let (err, msg) = tool_call(&mut s, "update_clip", json!({ "clip_id": id, "speed": 2 }));
        assert!(err && msg.contains("speed"));
        let (err, msg) = tool_call(&mut s, "update_clip", json!({ "clip_id": 999 }));
        assert!(err && msg.contains("999"));

        let (err, out) = tool_call(&mut s, "split_clip", json!({ "time": 3.0 }));
        assert!(!err, "{out}");
        assert_eq!(s.project.clip_count(), 2);
        let (err, _) = tool_call(&mut s, "undo", json!({}));
        assert!(!err);
        assert_eq!(s.project.clip_count(), 1);
        tool_call(&mut s, "redo", json!({}));
        assert_eq!(s.project.clip_count(), 2);

        let (_, out) = tool_call(&mut s, "get_project", json!({}));
        let p: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(p["tracks"][0]["kind"], "Text");
        assert_eq!(p["tracks"][0]["clips"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn keyframe_tools() {
        let mut s = Server::new(Vec::new());
        let (_, out) = tool_call(&mut s, "add_text", json!({ "content": "Hi", "start": 2.0, "duration": 4.0 }));
        let id = serde_json::from_str::<Value>(&out).unwrap()["clip"]["id"].as_u64().unwrap();
        let (err, out) = tool_call(&mut s, "set_keyframes", json!({
            "clip_id": id, "property": "y",
            "keyframes": [{ "time": 2.0, "value": 0.5, "ease": "ease_out" }, { "time": 3.0, "value": 0.3 }],
        }));
        assert!(!err, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["clip"]["keyframes"]["y"][1]["time"], json!(3.0)); // timeline time
        assert_eq!(s.project.clip(id).unwrap().keyframes[&Prop::Y][0].time, 0.0); // stored clip-local
        assert_eq!(s.project.clip(id).unwrap().keyframes[&Prop::Y][0].ease, Ease::EaseOut);

        let (err, msg) = tool_call(&mut s, "set_keyframes", json!({ "clip_id": id, "property": "rotation", "keyframes": [{ "time": 2, "value": 9 }] }));
        assert!(err && msg.contains("rotation"));
        let (err, msg) = tool_call(&mut s, "set_keyframes", json!({ "clip_id": id, "property": "x", "keyframes": [{ "time": 9, "value": 0 }] }));
        assert!(err && msg.contains("outside"));

        let (_, out) = tool_call(&mut s, "update_clip", json!({ "clip_id": id, "y": 0.1 }));
        assert!(out.contains("warning"));

        let (err, _) = tool_call(&mut s, "apply_motion_preset", json!({ "clip_id": id, "preset": "pop_in" }));
        assert!(!err);
        assert!(s.project.clip(id).unwrap().is_animated(Prop::Opacity));
        let (err, _) = tool_call(&mut s, "apply_motion_preset", json!({ "clip_id": id, "preset": "spin_in" }));
        assert!(err);

        tool_call(&mut s, "clear_keyframes", json!({ "clip_id": id, "property": "y", "keep_value_at": 3.0 }));
        let c = s.project.clip(id).unwrap();
        assert!(!c.is_animated(Prop::Y) && (c.transform.y - 0.3).abs() < 1e-6);
        tool_call(&mut s, "clear_keyframes", json!({ "clip_id": id }));
        assert!(!s.project.clip(id).unwrap().has_keyframes());
    }

    #[test]
    fn transition_tool() {
        let mut s = Server::new(Vec::new());
        let p = &mut s.project;
        let m = p.add_media(MediaItem {
            id: 0,
            path: "v.mp4".into(),
            name: "v.mp4".into(),
            kind: MediaKind::Video,
            duration: 10.0,
            width: 1280,
            height: 720,
            fps: 30.0,
            has_audio: false,
        });
        let a = p.new_media_clip(m, 0.0).unwrap();
        let b = p.new_media_clip(m, 0.0).unwrap();
        let (aid, bid) = (a.id, b.id);
        p.append_to_main(a);
        p.append_to_main(b);
        let (err, msg) = tool_call(&mut s, "set_transition", json!({ "clip_id": aid, "kind": "dissolve" }));
        assert!(err && msg.contains("directly before"), "{msg}");
        let (err, out) = tool_call(&mut s, "set_transition", json!({ "clip_id": aid, "at": "end", "kind": "Wipe left", "duration": 1.0 }));
        assert!(!err, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["incoming_clip"]["id"], json!(bid));
        assert_eq!(v["incoming_clip"]["transition_in"]["kind"], "wipe_left");
        assert_eq!(v["incoming_clip"]["transition_in_effective"]["start"], json!(9.5));
        let (err, _) = tool_call(&mut s, "set_transition", json!({ "clip_id": bid, "kind": "none" }));
        assert!(!err);
        assert!(s.project.clip(bid).unwrap().transition_in.is_none());
    }

    #[test]
    fn smart_audio_tools() {
        let mut s = Server::new(Vec::new());
        let (err, out) = tool_call(&mut s, "update_project", json!({ "loudness_target": -16 }));
        assert!(!err && out.contains("-16"), "{out}");
        assert_eq!(s.project.audio.loudness, Some(-16.0));
        let (err, _) = tool_call(&mut s, "update_project", json!({ "loudness_target": "off" }));
        assert!(!err);
        assert_eq!(s.project.audio.loudness, None);
        let (err, msg) = tool_call(&mut s, "update_project", json!({ "loudness_target": 3 }));
        assert!(err && msg.contains("between"));
        let (err, msg) = tool_call(&mut s, "auto_duck", json!({}));
        assert!(err && msg.contains("No voice clips"), "{msg}");
    }

    #[test]
    fn ripple_delete() {
        let mut s = Server::new(Vec::new());
        for start in [0.0, 3.0, 6.0] {
            tool_call(&mut s, "add_text", json!({ "content": "x", "start": start, "track_index": null }));
        }
        let ids: Vec<Id> = s.project.tracks[0].clips.iter().map(|c| c.id).collect();
        let (err, _) = tool_call(&mut s, "delete_clip", json!({ "clip_id": ids[1], "ripple": true }));
        assert!(!err);
        assert_eq!(s.project.clip(ids[2]).unwrap().start, 3.0);
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_color("#11223344").unwrap(), [0x11, 0x22, 0x33, 0x44]);
        assert!(parse_color("#12345").is_err());
        assert_eq!(parse_filter("B&W").unwrap(), FilterPreset::BlackWhite);
        assert_eq!(parse_aspect("9:16").unwrap(), Aspect::Portrait);
    }
}
