<p align="center"><img src="assets/icons/rustcut-logo.png" alt="RustCut" width="300"></p>

# RustCut

A CapCut-style desktop video editor written in Rust — native UI (egui), FFmpeg for
decoding and rendering, offline, no account, no watermark.

```
┌───────────────────────── Top bar: File · Edit · name · undo/redo · ratio · Export ─────────────────────────┐
│ Library (Media / Audio / Text /     │            Preview canvas            │  Inspector                     │
│ Filters / Animation)                │      (drag to move, scroll to scale) │  (transform, speed, audio,     │
│                                     │   ⏮ ⏪ ▶ ⏩ ⏭  transport              │   animation, filters, text)    │
├─────────────────────────────────────┴──────────────────────────────────────┴────────────────────────────────┤
│ Timeline: toolbar · ruler · text tracks · overlay tracks · Main track · audio tracks                         │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Features
- Import video, images and audio (dialog or drag & drop) with thumbnails and waveforms
- Multi-track timeline: move across tracks, trim both edges, split, duplicate, delete, snapping, zoom, lock/hide/mute
- Real-time composited preview with audio, frame stepping and on-canvas positioning
- Transform (position, scale, rotation, opacity, fit/fill), speed 0.25×–4×, volume, fade in/out
- **Keyframe animation** for position, scale, rotation, opacity and volume: ◆ toggles in the inspector,
  5 easing curves, prev/next keyframe navigation, diamonds on the timeline, and one-click motion presets
  (Ken Burns, punch-in, slide in, pop in, spin in). Exports frame-accurately via per-frame FFmpeg expressions
- Filters (B&W, Vintage, Warm, Cool, Vivid, Matte) + brightness/contrast/saturation
- Text with presets, colour, background box and outline
- **Auto-captions**: local speech-to-text (whisper.cpp) → timed, editable caption clips; 4 styles,
  words-per-caption control, 19 languages + translate-to-English, `.srt` export
- **Transitions** at any cut between touching clips: cross-dissolve, dip to black/white, slide ×4,
  wipe ×4, zoom in; 0.1–3 s, centred on the cut using footage past each clip's edges (timeline length
  unchanged). Click the "+" on a cut in the timeline, or pick one in the Animation tab / inspector
- **Smart audio**: auto-ducking (music dips under speech as editable volume keyframes), noise removal
  per clip (Light/Medium/Strong, tuned to the measured noise floor), loudness normalisation of the export
  to −14/−16/−23 LUFS with a true-peak limiter, and a loudness meter
- **AI auto-reframe** (OpenRouter vision models): turn 16:9 into 9:16/1:1/4:5 with the subject kept in
  frame — written as smooth, editable position keyframes. AI → Auto-reframe all clips, or per clip in the inspector
- **AI B-roll & shot extension** (OpenRouter video models: Veo, Seedance, Kling, Wan, Grok Imagine…):
  generate clips from a prompt or from the frame at the playhead (AI → Generate video), or extend a clip
  from its last frame — optionally blending into the next clip (Inspector → Extend shot). Cost shown upfront
- **AI rough cut by prompt**: "60-second highlight, keep the funny bits, cut the ums" — a video-understanding
  model reads the word-level transcript and watches a tiny copy of the timeline; cuts are previewed in red, then
  ripple-applied across all tracks in one undo. Also writes titles, a YouTube description and chapters
- **💲 Cost report** (File → Cost report): every paid AI call is recorded in a local SQLite database
  (`economics.sqlite`) against the file it produced; reports compare RustCut's actual spend with commercial tool
  stacks (Adobe, CapCut Pro, Runway + Descript, pay-as-you-go APIs) per video clip, per audio clip and per project,
  with subscriptions spread over your videos per month. Save as HTML or CSV; prices are dated and editable
- **Local AI (Ollama, free, offline, private)**: a small decision model (default `qwen3`, also `tev1`; set in
  AI → settings) answers one multiple-choice question at a time:
  - *Offline rough cut* (AI → Rough cut → Engine: Local): keeps, cuts or trims the filler of each transcript sentence;
    quiet pauses go, music and footage without speech stay
  - *Smart transitions* (AI → Smart transitions): hard cut, dissolve, fade through black, slide or zoom per cut, from
    the song section, energy, beat and clips
- **Music videos** (Music menu; all free and local unless marked AI):
  - *Beat sync*: detects tempo, beats, bars and song sections; beats and sections show on the ruler and clips snap
    to them. **Beat montage** cuts a pile of clips on every beat / 2 beats / bar / 2 bars or "auto" (faster in loud
    parts); **beat effects** add zoom punch, shake, flicker or step zoom on the beat
  - *Multi-cam*: syncs performance takes to the song by their audio, one "Cam N" track each; switch angles live with
    the number keys while it plays (snapped to beats) or auto-switch on the beat grid onto a "Multi-cam edit" track
  - *Lyrics*: paste lyrics (aligned to the vocals with whisper), open an `.lrc`, or transcribe; styles: karaoke (the white
    line lights up word by word as it is sung), lines, big centred lines, pop words (one big word at a time,
    optionally landing on the beat) and build-up; `.srt`/`.lrc` export
  - *Audio-reactive*: spectrum bars, circular spectrum, waveform and vectorscope visualizer overlays (transparent), and
    throb, glow, blur, shake, fade or wobble that follow the bass or loudness. Blur and glow are also ordinary
    animatable clip properties (inspector)
  - *AI music video*: shots planned on the bars from your idea, the song's sections and lyrics; repeated sections
    reuse shots; storyboard approval before any video is paid for; cut exactly on the beat. Plus **best 30 seconds**
- **🪄 Make a video from a prompt** (AI → Make a video, the storyboard): "30-second ad for a coffee shop,
  warm and cozy, 9:16" → a script and shot list → one generated frame per shot that you approve, regenerate,
  edit, reorder or swap for your own image → only approved shots are animated (image-to-video, in parallel) and
  assembled with cross-dissolves, captions, ducked music and −14 LUFS. Costs are shown before each step
- **AI thumbnails, title cards & backgrounds** (OpenRouter image models, a few cents each), AI → Generate image:
  - *Thumbnail maker*: the frame at the playhead + a headline → a punchy YouTube-style PNG, saved next to the
    exported video as `<name>-thumbnail.png`
  - *Stills*: title cards, lower-third backdrops and transparent stickers from a prompt, dropped on an overlay
    track as ordinary image clips (filters, keyframes and transitions apply)
  - *Background fill*: a landscape clip in a 9:16 canvas gets an outpainted, canvas-filling background plate
    instead of black bars (Inspector → Background fill, or right-click → Fill background)
- Extract audio from video
- 16:9, 9:16, 1:1, 4:3, 4:5 canvases
- MP4 export (H.264/AAC) 480p–4K, 24–60 fps, with progress and cancel
- Save/load projects (`.rcproj`), **File → Open recent** (last 5), autosave + crash recovery, 200-step undo/redo

## Requirements
- Rust 1.80+
- FFmpeg + ffprobe on `PATH` (Windows: `winget install ffmpeg`). Override with `RUSTCUT_FFMPEG` / `RUSTCUT_FFPROBE`.
- Auto-captions: an FFmpeg 8+ build with the `whisper` filter (the gyan.dev "full" build has it), **or**
  whisper.cpp's `whisper-cli` on `PATH` (`RUSTCUT_WHISPER` to override). Speech models are downloaded
  in-app on first use to `%LOCALAPPDATA%\RustCut\models` (`RUSTCUT_MODELS_DIR` / `RUSTCUT_WHISPER_MODEL` to override).

## AI features (optional)
AI features use models through [OpenRouter](https://openrouter.ai) and your own API key: set the
`OPENROUTER_API_KEY` environment variable, or paste a key in **AI → OpenRouter settings** (stored in your
user profile, never in projects). Pick any vision model there; the default is a cheap, fast Gemini Flash Lite.
Only small still frames (≤512 px) are sent, and only when you run a feature.

## Run
```bash
cargo run --release
```
Open a project or media file directly: `rustcut path\to\project.rcproj`.

Headless render (batch / CI):
```bash
rustcut --export project.rcproj out.mp4 1080 30
```

## MCP server (AI agents)
`rustcut-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server (stdio) that lets
an AI agent edit RustCut projects: import media, place and trim clips, add text, apply speed/filters/fades,
split, ripple-delete, undo, **render a frame as an image** to check its work, save a `.rcproj`, and export MP4.

