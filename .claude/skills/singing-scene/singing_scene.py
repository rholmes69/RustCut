"""A lip-synced singing scene in a RustCut project: a still -> Seedance 2.0 performance shot -> sync fix.
This is the process behind Bond Money's choir take (performance-shot-01-png-1.mp4).

  dry     --project P --still S --start T --end E [--track 1] [--direction-file F] [--model fast]
          Free. Places the still in a throwaway copy of the project and prints Seedance's segments, lyrics and cost.
  run     (same options) [--sheet <character sheet>]
          Paid. Backs up the project as <name>.before-singing.rcproj, then places the still, runs the performance
          shot, mutes it and saves. Prints the new clip(s) with their ids.
  fit     --project P --clip ID [--mouth x,y,w,h]
          Free. Measures the speed and shift that best match the mouth to the vocal.
  apply   --project P --clip ID --speed X --shift D [--close-gap]
          Free. Sets the speed, moves the start by the shift, keeps the end, mutes the clip, and stretches the clip
          before it to meet it with --close-gap. Backs up the project as <name>.before-sync.rcproj first.
  preview --project P --clip ID --out F.mp4 [--speed X --shift D]
          Free. Writes the clip's span with the song, as placed or as it would be with --speed/--shift.

The project must be closed in the RustCut app for run and apply. Needs target/release/rustcut-mcp.exe.
"""
import argparse
import json
import math
import os
import shutil
import subprocess
import sys

EXE = "C:/Users/sales/MyVideoEditor/target/release/rustcut-mcp.exe"


class Mcp:
    def __init__(self):
        env = {k: v for k, v in os.environ.items() if k != "OPENROUTER_API_KEY"}
        self.p = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env)
        self.n = 0
        self.call("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "singing-scene", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")

    def call(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                sys.exit("rustcut-mcp exited")
            r = json.loads(line)
            if r.get("id") == self.n:
                return r

    def tool(self, name, args):
        res = self.call("tools/call", {"name": name, "arguments": args}).get("result", {})
        text = " ".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        if res.get("isError"):
            if "content_policy_violation" in text or "likenesses of real people" in text:
                print("REFUSED by fal's ByteDance filter (not charged). Redraw the still with a different angle or light"
                      " (a three-quarter angle passed on Bond Money), or use Kling + lipsync-2.", flush=True)
            print(f"{name} ERROR: {text[:600]}", flush=True)
            self.close()
            sys.exit(1)
        return text

    def close(self):
        self.p.stdin.close()
        self.p.wait()


def rustcut_running():
    out = subprocess.run(["tasklist", "/FI", "IMAGENAME eq rustcut.exe"], capture_output=True, text=True).stdout
    return "rustcut.exe" in out.lower()


def need_closed():
    if rustcut_running():
        sys.exit("RustCut is running. Ask the user to close it (or confirm the project isn't open), then rerun.")


def backup(project, tag):
    root, ext = os.path.splitext(project)
    dst = f"{root}.{tag}{ext}"
    shutil.copyfile(project, dst)
    print("backup:", dst, flush=True)


def clips(mcp):
    """Every clip as (track, clip dict, media path)."""
    p = json.loads(mcp.tool("get_project", {}))
    media = {m["id"]: m.get("path", m.get("name", "")) for m in p.get("media", [])}
    out = []
    for ti, t in enumerate(p["tracks"]):
        for c in t.get("clips", []):
            src = c.get("source")
            mid = src.get("Media") if isinstance(src, dict) else c.get("media_id")
            out.append((ti, t, c, media.get(mid, "")))
    return out


def song_clip(mcp, at):
    """The longest non-voice audio clip under time `at` (the song, not a dialogue line)."""
    under = [(c, path) for ti, t, c, path in clips(mcp)
             if str(t.get("kind", "")).lower() == "audio" and c["start"] <= at < c["start"] + c["duration"] and path and c.get("audio_role") != "voice"]
    if not under:
        sys.exit("no song clip under the scene")
    return max(under, key=lambda x: x[0]["duration"])


def find_clip(mcp, cid):
    for ti, t, c, path in clips(mcp):
        if c["id"] == cid:
            return ti, c, path
    sys.exit(f"no clip {cid}")


def probe(path):
    return float(subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path], capture_output=True, text=True).stdout.strip())


