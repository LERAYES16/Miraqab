//! The recording session supervisor.
//!
//! A recording is a series of *segments*. Each segment is one FFmpeg process
//! writing a Matroska file (crash tolerant). A new segment starts after a
//! pause, when the captured window changes size, or when the adaptive load
//! manager changes the frame rate. At the end the segments are joined into the
//! requested container, verified, and only then published.
//!
//! Threads: one supervisor thread (wakes 4x/second, does almost nothing) plus
//! two tiny reader threads per FFmpeg process draining its progress/log pipes.
//! Memory is bounded: Miraqab never holds frames; FFmpeg's own queues are bounded
//! (`-thread_queue_size`) and its stderr tail is capped at 30 lines.

use crate::audio::{self, Level};
use crate::capture::{Availability, Backend, Source};
use crate::ffmpeg::{self, AudioInput, Caps, Ffmpeg, MediaInfo, Progress, ProgressParser, SegmentPlan};
use crate::geometry::{fit, Rect, Size};
use crate::join::{self, Segment};
use crate::settings::{Codec, ResizeMode, Resolution, Settings, FPS_PRESETS};
use crate::storage::{self, Manifest};
use crate::tr;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
pub struct AdaptiveConfig {
    /// Seconds of continuous overload before quality is stepped down.
    pub sustain_secs: u32,
    /// Seconds of stable running before a step back up is attempted.
    pub recover_secs: u32,
    /// FFmpeg `speed` (1.0 = real time) below which the machine counts as overloaded.
    pub speed_threshold: f64,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        AdaptiveConfig { sustain_secs: 6, recover_secs: 60, speed_threshold: 0.92 }
    }
}

pub struct Request {
    pub settings: Settings,
    pub source: Source,
    /// Stop by itself after this much *recorded* time (pauses do not count).
    pub max_seconds: Option<f64>,
    /// Diagnostics/tests only: use a synthetic tone instead of real audio devices.
    pub test_tone: Option<u32>,
    pub adaptive: AdaptiveConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Recording,
    Paused,
    /// Paused by Miraqab because the window is minimised/hidden; resumes by itself.
    AutoPaused,
    Finalizing,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub state: State,
    pub recorded_secs: f64,
    pub frames: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub speed: Option<f64>,
    pub bytes: u64,
    pub segment: usize,
    pub capture: Size,
    pub canvas: Size,
    pub fps: u32,
    pub ffmpeg_cpu_percent: Option<f64>,
    pub ffmpeg_rss_mb: Option<f64>,
    pub free_bytes: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct StartInfo {
    pub final_path: PathBuf,
    pub canvas: Size,
    pub fps: u32,
    pub kbps: u32,
    pub codec: &'static str,
    pub audio_labels: Vec<String>,
    pub est_mb_per_min: f64,
    pub free_bytes: Option<u64>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Event {
    Started(StartInfo),
    Notice(String),
    Warning(String),
    Status(Status),
}

#[derive(Clone, Copy, Debug)]
pub enum Cmd {
    Pause,
    Resume,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeKind {
    Complete,
    /// A file exists but the recording ended abnormally (reason inside).
    Partial(String),
    /// No usable final file was produced (reason inside).
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct AudioReport {
    pub label: String,
    pub level: Option<Level>,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub kind: OutcomeKind,
    pub path: Option<PathBuf>,
    /// Directory holding the raw segments (kept for Mode B, or when joining failed).
    pub segments_dir: Option<PathBuf>,
    pub duration: f64,
    pub bytes: u64,
    pub segments: usize,
    pub dropped: u64,
    pub duplicated: u64,
    pub audio: Vec<AudioReport>,
    pub notes: Vec<String>,
}

// ---- adaptive load management (pure, unit-tested) --------------------------------

/// The ladder of (fps, x264 preset) steps from the user's settings downwards.
/// Resolution and bitrate are never touched.
pub fn ladder(fps: u32, preset: &str, h264: bool) -> Vec<(u32, String)> {
    let mut v = vec![(fps, preset.to_string())];
    if h264 && preset != "ultrafast" {
        v.push((fps, "ultrafast".to_string()));
    }
    let keep = v.last().unwrap().1.clone();
    let mut cur = fps;
    for _ in 0..2 {
        match FPS_PRESETS.iter().rev().find(|&&f| f < cur && f >= 5) {
            Some(&f) => {
                v.push((f, keep.clone()));
                cur = f;
            }
            None => break,
        }
    }
    v
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Degrade,
    Recover,
}

pub struct Adapt {
    cfg: AdaptiveConfig,
    pub level: usize,
    max_level: usize,
    over: u32,
    ok: u32,
    backoff: u32,
    since_recover: u32,
}

impl Adapt {
    pub fn new(cfg: AdaptiveConfig, max_level: usize) -> Adapt {
        Adapt { cfg, level: 0, max_level, over: 0, ok: 0, backoff: 1, since_recover: u32::MAX / 2 }
    }

    /// Call once per second with FFmpeg's current speed factor.
    pub fn on_second(&mut self, speed: Option<f64>) -> Option<Move> {
        self.since_recover = self.since_recover.saturating_add(1);
        let overloaded = speed.map(|s| s < self.cfg.speed_threshold).unwrap_or(false);
        if overloaded {
            self.ok = 0;
            self.over += 1;
            if self.over >= self.cfg.sustain_secs && self.level < self.max_level {
                // degrading soon after a recovery attempt means the attempt was too eager
                if self.since_recover < self.cfg.recover_secs {
                    self.backoff = (self.backoff * 2).min(8);
                }
                self.level += 1;
                self.over = 0;
                return Some(Move::Degrade);
            }
        } else {
            self.over = 0;
            if self.level > 0 {
                self.ok += 1;
                if self.ok >= self.cfg.recover_secs * self.backoff {
                    self.level -= 1;
                    self.ok = 0;
                    self.since_recover = 0;
                    return Some(Move::Recover);
                }
            }
        }
        None
    }
}

/// Mode B canvas: the source size, scaled down (never up) to fit the chosen resolution limit.
pub fn adaptive_canvas(st: &Settings, src: Size) -> Size {
    let limit = match st.resolution {
        Resolution::Original => return src.even(),
        other => other.canvas(src),
    };
    if src.w <= limit.w && src.h <= limit.h {
        src.even()
    } else {
        fit(src, limit).scaled
    }
}

// ---- Linux process statistics ----------------------------------------------------

#[cfg(target_os = "linux")]
fn proc_ticks_and_rss(pid: u32) -> Option<(u64, f64)> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
    Some((ticks, kb / 1024.0))
}

#[cfg(not(target_os = "linux"))]
fn proc_ticks_and_rss(_pid: u32) -> Option<(u64, f64)> {
    None
}

#[cfg(target_os = "linux")]
fn clk_tck() -> f64 {
    extern "C" {
        fn sysconf(name: i32) -> i64;
    }
    // SAFETY: sysconf(_SC_CLK_TCK = 2) has no preconditions.
    let v = unsafe { sysconf(2) };
    if v > 0 { v as f64 } else { 100.0 }
}

/// Resident memory of this process in MB (Linux), for diagnostics.
pub fn self_rss_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        proc_ticks_and_rss(std::process::id()).map(|x| x.1)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ---- segments ----------------------------------------------------------------------

struct Seg {
    child: Child,
    stdin: Option<ChildStdin>,
    prog: Receiver<Progress>,
    tail: Arc<Mutex<VecDeque<String>>>,
    qwarn: Arc<AtomicU64>,
    readers: Vec<JoinHandle<()>>,
    path: PathBuf,
    capture: Size,
    canvas: Size,
    started: Instant,
    last: Progress,
}

#[derive(Clone, Debug)]
struct SegRec {
    path: PathBuf,
    info: Option<MediaInfo>,
    secs: f64,
    dropped: u64,
    dup: u64,
}

fn read_lines_lossy<R: Read>(mut r: R, mut f: impl FnMut(String)) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    f(String::from_utf8_lossy(&line).trim_end().to_string());
                }
                if buf.len() > 1 << 20 {
                    buf.clear(); // pathological output without newlines: stay bounded
                }
            }
        }
    }
}

