//! End-to-end recording tests: real Xvfb, real windows, real FFmpeg, real output files
//! inspected with FFprobe. Skipped (with a message) when Xvfb/gcc/ffmpeg are missing.
#![cfg(target_os = "linux")]
mod common;
use common::*;
use miraqab::capture::{Backend, Source, X11Backend};
use miraqab::ffmpeg::Ffmpeg;
use miraqab::geometry::Rect;
use miraqab::recorder::{self, AdaptiveConfig, Cmd, Handle, Outcome, OutcomeKind, Request};
use miraqab::settings::{Container, ResizeMode, Resolution, Settings};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn settings(out: &Path) -> Settings {
    let mut s = Settings::default();
    s.set_profile(miraqab::settings::Profile::Low); // 15 fps, 480p, ultrafast
    s.output_dir = out.to_path_buf();
    s.countdown = 0;
    s
}

fn request(st: Settings, src: Source, tone: Option<u32>) -> Request {
    Request { settings: st, source: src, max_seconds: None, test_tone: tone, adaptive: AdaptiveConfig::default() }
}

fn begin(xs: &XServer, req: Request) -> Result<Handle, Vec<String>> {
    let ff = Ffmpeg::locate().expect("ffmpeg");
    let backend: Box<dyn Backend> = Box::new(X11Backend::new(&xs.display));
    recorder::start(req, backend, ff).map(|(h, _)| h)
}

fn record(xs: &XServer, req: Request, script: impl FnOnce(&Handle)) -> Outcome {
    let h = begin(xs, req).expect("start");
    script(&h);
    h.send(Cmd::Stop);
    h.wait()
}

/// RGB24 pixels of one frame (`seek` like "-ss 0.4" or "-sseof -0.4").
fn frame(path: &Path, seek: &[&str], w: u32, h: u32) -> Vec<u8> {
    let o = Command::new("ffmpeg").args(["-v", "error", "-nostdin"]).args(seek).arg("-i").arg(path).args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).stderr(Stdio::null()).output().unwrap();
    assert_eq!(o.stdout.len(), (w * h * 3) as usize, "frame size from {}", path.display());
    o.stdout
}

fn px(f: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * w + x) * 3) as usize;
    (f[i], f[i + 1], f[i + 2])
}

fn decodes_cleanly(path: &Path) -> bool {
    let o = Command::new("ffmpeg").args(["-v", "error", "-nostdin", "-i"]).arg(path).args(["-f", "null", "-"]).output().unwrap();
    o.status.success() && o.stderr.is_empty()
}

fn is_dark(p: (u8, u8, u8)) -> bool {
    p.0 < 40 && p.1 < 40 && p.2 < 40
}

#[test]
fn records_a_region_to_a_valid_480p_file() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let _w = TestWindow::open(&xs.display, "400x300", &[], 30);
    let out = temp_out("region");
    let o = record(&xs, request(settings(&out), Source::Region(Rect { x: 0, y: 0, w: 640, h: 480 }), None), |_| sleep(3500));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert_eq!((p.width, p.height), (854, 480), "480p canvas");
    assert_eq!(p.video_codec, "h264");
    assert_eq!(p.audio_streams, 0);
    assert!((p.duration - 3.5).abs() < 1.2, "duration {}", p.duration);
    assert!(p.video_frames >= 35, "frames {}", p.video_frames);
    assert!(decodes_cleanly(o.path.as_ref().unwrap()));
    assert!(o.path.as_ref().unwrap().extension().unwrap() == "mp4");
    // temp folder is gone after a successful finish
    assert!(std::fs::read_dir(&out).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".miraqab-")));
}

#[test]
fn records_the_whole_monitor() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1280x720");
    let out = temp_out("monitor");
    let mut st = settings(&out);
    st.resolution = Resolution::Original;
    let o = record(&xs, request(st, Source::Monitor(0), None), |_| sleep(2500));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert_eq!((p.width, p.height), (1280, 720));
}

#[test]
fn window_resize_keeps_one_stable_video_and_scales_the_picture() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1280x900");
    // 320x240 (4:3), after 3 s becomes 640x360 (16:9)
    let w = TestWindow::open(&xs.display, "320x240", &["3:640x360"], 30);
    let out = temp_out("resize");
    let mut st = settings(&out);
    st.resolution = Resolution::Height(360); // 640x360 canvas
    let src = Source::Window { id: w.id as u64, title: "Miraqab Test Window".into() };
    let o = record(&xs, request(st, src, None), |_| sleep(8000));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let path = o.path.clone().unwrap();
    let p = ffprobe(&path);
    assert_eq!((p.width, p.height), (640, 360), "output size must stay constant (Mode A)");
    assert!(o.segments >= 2, "a resize must start a new segment: {:?}", o.notes);
    assert!(o.notes.iter().any(|n| n.contains("640x360")), "the user is told about the resize: {:?}", o.notes);
    assert!(p.duration > 6.0 && p.duration < 9.0, "duration {} (a short gap at the resize is expected)", p.duration);
    assert!(decodes_cleanly(&path));

    // early frame: 4:3 picture pillarboxed (black bars at the sides, red on the left half of the picture)
    let early = frame(&path, &["-ss", "1.0"], 640, 360);
    assert!(is_dark(px(&early, 640, 20, 180)), "pillarbox bar at the start: {:?}", px(&early, 640, 20, 180));
    let (r, _, b) = px(&early, 640, 200, 180);
    assert!(r > 140 && b < 90, "left half of the 4:3 picture is red: {:?}", (r, b));
    // late frame: the enlarged 16:9 window fills the whole canvas, no bars
    let late = frame(&path, &["-sseof", "-1.0"], 640, 360);
    assert!(!is_dark(px(&late, 640, 20, 180)), "no bar after the window grew: {:?}", px(&late, 640, 20, 180));
    let (r, _, _) = px(&late, 640, 100, 180);
    let (_, _, b) = px(&late, 640, 540, 180);
    assert!(r > 140 && b > 140, "left red / right blue fill the canvas");
}

