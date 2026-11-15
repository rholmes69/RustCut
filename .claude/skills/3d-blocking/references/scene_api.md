# Scene API (`from previz import *`)

Units are metres and seconds. Y is up. Angles are in degrees. A figure faces **-Z** at yaw 0, so a camera at -Z looks
at its front. Any argument marked "(t)" can be a constant or a function of time `lambda t: ...`.

## Scene file contract
```python
from previz import *
START, END = 0.0, 8.0                 # clip range in scene time (labels and timing show time - START)
NODES = [...]                          # every top-level node
SHOTS = [Shot('A', 0, 4, cam_a, 'Title', 'one-line action note'), Shot('B', 4, 8, cam_b, ...)]
LOOK = dict(bg=BG_DARK, fog=(BG_DARK, 45, 220))   # bg/fog may be functions of t; fog=None for none
def dynamic(t): return [...]           # optional: nodes rebuilt every frame
```
Shots must cover START to END. Each `cam` is `f(t) -> Cam`.

## Animation helpers
| helper | use |
|---|---|
| `ease(x, kind='inout')` | 0..1 easing; kind `'in'`, `'out'`, `'inout'`, `'linear'` |
| `track((t0, v0), (t1, v1), ..., kind=)` | keyframe track; values may be scalars or tuples; holds the ends |
| `val(x, t)` | evaluate a constant-or-function |
| `lerp(a, b, u)` | tuple lerp |
| `catmull(keys)` / `smoothed(fn, t0, t1, dims, sigma)` | spline plus time smoothing (used by the cameras) |
| `shake(t, amp, freq, seed)` | small 3D camera shake vector, for impacts only |
| `Beat(bpm, first)` | `.time(n)`, `.phase(t) -> (i, frac)`, `.hop(t, amp)`, `.snap(t, deg)` |
| `yaw_to(src, dst)` | yaw that makes a figure at src face dst |
| `world_pos(root, node, t)` | world position of any node inside root, e.g. `world_pos(hero, hero.hand_r, t)` |

## Primitives (all accept `pos, rot, scale, visible, name, emissive=False`)
| primitive | notes |
|---|---|
| `box(size, color, anchor='center' or 'bottom')` | `anchor='bottom'` puts pos at the bottom face |
| `cylinder(r, h, n, color, anchor='bottom', smooth=True)` | `smooth=False` gives faceted, flat-shaded sides (stages) |
| `sphere(r, n, color)` | |
| `torus(R, r, n, m, color)` | flat in XZ; `rot=(90,0,0)` stands it up facing -Z; `rot=(0,ry,90)` stands it up sideways |
| `ring_flat(r0, r1, n, color)` | flat floor ring (stage edge, shockwave) |
| `group(*children, pos, rot, scale, visible)` | transform hierarchy; children inherit |
| `Node(...)` | empty node, used as a marker |

`rot=(rx, ry, rz)` is applied as Ry·Rx·Rz, so `ry` (yaw) is always about world-up. Spin a prop with ry and tilt it
with rz or rx. `scale` can be a scalar or `(sx, sy, sz)`; use 0.001, not 0, for "invisible by scale".

## Figure
`figure(body=LEAD, head=HEAD, pos, yaw, scale, visible, height=1.76, name)` returns a group with markers:
- `fig.hand_r`, `fig.hand_l`: hip height, slightly forward. Hold props here.
- `fig.back`: between the shoulders.
- `fig.head_height`: the height of the head centre above the feet.

Colours: `LEAD` (dark hero), `EXTRA` (grey crowd and clones), `LIGHT_BODY` (cream second hero), `HEAD`, `MASK`.

## Effects
| effect | what it does |
|---|---|
| `hologram_in(fig, t0, color=TEAL, dur=0.45)` | flickering full-height shell, rising scan ring and spiral particles. Make the figure `visible` from `t0 + dur - 0.1`. No grow or bounce. |
| `shatter(p, t0, colors=(TEAL, PINK), n=24, spread=1)` | cube debris burst. Hide the figure at t0. |
| `beam_strike(p, t_hit, color=TEAL)` | beam drops from the sky onto p, plus two shockwave rings |
| `scaffold_building(x, z, w, h, t0, tiers)` | glowing scaffold draws up, the solid fills in, the scaffold blinks out |
| `city_ring(r_min, r_max, n, seed, t_build=None, keep_clear=fn)` | ring of block buildings; with `t_build`, each builds itself, nearest first |

## Set pieces (generic locations and props)
Every set piece returns a node (or a list), and its front faces -Z before yaw.

