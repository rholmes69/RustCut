//! "✨ Copilot" window: type or speak to control the editor. Ctrl+Space talks from
//! anywhere. See `ai::copilot` for the model loop and `ai::listen` for the microphone.

use crate::ai::copilot::{self, Event, Heard, WINDOWS};
use crate::ai::listen::Recorder;
use crate::app::EditorApp;
use crate::captions;
use crate::mcp::Bridge;
use crate::model::{ClipSource, Id};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Button, Color32, Context, Key, Modifiers, RichText, ScrollArea, TextEdit};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long follow-ups work without the wake word.
const FOLLOW_UP: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, PartialEq)]
enum Who {
    User,
    Ai,
    Tool,
    Error,
}

struct Line {
    who: Who,
    text: String,
}

struct Job {
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

const EXAMPLES: &[&str] = &[
    "Add a 1 second fade in and fade out to the selected clip",
    "Put a title that says \"Bond Money\" at the start for 3 seconds",
    "Make the selected clip black and white and 20% brighter",
    "Mute every audio track except the song",
    "How long is the video and how many clips are on the main track?",
];

#[derive(Default)]
pub struct CopilotUi {
    pub open: bool,
    input: String,
    log: Vec<Line>,
    /// Conversation sent back to the model (without the system prompt).
    history: Vec<Value>,
    job: Option<Job>,
    bridge: Bridge,
    rec: Option<Recorder>,
    hearing: Option<Task<String>>,
    spent: f64,
    focus_input: bool,
    show_settings: bool,
    /// Always listening for "Copilot, ..." / "RustCut, ...".
    pub hands_free: bool,
    /// The open recorder is the hands-free one (not push-to-talk).
    rec_hf: bool,
    /// The running transcription came from hands-free listening.
    hearing_hf: bool,
    /// Until then, a hands-free request needs no wake word (just after a reply).
    follow_until: Option<Instant>,
    /// Open the follow-up window once the spoken reply has finished.
    follow_pending: bool,
}

impl CopilotUi {
    /// While a request runs, its edits are held back from the undo history so the
    /// whole request becomes one undo step.
    pub fn holds_undo(&self) -> bool {
        self.job.is_some()
    }

    fn busy(&self) -> bool {
        self.job.is_some() || self.hearing.is_some()
    }

