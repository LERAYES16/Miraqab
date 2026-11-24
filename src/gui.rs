//! The Arabic control panel: a tiny HTTP server inside the Miraqab process.
//!
//! Why this exists: a proper right-to-left Arabic interface needs text shaping and bidi
//! layout, which a Windows console does not provide and which no GUI library is available
//! for in a dependency-free build. The user's own browser already has both, so Miraqab serves
//! one page to it. Nothing is bundled, nothing leaves the machine:
//!
//! * the server listens on 127.0.0.1 only;
//! * every request needs a random token (printed in the URL) in the query or a header, and
//!   a `Host` header naming the loopback address, which blocks other websites and DNS rebinding;
//! * the page is static; window titles etc. are inserted with `textContent` (never as HTML).
//!
//! Recording itself runs in this process / FFmpeg, not in the browser: closing the tab does
//! not stop a recording.

use crate::app;
use crate::audio;
use crate::capture;
use crate::ffmpeg::Ffmpeg;
use crate::recorder::{self, AdaptiveConfig, Cmd, Event, Handle, OutcomeKind, Request, StartInfo, State, Status};
use crate::sig;
use crate::storage;
use crate::tr;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("../assets/ui.html");

#[derive(Default)]
struct GuiState {
    handle: Option<Handle>,
    recording: bool,
    countdown: Option<u32>,
    status: Option<Status>,
    info: Option<StartInfo>,
    log: Vec<String>,
    outcome: Option<(&'static str, Vec<String>)>,
    errors: Vec<String>,
}

struct App {
    token: String,
    port: u16,
    st: Mutex<GuiState>,
    quit: AtomicBool,
}

pub fn make_token() -> String {
    // RandomState is seeded from the operating system's entropy source on every platform.
    let mut out = String::new();
    for i in 0..4u32 {
        let mut h = RandomState::new().build_hasher();
        h.write_u32(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '<' || c == '>' || c == '&' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_arr(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(","))
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect()
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: String,
}

fn read_request(s: &mut TcpStream) -> Option<Req> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 32 * 1024 {
            return None;
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers: HashMap<String, String> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))).collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 64 * 1024 {
        return None;
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    Some(Req { method, path: path.to_string(), query: parse_form(q), headers, body: String::from_utf8_lossy(&body).into_owned() })
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, body: &str) {
    let text = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'\r\nConnection: close\r\n\r\n",
        code,
        text,
        ctype,
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body.as_bytes());
}

fn json_ok(s: &mut TcpStream, body: String) {
    respond(s, 200, "application/json; charset=utf-8", &body);
}

fn open_browser(url: &str) {
    let r = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/c", "start", "", url]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
    };
    if r.is_err() {
        println!("{}", tr!("تعذّر فتح المتصفح تلقائيًا؛ افتح الرابط أعلاه يدويًا.", "could not open the browser automatically; open the link above yourself."));
    }
}

