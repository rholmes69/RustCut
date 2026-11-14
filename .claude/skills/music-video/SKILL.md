---
name: music-video
description: Make a complete AI music video in RustCut from a song, lyrics and a cast (character sheets), with beat-synced cuts, synced lyrics, cost estimates and a review step before animation. Use when the user asks for a music video for an artist or song.
---

# Make a music video in RustCut

Proven on "Checklist", "Under The Influence", "Bond Money" and "Last Night" (about $3.70–5.20 each). Last Night's full making-of, including a `theme.txt` to copy, is in `Videos\RustCut AI\Last Night music video\WORKFLOW - making of Last Night.md`.

## Rules (always)
- **Ask before spending money.** Give an estimate first, and stop after the stills so the user can review them before animating.
- **Never print API keys** (OpenRouter, ElevenLabs, fal.ai). Check presence or length only.
- **Don't kill a running RustCut or SonicLens.** If the project is open in RustCut, warn the user: the pipeline rewrites the `.rcproj`, so they should close it (or not save over it) and reopen it afterwards.
- Disk space is tight: build with `CARGO_INCREMENTAL=0`.
- The shell may have a stale `OPENROUTER_API_KEY`: prefix paid runs with `env -u OPENROUTER_API_KEY`.
- Don't name real celebrities in prompts (e.g. "dresses like Snoop Dogg"). Describe the style generically and tell the user why.

## Prices (OpenRouter, measured)
| Step | Cost |
|---|---|
| Character sheet (one image) | ≈ $0.07 |
| Plan (LLM) | ≈ $0.02 |
| Each still | ≈ $0.067 |
| Each shot animated (grok-imagine-video lite, ~4 s) | ≈ $0.135 |

Estimate = sheets + $0.02 + shots × $0.067, then shots × $0.135 for animation. Default to 22 shots for a ~5-minute song and 16–18 for ~3–4 minutes.

## Two ways to run it
- **MCP tools** (when the `rustcut` MCP server is connected and current): `new_project`, `import_media`, `add_clip`, `detect_beats`, `add_lyrics`, `music_video_plan` (with `cast`), `storyboard_update_shot`, `storyboard_produce`, `export_video`, `save_project`. This is the same path SonicLens Director (`C:\Users\sales\MyMusicDirector`) uses.
- **Pipeline test** (always works, used for all three videos so far). Run from `C:\Users\sales\MyVideoEditor`:

```bash
cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=<stage> … CARGO_INCREMENTAL=0 cargo test --offline --lib pipeline_music_video -- --ignored --nocapture
```

with `D="C:/Users/sales/Videos/RustCut AI/<Title> music video"`.

## Steps (pipeline test)
1. **Set up `$D`.** Copy in the song (usually in `C:\Users\sales\Downloads`), the lead's reference photo (Aishyla's is `Videos\RustCut AI\Checklist music video\aishyla-reference.png`) and any character sheets the user provides.
   - **Clean the reference photos:** crop social-media screenshots down to just the person, with no phone frame, profile card or buttons, or the stills copy that UI.
   - **Several looks of one person:** put them side by side in one image (`ffmpeg … hstack`) and use it as that person's single cast entry.
2. **Write `lyrics.txt`.** Keep the `[Section]` headers and the sung lines only. Remove markdown (`###`, `*`), stage directions like `*(Music swells…)*` and performance notes like `(Whispered)`.
   - **Check which version the recording sings.** Transcribe it for free with FFmpeg's whisper filter (model `%LOCALAPPDATA%\RustCut\models\ggml-tiny.bin`; the command is in the Last Night workflow) and compare. Last Night had two lyric versions with different points of view, and the remix ends before the longer version's outro.
   - **Cast from who sings to whom:** the lyrics' point of view decides the casting, even where the treatment says otherwise.
   - **Missing or cut-off lyrics:** if the user's paste has no lyrics or stops mid-line, say so and ask before using the transcript.
3. **prepare (free):** `MV_STAGE=prepare MV_SONG=<mp3> MV_LYRICS="$D/lyrics.txt" MV_ASPECT=16:9 MV_LYRIC_STYLE=lines`. Report the song length, BPM, sections and how many lyric lines were aligned. Keep the lyrics' `[Section]` headers: prepare replaces the loudness-guessed sections with them (it prints `section … verse/chorus/bridge…` lines). Without headers, a song that is loud throughout gets labelled almost entirely "chorus" and the story cycles out of order.
4. **Character sheets for new characters (paid, ask first):** `MV_STAGE=sheet MV_SHEET_NAME=<Name> MV_SHEET_PROMPT="<age, ethnicity, hair, build, outfit>"` writes `$D/<name>-character-sheet.png`. Show it to the user.
5. **Write `theme.txt`:** the concept, the look (e.g. night scenes cold and fluorescent, flashbacks warm and golden), the characters, and the story mapped to the song's sections. Name cast members exactly as in `MV_CAST` in the scenes they appear in, because a shot only gets a person's photo when its description names them. Spell out the emotional beats the user asked for.
   - **Give every cast member a wardrobe by scene,** the male lead included. Otherwise his character sheet's own clothes leak in: Aidan's denim jacket showed up in 5 of the Last Night shots.
   - **Keep one outfit per continuous stretch of story:** don't change clothes in the middle of a single night.
