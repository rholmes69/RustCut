"""B-roll helper for a RustCut music-video storyboard (free, local file edits only).

  python -I broll.py list  "<storyboard.json>"              slots with time, section, shot and lyric
  python -I broll.py add   "<storyboard.json>" broll.json   append b-roll shots (no lead, no frame yet)
  python -I broll.py place "<storyboard.json>" "40=23,145=25"  put shot 23 on the slot playing at 40 s, ...

broll.json is a list of {"description": "...", "motion": "...", "seconds": 4}.
Shot numbers are 1-based, as in the contact sheets. Every write first saves
storyboard.before-broll.json (once) so the original can be restored.
"""
import json
import os
import shutil
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def save(path, sb):
    backup = os.path.join(os.path.dirname(path), "storyboard.before-broll.json")
    if not os.path.exists(backup):
        shutil.copyfile(path, backup)
        print("backup:", backup)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(sb, f, indent=2, ensure_ascii=False)


def mmss(t):
    return f"{int(t // 60)}:{t % 60:04.1f}"


def cmd_list(path):
    sb = load(path)
    broll = {i for i, s in enumerate(sb["shots"]) if s.get("no_lead")}
    for n, s in enumerate(sb["slots"], 1):
        mark = " b-roll" if s["shot"] in broll else ""
        lyric = (s.get("lyric") or "").replace("\n", " / ")[:70]
        print(f"{n:3} {mmss(s['start']):>7}-{mmss(s['end']):<7} {s.get('section', ''):<11} shot {s['shot'] + 1:<3}{mark:<7} {lyric}")
    print(f"{len(sb['shots'])} shots, {len(sb['slots'])} slots, b-roll shots: {sorted(i + 1 for i in broll)}")


def cmd_add(path, spec):
    sb = load(path)
    new = load(spec)
    first = len(sb["shots"]) + 1
    for b in new:
        sb["shots"].append({
            "description": b["description"].strip(),
            "motion": b.get("motion", "").strip(),
            "caption": "",
            "seconds": float(b.get("seconds", 4)),
            "frame": None,
            "approved": False,
            "video": None,
            "no_lead": True,
        })
    save(path, sb)
    print(f"added shots {first}-{len(sb['shots'])}; draw them with MV_STAGE=redraw MV_REDRAW=\"\"")


def cmd_place(path, pairs):
    sb = load(path)
    for pair in pairs.split(","):
        at, shot = pair.split("=")
        at, shot = float(at), int(shot)
        if not 1 <= shot <= len(sb["shots"]):
            sys.exit(f"no shot {shot}")
        slot = next((s for s in sb["slots"] if s["start"] <= at < s["end"]), None)
        if slot is None:
            sys.exit(f"no slot plays at {at} s")
        print(f"{mmss(slot['start'])}-{mmss(slot['end'])}: shot {slot['shot'] + 1} -> {shot}  ({(slot.get('lyric') or '')[:60]})")
        slot["shot"] = shot - 1
    save(path, sb)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    cmd, path = sys.argv[1], sys.argv[2]
    if cmd == "list":
        cmd_list(path)
    elif cmd == "add":
        cmd_add(path, sys.argv[3])
    elif cmd == "place":
        cmd_place(path, sys.argv[3])
    else:
        sys.exit(__doc__)
