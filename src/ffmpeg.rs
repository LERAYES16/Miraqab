//! Everything that touches the FFmpeg executable: finding it, asking what it
//! supports, building a segment command line, parsing its progress output and
//! inspecting the files it produces.
//!
//! Design decision (see docs/architecture.md): FFmpeg is used as an external
//! process, not linked. That keeps Miraqab free of C build dependencies and
//! of FFmpeg's license obligations in our own binary, lets every distro use its
//! own FFmpeg, and means an FFmpeg crash can never take the recorder down.

use crate::geometry::{fit, Size};
use crate::settings::{Codec, Settings};
use crate::tr;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Audio input description (platform specific FFmpeg input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioInput {
    /// PulseAudio / PipeWire-pulse source name, e.g. `@DEFAULT_MONITOR@` or `alsa_output….monitor`.
    Pulse(String),
    /// Windows DirectShow audio device name.
    Dshow(String),
    /// Synthetic sine tone. Only for diagnostics and automated tests; always labelled as such.
    TestTone(u32),
}

impl AudioInput {
    pub fn ffmpeg_input(&self) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            AudioInput::Pulse(d) => [v(&["-f", "pulse", "-i"]), vec![d.clone()]].concat(),
            AudioInput::Dshow(d) => [v(&["-f", "dshow", "-i"]), vec![format!("audio={}", d)]].concat(),
            AudioInput::TestTone(hz) => {
                [v(&["-f", "lavfi", "-i"]), vec![format!("sine=frequency={}:sample_rate=48000", hz)]].concat()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ffmpeg {
    pub path: PathBuf,
    pub version: (u32, u32),
    pub version_text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub encoders: HashSet<String>,
    /// Input/output devices (x11grab, pulse, gdigrab, dshow, ...).
    pub devices: HashSet<String>,
}

pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

fn exe_name(n: &str) -> String {
    if cfg!(windows) { format!("{}.exe", n) } else { n.to_string() }
}

/// Candidate paths in priority order: env override, next to our executable (bundled build), PATH.
pub fn candidate_paths(program: &str, env_override: Option<PathBuf>, exe_dir: Option<PathBuf>, path_var: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let name = exe_name(program);
    let mut v = Vec::new();
    if let Some(p) = env_override {
        v.push(p);
    }
    if let Some(d) = exe_dir {
        v.push(d.join(&name));
        v.push(d.join("bin").join(&name));
    }
    if let Some(p) = path_var {
        for dir in std::env::split_paths(&p) {
            v.push(dir.join(&name));
        }
    }
    v
}

pub fn find_program(program: &str, env_key: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    candidate_paths(program, std::env::var_os(env_key).map(PathBuf::from), exe_dir, std::env::var_os("PATH"))
        .into_iter()
        .find(|p| p.is_file())
}

/// `ffmpeg version 6.1.1-3ubuntu5 …` / `n7.0` / `N-113456-gabcdef` -> (major, minor).
/// Development builds (`N-…`) are treated as very new, which is what they are.
pub fn parse_version(text: &str) -> (u32, u32) {
    let first = text.lines().next().unwrap_or("");
    let after = first.split("version").nth(1).unwrap_or("").trim();
    let tok = after.split_whitespace().next().unwrap_or("");
    let tok = tok.trim_start_matches(['n', 'N']);
    if tok.starts_with('-') || tok.is_empty() || !tok.chars().next().unwrap().is_ascii_digit() {
        return (99, 0);
    }
    let mut it = tok.split(|c: char| !c.is_ascii_digit());
    let major = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

pub fn parse_encoders(text: &str) -> HashSet<String> {
    text.lines()
        .filter(|l| l.starts_with(" V") || l.starts_with(" A") || l.starts_with(" S"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|s| s.to_string())
        .collect()
}

pub fn parse_devices(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut past = false;
    for l in text.lines() {
        if l.trim_start().starts_with("--") {
            past = true;
            continue;
        }
        if past {
            if let Some(names) = l.split_whitespace().nth(1) {
                for n in names.split(',') {
                    out.insert(n.to_string());
                }
            }
        }
    }
    out
}

impl Ffmpeg {
    pub fn locate() -> Result<Ffmpeg, String> {
        let path = find_program("ffmpeg", "MIRAQAB_FFMPEG").ok_or_else(|| {
            tr!(
                "لم يُعثر على FFmpeg. ثبّته (مثلًا: sudo apt install ffmpeg) أو ضع ffmpeg بجوار البرنامج أو حدّد المسار في المتغير MIRAQAB_FFMPEG.",
                "FFmpeg was not found. Install it (e.g. sudo apt install ffmpeg), put ffmpeg next to the program, or set MIRAQAB_FFMPEG."
            )
        })?;
        let mut cmd = Command::new(&path);
        hide_console(&mut cmd);
        let out = cmd.args(["-hide_banner", "-version"]).output().map_err(|e| tr!("تعذّر تشغيل FFmpeg ({}): {}", "cannot run FFmpeg ({}): {}", path.display(), e))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() || !text.contains("ffmpeg version") {
            return Err(tr!("الملف {} ليس FFmpeg صالحًا", "{} is not a working FFmpeg", path.display()));
        }
        Ok(Ffmpeg { version: parse_version(&text), version_text: text.lines().next().unwrap_or("").to_string(), path })
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.path);
        hide_console(&mut c);
        c
    }

    pub fn caps(&self) -> Caps {
        let run = |args: &[&str]| -> String {
            self.command().args(["-hide_banner"]).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
        };
        Caps { encoders: parse_encoders(&run(&["-encoders"])), devices: parse_devices(&run(&["-devices"])) }
    }

    /// Duration, dimensions and stream layout of a media file, read with `ffmpeg -i`
    /// (so the recorder works even when `ffprobe` is not installed).
    pub fn media_info(&self, path: &Path) -> Option<MediaInfo> {
        let out = self.command().args(["-hide_banner", "-i"]).arg(path).output().ok()?;
        Some(parse_media_info(&String::from_utf8_lossy(&out.stderr)))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    pub video: Option<Size>,
    pub video_codec: Option<String>,
    pub audio_streams: usize,
}

pub fn parse_media_info(stderr: &str) -> MediaInfo {
    let mut m = MediaInfo::default();
    for l in stderr.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("Duration:") {
            let t = rest.split(',').next().unwrap_or("").trim();
            let p: Vec<&str> = t.split(':').collect();
            if p.len() == 3 {
                if let (Ok(h), Ok(mi), Ok(s)) = (p[0].parse::<f64>(), p[1].parse::<f64>(), p[2].parse::<f64>()) {
                    m.duration = Some(h * 3600.0 + mi * 60.0 + s);
                }
            }
        } else if l.starts_with("Stream #") && l.contains(": Video:") && m.video.is_none() && !l.contains("attached pic") {
            let after = l.split(": Video:").nth(1).unwrap_or("");
            m.video_codec = after.split_whitespace().next().map(|s| s.trim_end_matches(',').to_string());
            for seg in after.split(',') {
                let word = seg.split_whitespace().next().unwrap_or("");
                if let Some((w, h)) = word.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                        if w >= 16 && h >= 16 {
                            m.video = Some(Size::new(w, h));
                            break;
                        }
                    }
                }
            }
        } else if l.starts_with("Stream #") && l.contains(": Audio:") {
            m.audio_streams += 1;
        }
    }
    m
}

// ---- command line --------------------------------------------------------

pub struct SegmentPlan<'a> {
    pub settings: &'a Settings,
    /// FFmpeg input arguments for the video source (including `-f … -i …`).
    pub video_input: Vec<String>,
    /// Exact size FFmpeg will capture.
    pub capture: Size,
    /// Output (encoded) size.
    pub canvas: Size,
    /// Audio inputs with their volume in percent. Order: system first, microphone second.
    pub audio: Vec<(AudioInput, u32)>,
    pub output: PathBuf,
    /// Stop by itself after this many seconds (None: run until told to stop).
    pub max_seconds: Option<f64>,
    /// Frame rate used for this segment (may be lower than `settings.fps` when adaptive mode stepped down).
    pub fps: u32,
    pub preset: String,
}

/// The filter graph for one segment (pure function, unit-tested).
pub fn build_filter_graph(plan: &SegmentPlan, ver: (u32, u32)) -> String {
    let p = fit(plan.capture, plan.canvas);
    let flags = if plan.preset == "ultrafast" { "fast_bilinear" } else { "bicubic" };
    // One scale filter does the pixel-format conversion and, only if sizes differ, the resize.
    let mut v = format!("[0:v]scale={}:{}:flags={}:out_color_matrix=bt709:out_range=tv", p.scaled.w, p.scaled.h, flags);
    if p.needs_pad {
        v.push_str(&format!(",pad={}:{}:{}:{}:color=black", plan.canvas.w, plan.canvas.h, p.pad_x, p.pad_y));
    }
    v.push_str(",setsar=1,format=yuv420p[v]");
    let mut parts = vec![v];
    let n = plan.audio.len();
    for (i, (_, vol)) in plan.audio.iter().enumerate() {
        let mut f = format!("[{}:a]", i + 1);
        if *vol != 100 {
            f.push_str(&format!("volume={:.2},", *vol as f64 / 100.0));
        }
        f.push_str("aresample=async=1:first_pts=0");
        let label = if n == 1 || plan.settings.separate_tracks { format!("[a{}]", i + 1) } else { format!("[m{}]", i + 1) };
        f.push_str(&label);
        parts.push(f);
    }
    if n == 2 && !plan.settings.separate_tracks {
        let norm = if ver >= (4, 4) { ":normalize=0" } else { "" };
        parts.push(format!("[m1][m2]amix=inputs=2:duration=longest{}[a1]", norm));
    }
    parts.join(";")
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Video encoder options shared by live capture and the re-encoding join.
pub fn video_encoder_args(st: &Settings, canvas: Size, fps: u32, preset: &str, caps: &Caps) -> Result<Vec<String>, String> {
    let need = |enc: &str| -> Result<(), String> {
        if caps.encoders.contains(enc) {
            Ok(())
        } else {
            Err(tr!("إصدار FFmpeg الحالي لا يحتوي على المرمّز {} المطلوب", "this FFmpeg build does not include the required encoder {}", enc))
        }
    };
    let kbps = st.effective_bitrate(canvas);
    let br = format!("{}k", kbps);
    let mut a = Vec::new();
    match st.effective_codec() {
        Codec::H264 => {
            need("libx264")?;
            a.extend(owned(&["-c:v", "libx264", "-preset", preset]));
            a.extend(owned(&["-b:v", &br, "-maxrate", &br, "-bufsize", &format!("{}k", kbps * 2)]));
        }
        Codec::Vp9 => {
            need("libvpx-vp9")?;
            a.extend(owned(&["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-row-mt", "1", "-b:v", &br]));
        }
        Codec::Av1 => {
            if caps.encoders.contains("libsvtav1") {
                a.extend(owned(&["-c:v", "libsvtav1", "-preset", "10", "-b:v", &br]));
            } else if caps.encoders.contains("libaom-av1") {
                a.extend(owned(&["-c:v", "libaom-av1", "-usage", "realtime", "-cpu-used", "8", "-b:v", &br]));
            } else {
                return Err(tr!("لا يتوفر مرمّز AV1 في FFmpeg الحالي", "no AV1 encoder is available in this FFmpeg"));
            }
        }
    }
    a.extend(owned(&["-g", &(fps * 2).max(1).to_string(), "-pix_fmt", "yuv420p", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]));
    Ok(a)
}

/// Audio encoder options (empty when the recording has no audio).
pub fn audio_encoder_args(st: &Settings, caps: &Caps, has_audio: bool) -> Result<Vec<String>, String> {
    if !has_audio {
        return Ok(Vec::new());
    }
    let mut a = Vec::new();
    if st.container == crate::settings::Container::WebM {
        if !caps.encoders.contains("libopus") {
            return Err(tr!("إصدار FFmpeg الحالي لا يحتوي على المرمّز {} المطلوب", "this FFmpeg build does not include the required encoder {}", "libopus"));
        }
        a.extend(owned(&["-c:a", "libopus", "-b:a", "96k"]));
    } else {
        a.extend(owned(&["-c:a", "aac", "-b:a", "128k"]));
    }
    a.extend(owned(&["-ar", "48000", "-ac", "2"]));
    Ok(a)
}

/// Constant frame rate; the option was renamed in FFmpeg 5.1.
pub fn cfr_args(ver: (u32, u32), fps: u32) -> Vec<String> {
    let mode = if ver >= (5, 1) { owned(&["-fps_mode", "cfr"]) } else { owned(&["-vsync", "cfr"]) };
    [mode, owned(&["-r", &fps.to_string()])].concat()
}

pub fn build_args(plan: &SegmentPlan, ff: &Ffmpeg, caps: &Caps) -> Result<Vec<String>, String> {
    let st = plan.settings;
    let mut a = owned(&["-hide_banner", "-loglevel", "warning", "-nostats", "-progress", "pipe:1", "-y"]);

    // Every input gets a deep thread queue so a busy encoder never stalls the capture threads.
    let mut add_input = |args: &[String]| {
        a.extend(owned(&["-thread_queue_size", "512"]));
        a.extend(args.iter().cloned());
    };
    add_input(&plan.video_input);
    for (inp, _) in &plan.audio {
        add_input(&inp.ffmpeg_input());
    }

    a.push("-filter_complex".into());
    a.push(build_filter_graph(plan, ff.version));
    a.extend(owned(&["-map", "[v]"]));
    let n = plan.audio.len();
    if n == 1 || (n == 2 && !st.separate_tracks) {
        a.extend(owned(&["-map", "[a1]"]));
    } else if n == 2 {
        a.extend(owned(&["-map", "[a1]", "-map", "[a2]", "-metadata:s:a:0", "title=System audio", "-metadata:s:a:1", "title=Microphone"]));
    }
    a.extend(video_encoder_args(st, plan.canvas, plan.fps, &plan.preset, caps)?);
    a.extend(cfr_args(ff.version, plan.fps));
    a.extend(audio_encoder_args(st, caps, n > 0)?);
    if let Some(t) = plan.max_seconds {
        a.extend(["-t".to_string(), format!("{:.3}", t)]);
    }
    // Segments are always Matroska: it survives a crash or power loss, unlike MP4.
    a.extend(owned(&["-f", "matroska"]));
    a.push(plan.output.to_string_lossy().into_owned());
    Ok(a)
}

// ---- progress ------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub frame: u64,
    pub fps: f64,
    pub dup_frames: u64,
    pub drop_frames: u64,
    pub out_time_secs: f64,
    pub speed: Option<f64>,
    pub total_size: u64,
    pub ended: bool,
}

#[derive(Default)]
pub struct ProgressParser {
    cur: Progress,
}

impl ProgressParser {
    /// Feed one `key=value` line of `-progress` output. A complete block is returned at `progress=…`.
    pub fn feed(&mut self, line: &str) -> Option<Progress> {
        let (k, v) = line.trim().split_once('=')?;
        let v = v.trim();
        match k {
            "frame" => self.cur.frame = v.parse().unwrap_or(self.cur.frame),
            "fps" => self.cur.fps = v.parse().unwrap_or(0.0),
            "dup_frames" => self.cur.dup_frames = v.parse().unwrap_or(0),
            "drop_frames" => self.cur.drop_frames = v.parse().unwrap_or(0),
            "total_size" => self.cur.total_size = v.parse().unwrap_or(self.cur.total_size),
            "out_time_us" => {
                if let Ok(us) = v.parse::<i64>() {
                    self.cur.out_time_secs = (us.max(0) as f64) / 1e6;
                }
            }
            "speed" => self.cur.speed = v.trim_end_matches('x').trim().parse().ok(),
            "progress" => {
                self.cur.ended = v == "end";
                return Some(self.cur.clone());
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Container, Settings};

    fn caps_all() -> Caps {
        Caps { encoders: ["libx264", "libvpx-vp9", "libopus", "aac", "libsvtav1"].iter().map(|x| x.to_string()).collect(), devices: HashSet::new() }
    }
    fn ff(v: (u32, u32)) -> Ffmpeg {
        Ffmpeg { path: "ffmpeg".into(), version: v, version_text: String::new() }
    }
    fn plan<'a>(st: &'a Settings, capture: Size, canvas: Size, audio: Vec<(AudioInput, u32)>) -> SegmentPlan<'a> {
        SegmentPlan {
            settings: st,
            video_input: ["-f", "x11grab", "-framerate", "30", "-i", ":0.0"].iter().map(|x| x.to_string()).collect(),
            capture,
            canvas,
            audio,
            output: "/tmp/o.mkv".into(),
            max_seconds: None,
            fps: st.fps,
            preset: st.x264_preset(),
        }
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("ffmpeg version 6.1.1-3ubuntu5 Copyright"), (6, 1));
        assert_eq!(parse_version("ffmpeg version 4.2.7-0ubuntu0.1 Copyright"), (4, 2));
        assert_eq!(parse_version("ffmpeg version n7.0 Copyright"), (7, 0));
        assert_eq!(parse_version("ffmpeg version N-113456-gabcdef Copyright"), (99, 0));
        assert_eq!(parse_version("garbage"), (99, 0));
    }

    #[test]
    fn encoder_and_device_parsing() {
        let enc = " V....D libx264              libx264 H.264\n A....D libopus              libopus Opus\n ------\n";
        let e = parse_encoders(enc);
        assert!(e.contains("libx264") && e.contains("libopus"));
        let dev = "Devices:\n D. = Demuxing supported\n --\n D  x11grab        X11 screen capture\n DE video4linux2,v4l2 V4L2\n";
        let d = parse_devices(dev);
        assert!(d.contains("x11grab") && d.contains("v4l2") && d.contains("video4linux2"));
    }

    #[test]
    fn media_info_parsing_ignores_codec_tags() {
        let txt = "Input #0, matroska,webm, from 'a.mkv':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 10 kb/s\n  Stream #0:0: Video: h264 (High 4:4:4 Predictive), yuv420p(tv, bt709, progressive), 1280x720 [SAR 1:1 DAR 16:9], 30 fps, 30 tbr, 1k tbn\n  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp\n  Stream #0:2: Audio: aac (LC), 48000 Hz, stereo, fltp\n";
        let m = parse_media_info(txt);
        assert_eq!(m.duration, Some(62.5));
        assert_eq!(m.video, Some(Size::new(1280, 720)));
        assert_eq!(m.video_codec.as_deref(), Some("h264"));
        assert_eq!(m.audio_streams, 2);
        let tagged = "  Stream #0:0: Video: h264 (avc1 / 0x31637661), yuv420p, 320x240, 10 fps\n";
        assert_eq!(parse_media_info(tagged).video, Some(Size::new(320, 240)));
        assert_eq!(parse_media_info("nonsense"), MediaInfo::default());
    }

    #[test]
    fn filter_graph_no_rescale_when_sizes_match() {
        let st = Settings::default();
        let p = plan(&st, Size::new(1280, 720), Size::new(1280, 720), vec![]);
        let g = build_filter_graph(&p, (6, 1));
        assert!(g.contains("scale=1280:720"), "{}", g);
        assert!(!g.contains("pad="), "no padding when the picture fills the canvas: {}", g);
    }

    #[test]
    fn filter_graph_letterboxes_the_spec_example() {
        let st = Settings::default();
        let p = plan(&st, Size::new(640, 480), Size::new(1280, 720), vec![]);
        let g = build_filter_graph(&p, (6, 1));
        assert!(g.contains("scale=960:720"), "{}", g);
        assert!(g.contains("pad=1280:720:160:0"), "{}", g);
        let big = plan(&st, Size::new(1920, 1080), Size::new(1280, 720), vec![]);
        let g2 = build_filter_graph(&big, (6, 1));
        assert!(g2.contains("scale=1280:720") && !g2.contains("pad="), "{}", g2);
    }

    #[test]
    fn audio_mixing_variants() {
        let mut st = Settings::default();
        st.system_audio = true;
        st.mic = true;
        let two = vec![(AudioInput::Pulse("a".into()), 100), (AudioInput::Pulse("b".into()), 150)];
        let g = build_filter_graph(&plan(&st, Size::new(640, 360), Size::new(640, 360), two.clone()), (6, 1));
        assert!(g.contains("amix=inputs=2") && g.contains("normalize=0") && g.contains("volume=1.50"), "{}", g);
        let old = build_filter_graph(&plan(&st, Size::new(640, 360), Size::new(640, 360), two.clone()), (4, 2));
        assert!(!old.contains("normalize"), "FFmpeg < 4.4 has no normalize option: {}", old);
        st.separate_tracks = true;
        let args = build_args(&plan(&st, Size::new(640, 360), Size::new(640, 360), two), &ff((6, 1)), &caps_all()).unwrap();
        assert!(args.iter().filter(|a| *a == "-map").count() == 3, "{:?}", args);
        assert!(args.contains(&"title=Microphone".to_string()));
    }

    #[test]
    fn args_for_each_codec_and_version() {
        let mut st = Settings::default();
        let p = plan(&st, Size::new(1280, 720), Size::new(1280, 720), vec![]);
        let a = build_args(&p, &ff((6, 1)), &caps_all()).unwrap();
        assert!(a.contains(&"libx264".to_string()) && a.contains(&"-fps_mode".to_string()));
        assert!(!a.contains(&"-c:a".to_string()), "no audio codec without audio");
        let a = build_args(&p, &ff((4, 4)), &caps_all()).unwrap();
        assert!(a.contains(&"-vsync".to_string()) && !a.contains(&"-fps_mode".to_string()));
        assert_eq!(a.last().unwrap(), "/tmp/o.mkv");
        // input options come before -i and every input has a queue
        let q = a.iter().position(|x| x == "-thread_queue_size").unwrap();
        let i = a.iter().position(|x| x == "-i").unwrap();
        assert!(q < i);
        // a missing encoder is reported, not ignored
        assert!(build_args(&p, &ff((6, 1)), &Caps::default()).is_err());

        st.container = Container::WebM;
        let aud = vec![(AudioInput::Pulse("x".into()), 100)];
        let w = build_args(&plan(&st, Size::new(1280, 720), Size::new(1280, 720), aud), &ff((6, 1)), &caps_all()).unwrap();
        assert!(w.contains(&"libvpx-vp9".to_string()) && w.contains(&"libopus".to_string()));

        st.codec = Some(Codec::Av1);
        st.container = Container::Mkv;
        let mut only_aom = caps_all();
        only_aom.encoders.remove("libsvtav1");
        assert!(build_args(&plan(&st, Size::new(640, 360), Size::new(640, 360), vec![]), &ff((6, 1)), &only_aom).is_err());
    }

    #[test]
    fn candidate_order_prefers_override_then_bundled_then_path() {
        let c = candidate_paths("ffmpeg", Some("/x/ff".into()), Some("/app".into()), Some("/usr/bin:/bin".into()));
        assert_eq!(c[0], PathBuf::from("/x/ff"));
        assert!(c[1].starts_with("/app"));
        assert!(c.last().unwrap().starts_with("/bin"));
    }

    #[test]
    fn progress_blocks() {
        let mut p = ProgressParser::default();
        let mut got = None;
        for l in "frame=10\nfps=9.9\ndrop_frames=2\ndup_frames=1\nout_time_us=1500000\nspeed=0.98x\nprogress=continue".lines() {
            got = p.feed(l).or(got);
        }
        let g = got.unwrap();
        assert_eq!((g.frame, g.drop_frames, g.dup_frames), (10, 2, 1));
        assert!((g.out_time_secs - 1.5).abs() < 1e-9);
        assert_eq!(g.speed, Some(0.98));
        assert!(!g.ended);
        assert!(p.feed("progress=end").unwrap().ended);
        assert_eq!(p.feed("speed=N/A\n"), None);
    }
}
