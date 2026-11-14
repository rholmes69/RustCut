---
name: singing-scene
description: Create a new lip-synced singing scene of a music video's lead (e.g. Aishyla singing on stage) in a RustCut project. It draws a still, turns it into a Seedance 2.0 performance shot that sings the song's real words, then measures and fixes the lip-sync timing. This is the process behind Bond Money's choir take (performance-shot-01-png-1.mp4). Use when the user wants the main character to sing specific lyrics in a new shot or scene.
---

# A lip-synced singing scene

This is the process behind Bond Money's choir take, 2:38.9–2:49.8: Aishyla on the church stage singing "You forget who stood guard… it's a war I can't win". For lip-syncing a clip that's **already on the timeline**, see `/lip-sync`.

Everything runs through `singing_scene.py` in this folder. Set these first:

```bash
S=.claude/skills/singing-scene/singing_scene.py
D="C:/Users/sales/Videos/RustCut AI/<Title> music video"
P="$D/<Title>.rcproj"
```

## Rules (always)
- **Ask before spending money.** Each paid step gets its own estimate and approval: the still, then the performance.
- **Never print API keys** (OpenRouter, fal.ai).
- **The project must be closed in RustCut** for `run` and `apply`; the script refuses while RustCut is running. Never kill RustCut: ask the user to close it.
- **Prices:**

  | Step | Cost |
  |---|---|
  | The still | ≈ $0.07 |
  | Seedance fast, per second of video | ≈ $0.24 |
  | Seedance best, per second of video | ≈ $0.30 |

  The video is the span plus 1 s: the Bond Money choir take was 11 s of song, 12 s of video, **$2.90**. A refusal costs nothing.

## 1. Choose the span (free)
- Find the lyric lines the user wants. Get their times from the project's Lyrics track (`get_project`) or from the music-video `storyboard.json` slots.
- **Start the span where a line starts.** End it where a line ends, or at a cut.
- **Keep it to 14 s or less:** one Seedance segment. Longer spans are split into overlapping segments, which costs more and adds joins.
- **Choose the track.**
  - **On top:** in Bond Money the take went on the overlay track above the main track, so the shots under it stay as a fallback.
  - **On the main track:** it then replaces the shot that's there.

## 2. Draw the still (paid ≈ $0.07)
Write a description based on `still-example-bond-money.txt`. These choices made the take work:
- **A three-quarter angle, medium shot, waist up, facing toward the camera**, with the **mouth fully visible**:
  - Put the mic on a stand below the chin, in the soft-focus foreground. Never put the mic in her hand at her mouth.
  - Leave her hands free.
- **"She has exactly the same face, hairstyle and outfit as in her reference photo",** then list them: hair, earrings, necklace and the scene's outfit, so it matches the scene around it.
- **Mouth open mid-note, emotion held back** ("eyes glossy … no tears on her cheeks"), so the performance can build.
- **Exact geography:** what's directly behind her, and what is **not** ("No pews and no congregation behind her"). Bond Money's first still put her in the aisle with the choir far away, and had to be redrawn.
- **Light and lens:** the look and grade of the video, plus "50mm, T1.8, shallow depth of field, … photorealistic skin texture".

Draw it with the music-video pipeline's `newshot` stage. It needs the video's storyboard folder in `$D`; every `/music-video` project has one. Pass the lead's reference for this scene:

```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=newshot MV_DESC="$(cat still.txt)" MV_CAST="Aishyla=$D/<her look for this scene>.png" CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture
```

The still lands in `$D/RustCut AI/new shots/<slug>/shot-01.png`; the log prints the path and the cost. Look at it and check:
- her likeness and outfit;
- where she is;
- the mic position;
- that the mouth is visible.

Show it to the user. Redraw it if any of these is wrong.

## 3. Write the performance direction (free)
Use the user's own prompt when they give one; Bond Money's is in `direction-template.txt`:
- "continuous single take, no cuts";
- the breathing;
- the mouth shape on the belted notes;
- the brow and eyes;
- the chin drop on the release;
- the background action;
- "subtle organic handheld breathing camera movement, no zoom".

Save it as `$D/direction.txt`.

## 4. Dry run (free)
```bash
python -I $S dry --project "$P" --still "<still.png>" --start <t> --end <t> --track 2 --direction-file "$D/direction.txt"
```
It prints the segments, the lyrics Seedance will be given, the video length and `estimated_usd`. It saves nothing. Check that the lyrics are the lines the user asked for, then quote the cost and get approval.

## 5. Run (paid; a few minutes, run it in the background)
```bash
python -I $S run --project "$P" --still "<still.png>" --start <t> --end <t> --track 2 --direction-file "$D/direction.txt"
```
- It backs up the project as `<Title>.before-singing.rcproj`.
- It places the still, runs the performance shot, mutes the new take and saves.
- The take file lands in `$D/RustCut AI/lipsync/performance-<still>-N.mp4`.
- It prints the clip id and a **first-guess speed** (video length ÷ span).

