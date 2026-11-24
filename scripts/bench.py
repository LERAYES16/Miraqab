#!/usr/bin/env python3
"""Measure what Miraqab + FFmpeg actually cost on THIS machine.

Starts a private Xvfb, an animated test window (tests/helpers/xwin.c), records for a fixed
time with each configuration and reports CPU% (cpu seconds / wall seconds, one core = 100%),
peak RSS of the heaviest process, and output size. Results are only valid for the machine
they ran on; docs/performance.md says which machine produced the published numbers.

usage: scripts/bench.py [--seconds 12] [--bin target/release/miraqab]
"""
import argparse, os, resource, subprocess, sys, tempfile, time, shutil

ap = argparse.ArgumentParser()
ap.add_argument("--seconds", type=int, default=12)
ap.add_argument("--bin", default="target/release/miraqab")
a = ap.parse_args()
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
binp = os.path.join(root, a.bin) if not os.path.isabs(a.bin) else a.bin
tmp = tempfile.mkdtemp(prefix="miraqab-bench-")
xwin = os.path.join(tmp, "xwin")
subprocess.check_call(["gcc", "-O2", "-o", xwin, os.path.join(root, "tests/helpers/xwin.c"), "-lX11"])
disp = ":97"
xvfb = subprocess.Popen(["Xvfb", disp, "-screen", "0", "1920x1080x24", "-nolisten", "tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.5)
env = dict(os.environ, DISPLAY=disp)
win = subprocess.Popen([xwin, "1280x720", str(a.seconds * 20 + 60)], stdout=subprocess.PIPE, env=env, text=True)
wid = win.stdout.readline().strip()
time.sleep(1)

cases = [
    ("ultralight 240p10 window", ["--profile", "ultralight", "--source", f"window:{wid}"]),
    ("480p10 window", ["--fps", "10", "--res", "480p", "--source", f"window:{wid}"]),
    ("480p15 window", ["--fps", "15", "--res", "480p", "--source", f"window:{wid}"]),
    ("720p30 window", ["--profile", "balanced", "--source", f"window:{wid}"]),
    ("720p30 full monitor 1920x1080", ["--profile", "balanced", "--source", "monitor:1"]),
    ("1080p30 full monitor (high)", ["--profile", "high", "--source", "monitor:1"]),
]
print(f"{'case':34} {'cpu% (1 core=100)':>18} {'peak RSS MB':>12} {'size MB':>8} {'secs':>5}")
for name, args in cases:
    out = os.path.join(tmp, "out")
    shutil.rmtree(out, ignore_errors=True)
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.time()
    r = subprocess.run([binp, "record", "--no-countdown", "--lang", "en", "--out", out, "--duration", str(a.seconds)] + args,
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    wall = time.time() - t0
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)
    files = [f for f in os.listdir(out)] if os.path.isdir(out) else []
    size = sum(os.path.getsize(os.path.join(out, f)) for f in files if os.path.isfile(os.path.join(out, f))) / 1e6
    status = "" if r.returncode == 0 else f"  (exit {r.returncode})"
    print(f"{name:34} {100*cpu/wall:18.0f} {after.ru_maxrss/1024:12.0f} {size:8.2f} {wall:5.1f}{status}")
win.kill(); xvfb.kill(); shutil.rmtree(tmp, ignore_errors=True)
