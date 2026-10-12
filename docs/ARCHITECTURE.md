# RustCut — Architecture

```
                ┌────────────────────────── UI thread (egui / eframe) ──────────────────────────┐
                │  topbar   library   preview   inspector   timeline   export_dialog            │
                │        \      |        |          |          |          /                   │
                │         └──────────── EditorApp (app.rs) ───────────────┘                     │
                │              project: Project   history: History (snapshot undo)              │
                └───────┬───────────────┬──────────────────┬───────────────────┬────────────────┘
                        │               │                  │                   │
              AssetManager      PreviewEngine        AudioEngine          ExportJob
              (probe, thumbs,   (1 ffmpeg decode     (1 ffmpeg PCM pipe   (1 ffmpeg process,
               waveforms)        pipe per visible     per audible clip →   filter_complex
               worker threads    clip → RGBA frames   rodio mixer)         graph, progress)
                                 → GPU textures)
```

## Modules
| Path | Responsibility |
|---|---|
| `model.rs` | Pure data: `Project`, `Track`, `Clip`, `MediaItem`, styles. All edit operations (place, split, duplicate, overlap resolution, track housekeeping). Serde + `PartialEq` so the whole project can be snapshotted and saved. |
| `history.rs` | Snapshot undo/redo. The app "auto-commits" a snapshot whenever the project changed and no pointer/keyboard interaction is in progress — every edit is undoable without per-command bookkeeping. |
| `media/ffmpeg.rs` | Locating FFmpeg, `ffprobe` parsing, single-frame extraction, waveform peaks, `atempo` chains. Console windows suppressed on Windows. |
| `media/decoder.rs` | `VideoStream`: an FFmpeg child process emitting raw RGBA frames at preview fps into a bounded channel (back-pressure keeps memory flat). Killed on drop. |
| `media/assets.rs` | Background import (probe), thumbnails, waveforms, filter-preview thumbnails. |
| `playback/mod.rs` | `PreviewEngine`: keeps one decoder slot per visible clip, decides when to restart (seek) vs. drain (play), pre-warms clips about to start, applies colour filters on CPU and uploads textures. |
| `playback/audio.rs` | `AudioEngine`: lazily starts a PCM pipe per audible clip shortly before it begins, with volume and fades applied per sample. |
| `render/` | Shared "what you see is what you export" maths: layer sizing, text metrics, fonts, colour filters (CPU implementation + equivalent FFmpeg filter strings). |
| `export.rs` | Builds a single FFmpeg `filter_complex` graph (black canvas → per-clip overlay chains → `drawtext` → `amix`) and runs it with progress reporting and cancel. Also used by the headless CLI. |
| `mcp.rs` + `bin/rustcut-mcp.rs` | MCP server (JSON-RPC 2.0 over stdio, protocol 2025-06-18, with 2025-03-26 and 2024-11-05 fallbacks). Holds one project in memory with undo/redo and exposes 18 tools. `render_frame` re-bases only the visible clips to t=0, so frames render fast at any timestamp. Progress notifications during export. |
| `captions.rs` | Auto-captions. Detects an engine (FFmpeg `whisper` filter, else `whisper-cli`), downloads models, transcribes a clip or the whole timeline mix (the export graph built audio-only), parses SRT, splits into short captions and lays them out as text clips on a caption track. |
| `task.rs` | Generic cancellable background job with progress, polled by the UI. |
| `ui/` | Panels. Timeline and preview are custom-painted with egui's painter for full control over hit-testing, snapping and drag behaviour. |

## Coordinate conventions
- **Time**: seconds (`f64`) on the timeline. A clip maps timeline time `t` to source time `source_in + (t - start) * speed`.
- **Position**: `transform.x/y` are offsets of the layer centre from the canvas centre, as a fraction of canvas width/height.
- **Scale**: multiplier on the "contain" fit of the media inside the canvas.
- **Text size**: in pixels relative to a 1080 px short side; scaled to the actual output.
- **Z-order**: tracks are listed top→bottom as shown in the timeline; upper tracks draw on top.

## Keyframes
Keyframes live on the clip (`Clip::keyframes`, times relative to the clip start) and are evaluated by
`model::interpolate`. The exporter compiles the same keyframes into FFmpeg expressions
(`export::keyframe_expr`, unit-tested against `interpolate`): `overlay` x/y and `rotate` angle are
per-frame expressions; animated scale uses `scale … eval=frame` inside a fixed-size transparent `pad`
(so downstream filters see a constant frame size); opacity is driven per frame by a generated `sendcmd`
script; volume uses `volume … eval=frame`; text uses `drawtext` expressions. `render_frame` bakes the
values at the requested time.

