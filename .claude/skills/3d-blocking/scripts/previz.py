"""previz.py - Claude 3D Blocking engine. Pure Python (numpy + Pillow + ffmpeg), no GPU, no network.

Build a scene from flat-coloured block primitives, animate with keyframes, frame it with smoothed camera paths,
then render clean + labeled MP4s (optionally with the song) for use as a video-to-video previz reference.

Coordinates: metres. Y is up. A figure faces -Z at yaw 0. Angles in degrees.

CLI
  python previz.py stills  SCENE.py --times 0.5,2,4 [--size 480x270] [--out DIR]
  python previz.py render  SCENE.py [--out DIR] [--name NAME] [--size 960x540] [--fps 24] [--procs N]
                           [--audio SONG.mp3 --audio-start 22.8]
  python previz.py check   SCENE.py          # frame-by-frame scan for jumps / pops / camera snaps

A scene file defines:  START (s), END (s), SHOTS = [Shot(...)], NODES = [...]  and optionally dynamic(t) -> [nodes],
LOOK = dict(bg=..., fog=(color, start, end)).  See references/scene_api.md.
"""
import math, os, sys, subprocess, shutil, importlib.util, time, argparse
import numpy as np
from PIL import Image, ImageDraw, ImageFont

# ============================================================================= palette (flat, matte - like the 3D block examples)
FLOOR = (0.36, 0.38, 0.44)        # dark-world floor
FLOOR_LIGHT = (0.72, 0.72, 0.74)  # bright interior floor
STAGE = (0.62, 0.64, 0.70)
BUILDING = (0.46, 0.49, 0.56)
WALL = (0.80, 0.76, 0.70)
LEAD = (0.12, 0.12, 0.15)         # hero figure body (dark so it reads)
EXTRA = (0.42, 0.44, 0.50)        # backup dancers / clones / crowd
LIGHT_BODY = (0.95, 0.90, 0.75)   # a second hero (e.g. cream jacket)
HEAD = (0.88, 0.89, 0.92)
MASK = (0.96, 0.96, 0.98)
TEAL = (0.30, 0.90, 1.00)
PINK = (1.00, 0.30, 0.75)
YELLOW = (0.98, 0.82, 0.30)
RED = (0.90, 0.12, 0.14)
BG_DARK = (0.22, 0.24, 0.29)
BG_DAY = (0.86, 0.88, 0.90)

# ============================================================================= animation helpers
def ease(x, kind='inout'):
    x = min(max(x, 0.0), 1.0)
    if kind == 'linear': return x
    if kind == 'in': return x * x
    if kind == 'out': return 1 - (1 - x) * (1 - x)
    return x * x * (3 - 2 * x)


def track(*keys, kind='inout'):
    """track((t0, v0), (t1, v1), ...) -> f(t). Values: numbers or tuples. Eased between keys, held outside."""
    keys = sorted(keys, key=lambda k: k[0])
    def f(t):
        if t <= keys[0][0]: return keys[0][1]
        if t >= keys[-1][0]: return keys[-1][1]
        for (ta, va), (tb, vb) in zip(keys[:-1], keys[1:]):
            if ta <= t <= tb:
                u = ease((t - ta) / max(tb - ta, 1e-9), kind)
                if isinstance(va, (tuple, list)): return tuple(a + (b - a) * u for a, b in zip(va, vb))
                return va + (vb - va) * u
    return f


def val(x, t): return x(t) if callable(x) else x


def lerp(a, b, u): return tuple(x + (y - x) * u for x, y in zip(a, b))


def catmull(keys):
    """keys [(t, (x, y, z, ...))] -> f(t): smooth Catmull-Rom curve through every key (time-parameterised)."""
    ts = [k[0] for k in keys]; ps = [np.array(k[1], float) for k in keys]
    def f(t):
        if t <= ts[0]: return tuple(ps[0])
        if t >= ts[-1]: return tuple(ps[-1])
        i = max(j for j in range(len(ts) - 1) if ts[j] <= t)
        p0, p1, p2, p3 = ps[max(i - 1, 0)], ps[i], ps[i + 1], ps[min(i + 2, len(ps) - 1)]
        u = (t - ts[i]) / (ts[i + 1] - ts[i]); u2, u3 = u * u, u * u * u
        return tuple(0.5 * ((2 * p1) + (-p0 + p2) * u + (2 * p0 - 5 * p1 + 4 * p2 - p3) * u2 + (-p0 + 3 * p1 - 3 * p2 + p3) * u3))
    return f