| piece | notes |
|---|---|
| `ground(size, color, y, tiles)` / `plane(...)` | tiled floor sitting 1cm below y, so roads, rugs and water laid on it never z-fight. Don't use a giant box as a floor. |
| `road(p0, p1, width, color, lines=YELLOW or None)` | p0/p1 are (x, z); `DIRT, lines=None` makes a dirt road |
| `fence(p0, p1, h)` | post-and-rail |
| `hill(x, z, r, h)` | rolling hill (flattened dome) |
| `tree(x, z, h, kind='round' / 'pine' / 'palm')`, `forest(x0, z0, x1, z1, n, keep_clear=fn(x, z))` | carve paths and camera lanes with keep_clear |
| `house(x, z, w, d, h, yaw, color, roof, porch, lit)` | gabled house; a red one with no porch reads as a barn |
| `building(x, z, w, d, h, color, yaw, lit, seed)` | city block with a window grid on its -Z face; `yaw=±90` faces a street running along Z |
| `car(pos, yaw, color, kind='sedan' / 'pickup' / 'van', lights)` | open cab, so drivers show. Markers: `.seat_l` / `.seat_r` (a seated figure of height ~1.0 goes at the marker's local position), `.tailgate` |
| `streetlight(x, z, h, arm, side, pool)` | pole, arm, lamp and a pool of light on the ground |
| `crowd(center, w, d, n, seed, face, beat, jump, sway)` | low-poly crowd facing `face`; `jump` may be a function of t (e.g. bigger after a drop) |
| `mic_stand`, `amp(stack)`, `drum_kit`, `guitar()`, `truss(cans, light)` | band gear; `attach(fig, guitar())` parents a prop to a figure |
| `room(w, d, h, wall, floor, open_sides=('-z',), ceiling)` | interior; leave a side open for the camera |
| `window`, `bed`, `couch`, `table`, `chair`, `floor_lamp`, `rug` | interior dressing |
| `sun(azimuth, elevation, dist, r)` | horizon sun disc; keep the fog end beyond `dist` |
| `cone(r, h)` | primitive used by pine trees |
| `attach(parent, *nodes)` | make nodes children of a figure or vehicle so they move together |

Extra colours: `GRASS, DIRT, ASPHALT, SIDEWALK, WOOD, FOLIAGE, BRICK, CREAM, METAL, BLACK, WARM, BLUE, SAND, WATER, FABRIC`.
Extra backgrounds: `BG_SUNSET, BG_NIGHT, BG_OVERCAST, BG_INTERIOR, BG_SKY`.

**Poses without limbs:**
- Seated: a shorter figure (`height` 1.0–1.1) placed on the seat.
- Lying down: wrap the figure in `group(figure(...), rot=(90, yaw, 0))`.
- Leaning or headbanging: small whole-figure pitch via a group `rot=(pitch, 0, 0)`.
- Colour can be a function of t, e.g. LED walls pulsing on the beat.

## Cameras
| camera | use |
|---|---|
| `Cam(pos, target, lens=35, roll=0, flash=0, fade=0)` | lens is the focal length in mm (18 = wide, 50 = tight); flash and fade blend to white or black (0..1) |
| `path_cam([(t, pos, target, lens), ...], sigma=0.22)` | free smooth path, ideal for oners |
| `orbit_cam([(t, theta, radius, height, target_y), ...], center, lens, sigma)` | orbits, cranes and overheads; radius 0.08 with a large height gives a straight overhead with no flip |
| `follow_cam(subject_fn, offset, look, lens, t0, t1, sigma)` | follows a moving subject: drone chase, car-side, walk-and-talk, leading shot; offsets are world-space (constant or f(t)) |
| `handheld(cam, amp=0.035, freq=5, roll=1.2)` | wraps any camera with handheld wobble |
| `whip(cam_a, cam_b, t_mid, dur=0.28)` | fast whip pan between two framings inside one Shot |
| `dolly_zoom(subject, direction, d0, d1, lens0, t0, t1)` | vertigo; the subject stays the same size while the background stretches |
| `Cam(..., flash=k)` + `shake(t)` | impact or drop: white flash and short shake (see `edm_festival.py`) |
| `orbit(theta, r, h, center)` | position on a circle; theta 0 = in front of a figure facing -Z, 90 = +X side |

`lens` in `orbit_cam` can be a track: `lens=track((0, 18), (8, 26))`.

## Palette
`FLOOR, FLOOR_LIGHT, STAGE, BUILDING, WALL, LEAD, EXTRA, LIGHT_BODY, HEAD, MASK, TEAL, PINK, YELLOW, RED, BG_DARK, BG_DAY`.
Any RGB tuple from 0 to 1 works too.

## CLI
```
python previz.py stills scene.py --times 0.5,2,4 [--size 480x270] [--out DIR]     -> DIR/stills.jpg
python previz.py check  scene.py                                                  -> lists pops and snaps
python previz.py render scene.py [--out DIR] [--name N] [--size 960x540] [--fps 24] [--procs N]
                        [--audio song.mp3 --audio-start SECONDS]
```
