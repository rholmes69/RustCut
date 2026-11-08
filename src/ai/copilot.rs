//! Copilot: control the editor by typing or speaking. A language model (local Ollama
//! or a cloud model through OpenRouter) plans the edit and calls tools; the editor
//! runs every tool on the UI thread (the same tools the MCP server offers, plus app
//! controls like play, seek and opening windows), so each step shows up live.
//!
//! Simple commands ("play", "undo", "go to 1:20", "split here") skip the model and
//! run instantly.

use crate::ai::openrouter::{self, Client};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Most model ↔ tool round trips per request.
const MAX_STEPS: usize = 16;

#[derive(Clone)]
pub enum Backend {
    Ollama { url: String, model: String },
    OpenRouter { key: String, model: String },
}

impl Backend {
    pub fn label(&self) -> String {
        match self {
            Backend::Ollama { model, .. } => format!("local · {model}"),
            Backend::OpenRouter { model, .. } => format!("cloud · {model}"),
        }
    }

    pub fn local(&self) -> bool {
        matches!(self, Backend::Ollama { .. })
    }
}

pub enum Event {
    /// Run a tool and send its result (or an error message) back.
    Tool { name: String, args: Value, reply: Sender<Result<String, String>> },
    /// Something the model said. `last` = its final answer for this request.
    Say { text: String, last: bool },
    /// Finished: the conversation so far (without the system prompt) and the cost.
    Done { history: Vec<Value>, usd: f64 },
    Failed(String),
}

/// App controls the Copilot can use besides the editing tools.
pub fn app_tools() -> Vec<Value> {
    let f = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({ "type": "function", "function": { "name": name, "description": desc,
            "parameters": { "type": "object", "properties": props, "required": req } } })
    };
    vec![
        f("play", "Start playback from the playhead.", json!({}), &[]),
        f("pause", "Pause playback.", json!({}), &[]),
        f("seek", "Move the playhead.", json!({ "time": { "type": "number", "description": "seconds on the timeline" } }), &["time"]),
        f("select_clip", "Select a clip (the Inspector shows it) and move the playhead to its start.", json!({ "clip_id": { "type": "integer" } }), &["clip_id"]),
        f("preview_clip", "Open a preview window that plays just this clip.", json!({ "clip_id": { "type": "integer" } }), &["clip_id"]),
        f("split_at_playhead", "Split the clip under the playhead (the selected one if it's there).", json!({}), &[]),
        f("ripple_delete", "Delete a clip and close the gap it leaves (later clips on its track move left).", json!({ "clip_id": { "type": "integer" } }), &["clip_id"]),
        f(
            "freeze_frame",
            "Hold one frame of a video clip as a still for some seconds; the rest of the track moves later.",
            json!({ "clip_id": { "type": "integer", "description": "default: the clip under the playhead" },
                    "time": { "type": "number", "description": "timeline seconds of the frame; default: the playhead" },
                    "seconds": { "type": "number", "description": "how long to hold it, default 2" } }),
            &[],
        ),
        f("add_marker", "Add a named marker (chapter / cue point) on the timeline.", json!({ "time": { "type": "number", "description": "default: the playhead" }, "name": { "type": "string" } }), &["name"]),
        f("go_to_marker", "Move the playhead to the marker whose name best matches.", json!({ "name": { "type": "string" } }), &["name"]),
        f("delete_marker", "Delete the marker whose name best matches (or all markers with name \"all\").", json!({ "name": { "type": "string" } }), &["name"]),
        f("markers_from_sections", "Replace the markers with one per song section (Verse 1, Chorus, Bridge…).", json!({}), &[]),
        f("copy_chapters", "Copy the markers as a YouTube chapter list to the clipboard and return it.", json!({}), &[]),
        f("undo", "Undo the last edit.", json!({}), &[]),
        f("redo", "Redo.", json!({}), &[]),
        f("save_project", "Save the project to its file.", json!({}), &[]),
        f(
            "open_window",
            "Open a window of the editor for the user. Use it for things you can't do yourself: exporting, \
             paid AI generation (video, images, storyboards, voices, reshoots), rough cuts.",
            json!({ "window": { "type": "string", "enum": WINDOWS.iter().map(|w| w.0).collect::<Vec<_>>() } }),
            &["window"],
        ),
    ]
}

