---
name: music-video-production
description: The whole RustCut music-video production, start to finish, for an artist's song (e.g. Aidan B. with Aishyla) - treatment, stills and redraws, animation, story-order re-cut, b-roll, HyperFrames intro and outro cards with the logo, beat-matched transitions, then a matching lyric video, ending with exports and delivery. It starts by asking for the artist name, song title, treatment, lyrics and the audio file (default: Downloads). Use when the user asks for a new music video project "with everything", a lyric video for a finished music video, intro/outro cards with the logo, or asks which step comes next.
---

# Music video production: treatment to lyric video

The full run proven on Aidan B.'s **"You Are Always On My Mind"** (3:16, 5.93 USD) and **"MidNight Kiss"** (ATL remix, 5:31, 6.75 USD), both with Aishyla. Only the stills, redraws and animation cost money; the re-cut, cards, transitions and lyric video are free.

This skill is the running order. Each stage that has its own skill links to it; follow that skill for the detail.

```bash
D="C:/Users/sales/Videos/RustCut AI/<Title> music video"
P="$D/<Title>.rcproj"
SB="$D/music-video-<slug>/storyboard.json"      # slug = artist + title, printed by prepare
K=.claude/skills/music-video-production
```

## Rules (always)
- **Ask before spending.** Estimate first; show the stills (contact sheet) before animating; quote every redraw.
- **Never print API keys.** Prefix paid runs with `env -u OPENROUTER_API_KEY`.
- **RustCut must be closed** before any stage that rewrites the `.rcproj` (`tasklist | grep -i rustcut.exe`; `rustcut-mcp.exe` is fine). Never kill it; ask the user.
- **Back up before each stage**: `cp "$P" "$D/<Title>.before-<stage>.rcproj"` (produce, recut, broll, intro-outro, transitions).
- Don't name celebrities in prompts. Don't alter images to get past a content filter.
- If OpenRouter returns **HTTP 402**, nothing was charged: the account is out of credits. Tell the user and wait.
- Disk is tight: build with `CARGO_INCREMENTAL=0 … --release --offline`. If an export fails with a full disk, ask before clearing `%TEMP%`.

## 0. Ask the user first (before anything else)
Unless the user already gave them in their message, ask for these five things **in one message**, numbered, before creating any folder or spending anything:

1. **Artist name** (e.g. "Aidan B.").
2. **Song title** (exactly as it should appear on the cards, e.g. "MidNight Kiss").
3. **Music video treatment**: the story, look, cast and wardrobe. If they have none, offer to draft one from the lyrics (step 1).
4. **Song lyrics**, with the section headers if they have them ([Verse 1], [Chorus]…).
5. **Location of the audio file.** Default: the **Downloads** folder (`C:\Users\sales\Downloads`).

For the audio file, look in the default location yourself and offer what's there, newest first:
```bash
ls -t /c/Users/sales/Downloads/*.{wav,mp3,flac,m4a,aiff} 2>/dev/null | head -8
```
Then use AskUserQuestion with up to four of those files as options (the newest one marked Recommended, especially if its name matches the song title), and the "Other" answer for a different path. If nothing is in Downloads, ask for the path.

