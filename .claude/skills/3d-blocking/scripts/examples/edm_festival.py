"""Example 8 - EDM / FESTIVAL music video, OUTDOOR night, ONE continuous shot with a BEAT DROP (10s, 128 BPM).
Drone flies in low over a huge crowd toward a festival stage during the build-up (LED wall dim, crowd swaying);
on the DROP the LED wall flashes with the beat, light beams sweep, the crowd jumps and the camera gets a white
flash + shake, then a jib swings down onto the DJ.
Shows: colour as a function of time (LED wall pulsing on the beat), sweeping light beams (rotating thin boxes),
crowd jump that changes over time, Cam(flash=...) + shake on an impact, build-up -> drop structure.

Render:  python ../previz.py render edm_festival.py --out out [--audio song.mp3 --audio-start 75]
"""
from previz import *

START, END = 0.0, 10.0
BPM = 128.0
beat = Beat(BPM, 0.0)
T_DROP = beat.time(12)                                  # the drop lands on beat 12 (~5.63s)
e = lambda x: ease(min(1.0, max(0.0, x)))
dropped = lambda t: t >= T_DROP
SZ, SH = 20.0, 1.6                                      # stage front z, stage height; stage faces -Z

# ---------------------------------------------------------------- LED wall: dim and steady in the build, flashing colours on every beat after the drop
PAL = [PINK, TEAL, BLUE, YELLOW]
def led(t):
    if not dropped(t): return (0.22, 0.24, 0.40)
    i, f = beat.phase(t); c = PAL[i % 4]; k = 1 - 0.6 * f
    return tuple(0.15 + (x - 0.15) * k for x in c)

beams = []
for i in range(6):                                       # thin sweeping beams from the top truss (only after the drop)
    x = -9 + 3.6 * i
    beams.append(box((0.18, 40, 0.18), PAL[i % 4], pos=(x, SH + 9.4, SZ + 2), anchor='bottom', emissive=True,
                     rot=lambda t, i=i: (-35 + 8 * math.sin(t * 1.3 + i), 0, 180 + 28 * math.sin(t * 2.1 + i * 1.7)),
                     visible=dropped, name='beam'))

set_ = [ground(500, (0.16, 0.17, 0.18)),
        box((26, SH, 10), (0.14, 0.14, 0.16), pos=(0, 0, SZ + 5), anchor='bottom'),                      # stage deck
        box((18, 8, 0.4), led, pos=(0, SH + 0.4, SZ + 8), anchor='bottom', emissive=True, name='led_wall'),  # LED wall
        box((2.8, 0.85, 1.0), (0.1, 0.1, 0.12), pos=(0, SH, SZ + 2.5), anchor='bottom'),                  # DJ booth (waist high)
        truss(0, SZ + 1.6, 26, SH + 9.4, cans=10, light=WARM), box((3, 6, 3), BLACK, pos=(-15, 0, SZ + 2), anchor='bottom'),
        box((3, 6, 3), BLACK, pos=(15, 0, SZ + 2), anchor='bottom')]                                        # speaker stacks
set_ += forest(-120, 60, 120, 140, 30, seed=7, kind='pine', h=(10, 16))
dj = figure(LEAD, HEAD, pos=lambda t: (0, SH + beat.hop(t, 0.03 if not dropped(t) else 0.08), SZ + 3.3),
            yaw=lambda t: beat.snap(t, 6 if not dropped(t) else 14), name='dj')
crowd_ = crowd((0, 0, SZ - 15), 30, 24, 130, seed=11, face=(0, 3, SZ + 4), beat=beat,
               jump=lambda t: 0.05 if not dropped(t) else 0.35, sway=7)
NODES = set_ + beams + [dj] + crowd_

# ---------------------------------------------------------------- one move: drone fly-in over the crowd -> drop -> jib down onto the DJ
path = path_cam([
    (0.0, (0.0, 7.5, -26.0), (0, 4.0, SZ + 4), 24),
    (3.0, (0.6, 5.0, -10.0), (0, 4.0, SZ + 4), 24),
    (T_DROP, (0.4, 3.6, 4.0), (0, 3.6, SZ + 4), 26),    # skimming just over heads as the drop hits
    (8.0, (2.6, 4.8, 12.0), (0, 3.0, SZ + 3.3), 30),
    (10.0, (1.0, 4.9, 17.6), (0, 2.6, SZ + 3.3), 40),   # jib down onto the DJ
], sigma=0.3)


def cam(t):
    c = path(t); d = t - T_DROP
    if 0 <= d < 0.6:                                     # impact: white flash + short shake right on the drop
        k = 1 - d / 0.6; s = shake(t, 0.12 * k, 14)
        return Cam(tuple(np.add(c.pos, s)), c.target, c.lens, roll=3 * k * math.sin(d * 40), flash=0.8 * k ** 2)
    return c


SHOTS = [Shot('ONER', START, END, cam, 'Fly-in -> drop -> jib to DJ',
              'Build: drone glides in over the swaying crowd | DROP: flash, LED wall + beams on the beat, crowd jumps | jib down onto the DJ')]
LOOK = dict(bg=BG_NIGHT, fog=(BG_NIGHT, 40, 160))