/// `open_window` targets: name and what it is.
pub const WINDOWS: &[(&str, &str)] = &[
    ("export", "Export video"),
    ("timeline_storyboard", "Timeline storyboard (every clip as a tile)"),
    ("storyboard", "Make a video from a prompt (AI storyboard)"),
    ("rough_cut", "Rough cut by prompt"),
    ("voices", "Dialogue & voices (ElevenLabs)"),
    ("transitions", "Smart transitions"),
    ("generate_video", "Generate video (B-roll)"),
    ("generate_image", "Generate image"),
    ("lyrics", "Lyrics"),
    ("beat_montage", "Beat montage"),
    ("costs", "Cost report"),
    ("ai_settings", "OpenRouter settings"),
];

pub fn system_prompt(state: &str) -> String {
    format!(
        "You are the Copilot inside RustCut, a CapCut-style video editor. The user types or speaks a request; \
carry it out by calling tools, then answer in ONE short sentence saying what you did (it may be read aloud, so no \
markdown, ids or lists unless asked). If the request is a question, answer it briefly.\n\
Rules:\n\
- Act, don't ask, when the intent is clear. Ask a short question only when it is genuinely ambiguous.\n\
- 'this clip', 'it', 'the selected clip' mean the selected clip; 'here' / 'now' mean the playhead.\n\
- Call get_project when you need ids, names or times you don't have. Don't invent ids.\n\
- Times are seconds on the timeline. Clip x/y are offsets of the layer centre from the canvas centre as a \
fraction of the canvas (y=0.35 = lower third). scale multiplies the fit-to-canvas size. The bottom video track is the main track.\n\
- You can't generate paid AI media (video, images, voices, lip-sync, reshoots) or export; open the matching window \
with open_window and tell the user what to do there.\n\
- Everything you do in one request is a single undo step.\n\n\
Editor state right now:\n{state}"
    )
}

/// Instant commands that need no model: returns the app tools to run.
pub fn quick_command(text: &str) -> Option<Vec<(String, Value)>> {
    let t: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ':' || c == '.' || c == ' ' { c } else { ' ' }).collect();
    // Whisper ends sentences with a period: "Play it." (keep decimals like "1.5").
    let mut words: Vec<&str> = t.split_whitespace().map(|w| w.trim_end_matches('.')).filter(|w| !w.is_empty()).collect();
    // "please", "rustcut", "hey copilot" and trailing "it/that/now" don't change the meaning.
    words.retain(|w| !matches!(*w, "please" | "rustcut" | "copilot" | "hey" | "ok" | "okay" | "now" | "no" | "oops"));
    while words.last().is_some_and(|w| matches!(*w, "it" | "that" | "this" | "the" | "video" | "project" | "here")) && words.len() > 1 {
        words.pop();
    }
    let s = words.join(" ");
    let one = |name: &str| Some(vec![(name.to_string(), json!({}))]);
    match s.as_str() {
        "play" | "start" | "play video" | "resume" | "start playback" => return one("play"),
        "pause" | "stop" | "stop playback" | "hold on" => return one("pause"),
        "undo" | "go back" => return one("undo"),
        "redo" => return one("redo"),
        "save" | "save project" => return one("save_project"),
        "split" | "split here" | "cut here" | "split clip" | "split the clip" => return one("split_at_playhead"),
        "export" | "export video" | "render" => return Some(vec![("open_window".into(), json!({ "window": "export" }))]),
        "add marker" | "add a marker" | "marker" | "mark" | "drop a marker" => return Some(vec![("add_marker".into(), json!({ "name": "" }))]),
        "next marker" | "next chapter" => return Some(vec![("go_to_marker".into(), json!({ "name": "", "dir": 1 }))]),
        "previous marker" | "last marker" | "previous chapter" => return Some(vec![("go_to_marker".into(), json!({ "name": "", "dir": -1 }))]),
        "freeze frame" | "freeze" | "freeze this frame" => return one("freeze_frame"),
        "ripple delete" => return one("ripple_delete"),
        "show storyboard" | "timeline storyboard" | "storyboard" => {
            return Some(vec![("open_window".into(), json!({ "window": "timeline_storyboard" }))]);
        }
        "go to start" | "go to the start" | "go to beginning" | "go to the beginning" | "rewind" | "start over" => {
            return Some(vec![("seek".into(), json!({ "time": 0.0 }))]);
        }
        _ => {}
    }
    // "go to 1:20", "jump to 95 seconds", "go to 2 minutes", "seek 30"
    let rest = ["go to ", "jump to ", "skip to ", "seek to ", "seek ", "move to "].iter().find_map(|p| s.strip_prefix(p))?;
    let rest = rest.trim_start_matches("the ").trim();
    if rest == "end" {
        return Some(vec![("seek".into(), json!({ "time": f64::MAX }))]);
    }
    parse_time(rest).map(|t| vec![("seek".into(), json!({ "time": t }))])
}