def place_and_perform(a, dry):
    direction = open(a.direction_file, encoding="utf-8").read().strip() if a.direction_file else ""
    mcp = Mcp()
    mcp.tool("open_project", {"path": a.project})
    media = json.loads(mcp.tool("import_media", {"paths": [a.still]}))["imported"][0]["media_id"]
    add = {"media_id": media, "start": a.start, "duration": a.end - a.start}
    if a.track is not None:
        add["track_index"] = a.track
    clip = json.loads(mcp.tool("add_clip", add))["clip"]
    print(f"still placed as clip {clip['id']} on track {clip['track_index']}: {clip['start']:.2f}-{clip['start'] + clip['duration']:.2f}", flush=True)
    if abs(clip["start"] - a.start) > 0.02:
        print("WARNING: the still didn't land where asked (the spot is occupied?). Check the track and times.", flush=True)
    args = {"clip_id": clip["id"], "model": a.model, "direction": direction, "dry_run": dry}
    if a.audio_clip is not None:
        args["audio_clip_id"] = a.audio_clip
    if a.sheet and not dry:
        args["character_sheet"] = a.sheet
    print(mcp.tool("performance_shot", args)[:2000], flush=True)
    if dry:
        mcp.close()  # nothing saved: the project file is unchanged
        return
    # Mute the new take(s) so the song carries the scene, then save and list them.
    new = []
    for ti, t, c, path in clips(mcp):
        if ti == clip["track_index"] and c["start"] < a.end - 0.05 and c["start"] + c["duration"] > a.start + 0.05 and "performance" in os.path.basename(path):
            mcp.tool("update_clip", {"clip_id": c["id"], "volume": 0})
            new.append((c, path))
    print(mcp.tool("save_project", {}), flush=True)
    mcp.close()
    for c, path in new:
        span = c["duration"]
        length = probe(path)
        print(f"take: clip {c['id']} {c['start']:.3f}-{c['start'] + span:.3f}  {path}")
        print(f"  video {length:.2f} s for a {span:.2f} s span -> first-guess speed {length / span:.4f} (then run fit)")


# ---------- sync measurement ----------