**If fal refuses it** ("likenesses of real people", content_policy_violation): you aren't charged and the project is unchanged.
- Don't retry the same still. Redraw it with a different angle and light: the Bond Money still was refused twice, and a **three-quarter angle** redraw passed.
- Leave `--sheet` off: a character sheet didn't help.
- **Fallback:** Kling v3.0 animates the still (`MV_STAGE=newshot MV_STILL=<still> MV_VIDEO_MODEL=kwaivgi/kling-v3.0-std MV_MOTION="<direction>" MV_SECONDS=<span>`, ≈ $0.92 for 11 s). Then the `lip_sync` MCP tool (lipsync-2, ≈ $0.05/s) fixes the mouth. This sync is weaker on held notes.

## 6. Fix the sync (free)
**Why:** Seedance renders about 1 s more video than the span and spreads the singing across all of it. Played at 1×, the mouth slowly falls behind the vocal. Bond Money needed **1.095×**, which is 12.04 s ÷ 11 s.

1. **Measure:** `python -I $S fit --project "$P" --clip <id>`.
   - It compares how much the mouth area moves and opens with how loud the vocal is, over a range of speeds and shifts.
   - It prints the score as placed now and a **recommended** speed and shift. For a fresh take, that's speed = video length ÷ span.
   - If her face isn't in the centre of the frame, pass `--mouth x,y,w,h` (fractions of the frame).
   - Scores: about 0.05 or below is out of sync, and about 0.2 is good.
   - The measure is rough. Prefer the recommended line over a slightly higher "best overall".
2. **Try it without changing anything:** `python -I $S preview --project "$P" --clip <id> --speed <x> --shift <d> --out "$D/<scene> (with song).mp4"`. Open it for the user.
3. **Apply:** `python -I $S apply --project "$P" --clip <id> --speed <x> --shift <d> --close-gap`.
   - It backs up the project as `<Title>.before-sync.rcproj`.
   - It sets the speed, moves the start (a positive shift starts the take later) and keeps the end.
   - It keeps the take muted.
   - On the same track, it shortens the clip before or stretches it to meet the take, so there's no gap.
4. Run `fit` again. The recommended shift should now be about 0.
5. **The user's ears decide.** If a line still looks early or late, nudge the shift in 0.05 s steps.

## 7. Check and report
- **Mouth frames:** `ffmpeg -i <take.mp4> -vf "fps=2,crop=iw/2.4:ih/2:iw*0.29:0,scale=200:-2,tile=11x2:padding=2" -frames:v 1 mouth.jpg`. Look for the vowels being formed, natural teeth, no cuts and no subtitles.
- **Report:**
  - the preview with the song;
  - the clip's place and speed;
  - the backups;
  - the cost: stills, plus the performance, plus any fallback.
- Bond Money's scene cost **about $4.57** in total. That included one wasted still and both takes (A and B).

## Gotchas
- **Last Night's stills were refused twice** (shot 15 and a fresh three-quarter redraw). Its lead was drawn from real photos and looks very like them, so expect a refusal when the cast reference is a real photo. Offer Kling + lipsync-2 after one refusal on such a project. That piano take cost $0.07 for the still, $1.01 for Kling (12 s) and $0.60 for lipsync-2. To do it: import the Kling video, `add_clip` it on the span, set volume 0, call `lip_sync {video_clip_id, voice_clip_id: <song clip>}`, then `save_project` to a copy for review.
- **For the animation before lipsync-2, test the models first and keep the face steady.**
  - **What went wrong first:** Kling v3.0 Std with a big emotional prompt (throwing her head back, turning to the lens) morphed Aishyla's face. The tear in the still plus "heartfelt… eyes squeeze shut" made her look like she was crying, which the user disliked.
  - **What fixed it:** a calm, confident still (mouth open on a soft note, "no tears, dry cheeks", seated *at* the keyboard with her hands on the keys) and a steady prompt (`Videos\RustCut AI\Last Night music video\direction-steady.txt`: head mostly still, "exactly the same face throughout", soft half-lidded eyes).
  - **The test:** 5 s of each model ($1.64 for three), compared on face crops (`fps=2,crop=…,tile=10x1`, one row per model).

    | Model | Result |
    |---|---|
    | Hailuo 3 | The steadiest face, the least movement |
    | Wan 3.0 | Steady face, the most singing, a strong push-in. **The user's pick** ($1.02 for 12 s) |
    | Kling v3.0 Pro | Drifted into a wide grin |

  - **Last Night's final piano take** cost $1.62 (Wan plus lipsync-2). The whole scene, including the false starts, cost $5.15.
- **A lipsync-2 take needs no speed fix.** Its output is as long as the span, and the mouth is driven by the song. `fit` is for Seedance takes, and only works when the face stays fairly still. On the Last Night Kling take, the head tilted and turned and the scores were noise. Judge by the mouth frames and the preview instead.
- **Moving the take later breaks the sync.** If someone drags it in RustCut, run `fit` again. A big recommended shift (over 0.5 s) means it was moved.
- `run` and `apply` drive `target/release/rustcut-mcp.exe`. If it was rebuilt while locked, use the current one; the old copies are named `rustcut-mcp-old*.exe`.