```bash
cargo build --release --bin rustcut-mcp
claude mcp add rustcut -- C:/Users/sales/MyVideoEditor/target/release/rustcut-mcp.exe
```
This repo also ships a `.mcp.json`, so Claude Code opened in this folder offers to connect automatically.
Other clients (Claude Desktop, Cursor, …) use the same command in their MCP config.

Tools: `get_project`, `new_project`, `open_project`, `save_project`, `update_project`, `import_media`,
`remove_media`, `add_clip`, `add_text`, `update_clip`, `split_clip`, `delete_clip`, `duplicate_clip`,
`update_track`, `undo`, `redo`, `render_frame`, `export_video`, `download_caption_model`, `auto_caption`, `export_srt`, `set_keyframes`, `clear_keyframes`, `apply_motion_preset`, `set_transition`, `auto_duck`, `set_clip_audio`, `measure_loudness`, `auto_reframe`, `list_video_models`, `generate_video`, `extend_clip`, `rough_cut`, `generate_thumbnail`, `generate_still`, `background_fill`, `storyboard_plan`, `storyboard_update_shot`, `storyboard_produce`, `detect_beats`, `beat_montage`, `beat_effect`, `multicam_sync`, `multicam_switch`, `add_lyrics`, `export_lrc`, `add_visualizer`, `audio_react`, `music_video_plan`, `best_section`, `auto_transitions` (local AI), `cost_report`, `set_price`; `rough_cut` also takes `engine: "local"`. For AI tools, give the server a key via an `env` block in `.mcp.json`.

Example prompt: *"Make a 9:16 short from clip.mp4: keep 0:10–0:25, add the caption 'Day 1' at the top,
put music.mp3 underneath at 40% volume, check a frame, then export to C:/Videos/short.mp4."*

## Test
```bash
cargo test
```

## Docs
- [Product requirements](docs/PRD.md) — personas, scope, roadmap, shortcuts, metrics
- [Architecture](docs/ARCHITECTURE.md) — modules, threading, coordinate conventions
