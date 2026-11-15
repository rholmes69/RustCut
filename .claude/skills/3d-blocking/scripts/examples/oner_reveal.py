"""Example 1 - ONE continuous shot (11s): bright room -> through a screen -> dark world builds -> hero materializes.
Shows: path_cam (smoothed oner), a prop interaction (button press), passing through an opening, city build with
scaffolding, beam strike + hologram reveal, two "worlds" with their own background/fog.

Render:  python ../previz.py render oner_reveal.py --out out
"""
import math
from previz import *

START, END = 0.0, 11.0
T_THROUGH = 4.1                     # camera passes through the screen into the dark world

# ---------------------------------------------------------------- bright room (z < 0), back wall at z = 0 with a screen opening
HOLE = (1.4, 1.0, 1.55)             # width, height, centre-y of the opening the camera flies through
room = [box((12, 0.1, 10), FLOOR_LIGHT, pos=(0, -0.05, -5)),
        box((6 - HOLE[0] / 2, 4, 0.2), WALL, pos=(-(6 + HOLE[0] / 2) / 2, 0, 0), anchor='bottom'),     # wall left of the opening
        box((6 - HOLE[0] / 2, 4, 0.2), WALL, pos=((6 + HOLE[0] / 2) / 2, 0, 0), anchor='bottom'),      # wall right of the opening
        box((HOLE[0], HOLE[2] - HOLE[1] / 2, 0.2), WALL, pos=(0, 0, 0), anchor='bottom'),
        box((HOLE[0], 4 - HOLE[2] - HOLE[1] / 2, 0.2), WALL, pos=(0, HOLE[2] + HOLE[1] / 2, 0), anchor='bottom'),
        box((0.2, 4, 10), WALL, pos=(-6, 0, -5), anchor='bottom'), box((0.2, 4, 10), WALL, pos=(6, 0, -5), anchor='bottom'),
        box((12, 0.2, 10), (0.9, 0.9, 0.9), pos=(0, 4, -5)),
        box((1.6, 1.0, 0.45), (0.25, 0.25, 0.28), pos=(0, 0, -0.35), anchor='bottom', name='console')]   # console under the screen
press = track((1.62, 1.0), (1.72, 0.35), (1.95, 0.35), (2.1, 1.0))
room.append(cylinder(0.07, 0.06, 16, RED, pos=(-0.3, 1.0, -0.4), scale=lambda t: (1, press(t), 1)))       # the big red button
# the screen itself: a glowing panel filling the opening; it switches off the instant the camera reaches it
room.append(box((HOLE[0], HOLE[1], 0.04), TEAL, pos=(0, HOLE[2] - HOLE[1] / 2, -0.02), anchor='bottom', emissive=True,
                visible=lambda t: cam(t).pos[2] < -0.12))

# ---------------------------------------------------------------- the player walks up to the console and faces it
walk = track((0.0, (-3.4, 0, -6.0)), (1.1, (-0.6, 0, -1.15)))     # stands left of centre, so the camera can pass her
player = figure(LIGHT_BODY, HEAD, pos=lambda t: walk(t),
                yaw=lambda t: 180 + (-60 * (1 - ease((t - 0.9) / 0.35)) if t < 1.25 else 0), name='player')

# ---------------------------------------------------------------- the dark world beyond the wall (centre = stage)
W0 = (0.0, 0.0, 25.0)
world = [plane(196, FLOOR, -0.01, 16, center=(0, 98.2)),
         cylinder(8, 0.4, 40, STAGE, smooth=False, pos=W0, scale=lambda t: (track((5.0, 0.001), (5.8, 1))(t), 1, track((5.0, 0.001), (5.8, 1))(t)))]
world += [group(*city_ring(16, 70, 28, t_build=3.5, keep_clear=lambda x, z, w: z < -15 or (abs(x) < w + 6 and z < 0)), pos=W0)]
T_HIT, T_SOLID = 7.6, 8.35
hero = figure(LEAD, HEAD, pos=(0, 0.4, 25), yaw=0, visible=lambda t: t >= T_SOLID - 0.1, name='hero')
fx = beam_strike((0, 0.4, 25), T_HIT) + hologram_in(hero, T_SOLID - 0.45, TEAL)

NODES = room + [player] + world + [hero] + fx

# ---------------------------------------------------------------- the camera: ONE smooth continuous move
cam = path_cam([
    (0.0,  (-1.2, 0.45, -8.4), (-3.0, 1.0, -5.6), 22),     # low tracking as she walks in
    (1.1,  (0.6, 0.6, -4.4),   (-0.6, 1.0, -1.3), 24),
    (1.85, (0.75, 1.45, -1.25), (-0.3, 1.0, -0.4), 30),    # swoop in on the button from her right as she presses it
    (2.9,  (1.3, 1.95, -3.6),  (0.0, 1.55, 0.0), 30),      # rise and pull back over her right shoulder
    (3.6,  (0.35, 1.7, -1.7),  (0.0, 1.55, 0.6), 30),      # push toward the glowing screen
    (T_THROUGH, (0.0, 1.55, 0.4), (0.0, 1.2, 6.0), 26),    # through the screen opening
    (5.6,  (0.6, 9.0, 9.0),    (0.0, 1.0, 25.0), 24),      # high over the dark world as it builds
    (7.6,  (1.0, 3.2, 17.0),   (0.0, 1.6, 25.0), 26),      # glide down toward the stage for the beam
    (9.4,  (-0.8, 2.1, 21.6),  (0.0, 1.95, 25.0), 34),
    (11.0, (-0.25, 2.0, 23.0), (0.0, 1.95, 25.0), 45),     # settle into a close-up
], sigma=0.18)

SHOTS = [Shot('ONER', START, END, cam, 'Continuous reveal',
              'Walk in -> press the button -> over the shoulder -> through the screen -> world builds -> beam + hologram -> close-up')]

inside = lambda t: cam(t).pos[2] < 0          # which side of the screen wall the camera is on
LOOK = dict(bg=lambda t: BG_DAY if inside(t) else BG_DARK,
            fog=lambda t: None if inside(t) else (BG_DARK, 40, 200))