pub fn run(port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| tr!("تعذّر فتح المنفذ المحلي: {}", "cannot open the local port: {}", e))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let app = Arc::new(App { token: make_token(), port, st: Mutex::new(GuiState::default()), quit: AtomicBool::new(false) });
    let url = format!("http://127.0.0.1:{}/?t={}", port, app.token);
    println!("{}", tr!("لوحة تحكم مِرقاب تعمل على جهازك فقط:", "Miraqab control panel (this computer only):"));
    println!("  {}", url);
    println!("{}", tr!("اضغط Ctrl+C لإغلاقها (أو زر «إغلاق مِرقاب» في الصفحة).", "Press Ctrl+C to close it (or the quit button in the page)."));
    if std::env::var_os("MIRAQAB_NO_BROWSER").is_none() {
        open_browser(&url);
    }
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    while !sig::requested() && !app.quit.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                let app = app.clone();
                std::thread::spawn(move || handle(&app, &mut s));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(150)),
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    // closing while recording: finish the file safely first
    let recording = app.st.lock().map(|g| g.recording || g.countdown.is_some()).unwrap_or(false);
    if recording {
        println!("{}", tr!("جارٍ إنهاء التسجيل الحالي بأمان…", "finishing the current recording safely…"));
        if let Ok(mut g) = app.st.lock() {
            g.countdown = None;
            if let Some(h) = &g.handle {
                h.send(Cmd::Stop);
            }
        }
        let t = Instant::now();
        while app.st.lock().map(|g| g.recording).unwrap_or(false) && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

fn handle(app: &Arc<App>, s: &mut TcpStream) {
    let Some(r) = read_request(s) else { return respond(s, 400, "text/plain", "bad request") };
    let host_ok = matches!(r.headers.get("host").map(|h| h.as_str()), Some(h) if h == format!("127.0.0.1:{}", app.port) || h == format!("localhost:{}", app.port));
    let tok = r.headers.get("x-miraqab-token").or_else(|| r.query.get("t"));
    if !host_ok || tok.map(|t| t != &app.token).unwrap_or(true) {
        return respond(s, 403, "text/plain; charset=utf-8", "forbidden");
    }
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/") => respond(s, 200, "text/html; charset=utf-8", PAGE),
        ("GET", "/api/state") => json_ok(s, state_json(app)),
        ("GET", "/api/sources") => json_ok(s, sources_json()),
        ("GET", "/api/devices") => json_ok(s, devices_json()),
        ("GET", "/api/doctor") => {
            let items: Vec<String> = app::doctor_report().into_iter().map(|(ok, l)| format!("{{\"ok\":{},\"text\":{}}}", ok, json_str(&l))).collect();
            json_ok(s, format!("[{}]", items.join(",")))
        }
        ("POST", "/api/start") => match start(app, &parse_form(&r.body)) {
            Ok(()) => json_ok(s, "{\"ok\":true}".into()),
            Err(errs) => json_ok(s, format!("{{\"ok\":false,\"errors\":{}}}", json_arr(&errs))),
        },
        ("POST", "/api/audio-test") => json_ok(s, audio_test(&parse_form(&r.body))),
        ("POST", "/api/pause") | ("POST", "/api/resume") | ("POST", "/api/stop") => {
            if let Ok(mut g) = app.st.lock() {
                match r.path.as_str() {
                    "/api/stop" => {
                        g.countdown = None;
                        if let Some(h) = &g.handle {
                            h.send(Cmd::Stop);
                        }
                    }
                    p => {
                        if let Some(h) = &g.handle {
                            h.send(if p == "/api/pause" { Cmd::Pause } else { Cmd::Resume });
                        }
                    }
                }
            }
            json_ok(s, "{\"ok\":true}".into());
        }
        ("POST", "/api/quit") => {
            app.quit.store(true, Ordering::SeqCst);
            json_ok(s, "{\"ok\":true}".into());
        }
        _ => respond(s, 404, "text/plain", "not found"),
    }
}

fn state_json(app: &App) -> String {
    let g = app.st.lock().unwrap_or_else(|e| e.into_inner());
    let status = match &g.status {
        Some(s) if g.recording => {
            let st = match s.state {
                State::Recording => "recording",
                State::Paused => "paused",
                State::AutoPaused => "autopaused",
                State::Finalizing => "finalizing",
            };
            format!(
                "{{\"state\":\"{}\",\"time\":{},\"fps\":{},\"size\":{},\"dropped\":{},\"dup\":{},\"cpu\":{},\"free\":{},\"line\":{}}}",
                st,
                json_str(&storage::human_duration(s.recorded_secs)),
                s.fps,
                json_str(&storage::human_bytes(s.bytes)),
                s.dropped,
                s.duplicated,
                s.ffmpeg_cpu_percent.map(|c| format!("{:.0}", c)).unwrap_or_else(|| "null".into()),
                s.free_bytes.map(|f| json_str(&storage::human_bytes(f))).unwrap_or_else(|| "null".into()),
                json_str(&app::status_line(s))
            )
        }
        _ => "null".into(),
    };
    let info = match &g.info {
        Some(i) => format!(
            "{{\"file\":{},\"canvas\":{},\"kbps\":{},\"codec\":{},\"audio\":{},\"estimate\":{:.0},\"warnings\":{}}}",
            json_str(&i.final_path.display().to_string()),
            json_str(&i.canvas.to_string()),
            i.kbps,
            json_str(i.codec),
            json_arr(&i.audio_labels),
            i.est_mb_per_min,
            json_arr(&i.warnings)
        ),
        None => "null".into(),
    };
    let outcome = match &g.outcome {
        Some((kind, lines)) => format!("{{\"kind\":\"{}\",\"lines\":{}}}", kind, json_arr(lines)),
        None => "null".into(),
    };
    format!(
        "{{\"recording\":{},\"countdown\":{},\"status\":{},\"info\":{},\"log\":{},\"outcome\":{},\"errors\":{}}}",
        g.recording,
        g.countdown.map(|c| c.to_string()).unwrap_or_else(|| "null".into()),
        status,
        info,
        json_arr(&g.log),
        outcome,
        json_arr(&g.errors)
    )
}

