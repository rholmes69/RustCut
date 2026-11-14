---
name: broll
description: Write cinematic b-roll shots (gimbal moves such as a Zhiyun Crane 3 Lab) from a music video's treatment, then draw, animate and cut them into a RustCut music-video storyboard on the right lyrics, reusing every shot already animated. Use when the user asks for b-roll, cutaways, insert shots or detail shots for a music video, or for b-roll prompts from a treatment.
---

# B-roll from a video treatment

Proven on "Last Night" (Aishyla): 5 b-roll shots, $0.34 for the stills plus a $0.07 redraw and $0.65 to animate, cut into a 71-clip story-order video. The 22 shots already animated were reused for free. The worked example is in `example-last-night.json`.

## Rules (always)
- **Ask before spending money.** Show the prompts and the estimate first. Stop after the stills so the user can approve them before animating.
- **Never print API keys.** Prefix paid runs with `env -u OPENROUTER_API_KEY` (a stale one may be in the shell).
- **Warn before changing a project open in RustCut.** `produce` rewrites the `.rcproj`, so the user should close it or reopen it afterwards without saving over it. Don't kill a running RustCut.
- Don't name real celebrities in prompts.

## Prices (OpenRouter, measured)
| Step | Cost |
|---|---|
| Each b-roll still | ≈ $0.068 |
| Each b-roll shot animated (grok-imagine-video lite, 4 s) | ≈ $0.13 |
| Re-cut and export (shots already animated are reused) | free |

Estimate = n × $0.07 + n × $0.13 ≈ **$0.20 per b-roll shot**. Five shots cost about $1.

## 1. Read the treatment and the cut (free)
- Read the treatment, `theme.txt` (the wardrobe and the look by act) and the lyrics.
- List the current slots: `python -I .claude/skills/broll/broll.py list "$SB"`, with `SB="$D/music-video-<slug>/storyboard.json"`. It prints each cut's time, section, shot and lyric.
- Make a contact sheet of the existing stills (the command is in `/music-video` step 8), so the b-roll doesn't repeat what's already there.

## 2. Write the b-roll shots (free, as the director)
Write **one shot per act or story beat**: 5 for a 3–4 minute song. Draw each one from the treatment:

| Source in the treatment | Last Night example |
|---|---|
| A lyric image | "the bar lights low, the smoke hanging thick" → whiskey poured over an ice sphere |
| A visual motif | "nothing on your story" → a glowing phone story that dissolves into the real lounge |
| A turning point, shown through details | "making everything intertwine" → two hands inching across a candlelit table |
| A flash-forward or place | "rain-slicked sidewalk" → two pairs of feet walking in step through neon puddles |
| A transition between acts | night into morning → a skyline motion-lapse through sheer curtains |

**Each shot has three parts:**
- **Description, the first frame:** subject, setting, the act's lighting and palette, lens feel ("shallow depth of field"), and "No readable text or logos". Describe what the frame shows **at the start**. Put the change in the motion: for example, "a few inches apart, not yet touching".
- **Motion, the gimbal move plus the action:** use real stabiliser moves.
  - Underslung low glide along a surface.
  - A slow 180° roll ("vortex").
  - A steady push-in at table height.
  - A backward skim just above the ground.
  - An orbit.
  - A slow rise or reveal.
  - A sideways motion-lapse.
  Add slow motion for a slow song (match the BPM from `prepare`).
- **Seconds:** 4.

**Keep the leads out, or match them exactly:**
- B-roll shots are `no_lead`, so they get no cast photo. **Don't name cast members** in the description, or their photo comes back.
- **Hands, feet and silhouettes** of the leads must match the cast:
  - Give the skin tone ("Black woman's hand with warm deep-brown skin").
  - Give nails and jewellery ("long nude almond nails, thin gold bracelet").
  - Give that scene's wardrobe ("the hem of a burgundy satin dress", "the cuff of a charcoal grey suit jacket").
  - Last Night's shot 25 came out with a light-skinned hand that was already touching, and had to be redrawn.
