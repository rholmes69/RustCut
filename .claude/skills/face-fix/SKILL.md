---
name: face-fix
description: Fix a shot where the music video's main character (e.g. Aishyla) doesn't look like herself — diagnose why the face drifted (face too small in the still, wrong still, video model drift, name missing so no reference photo), redraw the still with her face large and described feature by feature, animate it with a face-safe motion, check it against her reference sheet, and swap it into every cut that used the old video. Use when the user says a shot's face is wrong, e.g. "shot-09-video.mp4 the woman's face doesn't look like Aishyla", "that's not her", "her face changes".
---

# Fix the main character's face in a shot

Proven on **Thirsty for U** shot 9: an overhead pool shot where Aishyla's face was tiny in the still, so the video model invented a different woman. One redraw (7¢) and one animation (19¢) fixed all three cuts. The transitions stayed.

Set these first:
```bash
D="C:/Users/sales/Videos/RustCut AI/<Title>"          # the project folder (the one holding the .rcproj)
P="$D/<Title>.rcproj"
SBD="$D/music-video-<slug>"                           # storyboard folder: storyboard.json, shot-NN.png, shot-NN-video.mp4
REF="$D/<lead reference photo or character sheet>"     # e.g. "aishyla-sheet (2).png"
```

## Rules (always)
- **Ask before spending.** Each redrawn still costs about 7¢. Animating costs about 13¢ for 4 s and 19¢ for 6 s (grok-imagine lite). Show the redrawn still and get a yes before animating.
- **Never print API keys.** Prefix paid runs with `env -u OPENROUTER_API_KEY`.
- The redraw and newshot stages don't touch the `.rcproj`. Only the swap in step 6 does, so **RustCut must be closed** for that step (`tasklist | grep -i rustcut.exe`). Never kill it: ask the user to close it.
- Don't name real celebrities in prompts.

## 1. Find the shot (free)
- From a file name like `shot-09-video.mp4`, the storyboard shot is number 9 (index 8). Read its `description`, `motion`, `frame` and `video`:
  `node -e 'const s=JSON.parse(require("fs").readFileSync(process.argv[1]));const x=s.shots[8];console.log(x)' "$SBD/storyboard.json"`
- Find every cut that uses that video. Use `open_project` (MCP), then search the media list for the file and the clips for its media id. Or use `python -I .claude/skills/broll/broll.py list "$SBD/storyboard.json"` (look for the slots with shot 9). Note their times for the report.

## 2. Diagnose (free): put the still, the video and the reference side by side
```bash
cd "$D" && ffmpeg -hide_banner -loglevel error -y -i "$SBD/shot-09.png" -i "$REF" -filter_complex "[0]scale=-2:400[a];[1]scale=-2:400[r];[a][r]hstack" face-still-vs-ref.jpg
ffmpeg -hide_banner -loglevel error -y -i "$SBD/shot-09-video.mp4" -vf "fps=1,scale=-2:240,tile=3x2:padding=3" -frames:v 1 face-video.jpg
```
Look at both (Read the jpgs) and decide which case it is:

| What you see | Cause | Fix |
|---|---|---|
| The face in the still is small: overhead, wide, far away, or under ~10% of the frame height | The image and video models have too few pixels to keep her likeness, so they invent a face | **Redraw closer** (step 3) |
| The still is already a different woman | The description doesn't name her, or the shot is `no_lead`, so no reference photo was sent | **Redraw** with her exact cast name and `no_lead` false |
| The still is right, but the face changes partway through the video | The motion has her turn away, cover her face, get lit hard, or cut. Big camera moves also cause it | **Keep the still and re-animate** with a face-safe motion (step 4) |
| Face right but hair, outfit or skin tone wrong | The description contradicts the sheet | Redraw, copying the look from `theme.txt` and the sheet |

Tell the user what you found in one or two lines, and give the estimate.

## 3. Redraw the still (paid, about 7¢)
Rules for the new description:
- **Name the lead exactly as in the cast** ("Aishyla"), so her photo goes to the image model.
- **Make her face large.** Start the description with the framing: "Tight close-up portrait from the chest up: Aishyla's face fills about a third of the frame height, facing the camera". Keep the original idea (overhead pool → a *close* overhead with her face filling a large part of the frame).
- **Leave out everything below the waist.** If the description mentions shoes, legs, walking or water at her ankles, the image model draws her full-body again and ignores "waist up". Thirsty for U shot 3's first redraw did exactly that. Move other people and the set into a blurred background.
- **List her identifying features** from the reference sheet: skin tone, brows, eye makeup, lips, hairline and baby hairs, earrings, and the hairstyle in this act. Add "clearly recognisable".
- Keep the shot's setting, light and palette, and end with "No readable text or logos."

