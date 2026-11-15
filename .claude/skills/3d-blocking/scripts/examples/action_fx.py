"""Example 3 - ACTION / FX beat in ONE continuous shot (8s): clones surround the hero, a glowing disc forms in her
hand, she throws it, it rides the ring and shatters every clone, comes back and she catches it; the camera starts
low inside the ring and cranes all the way up to a straight overhead.
Shows: orbit_cam crane-to-overhead (no snap), hologram pop-ins ranked by angle, a prop locked to fig.hand_r with
world_pos (physics contact), continuous prop rotations (no flips), a timed shatter chain.

Render:  python ../previz.py render action_fx.py --out out
"""
import math
from previz import *

START, END = 0.0, 8.0
STAGE_Y = 0.4
T_POP, POP_GAP, HOLO = 0.35, 0.12, 0.45     # clones pop in one after another
T_FORM, T_TILT, T_THROW = 2.0, 2.55, 2.95    # disc forms (upright), tilts flat, leaves the hand
T_BACK = 5.9                                 # disc returns to her hand (catch)
R_RING = 3.3
e = lambda x: ease(min(1.0, max(0.0, x)))

# ---------------------------------------------------------------- hero: two small nervous glances, otherwise still
glance = track((0.9, 0), (1.25, -16), (1.6, -16), (1.95, 14), (2.25, 14), (2.6, 0))
hero = figure(LEAD, HEAD, pos=(0, STAGE_Y, 0), yaw=lambda t: glance(t), name='hero')

# ---------------------------------------------------------------- the disc: path = hand -> out to the ring -> once round -> back to hand
hand = lambda t: tuple(world_pos(hero, hero.hand_r, t))
A0 = 35.0                                                          # angle where the disc meets the ring (front-right)
ang = track((T_THROW + 0.35, A0), (T_BACK - 0.35, A0 - 360))      # clockwise seen from above
rad = track((T_THROW, 0.5), (T_THROW + 0.4, R_RING), (T_BACK - 0.4, R_RING), (T_BACK, 0.5))


def flight(t):
    a = math.radians(ang(t)); r = rad(t)
    return (r * math.sin(a), STAGE_Y + 1.0, -r * math.cos(a))


def disc_pos(t):
    if t < T_THROW: return hand(t)
    if t > T_BACK: return hand(t)
    k = min(e((t - T_THROW) / 0.25), e((T_BACK - t) / 0.25))     # blend out of / back into the hand: no pop
    return lerp(hand(t), flight(t), k)


def disc_rot(t):
    tilt = 90 * (1 - e((t - T_TILT) / 0.35)) if t < T_BACK else 90 * e((t - T_BACK - 0.1) / 0.4)   # upright <-> flat, gradual
    if t < T_THROW: spin = 0.0
    elif t < T_BACK: spin = 900 * (t - T_THROW)                                  # 900 deg/s in flight
    else:
        d = min(t - T_BACK, 1.0); spin = 900 * (T_BACK - T_THROW) + 900 * d - 450 * d * d   # spin decays to a stop after the catch
    return (0, glance(t) + spin, tilt)


grow = track((T_FORM, 0.001), (T_FORM + 0.45, 1.0), kind='out')  # forms by scaling up - NO spin while forming
disc = torus(0.33, 0.045, 32, 6, TEAL, pos=disc_pos, rot=disc_rot, scale=lambda t: grow(t), emissive=True,
             visible=lambda t: t >= T_FORM, name='disc')

# ---------------------------------------------------------------- clone ring: pop in (pink holograms), shatter when the disc passes
ANGLES = [22.5 + 45 * i for i in range(8)]
order = sorted(ANGLES, key=lambda a: abs(((a - 180) + 180) % 360 - 180))   # start behind her, spread round to the front
clones, fx = [], []
for a in ANGLES:
    p = (R_RING * math.sin(math.radians(a)), STAGE_Y, -R_RING * math.cos(math.radians(a)))
    t_in = T_POP + POP_GAP * order.index(a)
    # the moment the disc's angle passes this clone (disc goes A0 -> A0-360)
    t_hit = next(t for t in [T_THROW + 0.35 + i / 240 for i in range(2000)] if ang(t) <= a - (360 if a > A0 else 0) + 1e-6)
    c = figure(EXTRA, MASK, pos=p, yaw=yaw_to(p, (0, 0, 0)), visible=lambda t, t_in=t_in, t_hit=t_hit: t_in + HOLO - 0.1 <= t < t_hit,
               name=f'clone{int(a)}')
    clones.append(c)
    fx += hologram_in(c, t_in, PINK, dur=HOLO)
    fx += shatter((p[0], p[1] + 0.9, p[2]), t_hit, colors=(PINK, TEAL), seed=int(a))

set_ = [ground(200, FLOOR), cylinder(6.5, STAGE_Y, 48, STAGE, smooth=False),
        ring_flat(6.3, 6.5, 64, TEAL, pos=(0, STAGE_Y + 0.01, 0), emissive=True)] + city_ring(16, 75, 30, seed=9)
NODES = set_ + [hero, disc] + clones + fx

# ---------------------------------------------------------------- ONE continuous move: low orbit inside the ring -> pull out -> overhead
cam = orbit_cam([
    (0.0, 160, 2.3, 1.0, 1.75),       # (t, theta, radius, height, target_y): low, inside the ring, behind her
    (2.0, 105, 2.2, 1.4, 1.5),        # swing round to her right side as the disc forms at her hip
    (2.95, 70, 2.5, 1.7, 1.35),       # front-right on the throw, start pulling out
    (4.4, -10, 5.6, 4.0, 0.9),        # follow the disc round the ring (same direction) from outside and above
    (6.2, -60, 3.0, 6.5, 0.5),
    (8.0, -90, 0.08, 7.5, 0.4),       # straight overhead (continuous up vector: no flip)
], center=(0, 0, 0), lens=track((0, 18), (3, 20), (8, 26)), sigma=0.25)

SHOTS = [Shot('FX', START, END, cam, 'Clone ring + disc throw',
              'Clones pop in around her -> disc forms at her hip -> throw -> disc rides the ring shattering each clone -> catch -> overhead')]
LOOK = dict(bg=BG_DARK, fog=(BG_DARK, 45, 220))
