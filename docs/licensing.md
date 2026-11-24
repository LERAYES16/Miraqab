# Licensing

- Miraqab's own code: MIT (`LICENSE`). It has **no third-party Rust dependencies**.
- **FFmpeg** is not part of this repository. Linux packages depend on the distribution's `ffmpeg`.
  The Windows zip produced by CI bundles an `ffmpeg.exe` from a third-party **GPL** build (BtbN). Distributing it means you must
  comply with the GPL for that binary (provide/offer its source, include its license). Check the build's own license files
  before publishing; this document is not legal advice. If you prefer, ship only `miraqab.exe` and ask users to install FFmpeg.
- H.264 (libx264) and AAC may be subject to patent licensing in some jurisdictions; VP9/Opus/AV1 are royalty-free options.