```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=redraw MV_REDRAW="9" \
MV_DESC_9="Close overhead shot … Aishyla floats calmly on her back …, framed from the waist up so her face fills a large part of the frame and is clearly recognisable: deep warm brown skin, defined brows, sharp winged liner, full lips, sculpted baby hairs, gold hoop earrings … No readable text or logos." \
MV_MOTION_9="A slow, steady push-in toward her face … One continuous shot, no cuts." \
CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture 2>&1 | grep "shot 9 →\|cost"
```
The new still is saved as `shot-09-2.png` (then `-3` and so on), and the old ones are kept. **Redraw clears the shot's `video` in the storyboard.** Step 6 sets it again.

Compare the new still with the reference (`hstack`, as in step 2) and show it to the user. If the face is still off, redraw once more with the face larger or more frontal. Then ask whether to animate.

## 4. Animate (paid, about 13–19¢)
Animate the approved still on its own with the `newshot` stage. It writes to `$D/RustCut AI/new shots/<slug>/shot-01-video.mp4` and doesn't change the project or the storyboard:
```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=newshot MV_SECONDS=6 \
MV_STILL="$SBD/shot-09-2.png" \
MV_DESC="Aishyla floats calmly on her back in … (a one-line version of the still)" \
MV_MOTION="A slow, steady push-in toward her face as … ; near the end she slowly opens her eyes. Her face, skin tone and features stay exactly the same throughout. One continuous shot, no cuts." \
CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture 2>&1 | grep "estimate\|video:\|panicked\|error"
```
**Face-safe motion:**
- Use a gentle push-in, a slow drift or a slow orbit of no more than about 30°.
- Avoid head turns away from the camera, hands or hair across the face, spins, whip pans, splashes and "transforms into".
- Always add "Her face, skin tone and features stay exactly the same throughout. One continuous shot, no cuts."
- Set `MV_SECONDS` to the longest cut that uses the shot, rounded up (6 covers most music-video cuts).

## 5. Check (free)
```bash
ffmpeg -hide_banner -loglevel error -y -i "$D/RustCut AI/new shots/<slug>/shot-01-video.mp4" -vf "fps=1,scale=-2:240,tile=3x2:padding=3" -frames:v 1 "$D/face-new-video.jpg"
```
Check every frame against the reference: the same face from the first frame to the last, no morphing, the outfit and setting unchanged. If it drifts late in the clip, re-animate with less motion or fewer seconds (paid again, so ask), or trim the cut to the good part.

## 6. Swap it in (free, RustCut closed)
1. Back up: `cp "$P" "$D/<Title>.before-facefix.rcproj"` and `cp "$SBD/storyboard.json" "$SBD/storyboard.before-facefix.json"`.
2. MCP: `open_project {path: P}`, then `import_media` the new video, then `replace_media {clip_id: <any cut using the old video>, with_media_id: <new media id>}` (default `scope: all`). That swaps it into **every cut** that used the old video and keeps each cut's place, length, speed, transitions and volume. Run `render_frame` at the middle of each cut and look at the face. Then run `save_project`.
3. **Point the storyboard at the new video**, so a later `produce` or b-roll re-cut reuses it instead of paying to re-animate shot 9:
   ```bash
   node -e 'const fs=require("fs");const f=process.argv[1];const s=JSON.parse(fs.readFileSync(f));s.shots[8].video=process.argv[2];fs.writeFileSync(f,JSON.stringify(s,null,2))' "$SBD/storyboard.json" "$D/RustCut AI/new shots/<slug>/shot-01-video.mp4"
   ```
4. Re-export the preview (`export_video` to `$D/<Title> - music video (preview 720p).mp4`). Sample a frame from each cut of the export to confirm.

## 7. Report
Cover:
- the cause you found;
- what changed in the shot;
- which cuts were swapped, with their times;
- the frame check;
- the cost (still plus animation);
- the backups.

Flag any back-to-back cuts that now replay the same video from its start, and offer to start the second cut later in the clip (`source_in`), which is free.

If the shot is lip-synced, the new video has no sync. Offer `/lip-sync`, with a dry run first.

**Deliver:** after a final re-export, offer the user a Google Drive backup (video and project zip), a YouTube upload with the details filled in from the project, and a release plan on Google Calendar: see `/music-video` step 13. Ask before each; they act on the user's real accounts.

**Fixing several shots:** do steps 2 and 3 for all of them in one redraw run (`MV_REDRAW="4,9,12"` with a `MV_DESC_N` for each), show one comparison sheet, then animate each one.