## Transitions
A transition is stored on the incoming clip (`Clip::transition_in`) and applies only while the previous
clip on the track touches it; `Project::track_transitions` returns the active ones, each shortened to at
most half of either clip so windows never overlap. Export: the two clips' normal layers stop/start at the
window edges, and the window is rendered from two extra inputs (footage past A's out-point and before
B's in-point, frame-held with `tpad` when missing), each composited onto a transparent canvas with the
same `layer_filters` as normal layers, blended with `xfade` and overlaid. The preview decodes clips
across their transition windows and mimics each `xfade` style (shapes measured from FFmpeg output).
`render_frame` renders a few-second window cut out of the timeline so transitions are exact.

## Smart audio
`smart_audio.rs`. Auto-ducking resolves each clip's role (`Project::audio_role`), finds speech in voice
clips from their 100 Hz waveform peaks (threshold, pause bridging, blip removal) and writes volume
keyframes onto music clips (attack 0.25 s, release 0.6 s). Clips with many volume keyframes are driven by
an `asendcmd` script in the export. Noise removal is `afftdn` with `nf` set from the clip's measured
noise floor (`astats`), applied in both the export and the preview audio pipes. Loudness: the exporter
measures the mix with `ebur128`, then applies a fixed gain plus a −1.5 dBTP `alimiter`, re-measuring with
secant correction until within 0.2 LU (single-pass `loudnorm` undershoots on short or peaky mixes).

## AI (OpenRouter)
`ai/openrouter.rs` is the shared client: blocking HTTPS via `ureq` (no subprocess, so the key never
appears in a process list), key from `OPENROUTER_API_KEY` or `%APPDATA%/RustCut/ai.json`, JSON-schema
structured outputs with defensive parsing, retries on 429/5xx, per-call `usage.cost`, and the live model
list. `ai/reframe.rs` samples ≤120 small JPEG frames in one FFmpeg pass, sends them in batches of 8 (the
first batch names the subject; the rest run 4 at a time and follow it), then plans a camera path: fill
gaps → centred moving average → deadzone (holds still until the subject drifts) → Ramer–Douglas–Peucker
simplification → position keyframes + fill scale, clamped so the frame never shows background. The model
call is injected, so the batching and path logic are unit-tested without the network.

## AI video generation
`ai/video_gen.rs`: reads `/api/v1/videos/models` (durations, resolutions, aspect ratios, first/last-frame
support, pricing SKUs), estimates cost across the three pricing styles (USD/s, cents/s, video tokens),
submits `POST /api/v1/videos` exactly once (no retries: a duplicate job would be billed twice) with
`frame_images` as base64 data URLs, polls `GET /api/v1/videos/{id}`, and downloads the authenticated
content URL into `RustCut AI/` next to the project (or Videos). `place_generated` adds it to the library,
at a time, at the end, or ripple-inserted after the clip being extended.

## AI rough cut
`ai/rough_cut.rs`: whisper over the timeline mix with `max_len` (short, ≈ word-level segments) and a
240p/2 fps/mono proxy of the timeline (a few hundred KB per minute) go to a video-capable chat model as a
`video_url` data URL with a JSON schema for keep-ranges, titles, description and chapters. Ranges are
clamped, snapped to transcript boundaries (±0.35 s), merged and filtered, then applied with
`Project::keep_ranges` → `cut_range`, a ripple cut through every track (trims, splits around, shifts,
keyframe-preserving). Chapter times are remapped onto the cut timeline.

## Production economics
`economics.rs` keeps `economics.sqlite` (config folder): `spend` (every paid image/video/chat call, recorded at the
three call sites with the output file), `lineage` (shot video ← storyboard frame), `tools`/`rates`/`stacks` (dated
commercial prices, seeded, editable), `settings` (videos per month) and saved `reports`. A report walks the visible
clips: an AI file's cost (plus its parents') is split between the clips that use it by duration; each clip gets the
commercial equivalent per stack (cheapest tool in the stack per task, "—" when none) and a duration-weighted share of
the stack's subscriptions; audio clips get transcription, noise removal, beat detection and mastering lines; spend in
the same folders that no clip uses shows up as "redraws and replaced shots". Output: egui table, HTML, CSV.

