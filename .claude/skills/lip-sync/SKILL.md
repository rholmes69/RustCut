---
name: lip-sync
description: Make characters in a RustCut timeline lip-sync to a song or dialogue with fal.ai, choosing between a Seedance 2.0 performance shot (best for singing) and Sync lipsync-2 mouth fixing (cheap, for dialogue), with dry-run cost estimates and a result check. Use when the user asks for lip-sync, a singing performance shot, or a character speaking a line.
---

# Lip-sync in RustCut

## Rules
- **Ask before spending money.** Always do a dry run or estimate first and quote the price.
- **Never print the fal.ai key.** RustCut reads it from `%APPDATA%\RustCut\ai.json` (`fal_key`) or `FAL_KEY`. If it seems missing from here, see the AppData note in `/music-video` (Claude's package shadow copy), and check with `grep -c '"fal_key": "'`, never by printing it.
- Warn the user before changing a project that's open in RustCut.

## Pick the method
| Method | Tool | Best for | Price |
|---|---|---|---|
| **Performance shot** (Seedance 2.0 via fal.ai) | `performance_shot` | Singing. Re-generates the shot from a still with the song audio and lyrics | Fast ≈ $0.24/s, best ≈ $0.30/s, of generated video |
| **Fix the mouth** (Sync lipsync-2) | `lip_sync` | Dialogue over existing footage | ≈ $0.05/s (pro ≈ $0.083/s) |

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
For the user doing it by hand: right-click a clip → **👄 Lip-sync to dialogue (AI)…** → choose **Method** (Performance shot / Fix the mouth) → check the segments and cost → click the button.