6. **Estimate and get approval** for the sheets, plan and stills.
7. **Choose the shot mode.** For a narrative idea or a timed treatment, **always** use `MV_MODE=story` (forgetting it on Last Night gave a first cut with repeated, out-of-order shots): story shots run in song order, each over its own stretch, with about a quarter of the shots as recurring performance setups. The default (`sections`) reuses a section's shots in its repeats (2nd chorus…), which suits performance videos but makes a story go backwards. Preview the cut plan for free with `MV_STAGE=outline MV_SHOTS=<n> MV_MODE=story` (it prints the shot list the text model will get).
   **plan (paid):** `MV_STAGE=plan MV_SHOTS=<n> MV_MODE=<story|sections> MV_CAST="Aishyla=$D/aishyla-reference.png;Name=$D/name-character-sheet.png" MV_THEME="$(cat "$D/theme.txt")"`. This also draws every still.
8. **Contact sheet:**
   ```bash
   cd "$D/music-video-<slug>" && ffmpeg -hide_banner -loglevel error -y -start_number 1 -i shot-%02d.png -vf "scale=480:-2,drawtext=fontfile='C\:/Windows/Fonts/arialbd.ttf':text='%{eif\:n+1\:d}':x=10:y=10:fontsize=36:fontcolor=yellow:box=1:boxcolor=black@0.6,tile=4x6:padding=4" -frames:v 1 ../preview-sheet.jpg
   ```
   Look at it yourself and list the problems: a character's look leaking into another's shot, garbled signs, shots that don't fit the story. Send it to the user and ask which shots to redraw, then whether to animate (≈ shots × $0.135).
9. **redraw (paid, ≈ $0.07 per shot):** `MV_STAGE=redraw MV_REDRAW="12,18" MV_DESC_12="…" MV_MOTION_12="…" [MV_NO_LEAD="3"]`. To keep a character's look out of another person's shot, leave their name out of the description. Show the redrawn stills.
   - **Wrong clothes:** name the outfit in full and add "(no denim)" or whatever leaked in.
   - **The lead doesn't look like herself** (wide or side-on shots): redraw as a medium three-quarter shot facing the camera, with the face lit, and list her signature features. Compare the new still with the reference side by side.
   - Pass the same `MV_CAST` to `redraw` as to `plan`.
10. **produce (paid):** `MV_STAGE=produce`, run in the background (about 2–10 minutes). It animates the shots, assembles them on the beat with the lyrics, and exports `$D/<Title> - music video (preview 720p).mp4`.
11. **Check and report:** sample frames (`fps=1/20,tile=4x4`), confirm the video is as long as the song, send the video, and give the total cost with a breakdown. Suggest next steps: `/broll` for treatment-based cutaways (about $0.20 a shot),
`/lip-sync` on close-up singing shots, `/reshoot` for weak cuts, and a 1080p export from RustCut.
12. **Hand off:** if the user already has a project file elsewhere (e.g. `Downloads\<Title>.rcproj`), back it up as `<Title>.before-storyboard.rcproj`, then copy the finished `.rcproj` over it. The project must be closed in RustCut while you do this.

## Reference shots (tagged images)
For a shot that needs specific references beyond the cast (an animal, a prop, a location), use Seedance 2.0 with tags instead of `newshot`: `@ImageN` is the N-th image given, max 9. Dry run first (free), then ask before spending (≈ $0.24/s fast, $0.30/s best):

```bash
cd /c/Users/sales/MyVideoEditor && REF_IMAGES="a.png;b.png;dog.png;place.jpg" REF_PROMPT_FILE=prompt.txt REF_SECONDS=5 REF_MODEL=fast REF_OUT="$D/RustCut AI/reference shots" [REF_DRY=1] CARGO_INCREMENTAL=0 cargo test --offline --lib refshot_live -- --ignored --nocapture
```

Or the MCP tool `reference_shot {prompt, images, seconds, model, dry_run}`. fal's ByteDance filter may refuse realistic photos of people as likenesses; character sheets pass more often.

## Gotchas
- The song can be longer than the lyrics suggest. Use the length from the prepare step.
- Claude's desktop app runs commands inside a Windows app package that redirects `AppData\Roaming`. A stale shadow copy of RustCut's settings lived at `C:\Users\sales\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\RustCut\` and hid the real `ai.json` until the user renamed it (to `ai.olc.json`). If keys look missing, check there first. Spending recorded from here may land in that folder's shadow `economics.sqlite` instead of the user's real one.
- Microsoft Store Python sees yet another copy of AppData. Check settings with bash `grep -c`, not Python.