fn sources_json() -> String {
    let mut b = match capture::default_backend() {
        Ok(b) => b,
        Err(e) => return format!("{{\"error\":{},\"windows\":[],\"monitors\":[]}}", json_str(&e)),
    };
    let mut err: Option<String> = None;
    let wins: Vec<String> = match b.windows() {
        Ok(w) => w.into_iter().map(|w| format!("{{\"id\":{},\"title\":{},\"size\":{},\"class\":{},\"hidden\":{}}}", json_str(&format!("0x{:x}", w.id)), json_str(&w.title), json_str(&w.size.to_string()), json_str(&w.class), w.hidden)).collect(),
        Err(e) => {
            err = Some(e);
            vec![]
        }
    };
    let mons: Vec<String> = match b.monitors() {
        Ok(m) => m.into_iter().map(|m| format!("{{\"n\":{},\"name\":{},\"w\":{},\"h\":{},\"x\":{},\"y\":{},\"primary\":{}}}", m.index + 1, json_str(&m.name), m.rect.w, m.rect.h, m.rect.x, m.rect.y, m.primary)).collect(),
        Err(e) => {
            err = err.or(Some(e));
            vec![]
        }
    };
    let warn = b.check_supported(&crate::capture::Source::Desktop).err();
    format!("{{\"error\":{},\"desktopBlocked\":{},\"windows\":[{}],\"monitors\":[{}]}}", err.map(|e| json_str(&e)).unwrap_or_else(|| "null".into()), warn.map(|w| json_str(&w)).unwrap_or_else(|| "null".into()), wins.join(","), mons.join(","))
}

fn devices_json() -> String {
    let ff = match Ffmpeg::locate() {
        Ok(f) => f,
        Err(e) => return format!("{{\"error\":{},\"devices\":[]}}", json_str(&e)),
    };
    match audio::list_devices(&ff) {
        Ok(d) => {
            let items: Vec<String> = d.iter().map(|d| format!("{{\"id\":{},\"label\":{},\"monitor\":{},\"default\":{}}}", json_str(&d.id), json_str(&d.label), d.monitor, d.default)).collect();
            format!("{{\"error\":null,\"devices\":[{}]}}", items.join(","))
        }
        Err(e) => format!("{{\"error\":{},\"devices\":[]}}", json_str(&e)),
    }
}

fn pairs_from_form(f: &HashMap<String, String>) -> Vec<(String, String)> {
    const KEYS: &[&str] = &["profile", "fps", "res", "bitrate", "format", "codec", "mode", "system-audio", "mic", "separate-tracks", "system-volume", "mic-volume", "system-device", "mic-device", "out", "name", "countdown", "adaptive-resources", "mouse"];
    KEYS.iter().filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.trim().to_string()))).filter(|(_, v)| !v.is_empty()).collect()
}

fn audio_test(f: &HashMap<String, String>) -> String {
    let which_mic = f.get("which").map(|w| w == "mic").unwrap_or(false);
    let ff = match Ffmpeg::locate() {
        Ok(f) => f,
        Err(e) => return format!("{{\"ok\":false,\"text\":{}}}", json_str(&e)),
    };
    let mut pairs = pairs_from_form(f);
    pairs.retain(|(k, _)| k != "system-audio" && k != "mic");
    pairs.push((if which_mic { "mic" } else { "system-audio" }.into(), "on".into()));
    let st = match app::effective_settings(&pairs) {
        Ok(s) => s,
        Err(e) => return format!("{{\"ok\":false,\"text\":{}}}", json_str(&e.join(" | "))),
    };
    let inputs = match audio::resolve_inputs(&ff, &st, None) {
        Ok(i) => i,
        Err(e) => return format!("{{\"ok\":false,\"text\":{}}}", json_str(&e.join(" | "))),
    };
    let Some((inp, _)) = inputs.first() else { return "{\"ok\":false,\"text\":\"\"}".into() };
    match audio::measure_input(&ff, inp, 2.0) {
        Ok(Some(l)) if !l.is_silent() => format!("{{\"ok\":true,\"level\":{:.1},\"text\":{}}}", l.max_db, json_str(&tr!("يُلتقط فعلًا — الذروة {:.1} dB", "really captured — peak {:.1} dB", l.max_db))),
        Ok(_) => format!("{{\"ok\":false,\"text\":{}}}", json_str(&tr!("الجهاز يعمل لكن لا يوجد صوت (صامت).", "the device opens but there is no sound (silent)."))),
        Err(e) => format!("{{\"ok\":false,\"text\":{}}}", json_str(&e)),
    }
}

