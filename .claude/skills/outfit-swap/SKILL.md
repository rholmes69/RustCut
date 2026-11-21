---
name: outfit-swap
description: Reshoot a music-video storyboard shot in RustCut with a character wearing a different outfit (e.g. "reshoot shot 7 but switch Aishyla's outfit to the white-tank character sheet") — redraw the still from the shot's own description with the new outfit sheet as that character's reference, animate it, swap it into every cut of that shot, keep the storyboard in sync, and flag other shots in the same scene that no longer match. Use when the user wants a character's wardrobe changed in a shot, or a scene made consistent with a new outfit.
---

# Reshoot a shot in a different outfit

Proven on **Falling Deeper** (Aidan B., Aishyla): shots 7 (penthouse) and 13 (rooftop sax and convertible dance) switched to Aishyla's white-tank sheet, 20¢ for shot 7 and about 30¢ for shot 13. Both cuts of each shot were swapped, and their transitions were kept.

Set these first:
```bash
D="C:/Users/sales/Videos/RustCut AI/<Title> music video"     # project folder
P="$D/<Title>.rcproj"
SB="$D/music-video-<slug>/storyboard.json"
C="C:/Users/sales/Videos/RustCut AI/Movie Brand Kit/Assets/Characters"   # outfit sheets live here
```

## Rules (always)
- **Ask before spending.** A redrawn still is about 7¢. Animation is about 3¢ a second with grok-imagine lite (4 s ≈ 13¢, 8 s ≈ 25¢). A new outfit sheet is about 7¢. Give the estimate, and always show the redrawn still before animating.
- **Never print API keys.** Prefix paid runs with `env -u OPENROUTER_API_KEY`.
- Drawing and animating don't touch the `.rcproj`; **only the swap (step 6) does**, so RustCut must be closed then (`tasklist | grep -i rustcut.exe`). Never kill it, ask the user to close it.
- Don't name real celebrities. When the user's outfit references are photos of real people, borrow only the clothes and keep the character's own face sheet as the reference.
- Never write a dollar sign directly before a digit in this file: skill arguments replace dollar-number placeholders. Write prices in cents (7¢) or as "1.30 USD".

## 1. Find the shot and its cuts (free)
- `shot-07-video.mp4` = storyboard shot 7 (index 6). Read its `description`, `motion` and `seconds`:
  `node -e 'const s=JSON.parse(require("fs").readFileSync(process.argv[1]));const x=s.shots[6];console.log(x.description,"\n",x.motion,"\n",x.seconds)' "$SB"`
- Find every cut using it. The project may list the same file under several media ids, so match by path:
  `node -e 'const p=JSON.parse(require("fs").readFileSync(process.argv[1]));const ids=p.media.filter(m=>/shot-07-video/.test(m.path)).map(m=>m.id);for(const c of p.tracks.flatMap(t=>t.clips))if(c.source&&ids.includes(c.source.Media))console.log(c.id,c.start.toFixed(2),c.duration.toFixed(2),JSON.stringify(c.transition_in||""))' "$P"`
- If the shot's video was already replaced (by a reshoot or a face fix), the cuts use that file instead. Look up `shots[i].video` and match that path.

## 2. Get the outfit sheet
- Use an existing sheet from `$C` (for example `aishyla-white-tank-character-sheet.png`).
- **No sheet yet (about 7¢, ask first):** make one that keeps the character's face:
  ```bash
  cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$C" MV_PROJECT=Characters MV_STAGE=sheet MV_SHEET_NAME="Aishyla white tank" MV_SHEET_REF="<her face sheet>" MV_SHEET_PROMPT="a young Black woman with warm deep-brown skin, a sleek high ponytail and gold hoop earrings, wearing <the outfit>" CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture 2>&1 | grep "^sheet:"
  ```
  For an animal or a prop, use `MV_SHEET_FULL_PROMPT` (it replaces the person template).

## 3. Redraw the still (paid, about 7¢)
- **`MV_CAST` replaces the whole cast for this run.** List everyone in the shot: the outfit sheet for the changed character, and the usual sheets for the rest.
- **Keep the shot's own description word for word**, then add the outfit as a sentence: "Aishyla wears a fitted white tank top and frayed mid-wash denim cut-off shorts, gold hoop earrings and a sleek high ponytail; her face is clearly visible." The sheet alone isn't enough. Without the sentence, the old outfit in the description wins.
- Keep the motion, and add "<Name>'s <outfit> and both faces stay exactly the same throughout."
```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=redraw MV_REDRAW="7" \
MV_CAST="Aidan=$D/aidan-character-sheet.png;Aishyla=$C/aishyla-white-tank-character-sheet.png" \
MV_DESC_7="<original description> Aishyla wears … No readable text or logos." MV_MOTION_7="<original motion> … stay exactly the same throughout." \
CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture 2>&1 | grep "shot 7 →\|cost"
```
The still is saved as `shot-07-2.png` (then `-3`, …), and redraw clears the shot's `video` in the storyboard (step 6 sets it again). Make an old-and-new comparison and show the user:
`ffmpeg -i shot-07.png -i shot-07-2.png -filter_complex "[0]scale=800:-2[a];[1]scale=800:-2[b];[a][b]hstack" -frames:v 1 ../shot-07-compare.jpg`

## 4. Animate (paid, about 3¢ a second)
```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=newshot MV_SECONDS=<shot seconds, rounded> \
MV_STILL="$D/music-video-<slug>/shot-07-2.png" MV_CAST="<same as step 3>" MV_DESC="<one-line version>" MV_MOTION="<motion from step 3>" \
CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture 2>&1 | grep "folder\|estimate\|video:"
```
The video lands in `$D/RustCut AI/new shots/<slug>/shot-01-video.mp4`.

## 5. Check (free)
`ffmpeg -i <video> -vf "fps=1.5,scale=-2:280,tile=3x2:padding=3" -frames:v 1 check.jpg`
- **The outfit stays in every frame:** it doesn't drift back to the old one.
- **The faces match their sheets.** A face that is small or in profile is where drift starts; if it does, see `/face-fix`.

## 6. Swap it in (free, RustCut closed)
1. Back up the project and the storyboard (`.before-<what>.rcproj`, `storyboard.before-<what>.json`).
2. MCP: `open_project`, then `import_media {paths: [video]}`, then `replace_media {clip_id: <any cut of the shot>, with_media_id}`. The default scope is all, so every cut of the shot changes and keeps its place, length and transition. Then `render_frame` in the middle of one cut, and `save_project`.
3. Point the storyboard at the new video, so a later `produce` or b-roll re-cut reuses it:
   `node -e 'const fs=require("fs");const f=process.argv[1];const s=JSON.parse(fs.readFileSync(f));s.shots[6].video=process.argv[2];fs.writeFileSync(f,JSON.stringify(s,null,2))' "$SB" "<video path>"`
4. Check that no top-level project keys were lost against the backup, then `export_video` the 720p preview.

## 7. Report and flag continuity
Report:
- which cuts changed, with their times and whether their transitions were kept;
- the frame check;
- the cost;
- the backups.

**Check the rest of the scene:** list the other shots in the same place or act (same setting words in their descriptions, or neighbouring slots). Name the ones that still show the old outfit, and offer to reshoot them the same way, with a price. Falling Deeper's penthouse scene was shots 7, 8 and 9: 8 shows only hands, and 9 still had the old outfit.
