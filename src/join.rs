//! Joining recording segments into the final file, safely.
//!
//! Rules (from the project brief):
//! - never present a broken join as a finished recording: every join is written
//!   to a `.part` file, verified, and only then published;
//! - never overwrite an existing file;
//! - never delete segments until the verified final file exists.

use crate::ffmpeg::{self, Caps, Ffmpeg, MediaInfo};
use crate::geometry::{fit, Size};
use crate::settings::{Container, Settings};
use crate::storage::unique_path;
use crate::tr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Segment {
    pub path: PathBuf,
    pub info: MediaInfo,
}

pub fn muxer_for(c: Container) -> &'static str {
    match c {
        Container::Mp4 => "mp4",
        Container::Mkv => "matroska",
        Container::WebM => "webm",
    }
}

/// Content of an FFmpeg concat-demuxer list (single quotes escaped as `'\''`).
pub fn concat_list_text(segs: &[PathBuf]) -> String {
    segs.iter().map(|p| format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"))).collect()
}

fn run_ffmpeg(ff: &Ffmpeg, args: &[String]) -> Result<(), String> {
    let out = ff.command().args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | "))
    }
}

/// Lossless join: same codec parameters in every segment, so packets are copied as they are.
pub fn join_copy(ff: &Ffmpeg, segs: &[PathBuf], container: Container, out: &Path) -> Result<(), String> {
    let list = out.with_extension("txt");
    std::fs::write(&list, concat_list_text(segs)).map_err(|e| e.to_string())?;
    let mut a: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "concat", "-safe", "0", "-i"].iter().map(|x| x.to_string()).collect();
    a.push(list.display().to_string());
    a.extend(["-map", "0", "-c", "copy"].iter().map(|x| x.to_string()));
    if container == Container::Mp4 {
        a.extend(["-movflags", "+faststart"].iter().map(|x| x.to_string()));
    }
    a.extend(["-f".to_string(), muxer_for(container).to_string(), out.display().to_string()]);
    let r = run_ffmpeg(ff, &a);
    let _ = std::fs::remove_file(&list);
    r
}

/// Join segments whose picture sizes differ (Mode B): every segment is scaled into the
/// common `canvas` with the same aspect-preserving fit as Mode A, then re-encoded once.
/// CPU-heavy, so it only runs at the end of a recording.
pub fn join_reencode(ff: &Ffmpeg, caps: &Caps, st: &Settings, segs: &[Segment], canvas: Size, fps: u32, out: &Path) -> Result<(), String> {
    let audio_n = segs.iter().map(|s| s.info.audio_streams).min().unwrap_or(0);
    let mut a: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|x| x.to_string()).collect();
    let mut graph = Vec::new();
    let mut concat_in = String::new();
    for (i, s) in segs.iter().enumerate() {
        a.extend(["-i".to_string(), s.path.display().to_string()]);
        let src = s.info.video.ok_or_else(|| tr!("المقطع {} بلا فيديو", "segment {} has no video", s.path.display()))?;
        let p = fit(src, canvas);
        let mut v = format!("[{}:v]scale={}:{}:flags=bicubic:out_color_matrix=bt709:out_range=tv", i, p.scaled.w, p.scaled.h);
        if p.needs_pad {
            v.push_str(&format!(",pad={}:{}:{}:{}:color=black", canvas.w, canvas.h, p.pad_x, p.pad_y));
        }
        v.push_str(&format!(",setsar=1,format=yuv420p,fps={}[v{}]", fps, i));
        graph.push(v);
        concat_in.push_str(&format!("[v{}]", i));
        for k in 0..audio_n {
            graph.push(format!("[{}:a:{}]aresample=48000,aformat=channel_layouts=stereo[a{}_{}]", i, k, i, k));
            concat_in.push_str(&format!("[a{}_{}]", i, k));
        }
    }
    let outs: String = std::iter::once("[v]".to_string()).chain((0..audio_n).map(|k| format!("[ja{}]", k))).collect();
    graph.push(format!("{}concat=n={}:v=1:a={}{}", concat_in, segs.len(), audio_n, outs));
    a.extend(["-filter_complex".to_string(), graph.join(";"), "-map".into(), "[v]".into()]);
    for k in 0..audio_n {
        a.extend(["-map".to_string(), format!("[ja{}]", k)]);
    }
    a.extend(ffmpeg::video_encoder_args(st, canvas, fps, &st.x264_preset(), caps)?);
    a.extend(ffmpeg::audio_encoder_args(st, caps, audio_n > 0)?);
    if st.container == Container::Mp4 {
        a.extend(["-movflags", "+faststart"].iter().map(|x| x.to_string()));
    }
    a.extend(["-f".to_string(), muxer_for(st.container).to_string(), out.display().to_string()]);
    run_ffmpeg(ff, &a)
}