def measure(video, song, t0, secs, mouth, extra_speeds=()):
    FPS, W, H = 24, 96, 72
    mx, my, mw, mh = mouth
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", video, "-vf", f"fps={FPS},crop=iw*{mw}:ih*{mh}:iw*{mx}:ih*{my},scale={W}:{H},format=gray",
                          "-f", "rawvideo", "-"], capture_output=True).stdout
    n = len(raw) // (W * H)
    frames = [raw[i * W * H:(i + 1) * W * H] for i in range(n)]
    dark = [sum(1 for p in f if p < 60) / (W * H) for f in frames]  # an open mouth is dark
    motion = [0.0] + [sum(abs(x - y) for x, y in zip(frames[i], frames[i - 1])) / (W * H) for i in range(1, n)]
    SR = 4800
    pcm = subprocess.run(["ffmpeg", "-v", "error", "-ss", f"{max(0.0, t0 - 2):.3f}", "-t", f"{secs + 4:.3f}", "-i", song, "-af",
                          "pan=mono|c0=0.5*c0+0.5*c1,highpass=f=250,lowpass=f=2500", "-ar", str(SR), "-f", "s16le", "-"], capture_output=True).stdout
    samples = [int.from_bytes(pcm[i:i + 2], "little", signed=True) for i in range(0, len(pcm) - 1, 2)]
    pre = min(2.0, t0)
    hop = SR // FPS

    def loud(t):
        i = int((t + pre) * SR)
        seg = samples[max(0, i - hop // 2):i + hop // 2]
        return math.sqrt(sum(s * s for s in seg) / len(seg)) if seg else 0.0

    def corr(xs, ys):
        ax, ay = sum(xs) / len(xs), sum(ys) / len(ys)
        sxy = sum((x - ax) * (y - ay) for x, y in zip(xs, ys))
        sx = math.sqrt(sum((x - ax) ** 2 for x in xs))
        sy = math.sqrt(sum((y - ay) ** 2 for y in ys))
        return sxy / (sx * sy) if sx and sy else 0.0

    guess = (n / FPS) / secs
    speeds = sorted({round(1.0 + k / 100, 2) for k in range(-4, 15)} | {round(guess, 4)} | {round(x, 6) for x in extra_speeds})
    results = []
    for s in speeds:
        for d20 in range(-30, 31):  # shifts of -1.5..+1.5 s
            d = d20 / 20
            # Source time v is heard at song time t0 + (v - d) / s.
            idx = [i for i in range(n) if 0 <= (i / FPS - d) / s <= secs]
            if len(idx) < FPS * 4:
                continue
            env = [loud((i / FPS - d) / s) for i in idx]
            results.append((corr([dark[i] for i in idx], env) + corr([motion[i] for i in idx], env), s, d))
    results.sort(reverse=True)
    return n, guess, results


def cmd_fit(a):
    mcp = Mcp()
    mcp.tool("open_project", {"path": a.project})
    ti, c, video = find_clip(mcp, a.clip)
    song, song_path = song_clip(mcp, c["start"])
    mcp.close()
    if c.get("source_in", 0) > 0.01:
        print("note: the clip is trimmed at the start; the fit measures from the start of the file")
    t0 = song["source_in"] + (c["start"] - song["start"]) * song.get("speed", 1.0)  # song time where the clip starts
    secs = c["duration"]
    n, guess, results = measure(video, song_path, t0, secs, [float(x) for x in a.mouth.split(",")], [c.get("speed", 1.0)])
    print(f"{n} frames. Best fits (score, speed, shift s):")
    for r in results[:8]:
        print(f"  {r[0]:.3f}  speed {r[1]:.4f}  shift {r[2]:+.2f}")
    now = [r for r in results if abs(r[1] - c.get("speed", 1.0)) < 1e-4 and r[2] == 0.0]
    print(f"as placed now (speed {c.get('speed', 1.0):.4f}, shift 0):", f"{now[0][0]:.3f}" if now else "n/a")
    # Seedance renders about 1 s more video than the span and spreads the singing over all of it, so a fresh take
    # (still at 1x) usually needs speed = video length / span. An adjusted take keeps its current speed.
    base = guess if abs(c.get("speed", 1.0) - 1.0) < 1e-3 else c.get("speed", 1.0)
    at_base = [r for r in results if abs(r[1] - round(base, 4)) < 1e-3 or abs(r[1] - base) < 1e-4]
    best = results[0]
    if at_base:
        r = at_base[0]
        print(f"recommended (speed = {'video length / span' if base == guess else 'current speed'}): score {r[0]:.3f}")
        print(f"  apply with: --speed {r[1]:.4f} --shift {r[2]:+.2f}")
    print(f"best overall (the measure is rough; prefer the recommended one unless this scores clearly higher): score {best[0]:.3f}")
    print(f"  apply with: --speed {best[1]:.4f} --shift {best[2]:+.2f}")


def cmd_apply(a):
    need_closed()
    backup(a.project, "before-sync")
    mcp = Mcp()
    mcp.tool("open_project", {"path": a.project})
    ti, c, video = find_clip(mcp, a.clip)
    start, end = c["start"], c["start"] + c["duration"]
    new_start = start - a.shift / a.speed  # source frame v then plays when the song sings it
    prev = None
    for tj, t, x, path in clips(mcp):
        if tj == ti and x["id"] != c["id"] and x["start"] < start and (prev is None or x["start"] > prev["start"]):
            prev = x
    if prev and new_start < prev["start"] + prev["duration"]:  # make room first
        mcp.tool("update_clip", {"clip_id": prev["id"], "duration": new_start - prev["start"]})
    mcp.tool("update_clip", {"clip_id": c["id"], "speed": a.speed, "volume": 0})
    mcp.tool("update_clip", {"clip_id": c["id"], "start": new_start})
    got = json.loads(mcp.tool("update_clip", {"clip_id": c["id"], "duration": end - new_start}))["clip"]
    print(f"take: clip {got['id']} track {got['track_index']} {got['start']:.3f}-{got['start'] + got['duration']:.3f} at {got['speed']:.4f}x, muted", flush=True)
    if got["track_index"] != ti:
        print("WARNING: the clip moved to another track (the spot was occupied). Check it in RustCut.")
    if prev and a.close_gap:
        prev_end = prev["start"] + prev["duration"]
        if new_start - prev_end > 0.005:
            src = prev["duration"] * prev.get("speed", 1.0)
            p2 = json.loads(mcp.tool("update_clip", {"clip_id": prev["id"], "speed": src / (new_start - prev["start"])}))["clip"]
            print(f"clip before: {p2['id']} {p2['start']:.3f}-{p2['start'] + p2['duration']:.3f} at {p2['speed']:.3f}x (gap closed)")
    print(mcp.tool("save_project", {}), flush=True)
    mcp.close()


def cmd_preview(a):
    mcp = Mcp()
    mcp.tool("open_project", {"path": a.project})
    ti, c, video = find_clip(mcp, a.clip)
    song, song_path = song_clip(mcp, c["start"])
    mcp.close()
    speed = a.speed if a.speed else c.get("speed", 1.0)
    end = c["start"] + c["duration"]
    start = c["start"] - (a.shift or 0.0) / speed
    t0 = song["source_in"] + (start - song["start"]) * song.get("speed", 1.0)
    dur = end - start
    src_in = c.get("source_in", 0.0)
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-ss", f"{src_in:.3f}", "-i", video, "-ss", f"{t0:.3f}", "-t", f"{dur:.3f}", "-i", song_path,
                    "-filter_complex", f"[0:v]setpts=PTS/{speed}[v]", "-map", "[v]", "-map", "1:a", "-t", f"{dur:.3f}", "-r", "24",
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-c:a", "aac", a.out], check=True)
    print("preview:", a.out)


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["dry", "run", "fit", "apply", "preview"])
    ap.add_argument("--project", required=True)
    ap.add_argument("--still")
    ap.add_argument("--start", type=float)
    ap.add_argument("--end", type=float)
    ap.add_argument("--track", type=int)
    ap.add_argument("--audio-clip", type=int)
    ap.add_argument("--direction-file")
    ap.add_argument("--model", default="fast", choices=["fast", "best"])
    ap.add_argument("--sheet")
    ap.add_argument("--clip", type=int)
    ap.add_argument("--mouth", default="0.4,0.3,0.2,0.3", help="mouth search box as x,y,w,h fractions of the frame (default: the centre)")
    ap.add_argument("--speed", type=float)
    ap.add_argument("--shift", type=float, default=0.0)
    ap.add_argument("--close-gap", action="store_true")
    ap.add_argument("--out")
    a = ap.parse_args()
    if a.cmd in ("dry", "run"):
        if not (a.still and a.start is not None and a.end is not None):
            sys.exit("dry/run need --still, --start and --end")
        if a.cmd == "run":
            need_closed()
            backup(a.project, "before-singing")
        place_and_perform(a, a.cmd == "dry")
    elif a.cmd == "fit":
        cmd_fit(a)
    elif a.cmd == "apply":
        if a.clip is None or a.speed is None:
            sys.exit("apply needs --clip and --speed")
        cmd_apply(a)
    elif a.cmd == "preview":
        if a.clip is None or not a.out:
            sys.exit("preview needs --clip and --out")
        cmd_preview(a)
