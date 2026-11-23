---
name: transitions
description: Analyse a RustCut music video and add the best-fit transitions and effects, timed to the song's beat - dips to white into flashbacks, dips to black at act changes, dissolves inside memories, one zoom punch on the first hook, overlay fades, and beat-synced effects - then verify every edge lands on a beat. Use when the user asks for transitions, effects, polish, "make it flow", or for cuts and transitions to match the beat.
---

# Beat-matched transitions and effects

Proven on **Bond Money v2** (Aishyla, 130.4 BPM, 92 cuts): 16 transitions and 4 overlay fades, all edges within ~20 ms of a beat, applied for free through the MCP server. The worked example is in `example-bond-money.md`.

Set these first:
```bash
T=.claude/skills/transitions/transitions.py
D="C:/Users/sales/Videos/RustCut AI/<Title> music video"
P="$D/<Title>.rcproj"
SB="$D/music-video-<slug>/storyboard.json"
```

## Rules (always)
- **Free**, but it edits the project: **RustCut must be closed** (`tasklist | grep -i rustcut.exe`). Never kill it; ask the user to close it, and re-check right before editing (users reopen it between turns).
- **Back up first:** `cp "$P" "$D/<Title>.before-transitions.rcproj"`.
- **Recommend first, apply after the user agrees.** Give the table (step 3), then apply what they pick.
- **The old MCP server drops `scene_todos`** when it saves (it predates them). After `save_project`, copy any top-level keys missing from the backup back into the saved file (see step 5). Until `rustcut-mcp.exe` is rebuilt, always do this.

## 1. Read the cut (free)
```bash
python -I $T list "$P" --storyboard "$SB"        # add --from/--to (seconds) to zoom in
```
For every main-track cut it prints the time, the shot (storyboard description or file/folder), the lyric on screen, the beat position (`DOWN` = first beat of a bar) and the offset in ms to the nearest beat, plus flags: `OFF-BEAT`, `gap`, `under T2:<id>` (an overlay hides this cut) and any transition already set. The overlays are listed at the end.

**The beat grid** is the project's saved analysis, `project["music"][<song clip id>]` (`bpm`, `beats`). If it's missing, run `detect_beats` (MCP) on the song clip first.

**Offset sign:** the offset is **beat − cut**. `-192ms` means the beat is 192 ms *before* the cut. Bond Money's first pass read this backwards and moved the choir cut the wrong way; the `check` step caught it.

## 2. Map the story (free)
Using the list, the lyrics and the treatment (`theme.txt`), mark:
- **Time jumps into memory** (warm flashbacks) and back out.
- **Place or scene changes** (church → diner).
- **Section starts:** the first hook, verse 2, the bridge, the outro.
- **Big moments:** a performance or lip-sync centrepiece, the final shot.
- **Overlays** (`under T2:…`): cuts hidden under an overlay can't show a main-track transition; use a fade on the overlay instead.

## 3. Choose (the director's rules)
**Most cuts stay hard cuts on the beat.** That's the energy of a music video. Only change, time, place and mood get a transition: about one every 15–30 s, never two in a row except a deliberate memory chain.

| Moment | Transition | Length |
|---|---|---|
| Into a warm flashback or a big performance moment | **Dip to white** | 2 beats |
| Leaving a memory, a scene or an act; into the bridge or outro | **Dip to black** | 2 beats |
| Between memories, or a verse that opens as a memory | **Dissolve** | 2 beats |
| The first hook's downbeat (once) | **Zoom in** | 1 beat |
| The final shot (freedom, sunrise) | **Dip to white** | 4 beats |
| Story → end card or outro video | **Dip to black** | 4 beats |
| A deliberate contrast (warm memory → cold cell) | **Hard cut** | – |
| Overlay clips (performance takes, reshoots, title cards) | **fade_in / fade_out** on the overlay | ending or starting on a beat |

Avoid **slides and wipes** in emotional, story or R&B videos: they read as TV or vlog. They can suit a hype, comedy or social cut.

**Durations in whole beats** (beat = 60 / BPM). RustCut centres a transition on the cut, so with the cut on a beat, a 2-beat transition starts 1 beat before and ends 1 beat after: both edges on beats. A 1-beat zoom lands on the eighth notes either side, which is fine for a punch.

**Effects** (use sparingly, on the hook and climax):
- `beat_effect {clip_ids, effect: pulse, every: bar, amount: 0.3}` on a few hook shots: a zoom punch on each downbeat.
- `beat_effect … effect: flicker` on a flashback-into-reality cut, or `shake` on a police or impact shot (amount ≤ 0.4).
- `audio_react {clip_ids, effect: glow, follow: bass, amount: 0.3}` on a performance shot for a light breathing glow.
- `apply_motion_preset {clip_id, preset: ken_burns}` on static b-roll stills that feel frozen.
- Leave lip-sync takes alone (no shake, pulse or zoom): motion breaks the mouth sync read.

Give the user a table: time, from → to, transition, length, why. Note any off-beat cuts, and anything odd you spot (the last story shot undercutting the ending, stacked layers at 0:00).

## 4. Apply (free, after the user agrees)
1. Check RustCut is closed, back up, then `open_project` (the output is large; it's saved to a file, which is fine).
2. **Snap off-beat cuts first.** A transition on an off-beat cut can't land on beats.
   - Lip-synced clips: move the **start** later/earlier and change `source_in` by the same amount (`update_clip {start, source_in, duration}`), so the sync stays; keep the end.
   - Then stretch the neighbour to meet it. With no spare footage, change its `speed` slightly (e.g. 0.95 → 0.89); it doesn't show.
   - **Watch the microseconds.** If the moved clip overlaps a neighbour by even 0.00001 s, `update_clip` silently puts it on another track ("placed on a different track"), and its transition goes inactive. Compute the neighbours' exact ends from the project, leave a gap under 0.1 ms, pass `track_index` back, and check the response says the main track.
3. `set_transition {clip_id: <incoming clip>, kind, duration}` for each cut. If it says **"there is no clip directly before this one"**, there's a tiny gap (1 ms): extend the earlier clip's `duration` to touch, then retry.
4. Overlays: `update_clip {fade_in | fade_out}`, sized so the fade ends (or starts) on a beat: fade_in = next beat − clip start.
5. `render_frame` at a few transition centres to see them (a dip to white shows washed-out white at the cut).
6. `save_project`, then restore the dropped keys:
   ```bash
   cd "$D" && node -e 'const fs=require("fs");const b=JSON.parse(fs.readFileSync("<Title>.before-transitions.rcproj","utf8"));const p=JSON.parse(fs.readFileSync("<Title>.rcproj","utf8"));const lost=Object.keys(b).filter(k=>!(k in p));for(const k of lost)p[k]=b[k];fs.writeFileSync("<Title>.rcproj",JSON.stringify(p,null,2));console.log("restored",lost)'
   ```

## 5. Verify (free) and report
```bash
python -I $T check "$P"
```
Every transition's start and end, and every overlay fade edge, in ms from a beat. **Under ~40 ms reads as on the beat.** It also flags gaps that stop a transition from applying. A transition into an end card after the song ends will show a large end offset; that's expected.

Report: what was applied (grouped by kind), the check numbers, any cut you snapped and how, the backup name, and that the to-do list was restored. Offer the next polish (beat effects on the hook, the ending fix).

**Deliver:** after a final re-export, offer the user a Google Drive backup (video and project zip), a YouTube upload with the details filled in from the project, and a release plan on Google Calendar: see `/music-video` step 13. Ask before each; they act on the user's real accounts.