#[test]
fn shrinking_window_does_not_break_the_recording() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1280x900");
    let w = TestWindow::open(&xs.display, "640x480", &["2:320x240"], 30);
    let out = temp_out("shrink");
    let src = Source::Window { id: w.id as u64, title: "Miraqab Test Window".into() };
    let o = record(&xs, request(settings(&out), src, None), |_| sleep(6000));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let path = o.path.clone().unwrap();
    let p = ffprobe(&path);
    assert_eq!((p.width, p.height), (854, 480));
    assert!(p.duration > 4.5, "recording continued after the shrink: {} s ({:?})", p.duration, o.notes);
    assert!(decodes_cleanly(&path));
}

#[test]
fn pause_does_not_record_the_paused_time() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let _w = TestWindow::open(&xs.display, "300x300", &[], 30);
    let out = temp_out("pause");
    let o = record(&xs, request(settings(&out), Source::Region(Rect { x: 0, y: 0, w: 320, h: 240 }), None), |h| {
        sleep(2500);
        h.send(Cmd::Pause);
        sleep(3000);
        h.send(Cmd::Resume);
        sleep(2500);
    });
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert!(p.duration > 3.8 && p.duration < 6.2, "5 s recorded + 3 s paused must give about 5 s, got {}", p.duration);
    assert_eq!(o.segments, 2);
    assert!(decodes_cleanly(o.path.as_ref().unwrap()));
}

#[test]
fn closing_the_window_ends_safely_and_says_so() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let mut w = TestWindow::open(&xs.display, "320x240", &[], 30);
    let out = temp_out("closed");
    let src = Source::Window { id: w.id as u64, title: "Miraqab Test Window".into() };
    let h = begin(&xs, request(settings(&out), src, None)).unwrap();
    sleep(2500);
    w.close();
    // the supervisor must stop on its own, without a Stop command
    let t = std::time::Instant::now();
    while !h.is_finished() {
        assert!(t.elapsed() < Duration::from_secs(10), "recorder did not notice that the window closed");
        sleep(100);
    }
    let o = h.wait();
    assert_eq!(o.kind, OutcomeKind::Complete);
    assert!(o.notes.iter().any(|n| n.contains("أُغلقت") || n.contains("closed")), "{:?}", o.notes);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert!(p.duration > 1.5 && p.duration < 5.0, "{}", p.duration);
}

#[test]
fn starting_on_a_missing_window_fails_cleanly() {
    if !x11_tools_available() {
        return;
    }
    // (window state detection itself is covered in tests/x11_protocol.rs)
    let xs = XServer::start("1024x768");
    let out = temp_out("missing");
    let src = Source::Window { id: 0x7ffff00, title: "ghost".into() };
    let err = begin(&xs, request(settings(&out), src, None)).err().expect("must refuse");
    assert!(!err.is_empty());
}

#[test]
fn audio_is_recorded_and_measured() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let _w = TestWindow::open(&xs.display, "300x300", &[], 30);
    let out = temp_out("audio");
    let mut st = settings(&out);
    st.system_audio = true;
    let o = record(&xs, request(st, Source::Region(Rect { x: 0, y: 0, w: 320, h: 240 }), Some(440)), |_| sleep(3000));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert_eq!(p.audio_streams, 1);
    assert_eq!(o.audio.len(), 1);
    assert!(o.audio[0].label.contains("اختبار") || o.audio[0].label.contains("test"), "a synthetic tone must never be called system audio: {}", o.audio[0].label);
    let level = o.audio[0].level.expect("level measured from the real file");
    assert!(!level.is_silent() && level.max_db > -30.0, "{:?}", level);
    assert!(decodes_cleanly(o.path.as_ref().unwrap()));
}

