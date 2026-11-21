---
name: lipsync-clips
description: Lip-sync a song in RustCut as a series of expressive Seedance 2.0 performance clips. Splits the song into 12 s audio clips (1 s lead-in and tail), draws an expressive keyframe per clip with Gemini from the artist's reference photos, then makes one multi-shot Seedance shot per clip from the user's lyrics, image(s) and shot list ("@Image 1 sings @Audio 1"), with acting direction that fits the song's tone. Use when the user wants the singer to lip-sync the song or part of it with real facial emotion, with different looks per scene.
---

# Lip-sync clips: the song in 12 s pieces

Each clip is 12 s of song plus 1 s before and 1 s after (14 s of audio, under the model's 15 s limit). Each clip becomes one multi-shot Seedance video of the singer, sung to that clip's audio. This is the workflow that gave Aishyla's best chorus (Thirsty for U v3): an expressive Gemini keyframe, then Seedance best with acting direction.

## Rules (always)
- **Ask before spending money.** Quote each paid step and get a yes: the keyframes, then the Seedance run. Do a `dry_run` first. Do the first clip alone and have the user check likeness, emotion and lip-sync before the rest.
- **Never print API keys** (fal.ai: `%APPDATA%\RustCut\ai.json` `fal_key`, or `FAL_KEY`; OpenRouter: `api_key`). Error text can echo a key: filter tool output with `sed -E 's/fal_[A-Za-z0-9_:-]+/[key]/g'`.
- **The fal.ai key must be the full `key_id:key_secret`** with no line break. If RustCut says "No fal.ai API key", the Save button in **AI → Dialogue & voices** wasn't pressed. A 401 "Expected '<key_id>:<key_secret>'" means only half the key was pasted.
- **Prices** (fal.ai, 720p): Seedance 2.0 fast ≈ 24¢/s, **best ≈ 30¢/s**. A 10 s best clip ≈ 3.03 USD, a 14 s clip ≈ 4.20 USD. Gemini keyframe ≈ 7¢. A whole 3:40 song is ~19 clips (≈ 60 USD+), so for a small budget do the hero clips only (choruses, bridge) and fill the rest with cheaper shots.
- Warn the user before changing a project that is open in RustCut.
- Run Python with `PYTHONIOENCODING=utf-8` on Windows, or printing a "→" crashes the driver before any paid call.

## 1. Split the audio (free)
```bash
python .claude/skills/lipsync-clips/lipsync_clips.py split "<song.wav>" "<project folder>/audio-clips" --project "<project>.rcproj"
```
Writes `clip-NN_<start>-<end>.mp3` and `clips.json` (with the Lyrics-track lines that start in each 12 s core). Show the user the table. Lyric timing from whisper is only good to about 1 s: for a single hook, re-cut the span by ear: `ffmpeg -ss <start> -t 10 -i <song> clip.mp3`, starting where the first sung word begins.

## 2. Ask the user, clip by clip
For every clip ask for **lyrics**, **image(s)** (different scenes use different images; `@Image 1` is normally the lead) and **shot descriptions** (2–4 shots). Offer a draft from the treatment, the lyrics and the scene so the user only corrects it. Prompt format:

```
The woman in @Image 1 sings the hook @Audio 1 as a raw, emotional performance.
Shot 1: Close-up, three-quarter angle, she sings "<line>" with a slow, knowing look into the lens, brows drawn together, a hint of a smirk on "<word>".
Shot 2: Her eyes close and her head tilts back slightly on "<line>", lips trembling, steam drifting.
Shot 3: Eyes snap open as the energy builds on "<line>", a powerful belt, jaw lifted, intense and unapologetic.
Amber backlight, steam, droplets on her shoulders, shallow depth of field.
```
**Acting direction is what makes it great.** Tie each shot to the words and the song's arc (restraint, then yearning, then a belt). Name the expression (brows, eyes closing, jaw, trembling lips, smirk). Keep the mouth visible in the singing shots. Put cutaways (hands, water, details) between them. Don't put a mic in front of the face. Describe wardrobe only if it differs from the keyframe.

## 3. Draw the keyframe (paid ≈ 7¢ each, draw 2)
**Seedance refuses real-person photos and frames made by a face-trained LoRA** ("likenesses of real people", HTTP 422, no charge). It accepts an AI-drawn still. So draw the start frame with Gemini from the cleaned reference photos (crop off phone UI bars and overlay cards first):

```bash
PYTHONIOENCODING=utf-8 python .claude/skills/lipsync-clips/gemini_frame.py "<project>/keyframes" clip06 "Cinematic 35mm film still, 16:9. <Name>, with exactly the same face, skin tone, hairstyle and jewelry as in the reference photos, wears <wardrobe> and stands in <setting>. Close-up, three-quarter angle, chest up, singing with raw emotion: brows drawn together with longing, eyes intense and glossy, lips parted mid-lyric. No microphone, hands out of frame. <light and atmosphere>, shallow depth of field, photorealistic skin texture, no text or logos." "<reference sheet.png>" --n 2
```
Look at both and pick: face fully visible, mouth open, likeness right, no stray text. Prefer the one that starts at the song's mood at that moment. If it is refused, do not alter the image to get past the filter: tell the user, and fall back to Kling Pro on the frame plus a lip-sync pass (below).

## 4. Generate one clip (paid)
```
reference_shot { prompt: "<prompt>", images: ["<chosen keyframe>"], audio: "<clip mp3>", seconds: 10, model: "best", dry_run: true }
```
`seconds` is the audio length rounded up (4–15). The dry run checks the tags (`@Image1…`, `@Audio1`) and returns `estimated_usd`. After approval, call it again without `dry_run`, then `save_project`. The video lands in `<project>/RustCut AI/reference shots/` and in the media library. The project must be open in the MCP session (`open_project`), and `reference_shot` needs the build with the `audio` parameter.

## 5. Check and preview
- Frames: `ffmpeg -i <clip.mp4> -vf "fps=1,scale=384:-2,tile=5x2:padding=2" -frames:v 1 check.jpg`. Look for emotion that fits the line, an open mouth on sung syllables, the same face and outfit, no subtitles.
- Preview with the song: `ffmpeg -i <clip.mp4> -ss <start> -t <len> -i <song> -map 0:v -map 1:a -c:v libx264 -crf 18 -pix_fmt yuv420p -c:a aac -shortest preview.mp4`. Put B-roll before and after to hear it in context, scaling every input to 1280x720 at 24 fps before `concat`.
- Place each result on the timeline at the clip's **core** start; trim the 1 s lead-in; mute the generated audio so the song plays alone.
- Report the running cost against the budget.

## Fallback: Kling Pro + lip-sync (when Seedance refuses the frame)
Kling accepts LoRA frames. Animate with `fal-ai/kling-video/v2.1/pro/image-to-video` (≈ 9.8¢/s) and write the acting into the prompt, then fix the mouth with **`fal-ai/sync-lipsync/v2`** and `"model": "lipsync-2-pro"` (≈ 5 USD/min). Do **not** use the old `fal-ai/sync-lipsync` (v1): its mouths looked bad. Expect a stiffer face than Seedance gives. Always check a download works: download with `requests` (not `urllib`, which fails SSL checks here) and log each result URL before downloading, because a failed download loses a paid image.
