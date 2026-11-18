# Lip-sync Studio: feature spec

**Status:** draft, 2026-11-18
**Source:** the Lera AiLab tutorial *"How I Made an AI Music Video with PERFECT Lip Sync"* (YouTube `uf-BlZVnGEQ`, 11:28, March 2026), which shows Dzine's lip-sync tool. It is compared here with what RustCut already does, and with what we learned making the Bond Money v2 music video.

## 1. Why

RustCut already has two lip-sync paths:

| Path | Code | What it does |
|---|---|---|
| **Fix the mouth** | `ai/lipsync.rs`, MCP `lip_sync` | Sync lipsync-2 or lipsync-2-pro on fal.ai. The part of a video clip under a voice clip (plus 0.15 s either side) is re-mouthed and replaced. |
| **Performance shot** | `ai/performance.rs`, MCP `performance_shot` | Seedance 2.0 makes a new shot from a still plus the song and lyrics. Spans over 14 s are split into 14 s segments that overlap by 1 s. |

Both work, but making a music video still takes many manual steps:
- generating a clip, importing it, placing it, muting it;
- making sure the speed is 1× and trimming it to the line;
- running the lip-sync, then checking the mouth by hand.

On Bond Money v2 these steps took about 30 minutes per scene. The tutorial shows one **guided workspace** instead: pick a face, pick a voice, see a small timeline, then generate. It also shows four capabilities we don't have:
- **image → lip-sync** for long audio;
- **face marking**;
- **auto-loop** when the audio is longer than the video;
- **1080p output**.

**Goal:** a single Lip-sync Studio in RustCut that takes a still or a clip plus any audio to a lip-synced clip on the timeline in one flow, with a cost estimate before anything is spent.

## 2. What the tutorial shows (feature inventory)

| # | Feature in the tutorial | Timestamp | RustCut today |
|---|---|---|---|
| T1 | Lip-sync workspace with **Normal / Pro** mode and **output quality** (720p / 1080p) | 3:47 | The model choice exists (lipsync-2 / -pro). The input is scaled down to ≤1280 px wide (`lipsync.rs:101`), so there's **no 1080p**. |
| T2 | **From an image** or **from a video** | 4:00 | Video: `lip_sync`. Image: only via a Seedance performance shot (≤14 s segments; refuses photo-real likenesses). |
| T3 | **Automatic face detection**, with **"Mark face manually"** as the fallback | 4:11, 7:18 | **None.** fal picks the face, and we can't choose one in a shot with several faces. |
| T4 | **Aspect ratio** step (keep the original or reframe) | 4:20 | The clip keeps the project aspect. `auto_reframe` exists separately. |
| T5 | A mini **timeline**: the scene image or video lane, the **detected face** lane, the audio lane | 4:26 | **None.** The user works on the main timeline. |
| T6 | **Pick a voice**: text-to-speech *or* upload audio | 4:35 | `speak_line` (ElevenLabs) and voice clips exist, but not inside the lip-sync flow. |
| T7 | Image mode accepts **up to 5 min** of audio; the still is **stretched to the audio length** | 4:47 | Performance shots split long spans and cost about $0.24/s. There's no cheap long-form option. |
| T8 | Works on the **full mix**: no need to isolate the vocals | 5:06 | `lip_sync` already accepts the song clip (used on Bond Money with `voice_clip_id: 3`). Not documented in the UI. |
| T9 | From an image, the **scene comes alive** (rain falling, car lights passing) without being prompted | 5:43 | Seedance does this. Fix the mouth doesn't (it needs existing footage). |
| T10 | Video mode accepts **up to 30 s** of audio, and the user **trims the video to the audio** | 7:34, 8:22 | `lip_sync` only covers the overlap. The clip has to be at 1× speed. There's no trim step. |
| T11 | **Side-profile** lip-sync works | 6:37 | Untested. Our skill says to prefer a three-quarter or frontal view. |
| T12 | **Moving singer**, with the original mouth movement replaced cleanly | 8:34 | lipsync-2 does this. On Bond Money, a wide-open, crying source mouth gave jagged teeth and smeared lips (see §6). |
| T13 | **Auto-loop**: when the audio is longer than the video, the video loops; this works best for cyclical motion | 8:54 | **None.** The audio past the end of the clip is ignored. |
| T14 | A **"Lip sync" button under a generated video** | 7:09 | `generate_video` and `reshoot` don't offer it. |
| T15 | Tip: use **medium shots**, since faces far away can't be detected well | 1:45 | This is in our skill docs, not in the app. |