## Local AI (Ollama)
`ai/local.rs`: `Decider` sends one multiple-choice prompt (task, state lines, lettered options) to Ollama's
`/api/generate` with reasoning off (`think: false`, or for raw-template decision models like tev1 an empty
`<think></think>` in a hand-built chat prompt), temperature 0, 8 output tokens; the reply must be one of the
offered letters (asked twice, then a safe default). Offline rough cut: whisper ≈ word segments → sentences →
keep/cut/trim per sentence (with neighbours as context) → keep ranges = timeline − cut sentences − filler parts −
quiet stretches from FFmpeg `silencedetect` (music and B-roll without speech stay) → the same `Plan` the cloud
rough cut produces. Smart transitions: each cut is described energy-first (style, section change, position, clips,
continuity, then beat timing) and mapped to a `Transition`.

## Music videos
`music.rs`: mono 11 kHz decode → 1024-point FFT (hop 256) → spectral flux (all bands and < 150 Hz) and RMS →
tempo from the onset autocorrelation (log-Gaussian prior at 120 BPM, double-lag bonus) → beats by dynamic
programming → downbeat phase with the most bass onsets → sections where per-bar loudness changes (snapped to
4-bar phrases). Analyses live in `Project.music` (per media id) and are mapped onto the timeline through the song
clip, feeding the ruler, snapping, `beat_montage` and `apply_beat_effect`. `multicam.rs` cross-correlates onset
envelopes with an FFT to place takes ("Cam N" tracks, `Track.angle`) and builds a `Track.multicam` edit from switch
points. `lyrics.rs` parses enhanced LRC, aligns plain lyrics to whisper words by edit-distance DP and lays out
caption clips. `reactive.rs` renders `showfreqs`/`showwaves`/`avectorscope` to transparent qtrle .mov and turns
bass/RMS envelopes into simplified (RDP) keyframes. `ai/music_video.rs` plans bar-aligned slots, allocates shots
per section kind (repeats reuse), and assembles at slot times; `best_window` picks the most energetic downbeat-
aligned window. Keyframe expressions are balanced trees (log depth) and graphs over 8 KB go to FFmpeg via
`-/filter_complex <file>`, so long beat animations export.

## AI storyboard (prompt to video)
`ai/storyboard.rs` chains the other AI modules: `plan` (chat JSON schema → `Storyboard { title, style, script,
music, shots[] }`, lengths rescaled to the target and clamped 2–10 s, saved as `storyboard.json` in its own folder
under `RustCut AI/`), `generate_missing_frames` (Image API; another shot's frame is passed as a reference for
consistency), `animate_many` (one `video_gen::generate` per approved shot on scoped threads, aggregated progress,
per-shot errors) and `assemble` (append to the main track, dissolves via `set_transition`, captions via
`captions::apply_captions`, music clip with role Music + `smart_audio::auto_duck`, `audio.loudness = −14`).
The window (`ui/storyboard.rs`) and the MCP `storyboard_*` tools are thin layers over these functions.

## AI images
`ai/image_gen.rs`: reads `/api/v1/images/models` (reference-image input, aspect ratios, resolutions,
transparent background), builds one `POST /api/v1/images` request per job (sent once, never retried) with
`input_references` as base64 data URLs, and decodes `data[].b64_json` into `RustCut AI/`. Jobs:
`Thumbnail` (composited frame via `render_frame_png`), `Still` (title card / lower third / sticker; the
latter two transparent) and `Plate` (the clip's middle frame, outpainted to the canvas ratio). A plate is
`Clip::background_plate` (a media id): the preview engine decodes it into its own slot (`plate_key`) and
draws it cover-scaled before the clip; the exporter adds a looped image input, scales/crops it to the canvas
and overlays it right before the clip, enabled over the clip's span. `Project::thumbnail` is copied next to
exports.

## Threading
No locks on the hot path: workers talk to the UI via `mpsc` channels and call
`ctx.request_repaint()` when they deliver something. The only shared state is the
export job's progress (`Arc<Mutex<_>>`) and its cancel flag (`AtomicBool`).

## Crate layout
`src/lib.rs` holds everything; `src/main.rs` (desktop app + `--export` CLI) and `src/bin/rustcut-mcp.rs`
are thin binaries over it.

## Testing
`cargo test` covers the model (split/place/overlap/duplicate/trim bounds), history,
filter math and the export graph builder.
