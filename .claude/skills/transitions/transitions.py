"""Beat-aware cut list and transition check for a RustCut music-video project.

  python -I transitions.py list  PROJECT.rcproj [--storyboard storyboard.json] [--from 0] [--to 9999]
      Every main-track cut: time, length, what the shot is, the lyric on screen, how far the cut is
      from the nearest beat (ms) and whether it is a downbeat, overlays covering it, gaps, and any
      transition already set.
  python -I transitions.py check PROJECT.rcproj
      Every transition and overlay fade: how far its start and end land from a beat. Flags gaps
      between touching clips (a 1 ms gap makes set_transition fail) and off-beat cuts.

Reads only. Beats come from the project's saved music analysis (project["music"][<song clip>]),
written by detect_beats / the music-video pipeline.
"""
import argparse
import bisect
import json
import os
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def beat_grid(p):
    music = p.get("music") or {}
    if not music:
        sys.exit("No beat analysis in this project. Run detect_beats (MCP) on the song clip first.")
    key = next(iter(music))
    m = music[key]
    return m.get("bpm", 0.0), m.get("beats", [])


def nearest(beats, t):
    i = bisect.bisect_left(beats, t)
    best = None
    for j in (i - 1, i):
        if 0 <= j < len(beats):
            d = beats[j] - t
            if best is None or abs(d) < abs(best[1]):
                best = (j, d)
    return best or (None, 0.0)


def main_track(p):
    vids = [i for i, t in enumerate(p["tracks"]) if t.get("kind") == "Video"]
    named = [i for i in vids if (p["tracks"][i].get("name") or "").lower() == "main"]
    return named[0] if named else vids[-1]


def labeler(p, storyboard):
    media = {m["id"]: m for m in p["media"]}
    shots = storyboard.get("shots", []) if storyboard else []

    def label(c):
        src = c.get("source") or {}
        if "Text" in src:
            return "TEXT " + src["Text"].get("content", "").replace("\n", " ")[:30]
        m = media.get(src.get("Media"))
        if not m:
            return "?"
        name = m["name"]
        parts = m["path"].replace("\\", "/").split("/")
        if name.startswith("shot-") and "-video" in name and shots and "music-video-" in m["path"]:
            try:
                n = int(name.split("-")[1])
                return f"S{n:02d} " + shots[n - 1]["description"][:44]
            except (ValueError, IndexError):
                pass
        folder = parts[-2] if len(parts) > 1 else ""
        return f"{name[:24]} [{folder[:30]}]"

    return label


def lyric_at(p, t):
    for tr in p["tracks"]:
        if (tr.get("name") or "").lower() != "lyrics":
            continue
        for c in tr["clips"]:
            if c["start"] <= t < c["start"] + c["duration"] and "Text" in c.get("source", {}):
                return c["source"]["Text"].get("content", "").replace("\n", " ")[:42]
    return ""


def fmt(t):
    return f"{int(t // 60)}:{t % 60:05.2f}"


def cmd_list(a):
    p = load(a.project)
    sb = load(a.storyboard) if a.storyboard and os.path.exists(a.storyboard) else None
    bpm, beats = beat_grid(p)
    label = labeler(p, sb)
    mi = main_track(p)
    overlays = [(i, c) for i, t in enumerate(p["tracks"]) if t.get("kind") == "Video" and i != mi for c in t["clips"]]
    print(f"BPM {bpm:.1f}  beat {60 / bpm if bpm else 0:.3f}s  bar {240 / bpm if bpm else 0:.3f}s  main track {mi}")
    print("id    time      len   beat  off   shot                                           | lyric")
    clips = sorted(p["tracks"][mi]["clips"], key=lambda c: c["start"])
    prev_end = None
    for c in clips:
        s = c["start"]
        if s < a.start or s > a.end:
            prev_end = s + c["duration"]
            continue
        bi, off = nearest(beats, s)
        pos = "DOWN" if bi is not None and bi % 4 == 0 else f"b{(bi or 0) % 4 + 1}  "
        flags = []
        if prev_end is not None and abs(s - prev_end) > 0.0005:
            flags.append(f"gap {1000 * (s - prev_end):.0f}ms")
        if abs(off) > 0.04:
            flags.append(f"OFF-BEAT {1000 * off:+.0f}ms")
        cov = [f"T{i}:{o['id']}" for i, o in overlays if o["start"] <= s + 0.05 < o["start"] + o["duration"]]
        if cov:
            flags.append("under " + ",".join(cov))
        if c.get("transition_in"):
            tr = c["transition_in"]
            flags.append(f"TR {tr['kind']} {tr['duration']}")
        print(f"{c['id']:<5} {fmt(s):>8} {c['duration']:5.2f}  {pos} {1000 * off:+4.0f}  {label(c)[:46]:<46} | {lyric_at(p, s + 0.3)}"
              + ("   <" + "; ".join(flags) + ">" if flags else ""))
        prev_end = s + c["duration"]
    print("\nOverlays (fades instead of transitions):")
    for i, o in sorted(overlays, key=lambda x: x[1]["start"]):
        print(f"  T{i} {o['id']:<5} {fmt(o['start'])}-{fmt(o['start'] + o['duration'])}  {label(o)[:50]}  fade_in {o.get('fade_in', 0)} fade_out {o.get('fade_out', 0)}")


def cmd_check(a):
    p = load(a.project)
    bpm, beats = beat_grid(p)
    mi = main_track(p)
    worst = 0.0
    print(f"BPM {bpm:.1f}. Offsets are ms to the nearest beat (under ~40 ms reads as on the beat).")
    clips = sorted(p["tracks"][mi]["clips"], key=lambda c: c["start"])
    for prev, c in zip(clips, clips[1:]):
        gap = c["start"] - (prev["start"] + prev["duration"])
        tr = c.get("transition_in")
        if tr and abs(gap) > 0.0005:
            print(f"  GAP {1000 * gap:.1f}ms before clip {c['id']} at {fmt(c['start'])}: the transition won't apply")
        if not tr:
            continue
        d = tr["duration"]
        s0, s1 = nearest(beats, c["start"] - d / 2)[1], nearest(beats, c["start"] + d / 2)[1]
        cut = nearest(beats, c["start"])[1]
        worst = max(worst, abs(s0), abs(s1))
        print(f"  {fmt(c['start'])} clip {c['id']:<5} {tr['kind']:<10} {d:.2f}s  cut {1000 * cut:+.0f}  start {1000 * s0:+.0f}  end {1000 * s1:+.0f}")
    for i, t in enumerate(p["tracks"]):
        if t.get("kind") != "Video" or i == mi:
            continue
        for o in t["clips"]:
            fi, fo = o.get("fade_in", 0) or 0, o.get("fade_out", 0) or 0
            if fi:
                print(f"  T{i} clip {o['id']} fade-in ends {1000 * nearest(beats, o['start'] + fi)[1]:+.0f}ms from a beat")
            if fo:
                print(f"  T{i} clip {o['id']} fade-out starts {1000 * nearest(beats, o['start'] + o['duration'] - fo)[1]:+.0f}ms from a beat")
    print(f"worst transition edge: {1000 * worst:.0f}ms")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    l = sub.add_parser("list")
    l.add_argument("project")
    l.add_argument("--storyboard")
    l.add_argument("--from", dest="start", type=float, default=0.0)
    l.add_argument("--to", dest="end", type=float, default=1e9)
    c = sub.add_parser("check")
    c.add_argument("project")
    a = ap.parse_args()
    {"list": cmd_list, "check": cmd_check}[a.cmd](a)


if __name__ == "__main__":
    main()
