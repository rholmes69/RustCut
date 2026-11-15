# Genre playbook: environment, camera language and cutting by music style

Starting points, not rules. Pick the row closest to the brief and copy the matching example scene, then change the
set, the beats and the moves to fit the user's story. Mixing rows is fine, e.g. a rap track in a desert, or a
country ballad in a bedroom.

| Genre | Typical locations | Camera language | Cutting / pace | Figure motion (whole body only) | Example |
|---|---|---|---|---|---|
| **Pop / K-pop** | stage, neon set, studio cyc, sci-fi city | low dolly-ins, side trucks on formations, rotating overheads, low orbits | a cut every 1–2 bars, hard on the downbeat | formations (V → line → diamond), synced snaps and spins, hologram pop-ins | `music_video_cuts.py`, `action_fx.py`, `oner_reveal.py` |
| **Country** | dirt road, fields, porch, pickup tailgate, barn, lone tree, sunset | drone chase, car-side tracking, low push-in with the sun behind, crane-up reveal of the land | slow: 3–4s shots, cut on the phrase | driving, sitting, slow walk, gentle sway | `country_roads.py` |
| **Rock / punk** | club stage, garage, rehearsal room, rooftop | handheld wide over the crowd, low-angle singer, whip pans between players, overhead drums, from behind the band into the crowd, crash push-in | fast: a cut every bar or half bar, hits on the snare | band bounce and lean on the beat, crowd jumps | `rock_club.py` |
| **Rap / hip-hop** | night street, block party, parking lot, car, rooftop, stairwell | low wide-lens leading shots (camera backs away), low orbits round the crew, car-mount, crane up high, sometimes handheld | oners or quick cuts; land the camera move on the bar | swagger walk, crew framing behind the lead, small beat sways | `rap_street.py` |
| **R&B / ballad** | bedroom, apartment window, bathtub, empty theatre, rain | slow push-ins, dolly zoom, top-down on a bed, slow pans, locked-off wides | long takes of 4–8s, few cuts | almost still: stand, sit, lie down, small turns | `ballad_apartment.py` |
| **EDM / festival** | festival stage, warehouse rave, DJ booth, LED walls | drone fly-in over the crowd, jib swing onto the DJ, flash and shake on the drop, speed ramps | build-up = one long move; drop = flash plus faster cuts or bigger motion | crowd sway then jump at the drop, DJ bounce | `edm_festival.py` |
| **Indie / folk** | forest path, lake, field, small town street, cabin | side-tracking with foreground trees (natural wipes), slow rise and reveal, handheld close | oners or slow cuts; foreground wipes double as hidden cuts | walking, stopping, looking out | `indie_forest.py` |
| **Metal** | warehouse, cave, burning field, dark stage | low wide angles, fast push-ins, strobing (flash), circular dolly round the band | very fast, many cuts per bar | headbang = small forward pitch of the whole figure on the beat | adapt `rock_club.py` |
| **Latin / reggaeton** | street party, beach, rooftop pool, colourful town | handheld follow, low dance angles, crowd circles, golden-hour drone | medium-fast cuts on the dembow pattern | partner turns, group circles, hip sway as yaw | adapt `music_video_cuts.py` + `crowd()` |
| **Lo-fi / bedroom pop** | desk, bedroom, bus window, rooftop at dusk | locked-off wides, slow push, top-down desk | long takes | sitting, small head turns | adapt `ballad_apartment.py` |

## Building a location from the set-piece library
- **Rural / outdoor**: `ground(GRASS)`, `road(..., DIRT, lines=None)`, `fence`, `hill`, `tree` / `forest`, `house`
  (a red one doubles as a barn), `sun`, `car(kind='pickup')`. Use `LOOK = dict(bg=BG_SUNSET, fog=(BG_SUNSET, 110, 520))`.
- **City night**: `road(ASPHALT, lines=YELLOW)`, sidewalks as boxes, `building(..., lit=True, yaw=±90)` facing the
  street, `streetlight` every 13–16m, parked `car`s plus one moving car with `lights=True`.
  Use `bg=BG_NIGHT, fog=(BG_NIGHT, 30, 120)`.
- **Club / stage**: `room(open_sides=('-z',))`, a stage box, `truss`, `amp`, `drum_kit`, `mic_stand`,
  `crowd(beat=...)`, and `attach(fig, guitar())`.
- **Festival**: a stage deck, an LED wall box with `color=lambda t: ...`, sweeping thin emissive boxes for light
  beams, speaker stacks, and `crowd(n=100+)`.
- **Interior**: `room(...)` with the camera on the open side, plus `window`, `bed`, `couch`, `table`, `chair`,
  `floor_lamp`, `rug`. Add wall art or shelves as depth cues, which the dolly zoom needs.
- **Nature**: `forest(keep_clear=...)` to carve the path and the camera lane, explicit foreground `tree`s for wipes,
  water as a flat `cylinder` with a `SAND` ring, and `bg=BG_OVERCAST` with fog for depth.

## Reading the song
- Get the BPM and a downbeat. Bars = 4 beats. Cut on bars or phrases (4 or 8 bars) for slow genres, and on bars or
  half bars for fast ones.
- Mark the structure: intro, verse, pre-chorus (build), chorus or drop. Put the biggest camera move or reveal on
  the chorus or drop downbeat.
- Slow songs (60–80 BPM) suit long takes and slow moves. Fast songs (120–170 BPM) suit short shots and energetic
  moves. Don't let the camera speed fight the tempo.