- No faces. Use details, textures, places and props.

**Output:** give the user the five shots as a numbered list, each with a title, the lyric or moment it covers, the description, the motion, the Crane move, and the estimate. When they only asked for prompts, stop here.

## 3. Add and draw (paid ≈ $0.07 per still, ask first)
1. Write the shots to `$D/broll.json` as `[{"description": "…", "motion": "…", "seconds": 4}, …]`.
2. Add them: `python -I .claude/skills/broll/broll.py add "$SB" "$D/broll.json"`. This backs up `storyboard.before-broll.json` once, then appends the shots as `no_lead` with no frame yet.
3. Draw only the missing stills, passing the same cast as the plan. The cast is still needed for the existing shots' redraws; b-roll gets none:
   ```bash
   cd /c/Users/sales/MyVideoEditor && env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=redraw MV_REDRAW="" MV_CAST="…" CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture
   ```
4. Make a contact sheet of the new stills and check them:
   - skin tone and wardrobe;
   - no text or logos;
   - the start state is right.

   Redraw a weak one with `MV_REDRAW="25" MV_DESC_25="…" MV_MOTION_25="…"`. Show the user and ask whether to animate.

## 4. Place them on the cut (free)
Choose the slots to replace with `broll.py list`:
- **The lyric names the image.** The pour went on "the bar lights low", the hands on "intertwine" and the phone on "post it on the 'gram".
- **Instrumental gaps and section changes** (`break` slots, the end of the bridge) suit place and transition shots. The sunrise went just before the final shot.
- **About one b-roll every 30–45 s.** Never put two b-roll shots back to back. Never replace a lip-sync or performance shot, the chorus hook, or the opening and closing shots.
- A strong motif can come back once in the bridge to echo act I. Last Night's phone shot 24 played at 53 s and again at 212 s.

Then place them: `python -I .claude/skills/broll/broll.py place "$SB" "40=23,53=24,145=25,165=26,239=27"`, where each pair is a time in the slot (seconds) = a b-roll shot number. List the slots again to check.

**A story told out of order:** if the first cut was made in `sections` mode and repeats shots out of order, fix the order in this same pass. Rewrite `slots[].shot` so the story runs in order; Last Night's order is in its `WORKFLOW` file. Back up the project as `<Title>.before-recut.rcproj` first.

## 5. Animate and re-cut (paid ≈ $0.13 per new shot, ask first)
1. Back up the project: `cp "$D/<Title>.rcproj" "$D/<Title>.before-broll.rcproj"`.
2. Run produce in the background (about 3 minutes):
   ```bash
   env -u OPENROUTER_API_KEY MV_DIR="$D" MV_PROJECT="<Title>" MV_STAGE=produce CARGO_INCREMENTAL=0 cargo test --release --offline --lib pipeline_music_video -- --ignored --nocapture
   ```
   It animates only the shots with no video and reuses the rest (the log says "reused N"). The user's own reshots in the storyboard are kept. It clears and rebuilds **only the main track**, so titles, lyric captions and the audio stay. It exports `$D/<Title> - music video (preview 720p).mp4`.
3. **Check:**
   - the video is still as long as the song;
   - sample frames at each b-roll time (`ffmpeg -ss <t> -i … -frames:v 1`, or `fps=1/15,tile=4x4`);
   - each b-roll lands on its lyric;
   - the titles are still there;
   - no animation flaws.
4. **Report:** the preview path, the cost (stills, redraws, animation, plus "N shots reused free"), the backups, and the frame check. Offer to redo a single flawed animation (≈ $0.13).

## Text-only alternative
For an abstract shot with no still to approve (smoke, light leaks, a city timelapse): `MV_STAGE=t2v MV_PROMPT_FILE=prompt.txt MV_SECONDS=6 [MV_DRY=1]`. It uses Kling by default. Do a dry run first: it prints the estimate. The video lands in `$D/RustCut AI/new shots/` for placing by hand in RustCut; it isn't added to the storyboard.
