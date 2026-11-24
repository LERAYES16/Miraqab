# Contributing
- `cargo test` runs unit tests and, if `Xvfb`, `gcc` (+libx11-dev) and `ffmpeg` are installed, real recording tests (otherwise they print SKIPPED).
- Keep it dependency-free unless a crate removes real complexity; justify any addition in the PR.
- Never claim a feature works on a platform you did not run it on. Put measured numbers in `docs/performance.md` with the machine described.
- Windows type-check on Linux: `RUSTFLAGS="--cfg miraqab_wincheck" cargo check --lib`.