## 3. Features

Priority: **P0** is the first release, **P1** comes next, **P2** later.

### F1. Lip-sync Studio window (P0)
A dockable window, the same kind as Motion graphics, opened from:
- a clip's right-click menu (**👄 Lip-sync…**), which replaces the current dialog;
- the AI menu;
- a **Lip-sync** button on the result of `generate_video`, `reshoot` and `reference_shot` (T14).

It has three steps, with **Back** and **Next** buttons:
1. **Source:** a video clip from the timeline, a video file, or a **still** (from the timeline, a file or the media library). It shows the face detection result (F3).
2. **Voice:** one of:
   - an audio clip already on the timeline (the default is the one that overlaps the source most, as `find_voice` does now);
   - an audio file;
   - **text-to-speech** with the character's voice (wraps `speak_line`: voice picker, model, stability).
   - When the audio is the song, an **audio range** picker snaps to lyric line starts.
3. **Generate:**
   - choose the method (F2), quality (F4), loop (F6) and aspect ratio;
   - shows a **cost estimate** and the length;
   - **Generate** runs it in the background with progress and **Cancel**.

**Mini timeline (T5):** three lanes, for the source (thumbnails), the face track (a face crop every 0.5 s) and the audio (waveform plus lyric captions). Drag the ends of the source lane to trim it to the audio (T10). Play previews the source with the audio, before anything is spent.

**Acceptance:**
- A user can go from right-clicking a clip to a synced result on the timeline without touching the main timeline.
- The estimate shown is within 10% of the real cost.

### F2. Method picker (P0)
| Method | When to use it | Engine |
|---|---|---|
| **Fix the mouth** | Existing footage, dialogue, or a sung line over a clip you already like | lipsync-2 / lipsync-2-pro (existing) |
| **Performance from a still** | A still, a short sung span (≤14 s per segment), the most emotion | Seedance 2.0 (existing `performance.rs`) |
| **Animate + sync** (new) | A still plus a long span, or when Seedance refuses | Image-to-video (Wan 3.0, Hailuo 3, Kling, as in `video_gen.rs`), then lipsync-2-pro, chained automatically |
| **Talking photo** (new, P1) | A still plus up to 5 min of audio, with a cheap, mostly static camera (T7) | An audio-driven avatar model on fal (to evaluate; see §7) |

**Recommend a method automatically:**
- a still with an audio span over 14 s → Animate + sync;
- a Seedance refusal → offer Animate + sync with one click (this happened 3 times on our projects);
- dialogue → Fix the mouth.

**Animate + sync** uses what worked on Bond Money:
- a steady-face motion prompt template with soft lips, dry cheeks, the head mostly toward the camera, and a push-in that stops at chest height;
- the source is muted, and its own audio is never sent;
- the source offset is chosen so the gestures land on the lyric (e.g. a hand to the chest on "battle in my chest").

