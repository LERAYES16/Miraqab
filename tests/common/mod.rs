//! Shared helpers for the end-to-end tests: a private Xvfb server per test and
//! the two small C helper programs (animated/resizable window, xwininfo stand-in).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
        || Command::new("sh").args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)]).status().map(|s| s.success()).unwrap_or(false)
}

/// Returns false (and prints why) when the tools needed for X11 end-to-end tests are missing.
pub fn x11_tools_available() -> bool {
    let ok = have("Xvfb") && have("gcc") && have("ffmpeg");
    if !ok {
        eprintln!("SKIPPED: needs Xvfb, gcc and ffmpeg");
    }
    ok
}

static NEXT: AtomicU32 = AtomicU32::new(0);

pub struct XServer {
    child: Child,
    pub display: String,
}

impl XServer {
    pub fn start(size: &str) -> XServer {
        let base = 120 + (std::process::id() % 60) * 4;
        loop {
            let n = base + NEXT.fetch_add(1, Ordering::SeqCst);
            if Path::new(&format!("/tmp/.X11-unix/X{}", n)).exists() || Path::new(&format!("/tmp/.X{}-lock", n)).exists() {
                continue;
            }
            let child = Command::new("Xvfb")
                .args([&format!(":{}", n), "-screen", "0", &format!("{}x24", size), "-nolisten", "tcp"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start Xvfb");
            let t = Instant::now();
            while !Path::new(&format!("/tmp/.X11-unix/X{}", n)).exists() {
                assert!(t.elapsed() < Duration::from_secs(10), "Xvfb did not start");
                std::thread::sleep(Duration::from_millis(50));
            }
            std::thread::sleep(Duration::from_millis(150));
            return XServer { child, display: format!(":{}", n) };
        }
    }
}

impl Drop for XServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn build_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("miraqab-test-helpers-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn compile(src: &str, out: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/helpers").join(src);
    let dest = build_dir().join(out);
    let st = Command::new("gcc").arg("-O1").arg("-o").arg(&dest).arg(&root).arg("-lX11").status().expect("run gcc");
    assert!(st.success(), "compiling {} failed", src);
    dest
}

pub fn xwin_binary() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| compile("xwin.c", "xwin"))
}

/// A real X11 window (child process) that can resize itself on a schedule.
pub struct TestWindow {
    child: Child,
    pub id: u32,
}

impl TestWindow {
    /// `steps` like `"2:640x480"` resize the window 2 s after it appears.
    pub fn open(display: &str, size: &str, steps: &[&str], lifetime_secs: u32) -> TestWindow {
        use std::io::{BufRead, BufReader};
        let mut cmd = Command::new(xwin_binary());
        cmd.arg(size);
        for s in steps {
            cmd.arg(s);
        }
        cmd.arg(lifetime_secs.to_string()).env("DISPLAY", display).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().expect("start xwin");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let id = u32::from_str_radix(line.trim().trim_start_matches("0x"), 16).expect("window id");
        std::thread::sleep(Duration::from_millis(300));
        TestWindow { child, id }
    }
    pub fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// ffprobe wrapper returning "key=value" pairs for the first video stream and the format.
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub video_codec: String,
    pub audio_streams: usize,
    pub video_frames: u64,
}

pub fn ffprobe(path: &Path) -> Probe {
    let run = |args: &[&str]| -> String {
        let o = Command::new("ffprobe").args(["-v", "error"]).args(args).arg(path).output().expect("run ffprobe");
        assert!(o.status.success(), "ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let v = run(&["-select_streams", "v:0", "-count_frames", "-show_entries", "stream=codec_name,width,height,nb_read_frames", "-of", "default=nw=1"]);
    let get = |k: &str| v.lines().find_map(|l| l.strip_prefix(&format!("{}=", k))).unwrap_or("").to_string();
    let dur = run(&["-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]);
    let a = run(&["-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"]);
    Probe {
        width: get("width").parse().unwrap_or(0),
        height: get("height").parse().unwrap_or(0),
        duration: dur.trim().parse().unwrap_or(0.0),
        video_codec: get("codec_name"),
        audio_streams: a.lines().filter(|l| !l.trim().is_empty()).count(),
        video_frames: get("nb_read_frames").parse().unwrap_or(0),
    }
}

pub fn temp_out(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("miraqab-test-{}-{}-{}", tag, std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
