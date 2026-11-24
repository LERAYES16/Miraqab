//! Miraqab (مِرقاب): a lightweight, Arabic-first screen recorder built on FFmpeg.
//!
//! Layout:
//! - `settings`, `geometry`, `i18n`: pure logic, fully unit-tested
//! - `x11` (Unix) / `winapi` (Windows): window & monitor discovery without any C library bindings
//! - `capture`: platform backends behind one interface
//! - `ffmpeg`: locating FFmpeg and building its command lines
//! - `recorder`: the recording session supervisor (segments, pause, resize, finalization)
//! - `storage`: output paths, free space, history, crash-recovery manifests

pub mod app;
pub mod audio;
pub mod capture;
pub mod ffmpeg;
pub mod geometry;
pub mod gui;
pub mod i18n;
pub mod join;
pub mod recorder;
pub mod settings;
pub mod sig;
pub mod storage;
#[cfg(any(windows, miraqab_wincheck))]
pub mod winapi;
#[cfg(target_os = "linux")]
pub mod x11;
