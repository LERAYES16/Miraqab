# Architecture

Std-only Rust (zero crates, edition 2021). FFmpeg runs as an external process.

| Module | Job |
|---|---|
| `settings` | profiles, resolution/fps presets, validation, config file (`~/.config/miraqab/config.conf`) |
| `geometry` | integer letterbox fitting (`fit`) |
| `capture` | `Source`, `Backend` trait, source parsing, session detection (X11/Wayland) |
| `x11` (Linux) | minimal X11 wire-protocol client: window list/state, RandR monitors |
| `winapi` (Windows) | user32 calls for window/monitor discovery, `gdigrab` arguments (**unverified**) |
| `ffmpeg` | locate/probe FFmpeg, build filter graph and arguments, parse `-progress` |
| `audio` | list devices, prove they open, measure and verify levels (`volumedetect`) |
| `recorder` | supervisor thread: segments, geometry polling, pause, adaptive resources, shutdown |
| `join` | concat segments, verify, publish without overwrite |
| `storage` | names, free space, session manifest, interrupted-session discovery, history |
| `app` / `main` | CLI |
| `gui` + `assets/ui.html` | local HTTP control panel (127.0.0.1, random token, Host check, CSP) |
| `sig` | Ctrl-C / console control handler |

Design rules: bounded memory (no frame ever passes through Miraqab; line readers are lossy), every FFmpeg child is placed
in its own process group and (Linux) gets `PDEATHSIG` so it cannot outlive Miraqab, and nothing is reported as success
unless it was checked (output has video, audio samples exist, duration is within tolerance).