    fn say(&mut self, who: Who, text: impl Into<String>) {
        self.log.push(Line { who, text: text.into() });
        if self.log.len() > 200 {
            self.log.drain(..50);
        }
    }
}

/// A short description of the editor's state for the model.
fn state(app: &EditorApp) -> String {
    let p = &app.project;
    let mut s = format!(
        "project \"{}\", {} canvas, {} fps, length {:.2} s, playhead {:.2} s ({}), {} media items in the library\n",
        p.name,
        p.aspect.short_label(),
        p.fps,
        p.duration(),
        app.playhead,
        if app.playing { "playing" } else { "paused" },
        p.media.len()
    );
    let main = p.main_track_id();
    s.push_str("tracks (top to bottom):\n");
    for (i, t) in p.tracks.iter().enumerate() {
        s.push_str(&format!(
            "  #{i} id {} {:?} \"{}\": {} clips{}{}{}\n",
            t.id,
            t.kind,
            t.name,
            t.clips.len(),
            if Some(t.id) == main { ", main track" } else { "" },
            if t.muted { ", muted" } else { "" },
            if t.hidden { ", hidden" } else { "" },
        ));
    }
    match app.selection.and_then(|id| p.clip(id).map(|c| (id, c))) {
        Some((id, c)) => {
            let what = match &c.source {
                ClipSource::Text(t) => format!("text \"{}\"", t.content.chars().take(60).collect::<String>()),
                ClipSource::Media(m) => p.media(*m).map(|m| format!("{:?} \"{}\"", m.kind, m.name)).unwrap_or_default(),
            };
            let track = p.locate(id).map(|(t, _)| t).unwrap_or(0);
            s.push_str(&format!("selected clip: id {id}, {what}, on track #{track}, {:.2}–{:.2} s\n", c.start, c.end()));
        }
        None => s.push_str("no clip selected\n"),
    }
    if !p.markers.is_empty() {
        let list: Vec<String> = p.markers.iter().take(40).map(|m| format!("{} at {:.2} s", m.name, m.time)).collect();
        s.push_str(&format!("markers: {}\n", list.join("; ")));
    }
    let sections = crate::music::timeline_sections(p);
    if !sections.is_empty() {
        let list: Vec<String> = sections.iter().take(30).map(|x| format!("{} {:.1}–{:.1}", x.label, x.start, x.end)).collect();
        s.push_str(&format!("song sections (s): {}\n", list.join(", ")));
    }
    s
}

/// Runs an app control. `None` if `name` isn't one (it's an editing tool).
fn app_tool(ctx: &Context, app: &mut EditorApp, name: &str, a: &Value) -> Option<Result<String, String>> {
    let id = |k: &str| a[k].as_u64().map(|v| v as Id).ok_or_else(|| format!("missing {k}"));
    let r = match name {
        "play" => {
            app.play();
            Ok("playing".into())
        }
        "pause" => {
            app.pause();
            Ok("paused".into())
        }
        "seek" => {
            let t = a["time"].as_f64().unwrap_or(0.0).clamp(0.0, app.project.duration());
            app.seek(t);
            app.timeline.reveal = Some(t);
            Ok(format!("playhead at {t:.2} s"))
        }
        "select_clip" => id("clip_id").and_then(|c| {
            if app.project.clip(c).is_none() {
                return Err(format!("no clip with id {c}"));
            }
            crate::ui::media_preview::show_in_timeline(app, c);
            Ok(format!("selected clip {c}"))
        }),
        "preview_clip" => id("clip_id").and_then(|c| {
            let start = app.project.clip(c).map(|c| c.start).ok_or_else(|| format!("no clip with id {c}"))?;
            crate::ui::media_preview::open_clip(ctx, app, c, format!("{} · clip {c}", widgets::format_time(start)));
            Ok("preview open".into())
        }),
        "split_at_playhead" => {
            let n = app.project.clip_count();
            app.split_at_playhead();
            if app.project.clip_count() > n {
                Ok("split".into())
            } else {
                Err("nothing to split under the playhead".into())
            }
        }
        "ripple_delete" => {
            let Some(c) = a["clip_id"].as_u64().map(|v| v as Id).or(app.selection) else {
                return Some(Err("say which clip (or select one)".into()));
            };
            match app.project.ripple_delete(c) {
                Some(len) => {
                    app.project.cleanup_tracks();
                    Ok(format!("deleted clip {c} and closed the {len:.2} s gap"))
                }
                None => Err(format!("no clip with id {c}")),
            }
        }
        "freeze_frame" => {
            if let Some(t) = a["time"].as_f64() {
                app.seek(t);
            }
            let secs = a["seconds"].as_f64().unwrap_or(2.0).clamp(0.2, 30.0);
            app.freeze_frame(a["clip_id"].as_u64().map(|v| v as Id), secs).map(|id| format!("froze the frame for {secs} s (new still clip {id})"))
        }
        "add_marker" => {
            let t = a["time"].as_f64().unwrap_or(app.playhead);
            let name = a["name"].as_str().unwrap_or("").trim();
            let name = if name.is_empty() { format!("Marker {}", app.project.markers.len() + 1) } else { name.to_string() };
            app.project.add_marker(t, name.clone());
            Ok(format!("added marker \"{name}\" at {}", widgets::format_time(t)))
        }
        "go_to_marker" => {
            let dir = a["dir"].as_i64().unwrap_or(0) as i32;
            let found = if dir != 0 {
                app.project.marker_near(app.playhead, dir).cloned()
            } else {
                find_marker(&app.project.markers, a["name"].as_str().unwrap_or("")).map(|i| app.project.markers[i].clone())
            };
            match found {
                Some(m) => {
                    app.seek(m.time);
                    app.timeline.reveal = Some(m.time);
                    Ok(format!("at marker \"{}\"", m.name))
                }
                None => Err("no matching marker".into()),
            }
        }
        "delete_marker" => {
            let name = a["name"].as_str().unwrap_or("");
            if name.eq_ignore_ascii_case("all") {
                let n = app.project.markers.len();
                app.project.markers.clear();
                Ok(format!("deleted {n} markers"))
            } else {
                match find_marker(&app.project.markers, name) {
                    Some(i) => Ok(format!("deleted marker \"{}\"", app.project.markers.remove(i).name)),
                    None => Err("no matching marker".into()),
                }
            }
        }
        "markers_from_sections" => match app.markers_from_sections() {
            0 => Err("the song has no sections yet (detect beats or add lyrics with [Verse]/[Chorus] headers)".into()),
            n => Ok(format!("added {n} markers from the song sections")),
        },
        "copy_chapters" => {
            if app.project.markers.is_empty() {
                Err("there are no markers yet".into())
            } else {
                Ok(format!("copied to the clipboard:\n{}", app.copy_chapters(ctx)))
            }
        }
        "undo" => {
            app.undo();
            Ok("undone".into())
        }
        "redo" => {
            app.redo();
            Ok("redone".into())
        }
        "save_project" => {
            app.save(false);
            Ok(match &app.project_path {
                Some(p) if !app.is_dirty() => format!("saved to {}", p.display()),
                _ => "the save dialog is open".into(),
            })
        }
        "open_window" => {
            let w = a["window"].as_str().unwrap_or("");
            match w {
                "export" => app.open_export(),
                "timeline_storyboard" => app.clip_board.open = true,
                "storyboard" => app.storyboard.open = true,
                "rough_cut" => app.rough_cut.open = true,
                "voices" => app.voice.open = true,
                "transitions" => app.local.transitions_open = true,
                "generate_video" => app.video_gen.open = true,
                "generate_image" => app.image_gen.open = true,
                "lyrics" => app.music.lyrics_open = true,
                "beat_montage" => app.music.montage_open = true,
                "costs" => app.economics.open = true,
                "ai_settings" => app.ai.open_settings(ctx),
                _ => return Some(Err(format!("unknown window `{w}`; choose one of {}", WINDOWS.iter().map(|w| w.0).collect::<Vec<_>>().join(", ")))),
            }
            Ok(format!("opened {w}"))
        }
        _ => return None,
    };
    Some(r)
}

/// The marker whose name best matches `name` (exact, then contains, then shared words).
fn find_marker(markers: &[crate::model::Marker], name: &str) -> Option<usize> {
    let q = name.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let n = |i: usize| markers[i].name.to_lowercase();
    (0..markers.len())
        .find(|&i| n(i) == q)
        .or_else(|| (0..markers.len()).find(|&i| n(i).contains(&q) || q.contains(&n(i))))
        .or_else(|| {
            let words: Vec<&str> = q.split_whitespace().collect();
            (0..markers.len()).max_by_key(|&i| words.iter().filter(|w| n(i).contains(**w)).count()).filter(|&i| words.iter().any(|w| n(i).contains(*w)))
        })
}

fn run_tool(ctx: &Context, app: &mut EditorApp, name: &str, args: &Value) -> Result<String, String> {
    if let Some(r) = app_tool(ctx, app, name, args) {
        return r;
    }
    let path = app.project_path.clone();
    let mut bridge = std::mem::take(&mut app.copilot.bridge);
    let r = bridge.call(&mut app.project, path.as_deref(), name, args);
    app.copilot.bridge = bridge;
    if app.playing {
        app.audio.start(&app.project, app.playhead);
    }
    r
}

/// Short label for a tool call in the chat.
fn describe(name: &str, args: &Value) -> String {
    let a = args.as_object().map(|o| {
        o.iter()
            .take(4)
            .map(|(k, v)| {
                let v = v.to_string();
                let v = if v.chars().count() > 40 { format!("{}…", v.chars().take(40).collect::<String>()) } else { v };
                format!("{k}={v}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    format!("🔧 {name}({})", a.unwrap_or_default())
}

/// Sends a request: instant commands run now, everything else goes to the model.
pub fn submit(ctx: &Context, app: &mut EditorApp, text: String) {
    let text = text.trim().to_string();
    if text.is_empty() || app.copilot.job.is_some() {
        return;
    }
    app.copilot.open = true;
    app.copilot.say(Who::User, text.clone());
    if let Some(cmds) = copilot::quick_command(&text) {
        let mut said = Vec::new();
        for (name, args) in cmds {
            match run_tool(ctx, app, &name, &args) {
                Ok(r) => said.push(r),
                Err(e) => app.copilot.say(Who::Error, e),
            }
        }
        if !said.is_empty() {
            let reply = said.join(", ");
            let mut c = reply.chars();
            let reply = c.next().map(|f| format!("{}{}.", f.to_uppercase(), c.as_str())).unwrap_or_default();
            if app.ai.settings.copilot_speak {
                copilot::speak(&reply);
            }
            app.copilot.say(Who::Ai, reply);
        }
        app.copilot.follow_pending = app.copilot.hands_free;
        return;
    }
    let backend = match copilot::backend(&app.ai.settings) {
        Ok(b) => b,
        Err(e) => return app.copilot.say(Who::Error, e.to_string()),
    };
    app.history.commit(&app.project);
    let tools: Vec<Value> = crate::mcp::copilot_tools(backend.local()).into_iter().chain(copilot::app_tools()).collect();
    // Keep the last few exchanges so follow-ups ("make it longer") work.
    let mut history = app.copilot.history.clone();
    if history.len() > 40 {
        let cut = history.iter().enumerate().skip(history.len() - 40).find(|(_, m)| m["role"] == "user").map(|(i, _)| i).unwrap_or(0);
        history.drain(..cut);
    }
    let (rx, cancel) = copilot::start(backend, copilot::system_prompt(&state(app)), history, text, tools, ctx.clone());
    app.copilot.job = Some(Job { rx, cancel });
}

fn start_listening(app: &mut EditorApp) {
    // Hands-free is already listening: treat the shortcut like the wake word.
    if app.copilot.hands_free {
        app.copilot.open = true;
        app.copilot.follow_until = Some(Instant::now() + FOLLOW_UP);
        return;
    }
    if app.copilot.rec.is_some() || app.copilot.busy() {
        return;
    }
    app.copilot.open = true;
    match Recorder::start() {
        Ok(r) => app.copilot.rec = Some(r),
        Err(e) => app.copilot.say(Who::Error, format!("Microphone: {e:#}")),
    }
}

/// The speech model to use: the captions' default if downloaded, else any downloaded one.
fn speech_model() -> Option<std::path::PathBuf> {
    std::iter::once(captions::DEFAULT_MODEL)
        .chain(captions::MODELS.iter().map(|m| m.name))
        .find(|m| captions::is_downloaded(m))
        .map(captions::model_path)
}

fn stop_listening(ctx: &Context, app: &mut EditorApp) {
    let Some(rec) = app.copilot.rec.take() else { return };
    let hf = std::mem::take(&mut app.copilot.rec_hf);
    if !rec.heard_speech() {
        if !hf {
            app.copilot.say(Who::Error, "I didn't hear anything — check the microphone, or type instead.");
        }
        return;
    }
    let Some(model) = speech_model() else {
        app.copilot.say(Who::Error, "Voice needs a speech model: download one in the Captions panel (Base is recommended), then try again.");
        app.copilot.hands_free = false;
        return;
    };
    app.copilot.hearing_hf = hf;
    let wav = std::env::temp_dir().join(format!("rustcut-voice-{}.wav", std::process::id()));
    let secs = match rec.finish(&wav) {
        Ok(s) => s,
        Err(e) => return app.copilot.say(Who::Error, format!("{e:#}")),
    };
    app.copilot.hearing = Some(Task::spawn(ctx.clone(), move |cancel, _| {
        let engine = captions::detect_engine().ok_or_else(|| anyhow::anyhow!("no speech-to-text engine (FFmpeg with whisper, or whisper.cpp) was found"))?;
        let opts = captions::Options { model, language: "en".into(), translate: false, max_len: None };
        let text = captions::transcribe_file(&wav, secs, &opts, &engine, cancel);
        let _ = std::fs::remove_file(&wav);
        text
    }));
}

/// Call every frame (before the editor's shortcuts).
pub fn poll(ctx: &Context, app: &mut EditorApp) {
    // Ctrl+Space: talk (or stop talking) from anywhere.
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space)) {
        if app.copilot.rec.is_some() {
            stop_listening(ctx, app);
        } else {
            start_listening(app);
        }
    }
    // Hands-free: keep a recorder open whenever nothing else is happening.
    let cp = &mut app.copilot;
    if cp.hands_free && cp.rec.is_none() && cp.hearing.is_none() {
        match Recorder::start_hands_free() {
            Ok(r) => {
                cp.rec = Some(r);
                cp.rec_hf = true;
            }
            Err(e) => {
                cp.hands_free = false;
                cp.say(Who::Error, format!("Microphone: {e:#}"));
            }
        }
    }
    if cp.follow_pending && !copilot::is_speaking() {
        cp.follow_pending = false;
        cp.follow_until = Some(Instant::now() + FOLLOW_UP);
    }
    if cp.follow_until.is_some_and(|t| Instant::now() > t) {
        cp.follow_until = None;
    }
    if let Some(rec) = &mut app.copilot.rec {
        ctx.request_repaint_after(Duration::from_millis(30));
        if app.copilot.rec_hf && copilot::is_speaking() {
            // Don't listen to our own voice.
            rec.discard();
        } else if rec.should_stop() {
            stop_listening(ctx, app);
        }
    }
    if let Some(state) = app.copilot.hearing.as_ref().and_then(|t| t.take_finished()) {
        app.copilot.hearing = None;
        let hf = std::mem::take(&mut app.copilot.hearing_hf);
        match state {
            TaskState::Done(text) if hf => heard_hands_free(ctx, app, &text),
            TaskState::Done(text) if !text.trim().is_empty() => submit(ctx, app, text),
            TaskState::Done(_) => app.copilot.say(Who::Error, "I couldn't make out any words — try again a little closer to the mic."),
            TaskState::Failed(e) => app.copilot.say(Who::Error, format!("Speech recognition: {e}")),
            _ => {}
        }
    }
    // Model events.
    loop {
        let Some(job) = &app.copilot.job else { break };
        let Ok(ev) = job.rx.try_recv() else { break };
        match ev {
            Event::Tool { name, args, reply } => {
                app.copilot.say(Who::Tool, describe(&name, &args));
                let r = run_tool(ctx, app, &name, &args);
                if let Err(e) = &r {
                    app.copilot.say(Who::Error, format!("{name}: {e}"));
                }
                let _ = reply.send(r);
            }
            Event::Say { text, last } => {
                if !text.is_empty() {
                    if last && app.ai.settings.copilot_speak {
                        copilot::speak(&text);
                    }
                    app.copilot.say(Who::Ai, text);
                }
            }
            Event::Done { history, usd } => {
                app.copilot.history = history;
                app.copilot.spent += usd;
                app.copilot.job = None;
                app.copilot.follow_pending = app.copilot.hands_free;
            }
            Event::Failed(e) => {
                app.copilot.say(Who::Error, e);
                app.copilot.job = None;
            }
        }
    }
}

/// A hands-free utterance: act on it if it starts with the wake word (or follows a reply).
fn heard_hands_free(ctx: &Context, app: &mut EditorApp, text: &str) {
    let following = app.copilot.follow_until.is_some();
    let reply = |app: &mut EditorApp, t: &str| {
        if app.ai.settings.copilot_speak {
            copilot::speak(t);
        }
        app.copilot.say(Who::Ai, t);
    };
    match copilot::hands_free(text, following) {
        Heard::Command(c) => {
            app.copilot.follow_until = None;
            if let Some(job) = &app.copilot.job {
                // Busy: only "stop" / "cancel" mean something now.
                let w = c.to_lowercase();
                if ["stop", "cancel", "never mind", "wait"].iter().any(|k| w.contains(k)) {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                return;
            }
            submit(ctx, app, c);
        }
        Heard::Wake => {
            reply(app, "Yes?");
            app.copilot.follow_pending = true;
        }
        Heard::Done => {
            app.copilot.follow_until = None;
            reply(app, "Okay.");
        }
        Heard::Sleep => {
            app.copilot.hands_free = false;
            app.copilot.rec = None;
            app.copilot.rec_hf = false;
            app.copilot.follow_until = None;
            reply(app, "Hands-free is off.");
        }
        Heard::Ignore => {}
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    if !app.copilot.open {
        return;
    }
    let mut open = true;
    let mut send = None;
    egui::Window::new(RichText::new("✨ Copilot").strong())
        .id(egui::Id::new("copilot"))
        .open(&mut open)
        .default_width(380.0)
        .default_pos(ctx.screen_rect().right_top() + egui::vec2(-400.0, 60.0))
        .resizable(true)
        .show(ctx, |ui| {
            let backend = copilot::backend(&app.ai.settings).map(|b| b.label()).unwrap_or_else(|_| "cloud · no OpenRouter key".into());
            ui.horizontal(|ui| {
                ui.label(widgets::dim(backend).small());
                if app.copilot.spent > 0.0 {
                    ui.label(widgets::dim(format!("· ${:.4} this session", app.copilot.spent)).small());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let hf = app.copilot.hands_free;
                    let label = RichText::new("👂 Hands-free").color(if hf { theme::ACCENT } else { theme::TEXT_DIM });
                    if ui
                        .selectable_label(hf, label)
                        .on_hover_text("Always listening: say \"Copilot, …\" (or \"RustCut, …\"). Right after a reply you can just keep talking. Say \"stop listening\" to turn it off.")
                        .clicked()
                    {
                        let cp = &mut app.copilot;
                        cp.hands_free = !hf;
                        cp.rec = None;
                        cp.rec_hf = false;
                        cp.follow_until = None;
                        if cp.hands_free && speech_model().is_none() {
                            cp.hands_free = false;
                            cp.say(Who::Error, "Hands-free needs a speech model: download one in the Captions panel first.");
                        }
                    }
                    if ui.small_button("⚙").on_hover_text("Copilot settings").clicked() {
                        app.copilot.show_settings ^= true;
                    }
                    if ui.small_button("🗑").on_hover_text("Clear the conversation").clicked() {
                        app.copilot.log.clear();
                        app.copilot.history.clear();
                    }
                });
            });
            if app.copilot.show_settings {
                settings(ui, app);
            }
            ui.separator();

            ScrollArea::vertical().max_height(340.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                if app.copilot.log.is_empty() {
                    ui.label(widgets::dim("Tell RustCut what to do — type below, or press 🎤 / Ctrl+Space and speak. Try:").small());
                    for ex in EXAMPLES {
                        if ui.add(Button::new(RichText::new(*ex).small()).wrap()).clicked() {
                            send = Some(ex.to_string());
                        }
                    }
                }
                for l in &app.copilot.log {
                    let (prefix, color) = match l.who {
                        Who::User => ("🗣 ", theme::ACCENT),
                        Who::Ai => ("✨ ", theme::TEXT),
                        Who::Tool => ("", theme::TEXT_DIM),
                        Who::Error => ("⚠ ", theme::DANGER),
                    };
                    let rt = RichText::new(format!("{prefix}{}", l.text)).color(color);
                    ui.add(egui::Label::new(if l.who == Who::Tool { rt.small().monospace() } else { rt }).wrap());
                }
            });
            ui.separator();

            if let (true, Some(rec)) = (app.copilot.rec_hf, &app.copilot.rec) {
                let level = (rec.level() * 12.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    let text = if copilot::is_speaking() {
                        "🔊 Speaking…"
                    } else if app.copilot.follow_until.is_some() {
                        "💬 Go ahead — no need to say \"Copilot\""
                    } else if app.copilot.job.is_some() {
                        "👂 Working… say \"Copilot, stop\" to cancel"
                    } else {
                        "👂 Say \"Copilot, …\""
                    };
                    ui.label(RichText::new(text).color(theme::ACCENT));
                    ui.add(egui::ProgressBar::new(level).desired_width(ui.available_width()));
                });
            }
            if let (false, Some(rec)) = (app.copilot.rec_hf, &app.copilot.rec) {
                let level = (rec.level() * 12.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    if ui.add(Button::new(RichText::new("⏹ Done").color(Color32::WHITE)).fill(theme::DANGER)).clicked() {
                        send = Some(String::new());
                    }
                    ui.label(if rec.heard_speech() { "Listening…" } else { "Speak now…" });
                    ui.add(egui::ProgressBar::new(level).desired_width(ui.available_width()));
                });
            } else if app.copilot.hearing.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Transcribing…");
                });
            } else if app.copilot.job.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Working…");
                    if ui.button("Stop").clicked() {
                        if let Some(j) = &app.copilot.job {
                            j.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                });
            }

