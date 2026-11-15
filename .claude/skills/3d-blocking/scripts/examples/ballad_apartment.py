"""Example 7 - R&B / BALLAD music video, INDOOR apartment at night (12s, 3 slow shots at 72 BPM).
Singer alone in a small bedroom: standing at the window (slow push-in from the doorway) -> sitting on the edge of
the bed (dolly zoom / vertigo) -> lying on the bed (slow rotating top-down).
Shows: room() with an open side for the camera, window/bed/lamp/rug set dressing, dolly_zoom(), a seated figure,
a lying figure (rotate the whole figure inside a group), slow long takes with minimal motion.

Render:  python ../previz.py render ballad_apartment.py --out out [--audio song.mp3 --audio-start 60]
"""
from previz import *

START, END = 0.0, 12.0
beat = Beat(72, 0.0)
WALLC = (0.62, 0.58, 0.55)

# ---------------------------------------------------------------- bedroom 6 x 6 m, open on the -Z side (where the camera works)
set_ = [ground(60, (0.12, 0.12, 0.13), y=-0.1),
        room(6, 6, 2.8, wall=WALLC, floor=WOOD, open_sides=('-z',)),
        window(1.2, 1.5, 2.92, 1.7, 1.4, glow=(0.50, 0.58, 0.82)),
        bed(-1.9, 1.3, yaw=-90), table(-2.6, 0.05, 0.45, 0.45, 0.55), floor_lamp(2.5, 2.5), rug(0.4, 0.6, 2.6, 1.8),
        chair(2.2, -0.6, yaw=200), box((1.4, 2.0, 0.5), (0.45, 0.36, 0.30), pos=(2.6, 0, 0.9), rot=(0, 90, 0), anchor='bottom')]  # wardrobe
set_ += [box((0.9, 0.7, 0.04), (0.36, 0.46, 0.56), pos=(-1.7, 1.75, 2.92)), box((0.5, 0.6, 0.04), (0.78, 0.62, 0.42), pos=(-0.6, 1.6, 2.92)),
         box((0.04, 0.6, 0.8), (0.70, 0.45, 0.42), pos=(-2.92, 1.7, 0.2)), box((0.3, 0.05, 1.4), WOOD, pos=(-2.85, 1.25, 2.0))]   # wall art + shelf (depth cues)

# ---------------------------------------------------------------- one singer, three setups (new figure per setup is fine across cuts)
at_window = figure(LEAD, HEAD, pos=(1.2, 0, 2.3), yaw=lambda t: 180 + beat.snap(t, 3), visible=lambda t: t < 4, name='window')
on_edge = figure(LEAD, HEAD, pos=(-1.9, 0.45, 0.42), height=1.1, yaw=lambda t: beat.snap(t, 4), visible=lambda t: 4 <= t < 8, name='bed_edge')
lying = group(figure(LEAD, HEAD), pos=(-0.95, 0.78, 1.3), rot=(90, -90, 0), visible=lambda t: t >= 8, name='lying')   # head toward the headboard
NODES = set_ + [at_window, on_edge, lying]

# ---------------------------------------------------------------- cameras: slow, long, almost still
b1 = path_cam([(0, (-1.7, 1.55, -3.9), (1.2, 1.5, 2.3), 32), (4, (-0.5, 1.6, -0.7), (1.2, 1.55, 2.3), 38)], sigma=0.4)   # doorway push-in
b2 = dolly_zoom(subject=(-1.9, 1.25, 0.42), direction=(0.3, 0.06, -1.0), d0=5.2, d1=1.4, lens0=72, t0=4, t1=8)          # vertigo
b3 = orbit_cam([(8, 0, 0.08, 4.4, 0.6), (12, 28, 0.08, 3.7, 0.6)], center=(-1.85, 0, 1.3), lens=28, sigma=0.4)           # top-down, slow turn

SHOTS = [
    Shot('B1', 0, 4, b1, 'Doorway push-in', 'From the doorway, a slow push toward her standing at the night window, back to camera'),
    Shot('B2', 4, 8, b2, 'Dolly zoom', 'Sitting on the edge of the bed facing camera; dolly in while zooming out - the room stretches behind her'),
    Shot('B3', 8, 12, b3, 'Top-down', 'Straight down on her lying on the bed; slow rotation and drift down'),
]
LOOK = dict(bg=BG_INTERIOR, fog=None)