#[derive(Debug)]
enum End {
    User,
    Limit,
    SourceGone,
    DiskFull,
    Error(String),
}

struct Sup {
    req: Request,
    ff: Ffmpeg,
    caps: Caps,
    backend: Box<dyn Backend>,
    audio: Vec<(AudioInput, u32)>,
    canvas: Size,
    tmp: PathBuf,
    final_path: PathBuf,
    ev: Sender<Event>,
    seg: Option<Seg>,
    recs: Vec<SegRec>,
    next_idx: usize,
    state: State,
    pending: Option<(Size, u32)>,
    fast_fail: u32,
    ladder: Vec<(u32, String)>,
    adapt: Adapt,
    notes: Vec<String>,
    cpu_prev: Option<(u64, Instant)>,
    cpu_now: Option<f64>,
    rss_now: Option<f64>,
    free_now: Option<u64>,
}

impl Sup {
    fn say(&self, e: Event) {
        let _ = self.ev.send(e);
    }
    fn notice(&mut self, s: String) {
        self.notes.push(s.clone());
        self.say(Event::Notice(s));
    }

    fn cur(&self) -> (u32, String) {
        self.ladder[self.adapt.level.min(self.ladder.len() - 1)].clone()
    }

    fn total_recorded(&self) -> f64 {
        self.recs.iter().map(|r| r.secs).sum::<f64>() + self.seg.as_ref().map(|s| s.last.out_time_secs).unwrap_or(0.0)
    }

