//! Capture sources and the platform backends that find them.
//!
//! Pixels are always captured by FFmpeg (`x11grab` on Linux, `gdigrab` on
//! Windows). A backend's job is to (1) enumerate windows and monitors,
//! (2) say what a source looks like *right now*, and (3) produce the FFmpeg
//! input arguments for it. Keeping this behind one trait keeps the recording
//! engine free of any desktop-environment assumptions.

use crate::geometry::{Rect, Size};
use crate::tr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The whole desktop (all monitors).
    Desktop,
    /// One monitor, by index in [`Backend::monitors`] (0-based).
    Monitor(usize),
    /// One application window. The id is the native handle (X11 window id / Windows HWND).
    Window { id: u64, title: String },
    /// A fixed rectangle of the desktop.
    Region(Rect),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Desktop => tr!("سطح المكتب كاملًا", "entire desktop"),
            Source::Monitor(i) => tr!("الشاشة {}", "monitor {}", i + 1),
            Source::Window { title, .. } => tr!("النافذة «{}»", "window \"{}\"", title),
            Source::Region(r) => tr!("المنطقة {}x{} عند ({},{})", "region {}x{} at ({},{})", r.w, r.h, r.x, r.y),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowEntry {
    pub id: u64,
    pub title: String,
    pub class: String,
    pub size: Size,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub struct MonitorEntry {
    pub index: usize,
    pub name: String,
    pub rect: Rect,
    pub primary: bool,
}

/// What a source looks like right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The window/monitor no longer exists.
    Gone,
    /// It exists but cannot be captured at the moment (minimised, on another workspace).
    Hidden,
    /// Capturable now, covering this rectangle (absolute desktop coordinates).
    Visible(Rect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    X11,
    Wayland,
    Windows,
    Unknown,
}

pub fn detect_session() -> SessionKind {
    if cfg!(windows) {
        return SessionKind::Windows;
    }
    let ty = std::env::var("XDG_SESSION_TYPE").unwrap_or_default().to_lowercase();
    let wl = std::env::var_os("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
    let x = std::env::var_os("DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
    match ty.as_str() {
        "wayland" => SessionKind::Wayland,
        "x11" if x => SessionKind::X11,
        _ if wl => SessionKind::Wayland,
        _ if x => SessionKind::X11,
        _ => SessionKind::Unknown,
    }
}

pub trait Backend: Send {
    fn name(&self) -> String;
    /// `Err` = this source cannot be recorded here (and why); `Ok(Some(w))` = allowed with a warning.
    fn check_supported(&self, src: &Source) -> Result<Option<String>, String>;
    fn windows(&mut self) -> Result<Vec<WindowEntry>, String>;
    fn monitors(&mut self) -> Result<Vec<MonitorEntry>, String>;
    fn probe(&mut self, src: &Source) -> Availability;
    /// FFmpeg input arguments (including `-f … -i …`) capturing exactly `rect` now.
    fn video_input(&self, src: &Source, rect: Rect, fps: u32, draw_mouse: bool) -> Vec<String>;
}

pub fn default_backend() -> Result<Box<dyn Backend>, String> {
    #[cfg(target_os = "linux")]
    {
        match detect_session() {
            SessionKind::X11 | SessionKind::Wayland => {
                let d = std::env::var("DISPLAY").map_err(|_| wayland_only_message())?;
                let mut b = X11Backend::new(&d);
                b.wayland = detect_session() == SessionKind::Wayland;
                Ok(Box::new(b))
            }
            _ => Err(tr!("لا توجد جلسة رسومية: المتغير DISPLAY غير معرّف", "no graphical session: DISPLAY is not set")),
        }
    }
    #[cfg(windows)]
    {
        Ok(Box::new(crate::winapi::WindowsBackend::new()))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(tr!("هذا النظام غير مدعوم", "this operating system is not supported"))
    }
}

pub fn wayland_only_message() -> String {
    tr!(
        "جلسة Wayland بلا X11: التقاط الشاشة عبر XDG Desktop Portal وPipeWire غير منجز بعد في مِرقاب. سجّل الدخول بجلسة X11 (جلسة Cinnamon الافتراضية في Linux Mint).",
        "Wayland session without X11: capture through XDG Desktop Portal + PipeWire is not implemented yet. Log in to an X11 session (the default Cinnamon session on Linux Mint)."
    )
}

/// Parse `desktop`, `monitor:N` (1-based), `window:ID`, `title:TEXT` or `region:X,Y,WxH`.
/// A title that matches several windows is an error listing them: Miraqab never guesses
/// which window to record.
pub fn parse_source(spec: &str, backend: &mut dyn Backend) -> Result<Source, String> {
    let spec = spec.trim();
    let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
    match kind.to_lowercase().as_str() {
        "desktop" | "screen" | "سطح-المكتب" => Ok(Source::Desktop),
        "monitor" | "m" | "شاشة" => {
            let n: usize = rest.parse().map_err(|_| tr!("رقم الشاشة غير صالح: {}", "invalid monitor number: {}", rest))?;
            let mons = backend.monitors()?;
            if n == 0 || n > mons.len() {
                return Err(tr!("الشاشة {} غير موجودة (المتاح: 1 إلى {})", "monitor {} does not exist (available: 1 to {})", n, mons.len()));
            }
            Ok(Source::Monitor(n - 1))
        }
        "window" | "w" | "نافذة" => {
            let id = if let Some(h) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).ok()
            } else {
                rest.parse().ok()
            }
            .ok_or_else(|| tr!("معرّف النافذة غير صالح: {}", "invalid window id: {}", rest))?;
            let wins = backend.windows()?;
            match wins.iter().find(|w| w.id == id) {
                Some(w) => Ok(Source::Window { id, title: w.title.clone() }),
                None => Err(tr!("النافذة {} غير موجودة. استخدم الأمر windows لعرض النوافذ.", "window {} does not exist. Use the windows command to list windows.", rest)),
            }
        }
        "title" | "t" | "عنوان" => {
            let needle = rest.to_lowercase();
            if needle.is_empty() {
                return Err(tr!("اكتب جزءًا من عنوان النافذة بعد title:", "give part of the window title after title:"));
            }
            let wins = backend.windows()?;
            let hits: Vec<&WindowEntry> = wins.iter().filter(|w| w.title.to_lowercase().contains(&needle)).collect();
            match hits.len() {
                0 => Err(tr!("لا توجد نافذة عنوانها يحتوي «{}»", "no window title contains \"{}\"", rest)),
                1 => Ok(Source::Window { id: hits[0].id, title: hits[0].title.clone() }),
                _ => {
                    let list: Vec<String> = hits.iter().map(|w| format!("  0x{:x}  {}", w.id, w.title)).collect();
                    Err(tr!("«{}» تطابق أكثر من نافذة، اختر واحدة بمعرّفها (window:ID):\n{}", "\"{}\" matches several windows; pick one by id (window:ID):\n{}", rest, list.join("\n")))
                }
            }
        }
        "region" | "r" | "منطقة" => {
            let bad = || tr!("صيغة المنطقة: region:X,Y,WxH مثل region:0,0,800x600", "region syntax: region:X,Y,WxH, e.g. region:0,0,800x600");
            let (xy, wh) = rest.rsplit_once(',').ok_or_else(bad)?;
            let (x, y) = xy.split_once(',').ok_or_else(bad)?;
            let (w, h) = wh.to_lowercase().split_once('x').map(|(a, b)| (a.to_string(), b.to_string())).ok_or_else(bad)?;
            let p = |s: &str| s.trim().parse::<i64>().map_err(|_| bad());
            let (x, y, w, h) = (p(x)?, p(y)?, p(&w)?, p(&h)?);
            if x < 0 || y < 0 || w < 16 || h < 16 {
                return Err(tr!("إحداثيات أو أبعاد المنطقة غير صالحة (الحد الأدنى 16×16)", "invalid region coordinates or size (minimum 16x16)"));
            }
            Ok(Source::Region(Rect { x: x as i32, y: y as i32, w: w as u32, h: h as u32 }))
        }
        _ => Err(tr!("مصدر غير معروف: {} (المتاح: desktop monitor:N window:ID title:TEXT region:X,Y,WxH)", "unknown source: {} (available: desktop monitor:N window:ID title:TEXT region:X,Y,WxH)", spec)),
    }
}

// ---- X11 backend ------------------------------------------------------------

#[cfg(target_os = "linux")]
pub use self::x11_backend::X11Backend;

#[cfg(target_os = "linux")]
mod x11_backend {
    use super::*;
    use crate::x11::{WinState, XErr, X11};

    pub struct X11Backend {
        display: String,
        conn: Option<X11>,
        /// True when the session is Wayland and X11 is only XWayland.
        pub wayland: bool,
    }

    impl X11Backend {
        pub fn new(display: &str) -> X11Backend {
            X11Backend { display: display.to_string(), conn: None, wayland: false }
        }

        /// Run `f` on the connection, reconnecting once if the socket failed.
        fn with<T>(&mut self, f: impl Fn(&mut X11) -> Result<T, XErr>) -> Result<T, String> {
            for attempt in 0..2 {
                if self.conn.is_none() {
                    self.conn = Some(X11::connect_to(&self.display).map_err(|e| tr!("تعذّر الاتصال بخادم X11 ({}): {}", "cannot connect to the X11 server ({}): {}", self.display, e))?);
                }
                match f(self.conn.as_mut().unwrap()) {
                    Ok(v) => return Ok(v),
                    Err(XErr::Other(e)) if attempt == 0 => {
                        let _ = e;
                        self.conn = None;
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            unreachable!()
        }
    }

    impl Backend for X11Backend {
        fn name(&self) -> String {
            if self.wayland { "X11 (XWayland)".into() } else { "X11".into() }
        }

        fn check_supported(&self, src: &Source) -> Result<Option<String>, String> {
            if !self.wayland {
                return Ok(None);
            }
            match src {
                Source::Window { .. } => Ok(Some(tr!(
                    "جلسة Wayland: يمكن تسجيل نوافذ برامج XWayland فقط، وقد تظهر سوداء في بعض بيئات سطح المكتب.",
                    "Wayland session: only XWayland application windows can be recorded, and they may come out black on some desktops."
                ))),
                _ => Err(tr!(
                    "جلسة Wayland: تسجيل الشاشة أو المنطقة غير مدعوم بعد (سيُنتج إطارات سوداء). سجّل الدخول بجلسة X11 أو سجّل نافذة XWayland.",
                    "Wayland session: monitor/region capture is not supported yet (it would record black frames). Log in to an X11 session or record an XWayland window."
                )),
            }
        }

        fn windows(&mut self) -> Result<Vec<WindowEntry>, String> {
            let list = self.with(|x| x.list_windows())?;
            Ok(list.into_iter().map(|w| WindowEntry { id: w.id as u64, title: w.title, class: w.class, size: w.rect.size(), hidden: w.hidden }).collect())
        }

        fn monitors(&mut self) -> Result<Vec<MonitorEntry>, String> {
            let list = self.with(|x| x.monitors())?;
            Ok(list.into_iter().enumerate().map(|(index, m)| MonitorEntry { index, name: m.name, rect: m.rect, primary: m.primary }).collect())
        }

        fn probe(&mut self, src: &Source) -> Availability {
            match src {
                Source::Region(r) => Availability::Visible(*r),
                Source::Desktop => match self.with(|x| x.root_size()) {
                    Ok(s) => Availability::Visible(Rect { x: 0, y: 0, w: s.w, h: s.h }),
                    Err(_) => Availability::Gone,
                },
                Source::Monitor(i) => match self.monitors() {
                    Ok(m) => m.get(*i).map(|m| Availability::Visible(m.rect)).unwrap_or(Availability::Gone),
                    Err(_) => Availability::Gone,
                },
                Source::Window { id, .. } => {
                    let id = *id as u32;
                    match self.with(|x| Ok(x.window_state(id))) {
                        Ok(WinState::Visible(r)) => Availability::Visible(r),
                        Ok(WinState::Hidden) => Availability::Hidden,
                        _ => Availability::Gone,
                    }
                }
            }
        }

        fn video_input(&self, src: &Source, rect: Rect, fps: u32, draw_mouse: bool) -> Vec<String> {
            let disp = if self.display.contains('.') { self.display.clone() } else { format!("{}.0", self.display) };
            let mut a: Vec<String> = vec![
                "-f".into(),
                "x11grab".into(),
                "-framerate".into(),
                fps.to_string(),
                "-draw_mouse".into(),
                if draw_mouse { "1" } else { "0" }.into(),
                "-video_size".into(),
                format!("{}x{}", rect.w, rect.h),
            ];
            match src {
                Source::Window { id, .. } => {
                    a.extend(["-window_id".to_string(), format!("0x{:x}", id), "-i".to_string(), disp]);
                }
                _ => a.extend(["-i".to_string(), format!("{}+{},{}", disp, rect.x.max(0), rect.y.max(0))]),
            }
            a
        }
    }
}
