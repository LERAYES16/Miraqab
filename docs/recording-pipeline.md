# Recording pipeline

1. **Preflight** (`recorder::start`): validate settings, output folder (writable, free space), FFmpeg encoders, source, and
   prove each audio input opens. Any failure stops here with a message; nothing is recorded.
2. **Segments.** Each segment is one FFmpeg process writing Matroska (`.mkv`, tolerant of abrupt ends) into
   `.miraqab-<name>-tmp/`, with a `session.txt` manifest. A segment ends on pause, source resize, adaptive change or stop.
3. **Supervisor** (250 ms tick): polls the source geometry every 500 ms with a two-probe debounce, restarts on resize or on an
   unexpected FFmpeg exit, handles a vanished window (ends cleanly, marked complete with a note), checks free disk every 10 s
   (100 MB floor), and asks FFmpeg to stop with `q` on stdin (20 s timeout, then kill).
4. **Why restart on resize?** Measured with x11grab: a window capture does not follow resizes; shrinking below
   `-video_size` makes FFmpeg die with BadMatch while still exiting 0.
5. **Fixed mode (A):** constant canvas, integer aspect-preserving letterbox. **Adaptive mode (B):** each segment gets its own
   canvas (never upscaled); the final join re-encodes to the largest size; originals are kept in `<name>_segments/`.
6. **Join:** `concat` demuxer with `-c copy` (mode A) into a `.part` file; verify (video stream, audio stream count,
   duration within 3 % / 1.5 s); publish by hard-link-then-rename so an existing file is never replaced.
7. **Recovery:** `miraqab recover` joins the segments of an interrupted session; the result is named `-partial`.