/// "1:20", "1:02:03", "95", "95 seconds", "2 minutes", "1 minute 30 seconds".
fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.contains(':') {
        let mut t = 0.0;
        for part in s.split_whitespace().next()?.split(':') {
            t = t * 60.0 + part.parse::<f64>().ok()?;
        }
        return Some(t);
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    let (mut t, mut i, mut any) = (0.0, 0, false);
    while i < words.len() {
        let n: f64 = words[i].parse().ok()?;
        let unit = words.get(i + 1).copied().unwrap_or("seconds");
        t += n * match unit {
            u if u.starts_with("min") => 60.0,
            u if u.starts_with("sec") || u == "s" => 1.0,
            u if u.starts_with("hour") => 3600.0,
            _ => return None,
        };
        any = true;
        i += 2;
        if words.get(i) == Some(&"and") {
            i += 1;
        }
    }
    any.then_some(t)
}

/// Starts one request on a worker thread. `history` is the conversation so far
/// (without the system prompt); `user` the new request.
pub fn start(backend: Backend, system: String, mut history: Vec<Value>, user: String, tools: Vec<Value>, ctx: egui::Context) -> (Receiver<Event>, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        history.push(json!({ "role": "user", "content": user }));
        let mut usd = 0.0;
        let r = run(&backend, &system, &mut history, &tools, &tx, &c, &mut usd, &ctx);
        let _ = tx.send(match r {
            Ok(()) => Event::Done { history, usd },
            Err(_) if c.load(Ordering::Relaxed) => Event::Done { history, usd },
            Err(e) => Event::Failed(format!("{e:#}")),
        });
        ctx.request_repaint();
    });
    (rx, cancel)
}

#[allow(clippy::too_many_arguments)]
fn run(backend: &Backend, system: &str, history: &mut Vec<Value>, tools: &[Value], tx: &Sender<Event>, cancel: &AtomicBool, usd: &mut f64, ctx: &egui::Context) -> Result<()> {
    // Long tool results (a big project summary) are cut so small local models keep up.
    let max_result = if backend.local() { 8_000 } else { 24_000 };
    for _ in 0..MAX_STEPS {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let mut messages = vec![json!({ "role": "system", "content": system })];
        messages.extend(history.iter().cloned());
        let (msg, calls) = ask(backend, messages, tools, usd)?;
        let text = clean(msg["content"].as_str().unwrap_or(""));
        history.push(msg);
        if calls.is_empty() {
            let _ = tx.send(Event::Say { text, last: true });
            ctx.request_repaint();
            return Ok(());
        }
        if !text.is_empty() {
            let _ = tx.send(Event::Say { text, last: false });
        }
        for (id, name, args) in calls {
            let (rtx, rrx) = mpsc::channel();
            tx.send(Event::Tool { name: name.clone(), args, reply: rtx }).map_err(|_| anyhow!("the Copilot window closed"))?;
            ctx.request_repaint();
            let result = loop {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                match rrx.recv_timeout(Duration::from_millis(100)) {
                    Ok(r) => break r,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => bail!("the editor stopped answering"),
                }
            };
            let mut content = match result {
                Ok(s) if s.trim().is_empty() => "done".to_string(),
                Ok(s) => s,
                Err(e) => format!("ERROR: {e}"),
            };
            if content.len() > max_result {
                let mut cut = max_result;
                while !content.is_char_boundary(cut) {
                    cut -= 1;
                }
                content.truncate(cut);
                content.push_str("\n…(truncated)");
            }
            history.push(match backend {
                Backend::OpenRouter { .. } => json!({ "role": "tool", "tool_call_id": id, "content": content }),
                Backend::Ollama { .. } => json!({ "role": "tool", "tool_name": name, "content": content }),
            });
        }
    }
    let _ = tx.send(Event::Say { text: "I stopped after too many steps — say what's left and I'll continue.".into(), last: true });
    Ok(())
}

