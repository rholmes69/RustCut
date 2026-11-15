# Lessons: problems that came up in real reviews and how to fix them

Read before the first render. Each one cost an iteration once.

## Look
- **Too detailed.** Textures, grids, glow, arms and earrings make the AI copy the wrong things. Fix: block figures,
  flat colours, simple sets. Keep only what changes motion, camera, framing or pacing.
- **No limb animation**, not even "just one arm for the throw". The AI animates the body; a block arm confuses it. For
  a prop in hand, put the prop at `fig.hand_r` and let the AI imagine the arm.
- **Effects too big.** Oversized accessories and effects read as objects. Keep effects short and around body scale.

## Motion
- **Bouncing.** Figures that grow with an overshoot look like they bounce. Holograms appear at full height
  (`hologram_in` does this) and there are no squash-and-stretch springs.
- **Pops and jitter.** A rotation that snaps (a disc going from flat to upright in one frame), a scale that snaps
  back (a crouch ending) or a wrapped angle (360 → 0) is visible as a pop. Fix: keep rotations continuous (gradual
  tilts), hold the end value (`y += 360` stays), and run `previz.py check`.
- **Too fast.** If the user says a reveal or pop-up is too fast, slow it by 30–50% or add a second; don't just ease
  harder.
- **Nervous, not dramatic.** Small glances (±15°) read as nervous. A crouch or big squash reads as a glitch.
- **Spin only when it means something.** A prop forming in the hand should scale up without spinning. It spins only
  in flight, then the spin decays after the catch.

## Camera
- **Overhead flip.** A camera looking straight down with a fixed world-up flips at the top. The engine blends up
  toward the travel direction, so `orbit_cam` to radius 0.08 is safe. Don't force `up` yourself.
- **"Weird cut" inside a oner.** It is usually a camera speed spike or a direction reversal at a key. Smooth it
  (sigma 0.2–0.25) and space the keys evenly in time.
- **Head too high or low in frame.** Adjust `target_y` (orbit_cam) or the target y, not the lens. To show bodies
  popping in behind the hero, raise the camera slightly and pull the radius out.
- **Rotation too much.** Orbits beyond ~120° in 3s feel like a merry-go-round. Halve the sweep.
- **Starts on black.** Don't fade in from black unless asked; the first frame is the AI's first reference image.

## Physics and layout
- **Floating props and buttons.** Seat props on the surface they belong to; compute the surface height (including
  slopes) and place the prop there.
- **Hands through objects.** Put interaction targets within reach of the hand markers and verify with `world_pos`.
  Print the distances.
- **Left/right mirrored.** Confirm the user's left and right relative to the character (a figure faces -Z, so its
  right hand is at +X). Joystick left and buttons right means from the player's point of view.
- **Walk-to-stop pops.** A walk that eases from zero speed or stops abruptly looks robotic. Use a constant pace,
  then a deceleration whose starting speed matches the pace (see `walk_z` in `rap_street.py`).
- **Prop height.** Hold props at the hip (`hand_r`), not at chest or head height, unless the action calls for it.

## Rendering artefacts
- **Striped or flickering ground.** This is z-fighting: two surfaces in the same plane, or a giant box whose
  underside fights its top at distance. Use `ground()` / `plane()` (a tiled top-only floor 1cm down) and lay flat
  pieces slightly above it.
- **Hidden driver or performer.** Solid boxes hide whatever is inside. Vehicles have open cabs; for booths or desks,
  keep them waist height and raise the camera to look down.
- **Dolly zoom does nothing.** It needs depth cues behind the subject (wall art, shelves, trees). A plain wall shows
  no change.
- **Subject drops out of frame during a swing.** When the camera swings toward a reveal, aim the target between the
  subject and the reveal, add a middle key, and check stills every 0.5s through the swing.
- **Wide lens next to crowds or crews.** People 1–2m from a 16mm lens become giant blobs. Orbit at a larger radius,
  or keep the orbit on the side away from them.

## Workflow
- Show a still contact sheet before any full render, and look at the rendered video before delivering it.
- Deliver clean and labeled versions (plus no-music when there is a song) and a timing sheet.
- When asked for a prompt, use the user's own prompt structure if they have one; change only what the new blocking
  changed.