            ui.horizontal(|ui| {
                let mic = ui
                    .add_enabled(!app.copilot.busy() && app.copilot.rec.is_none(), Button::new(RichText::new("🎤").size(18.0)).min_size(egui::vec2(36.0, 36.0)))
                    .on_hover_text("Speak a command (Ctrl+Space). Stops by itself when you pause.");
                if mic.clicked() {
                    start_listening(app);
                }
                let edit = ui.add(
                    TextEdit::multiline(&mut app.copilot.input)
                        .desired_rows(2)
                        .desired_width(ui.available_width())
                        .hint_text("Tell RustCut what to do…  (Enter sends, Shift+Enter new line)"),
                );
                if std::mem::take(&mut app.copilot.focus_input) {
                    edit.request_focus();
                }
                if edit.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift) {
                    let t = std::mem::take(&mut app.copilot.input);
                    send = Some(t.trim_end_matches('\n').to_string());
                }
            });
        });
    match send {
        Some(t) if t.is_empty() => stop_listening(ctx, app),
        Some(t) => submit(ctx, app, t),
        None => {}
    }
    if !open {
        app.copilot.open = false;
        if let Some(j) = &app.copilot.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        app.copilot.rec = None;
        app.copilot.rec_hf = false;
        app.copilot.hands_free = false;
    }
}