type Call = (String, String, Value);

/// One model turn: the assistant message (as it goes back into the history) and its tool calls.
fn ask(backend: &Backend, messages: Vec<Value>, tools: &[Value], usd: &mut f64) -> Result<(Value, Vec<Call>)> {
    match backend {
        Backend::OpenRouter { key, model } => {
            let body = json!({ "model": model, "messages": messages, "tools": tools, "temperature": 0.2 });
            let (mut msg, usage) = Client::new(key.clone()).chat_message(&body)?;
            *usd += usage.cost;
            let calls = msg["tool_calls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            let args = match &c["function"]["arguments"] {
                                Value::String(s) if s.trim().is_empty() => json!({}),
                                Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
                                v => v.clone(),
                            };
                            (c["id"].as_str().unwrap_or("").to_string(), c["function"]["name"].as_str().unwrap_or("").to_string(), args)
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Keep only what the API accepts back in the history.
            let content = msg["content"].clone();
            let tool_calls = msg["tool_calls"].take();
            let mut m = json!({ "role": "assistant", "content": if content.is_null() { json!("") } else { content } });
            if tool_calls.is_array() {
                m["tool_calls"] = tool_calls;
            }
            Ok((m, calls))
        }
        Backend::Ollama { url, model } => {
            let body = json!({
                "model": model, "messages": messages, "tools": tools, "stream": false, "think": false,
                "options": { "num_ctx": 12288, "temperature": 0.2 },
            });
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(300)).build();
            let v: Value = match agent.post(&format!("{}/api/chat", url.trim_end_matches('/'))).send_json(body) {
                Ok(r) => r.into_json().context("Ollama returned unreadable JSON")?,
                Err(ureq::Error::Status(code, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    if body.contains("does not support tools") {
                        bail!("the local model `{model}` can't use tools — pick qwen3 (or another tool-capable model) in the Copilot's settings");
                    }
                    bail!("Ollama: HTTP {code}: {}", body.chars().take(300).collect::<String>())
                }
                Err(e) => bail!("Ollama isn't reachable at {url} ({e}). Start Ollama, or switch the Copilot to a cloud model."),
            };
            let msg = v["message"].clone();
            let calls = msg["tool_calls"]
                .as_array()
                .map(|a| a.iter().map(|c| (String::new(), c["function"]["name"].as_str().unwrap_or("").to_string(), c["function"]["arguments"].clone())).collect())
                .unwrap_or_default();
            Ok((msg, calls))
        }
    }
}

/// Drops `<think>` blocks some local models emit and tidies whitespace.
fn clean(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    out.trim().to_string()
}

/// True while the Copilot is talking (hands-free listening ignores the mic then,
/// so it doesn't hear itself).
static SPEAKING: AtomicBool = AtomicBool::new(false);

pub fn is_speaking() -> bool {
    SPEAKING.load(Ordering::Relaxed)
}

/// What a hands-free utterance means.
#[derive(Debug, PartialEq)]
pub enum Heard {
    /// A request (wake word removed).
    Command(String),
    /// Just the wake word: listen for the request.
    Wake,
    /// "that's all", "thanks": stop taking follow-ups without the wake word.
    Done,
    /// "stop listening", "go to sleep": turn hands-free off.
    Sleep,
    /// Not for us.
    Ignore,
}

