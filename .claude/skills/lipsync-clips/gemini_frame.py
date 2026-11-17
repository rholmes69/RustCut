#!/usr/bin/env python3
"""Draw a keyframe with Gemini through OpenRouter (/images) from reference photos. ~ $0.07 per image.

  python gemini_frame.py <out_dir> <name> "<prompt>" <ref1> [<ref2> ...] [--n 2]

Writes <out_dir>/<name>-1.png, -2.png ... Uses the OpenRouter key and image model saved in RustCut's
settings (%APPDATA%\\RustCut\\ai.json); the key is never printed. Run with PYTHONIOENCODING=utf-8.
Why this exists: Seedance rejects real-person photos and frames from a face-trained LoRA as
"likenesses of real people", but accepts an AI-drawn still like this one.
"""
import argparse, base64, json, os, sys
from pathlib import Path

import requests

ap = argparse.ArgumentParser()
ap.add_argument("out_dir")
ap.add_argument("name")
ap.add_argument("prompt")
ap.add_argument("refs", nargs="+")
ap.add_argument("--n", type=int, default=2)
a = ap.parse_args()
cfg = json.load(open(os.path.expandvars(r"%APPDATA%\RustCut\ai.json"), encoding="utf-8-sig"))
mime = lambda p: "image/jpeg" if p.lower().endswith((".jpg", ".jpeg")) else "image/png"
refs = [{"type": "image_url", "image_url": {"url": f"data:{mime(r)};base64," + base64.b64encode(open(r, "rb").read()).decode()}} for r in a.refs]
out_dir = Path(a.out_dir)
out_dir.mkdir(parents=True, exist_ok=True)
for i in range(a.n):
    r = requests.post("https://openrouter.ai/api/v1/images", timeout=300,
                      headers={"Authorization": "Bearer " + cfg["api_key"]},
                      json={"model": cfg.get("image_model") or "google/gemini-3.1-flash-image", "prompt": a.prompt, "n": 1,
                            "aspect_ratio": "16:9", "input_references": refs})
    j = r.json()
    b64 = ((j.get("data") or [{}])[0]).get("b64_json") or ""
    if not b64:
        print("no image:", str(j)[:300], file=sys.stderr)
        continue
    out = out_dir / f"{a.name}-{i + 1}.png"
    out.write_bytes(base64.b64decode(b64.split("base64,")[-1]))
    print("saved", out, "cost", j.get("usage", {}).get("cost"))
