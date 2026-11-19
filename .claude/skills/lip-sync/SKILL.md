---
name: lip-sync
description: Make characters in a RustCut timeline lip-sync to a song or dialogue with fal.ai, choosing between a Seedance 2.0 performance shot (best for singing) and Sync lipsync-2 mouth fixing (cheap, for dialogue), with dry-run cost estimates and a result check. Use when the user asks for lip-sync, a singing performance shot, or a character speaking a line.
---

# Lip-sync in RustCut

**For a new singing scene of the lead** (draw a still, make a performance shot, fix the sync timing), use `/singing-scene`. Its `fit` and `apply` steps also fix the timing of any performance shot made here: Seedance takes play about 9% slow at 1× speed.

## Rules
- **Ask before spending money.** Always do a dry run or estimate first and quote the price.
- **Never print the fal.ai key.** RustCut reads it from `%APPDATA%\RustCut\ai.json` (`fal_key`) or `FAL_KEY`. If it seems missing from here, see the AppData note in `/music-video` (Claude's package shadow copy), and check with `grep -c '"fal_key": "'`, never by printing it.
- Warn the user before changing a project that's open in RustCut.

## Pick the method
| Method | Tool | Best for | Price |
|---|---|---|---|
| **Performance shot** (Seedance 2.0 via fal.ai) | `performance_shot` | Singing. Re-generates the shot from a still with the song audio and lyrics | Fast ≈ $0.24/s, best ≈ $0.30/s, of generated video |
| **Fix the mouth** (Sync lipsync-2) | `lip_sync` | Dialogue over existing footage | ≈ $0.05/s (pro ≈ $0.083/s) |
| **Animate + sync** (Wan/Hailuo/Kling, then lipsync-2-pro) | `lip_sync_still` | A still of the singer; likenesses Seedance refuses; a calm, clean mouth | ≈ $0.06/s of video + $0.083/s of sync (≈ $1.66 for 10.6 s) |

**When Seedance refuses** (`performance_shot` now returns `{refused: true, charged: false}` instead of an error), switch to `lip_sync_still {method: "animate_sync"}`. It uses a steady-face prompt (soft lips, no tears, face to camera, push-in that stops at chest height): on Bond Money's choir redo, a wide-open crying mouth from a big emotional prompt came out with jagged teeth after lipsync-2; this template plus the pro model was clean.

## Lip-sync Studio tools (the MCP server built after 2026-11-18)
- **`lip_sync`** now takes `quality: "high"` (1080p; for close-ups), `start`/`end` (trim to one or two lyric lines), `place: "above"` (keep the original underneath), `face_box` (sync only that person), and `dry_run: true` (span + estimate, free).
- **`detect_faces {clip_id}`** (≈ $0.01): every face per frame, plus a size warning. Pass one box as `face_box` when two people are in the shot; the face is cropped, synced and blended back.
- **`lip_sync_still {image, audio_clip_id | audio_path | text + voice, start, end | seconds, method, video_model?, direction?, dry_run}`**: the whole flow from a still. A `text` line is reused if it was made before (no second charge). Give `end` when the audio is the whole song.
- **`lipsync_takes {clip_id, use_media_id?}`**: every lip-sync records the original and each result as takes; swap back without undo.
- Always dry-run first and quote `estimated_usd` and its `parts`.

## Phase 2 (the MCP server built after 2026-11-18, branch lipsync-studio-phase2)
- **Any speed:** clips at 0.888× (a beat fix) or 2× sync without resetting the speed; the span is rendered at its timeline speed and the result placed at 1×.
- **Stills:** `lip_sync` on an image clip is a cheap talking photo (lipsync-2 on the held still, ≈ $0.05/s). The mouth moves but nothing else does; for a living scene use `lip_sync_still` (Animate + sync).
- **Voice longer than the shot:** the dry run returns `voice_past_shot {seconds, seam, suggested_fill}` and `next_shot_in_the_way`. Pass `fill: "loop" | "pingpong" | "hold"` (ping-pong when the seam is over 0.08, so a loop wouldn't jump) and `place: "above"` when the next shot is in the way. Loops suit swaying or waves, not walking.
- **`lipsync_check {clip_id, face_box, voice_clip_id?}`** (free): a rough sync score (Bond Money choir: clean pro take 0.21, unsynced take 0.18, the rejected jagged take 0.14), the mouth's lag, and a mouth contact sheet PNG. **The sheet is the real check**: open it and look for jagged teeth, smeared "oh" shapes and tears. Without `face_box` the numbers are noise (`reliable: false`) and `apply_nudge` refuses. A push-in moves the face out of a fixed box in later frames.

**Choose good shots:** close-ups and medium shots facing the camera, where the singer is clearly the subject. On a wide shot the face is small, so the sync barely reads (seen on Bond Money shot 16). In storyboards, prefer shots described as close-ups or performance shots.

## Performance shot (the "method 1" workflow)
How it works: the clip's still, plus the song audio for that span, plus the lyrics on the timeline for that span, go to Seedance 2.0. Spans over 14 s are split into segments that overlap by 1 s, cut at lyric line starts. Each video is audio length + 1 s, the prompt asks for a single continuous shot with no subtitles, and the results replace the clip back to back, muted so the song plays.

1. Find the clip id (`get_project`). The song must be on an audio track under it, and lyric text clips help accuracy.
2. **Dry run:** `performance_shot {clip_id, dry_run: true, model: "fast"}` returns the segments, their lyrics and the estimated cost. Show it to the user and get approval.
3. **Run:** `performance_shot {clip_id, model: "fast"|"best", direction: "belts the hook passionately in the rain", character_sheet: "<path to the singer's reference photo or sheet>"}`. It takes a few minutes per segment.
4. Save with `save_project`.

**Fallback when the MCP server is old or unavailable** (the paid test harness; it backs up the project to `<name>.before-performance.rcproj`):

```bash
cd /c/Users/sales/MyVideoEditor && PERF_PROJECT="<path>.rcproj" PERF_CLIP=<id> [PERF_DRY=1] PERF_MODEL=fast PERF_SHEET="<photo>" PERF_DIRECTION="…" CARGO_INCREMENTAL=0 cargo test --offline --lib performance_live -- --ignored --nocapture
```

The project must be closed in RustCut, or the user must reload it afterwards without saving over it.

## Fix the mouth (dialogue)
1. Put the line on an audio track over the shot as a Voice clip. `speak_line` can generate it with ElevenLabs in a cloned voice (Aishyla and Aidan B. exist; only for voices the user has rights to).
2. `lip_sync {video_clip_id, voice_clip_id?, model: "lipsync-2"|"lipsync-2-pro"}`. Only the span under the line (plus 0.15 s) is replaced. The clip's speed must be 1×.

## Check the result
- Frames: `ffmpeg -i <result.mp4> -vf "fps=2,scale=480:-2,tile=5x2:padding=3" -frames:v 1 check.jpg`. Look for an open or closed mouth on the sung or spoken syllables, an unchanged face, outfit and setting, and no cuts or subtitles.
- A preview with the song, for the user to judge by ear:
  `ffmpeg -ss <start> -t <len> -i <song.mp3> -i <result.mp4> -map 1:v -map 0:a -c:v copy -c:a aac -shortest "<name> (with song).mp4"`, then send it to them.
- Report the cost. Runs are recorded in the cost report as "lipsync".

## In the app
For the user doing it by hand: right-click a clip → **👄 Lip-sync Studio…** (or AI → 👄 Lip-sync Studio…):
1. **Shot & face:** the clip, or bring in a video file or a still; **Detect faces** or **Mark face manually**.
2. **Voice:** a clip on the timeline (with a lyric-line range), an audio file, or text to speech (a line made before is reused free).
3. **Generate:** the recommended method is starred; choose quality, Replace or On the track above, see the cost breakdown, then Generate. Afterwards, the **Takes** list swaps between the original and each result.

The mini timeline shows the shot, the chosen face and the waveform with lyrics; drag the gold handles to trim the span. The older **👄 Lip-sync to dialogue (AI)…** item still works.
