# Camera moves catalogue

The figure is at the origin and faces -Z, so the front is at -Z. Heights are in metres and lens is in mm. Each recipe
can be pasted as is, then tuned with stills. For a oner, chain recipes into one `path_cam` / `orbit_cam` key list.
Never cut between two moves of one continuous shot.

| Move | Feel | Recipe |
|---|---|---|
| **Low front dolly-in** | hero intro, power | `Cam(lerp((0.3,0.6,-5.4),(0.2,1.05,-4.0),u),(0,1.3,0.6),26)` |
| **Low tracking** | walking, following | `path_cam` with pos 0.4–0.6m high, 2–4m to the side and slightly behind, target at chest height, lens 22–24 |
| **Side truck** | dance line, reveal a group | `Cam(lerp((-4,1.3,-4.6),(3.6,1.4,-4.4),u), lerp((-1.6,1.2,.4),(1.6,1.2,.4),u), 26)` |
| **Low orbit** | performance, swagger | `Cam(orbit(-70+75*u, 5.4-0.6*u, 0.75+0.3*u), (0,1.4,0.4), 22)` |
| **Close inside orbit** | tension, surrounded | `orbit_cam([(0,160,2.3,1.0,1.75),(3,105,2.2,1.4,1.5)], lens=18)`: inside a ring of clones |
| **Crane to overhead** | resolve, reveal the pattern | end the key list with `(t, theta, 0.08, 7–11, 0.4)`; the continuous up vector means no flip |
| **Rotating overhead** | formations | `Cam((0.06*sin(a), 11-u, 0.3-0.06*cos(a)), (0,0,0.3), 32)` with `a` from 0 to 30° |
| **Swoop to a prop** | interaction close-up | a path_cam key 1m from the prop, lens 30–34, at the moment of contact (button press) |
| **Over the shoulder** | POV setup | key 0.4–0.8m behind and above one shoulder, target past the figure, lens 28–30 |
| **Push through an opening** | world transition | keys on the axis of the opening: 2m before, then 0.4m past it; switch bg and fog on the camera's side (`cam(t).pos`) |
| **High establishing to descent** | world build, arrival | high key (8–10m up, 15m back), then glide down toward the hero over 2–3s |
| **Dolly zoom** | vertigo, realisation | move the camera back while raising the lens so the subject stays the same size: `dist*lens` constant |
| **Track a thrown prop** | action | orbit around the centre in the same direction as the prop, 4–6m out and 3–4m up; don't lock onto the prop itself (too jerky) |
| **Drone chase** | road movie, epic | `follow_cam(vehicle_pos, offset=lerp((2,11,18),(2.5,5.5,12),u), look=(0,0.8,-10), lens=24)` |
| **Car-side tracking** | driver performance | `follow_cam(vehicle_pos, offset=(-4.4,1.6,0.8), look=(-0.45,1.4,-0.5), lens=28)`: matches the car's speed |
| **Leading shot** | walk toward camera, swagger | path_cam keys 3m in front of the walker, 0.6m high, lens 14–18, generated from the walk function |
| **Foreground-wipe side track** | indie, nature, hidden cuts | camera lane parallel to the path with tree trunks (or pillars, people) between them; each trunk is a natural cut point |
| **Whip pan** | rock energy, transitions | `whip(cam_a, cam_b, t_mid, 0.28)` inside one Shot |
| **Handheld** | rock, rap, documentary | `handheld(any_cam, 0.03–0.05)`; add `roll=3` for low, aggressive angles |
| **Drone fly-in over a crowd** | festival build-up | path_cam from 25m back and 7m up, gliding down to just over the heads by the drop |
| **Jib down onto a performer** | DJ, singer reveal | rise above, then descend toward the performer with a tighter lens (30 → 40) |
| **Crash push-in** | final hit | `ease(u, 'in')` push of 4m in one bar plus a lens increase plus handheld |
| **Handheld impact** | hits | add `shake(t, 0.03)` to pos for 0.2–0.3s after an impact only |

## Rules of thumb
- **Speed**: no more than ~3 m/s for a camera near a person, and around 90°/s for an orbit. Smoothing (sigma
  0.18–0.25) removes speed spikes at the keys. If a still sequence shows a big jump between neighbours, add a key
  or lengthen the move.
- **Framing**: keep a head 15–30% from the top of frame in mids. Lower the target_y or raise the camera if the head
  rides too high.
- **Lens**: 18–22 is wide and dynamic, 26–34 is natural, 40–50 is a close-up. Wide lenses near figures distort; keep
  them for low and dramatic moves.
- **Cuts**: put them on bars or beats. Change the angle by at least 30° or the size by a full step between cuts.
- **Don't** pass the camera through bodies, walls or props. Route it around them, or move the figure.