def smoothed(fn, t0, t1, dims, sigma=0.22, rate=120):
    """Sample fn(t)->tuple over [t0, t1], Gaussian-smooth in time (removes speed spikes at keyframes), return g(t)."""
    pad = 3 * sigma
    ts = np.arange(t0 - pad, t1 + pad, 1 / rate)
    raw = np.array([np.ravel(fn(min(max(t, t0), t1))) for t in ts], float)
    k = np.arange(-int(3 * sigma * rate), int(3 * sigma * rate) + 1)
    w = np.exp(-0.5 * (k / (sigma * rate)) ** 2); w /= w.sum()
    sm = np.stack([np.convolve(np.pad(raw[:, c], len(k) // 2, mode='edge'), w, 'valid') for c in range(raw.shape[1])], 1)
    def g(t):
        i = min(len(ts) - 1, max(0, int(round((t - ts[0]) * rate))))
        return tuple(sm[i])
    return g


def shake(t, amp=0.02, freq=9.0, seed=0):
    r = np.random.default_rng(seed); ph = r.uniform(0, 6.28, 6)
    return (amp * math.sin(t * freq + ph[0]) * math.cos(t * 1.7 * freq + ph[1]),
            amp * math.sin(t * 1.3 * freq + ph[2]) * math.cos(t * 0.7 * freq + ph[3]),
            amp * 0.5 * math.sin(t * 1.1 * freq + ph[4]))


class Beat:
    """Music grid. Beat(bpm=117.45, first=2.25).  .phase(t) -> (beat_index, fraction).  .hop(t) small lift on each beat.
    .snap(t, deg) alternating left/right turn that hits on the beat (whole-body choreography, no limbs)."""
    def __init__(self, bpm=120.0, first=0.0):
        self.spb, self.first = 60.0 / bpm, first
    def phase(self, t):
        b = (t - self.first) / self.spb; return int(math.floor(b)), b - math.floor(b)
    def time(self, n): return self.first + n * self.spb
    def hop(self, t, amp=0.05):
        _, f = self.phase(t); return amp * math.sin(math.pi * f / 0.5) if f < 0.5 else 0.0     # up and down in the first half-beat
    def snap(self, t, deg=22):
        i, f = self.phase(t); s = 1 if i % 2 else -1
        return deg * s * (2 * ease(min(1, f / 0.3)) - 1)          # swings from the other side to this side: continuous


# ============================================================================= math
def rot_matrix(rx, ry, rz):
    rx, ry, rz = map(math.radians, (rx, ry, rz))
    cx, sx, cy, sy, cz, sz = math.cos(rx), math.sin(rx), math.cos(ry), math.sin(ry), math.cos(rz), math.sin(rz)
    Rx = np.array([[1, 0, 0], [0, cx, -sx], [0, sx, cx]]); Ry = np.array([[cy, 0, sy], [0, 1, 0], [-sy, 0, cy]])
    Rz = np.array([[cz, -sz, 0], [sz, cz, 0], [0, 0, 1]])
    return Ry @ Rx @ Rz


def mat4(pos=(0, 0, 0), rot=(0, 0, 0), scale=(1, 1, 1)):
    M = np.eye(4)
    if np.isscalar(scale): scale = (scale, scale, scale)
    M[:3, :3] = rot_matrix(*rot) @ np.diag(scale); M[:3, 3] = pos
    return M


def yaw_to(src, dst):
    """Yaw (deg) that turns a figure standing at src to face dst (figures face -Z at yaw 0)."""
    dx, dz = dst[0] - src[0], dst[2] - src[2]
    return math.degrees(math.atan2(-dx, -dz))


# ============================================================================= geometry
class Node:
    """Transform + optional mesh + children. pos/rot/scale/visible/color may be callables of t."""
    def __init__(self, verts=None, faces=None, color=(0.7, 0.7, 0.7), pos=(0, 0, 0), rot=(0, 0, 0), scale=1.0,
                 emissive=False, visible=True, children=None, name='', smooth=False):
        self.smooth = smooth
        self.verts = None if verts is None else np.asarray(verts, float)
        self.faces = None if faces is None else np.asarray(faces, int)
        self.color, self.pos, self.rot, self.scale = color, pos, rot, scale
        self.emissive, self.visible, self.name = emissive, visible, name
        self.children = list(children or [])


def group(*children, pos=(0, 0, 0), rot=(0, 0, 0), scale=1.0, visible=True, name=''):
    return Node(pos=pos, rot=rot, scale=scale, visible=visible, children=children, name=name)


def box(size=(1, 1, 1), color=(0.7, 0.7, 0.7), pos=(0, 0, 0), rot=(0, 0, 0), anchor='center', **kw):
    """anchor 'bottom' = sits on y=0 of its parent."""
    w, h, d = size; y0 = 0 if anchor == 'bottom' else -h / 2; x0, z0 = -w / 2, -d / 2
    v = np.array([[x0, y0, z0], [x0 + w, y0, z0], [x0 + w, y0 + h, z0], [x0, y0 + h, z0],
                  [x0, y0, z0 + d], [x0 + w, y0, z0 + d], [x0 + w, y0 + h, z0 + d], [x0, y0 + h, z0 + d]])
    f = [[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4], [3, 7, 6], [3, 6, 2], [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5]]
    return Node(v, f, color, pos, rot, **kw)


def cylinder(r=0.5, h=1.0, n=24, color=(0.7, 0.7, 0.7), pos=(0, 0, 0), rot=(0, 0, 0), anchor='bottom', smooth=True, **kw):
    y0 = 0 if anchor == 'bottom' else -h / 2
    a = np.linspace(0, 2 * np.pi, n, endpoint=False)
    bot = np.stack([r * np.cos(a), np.full(n, y0), r * np.sin(a)], 1); top = bot + [0, h, 0]
    v = np.vstack([bot, top, bot, top, [[0, y0, 0], [0, y0 + h, 0]]]); f = []
    for i in range(n):
        j = (i + 1) % n
        f += [[i, j, n + j], [i, n + j, n + i], [4 * n, 2 * n + j, 2 * n + i], [4 * n + 1, 3 * n + i, 3 * n + j]]
    return Node(v, f, color, pos, rot, smooth=smooth, **kw)


def sphere(r=0.5, n=16, color=(0.7, 0.7, 0.7), pos=(0, 0, 0), rot=(0, 0, 0), scale=1.0, smooth=True, **kw):
    lat = np.linspace(0, np.pi, n // 2 + 1); lon = np.linspace(0, 2 * np.pi, n, endpoint=False)
    v = [[r * math.sin(a) * math.cos(b), r * math.cos(a), r * math.sin(a) * math.sin(b)] for a in lat for b in lon]
    f = []; m = len(lon)
    for i in range(len(lat) - 1):
        for j in range(m):
            a, b, c, d = i * m + j, i * m + (j + 1) % m, (i + 1) * m + (j + 1) % m, (i + 1) * m + j
            f += [[a, c, b], [a, d, c]]
    return Node(v, f, color, pos, rot, scale=scale, smooth=smooth, **kw)


def torus(R=0.5, r=0.05, n=28, m=6, color=TEAL, pos=(0, 0, 0), rot=(0, 0, 0), **kw):
    """Ring lying flat in XZ (axis = Y). rot=(90,0,0) stands it up facing -Z; rot=(0,0,90) faces sideways."""
    v, f = [], []
    for i in range(n):
        a = 2 * math.pi * i / n
        for j in range(m):
            b = 2 * math.pi * j / m; rr = R + r * math.cos(b)
            v.append([rr * math.cos(a), r * math.sin(b), rr * math.sin(a)])
    for i in range(n):
        for j in range(m):
            a0, a1 = i * m + j, ((i + 1) % n) * m + j; b0, b1 = i * m + (j + 1) % m, ((i + 1) % n) * m + (j + 1) % m
            f += [[a0, a1, b1], [a0, b1, b0]]
    return Node(v, f, color, pos, rot, smooth=True, **kw)


def ring_flat(r0, r1, n=56, color=TEAL, pos=(0, 0, 0), **kw):
    """Flat annulus on the floor (shockwaves, markers)."""
    v, f = [], []
    for i in range(n):
        a = 2 * math.pi * i / n
        v += [[r0 * math.cos(a), 0, r0 * math.sin(a)], [r1 * math.cos(a), 0, r1 * math.sin(a)]]
    for i in range(n):
        j = (i + 1) % n; f += [[2 * i, 2 * j, 2 * j + 1], [2 * i, 2 * j + 1, 2 * i + 1]]
    return Node(v, f, color, pos, **kw)


def world_pos(root, target, t):
    """World position of node `target` (anywhere inside `root`) at time t - use it to put props in hands etc."""
    def walk(n, M):
        M = M @ mat4(val(n.pos, t), val(n.rot, t), val(n.scale, t))
        if n is target: return M[:3, 3]
        for c in n.children:
            r = walk(c, M)
            if r is not None: return r
    return walk(root, np.eye(4))


# ============================================================================= block figure (the stand-in for every character)
def figure(body=LEAD, head=HEAD, pos=(0, 0, 0), yaw=0.0, scale=1.0, visible=True, height=1.76, name='figure'):
    """A block figure: box body + sphere head. NO limbs, NO hair, NO neck animation - the AI video animates those.
    Move it only as a whole: pos(t), yaw(t), scale(t) (e.g. (1, s, 1) to rise in), visible(t).
    Markers for props: fig.hand_r / fig.hand_l (hip height, slightly forward), fig.back (between the shoulders)."""
    hb = height * 0.81
    hand_r, hand_l, back = Node(name='hand_r'), Node(name='hand_l'), Node(name='back')
    g = group(box((0.42, hb, 0.30), body, anchor='bottom'), sphere(0.16, 16, head, pos=(0, hb + 0.18, 0)),
              group(hand_r, pos=(0.30, 0.93, -0.40)), group(hand_l, pos=(-0.30, 0.93, -0.40)), group(back, pos=(0, 1.2, 0.2)),
              pos=pos, rot=(lambda t: (0, val(yaw, t), 0)) if callable(yaw) else (0, yaw, 0), scale=scale, visible=visible, name=name)
    g.hand_r, g.hand_l, g.back = hand_r, hand_l, back
    g.head_height = hb + 0.18
    return g


# ============================================================================= effects (all built from simple blocks)
def shatter(p, t0, colors=(TEAL, PINK), n=24, size=0.13, spread=1.0, life=0.9, seed=0):
    """Cube debris bursting out of point p at t0 (a figure 'derezzing'). Hide the figure at t0."""
    rng = np.random.default_rng(seed); out = []
    for k in range(n):
        o = np.array([rng.uniform(-0.25, 0.25), rng.uniform(-0.9, 0.8), rng.uniform(-0.2, 0.2)]) * spread
        v = np.array([rng.uniform(-1, 1), rng.uniform(0.2, 1.6), rng.uniform(-1, 1)]) * 2.6 * spread
        def pos(t, o=o, v=v):
            u = max(0.0, t - t0); return tuple(np.array(p) + o + v * u - np.array([0, 2.5 * spread, 0]) * u * u)
        out.append(box((size,) * 3, colors[k % len(colors)], pos=pos, rot=(lambda t, k=k: (t * 300 + k * 30, t * 200, 0)),
                       scale=(lambda t: max(0.001, 1 - (t - t0) / life)), visible=(lambda t: t0 <= t < t0 + life)))
    return out


def hologram_in(fig, t0, color=TEAL, dur=0.45, particles=8, height=1.76):
    """Hologram reveal for a figure: a flickering full-height shell + a scan ring rising up the body + spiralling
    particles, then the solid figure takes over. Make the real figure visible from t0 + dur - 0.1.
    No growing / bouncing - the shell appears at full height (growing reads as a bounce in the AI render)."""
    hb = height * 0.81
    P = lambda t: val(fig.pos, t)
    out = [group(box((0.44, hb + 0.02, 0.32), color, anchor='bottom', emissive=True), sphere(0.17, 12, color, pos=(0, hb + 0.18, 0), emissive=True),
                 pos=P, rot=fig.rot,
                 visible=lambda t: t0 <= t < t0 + dur + 0.05 and (math.sin(t * 60) > -0.3 or t > t0 + 0.3)),
           torus(0.42, 0.025, 24, 4, tuple(min(1, c * 0.5 + 0.5) for c in color), emissive=True,
                 pos=lambda t: (P(t)[0], P(t)[1] + 0.05 + (hb + 0.3) * min(1, (t - t0) / dur), P(t)[2]),
                 visible=lambda t: t0 <= t < t0 + dur)]
    for k in range(particles):
        ph = 2 * math.pi * k / particles
        def pp(t, ph=ph, k=k):
            u = max(0.0, t - t0) / 0.6; x, y, z = P(t)
            return (x + 0.55 * math.cos(ph + u * 8), y + 0.1 + 2.0 * u + 0.1 * (k % 2), z + 0.55 * math.sin(ph + u * 8))
        out.append(box((0.06,) * 3, color, emissive=True, pos=pp, visible=lambda t: t0 <= t < t0 + 0.6))
    return out


def beam_strike(p, t_hit, color=TEAL, height=45.0, t_start=None, fade=0.45):
    """A beam drops from the sky onto point p (hits at t_hit) + two shockwave rings across the floor."""
    t_start = t_hit - 0.25 if t_start is None else t_start
    L = track((t_start, 0.01), (t_hit, height), kind='in')
    W = lambda t: max(0.001, 1.0 if t < t_hit + 0.15 else 1 - (t - t_hit - 0.15) / fade) * 0.55
    out = [cylinder(1, 1, 18, color, emissive=True, pos=lambda t: (p[0], p[1] + height - L(t), p[2]),
                    scale=lambda t: (W(t), L(t), W(t)), visible=lambda t: t_start <= t < t_hit + 0.15 + fade)]
    for dt, col in ((0.0, color), (0.25, PINK)):
        t0 = t_hit + dt
        out.append(ring_flat(0.96, 1.0, 64, col, pos=(p[0], p[1] + 0.03, p[2]),
                             scale=lambda t, t0=t0: (0.8 + (t - t0) * 20,) * 3, visible=lambda t, t0=t0: t0 <= t < t0 + 1.1))
    return out


def scaffold_building(x, z, w, h, t0, color=BUILDING, scaffold=TEAL, tiers=1):
    """A building that builds itself: glowing scaffold frame draws upward, the solid fills in, scaffold blinks out."""
    sc = track((t0, 0.001), (t0 + 0.55, 1.0), kind='out'); fill = track((t0 + 0.45, 0.001), (t0 + 1.25, 1.0), kind='out')
    parts, y = [], 0.0
    for i in range(tiers):
        tw = w * (1 - i * 0.15); th = h / tiers
        parts += [box((tw, th, tw), color, pos=(0, y, 0), anchor='bottom'), box((tw * 1.2, 0.25, tw * 1.2), color, pos=(0, y + th, 0), anchor='bottom')]
        y += th + 0.25
    hf = w * 0.6; bars = [box((0.07, y, 0.07), scaffold, pos=(sx * hf, 0, sz * hf), anchor='bottom', emissive=True) for sx in (-1, 1) for sz in (-1, 1)]
    for k in range(1, max(3, int(y / 3.5)) + 1):
        yy = y * k / max(3, int(y / 3.5))
        bars += [box((2 * hf, 0.06, 0.06), scaffold, pos=(0, yy, sz * hf), emissive=True) for sz in (-1, 1)]
        bars += [box((0.06, 0.06, 2 * hf), scaffold, pos=(sx * hf, yy, 0), emissive=True) for sx in (-1, 1)]
    return [group(*parts, pos=(x, 0, z), scale=lambda t: (1, fill(t), 1), visible=lambda t: t >= t0 + 0.45),
            group(*bars, pos=(x, 0, z), scale=lambda t: (1, sc(t), 1),
                  visible=lambda t: t0 <= t < t0 + 1.45 and (t < t0 + 1.15 or math.sin(t * 60) > 0))]


def city_ring(r_min=15, r_max=70, n=30, seed=11, t_build=None, keep_clear=None):
    """A ring of plain block buildings (stacked tiers / towers) around the origin. t_build: if set, each one builds
    with scaffolding starting around that time (nearer first). keep_clear(x, z, w) -> True to skip a spot."""
    rng = np.random.default_rng(seed); placed, tries, out = [], 0, []
    while len(placed) < n and tries < 3000:
        tries += 1
        a = rng.uniform(0, 2 * math.pi); r = rng.uniform(r_min, r_max); x, z = r * math.sin(a), -r * math.cos(a); w = rng.uniform(4, 8)
        if keep_clear and keep_clear(x, z, w): continue
        if any((x - px) ** 2 + (z - pz) ** 2 < (w + pw + 3) ** 2 for px, pz, pw in placed): continue
        placed.append((x, z, w))
    for i, (x, z, w) in enumerate(placed):
        tall = i % 3 == 2; h = rng.uniform(18, 32) if tall else rng.uniform(8, 14); tiers = 1 if tall else 3
        if t_build is None:
            out.append(scaffold_building(x, z, w * (0.6 if tall else 0.9), h, -99, tiers=tiers)[0])
        else:
            out += scaffold_building(x, z, w * (0.6 if tall else 0.9), h, t_build + math.hypot(x, z) * 0.02, tiers=tiers)
    return out


# ============================================================================= set pieces: generic environments & props
# Muted, flat colours so each genre/location reads at a glance without textures.
GRASS = (0.50, 0.55, 0.42); DIRT = (0.68, 0.60, 0.47); ASPHALT = (0.27, 0.28, 0.31); SIDEWALK = (0.50, 0.50, 0.52)
WOOD = (0.52, 0.40, 0.29); FOLIAGE = (0.34, 0.45, 0.33); BRICK = (0.52, 0.36, 0.31); CREAM = (0.90, 0.86, 0.76)
METAL = (0.55, 0.57, 0.60); BLACK = (0.10, 0.10, 0.11); WARM = (1.00, 0.80, 0.48); BLUE = (0.30, 0.45, 0.85)
SAND = (0.86, 0.78, 0.60); WATER = (0.36, 0.52, 0.62); FABRIC = (0.62, 0.55, 0.62)
BG_SUNSET = (0.96, 0.76, 0.56); BG_NIGHT = (0.09, 0.10, 0.15); BG_OVERCAST = (0.72, 0.75, 0.78); BG_INTERIOR = (0.15, 0.15, 0.17)
BG_SKY = (0.66, 0.80, 0.92)


def cone(r=0.5, h=1.0, n=12, color=FOLIAGE, pos=(0, 0, 0), rot=(0, 0, 0), **kw):
    a = np.linspace(0, 2 * np.pi, n, endpoint=False)
    v = np.vstack([np.stack([r * np.cos(a), np.zeros(n), r * np.sin(a)], 1), [[0, h, 0], [0, 0, 0]]]); f = []
    for i in range(n):
        j = (i + 1) % n; f += [[i, n, j], [n + 1, i, j]]
    return Node(v, f, color, pos, rot, **kw)


def attach(parent, *nodes):
    """Parent props to a figure/vehicle so they move with it: attach(singer, guitar())."""
    parent.children.extend(nodes); return parent


def _along(p0, p1):
    """(origin, length, yaw) to lay something built along local +X from p0 to p1 (x, z)."""
    dx, dz = p1[0] - p0[0], p1[1] - p0[1]
    return math.hypot(dx, dz), math.degrees(math.atan2(-dz, dx))


def plane(size=400, color=GRASS, y=0.0, tiles=16, center=(0, 0)):
    """Flat tiled floor (top surface only). Tiling keeps depth precise and fog smooth over huge distances."""
    n = tiles; xs = np.linspace(-size / 2, size / 2, n + 1); v, f = [], []
    for zi in range(n + 1):
        for xi in range(n + 1): v.append([center[0] + xs[xi], y, center[1] + xs[zi]])
    for zi in range(n):
        for xi in range(n):
            a = zi * (n + 1) + xi; f += [[a, a + n + 2, a + 1], [a, a + n + 1, a + n + 2]]
    return Node(v, f, color, name='plane')


def ground(size=400, color=GRASS, y=0.0, tiles=16):
    """The world floor: a tiled plane 1 cm below y, so flat pieces laid on it (roads, rugs, water) never z-fight."""
    return plane(size, color, y - 0.01, tiles)


def hill(x, z, r=30, h=6, color=GRASS):
    """Rolling hill: a flattened half-buried dome."""
    return sphere(1, 14, color, pos=(x, 0, z), scale=(r, h, r))


def tree(x, z, h=6.0, kind='round', color=FOLIAGE, trunk=WOOD):
    """kind: 'round' (leafy), 'pine' (stacked cones), 'palm' (thin trunk + flat crown)."""
    if kind == 'pine':
        parts = [cylinder(0.12 * h / 6, h * 0.3, 8, trunk)] + [cone(h * (0.32 - 0.07 * i), h * 0.38, 10, color, pos=(0, h * (0.22 + 0.22 * i), 0)) for i in range(3)]
    elif kind == 'palm':
        parts = [cylinder(0.11, h, 8, trunk), sphere(1, 10, color, pos=(0, h, 0), scale=(h * 0.32, h * 0.07, h * 0.32))]
    else:
        parts = [cylinder(0.14 * h / 6, h * 0.55, 8, trunk), sphere(h * 0.30, 10, color, pos=(0, h * 0.68, 0), smooth=False)]
    return group(*parts, pos=(x, 0, z), name='tree')


def forest(x0, z0, x1, z1, n=20, seed=3, kind='mix', h=(5, 9), keep_clear=None):
    rng = np.random.default_rng(seed); out = []
    for _ in range(n * 4):
        if len(out) >= n: break
        x, z = rng.uniform(x0, x1), rng.uniform(z0, z1)
        if keep_clear and keep_clear(x, z): continue
        k = kind if kind != 'mix' else ('pine' if rng.random() < 0.5 else 'round')
        out.append(tree(x, z, rng.uniform(*h), k))
    return out


def fence(p0, p1, h=1.1, gap=2.4, color=WOOD):
    """Post-and-rail fence from p0=(x, z) to p1=(x, z)."""
    L, yaw = _along(p0, p1); n = max(1, int(L / gap))
    parts = [box((0.12, h, 0.12), color, pos=(L * i / n, 0, 0), anchor='bottom') for i in range(n + 1)]
    parts += [box((L, 0.09, 0.05), color, pos=(L / 2, y, 0)) for y in (h * 0.45, h * 0.85)]
    return group(*parts, pos=(p0[0], 0, p0[1]), rot=(0, yaw, 0), name='fence')


def road(p0, p1, width=6.0, color=ASPHALT, lines=YELLOW, dash=3.0):
    """Flat road strip from p0 to p1 (x, z) with optional centre dashes. Use DIRT and lines=None for a dirt road."""
    L, yaw = _along(p0, p1)
    parts = [box((L, 0.04, width), color, pos=(L / 2, 0.0, 0), anchor='bottom')]
    if lines: parts += [box((dash, 0.02, 0.15), lines, pos=(dash * (2 * i + 0.5), 0.04, 0), anchor='bottom') for i in range(int(L / (2 * dash)))]
    return group(*parts, pos=(p0[0], 0, p0[1]), rot=(0, yaw, 0), name='road')


def house(x, z, w=8, d=10, h=3.6, yaw=0, color=CREAM, roof=BRICK, porch=True, lit=False):
    """Gabled house; front (door, porch) faces -Z before yaw. lit=True puts warm lit windows on (night)."""
    s = math.radians(28); half = w / 2 + 0.3
    parts = [box((w, h, d), color, anchor='bottom'),
             box((half / math.cos(s), 0.18, d + 0.6), roof, pos=(-w / 4, h + math.tan(s) * w / 4, 0), rot=(0, 0, 28)),
             box((half / math.cos(s), 0.18, d + 0.6), roof, pos=(w / 4, h + math.tan(s) * w / 4, 0), rot=(0, 0, -28)),
             box((1.0, 2.1, 0.08), WOOD, pos=(0, 0, -d / 2 - 0.02), anchor='bottom')]
    win = WARM if lit else (0.40, 0.46, 0.52)
    parts += [box((1.1, 1.0, 0.06), win, pos=(sx * w / 3.2, 1.3, -d / 2 - 0.03), anchor='bottom', emissive=lit) for sx in (-1, 1)]
    if porch: parts += [box((w * 0.8, 0.3, 2.2), WOOD, pos=(0, 0, -d / 2 - 1.1), anchor='bottom')]
    return group(*parts, pos=(x, 0, z), rot=(0, yaw, 0), name='house')


def building(x, z, w=10, d=10, h=14, color=BRICK, yaw=0, lit=False, seed=0, floors=None):
    """City block building; window grid on the -Z face (before yaw), some lit if lit=True (night)."""
    rng = np.random.default_rng(seed); parts = [box((w, h, d), color, anchor='bottom')]
    floors = floors or max(2, int(h / 3.2)); cols = max(2, int(w / 2.2))
    for fy in range(1, floors):
        for cx in range(cols):
            on = lit and rng.random() < 0.45
            parts.append(box((w / cols * 0.5, 1.3, 0.06), WARM if on else (0.22, 0.24, 0.28),
                             pos=(-w / 2 + w / cols * (cx + 0.5), fy * h / floors - 0.4, -d / 2 - 0.03), anchor='bottom', emissive=on))
    return group(*parts, pos=(x, 0, z), rot=(0, yaw, 0), name='building')


def car(pos=(0, 0, 0), yaw=0.0, color=RED, kind='sedan', lights=False, visible=True, name='car'):
    """Block vehicle, front faces -Z at yaw 0. kind 'sedan' | 'pickup' | 'van'. pos/yaw may be functions of t.
    Markers: .seat_l / .seat_r (driver/passenger head-room), .tailgate (pickup: sitting spot on the dropped tailgate)."""
    wheels = [cylinder(0.36, 0.26, 12, BLACK, pos=(sx * 0.98, 0.36, sz), rot=(0, 0, 90), anchor='center')
              for sx in (-1, 1) for sz in (-1.45, 1.45)]
    def cab(w, z0, d, y0, h, col):          # open cab: lower body + roof + 4 pillars, so drivers stay visible
        return [box((w, 0.08, d), col, pos=(0, y0 + h - 0.08, z0), anchor='bottom')] + \
               [box((0.08, h, 0.08), col, pos=(sx * (w / 2 - 0.04), y0, z0 + sz * (d / 2 - 0.04)), anchor='bottom') for sx in (-1, 1) for sz in (-1, 1)]
    if kind == 'pickup':
        parts = [box((1.95, 0.6, 5.3), color, pos=(0, 0.42, 0), anchor='bottom'),          # chassis / lower body
                 box((1.85, 0.3, 1.7), color, pos=(0, 1.02, -0.55), anchor='bottom'),      # cab lower half
                 box((1.85, 0.25, 1.4), color, pos=(0, 1.02, -2.0), anchor='bottom'),      # hood
                 box((0.1, 0.45, 2.5), color, pos=(-0.92, 1.02, 1.35), anchor='bottom'),   # bed walls
                 box((0.1, 0.45, 2.5), color, pos=(0.92, 1.02, 1.35), anchor='bottom'),
                 box((1.85, 0.08, 0.6), color, pos=(0, 0.98, 2.95), anchor='bottom')]     # dropped tailgate
        parts += cab(1.85, -0.55, 1.7, 1.32, 0.62, color)
    elif kind == 'van':
        parts = [box((2.0, 1.9, 5.2), color, pos=(0, 0.42, 0), anchor='bottom'),
                 box((1.9, 0.6, 0.05), (0.35, 0.42, 0.48), pos=(0, 1.55, -2.61), anchor='bottom')]
    else:
        parts = [box((1.85, 0.6, 4.5), color, pos=(0, 0.42, 0), anchor='bottom')] + cab(1.7, 0.15, 2.3, 1.02, 0.6, color)
    L = 2.65 if kind == 'pickup' else (2.6 if kind == 'van' else 2.25)
    parts += [box((0.35, 0.16, 0.05), WARM if lights else (0.9, 0.9, 0.85), pos=(sx * 0.65, 0.75, -L - 0.01), anchor='bottom', emissive=lights) for sx in (-1, 1)]
    parts += [box((0.3, 0.14, 0.05), RED, pos=(sx * 0.7, 0.8, L + 0.01), anchor='bottom', emissive=lights) for sx in (-1, 1)]
    seat_l, seat_r, tail = Node(name='seat_l'), Node(name='seat_r'), Node(name='tailgate')
    parts += [group(seat_l, pos=(-0.45, 0.62, -0.5 if kind == 'pickup' else 0.3)), group(seat_r, pos=(0.45, 0.62, -0.5 if kind == 'pickup' else 0.3)),
              group(tail, pos=(0, 1.02, 2.9))]
    g = group(*parts, *wheels, pos=pos, rot=(lambda t: (0, val(yaw, t), 0)) if callable(yaw) else (0, yaw, 0), visible=visible, name=name)
    g.seat_l, g.seat_r, g.tailgate = seat_l, seat_r, tail
    return g


def streetlight(x, z, h=6.0, arm=1.6, side=1, pool=True, color=WARM):
    """Pole + arm + lamp; side=+1 arm points +X, -1 points -X. pool=True adds a lighter disc of light on the ground."""
    parts = [cylinder(0.09, h, 8, METAL), box((arm, 0.08, 0.08), METAL, pos=(side * arm / 2, h, 0)),
             box((0.5, 0.14, 0.3), color, pos=(side * arm, h - 0.1, 0), emissive=True)]
    if pool: parts.append(cylinder(2.8, 0.015, 24, (0.46, 0.43, 0.38), pos=(side * arm, 0.01, 0)))
    return group(*parts, pos=(x, 0, z), name='streetlight')


def crowd(center=(0, 0, 0), w=10, d=5, n=40, seed=2, face=(0, 0, -50), beat=None, jump=0.18, sway=0.0, body=EXTRA, head=HEAD, height=1.7):
    """A block crowd (low-poly) filling a w x d area around center, everyone facing `face`.
    beat=Beat(...) makes them jump on the beat (jump = metres, may be a function of t, e.g. bigger after the drop);
    sway = degrees of side-to-side on the beat."""
    rng = np.random.default_rng(seed); out = []; hb = height * 0.81
    cols = max(1, int(round(math.sqrt(n * w / d)))); rows = max(1, int(math.ceil(n / cols)))
    for i in range(n):
        cx, cz = i % cols, i // cols
        x = center[0] - w / 2 + w * (cx + 0.5) / cols + rng.uniform(-0.25, 0.25) * w / cols
        z = center[2] - d / 2 + d * (cz + 0.5) / rows + rng.uniform(-0.25, 0.25) * d / rows
        s = rng.uniform(0.9, 1.08); amp = rng.uniform(0.5, 1.2); y0 = center[1]
        yaw0 = yaw_to((x, 0, z), face) + rng.uniform(-12, 12)
        pos = (lambda t, x=x, z=z, amp=amp, y0=y0: (x, y0 + beat.hop(t, amp * val(jump, t)), z)) if beat else (x, y0, z)
        rot = (lambda t, yaw0=yaw0: (0, yaw0 + beat.snap(t, sway), 0)) if (beat and sway) else (0, yaw0, 0)
        out.append(group(box((0.42, hb, 0.3), body, anchor='bottom'), sphere(0.15, 8, head, pos=(0, hb + 0.17, 0)),
                         pos=pos, rot=rot, scale=s, name='crowd'))
    return out


def mic_stand(x, z, h=1.6, y=0.0):
    return group(cylinder(0.22, 0.04, 12, BLACK), cylinder(0.02, h, 6, METAL), sphere(0.06, 8, BLACK, pos=(0, h + 0.04, -0.05)),
                 pos=(x, y, z), name='mic')


def amp(x, z, yaw=0, h=1.2, y=0.0, stack=1):
    parts = []
    for i in range(stack):
        parts += [box((0.9, h / stack - 0.02, 0.4), BLACK, pos=(0, i * h / stack, 0), anchor='bottom'),
                  box((0.75, h / stack * 0.7, 0.02), (0.22, 0.22, 0.24), pos=(0, i * h / stack + h / stack * 0.12, -0.21), anchor='bottom')]
    return group(*parts, pos=(x, y, z), rot=(0, yaw, 0), name='amp')


def drum_kit(x, z, yaw=0, y=0.0, color=RED):
    """Kick, snare, toms, cymbals and stool; drummer sits behind (+Z side), facing -Z."""
    cym = lambda px, pz, hh: [cylinder(0.015, hh, 6, METAL, pos=(px, 0, pz)), cylinder(0.24, 0.015, 16, YELLOW, pos=(px, hh, pz))]
    parts = [cylinder(0.3, 0.45, 16, color, pos=(0, 0.3, -0.2), rot=(90, 0, 0), anchor='center'),
             cylinder(0.18, 0.14, 14, (0.9, 0.9, 0.9), pos=(-0.35, 0.62, 0.15)),
             cylinder(0.15, 0.16, 14, color, pos=(-0.15, 0.82, -0.15)), cylinder(0.15, 0.16, 14, color, pos=(0.17, 0.82, -0.15)),
             cylinder(0.21, 0.4, 14, color, pos=(0.5, 0.25, 0.15)), cylinder(0.2, 0.5, 10, BLACK, pos=(0, 0, 0.55))]
    parts += cym(-0.6, -0.1, 1.15) + cym(0.65, -0.2, 1.25) + cym(-0.75, 0.3, 0.9)
    return group(*parts, pos=(x, y, z), rot=(0, yaw, 0), name='drums')


def guitar(color=RED):
    """A guitar to attach to a figure: attach(fig, guitar()). Sits diagonally across the hips/chest."""
    return group(box((0.38, 0.44, 0.08), color), box((0.07, 0.62, 0.04), WOOD, pos=(0, 0.52, 0)),
                 box((0.1, 0.14, 0.04), BLACK, pos=(0, 0.88, 0)), pos=(0.08, 0.95, -0.22), rot=(0, 0, 62), name='guitar')   # neck toward the left hand


def truss(x, z, w=10, h=5, yaw=0, cans=6, color=METAL, light=WARM):
    """Stage lighting truss: two towers + a top beam with light cans."""
    parts = [box((0.3, h, 0.3), color, pos=(sx * w / 2, 0, 0), anchor='bottom') for sx in (-1, 1)]
    parts += [box((w + 0.3, 0.3, 0.3), color, pos=(0, h, 0))]
    parts += [cylinder(0.14, 0.3, 10, light, pos=(-w / 2 + w * (i + 0.5) / cans, h - 0.45, 0), emissive=True) for i in range(cans)]
    return group(*parts, pos=(x, 0, z), rot=(0, yaw, 0), name='truss')


def room(w=6, d=6, h=2.8, wall=WALL, floor=WOOD, open_sides=('-z',), ceiling=False, pos=(0, 0, 0)):
    """Interior: floor + walls. Leave a side open ('-z', '+z', '-x', '+x') for the camera."""
    parts = [box((w, 0.1, d), floor, pos=(0, -0.05, 0))]
    walls = {'-z': ((w, h, 0.12), (0, 0, -d / 2)), '+z': ((w, h, 0.12), (0, 0, d / 2)),
             '-x': ((0.12, h, d), (-w / 2, 0, 0)), '+x': ((0.12, h, d), (w / 2, 0, 0))}
    parts += [box(s, wall, pos=p, anchor='bottom') for k, (s, p) in walls.items() if k not in open_sides]
    if ceiling: parts.append(box((w, 0.1, d), (0.9, 0.9, 0.9), pos=(0, h, 0)))
    return group(*parts, pos=pos, name='room')


def window(x, y, z, w=1.6, h=1.5, yaw=0, glow=(0.55, 0.62, 0.80), frame=CREAM):
    """A window: bright pane + frame + cross bars. Put it 2 cm in front of a wall."""
    parts = [box((w, h, 0.03), glow, emissive=True), box((w + 0.12, 0.08, 0.06), frame, pos=(0, h / 2, 0)), box((w + 0.12, 0.08, 0.06), frame, pos=(0, -h / 2, 0)),
             box((0.08, h, 0.06), frame, pos=(-w / 2, 0, 0)), box((0.08, h, 0.06), frame, pos=(w / 2, 0, 0)),
             box((0.05, h, 0.05), frame), box((w, 0.05, 0.05), frame)]
    return group(*parts, pos=(x, y, z), rot=(0, yaw, 0), name='window')


def bed(x, z, yaw=0, color=FABRIC):
    return group(box((1.6, 0.45, 2.1), WOOD, anchor='bottom'), box((1.55, 0.18, 2.0), color, pos=(0, 0.45, 0.02), anchor='bottom'),
                 box((1.3, 0.12, 0.45), CREAM, pos=(0, 0.63, 0.75), anchor='bottom'), box((1.6, 1.0, 0.08), WOOD, pos=(0, 0, 1.05), anchor='bottom'),
                 pos=(x, 0, z), rot=(0, yaw, 0), name='bed')


def couch(x, z, yaw=0, color=FABRIC):
    return group(box((2.2, 0.45, 0.9), color, anchor='bottom'), box((2.2, 0.5, 0.2), color, pos=(0, 0.45, 0.35), anchor='bottom'),
                 box((0.2, 0.3, 0.9), color, pos=(-1.0, 0.45, 0), anchor='bottom'), box((0.2, 0.3, 0.9), color, pos=(1.0, 0.45, 0), anchor='bottom'),
                 pos=(x, 0, z), rot=(0, yaw, 0), name='couch')


def table(x, z, w=1.2, d=0.8, h=0.75, yaw=0, color=WOOD):
    legs = [box((0.06, h, 0.06), color, pos=(sx * (w / 2 - 0.05), 0, sz * (d / 2 - 0.05)), anchor='bottom') for sx in (-1, 1) for sz in (-1, 1)]
    return group(box((w, 0.05, d), color, pos=(0, h, 0), anchor='bottom'), *legs, pos=(x, 0, z), rot=(0, yaw, 0), name='table')


def chair(x, z, yaw=0, color=WOOD):
    return group(table(0, 0, 0.45, 0.45, 0.45, color=color), box((0.45, 0.5, 0.05), color, pos=(0, 0.45, 0.2), anchor='bottom'),
                 pos=(x, 0, z), rot=(0, yaw, 0), name='chair')


def floor_lamp(x, z, h=1.6, color=WARM):
    return group(cylinder(0.18, 0.03, 12, BLACK), cylinder(0.02, h, 6, METAL), cylinder(0.2, 0.3, 12, color, pos=(0, h - 0.1, 0), emissive=True),
                 pos=(x, 0, z), name='lamp')


def rug(x, z, w=2.4, d=1.6, color=(0.62, 0.42, 0.40)):
    return box((w, 0.02, d), color, pos=(x, 0, z), anchor='bottom')


def sun(azimuth=0, elevation=8, dist=260, r=16, color=(1.0, 0.92, 0.70)):
    """Big flat sun disc on the horizon (azimuth 0 = straight ahead at -Z). Keep fog end beyond `dist`."""
    a, e = math.radians(azimuth), math.radians(elevation)
    return sphere(r, 16, color, pos=(dist * math.sin(a) * math.cos(e), dist * math.sin(e), -dist * math.cos(a) * math.cos(e)), emissive=True)


# ============================================================================= camera
class Cam:
    def __init__(self, pos, target, lens=35, roll=0.0, flash=0.0, fade=0.0):
        self.pos, self.target, self.lens, self.roll, self.flash, self.fade = pos, target, lens, roll, flash, fade


def orbit(theta, radius, height, center=(0, 0, 0)):
    """Point on a circle round `center`. theta=0 is in front of a figure facing -Z (i.e. at -Z)."""
    a = math.radians(theta); return (center[0] + radius * math.sin(a), height, center[2] - radius * math.cos(a))


def path_cam(keys, sigma=0.22):
    """One smooth continuous camera move. keys = [(t, pos, target, lens), ...] -> f(t)->Cam.
    Catmull-Rom through the keys, then Gaussian-smoothed in time so there are no speed spikes at the keys."""
    t0, t1 = keys[0][0], keys[-1][0]
    c = catmull([(k[0], (*k[1], *k[2], k[3])) for k in keys])
    g = smoothed(c, t0, t1, 7, sigma)
    return lambda t: Cam(g(t)[:3], g(t)[3:6], g(t)[6])


def orbit_cam(keys, center=(0, 0, 0), lens=24, sigma=0.22):
    """Smoothed orbit / crane. keys = [(t, theta_deg, radius, height, target_y), ...]. radius -> 0.08 with a big
    height = straight overhead (the continuous-up camera means no snap when it gets there)."""
    t0, t1 = keys[0][0], keys[-1][0]
    g = smoothed(catmull([(k[0], k[1:]) for k in keys]), t0, t1, 4, sigma)
    def f(t):
        th, r, h, ty = g(t); return Cam(orbit(th, r, h, center), (center[0], ty, center[2]), val(lens, t))
    return f


def follow_cam(subject, offset, look=(0, 1.2, 0), lens=24, t0=0.0, t1=10.0, sigma=0.35):
    """Camera that follows a moving subject (car chase, walk-and-talk, leading shot). subject(t) -> (x, y, z);
    offset / look are world-space offsets from the subject (constants or functions of t). Smoothed like a real rig."""
    def raw(t):
        s = np.asarray(subject(t), float); return (*(s + np.asarray(val(offset, t), float)), *(s + np.asarray(val(look, t), float)))
    g = smoothed(raw, t0, t1, 6, sigma)
    return lambda t: Cam(g(t)[:3], g(t)[3:6], val(lens, t))


def handheld(cam, amp=0.035, freq=5.0, seed=1, roll=1.2):
    """Wrap any camera with handheld wobble (rock, rap, documentary energy). Keep amp 0.02-0.06."""
    def f(t):
        c = cam(t); p = np.asarray(val(c.pos, t), float) + shake(t, amp, freq, seed); g = np.asarray(val(c.target, t), float) + shake(t, amp * 1.5, freq * 0.8, seed + 7)
        return Cam(tuple(p), tuple(g), val(c.lens, t), val(c.roll, t) + roll * math.sin(t * freq * 0.6 + seed))
    return f


def whip(cam_a, cam_b, t_mid, dur=0.28):
    """Whip pan: hard swing from cam_a's view to cam_b's around t_mid (use inside ONE Shot). Reads as a fast pan."""
    def f(t):
        u = (t - (t_mid - dur / 2)) / dur
        if u <= 0: return cam_a(t)
        if u >= 1: return cam_b(t)
        a, b = cam_a(t), cam_b(t); u = ease(u)
        pa, pb = np.asarray(val(a.pos, t), float), np.asarray(val(b.pos, t), float)
        da = np.asarray(val(a.target, t), float) - pa; db = np.asarray(val(b.target, t), float) - pb
        d = da / np.linalg.norm(da) * (1 - u) + db / np.linalg.norm(db) * u; p = pa + (pb - pa) * u
        return Cam(tuple(p), tuple(p + 5 * d / np.linalg.norm(d)), val(a.lens, t) + (val(b.lens, t) - val(a.lens, t)) * u)
    return f


def dolly_zoom(subject, direction, d0, d1, lens0, t0, t1, height=None):
    """Vertigo: camera travels from distance d0 to d1 along `direction` (from subject) while the lens compensates so
    the subject stays the same size and the background stretches/squeezes."""
    dvec = np.asarray(direction, float); dvec /= np.linalg.norm(dvec); s = np.asarray(subject, float)
    def f(t):
        d = d0 + (d1 - d0) * ease((t - t0) / (t1 - t0)); p = s + dvec * d
        if height is not None: p[1] = height
        return Cam(tuple(p), tuple(s), lens0 * d / d0)
    return f


# ============================================================================= renderer
class Renderer:
    def __init__(self, width=960, height=540, bg=BG_DARK, light_dir=(-0.4, -1.0, -0.6), ambient=0.42, fog=None):
        self.W, self.H, self.bg, self.ambient, self.fog = width, height, np.array(bg, float), ambient, fog
        l = np.asarray(light_dir, float); self.light = -l / np.linalg.norm(l)

    def _collect(self, node, t, parent, out):
        if not val(node.visible, t): return
        M = parent @ mat4(val(node.pos, t), val(node.rot, t), val(node.scale, t))
        if node.verts is not None and len(node.faces):
            v = node.verts @ M[:3, :3].T + M[:3, 3]; T = v[node.faces]
            fn = np.cross(T[:, 1] - T[:, 0], T[:, 2] - T[:, 0]); fn /= np.linalg.norm(fn, axis=1, keepdims=True) + 1e-12
            if node.smooth:
                vn = np.zeros_like(v); np.add.at(vn, node.faces.ravel(), np.repeat(fn, 3, 0))
                vn /= np.linalg.norm(vn, axis=1, keepdims=True) + 1e-12; N = vn[node.faces]
            else:
                N = np.repeat(fn[:, None], 3, 1)
            col = np.asarray(val(node.color, t), float)
            if node.emissive: col = 0.55 * col + 0.25          # flat blocking look: effects are coloured, never glowing
            out.append((T, col, False, N))
        for c in node.children: self._collect(c, t, M, out)

    def render(self, nodes, cam, t, bg=None, fog=None):
        W, H = self.W, self.H; tris = []
        for n in nodes: self._collect(n, t, np.eye(4), tris)
        img = np.empty((H, W, 3), np.float32); img[:] = self.bg if bg is None else bg
        fog = self.fog if fog is None else fog
        zbuf = np.full((H, W), np.inf, np.float32)
        p = np.asarray(val(cam.pos, t), float); tg = np.asarray(val(cam.target, t), float)
        fwd = tg - p; fwd /= np.linalg.norm(fwd)
        # continuous "up": blends from world-up to the camera's direction of travel as it tilts to overhead,
        # so passing over the top never flips the frame (the classic overhead snap).
        hd = np.array([fwd[0], 0.0, fwd[2]]); hn = np.linalg.norm(hd)
        if hn > 1e-6:
            hd /= hn; self._last_hd = hd
        else:
            hd = getattr(self, '_last_hd', np.array([0, 0, 1.0]))
        w = min(1.0, max(0.0, (abs(fwd[1]) - 0.75) / 0.2)); w = w * w * (3 - 2 * w)
        up0 = np.array([0, 1.0, 0]) * (1 - w) + hd * w; up0 /= np.linalg.norm(up0)
        right = np.cross(fwd, up0); right /= np.linalg.norm(right); up = np.cross(right, fwd)
        rl = math.radians(val(cam.roll, t)); right, up = right * math.cos(rl) + up * math.sin(rl), up * math.cos(rl) - right * math.sin(rl)
        if tris:
            T = np.concatenate([x[0] for x in tris]); C = np.concatenate([np.repeat(x[1][None], len(x[0]), 0) for x in tris])
            N = np.concatenate([x[3] for x in tris]); lam = np.abs(N @ self.light)
            shade = (self.ambient + (1 - self.ambient) * lam)[..., None] * C[:, None, :]
            V = T - p; x, y, z = V @ right, V @ up, V @ fwd
            f = W * val(cam.lens, t) / 36.0; near = 0.05
            if fog is not None:
                fc, f0, f1 = fog; dist = np.clip((z.mean(1) - f0) / (f1 - f0), 0, 1)[:, None, None] * 0.92
                shade = shade * (1 - dist) + np.asarray(fc, float) * dist
            front = (z > near).all(1); part = (z > near).any(1) & ~front
            polys = [(np.stack([x[i], y[i], z[i]], 1), shade[i]) for i in np.nonzero(front)[0]]
            for i in np.nonzero(part)[0]:
                P3 = np.stack([x[i], y[i], z[i]], 1); Cv = shade[i]; op, oc = [], []
                for a in range(3):
                    A, Bv = P3[a], P3[(a + 1) % 3]
                    if A[2] > near: op.append(A); oc.append(Cv[a])
                    if (A[2] > near) != (Bv[2] > near):
                        u = (near - A[2]) / (Bv[2] - A[2]); op.append(A + (Bv - A) * u); oc.append(Cv[a] + (Cv[(a + 1) % 3] - Cv[a]) * u)
                for k in range(1, len(op) - 1): polys.append((np.array([op[0], op[k], op[k + 1]]), np.array([oc[0], oc[k], oc[k + 1]])))
            cx, cy = W / 2, H / 2
            for P3, col in polys:
                sx = cx + f * P3[:, 0] / P3[:, 2]; sy = cy - f * P3[:, 1] / P3[:, 2]; iz = 1.0 / P3[:, 2]
                x0, x1 = int(max(0, math.floor(sx.min()))), int(min(W - 1, math.ceil(sx.max())))
                y0, y1 = int(max(0, math.floor(sy.min()))), int(min(H - 1, math.ceil(sy.max())))
                if x0 > x1 or y0 > y1: continue
                den = (sy[1] - sy[2]) * (sx[0] - sx[2]) + (sx[2] - sx[1]) * (sy[0] - sy[2])
                if abs(den) < 1e-9: continue
                ys, xs = np.mgrid[y0:y1 + 1, x0:x1 + 1].astype(np.float32) + 0.5
                w0 = ((sy[1] - sy[2]) * (xs - sx[2]) + (sx[2] - sx[1]) * (ys - sy[2])) / den
                w1 = ((sy[2] - sy[0]) * (xs - sx[2]) + (sx[0] - sx[2]) * (ys - sy[2])) / den; w2 = 1 - w0 - w1
                inside = (w0 >= -1e-4) & (w1 >= -1e-4) & (w2 >= -1e-4)
                if not inside.any(): continue
                depth = 1.0 / (w0 * iz[0] + w1 * iz[1] + w2 * iz[2]); zb = zbuf[y0:y1 + 1, x0:x1 + 1]; m = inside & (depth < zb)
                zb[m] = depth[m]; img[y0:y1 + 1, x0:x1 + 1][m] = (w0[m, None] * col[0] + w1[m, None] * col[1] + w2[m, None] * col[2])
        fl, fd = val(cam.flash, t), val(cam.fade, t)
        if fl > 0: img = img * (1 - fl) + fl
        if fd > 0: img = img * (1 - fd)
        return (np.clip(img, 0, 1) * 255).astype(np.uint8)


# ============================================================================= shots & scene loading
class Shot:
    """Shot(id, t0, t1, cam, title, note). cam: f(t)->Cam (or (pos, target, lens[, roll]) tuple).
    For a single continuous shot use ONE Shot covering the whole range."""
    def __init__(self, id, t0, t1, cam, title='', note=''):
        self.id, self.t0, self.t1, self.cam, self.title, self.note = id, t0, t1, cam, title, note


def load_scene(path):
    spec = importlib.util.spec_from_file_location('scene_mod', os.path.abspath(path))
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, os.path.dirname(os.path.abspath(path)))
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    spec.loader.exec_module(m)
    return m


def shot_at(S, t):
    for s in S.SHOTS:
        if s.t0 <= t < s.t1: return s
    return S.SHOTS[-1]


def frame_setup(S, t):
    s = shot_at(S, t)
    c = s.cam(t)
    if not isinstance(c, Cam): c = Cam(*c)
    nodes = list(S.NODES) + (list(S.dynamic(t)) if hasattr(S, 'dynamic') else [])
    look = getattr(S, 'LOOK', {})
    bg = look.get('bg', BG_DARK); fog = look.get('fog')
    if callable(bg): bg = bg(t)
    if callable(fog): fog = fog(t)
    return nodes, c, bg, fog, s


def render_frame(S, R, t):
    nodes, c, bg, fog, _ = frame_setup(S, t)
    return R.render(nodes, c, t, bg=bg, fog=fog)


# ============================================================================= output
def _font(sz):
    for p in ('/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf', '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf',
              '/System/Library/Fonts/Helvetica.ttc', 'C:/Windows/Fonts/arialbd.ttf'):
        if os.path.exists(p): return ImageFont.truetype(p, sz)
    return ImageFont.load_default()


def label_frame(fr, s, t, start):
    im = Image.fromarray(fr); d = ImageDraw.Draw(im); W, H = im.size
    f1, f2 = _font(H // 28), _font(H // 40)
    d.rectangle([0, 0, W, H // 16], fill=(0, 0, 0)); d.text((10, 5), f"{s.id}  {s.title}", font=f1, fill=(255, 255, 255))
    d.text((W - 10, 5), f"{t - start:5.2f}s", font=f1, fill=(255, 220, 120), anchor='ra')
    if s.note:
        d.rectangle([0, H - H // 13, W, H], fill=(0, 0, 0)); d.text((10, H - H // 13 + 6), s.note[:160], font=f2, fill=(220, 220, 220))
    return np.asarray(im)


_S = _R = None


def _init(path, w, h):
    global _S, _R
    _S = load_scene(path); look = getattr(_S, 'LOOK', {})
    _R = Renderer(w, h, bg=look.get('bg', BG_DARK) if not callable(look.get('bg')) else BG_DARK)


def _work(args):
    i, t, fdir = args
    Image.fromarray(render_frame(_S, _R, t)).save(os.path.join(fdir, f'f{i:05d}.png'), compress_level=1)
    return i


def cmd_stills(a):
    S = load_scene(a.scene); w, h = map(int, a.size.split('x')); R = Renderer(w, h)
    ts = [float(x) for x in a.times.split(',')]; ims = []
    for t in ts:
        im = Image.fromarray(render_frame(S, R, t)); ImageDraw.Draw(im).text((6, 4), f"{t - S.START:.2f}s", font=_font(h // 12), fill=(255, 220, 0))
        ims.append(im)
    cols = 3 if len(ims) > 4 else len(ims); rows = (len(ims) + cols - 1) // cols
    sheet = Image.new('RGB', (w * cols, h * rows))
    for k, im in enumerate(ims): sheet.paste(im, ((k % cols) * w, (k // cols) * h))
    os.makedirs(a.out, exist_ok=True); p = os.path.join(a.out, 'stills.jpg'); sheet.save(p, quality=88); print(p)


def cmd_render(a):
    from multiprocessing import Pool
    S = load_scene(a.scene); w, h = map(int, a.size.split('x'))
    start, end = S.START, S.END; n = int(round((end - start) * a.fps))
    name = a.name or os.path.splitext(os.path.basename(a.scene))[0]
    os.makedirs(a.out, exist_ok=True); fdir = os.path.join(a.out, name + '_frames'); os.makedirs(fdir, exist_ok=True)
    jobs = [(i, start + i / a.fps, fdir) for i in range(n)]; t0 = time.time()
    procs = a.procs or max(1, (os.cpu_count() or 2))
    with Pool(procs, initializer=_init, initargs=(a.scene, w, h)) as pool:
        for k, _ in enumerate(pool.imap(_work, jobs, chunksize=4)):
            if k % 48 == 0: print(f'{k}/{n} frames  {time.time() - t0:.0f}s', flush=True)
    ldir = os.path.join(a.out, name + '_lab'); os.makedirs(ldir, exist_ok=True)
    for i, t, _ in jobs:
        fr = np.asarray(Image.open(os.path.join(fdir, f'f{i:05d}.png')).convert('RGB'))
        Image.fromarray(label_frame(fr, shot_at(S, t), t, start)).save(os.path.join(ldir, f'f{i:05d}.jpg'), quality=92)
    def enc(pattern, outp, audio):
        cmd = ['ffmpeg', '-y', '-loglevel', 'error', '-framerate', str(a.fps), '-i', pattern]
        if audio:
            cmd += ['-ss', str(a.audio_start), '-t', str(end - start), '-i', a.audio,
                    '-map', '0:v', '-map', '1:a', '-c:a', 'aac', '-b:a', '256k']
        cmd += ['-c:v', 'libx264', '-preset', 'veryfast', '-crf', '17', '-pix_fmt', 'yuv420p']
        if audio: cmd += ['-shortest']
        subprocess.run(cmd + [outp], check=True)
    au = bool(a.audio)
    enc(os.path.join(fdir, 'f%05d.png'), os.path.join(a.out, name + '_clean.mp4'), au)
    enc(os.path.join(ldir, 'f%05d.jpg'), os.path.join(a.out, name + '_labeled.mp4'), au)
    if au: enc(os.path.join(fdir, 'f%05d.png'), os.path.join(a.out, name + '_clean_nomusic.mp4'), False)
    lines = [f'{name}  {end - start:.2f}s @ {a.fps}fps', 'Shot   in-out (clip time)   title | action']
    lines += [f'{s.id:<6} {s.t0 - start:5.2f}-{s.t1 - start:5.2f}s  {s.title} | {s.note}' for s in S.SHOTS]
    open(os.path.join(a.out, name + '_timing.txt'), 'w').write('\n'.join(lines) + '\n')
    shutil.rmtree(fdir, ignore_errors=True); shutil.rmtree(ldir, ignore_errors=True)
    print('done', round(time.time() - t0, 1), 's ->', a.out)


def cmd_check(a):
    """Flag sudden jumps (pops, snaps, flips) in the camera and every animated top-level node, ignoring shot cuts."""
    S = load_scene(a.scene); fps = 24; ts = np.arange(S.START, S.END, 1 / fps)
    cuts = [s.t0 for s in S.SHOTS[1:]]; found = 0
    def scan(name, series, vis=None):
        nonlocal found
        d = np.linalg.norm(np.diff(series, axis=0), axis=1)
        for k in range(1, len(d) - 1):
            t = ts[k]
            if any(abs(t - c) < 0.06 for c in cuts): continue
            if vis is not None and not (vis[k - 1] and vis[k] and vis[k + 1]): continue   # appearing/disappearing is intended
            if d[k] > 1e-3 and d[k] > 3.5 * max(d[k - 1], 1e-9) and d[k] > 2.0 * d[k + 1] and d[k] > 2.5 * np.median(d):
                print(f'  {name}: jump at {t - S.START:.2f}s (step {d[k]:.3f}, previous {d[k - 1]:.3f})'); found += 1
    cams = [frame_setup(S, t)[1] for t in ts]
    scan('camera position', np.array([val(c.pos, t) for c, t in zip(cams, ts)], float))
    scan('camera target', np.array([val(c.target, t) for c, t in zip(cams, ts)], float))
    for i, n in enumerate(S.NODES):
        if not any(callable(x) for x in (n.pos, n.rot, n.scale)): continue
        ser, vis = [], []
        for t in ts:
            sc = val(n.scale, t); sc = (sc,) * 3 if np.isscalar(sc) else sc
            vis.append(bool(val(n.visible, t)))
            ser.append([*np.ravel(val(n.pos, t)), *[r / 30 for r in np.ravel(val(n.rot, t))], *np.ravel(sc)])
        scan(f'{n.name or "node"}#{i}', np.array(ser, float), vis)
    print('check: ' + ('no jumps found' if not found else f'{found} jump(s) - fix before rendering'))


def main():
    ap = argparse.ArgumentParser(); sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('stills'); p.add_argument('scene'); p.add_argument('--times', required=True)
    p.add_argument('--size', default='480x270'); p.add_argument('--out', default='out')
    p = sub.add_parser('render'); p.add_argument('scene'); p.add_argument('--out', default='out'); p.add_argument('--name')
    p.add_argument('--size', default='960x540'); p.add_argument('--fps', type=int, default=24); p.add_argument('--procs', type=int, default=0)
    p.add_argument('--audio'); p.add_argument('--audio-start', type=float, default=0.0)
    p = sub.add_parser('check'); p.add_argument('scene')
    a = ap.parse_args()
    {'stills': cmd_stills, 'render': cmd_render, 'check': cmd_check}[a.cmd](a)


if __name__ == '__main__':
    sys.modules.setdefault('previz', sys.modules['__main__'])   # scenes do `from previz import *`: share one copy
    main()