### F3. Face detection and manual face marking (P0)
- On step 1, run face detection on sample frames (every 0.5 s). The quickest way is to reuse `ai/reframe.rs`, which already samples frames and has a vision model on OpenRouter locate the subject in batches. Ask for face boxes instead (costs about a cent per clip). A local detector (free and offline) is an open question (§7).
- Show the boxes. If there are **several faces**, the user clicks the singer. If **none** is found, the user drags a box (**Mark face manually**, T3).
- **Face-size check (T15):** warn when the face is under about 8% of the frame height, e.g. "The face is small in this shot; the sync will barely read. Use a medium shot or close-up." This would have caught Bond Money shot 16.
- Send the chosen face to the engine: crop the source to the face region (with padding) and composite the result back, or use the engine's face/speaker selection if it has one (**to verify** against the Sync API).

**Acceptance:**
- In a shot with two people, the user can choose who sings.
- A shot with no detected face can still be synced after the user marks a box.

### F4. Output quality (P0)
- **Quality:** Standard (720p, cheaper) or **High (1080p)**. Today the input is scaled to ≤1280 px wide (`lipsync.rs:101`). High keeps the source up to 1920 px, or 1080 on the short side.
- **Engine tier:** lipsync-2 (Normal) or lipsync-2-pro (Pro), as in T1. The default is **Pro for close-ups**, and for faces over 25% of the frame height.
- The output keeps the source's frame rate and colour (re-encode with CRF ≤ 18; no extra scaling).

### F5. Audio handling (P0)
- **Full mix is fine (T8).** Send the song clip as-is by default.
- Optional **"Isolate vocals"** (P2): run a local stem split and send only the vocals. Use it when a dense mix syncs badly.
- **Range from the song:** pick lyric lines, and the range snaps to line starts (as `performance::split_segments` does).
- **Pad:** keep the 0.15 s lead-in and tail (`PAD`).
- **Text-to-speech in the flow (T6):** the generated line is placed on a Voice track at the playhead, with the music ducked (`speak_line` behaviour). **Reuse a line that was already generated** instead of making it again: on Bond Money, placing a line generated it twice and wasted 107 credits.

### F6. Auto-loop when the audio is longer than the video (P1)
When the audio span is longer than the source video (T13):
- **Loop** (the default when the motion is cyclical) or **ping-pong** (forward, then reversed; smooth for swaying), **hold the last frame**, or **extend** (use the existing `extend_clip` to generate more footage; costs money).
- Build the looped source locally with ffmpeg before sending it. lipsync-2 then drives the mouth over the whole length.
- **Motion check:** estimate the camera and subject drift between the first and last frame (optical flow, or a mean frame difference). If it's high, warn: *"This shot moves forward; a loop will jump back. Try ping-pong or extend."*
- The cost estimate uses the looped length.

**Acceptance:** an 8 s clip plus 16 s of audio gives a 16 s synced clip with no visible jump in ping-pong mode.

### F7. Remove the speed-1× restriction (P1)
Today `lipsync::plan` refuses a clip that isn't at 1× (`lipsync.rs:81`).
- Instead, render the segment **at its timeline speed** (apply `setpts` before sending).
- Place the result at 1× over the same span.

On Bond Money, the rent flashback at 0.888× couldn't be lip-synced without undoing the beat fix.

### F8. Place the result (P0)
- **Replace in place:** split out the synced span and keep the source's transition, fades, transform and filters. This is what `lip_sync` does now; keep it.
- **On a new track above** (an A/B option), so the original stays underneath as a fallback.
- Mute the result's own audio. Never leave a generated video's audio on: Wan returns an audio track.
- Keep every take in the library as `lipsync-<name>-N.mp4` and add a **Takes** dropdown on the clip, so the user can swap back without undo (we swapped takes three times on the choir scene).
- Keep `scene_todos` and other project fields intact (this bug only affects the stale MCP binary, but add a save round-trip test).

### F9. Quality check after generation (P1)
After a result comes back:
- Show a **mouth contact sheet**: a face crop at 2 fps, in a grid (the `ffmpeg … crop … tile` check from our skills).
- Show a **preview with the audio**, played in the Studio.
- Run automatic flags:
  - **sync score:** the mouth-area motion against the vocal loudness, as in `singing_scene.py fit`;
  - **mouth artefacts:** sharpness or edge noise in the mouth box compared with the rest of the face;
  - **face drift:** the face embedding at the start against later frames.
