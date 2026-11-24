# Linux support

- **Session:** X11 only. Wayland-only sessions: `desktop`, `monitor` and `region` are refused with an explanation; windows
  under XWayland are allowed with a warning. No Wayland capture (portal/PipeWire) is implemented or claimed.
- **Dependencies at runtime:** `ffmpeg` (with `x11grab`, `libx264`; `libvpx`/`libopus` for webm). Audio listing needs FFmpeg
  built with `pulse`. Works with PulseAudio or PipeWire's pulse compatibility — **only the synthetic path was tested here**.
- **Binaries:** the glibc build made in this sandbox requires **glibc ≥ 2.39** (checked with `objdump -T`), which excludes e.g.
  Ubuntu 22.04 / Mint 21. For "any distro" use the **static musl build** from CI (`x86_64-unknown-linux-musl`, unverified until
  CI runs). Building from source on your own distro always works.
- **Tested distribution:** only the sandbox's Ubuntu. Mint, Fedora, Arch etc. were not tested.