Rules for the answers:
- Use the title and artist exactly as given (capitalisation included) for the project name, the folder `C:/Users/sales/Videos/RustCut AI/<Title> music video`, the cards and the YouTube details.
- **Check the treatment isn't the lyrics pasted twice** (MidNight Kiss). If it is, say so and offer to draft one.
- If the treatment names the artist's love interest or other cast, confirm who they are and which reference photo or character sheet to use (Aishyla's is in `Videos\RustCut AI\Checklist music video\`). Ask this together with anything else that's missing, not one question at a time.
- Copy the audio file into the project folder; don't move the original.

Then repeat the five answers back in a short summary with the estimate for stages 2–5, and wait for a go-ahead.

## The stages
| # | Stage | How | Cost (measured) |
|---|---|---|---|
| 1 | Treatment → `theme.txt` | this skill, below | free |
| 2 | Lyrics + `prepare` (beats, sections, lyric timing) | `/music-video` steps 2–3 | free |
| 3 | Plan + stills, contact sheet | `/music-video` steps 7–8 | ≈ 1.60 USD for 22 shots |
| 4 | Redraws | `/music-video` step 9 | ≈ 7¢ each |
| 5 | Animate (`produce`) | `/music-video` step 10 | ≈ 13.5¢ per shot |
| 6 | Story-order re-cut | this skill, below | free |
| 7 | B-roll | `/broll` | ≈ 20¢ per shot |
| 8 | Transitions + beat effects | `/transitions` | free |
| 9 | Intro and outro cards with the logo | this skill, below | free |
| 10 | Lyric video | this skill, below | free |
| 11 | 1080p exports, delivery | this skill, below | free |

Show the user each stage's result and get a go-ahead before the next one. Typical totals: 6–7 USD with 22 shots, a few redraws and 5–6 b-roll shots.

Doing the cards (9) before transitions (8) is fine too; just re-add the outro if a later `produce` wipes it (see Gotchas).

## 1. Treatment → `theme.txt` (free)
Turn the user's treatment into the pipeline's format (examples: `example-theme-midnight-kiss.txt`, and the YAOOM one in its project folder):
- **Title line:** artist, title, genre, the one-sentence story.
- **LOOK:** lens and aspect (compose for a 2.39:1 crop: action in the centre band), palette, light, haze. Always: phones glow softly with **no readable text, letters or logos**; neon is abstract shapes.
- **CAST:** each person named exactly as in `MV_CAST`, with age, skin tone, hair, signature jewellery. Unnamed extras (a guitarist, a saxophonist) in silhouette, "never named".
- **WARDROBE:** a numbered look per act for each lead.
- **STORY BY SECTION:** one bullet per song section **with times from `prepare`**, naming the lead in each scene (a shot only gets a person's photo when its description names them).
- **EMOTIONAL BEATS** and "classy and romantic, no explicit content".

**If the "treatment" pasted is actually the lyrics** (it happened on MidNight Kiss), say so, draft a treatment from the lyrics and the song's mood, and get approval before planning.

**Remixes sing the song more than once.** If the track is much longer than the lyric sheet suggests, transcribe it (MCP `auto_caption`; download the `base` model once with `download_caption_model`), rewrite `lyrics.txt` in the real sung order (both passes, solos, the outro), re-run `prepare`, and set `project.music[...].sections` by hand if the detector merged them. Map the treatment onto the real structure (first pass = the meeting, solo = the slow dance, second pass = the night deepening).

## 3–5. Stills, redraws, animation
Follow `/music-video`. Lessons from these two videos:
- **Wardrobe leaks from the character sheet** (the sheet's denim or mesh shows up). Redraw with the outfit spelled out and "not denim / not mesh".
- A **face that drifts** or is too small: `/face-fix`. A prop change (cologne → perfume): redraw that shot's description and fix the lyric text too.
- Redraws save `shot-NN-2.png` and clear that shot's `video`; `produce` then animates only the shots with no video and reuses the rest.

## 6. Story-order re-cut (free)
`produce` in `sections` mode reuses shots by section, so the story plays out of order and repeats. Fix it:
1. `python -I .claude/skills/broll/broll.py list "$SB"` shows each slot's time, section, lyric and shot.
2. Rewrite `slots[].shot` (0-based) with node so the story runs in the order of `theme.txt`: each scene in its section, the reveal on its lyric, the ending last. **No shot twice in a row**; a repeat is fine in a later chorus.
3. Back up `<Title>.before-recut.rcproj`, run `produce` again (free: every video is reused). Sample frames to check.

## 9. Intro and outro cards with the logo (free, HyperFrames)
Templates are in `$K/cards/intro` (5.8 s, transparent, logo + "presents", then artist and title over the opening shot) and `$K/cards/outro` (10 s end card: title, artist, credit, then the logo).

1. Copy them, and the logo, into the project:
   ```bash
   mkdir -p "$D/hyperframes" && cp -r $K/cards/intro $K/cards/outro "$D/hyperframes/"
   for c in intro outro; do mkdir -p "$D/hyperframes/$c/assets"; cp "C:/Users/sales/Videos/RustCut AI/MidNight Kiss music video/hyperframes/intro/assets/ready-rock-media-logo.png" "$D/hyperframes/$c/assets/"; done
   ```
2. Edit the variables in each `index.html` (`data-composition-variables` defaults **and** the CSS `--accent`/`--gold`, `--ink`), plus the text in `compositions/title.html` and `compositions/endcard.html`:
   - `artist` ("AIDAN B."), `title1`/`title2` (the title split over two lines), `credit` ("with Aishyla");
   - the accent colour from the video's palette (YAOOM warm gold, MidNight `#FF5FD2` neon pink);
   - the flare's `data-variable-values`: `tint`, `hue`, `hueSpread`. **Keep the intro flare light** (`emitters` ≈ 22, `intensity` ≈ 4): the first MidNight snapshot at 46/7 buried the shot;
   - the outro's background gradient (MidNight: `#141a3c` → `#04050c`).
3. Check and look:
   ```bash
   cd "$D/hyperframes/intro" && npx hyperframes check --json && npx hyperframes snapshot --at 1.2,2.6,4.0,5.0 --no-end -o snaps
   cd ../outro && npx hyperframes check --json && npx hyperframes snapshot --at 0.5,2.0,3.5,5.5,7.4 --no-end -o snaps
   ```
   Read the snapshots. Fix any error before rendering.
4. Render:
   ```bash
   (cd "$D/hyperframes/intro" && npx hyperframes render --format=mov --quality=delivery --skill=hyperframes -o "../../<Title> - intro with logo (HyperFrames).mov" --quiet)
   (cd "$D/hyperframes/outro" && npx hyperframes render --format=mp4 --quality=delivery --skill=hyperframes -o "../../<Title> - outro with logo (HyperFrames).mp4" --quiet)
   ```
