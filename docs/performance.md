# Performance (measured, with the machine stated)

**Machine:** cloud sandbox VM, **2 vCPU**, Xvfb virtual display (no GPU, no real compositor), FFmpeg 6.1.1, release build.
This is **not** an Intel i3-4005U and not a real desktop; real-world numbers will differ, possibly a lot.
Method: `scripts/bench.py` records an animated test window / the 1920x1080 virtual screen for 12 s per case.
CPU% = (user+system CPU seconds of Miraqab and FFmpeg) / wall seconds, so 100 % = one full core. Peak RSS is the largest
single process (FFmpeg). Output sizes are not shown because the synthetic animation compresses unrealistically well.

| Case | CPU % | Peak RSS |
|---|---:|---:|
| Ultra Light, 240p 10 fps, window | 4 | 58 MB |
| 480p 10 fps, window | 7 | 94 MB |
| 480p 15 fps, window | 9 | 94 MB |
| Balanced, 720p 30 fps, window | 16 | 137 MB |
| Balanced, 720p 30 fps, full 1080p screen | 25 | 138 MB |
| High, 1080p 30 fps, full screen | 33 | 279 MB |

Miraqab itself: ~4 MB resident when idle (shown by `miraqab doctor`); binary ≈ 0.8 MB. Not measured: resize/pause gap length,
audio overhead, real screen content, long recordings, the i3-4005U. Reproduce with `python3 scripts/bench.py`.