#[test]
fn separate_audio_tracks_and_webm() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let _w = TestWindow::open(&xs.display, "300x300", &[], 30);
    let out = temp_out("tracks");
    let mut st = settings(&out);
    st.system_audio = true;
    st.mic = true;
    st.separate_tracks = true;
    st.container = Container::Mkv;
    let o = record(&xs, request(st, Source::Region(Rect { x: 0, y: 0, w: 320, h: 240 }), Some(300)), |_| sleep(2500));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    assert_eq!(ffprobe(o.path.as_ref().unwrap()).audio_streams, 2);

    let out2 = temp_out("webm");
    let mut st = settings(&out2);
    st.container = Container::WebM;
    st.system_audio = true;
    let o = record(&xs, request(st, Source::Region(Rect { x: 0, y: 0, w: 320, h: 240 }), Some(300)), |_| sleep(2500));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let p = ffprobe(o.path.as_ref().unwrap());
    assert_eq!((p.video_codec.as_str(), p.audio_streams), ("vp9", 1));
}

#[test]
fn missing_audio_device_and_bad_paths_are_reported_before_recording() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let out = temp_out("errors");
    // this sandbox has no sound server: asking for real system audio must fail clearly, not record silence
    if Command::new("sh").args(["-c", "command -v pactl >/dev/null || ! ffmpeg -hide_banner -sources pulse 2>/dev/null | grep -q ."]).status().unwrap().success() {
        let mut st = settings(&out);
        st.system_audio = true;
        let e = begin(&xs, request(st, Source::Desktop, None)).err().expect("no sound server must be an error");
        assert!(!e.is_empty());
    }
    // output folder under a regular file
    let f = out.join("file");
    std::fs::write(&f, b"x").unwrap();
    let mut st = settings(&f.join("sub"));
    st.system_audio = false;
    assert!(begin(&xs, request(st, Source::Desktop, None)).is_err());
    // incompatible container/codec
    let mut st = settings(&out);
    st.container = Container::WebM;
    st.apply("codec", "h264").unwrap();
    assert!(begin(&xs, request(st, Source::Desktop, None)).is_err());
    // nothing was left behind
    assert!(std::fs::read_dir(&out).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".miraqab-")));
}

#[test]
fn existing_recordings_are_never_overwritten() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let out = temp_out("noover");
    let precious = out.join("same.mp4");
    std::fs::write(&precious, b"do not touch").unwrap();
    let mut st = settings(&out);
    st.name = Some("same".into());
    let o = record(&xs, request(st, Source::Region(Rect { x: 0, y: 0, w: 320, h: 240 }), None), |_| sleep(1500));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    assert_eq!(std::fs::read(&precious).unwrap(), b"do not touch");
    assert!(o.path.unwrap().ends_with("same-2.mp4"));
}

#[test]
fn adaptive_resolution_mode_keeps_segments_and_joins_them() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1280x900");
    let w = TestWindow::open(&xs.display, "320x240", &["2.5:640x480"], 30);
    let out = temp_out("modeb");
    let mut st = settings(&out);
    st.resize_mode = ResizeMode::Adaptive;
    st.resolution = Resolution::Original;
    let src = Source::Window { id: w.id as u64, title: "Miraqab Test Window".into() };
    let o = record(&xs, request(st, src, None), |_| sleep(7000));
    assert_eq!(o.kind, OutcomeKind::Complete, "{:?}", o);
    let path = o.path.clone().unwrap();
    let p = ffprobe(&path);
    assert_eq!((p.width, p.height), (640, 480), "joined into the largest size");
    assert!(decodes_cleanly(&path));
    let seg_dir = o.segments_dir.clone().expect("Mode B keeps the original segments");
    let n = std::fs::read_dir(&seg_dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".mkv")).count();
    assert!(n >= 2, "segments kept: {}", n);
    let first = ffprobe(&seg_dir.join("seg0001.mkv"));
    assert_eq!((first.width, first.height), (320, 240), "the first segment keeps its own native size");
}

#[test]
fn interrupted_session_can_be_recovered() {
    if !x11_tools_available() {
        return;
    }
    let out = temp_out("recover");
    let tmp = out.join(".miraqab-crashed-tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    for i in 1..=2 {
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=2", "-pix_fmt", "yuv420p", "-c:v", "libx264", "-f", "matroska"])
            .arg(tmp.join(format!("seg{:04}.mkv", i)))
            .status()
            .unwrap();
        assert!(st.success());
    }
    let m = miraqab::storage::Manifest { final_path: out.join("crashed.mp4"), container: "mp4".into(), mode: "fixed".into(), started: 1, pid: 4_000_000_000 };
    std::fs::write(tmp.join(miraqab::storage::MANIFEST_NAME), m.to_text()).unwrap();
    assert_eq!(miraqab::storage::find_interrupted(&out), vec![tmp.clone()]);
    let ff = Ffmpeg::locate().unwrap();
    let rec: PathBuf = recorder::recover_dir(&ff, &tmp).expect("recover");
    let p = ffprobe(&rec);
    assert!((p.duration - 4.0).abs() < 0.5, "{}", p.duration);
    assert!(rec.file_name().unwrap().to_string_lossy().contains("recovered"));
    assert!(!tmp.exists(), "temp folder removed only after the verified join");
}
