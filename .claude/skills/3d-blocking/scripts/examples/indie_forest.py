"""Example 9 - INDIE / FOLK music video, OUTDOOR forest (overcast), ONE continuous shot (11s, 84 BPM).
Singer walks along a forest path; the camera side-tracks with her while tree trunks wipe past in the foreground
(parallax); the path opens onto a lake clearing, she stops at the shore and the camera rises and drifts out over
the water, looking back at her small against the treeline.
Shows: side-tracking with foreground occluders (natural wipes / hidden-cut points), forest() with keep_clear for
the path and camera lane, depth from fog, a rise-and-reveal ending, a slow walk with a velocity-matched stop.

Render:  python ../previz.py render indie_forest.py --out out [--audio song.mp3 --audio-start 20]
"""
from previz import *

START, END = 0.0, 11.0
beat = Beat(84, 0.0)
V, T_LIN, T_STOP = 1.25, 5.5, 7.5                      # slow walk, decelerates to a stop at the shore


def walk_x(t):
    D2 = V * (T_STOP - T_LIN) / 2; X1 = -6.0 + V * T_LIN
    if t < T_LIN: return -6.0 + V * t
    if t < T_STOP: s = (t - T_LIN) / (T_STOP - T_LIN); return X1 + D2 * (1 - (1 - s) ** 2)
    return X1 + D2


X_END = walk_x(99)
LR = 8.0
LAKE = (X_END + 3.0 + LR, 0.0, 0.5)                    # straight ahead at the end of the path: hidden until the camera turns
path_clear = lambda x, z: (abs(z) < 2.2 and x < X_END + 2) or (-6.5 < z < -3.0 and x < X_END + 4) or \
                          math.hypot(x - LAKE[0], z - LAKE[2]) < LR + 3 or (x > X_END - 3 and -10 < z < 3)

# ---------------------------------------------------------------- forest: dirt path along +X, foreground trunks between camera lane and path, lake clearing
set_ = [ground(400, (0.40, 0.47, 0.36)), road((-40, 0), (X_END + 1.5, 0), 1.8, DIRT, lines=None),
        cylinder(LR, 0.03, 40, WATER, pos=LAKE), cylinder(LR + 0.8, 0.02, 40, SAND, pos=(LAKE[0], 0, LAKE[2]))]
set_ += forest(-45, -40, 45, 40, 110, seed=21, kind='mix', h=(6, 11), keep_clear=path_clear)
set_ += [tree(x, -2.7, 7.5, 'round') for x in (-4.6, -0.8, 2.9)]          # foreground trunks: the camera passes behind them
set_ += forest(LAKE[0] - 30, LAKE[2] + 14, LAKE[0] + 30, LAKE[2] + 40, 40, seed=4, kind='pine', h=(8, 13))   # far shore treeline

singer = figure(LEAD, HEAD, pos=lambda t: (walk_x(t), beat.hop(t, 0.02), 0),
                yaw=lambda t: -90 - 15 * e1((t - T_STOP + 0.4) / 1.2), name='singer')      # walks +X, then turns to face the lake
NODES = set_ + [singer]


def e1(x): return ease(min(1.0, max(0.0, x)))


# ---------------------------------------------------------------- one move: side-track (camera lane z = -4.8) -> rise over the water, looking back
keys = [(t, (walk_x(t) + 0.3, 1.45, -4.8), (walk_x(t) + 0.6, 1.35, 0.0), 30) for t in (0.0, 2.0, 4.0, 5.5)]
keys += [(7.3, (X_END - 2.6, 1.8, -1.7), (X_END + 3.4, 1.2, 0.4), 28),           # over her shoulder: the lake opens up ahead
         (8.3, (X_END + 0.2, 2.4, -3.2), (X_END + 0.9, 1.1, 0.2), 28),
         (9.2, (X_END + 3.5, 3.2, -3.0), (X_END + 0.5, 1.2, 0.1), 28),
         (11.0, (X_END + 9.0, 4.2, 0.8), (X_END, 1.2, 0.0), 30)]                # out over the water, looking back at her and the treeline
cam = path_cam(keys, sigma=0.35)

SHOTS = [Shot('ONER', START, END, cam, 'Forest walk -> lake reveal',
              'Side-tracking as she walks the path, trunks wiping past in the foreground -> she stops at the shore -> camera rises out over the lake')]
LOOK = dict(bg=BG_OVERCAST, fog=(BG_OVERCAST, 14, 90))
