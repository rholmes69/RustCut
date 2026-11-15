---
name: 3d-blocking
description: Claude 3D Blocking - render simple 3D block previz videos (block figures, flat colours, smooth cinematic cameras, optional song sync) as motion references for AI video tools like Seedance. Use for blocking, previz, animatics, camera tests, music videos of any genre, indoor or outdoor scenes, oners or multi-shot sequences.
---

# Claude 3D Blocking

Turn a scene idea (plus optional reference images and a song) into a **3D block previz video**. The figures are simple
blocks, the colours are flat and there are no textures. The video's job is to show **direction, movement,
interaction, camera, framing and pacing**. An AI video model (Seedance, Kling, Veo, etc.) then uses it as a motion
reference and supplies the look from separate character and environment images.

Everything runs locally with `scripts/previz.py`, a pure Python software renderer (numpy, Pillow, ffmpeg). It needs no
GPU, no Blender and no network.

## When to use
- "Make a 3D block / blocking / previz / animatic of this scene."
- Music video sections timed to a song: dance formations, performance shots, beat-synced cuts.
- One continuous shot (a "oner") or several cut shots, each with its own camera move.
- Testing camera moves: orbit, crane to overhead, dolly zoom, drone chase, car-side tracking, leading shot, whip pan,
  handheld, push through a window or screen, side truck.
- Any genre and location: pop, country, rock, rap, R&B, EDM, indie, metal, Latin; stages, streets, fields, forests,
  bedrooms, clubs, festivals. `references/genre_playbook.md` maps each genre to its locations, camera language and
  pacing.

## The look (non-negotiable unless the user asks otherwise)
1. **Block figures only**: a box body plus a sphere head, via `figure()`. Do **not** animate limbs, hair, neck or
   faces, and don't add arm blocks. A figure moves only as a whole: position, yaw, scale and visibility. The AI model
   animates the body. Limb animation in the block confuses it.
2. **Flat matte colours**: no textures, grid lines, bloom or glow halos. "Emissive" parts are simply brighter flat
   colours. Keep the provided palette or pick a few clear colours that separate the characters, e.g. a dark lead,
   grey extras, a cream second hero, and accent colours for effects.
3. **Simple sets**: build them from the set-piece library (roads, fences, hills, trees and forests, houses, city
   buildings, cars with drivers, streetlights, stages, band gear, crowds, rooms and furniture, sun) or plain boxes,
   cylinders and tori. Use a few muted colours per location so it reads at a glance, e.g. golden-hour field, night
   street, dark club, overcast forest.
4. **Effects are blocks too**: hologram reveal, beam strike, shockwave rings, cube shatter, scaffold building build.
   Keep them readable and short.
5. **Smooth everything**: cameras run through `path_cam` / `orbit_cam`, which use Catmull-Rom plus Gaussian
   smoothing. Rotations stay continuous: no snapping a prop from flat to upright. Nothing teleports unless it is a cut.

## Workflow
1. **Gather the brief.** Get the genre, the location (indoor or outdoor, time of day), the story beats, the characters (which colour block is who), the setting, the duration,
   oner or cuts, and the camera ideas. If there is a song, get the file, the clip's start time in the song, the BPM,
   and where the downbeats fall. Look at any reference images and the user's previous blocking.
2. **Write a shot plan first.** Give each shot an id, its in and out times, the camera move and one line of action.
   For a oner, list the camera key moments instead. Share it in a short list and continue unless the user wants to
   approve it first.
3. **Write the scene file** next to `scripts/previz.py`, or anywhere, and start with `from previz import *`.
   - Copy the closest example from `scripts/examples/` (see the table below) and edit it.
   - Pick the location and camera language from `references/genre_playbook.md`.
   - The API is in `references/scene_api.md`. The camera recipes are in `references/camera_moves.md`.
4. **Stills first.** Run `python previz.py stills scene.py --times 0.5,1.5,3,...` and look at the contact sheet, using
   12 or so times across the clip plus every key moment. Fix the framing until each still reads clearly. Check:
   - Heads aren't cut off.
   - Nothing hides the action.
   - The camera isn't inside a body or a wall.
   - The prop sits where it should.
5. **Physics check.** Props must sit on surfaces and be held at the hand markers (`fig.hand_r`, via `world_pos`).
   Nothing floats or intersects. Verify contacts numerically when it matters, by printing distances.
6. **Jitter check.** Run `python previz.py check scene.py`. It flags single-frame pops, flips and camera snaps, and
   ignores cuts and things appearing or disappearing. Fix everything it reports before rendering.
