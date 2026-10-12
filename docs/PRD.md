# RustCut — Product Requirements (v0.1)

## 1. Vision
A fast, free, offline desktop video editor that gives creators the CapCut editing
experience (library → timeline → preview → export in minutes) without an account,
cloud upload, watermark or Electron-sized memory footprint. Native Rust UI, FFmpeg
under the hood.

## 2. Target users
| Persona | Needs | What "done" looks like |
|---|---|---|
| **Short-form creator** (TikTok / Reels / Shorts) | 9:16 canvas, captions, quick cuts, music bed | Cut a 30 s vertical clip with captions and music in < 5 min |
| **YouTuber / educator** | 16:9, overlays, titles, voice-over, fades | Assemble a 10 min video from several takes |
| **Social marketer** | 1:1 / 4:5, brand text, filters | Re-crop one asset into several aspect ratios |

## 3. Scope

### P0 — MVP (implemented in v0.1)
- **Media library**: import video / image / audio via dialog or drag-and-drop; thumbnails, durations, waveforms; remove; reveal in folder.
- **Multi-track timeline**: magnetic-free tracks auto-created per type (text, video overlays, main track, audio). Move clips across tracks, trim both edges, split at playhead, duplicate, delete, snapping to playhead/clip edges, zoom and scroll, ruler scrubbing, lock / hide / mute per track.
- **Preview player**: real-time composited preview of all layers (video, image, text), play/pause, frame stepping, audio playback, on-canvas drag to reposition, wheel to scale.
- **Inspector**: transform (position, scale, rotation, opacity, fit/fill), speed 0.25×–4×, volume, fade in/out, filters (B&W, Vintage, Warm, Cool, Vivid, Matte) and adjustments (brightness, contrast, saturation), text styling (size, bold, colour, background, outline).
- **Text**: style presets (Title, Subtitle, Caption, Neon, Lower third).
- **Animations / transitions**: fade in, fade out, dip-to-black between adjacent clips.
- **Audio**: extract (detach) audio from a video clip onto its own track.
- **Canvas**: 16:9, 9:16, 1:1, 4:3, 4:5.
- **Export**: H.264/AAC MP4, 720p–4K, 24–60 fps, three quality tiers, progress and cancel. Headless `--export` CLI for batch rendering.
- **Project**: save/load `.rcproj` (JSON), unlimited-feeling undo/redo (200 steps).
- **Music video toolkit** (v1.1): beat/bar/section detection (spectral-flux onsets, autocorrelation tempo, DP beat tracking, bass-weighted downbeats, loudness sections) driving ruler marks, snapping, beat montage (energy-aware cut rates) and beat effects; multi-cam sync by audio cross-correlation with live number-key or auto switching; lyrics from .lrc, aligned pasted lyrics or transcription in three styles with .lrc export; FFmpeg visualizer overlays and bass/loudness-reactive keyframes; AI music video (bar-timed shots, section-aware shot reuse, storyboard approval) and best-30-seconds trim.
- **Prompt-to-video storyboard** (v1.0): a text model writes the script and shot list (JSON schema); the Image API draws one frame per shot (the first drawn frame is the style reference for the rest); the user approves, regenerates, edits, reorders, removes or swaps frames; only approved shots go to image-to-video (in parallel), so video money is spent only on approved shots; assembly appends them in order with 0.5 s cross-dissolves, captions from the shot list, optional ducked music and a −14 LUFS target. Storyboards persist as `storyboard.json` and can be reopened.
- **AI thumbnails, title cards & backgrounds** (v0.9): OpenRouter Image API with the chosen frame as a reference image. Thumbnail maker (playhead frame + headline + style → PNG, set as the project thumbnail and copied next to exports); title cards, lower-third backdrops and transparent stickers placed on overlay tracks as image clips; background fill outpaints a clip's middle frame into a canvas-shaped plate drawn behind it (preview and export). Transparent kinds fall back to a model that supports transparency.
- **AI rough cut by prompt** (v0.8): transcript (whisper, ≈ word timing) + low-res video/audio to a video-capable model; keep-ranges snapped to speech boundaries; preview then one-undo ripple cut across all tracks; titles, description and chapters remapped to the cut.
- **AI B-roll & shot extension** (v0.7): text/image-to-video through OpenRouter's async video API; model picker with per-clip cost estimates; extend a clip from its last frame or bridge two clips (first + last frame); ripple-inserted after the clip.
- **AI platform + auto-reframe** (v0.6): shared OpenRouter client (key from env or user settings, model picker with live prices, cost tracking, actionable errors); auto-reframe follows the subject when changing aspect ratio, as editable keyframes.
- **Smart audio** (v0.5): auto-ducking driven by voice detection with per-clip roles (voice/music/effect), adaptive noise removal, export loudness normalisation (−14/−16/−23 LUFS, ±0.2 LU) and a loudness meter.
- **Transitions** (v0.4): 12 styles at cuts between touching clips on video tracks, centred on the cut with automatic frame-hold when a clip has no spare footage; timeline "+" cut markers, inspector and Animation-tab pickers; MCP `set_transition`.
- **Keyframe animation** (v0.3): position, scale, rotation, opacity and volume with linear/smooth/ease-in/ease-out/hold easing; motion presets; keyframes stay attached to the content through split, trim and speed changes.
- **Auto-captions** (v0.2): on-device speech-to-text via whisper.cpp; captions are editable text clips on a dedicated track; styles, words-per-caption, position, language/translate; `.srt` export (also offered at video export).

