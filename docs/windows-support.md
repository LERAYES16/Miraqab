# Windows support (UNVERIFIED)

The author had no Windows machine or Windows toolchain. What exists:

- `src/winapi.rs`: window enumeration (EnumWindows, filtering invisible/tool/cloaked windows), monitor enumeration,
  client-rect polling, `gdigrab` arguments, `GetLocalTime`, `GetDiskFreeSpaceExW`. It **compiles and type-checks on Linux**
  with `RUSTFLAGS="--cfg miraqab_wincheck" cargo check --lib`. It has never been linked or run.
- `src/sig.rs` Windows console handler: same status.
- CI (`.github/workflows/release.yml`) builds on `windows-latest`, smoke-runs `miraqab.exe version`, and bundles an `ffmpeg.exe`.
  **Never executed.** Expect to fix small compile or path problems on the first run.

Known limits even if everything works: no WASAPI loopback (system audio only via DirectShow devices like "Stereo Mix" or a
virtual cable); Arabic text in the classic Windows console is poor — use the browser panel; windows are captured by title
(`gdigrab title=`), so two windows with the same title are refused as ambiguous; supported targets are Windows 10/11
(older versions were not checked).

To help: run `miraqab doctor` and `miraqab windows` on Windows and open an issue with the output.