/// Wake words as Whisper tends to write them (one word or two).
const WAKE: &[&[&str]] = &[
    &["copilot"], &["co", "pilot"], &["copilots"], &["rustcut"], &["rust", "cut"], &["russ", "cut"],
    &["rest", "cut"], &["rush", "cut"], &["rustcat"], &["rust", "cat"], &["rascut"],
];

/// Interprets a hands-free utterance. `following` = right after a reply or the wake
/// word, when no wake word is needed.
pub fn hands_free(text: &str, following: bool) -> Heard {
    let norm = |w: &str| w.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let words: Vec<&str> = text.split_whitespace().collect();
    let low: Vec<String> = words.iter().map(|w| norm(w)).collect();
    let plain = low.iter().filter(|w| !w.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    if ["stop listening", "go to sleep", "goodbye copilot", "goodbye rustcut", "hands free off"].iter().any(|p| plain.contains(p)) {
        return Heard::Sleep;
    }
    // Skip a leading "hey" / "ok", then look for the wake word in the first few words.
    let mut found = None;
    'outer: for start in 0..low.len().min(3) {
        for w in WAKE {
            if low.len() >= start + w.len() && w.iter().zip(&low[start..]).all(|(a, b)| a == b) {
                let lead_ok = low[..start].iter().all(|x| matches!(x.as_str(), "hey" | "ok" | "okay" | "hi" | "yo" | ""));
                if lead_ok {
                    found = Some(start + w.len());
                    break 'outer;
                }
            }
        }
    }
    let rest = match found {
        Some(i) => words[i..].join(" "),
        None if following => text.trim().to_string(),
        None => return Heard::Ignore,
    };
    let rest = rest.trim_start_matches([',', '.', '!', '?', ' ']).trim().to_string();
    let r = norm(&rest.replace(' ', ""));
    if r.is_empty() {
        return if found.is_some() { Heard::Wake } else { Heard::Ignore };
    }
    if ["thatsall", "thanks", "thankyou", "nevermind", "thatsit", "nothing", "allgood", "imdone"].contains(&r.as_str()) {
        return Heard::Done;
    }
    Heard::Command(rest)
}

/// Says `text` with the system voice (Windows speech synthesis), without blocking.
pub fn speak(text: &str) {
    let text: String = text.chars().filter(|c| !matches!(c, '*' | '#' | '`' | '_')).take(400).collect();
    if text.trim().is_empty() || !cfg!(windows) {
        return;
    }
    SPEAKING.store(true, Ordering::Relaxed);
    std::thread::spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                SPEAKING.store(false, Ordering::Relaxed);
            }
        }
        let _done = Done;
        // The text goes in an environment variable, never into the script itself.
        let _ = crate::media::ffmpeg::command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak($env:RUSTCUT_SAY)"])
            .env("RUSTCUT_SAY", text)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// The backend from the user's settings.