- Offer one-click fixes:
  - **Re-sync with Pro**;
  - **Re-animate calmer** (Animate + sync with the steady template);
  - **Nudge the timing** ±0.05 s;
  - **Revert to the previous take**.

### F10. Batch lip-sync for a music video (P2)
Select several clips, or a range, under the song. The Studio lists every shot with:
- the detected face size;
- the recommended method;
- the cost.

The user ticks the shots to include and runs them as a queue. This is the `lipsync-clips` skill's workflow, built into the app.

### F11. MCP and Copilot parity (P0, alongside the UI)
Every Studio capability is also an MCP tool parameter:
- `lip_sync`: add `quality`, `face_box`, `loop: "loop"|"pingpong"|"hold"|"extend"`, `audio_range`, `place: "replace"|"above"`;
- a new `lip_sync_still {image, audio_clip|audio_path|text+voice, method, seconds, motion}`;
- `dry_run: true` on all of them, returning the cost, the length and the face check.

Update the `/lip-sync` and `/singing-scene` skills to use these tools.

## 4. Out of scope
- Training or hosting our own lip-sync model.
- Voice cloning. This already exists (`clone_voice`); only use voices the user has rights to.
- Generating the song itself.

## 5. Costs (current prices we have measured)
| Engine | Price |
|---|---|
| lipsync-2 | ≈ $0.05 per second of output |
| lipsync-2-pro | ≈ $0.083/s |
| Seedance 2.0 (fast) | ≈ $0.24/s; (best) ≈ $0.30/s. The video is the span + 1 s |
| Wan 3.0 image-to-video | ≈ $0.72 for 12 s |
| Hailuo 3 Max | ≈ $0.64 for 8 s |

The Studio always shows the estimate before generating, and records the real cost in the cost report (`lipsync`, `performance`, `video`).

## 6. Lessons from Bond Money v2 to build in
- **Source mouth matters.** A source with a wide-open, crying mouth (Wan with a "belting, anguished" prompt) gave jagged teeth and rubbery "oh" shapes after lipsync-2. A soft-lipped source plus lipsync-2-pro was clean. → The steady template in F2, and the artefact check in F9.
- **Tears creep in** even when the prompt says "no tears". → Check them in F9, and keep the push-in at chest height.
- **Seedance refuses photo-real likenesses** ("likenesses of real people"; free). → Fall back automatically in F2.
- **Speed ≠ 1×** blocked a lip-sync. → F7.
- **Microsecond overlaps** move a replaced clip to another track and switch off its transition. → F8 must place it on the same track exactly.

## 7. Open questions
1. **Talking-photo engine (F2, P1):** compare 2–3 audio-driven avatar or "image + audio → video" models on fal for quality on singing, the maximum length, the price per second and whether they refuse likenesses. Spend about $3 on a 10 s test of each, using the Aishyla podium still.
2. **Face selection in Sync lipsync-2:** does the fal endpoint take a face box or a speaker index? If not, use the crop-and-composite method (F3).
3. **Side profile (T11):** test lipsync-2-pro on a profile shot before promising it in the UI.
4. **Face detector:** start with the OpenRouter vision model (`reframe.rs`). Later, should we add a local detector (an ONNX model; licence, size, a new dependency) so it's free and works offline?

## 8. Release plan
| Phase | Contents |
|---|---|
| 1 (P0) | F1 Studio with steps and estimate, F2 (existing methods + Animate + sync + auto-fallback), F3 face detection and manual marking, F4 1080p, F5 audio and TTS reuse, F8 placement and takes, F11 MCP parameters |
| 2 (P1) | F6 auto-loop and ping-pong, F7 any speed, F9 quality check, Talking photo after the evaluation |
| 3 (P2) | F10 batch, vocal isolation |
