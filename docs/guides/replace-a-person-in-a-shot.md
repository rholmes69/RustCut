# Replace a person in some shots of an AI music video

Example: in the "Checklist" video, Aishyla was playing the saxophone in shots 3 and 12.
These steps replace her with a male saxophone player in just those shots. The other
shots and their videos stay as they are and are not paid for again.

**Cost:** about $0.07 per redrawn frame plus about $0.13 per re-animated shot
(Grok Imagine 1.5 Lite). For two shots: about $0.40.

## Why a special step is needed

The storyboard has a *lead character photo* (Aishyla). Every frame is drawn with that
photo, so any person in a shot comes out looking like her. To show someone else, you tell
RustCut that the lead is **not** in that shot.

## In the app

1. **Open the project.** File → Open project… →
   `Videos\RustCut AI\Checklist music video\Checklist.rcproj`.
2. **Open the storyboard.** AI → 🪄 Make a video from a prompt (storyboard)… →
   **📂 Open saved storyboard…** → `music-video-checklist\storyboard.json`.
   The shot cards appear with their current frames.
3. **Edit shot 3** (the first sax shot):
   - Change the description to describe the new person, for example:
     *"Medium silhouette of a stylish male tenor saxophonist in his thirties, dark suit and
     open-collar shirt, performing in the corner of a dimly lit, haze-filled jazz club under a
     single moody teal spotlight. No women in the shot."*
   - Untick **Lead appears in this shot**.
4. **Redraw it:** click **↻** on the card (≈ $0.07). Check the new frame. If it isn't right,
   adjust the description and click ↻ again.
5. **Repeat steps 3–4 for shot 12** (the second sax shot), e.g. *"Low-angle medium shot of
   a male saxophone player in his thirties, short beard, dark suit, leaning back mid-solo under
   an amber spotlight, smoke in the beam. No women in the shot."*
6. **Approve** the two new frames (or click **✔ Approve all drawn**).
7. **Animate and re-assemble.** The button now reads
   **🎬 Animate 2 new shots, reuse 14 & assemble (≈ $0.26)**. Click it.
   Only the changed shots are animated; the others are reused. The video is cut back onto
   the song's bars and replaces the previous version on the timeline (no duplicate track).
8. **Check it**, then **Export** (Ctrl+E) at 1080p for the final file.

Tips:
- Words like *"No women in the shot"* or naming the person's look help the image model.
- Editing a shot's description or motion, or redrawing it, marks only that shot for
  re-animation; everything else stays paid-for.
- Undo (Ctrl+Z) reverts the re-assembly on the timeline if you change your mind.

## With an AI assistant (MCP tools)

```text
storyboard_plan          open = ".../music-video-checklist/storyboard.json", draw_frames = false
storyboard_update_shot   shot = 3,  lead = false, description = "…male tenor saxophonist…", regenerate = true
storyboard_update_shot   shot = 12, lead = false, description = "…male saxophone player…",  regenerate = true
storyboard_update_shot   shot = 3,  approved = true      (and the same for 12)
storyboard_produce       model = "x-ai/grok-imagine-video-1.5-lite"
export_video             output = ".../Checklist - music video.mp4", resolution = 1080
```

`storyboard_produce` reports `reused_shots` (14 here) and the cost of the new ones.