7. **Render.** Run `python previz.py render scene.py --out out [--audio song.mp3 --audio-start 22.8]`. It produces:
   - `NAME_clean.mp4`: the reference you give the AI video tool.
   - `NAME_labeled.mp4`: shot id, title, timer and action note burned in, for review.
   - `NAME_clean_nomusic.mp4`: only when audio is given.
   - `NAME_timing.txt`: the shot list with clip times.

   At 960×540 and 24fps, expect roughly 5–15s of render time per second of video on 2 cores.
8. **Look at the result.** Pull 6–12 frames, or the stills sheet, before delivering. Then deliver the clean and
   labeled videos plus the timing sheet, and offer a matching video prompt (below).
9. **Iterate on feedback.** Typical notes are "less bouncing", "camera too fast", "head too high in frame", "hand goes
   through the machine" and "weird cut at 7s". `references/lessons.md` lists each one with its fix.

## Timing to a song
- `beat = Beat(bpm, first_beat_time)`. `beat.time(n)` gives the time of beat n. `beat.hop(t)` is a small lift on each
  beat. `beat.snap(t, deg)` is a continuous left/right turn that lands on the beat.
- Put cuts, pops, impacts and formation changes on beats or bars. Render with `--audio` and `--audio-start` (where
  this clip begins in the song) so the review video plays in sync.
- If the BPM is unknown, estimate it with librosa if it is installed, or ask the user.

## Oner vs multi-shot
- **Oner**: a single `Shot` with `path_cam` (free path) or `orbit_cam` (orbit, crane, overhead). Example:
  `examples/oner_reveal.py`, which goes room → button press → through a screen → a world builds → beam and hologram →
  close-up. Example: `examples/action_fx.py`, which orbits low inside a ring of clones, follows a thrown disc and
  cranes up to an overhead.
- **Cuts**: several `Shot`s, each with its own camera function, cut on the beat. Vary the camera from shot to shot,
  e.g. low dolly, side truck, overhead, low orbit. Example: `examples/music_video_cuts.py`.

## Example scenes (copy the closest one)
| file | genre and location | structure | shows |
|---|---|---|---|
| `music_video_cuts.py` | pop/K-pop, neon stage | 4 cuts, 10s | formations, beat snaps, hologram pop-ins |
| `oner_reveal.py` | sci-fi, room to city | oner, 11s | prop press, push through a screen, city build, beam and hologram |
| `action_fx.py` | sci-fi action, stage | oner, 8s | clone ring, prop in hand, throw and shatter chain, crane to overhead |
| `country_roads.py` | country, fields at sunset | 4 cuts, 12s | drone chase, car-side tracking, driver in a pickup, tailgate, crane reveal |
| `rock_club.py` | rock, club stage | 6 cuts on bars, 12s | band gear, jumping crowd, handheld, whip pan, overhead drums, crash push |
| `rap_street.py` | rap, night city street | oner, 11s | leading shot, crew, walk-to-stop, low orbit, passing car, crane up |
| `ballad_apartment.py` | R&B/ballad, bedroom at night | 3 cuts, 12s | doorway push, dolly zoom, top-down on the bed, seated and lying figures |
| `edm_festival.py` | EDM, festival at night | oner, 10s | drone fly-in, beat drop (flash, shake), LED wall and beams on the beat, jib to DJ |
| `indie_forest.py` | indie/folk, forest and lake | oner, 11s | side track with foreground trunk wipes, lake reveal, rise over water |

## Writing the video-model prompt (offer after delivering)
Keep it short and structured. The previz video is the motion and camera reference, and the images give the look.
```
Use @video1 as the motion, camera and timing reference (3D block previz - follow its blocking, framing and camera
move exactly; ignore its grey-box look). @image1 = <character>, @image2 = <environment>, @image3 = <prop>.
[00:00-00:03] <what happens + camera>
[00:03-00:06] ...
Style: <look>. Sound: <sound effects>, no music.
```
Map each block colour to its character, e.g. "the dark block figure is @image1, the grey masked figures are her
clones". Describe the effects in the target look, for example "pink holographic clones flicker in" rather than
"pink boxes appear".

## Files
- `scripts/previz.py`: the engine and CLI (stills / render / check).
- `scripts/examples/*.py`: nine example scenes (table above).
- `references/scene_api.md`: every primitive, set piece, helper, effect and camera, with its arguments.
- `references/genre_playbook.md`: genre → locations, camera language, pacing, motion, and how to build each location.
- `references/camera_moves.md`: a catalogue of camera moves with ready-to-paste keys.
- `references/lessons.md`: common problems and their fixes. Read it before the first render.

Requirements: Python 3.9+, `numpy`, `Pillow`, `ffmpeg` on PATH. If any are missing, run
`pip install numpy pillow` and install ffmpeg.
