"""Example 4 - COUNTRY music video, OUTDOOR golden hour (12s, 4 shots).
Pickup truck rolls down a dirt road between fences at sunset -> singer at the wheel -> sitting on the tailgate by a
lone tree -> walks to the fence and the camera cranes up to reveal the fields, farmhouse and sun.
Shows: follow_cam (drone chase + car-side tracking), a vehicle with a driver inside, seated figures (shorter
height), sun + warm fog, crane-up reveal, open landscape set pieces (hills, fields, fences, trees, house).

Render:  python ../previz.py render country_roads.py --out out [--audio song.mp3 --audio-start 30]
"""
from previz import *

START, END = 0.0, 12.0
beat = Beat(96, 0.0)                                   # country mid-tempo
e = lambda x: ease(min(1.0, max(0.0, x)))
T_STOP = 5.6

# ---------------------------------------------------------------- landscape: dirt road along Z, fences both sides, fields, hills
FIELD = (0.80, 0.70, 0.44)                              # wheat
set_ = [ground(600, GRASS), road((0, 160), (0, -160), 4.5, DIRT, lines=None),
        fence((3.3, 150), (3.3, -150)), fence((-3.3, 150), (-3.3, -150)),
        box((60, 0.05, 120), FIELD, pos=(36, 0, 20), anchor='bottom'), box((50, 0.05, 90), FIELD, pos=(-32, 0, 60), anchor='bottom'),
        hill(-80, -160, 70, 14), hill(60, -190, 90, 20), hill(170, -60, 80, 16), hill(-170, 40, 90, 18),
        house(-24, -34, 9, 11, 3.6, yaw=25, color=CREAM, roof=BRICK),
        house(-40, -16, 10, 14, 5.0, yaw=25, color=(0.62, 0.24, 0.22), roof=(0.35, 0.33, 0.33), porch=False),   # barn
        tree(-7.5, -5, 8.5, 'round'), sun(azimuth=12, elevation=4, dist=250)]
set_ += forest(-120, -140, -50, -80, 14, seed=5, h=(6, 10)) + forest(40, -140, 120, -90, 12, seed=8, h=(6, 10))

# ---------------------------------------------------------------- the truck drives in (front = -Z) and stops by the lone tree
truck_z = track((0.0, 75.0), (T_STOP, 0.0), kind='out')         # decelerates to a stop
truck_pos = lambda t: (0.0, 0.0, truck_z(t))
truck = car(truck_pos, 0, (0.35, 0.45, 0.58), 'pickup', name='truck')
driver = figure(LEAD, HEAD, pos=(-0.45, 0.62, -0.5), height=1.0, visible=lambda t: t < 6.0, name='driver')   # seated = shorter figure
attach(truck, driver)

# tailgate (shot 3) and fence (shot 4) setups - new figures per setup are fine across cuts
sit = figure(LEAD, HEAD, pos=(0, 1.02, 3.0), height=1.05, yaw=lambda t: 180 + beat.snap(t, 5),
             visible=lambda t: 6.0 <= t < 9.0, name='tailgate_sit')
walk = track((9.0, (1.2, 0, 3.6)), (10.6, (2.6, 0, 5.2)))
stand = figure(LEAD, HEAD, pos=walk, yaw=lambda t: -90 + 30 * (1 - e((t - 10.2) / 0.8)), visible=lambda t: t >= 9.0, name='at_fence')
NODES = set_ + [truck, sit, stand]

# ---------------------------------------------------------------- cameras
c1 = follow_cam(truck_pos, offset=lambda t: lerp((2.0, 11.0, 18.0), (2.5, 5.5, 12.0), e(t / 3.5)), look=(0, 0.8, -10),
                lens=24, t0=0.0, t1=3.5)                                                     # drone chase, descending
c2 = follow_cam(truck_pos, offset=lambda t: lerp((-4.4, 1.6, 0.8), (-4.0, 1.5, -2.6), e((t - 3.5) / 2.5)),
                look=(-0.45, 1.4, -0.5), lens=28, t0=3.5, t1=6.0)                         # tracking at the driver's window
c3 = path_cam([(6.0, (1.6, 0.55, 7.8), (0, 1.55, 3.0), 26), (9.0, (0.8, 0.75, 5.8), (0, 1.6, 3.0), 30)], sigma=0.3)   # low push-in, sun behind
c4 = path_cam([(9.0, (0.4, 1.5, 7.2), (2.0, 1.4, 4.4), 28), (10.4, (-0.5, 3.5, 9.5), (3.5, 1.2, 4.5), 26),
               (12.0, (-3.0, 10.0, 14.5), (5.0, 0.0, -3.0), 24)], sigma=0.3)               # crane up + back: reveal

SHOTS = [
    Shot('C1', 0.0, 3.5, c1, 'Drone chase', 'High behind the pickup as it rolls down the dirt road at sunset; drone drifts down'),
    Shot('C2', 3.5, 6.0, c2, 'Car-side tracking', 'Alongside the driver window, matching speed; slides forward as the truck slows'),
    Shot('C3', 6.0, 9.0, c3, 'Tailgate low push', 'Singer sits on the dropped tailgate, swaying; low slow push-in, sun behind'),
    Shot('C4', 9.0, 12.0, c4, 'Crane-up reveal', 'Singer walks to the fence and looks out; camera cranes up and back over the fields'),
]
LOOK = dict(bg=BG_SUNSET, fog=(BG_SUNSET, 110, 520))