5. Place them (MCP, RustCut closed, backup `<Title>.before-intro-outro.rcproj`): `open_project`, `import_media` both;
   - intro: `add_clip {media_id, start: 0}` lands on the track above Main, over the opening shot;
   - outro: `add_clip {media_id}` with no start appends it to the end of Main; then `set_transition {clip_id: <outro clip>, kind: "dip_black", duration: 4 beats}` (4 × 60 / BPM).
   - `render_frame` at 1 s, 3.5 s and song end + 4 s; `save_project`; restore any top-level keys the MCP server dropped (the node one-liner in `/transitions` step 4.6); export the 720p preview.

## 10. Lyric video (free)
A calm companion to the music video: a drifting gradient background, the music video's stills dimmed and blurred with a slow Ken Burns drift and dissolves, the same lyric timing restyled in Playfair, a wave visualizer, and the same intro and outro cards.

1. **Background** (two palettes: the video's main look, then the ending's, cross-fading where the outro starts; durations add up past the song length):
   ```bash
   cd "$D" && ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "gradients=s=1280x720:r=30:d=<outro start + 4>:n=4:c0=0x04050c:c1=0x1c0c44:c2=0x7a1462:c3=0x020309:speed=0.004:type=radial:seed=9" \
    -f lavfi -i "gradients=s=1280x720:r=30:d=<song − outro start + 4>:n=4:c0=0x081030:c1=0x2a4a7a:c2=0xb87a66:c3=0x04060c:speed=0.004:type=radial:seed=13" \
    -filter_complex "[0][1]xfade=transition=fade:duration=4:offset=<outro start>,noise=alls=6:allf=t,vignette=PI/4.5,format=yuv420p" \
    -t <song + 0.1> -c:v libx264 -preset veryfast -crf 20 lyric-background.mp4
   ```
   Those are MidNight's neon → dawn colours. YAOOM used warm amber/rose (`0x1a0306, 0x6e1020, 0xd9821e, 0x0a0102`).
2. **Scenes** → `$D/lyric-scenes.json` (copy `example-lyric-scenes-midnight-kiss.json`). Print the lyric lines first:
   ```bash
   node -e 'const p=JSON.parse(require("fs").readFileSync(process.argv[1]));p.tracks.find(t=>t.kind=="Text").clips.forEach(c=>console.log(c.start.toFixed(2),c.source.Text.content.replace(/\n/g," / ")))' "$P"
   ```
   - One still per 8–15 s, **in story order**, each starting on a lyric line start; the first starts when the intro card ends (5.8), the last ends at the song's end.
   - Use the **latest redraw** of each shot (`shot-06-2.png`, not `shot-06.png`), and b-roll stills on instrumental parts (sax/guitar solos, the dawn).
   - The still matches the line: the kiss on "Midnight kiss", hands on "hands intertwined", the sunrise in the outro. Avoid stills with garbled sign text where you can.
   - `dissolve`: 2–4 beats in seconds; `endDip`: 4 beats. Optional `textColor` (`[255,240,250,255]` for neon, default warm ivory), `font`, `fontSize`.
3. **Build** (writes `<Title> - lyric video.rcproj`, backing up an old one as `.before-rebuild`):
   ```bash
   node $K/build-lyric-video.js "$D/lyric-scenes.json"
   ```
4. **Visualizer** (MCP): `open_project` the lyric project, `add_visualizer {kind: "wave", color: <accent>, position: "bottom"}`, then `update_clip {clip_id: <its clip>, opacity: 0.55}`. `render_frame` at 3.5 s (intro), a chorus and the outro; check the lyric is readable over every still. `save_project`.
5. **Export**: `export_video {output: "$D/<Title> - lyric video (preview 720p).mp4", resolution: 720}` (about 9 minutes for 5:40), then open it for the user.

To change a still later: edit `lyric-scenes.json`, rebuild, and add the visualizer again (the rebuild drops it).

## 11. Finish and deliver
- Ask the user to check by ear anything you guessed (MidNight's opening "Ooh, yeah" at 0:06 was placed by hand).
- **1080p exports** of the music video and the lyric video: `export_video {output: "… (1080p).mp4", resolution: 1080, quality: "high"}`. Check disk space first.
- Report the cost table (stills, redraws, animation, b-roll; everything else free) and the backups.
- **Deliver:** offer a Google Drive backup (video and project zip), a YouTube upload with the details filled in, and a release plan on Google Calendar: `/music-video` step 13. Ask before each; they act on the user's real accounts.
- Optional extras to offer, quoted first: 2.39:1 cinema bars (free), a chorus lip-sync on the artist (`/lip-sync`, `/lipsync-clips`; paid).

## Gotchas
- **`produce` rebuilds the whole main track**, so the outro card on Main is lost: re-add it (step 9.5) after any later `produce`. The intro on the upper track, the lyrics and the audio survive.
- The old MCP server drops `scene_todos` (and other new top-level keys) when it saves: always run the restore one-liner from `/transitions` after `save_project`.
- `update_clip` with a new `speed` recomputes the duration from the full source; pass `duration` and `track_index` too, or the clip jumps to another track.
- `open_project` output is huge; it is saved to a file. Don't read it; use `get_project` or node on the `.rcproj`.
- On Windows, a printed "→" can crash a Python driver: set `PYTHONIOENCODING=utf-8`.