fn start(app: &Arc<App>, form: &HashMap<String, String>) -> Result<(), Vec<String>> {
    {
        let g = app.st.lock().map_err(|_| vec!["state".to_string()])?;
        if g.recording || g.countdown.is_some() {
            return Err(vec![tr!("هناك تسجيل جارٍ بالفعل.", "a recording is already in progress.")]);
        }
    }
    let ff = Ffmpeg::locate().map_err(|e| vec![e])?;
    let st = app::effective_settings(&pairs_from_form(form))?;
    let mut backend = capture::default_backend().map_err(|e| vec![e])?;
    let src = capture::parse_source(form.get("source").map(|s| s.as_str()).unwrap_or("desktop"), backend.as_mut()).map_err(|e| vec![e])?;
    let countdown = st.countdown;
    {
        let mut g = app.st.lock().unwrap();
        g.outcome = None;
        g.errors.clear();
        g.log.clear();
        g.info = None;
        g.status = None;
        g.countdown = Some(countdown);
    }
    let app2 = app.clone();
    std::thread::spawn(move || {
        for left in (1..=countdown).rev() {
            if let Ok(mut g) = app2.st.lock() {
                if g.countdown.is_none() {
                    return; // cancelled
                }
                g.countdown = Some(left);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if app2.st.lock().map(|g| g.countdown.is_none()).unwrap_or(true) {
            return;
        }
        let req = Request { settings: st, source: src, max_seconds: None, test_tone: None, adaptive: AdaptiveConfig::default() };
        match recorder::start(req, backend, ff) {
            Err(errs) => {
                if let Ok(mut g) = app2.st.lock() {
                    g.countdown = None;
                    g.errors = errs;
                }
            }
            Ok((h, info)) => {
                if let Ok(mut g) = app2.st.lock() {
                    g.countdown = None;
                    g.recording = true;
                    g.info = Some(info);
                    g.handle = Some(h);
                }
                pump(&app2);
            }
        }
    });
    Ok(())
}

/// Copy recorder events into the shared state until the recording ends, then store the outcome.
fn pump(app: &Arc<App>) {
    loop {
        let finished = {
            let mut g = app.st.lock().unwrap_or_else(|e| e.into_inner());
            let mut evs = Vec::new();
            if let Some(h) = &g.handle {
                while let Ok(e) = h.events.try_recv() {
                    evs.push(e);
                }
            }
            for e in evs {
                match e {
                    Event::Status(s) => g.status = Some(s),
                    Event::Notice(m) => {
                        g.log.push(m);
                    }
                    Event::Warning(m) => g.log.push(format!("⚠ {}", m)),
                    Event::Started(_) => {}
                }
                if g.log.len() > 60 {
                    g.log.remove(0);
                }
            }
            g.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)
        };
        if finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let h = app.st.lock().ok().and_then(|mut g| g.handle.take());
    let outcome = h.map(|h| h.wait());
    if let Ok(mut g) = app.st.lock() {
        if let Some(o) = outcome {
            let kind = match o.kind {
                OutcomeKind::Complete => "complete",
                OutcomeKind::Partial(_) => "partial",
                OutcomeKind::Failed(_) => "failed",
            };
            g.outcome = Some((kind, app::outcome_lines(&o)));
        }
        g.recording = false;
        g.status = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_blocks_html_and_control_chars() {
        assert_eq!(json_str("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        let evil = json_str("</script><img src=x onerror=alert(1)>");
        assert!(!evil.contains('<') && !evil.contains('>'));
        assert_eq!(json_str("نافذة"), "\"نافذة\"");
    }

    #[test]
    fn form_decoding() {
        let f = parse_form("source=window%3A0x1a&name=%D8%AA%D8%B3%D8%AC%D9%8A%D9%84+1&empty=&bad=%zz&end=%4");
        assert_eq!(f["source"], "window:0x1a");
        assert_eq!(f["name"], "تسجيل 1");
        assert_eq!(f["empty"], "");
        assert_eq!(f["bad"], "%zz");
        assert_eq!(f["end"], "%4");
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let (a, b) = (make_token(), make_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn only_known_keys_reach_the_settings() {
        let mut f = HashMap::new();
        f.insert("fps".to_string(), "20".to_string());
        f.insert("evil".to_string(), "1".to_string());
        f.insert("bitrate".to_string(), "".to_string());
        assert_eq!(pairs_from_form(&f), vec![("fps".to_string(), "20".to_string())]);
    }
}
