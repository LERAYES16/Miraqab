# Audio capture

- **Linux:** FFmpeg's `pulse` input. System sound = the monitor of the default sink (`@DEFAULT_MONITOR@`), microphone =
  `@DEFAULT_SOURCE@`; specific devices via `--system-device` / `--mic-device`. Listing uses `ffmpeg -sources pulse`.
- **Windows:** DirectShow only (unverified). No WASAPI loopback.
- **Mixed or separate:** by default system and mic are mixed with `amix` (`normalize=0` on FFmpeg ≥ 4.4); `--separate-tracks` writes two audio streams.
- **Proof, not promises:** before recording each input must open; after recording the final file is analysed with
  `volumedetect`. A present-but-silent track is reported as silent, and a file whose samples cannot be checked is reported as unverified.
- A built-in test tone (`--test-tone`) exists only for automated tests and is always labelled as such.