    fn begin_segment(&mut self, rect: Rect) -> Result<(), String> {
        let st = &self.req.settings;
        let canvas = match st.resize_mode {
            ResizeMode::Fixed => self.canvas,
            ResizeMode::Adaptive => adaptive_canvas(st, rect.size()),
        };
        let (fps, preset) = self.cur();
        self.next_idx += 1;
        let path = self.tmp.join(format!("seg{:04}.mkv", self.next_idx));
        let remaining = self.req.max_seconds.map(|l| (l - self.total_recorded()).max(0.2));
        let plan = SegmentPlan {
            settings: st,
            video_input: self.backend.video_input(&self.req.source, rect, fps, st.draw_mouse),
            capture: rect.size(),
            canvas,
            audio: self.audio.clone(),
            output: path.clone(),
            max_seconds: remaining,
            fps,
            preset,
        };
        let args = ffmpeg::build_args(&plan, &self.ff, &self.caps)?;
        let mut cmd = self.ff.command();
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // Ctrl-C in the terminal must not hit FFmpeg directly
            #[cfg(target_os = "linux")]
            {
                extern "C" {
                    fn prctl(option: i32, ...) -> i32;
                }
                // SAFETY: only async-signal-safe prctl(PR_SET_PDEATHSIG, SIGTERM) runs between fork and exec.
                // If Miraqab dies, FFmpeg gets SIGTERM, finalizes its segment and exits instead of recording forever.
                unsafe {
                    cmd.pre_exec(|| {
                        prctl(1, 15 as std::os::raw::c_ulong);
                        Ok(())
                    });
                }
            }
        }
        let mut child = cmd.spawn().map_err(|e| tr!("تعذّر تشغيل FFmpeg: {}", "cannot start FFmpeg: {}", e))?;
        let (ptx, prx) = mpsc::channel();
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let qwarn = Arc::new(AtomicU64::new(0));
        let mut readers = Vec::new();
        let out = child.stdout.take().unwrap();
        readers.push(std::thread::spawn(move || {
            let mut p = ProgressParser::default();
            read_lines_lossy(out, |l| {
                if let Some(pr) = p.feed(&l) {
                    let _ = ptx.send(pr);
                }
            });
        }));
        let err = child.stderr.take().unwrap();
        let (t2, q2) = (tail.clone(), qwarn.clone());
        readers.push(std::thread::spawn(move || {
            read_lines_lossy(err, |l| {
                if l.contains("queue blocking") {
                    q2.fetch_add(1, Ordering::Relaxed);
                }
                if let Ok(mut t) = t2.lock() {
                    if t.len() >= 30 {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            });
        }));
        let stdin = child.stdin.take();
        self.seg = Some(Seg { child, stdin, prog: prx, tail, qwarn, readers, path, capture: rect.size(), canvas, started: Instant::now(), last: Progress::default() });
        self.state = State::Recording;
        self.pending = None;
        self.cpu_prev = None;
        Ok(())
    }

    fn drain_progress(&mut self) {
        if let Some(s) = self.seg.as_mut() {
            while let Ok(p) = s.prog.try_recv() {
                s.last = p;
            }
        }
    }

    /// Ask FFmpeg to finish (so the container is closed properly), wait, collect the segment.
    fn stop_segment(&mut self) -> Option<SegRec> {
        let mut seg = self.seg.take()?;
        if let Some(mut si) = seg.stdin.take() {
            let _ = si.write_all(b"q");
            let _ = si.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match seg.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if Instant::now() > deadline => {
                    let _ = seg.child.kill();
                    let _ = seg.child.wait();
                    self.notice(tr!("لم يُنهِ FFmpeg عمله في الوقت المحدد فأُجبر على الإغلاق؛ قد يفقد المقطع آخر لحظات.", "FFmpeg did not finish in time and was stopped; the last moments of this segment may be lost."));
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            }
        }
        for r in seg.readers.drain(..) {
            let _ = r.join();
        }
        while let Ok(p) = seg.prog.try_recv() {
            seg.last = p;
        }
        let q = seg.qwarn.load(Ordering::Relaxed);
        if q > 0 {
            self.notes.push(tr!("تحذير FFmpeg: امتلأ طابور الإدخال {} مرة (علامة على حمل زائد).", "FFmpeg warned {} times that its input queue was full (a sign of overload).", q));
        }
        let info = self.ff.media_info(&seg.path);
        let size_ok = std::fs::metadata(&seg.path).map(|m| m.len() > 0).unwrap_or(false);
        let secs = info.as_ref().and_then(|i| i.duration).unwrap_or(seg.last.out_time_secs);
        let rec = SegRec { path: seg.path.clone(), info: if size_ok { info } else { None }, secs, dropped: seg.last.drop_frames, dup: seg.last.dup_frames };
        self.recs.push(rec.clone());
        Some(rec)
    }

    fn stderr_tail(&self) -> String {
        self.seg
            .as_ref()
            .and_then(|s| s.tail.lock().ok().map(|t| t.iter().rev().take(3).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ")))
            .unwrap_or_default()
    }

    fn status(&mut self) -> Status {
        let (fps, _) = self.cur();
        let (frames, speed, bytes, capture, canvas, pid) = match &self.seg {
            Some(s) => (s.last.frame, s.last.speed, s.last.total_size, s.capture, s.canvas, Some(s.child.id())),
            None => (0, None, 0, Size::default(), self.canvas, None),
        };
        #[cfg(target_os = "linux")]
        if let Some(pid) = pid {
            if let Some((ticks, rss)) = proc_ticks_and_rss(pid) {
                let now = Instant::now();
                if let Some((t0, w0)) = self.cpu_prev {
                    let dt = now.duration_since(w0).as_secs_f64();
                    if dt > 0.5 {
                        self.cpu_now = Some((ticks.saturating_sub(t0)) as f64 / clk_tck() / dt * 100.0);
                        self.cpu_prev = Some((ticks, now));
                    }
                } else {
                    self.cpu_prev = Some((ticks, now));
                }
                self.rss_now = Some(rss);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        Status {
            state: self.state,
            recorded_secs: self.total_recorded(),
            frames: frames + 0,
            dropped: self.recs.iter().map(|r| r.dropped).sum::<u64>() + self.seg.as_ref().map(|s| s.last.drop_frames).unwrap_or(0),
            duplicated: self.recs.iter().map(|r| r.dup).sum::<u64>() + self.seg.as_ref().map(|s| s.last.dup_frames).unwrap_or(0),
            speed,
            bytes: bytes + self.recs.iter().map(|r| std::fs::metadata(&r.path).map(|m| m.len()).unwrap_or(0)).sum::<u64>(),
            segment: self.next_idx,
            capture,
            canvas,
            fps,
            ffmpeg_cpu_percent: self.cpu_now,
            ffmpeg_rss_mb: self.rss_now,
            free_bytes: self.free_now,
        }
    }

    fn restart(&mut self, rect: Rect) -> Result<(), String> {
        self.stop_segment();
        self.begin_segment(rect)
    }

    fn run(mut self, rx: Receiver<Cmd>) -> Outcome {
        let mut ticks: u64 = 0;
        let end = loop {
            match rx.recv_timeout(TICK) {
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break End::User,
                Ok(Cmd::Pause) if self.state == State::Recording => {
                    self.stop_segment();
                    self.state = State::Paused;
                    self.notice(tr!("تم الإيقاف المؤقت", "paused"));
                }
                Ok(Cmd::Resume) if matches!(self.state, State::Paused | State::AutoPaused) => match self.req.settings.resize_mode {
                    _ => match self.backend.probe(&self.req.source) {
                        Availability::Visible(r) => {
                            if let Err(e) = self.begin_segment(r) {
                                break End::Error(e);
                            }
                            self.notice(tr!("استُؤنف التسجيل", "resumed"));
                        }
                        Availability::Hidden => self.notice(tr!("المصدر مخفي حاليًا؛ سيُستأنف التسجيل تلقائيًا عند ظهوره.", "the source is hidden right now; recording will resume by itself when it is visible.")),
                        Availability::Gone => break End::SourceGone,
                    },
                },
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            }
            ticks += 1;
            self.drain_progress();

            // 1. FFmpeg ended by itself?
            let exited = match self.seg.as_mut() {
                Some(s) => matches!(s.child.try_wait(), Ok(Some(_))),
                None => false,
            };
            if exited {
                let tail = self.stderr_tail();
                let started = self.seg.as_ref().map(|s| s.started.elapsed().as_secs_f64()).unwrap_or(0.0);
                let rec = self.stop_segment();
                if let (Some(limit), Some(_)) = (self.req.max_seconds, &rec) {
                    if self.total_recorded() >= limit - 0.4 {
                        break End::Limit;
                    }
                }
                match self.backend.probe(&self.req.source) {
                    Availability::Gone => break End::SourceGone,
                    Availability::Hidden => {
                        self.state = State::AutoPaused;
                        self.notice(tr!("المصدر أصبح مخفيًا: إيقاف مؤقت تلقائي.", "the source is hidden: paused automatically."));
                    }
                    Availability::Visible(r) => {
                        self.fast_fail = if started < 2.0 { self.fast_fail + 1 } else { 0 };
                        if self.fast_fail >= 3 {
                            break End::Error(tr!("توقف FFmpeg مرارًا عند البدء: {}", "FFmpeg keeps stopping right after start: {}", tail));
                        }
                        self.notice(tr!("توقف المقطع فجأة (غالبًا تغيّر حجم المصدر)؛ بدء مقطع جديد.", "a segment ended unexpectedly (usually the source changed size); starting a new one."));
                        if let Err(e) = self.begin_segment(r) {
                            break End::Error(e);
                        }
                    }
                }
                continue;
            }

            // 2. source check twice a second
            if ticks % 2 == 0 && !matches!(self.state, State::Finalizing) {
                let probe = self.backend.probe(&self.req.source);
                match (self.state, probe) {
                    (_, Availability::Gone) => break End::SourceGone,
                    (State::Recording, Availability::Hidden) => {
                        self.stop_segment();
                        self.state = State::AutoPaused;
                        self.notice(tr!("النافذة مخفية أو مصغّرة: إيقاف مؤقت تلقائي (سيُستأنف عند ظهورها).", "the window is hidden or minimised: paused automatically (resumes when it is visible)."));
                    }
                    (State::AutoPaused, Availability::Visible(r)) => {
                        if let Err(e) = self.begin_segment(r) {
                            break End::Error(e);
                        }
                        self.notice(tr!("عادت النافذة للظهور: استُؤنف التسجيل.", "the window is visible again: recording resumed."));
                    }
                    (State::Recording, Availability::Visible(r)) => {
                        let cap = self.seg.as_ref().map(|s| s.capture);
                        if cap.is_some() && cap != Some(r.size()) {
                            let n = match self.pending {
                                Some((sz, n)) if sz == r.size() => n + 1,
                                _ => 1,
                            };
                            self.pending = Some((r.size(), n));
                            if n >= 2 {
                                let old = cap.unwrap();
                                self.notice(tr!("تغيّر حجم المصدر من {} إلى {}؛ بدء مقطع جديد.", "source size changed from {} to {}; starting a new segment.", old, r.size()));
                                if let Err(e) = self.restart(r) {
                                    break End::Error(e);
                                }
                            }
                        } else {
                            self.pending = None;
                        }
                    }
                    _ => {}
                }
            }

            // 3. once a second: status, adaptive load management, disk space
            if ticks % 4 == 0 {
                if ticks % 40 == 0 {
                    self.free_now = storage::free_space_bytes(&self.tmp);
                    if self.free_now.map(|f| f < 100 * 1024 * 1024).unwrap_or(false) {
                        break End::DiskFull;
                    }
                }
                if self.req.settings.adaptive_resources && self.state == State::Recording {
                    let warm = self.seg.as_ref().map(|s| s.started.elapsed() > Duration::from_secs(4)).unwrap_or(false);
                    let speed = self.seg.as_ref().and_then(|s| s.last.speed);
                    if warm {
                        if let Some(mv) = self.adapt.on_second(speed) {
                            let (fps, preset) = self.cur();
                            let msg = match mv {
                                Move::Degrade => tr!("الحمل على الجهاز مرتفع: خُفِّض الأداء المطلوب إلى {} إطارًا/ث بإعداد {}. ستُستعاد الجودة تلقائيًا عند استقرار الأداء.", "the machine is overloaded: stepped down to {} fps with preset {}. Quality is restored automatically once it is stable.", fps, preset),
                                Move::Recover => tr!("استقر الأداء: استُعيدت الجودة إلى {} إطارًا/ث بإعداد {}.", "performance is stable: quality restored to {} fps with preset {}.", fps, preset),
                            };
                            self.notice(msg);
                            if let Availability::Visible(r) = self.backend.probe(&self.req.source) {
                                if let Err(e) = self.restart(r) {
                                    break End::Error(e);
                                }
                            }
                        }
                    }
                }
                let s = self.status();
                self.say(Event::Status(s));
            }

            // 4. limit reached while paused is impossible; while recording FFmpeg's -t ends the segment (handled above)
        };
        self.finalize(end)
    }

    fn expected_audio_streams(&self) -> usize {
        match self.audio.len() {
            0 => 0,
            2 if self.req.settings.separate_tracks => 2,
            _ => 1,
        }
    }

    fn audio_labels(&self) -> Vec<String> {
        let name = |a: &AudioInput| match a {
            AudioInput::TestTone(_) => tr!("نغمة اختبار (ليست صوت النظام)", "test tone (not system audio)"),
            _ => String::new(),
        };
        let (sys, mic) = (tr!("صوت النظام", "system audio"), tr!("الميكروفون", "microphone"));
        let st = &self.req.settings;
        let mut kinds = Vec::new();
        if st.system_audio {
            kinds.push(sys);
        }
        if st.mic {
            kinds.push(mic);
        }
        let tone = self.audio.iter().any(|(a, _)| matches!(a, AudioInput::TestTone(_)));
        match (self.expected_audio_streams(), tone) {
            (2, _) => kinds,
            (1, true) => vec![name(&self.audio[0].0)],
            (1, false) => vec![kinds.join(" + ")],
            _ => vec![],
        }
    }

    fn finalize(mut self, end: End) -> Outcome {
        self.state = State::Finalizing;
        let s = self.status();
        self.say(Event::Status(s));
        self.stop_segment();
        self.drain_progress();

        let mut reason: Option<String> = None;
        match &end {
            End::User | End::Limit => {}
            End::SourceGone => {
                let m = match self.req.source {
                    Source::Window { .. } => tr!("أُغلقت النافذة المسجَّلة؛ انتهى التسجيل عند هذه النقطة.", "the recorded window was closed; the recording ended there."),
                    _ => tr!("انقطع مصدر التسجيل (شاشة أو واجهة رسومية غير متاحة).", "the capture source became unavailable (display or graphical session lost)."),
                };
                if matches!(self.req.source, Source::Window { .. }) {
                    self.notice(m);
                } else {
                    reason = Some(m);
                }
            }
            End::DiskFull => reason = Some(tr!("المساحة الحرة على القرص شبه منتهية؛ أُوقف التسجيل لحماية الملف.", "the disk is almost full; recording was stopped to protect the file.")),
            End::Error(e) => reason = Some(e.clone()),
        }
        if let Some(r) = &reason {
            self.say(Event::Warning(r.clone()));
        }

        let dropped: u64 = self.recs.iter().map(|r| r.dropped).sum();
        let dup: u64 = self.recs.iter().map(|r| r.dup).sum();
        let valid: Vec<Segment> = self
            .recs
            .iter()
            .filter_map(|r| match &r.info {
                Some(i) if i.video.is_some() && i.duration.unwrap_or(0.0) > 0.05 => Some(Segment { path: r.path.clone(), info: i.clone() }),
                _ => None,
            })
            .collect();
        let base = Outcome { kind: OutcomeKind::Complete, path: None, segments_dir: None, duration: 0.0, bytes: 0, segments: valid.len(), dropped, duplicated: dup, audio: vec![], notes: self.notes.clone() };
        let fail = |this: &Sup, mut o: Outcome, why: String, keep: bool| -> Outcome {
            o.kind = OutcomeKind::Failed(why);
            if keep {
                o.segments_dir = Some(this.tmp.clone());
            }
            storage::append_history("failed", &this.final_path, 0.0, 0);
            o
        };
        if valid.is_empty() {
            let why = reason.unwrap_or_else(|| tr!("لم يُسجَّل أي إطار صالح.", "no valid frame was recorded."));
            let o = fail(&self, base, why, false);
            let _ = std::fs::remove_dir_all(&self.tmp);
            return o;
        }

        let st = self.req.settings.clone();
        let sum: f64 = valid.iter().map(|s| s.info.duration.unwrap_or(0.0)).sum();
        let part = self.tmp.join("join.part");
        let same = valid.iter().all(|s| s.info.video == valid[0].info.video);
        let paths: Vec<PathBuf> = valid.iter().map(|s| s.path.clone()).collect();
        let joined = if same {
            join::join_copy(&self.ff, &paths, st.container, &part)
        } else {
            let w = valid.iter().filter_map(|s| s.info.video).map(|v| v.w).max().unwrap_or(2);
            let h = valid.iter().filter_map(|s| s.info.video).map(|v| v.h).max().unwrap_or(2);
            self.notice(tr!("اختلفت أبعاد المقاطع؛ سيُعاد ترميزها مرة واحدة في ملف واحد بأبعاد {}x{} (قد يستغرق ذلك وقتًا).", "segments differ in size; they are re-encoded once into a single {}x{} file (this can take a while).", w & !1, h & !1));
            let fps = self.cur().0.max(self.ladder[0].0);
            join::join_reencode(&self.ff, &self.caps, &st, &valid, Size::new(w, h).even(), fps, &part)
        };
        let verified = joined.and_then(|_| join::verify(&self.ff, &part, sum, self.expected_audio_streams()));
        let info = match verified {
            Ok(i) => i,
            Err(e) => {
                let o = fail(&self, base, tr!("فشل دمج المقاطع: {}. المقاطع الأصلية محفوظة في {} ويمكن استرجاعها بالأمر recover.", "joining the segments failed: {}. The original segments are kept in {} and can be recovered with the recover command.", e, self.tmp.display()), true);
                let _ = std::fs::remove_file(&part);
                return o;
            }
        };

        // name the result honestly
        let mut target = self.final_path.clone();
        if reason.is_some() {
            let dir = target.parent().unwrap_or(Path::new(".")).to_path_buf();
            let stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("Miraqab").to_string();
            target = storage::unique_path(&dir, &format!("{}-partial", stem), st.container.ext());
        }
        let published = match join::publish(&part, &target) {
            Ok(p) => p,
            Err(e) => {
                let o = fail(&self, base, e, true);
                return o;
            }
        };

        // Mode B keeps the original segments for recovery; otherwise our temp dir is removed.
        let mut segments_dir = None;
        if st.resize_mode == ResizeMode::Adaptive && valid.len() > 1 {
            let keep = published.with_file_name(format!("{}_segments", published.file_stem().and_then(|s| s.to_str()).unwrap_or("Miraqab")));
            let _ = std::fs::remove_file(self.tmp.join(storage::MANIFEST_NAME));
            if std::fs::rename(&self.tmp, &keep).is_ok() {
                segments_dir = Some(keep);
            }
        } else {
            let _ = std::fs::remove_dir_all(&self.tmp);
        }

        let mut audio = Vec::new();
        let labels = self.audio_labels();
        for (i, label) in labels.iter().enumerate() {
            audio.push(AudioReport { label: label.clone(), level: audio::analyze_file(&self.ff, &published, i) });
        }
        let bytes = std::fs::metadata(&published).map(|m| m.len()).unwrap_or(0);
        let duration = info.duration.unwrap_or(sum);
        let mut o = base;
        o.path = Some(published.clone());
        o.segments_dir = segments_dir;
        o.duration = duration;
        o.bytes = bytes;
        o.audio = audio;
        o.notes = self.notes.clone();
        o.kind = match reason {
            Some(r) => OutcomeKind::Partial(r),
            None => OutcomeKind::Complete,
        };
        storage::append_history(if o.kind == OutcomeKind::Complete { "ok" } else { "partial" }, &published, duration, bytes);
        o
    }
}

pub struct Handle {
    tx: Sender<Cmd>,
    pub events: Receiver<Event>,
    th: Option<JoinHandle<Outcome>>,
}

impl Handle {
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
    pub fn is_finished(&self) -> bool {
        self.th.as_ref().map(|t| t.is_finished()).unwrap_or(true)
    }
    /// Wait for the recording to finish (send `Cmd::Stop` first to end it).
    pub fn wait(mut self) -> Outcome {
        let th = self.th.take().unwrap();
        th.join().unwrap_or_else(|_| Outcome { kind: OutcomeKind::Failed("internal error".into()), path: None, segments_dir: None, duration: 0.0, bytes: 0, segments: 0, dropped: 0, duplicated: 0, audio: vec![], notes: vec![] })
    }
}

/// Validate everything, then start recording. Every problem is reported *before* a single
/// frame is captured; the returned list is shown to the user as it is.
pub fn start(req: Request, mut backend: Box<dyn Backend>, ff: Ffmpeg) -> Result<(Handle, StartInfo), Vec<String>> {
    let st = req.settings.clone();
    st.validate()?;
    let mut warnings = Vec::new();
    match backend.check_supported(&req.source) {
        Err(e) => return Err(vec![e]),
        Ok(Some(w)) => warnings.push(w),
        Ok(None) => {}
    }
    let rect = match backend.probe(&req.source) {
        Availability::Visible(r) => r,
        Availability::Hidden => return Err(vec![tr!("المصدر مخفي أو مصغّر حاليًا؛ أظهِره ثم أعد المحاولة.", "the source is hidden or minimised; show it and try again.")]),
        Availability::Gone => return Err(vec![tr!("المصدر المحدد غير موجود.", "the selected source does not exist.")]),
    };
    if rect.w < 16 || rect.h < 16 {
        return Err(vec![tr!("حجم المصدر صغير جدًا ({}x{}).", "the source is too small ({}x{}).", rect.w, rect.h)]);
    }
    let caps = ff.caps();
    let dev = if cfg!(windows) { "gdigrab" } else { "x11grab" };
    if !caps.devices.contains(dev) {
        return Err(vec![tr!("إصدار FFmpeg الحالي لا يدعم الالتقاط ({}).", "this FFmpeg build cannot capture the screen ({}).", dev)]);
    }
    let audio = audio::resolve_inputs(&ff, &st, req.test_tone)?;
    storage::prepare_output_dir(&st.output_dir).map_err(|e| vec![e])?;
    let free = storage::free_space_bytes(&st.output_dir);
    if let Some(f) = free {
        if f < 200 * 1024 * 1024 {
            return Err(vec![tr!("المساحة الحرة ({}) أقل من الحد الأدنى للتسجيل (200 MB).", "free space ({}) is below the 200 MB minimum.", storage::human_bytes(f))]);
        }
        if f < 2 * 1024 * 1024 * 1024 {
            warnings.push(tr!("المساحة الحرة قليلة: {}.", "free space is low: {}.", storage::human_bytes(f)));
        }
    }
    let canvas = match st.resize_mode {
        ResizeMode::Fixed => st.resolution.canvas(rect.size()),
        ResizeMode::Adaptive => adaptive_canvas(&st, rect.size()),
    };
    let base = storage::sanitize_name(&st.name.clone().unwrap_or_else(|| storage::default_name(storage::local_now())));
    let final_path = storage::unique_path(&st.output_dir, &base, st.container.ext());
    let stem = final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("Miraqab").to_string();
    let tmp = st.output_dir.join(format!(".miraqab-{}-tmp", stem));
    std::fs::create_dir_all(&tmp).map_err(|e| vec![tr!("تعذّر إنشاء المجلد المؤقت: {}", "cannot create the temporary folder: {}", e)])?;
    let manifest = Manifest { final_path: final_path.clone(), container: st.container.ext().into(), mode: if st.resize_mode == ResizeMode::Fixed { "fixed".into() } else { "adaptive".into() }, started: storage::unix_now(), pid: std::process::id() };
    std::fs::write(tmp.join(storage::MANIFEST_NAME), manifest.to_text()).map_err(|e| vec![e.to_string()])?;

    let preset = st.x264_preset();
    let lad = ladder(st.fps, &preset, st.effective_codec() == Codec::H264);
    let adapt = Adapt::new(req.adaptive, lad.len() - 1);
    let (ev_tx, ev_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let info = StartInfo {
        final_path: final_path.clone(),
        canvas,
        fps: st.fps,
        kbps: st.effective_bitrate(canvas),
        codec: st.effective_codec().name(),
        audio_labels: Vec::new(),
        est_mb_per_min: st.estimate_mb_per_minute(canvas),
        free_bytes: free,
        warnings,
    };
    let mut sup = Sup {
        req,
        ff,
        caps,
        backend,
        audio,
        canvas,
        tmp: tmp.clone(),
        final_path,
        ev: ev_tx,
        seg: None,
        recs: Vec::new(),
        next_idx: 0,
        state: State::Recording,
        pending: None,
        fast_fail: 0,
        ladder: lad,
        adapt,
        notes: Vec::new(),
        cpu_prev: None,
        cpu_now: None,
        rss_now: None,
        free_now: free,
    };
    let mut info = info;
    info.audio_labels = sup.audio_labels();
    // All FFmpeg processes are spawned from the supervisor thread (see PDEATHSIG in `begin_segment`).
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let th = std::thread::Builder::new()
        .name("miraqab-recorder".into())
        .spawn(move || {
            match sup.begin_segment(rect) {
                Ok(()) => {
                    let _ = ready_tx.send(Ok(()));
                    sup.run(cmd_rx)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.clone()));
                    let _ = std::fs::remove_dir_all(&sup.tmp);
                    Outcome { kind: OutcomeKind::Failed(e), path: None, segments_dir: None, duration: 0.0, bytes: 0, segments: 0, dropped: 0, duplicated: 0, audio: vec![], notes: vec![] }
                }
            }
        })
        .map_err(|e| vec![e.to_string()])?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((Handle { tx: cmd_tx, events: ev_rx, th: Some(th) }, info)),
        Ok(Err(e)) => {
            let _ = th.join();
            Err(vec![e])
        }
        Err(_) => Err(vec![tr!("تعذّر بدء التسجيل", "recording could not be started")]),
    }
}

/// Join the segments of an interrupted recording (`miraqab recover`).
pub fn recover_dir(ff: &Ffmpeg, dir: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(dir.join(storage::MANIFEST_NAME)).map_err(|e| e.to_string())?;
    let m = Manifest::parse(&text).ok_or_else(|| tr!("ملف الجلسة تالف", "the session file is damaged"))?;
    let container = crate::settings::Container::parse(&m.container).unwrap_or(crate::settings::Container::Mkv);
    let mut segs = Vec::new();
    for p in storage::segment_files(dir) {
        if let Some(i) = ff.media_info(&p) {
            if i.video.is_some() && i.duration.unwrap_or(0.0) > 0.05 {
                segs.push((p, i));
            }
        }
    }
    if segs.is_empty() {
        return Err(tr!("لا توجد مقاطع صالحة في {}", "no valid segments in {}", dir.display()));
    }
    if segs.iter().any(|(_, i)| i.video != segs[0].1.video) {
        return Err(tr!("اختلفت أبعاد المقاطع؛ استخدم FFmpeg يدويًا لدمجها (المقاطع محفوظة كما هي).", "the segments differ in size; join them manually with FFmpeg (they are left untouched)."));
    }
    let sum: f64 = segs.iter().map(|(_, i)| i.duration.unwrap_or(0.0)).sum();
    let audio = segs.iter().map(|(_, i)| i.audio_streams).min().unwrap_or(0);
    let part = dir.join("recover.part");
    let paths: Vec<PathBuf> = segs.iter().map(|(p, _)| p.clone()).collect();
    join::join_copy(ff, &paths, container, &part)?;
    join::verify(ff, &part, sum, audio).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        e
    })?;
    let stem = m.final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("Miraqab").to_string();
    let parent = m.final_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let target = storage::unique_path(&parent, &format!("{}-recovered", stem), container.ext());
    let out = join::publish(&part, &target)?;
    // only now is the interrupted session's temp folder safe to remove
    let _ = std::fs::remove_dir_all(dir);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_steps() {
        let l = ladder(30, "veryfast", true);
        assert_eq!(l, vec![(30, "veryfast".into()), (30, "ultrafast".into()), (24, "ultrafast".into()), (20, "ultrafast".into())]);
        let u = ladder(10, "ultrafast", true);
        assert_eq!(u, vec![(10, "ultrafast".into()), (5, "ultrafast".into())]);
        let v = ladder(30, "x", false); // VP9/AV1: no preset step
        assert_eq!(v.len(), 3);
        assert_eq!(ladder(5, "ultrafast", true).len(), 1, "nothing lower than 5 fps");
    }

    fn cfg() -> AdaptiveConfig {
        AdaptiveConfig { sustain_secs: 3, recover_secs: 5, speed_threshold: 0.92 }
    }

    #[test]
    fn adaptive_needs_sustained_overload() {
        let mut a = Adapt::new(cfg(), 2);
        assert_eq!(a.on_second(Some(0.5)), None);
        assert_eq!(a.on_second(Some(1.0)), None); // a good second resets the count
        assert_eq!(a.on_second(Some(0.5)), None);
        assert_eq!(a.on_second(Some(0.5)), None);
        assert_eq!(a.on_second(Some(0.5)), Some(Move::Degrade));
        assert_eq!(a.level, 1);
        assert_eq!(a.on_second(None), None, "unknown speed is never treated as overload");
    }

    #[test]
    fn adaptive_recovers_and_backs_off() {
        let mut a = Adapt::new(cfg(), 2);
        for _ in 0..3 {
            a.on_second(Some(0.4));
        }
        assert_eq!(a.level, 1);
        let mut got = None;
        for _ in 0..5 {
            got = a.on_second(Some(1.0));
        }
        assert_eq!(got, Some(Move::Recover));
        assert_eq!(a.level, 0);
        // overload straight after the recovery attempt: degrade again and wait twice as long next time
        for _ in 0..3 {
            a.on_second(Some(0.4));
        }
        assert_eq!(a.level, 1);
        let mut recovered_after = 0;
        for i in 1..=20 {
            if a.on_second(Some(1.0)) == Some(Move::Recover) {
                recovered_after = i;
                break;
            }
        }
        assert_eq!(recovered_after, 10, "backoff doubled the wait");
    }

    #[test]
    fn adaptive_never_exceeds_max_level() {
        let mut a = Adapt::new(cfg(), 1);
        for _ in 0..30 {
            a.on_second(Some(0.1));
        }
        assert_eq!(a.level, 1);
    }

    #[test]
    fn adaptive_canvas_never_upscales() {
        let mut st = Settings::default();
        st.resolution = Resolution::Height(720);
        assert_eq!(adaptive_canvas(&st, Size::new(640, 480)), Size::new(640, 480));
        assert_eq!(adaptive_canvas(&st, Size::new(1920, 1080)), Size::new(1280, 720));
        assert_eq!(adaptive_canvas(&st, Size::new(1920, 1200)), Size::new(1152, 720));
        st.resolution = Resolution::Original;
        assert_eq!(adaptive_canvas(&st, Size::new(641, 481)), Size::new(640, 480));
    }

    #[test]
    fn lines_are_split_and_bounded() {
        let mut got = Vec::new();
        read_lines_lossy(&b"a=1\nb=2\r\n\xff\xfebad\nlast"[..], |l| got.push(l));
        assert_eq!(got[0], "a=1");
        assert_eq!(got[1], "b=2");
        assert!(got[2].ends_with("bad"));
        assert_eq!(got.len(), 3, "an unterminated trailing line is dropped, not mis-parsed");
    }
}
