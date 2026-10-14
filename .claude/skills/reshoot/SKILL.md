---
name: reshoot
description: Reshoot a single clip on a RustCut timeline from a new prompt — draw a new still (keeping the storyboard's style, cast and the shot's look), get the user's approval, animate it and swap it in for just that cut, then redo lip-sync if the clip had it. Use when the user wants to redo, regenerate, replace or fix a shot or scene.
---

# Reshoot a clip in RustCut

Only the chosen cut changes. If its storyboard shot is used for several cuts (common in music videos), the others stay as they are. To change every use of a shot, use the storyboard instead (AI → storyboard → Open saved storyboard → edit the shot → ↻ → animate), or the `/music-video` redraw stage.

## Rules
- **Ask before spending.** A still costs ≈ $0.07 and animation ≈ $0.14 for ~4 s with the default video model. Redoing a performance lip-sync costs about (clip length + 1 s) × $0.24.
- Always show the new still and get approval before animating.
- Warn the user before changing a project that's open in RustCut.

## Steps (MCP)
1. **Find the clip:** `get_project`, then match by time (e.g. "the rain shot at 1:54") or by media name (`shot-16-video.mp4`).
2. **Agree on the prompt.** `reshoot_clip` pre-fills the description and motion from the storyboard shot, so ask the user what should change and write a full description: who, where, framing, action, mood. Name cast members (e.g. "Aishyla") so their reference photos are used. Leave names out for shots without them, or pass `lead: false`.
3. **Draw the still:** `reshoot_clip {clip_id, description, motion, still_only: true}` (`keep_look` defaults to true, which keeps the current wardrobe, setting and colours). It returns the still's path, its cost and the source info (`storyboard_shot`, `uses_of_shot`, `was_lip_synced`).
4. **Show the still** to the user. Redraw with an adjusted description if needed (each ≈ $0.07).
5. **Animate and replace:** `reshoot_clip {clip_id, still: "<approved still path>", motion}` → the new footage takes the cut's exact place and length, look and volume.
6. **If `was_lip_synced`:** offer to redo the lip-sync with `/lip-sync` (`performance_shot` on the new clip id), with a dry run first.
7. `save_project`, and report the total cost.

Reshoot files go in `<project folder>/RustCut AI/reshoots/clip-<id>-<n>/`, outside the storyboard folder, so re-assembling the storyboard never removes them.

## When the MCP server doesn't have `reshoot_clip`
`rustcut-mcp.exe` predates the tool (it needs a rebuild, which isn't possible while RustCut, SonicLens or a Claude session is running it). Guide the user through the app instead: right-click the clip → **🎬 Reshoot this clip (AI)…** → edit the description and motion → **🎨 Draw new still** → **🎬 Animate & replace this clip**. Tick **Redo the lip-sync performance afterwards** when it appears.
