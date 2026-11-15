"""Example 2 - MULTI-SHOT music video section (10s, 4 cuts of 2.5s): a lead + 4 backup dancers on a stage.
Shows: Beat-synced whole-body choreography (snaps, hops, a group spin - no limbs), formation changes
(V -> line -> diamond), hologram pop-ins for the backups, and four different camera moves cut on the bar.

Render:  python ../previz.py render music_video_cuts.py --out out --audio song.mp3 --audio-start 40
(--audio-start = where in the song this section begins; set BPM / FIRST_BEAT below from the song.)
"""
import math
from previz import *

START, END = 0.0, 10.0
BPM, FIRST_BEAT = 117.45, 0.0          # from the song: tempo and the time (in this clip) of a downbeat
beat = Beat(BPM, FIRST_BEAT)
STAGE_Y = 0.4
e = lambda x: ease(min(1.0, max(0.0, x)))

# ---------------------------------------------------------------- formations: (x, z) for [lead, L2, L1, R1, R2]; all face -Z (camera front)
V_SHAPE = [(0, 0), (-2.6, 1.2), (-1.3, 0.6), (1.3, 0.6), (2.6, 1.2)]
LINE    = [(0, 0), (-2.4, 0), (-1.2, 0), (1.2, 0), (2.4, 0)]
DIAMOND = [(0, -0.6), (-1.6, 0.4), (-0.9, 1.5), (0.9, 1.5), (1.6, 0.4)]
MOVES = [(0.0, V_SHAPE), (4.6, V_SHAPE), (5.6, LINE), (7.2, LINE), (8.2, DIAMOND)]   # (time, formation) - eased between


def place(k, t):
    for (ta, fa), (tb, fb) in zip(MOVES, MOVES[1:]):
        if t < tb:
            u = e((t - ta) / (tb - ta)) if fa is not fb else 0
            return fa[k][0] + (fb[k][0] - fa[k][0]) * u, fa[k][1] + (fb[k][1] - fa[k][1]) * u
    return MOVES[-1][1][k]


def yaw(k, t):
    y = beat.snap(t, 25)                                              # everyone snaps left/right on the beat, in sync
    if t >= 8.4: y += 360 * e((t - 8.4) / 0.8)                        # group spin (stays at 360 after: no wrap-back pop)
    if k and 2.5 <= t < 3.5:                                          # backups ripple-turn toward the lead, one after another
        d = 0.12 * [0, 2, 1, 1, 2][k]
        y += (30 if place(k, t)[0] < 0 else -30) * math.sin(math.pi * e((t - 2.5 - d) / 0.6))
    return y


POP = {1: 0.55, 4: 0.75, 2: 0.95, 3: 1.15}                         # backups pop in as holograms during shot 1
dancers, fx = [], []
for k in range(5):
    body = LEAD if k == 0 else EXTRA
    head = HEAD if k == 0 else MASK
    vis = True if k == 0 else (lambda t, k=k: t >= POP[k] + 0.35)
    f = figure(body, head, pos=lambda t, k=k: (place(k, t)[0], STAGE_Y + beat.hop(t, 0.03), place(k, t)[1]),
               yaw=lambda t, k=k: yaw(k, t), visible=vis, name=f'dancer{k}')
    dancers.append(f)
    if k: fx += hologram_in(f, POP[k], PINK)

set_ = [ground(200, FLOOR),
        cylinder(6.5, STAGE_Y, 48, STAGE, smooth=False),
        ring_flat(6.3, 6.5, 64, TEAL, pos=(0, STAGE_Y + 0.01, 0), emissive=True)]
set_ += city_ring(16, 75, 30, seed=4)
NODES = set_ + dancers + fx


# ---------------------------------------------------------------- four shots, each its own smooth move (u = 0..1 through the shot)
def shot_cam(t0, t1, fn):
    return lambda t: fn(e((t - t0) / (t1 - t0)) * 0.5 + 0.5 * (t - t0) / (t1 - t0))   # half eased, half linear: never stalls


cam1 = shot_cam(0.0, 2.5, lambda u: Cam(lerp((0.3, 0.6, -5.4), (0.2, 1.05, -4.0), u), (0, 1.3, 0.6), 26))           # low front dolly-in + crane
cam2 = shot_cam(2.5, 5.0, lambda u: Cam(lerp((-4.0, 1.3, -4.6), (3.6, 1.4, -4.4), u), lerp((-1.6, 1.2, 0.4), (1.6, 1.2, 0.4), u), 26))  # side truck
cam3 = shot_cam(5.0, 7.5, lambda u: Cam((0.06 * math.sin(math.radians(30 * u)), 11.0 - 1.0 * u, 0.3 - 0.06 * math.cos(math.radians(30 * u))),
                                        (0, 0, 0.3), 32))                                                              # rotating overhead
cam4 = shot_cam(7.5, 10.0, lambda u: Cam(orbit(-70 + 75 * u, 5.4 - 0.6 * u, 0.75 + 0.3 * u, (0, 0, 0.4)), (0, 1.4, 0.4), 22))  # low orbit

SHOTS = [
    Shot('D1', 0.0, 2.5, cam1, 'Low front dolly', 'Lead centre; backups pop in as pink holograms into a V; everyone snaps left/right on the beat'),
    Shot('D2', 2.5, 5.0, cam2, 'Side truck', 'Camera slides along the front at chest height; backups ripple-turn toward the lead'),
    Shot('D3', 5.0, 7.5, cam3, 'Overhead', 'Top-down, slowly rotating: the V slides into a straight line, dancing in sync'),
    Shot('D4', 7.5, 10.0, cam4, 'Low orbit + spin', 'Low orbit to the front: line folds into a diamond, all spin together, end on the lead'),
]

LOOK = dict(bg=BG_DARK, fog=(BG_DARK, 45, 220))
