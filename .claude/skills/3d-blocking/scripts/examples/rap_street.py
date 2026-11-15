"""Example 6 - RAP / HIP-HOP music video, OUTDOOR night street, ONE continuous shot (11s, 90 BPM).
Rapper walks down the middle of a lit city street toward a low, wide leading camera with his crew behind him;
he stops, the crew fans out into a line, the camera orbits low around the group while a car cruises past, then
cranes up to a high angle over the street.
Shows: a leading shot (camera backs away in front of the walker), velocity-matched walk-to-stop, crew offsets
with lag, low wide-lens orbit, night city set (buildings with lit windows, streetlights with light pools,
parked + moving cars with lights), path_cam keys generated in a loop.

Render:  python ../previz.py render rap_street.py --out out [--audio song.mp3 --audio-start 12]
"""
from previz import *

START, END = 0.0, 11.0
beat = Beat(90, 0.0)
e = lambda x: ease(min(1.0, max(0.0, x)))
V, T_LIN, T_STOP = 1.6, 4.5, 6.3                       # walking speed (m/s), stops smoothly at T_STOP


def walk_z(t):                                         # constant pace, then decelerate to z = 0 with matching speed (no pop)
    D2 = V * (T_STOP - T_LIN) / 2; Z0 = V * T_LIN + D2
    if t < T_LIN: return Z0 - V * t
    if t < T_STOP: s = (t - T_LIN) / (T_STOP - T_LIN); return D2 * (1 - s) ** 2
    return 0.0


# ---------------------------------------------------------------- street along Z: road, sidewalks, buildings with lit windows, streetlights, cars
set_ = [ground(400, (0.20, 0.20, 0.22)), road((0, 120), (0, -120), 10, ASPHALT, lines=YELLOW),
        box((3, 0.15, 240), SIDEWALK, pos=(6.5, 0, 0), anchor='bottom'), box((3, 0.15, 240), SIDEWALK, pos=(-6.5, 0, 0), anchor='bottom')]
for i, z in enumerate(range(-90, 100, 13)):
    set_ += [building(14, z, 12, 10, 10 + (i * 7) % 16, (0.36, 0.30, 0.30) if i % 2 else (0.30, 0.31, 0.36), yaw=90, lit=True, seed=i),
             building(-14, z + 6, 12, 10, 12 + (i * 5) % 14, (0.30, 0.31, 0.36) if i % 2 else (0.36, 0.30, 0.30), yaw=-90, lit=True, seed=50 + i)]
for z in range(-80, 90, 16):
    set_ += [streetlight(5.6, z, 6, 1.8, side=-1), streetlight(-5.6, z + 8, 6, 1.8, side=1)]
set_ += [car((-4.0, 0, z), 0 if k % 2 else 180, c, 'sedan') for k, (z, c) in enumerate([(-16, BLUE), (-7, (0.75, 0.75, 0.72)), (12, BLACK), (22, RED)])]
cruiser = car(lambda t: (3.7, 0, 64 - 7.5 * t), 0, (0.55, 0.20, 0.55), 'sedan', lights=True, name='cruiser')   # rolls past, lights on

# ---------------------------------------------------------------- rapper + crew (whole-body swagger on the beat)
rapper = figure(LEAD, HEAD, pos=lambda t: (0, beat.hop(t, 0.035), walk_z(t)), yaw=lambda t: beat.snap(t, 12), name='rapper')
WALK_OFF = [(-1.3, 1.6), (1.3, 1.6), (-2.2, 2.9), (2.2, 2.9)]
LINE_OFF = [(-1.1, 1.4), (1.1, 1.4), (-2.2, 1.7), (2.2, 1.7)]
crew = []
for k in range(4):
    lag = 0.12 * (k + 1)
    def pos(t, k=k, lag=lag):
        u = e((t - T_STOP + 0.2 - lag) / 1.0)
        ox, oz = lerp(WALK_OFF[k], LINE_OFF[k], u)
        return (ox, beat.hop(t - lag, 0.03), walk_z(t - lag) + oz)
    crew.append(figure(EXTRA, HEAD, pos=pos, yaw=lambda t, lag=lag: beat.snap(t - lag, 10), name=f'crew{k}'))
NODES = set_ + [cruiser, rapper] + crew

# ---------------------------------------------------------------- ONE camera move, keys generated: lead -> orbit -> crane
keys = []
for t in (0.0, 1.5, 3.0, 4.5, 6.3):                    # leading: low (0.6m), wide (16mm), 3m in front, backing away
    z = walk_z(t); keys.append((t, (0.35 * math.sin(t * 0.8), 0.6, z - 3.0), (0, 1.3, z), 16))
C = (0, 0, 0.8)
for t, th, r, h in ((7.2, -30, 3.4, 0.7), (8.1, -60, 4.2, 0.85), (9.0, -92, 5.6, 1.4)):    # low orbit to the side (stay in front of the crew)
    keys.append((t, orbit(th, r, h, C), (0, 1.3, 0.8), 16))
keys += [(10.0, orbit(-112, 5.6, 4.8, C), (0, 0.9, 0.8), 18), (11.0, orbit(-128, 4.2, 10.0, C), (0, 0.3, 0.6), 20)]   # crane up high
cam = path_cam(keys, sigma=0.3)

SHOTS = [Shot('ONER', START, END, cam, 'Street walk -> orbit -> crane',
              'Low wide leading shot as he walks with the crew -> stops, crew fans into a line -> low orbit, car cruises by -> crane up high')]
LOOK = dict(bg=BG_NIGHT, fog=(BG_NIGHT, 30, 120))