pub fn backend(settings: &openrouter::AiSettings) -> Result<Backend> {
    if settings.copilot_cloud {
        let key = settings.api_key().ok_or_else(|| anyhow!(openrouter::NO_KEY))?;
        Ok(Backend::OpenRouter { key, model: settings.copilot_model.clone() })
    } else {
        Ok(Backend::Ollama { url: settings.ollama_url.clone(), model: settings.local_model.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(t: &str) -> Option<Vec<(String, f64)>> {
        quick_command(t).map(|v| v.into_iter().map(|(n, a)| (n, a["time"].as_f64().unwrap_or(-1.0))).collect())
    }

    #[test]
    fn quick_commands_skip_the_model() {
        assert_eq!(names("Play it."), Some(vec![("play".into(), -1.0)]));
        assert_eq!(names("please undo that"), Some(vec![("undo".into(), -1.0)]));
        assert_eq!(names("Go to 1:20"), Some(vec![("seek".into(), 80.0)]));
        assert_eq!(names("jump to 1 minute and 30 seconds"), Some(vec![("seek".into(), 90.0)]));
        assert_eq!(names("split here"), Some(vec![("split_at_playhead".into(), -1.0)]));
        assert_eq!(names("add a title that says hello"), None);
        assert_eq!(names("go to the kitchen scene"), None);
    }

    #[test]
    fn hands_free_wake_words() {
        assert_eq!(hands_free("Copilot, add a marker called Chorus.", false), Heard::Command("add a marker called Chorus.".into()));
        assert_eq!(hands_free("Hey Rust Cut play it", false), Heard::Command("play it".into()));
        assert_eq!(hands_free("Copilot.", false), Heard::Wake);
        assert_eq!(hands_free("so then we went to the store", false), Heard::Ignore);
        assert_eq!(hands_free("make the title bigger", true), Heard::Command("make the title bigger".into()));
        assert_eq!(hands_free("Thanks.", true), Heard::Done);
        assert_eq!(hands_free("Okay, stop listening.", false), Heard::Sleep);
        assert_eq!(names("no, undo that"), Some(vec![("undo".into(), -1.0)]));
        assert_eq!(names("Next marker."), Some(vec![("go_to_marker".into(), -1.0)]));
    }

    #[test]
    fn strips_thinking() {
        assert_eq!(clean("<think>hmm</think>\nDone, faded it in."), "Done, faded it in.");
    }

    #[test]
    fn copilot_tools_are_well_formed() {
        let tools: Vec<Value> = crate::mcp::copilot_tools(false).into_iter().chain(app_tools()).collect();
        assert!(tools.len() >= crate::mcp::COPILOT_TOOLS.len());
        assert_eq!(crate::mcp::copilot_tools(true).len(), crate::mcp::COPILOT_CORE_TOOLS.len());
        for t in &tools {
            assert_eq!(t["type"], "function");
            assert!(t["function"]["name"].is_string() && t["function"]["parameters"]["type"] == "object", "{t}");
        }
    }

    /// Manual, free with Ollama (or paid with COPILOT_CLOUD=model): runs one request
    /// against an empty project through the same tool bridge the editor uses.
    /// `COPILOT_ASK="…" cargo test --release --lib copilot_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn copilot_live() {
        let Ok(ask) = std::env::var("COPILOT_ASK") else { return };
        let settings = crate::ai::openrouter::AiSettings::load();
        let backend = match std::env::var("COPILOT_CLOUD") {
            Ok(m) => Backend::OpenRouter { key: settings.api_key().unwrap(), model: m },
            Err(_) => Backend::Ollama { url: settings.ollama_url.clone(), model: std::env::var("COPILOT_MODEL").unwrap_or(settings.local_model.clone()) },
        };
        let tools: Vec<Value> = crate::mcp::copilot_tools(backend.local()).into_iter().chain(app_tools()).collect();
        println!("{} · {} tools, {} KB of schemas", backend.label(), tools.len(), serde_json::to_string(&tools).unwrap().len() / 1024);
        let mut project = crate::model::Project::default();
        let mut bridge = crate::mcp::Bridge::default();
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        let (rx, _c) = start(backend, system_prompt("project \"Test\", empty timeline, playhead 0 s, no clip selected"), vec![], ask, tools, ctx);
        for ev in rx {
            match ev {
                Event::Tool { name, args, reply } => {
                    let r = if app_tools().iter().any(|t| t["function"]["name"] == name.as_str()) { Ok("ok".into()) } else { bridge.call(&mut project, None, &name, &args) };
                    println!("  tool {name} {args} -> {}", match &r { Ok(s) => s.chars().take(160).collect::<String>(), Err(e) => format!("ERR {e}") });
                    reply.send(r).unwrap();
                }
                Event::Say { text, last } => println!("  say{}: {text}", if last { " (final)" } else { "" }),
                Event::Done { usd, .. } => {
                    println!("done in {:.1} s, ${usd:.4}", t0.elapsed().as_secs_f64());
                    break;
                }
                Event::Failed(e) => panic!("{e}"),
            }
        }
        for t in &project.tracks {
            println!("track {:?}: {} clips", t.kind, t.clips.len());
        }
    }
}
