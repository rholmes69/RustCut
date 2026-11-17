#!/usr/bin/env python3
"""Split a song into lip-sync clips: N seconds of song plus PAD seconds before and after.

  python lipsync_clips.py split <song> <out_dir> [--project x.rcproj] [--seg 12] [--pad 1]

Writes <out_dir>/clip-NN_<start>-<end>.mp3 and <out_dir>/clips.json (one entry per clip: file,
file start/end, core start/end, and the project's Lyrics-track lines that start in the core span).
Free: only ffmpeg runs. Seedance takes at most 15 s of audio, so seg + 2*pad must be <= 15.
"""
import argparse, json, math, subprocess, sys
from pathlib import Path


def duration(song):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", song],
                         capture_output=True, text=True, check=True).stdout
    return float(out.strip())


def lyric_lines(project):
    p = json.load(open(project, encoding="utf-8-sig"))
    lines = []
    for t in p["tracks"]:
        if t["name"] == "Lyrics":
            for c in t["clips"]:
                text = c["source"]["Text"]["content"].replace("\n", " ")
                lines.append((c["start"], c["start"] + c["duration"], text))
    return sorted(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["split"])
    ap.add_argument("song")
    ap.add_argument("out")
    ap.add_argument("--project")
    ap.add_argument("--seg", type=float, default=12.0)
    ap.add_argument("--pad", type=float, default=1.0)
    a = ap.parse_args()
    if a.seg + 2 * a.pad > 15:
        sys.exit("seg + 2*pad must be <= 15 s (the model's audio limit)")
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    dur = duration(a.song)
    lines = lyric_lines(a.project) if a.project else []
    manifest = []
    for i in range(math.ceil(dur / a.seg)):
        core0, core1 = i * a.seg, min((i + 1) * a.seg, dur)
        s, e = max(0.0, core0 - a.pad), min(dur, core1 + a.pad)
        name = out / f"clip-{i + 1:02d}_{s:06.2f}-{e:06.2f}.mp3"
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-ss", f"{s:.3f}", "-t", f"{e - s:.3f}", "-i", a.song,
                        "-vn", "-ac", "2", "-b:a", "192k", str(name)], check=True)
        # a line belongs to the clip where it starts, so it is listed once, not twice
        ls = [t for (b, c, t) in lines if core0 <= b < core1]
        manifest.append({"clip": i + 1, "file": str(name), "start": round(s, 3), "end": round(e, 3),
                         "core_start": round(core0, 3), "core_end": round(core1, 3), "seconds": round(e - s, 3), "lyrics": ls})
        print(f"{i + 1:02d}  {s:7.2f}-{e:7.2f}s ({e - s:4.1f}s)  {' / '.join(ls)[:100]}")
    json.dump(manifest, open(out / "clips.json", "w", encoding="utf-8"), indent=1)
    print(f"{len(manifest)} clips -> {out}")


main()