/// A joined file is only accepted when it has video, the expected number of audio
/// streams and (nearly) the expected duration.
pub fn verify(ff: &Ffmpeg, out: &Path, expected_secs: f64, expect_audio: usize) -> Result<MediaInfo, String> {
    let info = ff.media_info(out).ok_or_else(|| tr!("تعذّر فحص الملف الناتج", "cannot inspect the joined file"))?;
    if info.video.is_none() {
        return Err(tr!("الملف الناتج لا يحتوي فيديو", "the joined file contains no video"));
    }
    if info.audio_streams != expect_audio {
        return Err(tr!("عدد المسارات الصوتية في الملف الناتج {} بدل {}", "the joined file has {} audio streams instead of {}", info.audio_streams, expect_audio));
    }
    let d = info.duration.ok_or_else(|| tr!("مدة الملف الناتج غير معروفة", "the joined file has no duration"))?;
    let tol = (expected_secs * 0.03).max(1.5);
    if (d - expected_secs).abs() > tol {
        return Err(tr!("مدة الملف الناتج {:.1} ث تختلف عن المتوقع {:.1} ث", "joined duration {:.1}s differs from the expected {:.1}s", d, expected_secs));
    }
    Ok(info)
}

/// Move a verified `.part` file to its final name without ever overwriting anything.
/// If the name was taken in the meantime a free `-N` variant is used. Returns the path used.
pub fn publish(part: &Path, final_path: &Path) -> Result<PathBuf, String> {
    let mut target = final_path.to_path_buf();
    if target.exists() {
        let dir = final_path.parent().unwrap_or(Path::new("."));
        let stem = final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("Miraqab");
        let ext = final_path.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
        target = unique_path(dir, stem, ext);
    }
    // hard_link fails if the target exists (no overwrite) and keeps the data safe if the
    // file system cannot do it we fall back to rename after re-checking.
    match std::fs::hard_link(part, &target) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            Ok(target)
        }
        Err(_) => {
            if target.exists() {
                return Err(tr!("الملف {} موجود", "{} already exists", target.display()));
            }
            std::fs::rename(part, &target).map_err(|e| tr!("تعذّر حفظ الملف النهائي: {}", "cannot store the final file: {}", e))?;
            Ok(target)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_list_escapes_quotes() {
        let t = concat_list_text(&[PathBuf::from("/a/seg1.mkv"), PathBuf::from("/it's/seg2.mkv")]);
        assert_eq!(t, "file '/a/seg1.mkv'\nfile '/it'\\''s/seg2.mkv'\n");
    }

    #[test]
    fn publish_never_overwrites() {
        let d = std::env::temp_dir().join(format!("miraqab-join-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let fin = d.join("rec.mp4");
        std::fs::write(&fin, b"precious").unwrap();
        let part = d.join("j.part");
        std::fs::write(&part, b"new").unwrap();
        let used = publish(&part, &fin).unwrap();
        assert_eq!(used, d.join("rec-2.mp4"));
        assert_eq!(std::fs::read(&fin).unwrap(), b"precious", "existing recording untouched");
        assert_eq!(std::fs::read(&used).unwrap(), b"new");
        assert!(!part.exists());
    }

    #[test]
    fn muxers() {
        assert_eq!(muxer_for(Container::WebM), "webm");
        assert_eq!(muxer_for(Container::Mkv), "matroska");
    }
}