fn settings(ui: &mut egui::Ui, app: &mut EditorApp) {
    let s = &mut app.ai.settings;
    let before = (s.copilot_cloud, s.copilot_model.clone(), s.copilot_speak, s.local_model.clone());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.copilot_cloud, false, "Local (Ollama, free)");
            ui.radio_value(&mut s.copilot_cloud, true, "Cloud (OpenRouter)");
        });
        if s.copilot_cloud {
            ui.horizontal(|ui| {
                ui.label("Model");
                ui.add(TextEdit::singleline(&mut s.copilot_model).desired_width(220.0))
                    .on_hover_text("Any OpenRouter model that supports tool calling, e.g. google/gemini-3.8-flash or anthropic/claude-sonnet-5.5");
            });
            if s.api_key().is_none() {
                ui.label(RichText::new("Add an OpenRouter key in AI → OpenRouter settings.").color(theme::DANGER).small());
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("Model");
                egui::ComboBox::from_id_salt("copilot-local").selected_text(&s.local_model).show_ui(ui, |ui| {
                    for m in &app.local.models {
                        ui.selectable_value(&mut s.local_model, m.clone(), m);
                    }
                });
            });
            ui.label(widgets::dim("Needs a tool-capable model such as qwen3:8b.").small());
        }
        ui.checkbox(&mut s.copilot_speak, "🔊 Speak replies");
        let voice = if speech_model().is_some() { "Voice: local Whisper ready." } else { "Voice: download a speech model in the Captions panel." };
        ui.label(widgets::dim(voice).small());
    });
    if before != (s.copilot_cloud, s.copilot_model.clone(), s.copilot_speak, s.local_model.clone()) {
        let _ = s.save();
    }
}

/// Opens the Copilot with the text box focused.
pub fn show(app: &mut EditorApp) {
    app.copilot.open = true;
    app.copilot.focus_input = true;
}