### P1 — Next
- More transition styles (circle, blur, glitch), audio crossfades at transitions, bezier keyframe curves.
- Word-by-word karaoke highlighting for captions, VAD for tighter timing, text-to-speech.
- Ripple delete & magnetic main track toggle.
- Hardware-accelerated decode/encode (NVENC/QSV/VideoToolbox), GPU compositor (wgpu).
- Proxy media for 4K sources, background render cache.

### P2 — Later
- Stickers / effects library, chroma key, masks, blend modes, speed curves, beat sync.
- Templates, cloud sync, collaboration.

### Non-goals (v0.1)
Accounts, cloud storage, online asset store, mobile build.

## 4. UX principles
1. **Four panes, one glance** — Library (left), Preview (centre), Inspector (right), Timeline (bottom) — same mental model as CapCut.
2. **Direct manipulation first** — drag media to the timeline, drag clips, drag on the canvas; menus are the fallback.
3. **Never block the UI** — probing, thumbnails, waveforms, decoding and export run off the UI thread.
4. **What you see is what you export** — preview and export share the same layout math, filter definitions and fonts.

## 5. Keyboard shortcuts
| Action | Shortcut |
|---|---|
| Play / pause | Space |
| Split at playhead | Ctrl+B |
| Delete clip | Delete / Backspace |
| Duplicate clip | Ctrl+D |
| Undo / Redo | Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) |
| Step frame / second | ← → / Shift+← → |
| Go to start / end | Home / End |
| Import / Export | Ctrl+I / Ctrl+E |
| Open / Save / Save as | Ctrl+O / Ctrl+S / Ctrl+Shift+S |
| Zoom timeline | Ctrl+scroll |

## 6. Success metrics
- Cold start < 1 s; idle memory < 150 MB.
- Scrub latency < 150 ms on 1080p H.264.
- Export of a 60 s 1080p30 project < 60 s on a 4-core laptop.
- 0 crashes on malformed media (errors are surfaced as toasts).

## 7. Risks
| Risk | Mitigation |
|---|---|
| FFmpeg not installed | Detected at start-up with a clear message; `RUSTCUT_FFMPEG` env override |
| Preview/export mismatch | Shared layout + filter modules, unit tests on the export graph |
| Process-per-clip decoding cost | Bounded channels (back-pressure), pre-warm next clip, throttled scrubbing |
