"""Example 5 - ROCK music video, INDOOR club stage (12s, 6 fast cuts on the bar at 140 BPM).
Four-piece band on a stage (singer at the mic, guitarist, bassist, drummer on a riser) and a crowd jumping on the
beat. Handheld energy: wide over the crowd -> low-angle singer -> whip pan guitar-to-bass -> overhead on the drums ->
from behind the band into the crowd -> crash push-in on the singer.
Shows: crowd(beat=...), attach(fig, guitar()), drum_kit / amp / mic_stand / truss, handheld(), whip(), cuts on bars.

Render:  python ../previz.py render rock_club.py --out out [--audio song.mp3 --audio-start 48]
"""
from previz import *

BPM = 140.0
beat = Beat(BPM, 0.0)
BAR = 4 * 60.0 / BPM                                  # 1.714s
START, END = 0.0, 7 * BAR                             # 7 bars = 12.0s
e = lambda x: ease(min(1.0, max(0.0, x)))
SY = 0.8                                              # stage height; band faces -Z toward the crowd

# ---------------------------------------------------------------- club: stage, back wall, truss, amps, crowd pit
set_ = [room(22, 30, 7, wall=(0.20, 0.20, 0.23), floor=(0.22, 0.22, 0.24), open_sides=('-z',), pos=(0, 0, -7)),
        box((12, SY, 6), (0.16, 0.16, 0.18), pos=(0, 0, 3), anchor='bottom'),                # stage
        box((3.2, 0.4, 2.2), (0.16, 0.16, 0.18), pos=(0, SY, 4.6), anchor='bottom'),          # drum riser
        truss(0, 1.0, 12.5, 6.0, cans=8, light=WARM), truss(0, 5.6, 12.5, 6.0, cans=6, light=PINK),
        amp(-3.6, 5.4, 0, 1.4, SY, stack=2), amp(-2.6, 5.4, 0, 1.4, SY, stack=2), amp(3.0, 5.4, 0, 1.2, SY),
        drum_kit(0, 4.2, 0, SY + 0.4), mic_stand(0, 0.75, 1.55, SY)]
set_ += crowd((0, 0, -5.2), 11, 7.5, 54, seed=4, face=(0, 2, 2), beat=beat, jump=0.22, sway=8)

# ---------------------------------------------------------------- band (whole-body only: bounce, sway, lean on the beat)
singer = figure(LEAD, HEAD, pos=lambda t: (0, SY + beat.hop(t, 0.04), 1.25), yaw=lambda t: beat.snap(t, 10), name='singer')
gtr = figure((0.45, 0.30, 0.28), HEAD, pos=lambda t: (-3.0, SY + beat.hop(t, 0.05), 2.2), yaw=lambda t: 20 + beat.snap(t, 14), name='guitarist')
bass = figure((0.30, 0.36, 0.48), HEAD, pos=lambda t: (3.0, SY + beat.hop(t, 0.03), 2.2), yaw=lambda t: -20 + beat.snap(t, 8), name='bassist')
attach(gtr, guitar(RED)); attach(bass, guitar((0.85, 0.85, 0.80)))
drummer = figure(EXTRA, HEAD, pos=lambda t: (0, SY + 0.4 + 0.35 + beat.hop(t, 0.03), 4.75), height=1.15, name='drummer')   # seated
NODES = set_ + [singer, gtr, bass, drummer]

# ---------------------------------------------------------------- six cuts, one per bar (R3 holds 2 bars for the whip)
b = lambda n: n * BAR
r1 = handheld(path_cam([(b(0), (0.5, 3.4, -13.5), (0, 2.0, 2.5), 24), (b(2), (0.2, 2.8, -9.0), (0, 2.0, 2.0), 26)]), 0.03)
r2 = handheld(path_cam([(b(2), (1.1, 0.9, -0.9), (0, 2.45, 1.25), 20), (b(3), (0.6, 0.85, -0.6), (0, 2.5, 1.25), 20)]), 0.04, roll=3)
cam_g = path_cam([(b(3), (-1.0, 1.7, -1.4), (-3.0, 1.9, 2.2), 30), (b(5), (-0.8, 1.7, -1.2), (-3.0, 1.9, 2.2), 32)])
cam_b = path_cam([(b(3), (0.8, 1.7, -1.4), (3.0, 1.9, 2.2), 30), (b(5), (1.0, 1.7, -1.2), (3.0, 1.9, 2.2), 32)])
r3 = handheld(whip(cam_g, cam_b, b(4)), 0.025)
r4 = lambda t: Cam(orbit(40 * (t - b(5)) / BAR, 0.08, 6.8, (0, 0, 4.6)), (0, SY, 4.6), 30)               # overhead on the drums, slow turn
r5 = path_cam([(b(5.5), (1.6, 2.7, 7.2), (0, 1.6, -5.0), 22), (b(6), (1.2, 2.5, 6.2), (0, 1.5, -5.0), 22)])
r6 = handheld(lambda t: Cam(lerp((0.1, 1.9, -5.5), (0.05, 2.15, -1.0), ease((t - b(6)) / BAR, 'in')), (0, 2.45, 1.25),
                            30 + 18 * ease((t - b(6)) / BAR, 'in')), 0.03)

SHOTS = [
    Shot('R1', b(0), b(2), r1, 'Wide over the crowd', 'Handheld from the back over the jumping crowd, pushing toward the stage'),
    Shot('R2', b(2), b(3), r2, 'Low-angle singer', 'Low, close, tilted up at the singer at the mic; strong handheld'),
    Shot('R3', b(3), b(5), r3, 'Whip guitar -> bass', 'On the guitarist, then a whip pan across the stage to the bassist on the downbeat'),
    Shot('R4', b(5), b(5.5), r4, 'Overhead drums', 'Straight down on the drummer, slow turn'),
    Shot('R5', b(5.5), b(6), r5, 'Behind the band', 'From behind the band into the crowd, silhouettes against the lights'),
    Shot('R6', b(6), b(7), r6, 'Crash push-in', 'Fast accelerating push from the crowd onto the singer for the last bar'),
]
LOOK = dict(bg=BG_INTERIOR, fog=(BG_INTERIOR, 25, 60))
