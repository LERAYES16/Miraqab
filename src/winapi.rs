//! Windows backend: window and monitor discovery through a handful of user32 calls
//! declared by hand (no crates), plus FFmpeg `gdigrab` input arguments.
//!
//! STATUS: this file has been compiler-checked on Linux with `--cfg miraqab_wincheck`
//! (types and logic only). It has NOT been linked or run on Windows by its author.
//! See docs/windows-support.md for exactly what is and is not verified.
//!
//! Windows 10/11 are the supported targets. FFmpeg's `gdigrab` itself also works on
//! older Windows, but Rust's current compiler target requires Windows 10, so Windows
//! 7/8.1 builds are out of scope for this version.

use crate::capture::{Availability, Backend, MonitorEntry, Source, WindowEntry};
use crate::geometry::{Rect, Size};
use crate::storage::DateTime;
use crate::tr;
use std::path::Path;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rc {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pt {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: Rc,
    work: Rc,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    dow: u16,
    day: u16,
    hour: u16,
    min: u16,
    sec: u16,
    ms: u16,
}

type EnumWindowsProc = unsafe extern "system" fn(isize, isize) -> i32;
type EnumMonitorsProc = unsafe extern "system" fn(isize, isize, *mut Rc, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: Option<EnumWindowsProc>, lparam: isize) -> i32;
    fn GetWindowTextLengthW(hwnd: isize) -> i32;
    fn GetWindowTextW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
    fn IsWindow(hwnd: isize) -> i32;
    fn IsWindowVisible(hwnd: isize) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn GetClientRect(hwnd: isize, rc: *mut Rc) -> i32;
    fn ClientToScreen(hwnd: isize, pt: *mut Pt) -> i32;
    fn GetWindowLongW(hwnd: isize, index: i32) -> i32;
    fn EnumDisplayMonitors(hdc: isize, clip: *const Rc, cb: Option<EnumMonitorsProc>, data: isize) -> i32;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetProcessDPIAware() -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: isize, attr: u32, out: *mut u32, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(t: *mut SystemTime);
    fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
}

const GWL_EXSTYLE: i32 = -20;
const WS_EX_TOOLWINDOW: i32 = 0x0000_0080;
const DWMWA_CLOAKED: u32 = 14;

unsafe extern "system" fn collect_window(hwnd: isize, lparam: isize) -> i32 {
    // SAFETY: lparam is the address of the Vec<isize> owned by `top_level_windows`, alive for the whole call.
    let v = &mut *(lparam as *mut Vec<isize>);
    v.push(hwnd);
    1
}

unsafe extern "system" fn collect_monitor(hmon: isize, _hdc: isize, _rc: *mut Rc, lparam: isize) -> i32 {
    // SAFETY: lparam is the address of the Vec<isize> owned by `monitors_raw`, alive for the whole call.
    let v = &mut *(lparam as *mut Vec<isize>);
    v.push(hmon);
    1
}

fn title_of(hwnd: isize) -> String {
    // SAFETY: the buffer is sized from GetWindowTextLengthW + 1 and GetWindowTextW writes at most that many units.
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), n + 1);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn client_rect(hwnd: isize) -> Option<Rect> {
    // SAFETY: plain out-parameters of the right size.
    unsafe {
        let mut rc = Rc::default();
        if GetClientRect(hwnd, &mut rc) == 0 {
            return None;
        }
        let mut p = Pt { x: 0, y: 0 };
        if ClientToScreen(hwnd, &mut p) == 0 {
            return None;
        }
        Some(Rect { x: p.x, y: p.y, w: (rc.right - rc.left).max(0) as u32, h: (rc.bottom - rc.top).max(0) as u32 })
    }
}

fn is_cloaked(hwnd: isize) -> bool {
    // SAFETY: out-parameter is a u32 as DWMWA_CLOAKED requires.
    unsafe {
        let mut v: u32 = 0;
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v, 4) == 0 && v != 0
    }
}

fn monitors_raw() -> Vec<MonitorEntry> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(0, std::ptr::null(), Some(collect_monitor), &mut handles as *mut Vec<isize> as isize);
    }
    let mut out = Vec::new();
    for h in handles {
        let mut mi = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: Rc::default(), work: Rc::default(), flags: 0 };
        // SAFETY: `mi.size` is set as the API requires.
        if unsafe { GetMonitorInfoW(h, &mut mi) } != 0 {
            let r = mi.monitor;
            out.push(MonitorEntry {
                index: out.len(),
                name: format!("DISPLAY{}", out.len() + 1),
                rect: Rect { x: r.left, y: r.top, w: (r.right - r.left).max(0) as u32, h: (r.bottom - r.top).max(0) as u32 },
                primary: mi.flags & 1 != 0,
            });
        }
    }
    out
}

fn virtual_desktop() -> Rect {
    // SM_XVIRTUALSCREEN=76, SM_YVIRTUALSCREEN=77, SM_CXVIRTUALSCREEN=78, SM_CYVIRTUALSCREEN=79
    // SAFETY: GetSystemMetrics has no preconditions.
    unsafe { Rect { x: GetSystemMetrics(76), y: GetSystemMetrics(77), w: GetSystemMetrics(78).max(0) as u32, h: GetSystemMetrics(79).max(0) as u32 } }
}

pub struct WindowsBackend {
    /// (handle, title) of the last window listing; used to warn about duplicate titles.
    known: Vec<(u64, String)>,
}

impl WindowsBackend {
    pub fn new() -> WindowsBackend {
        // Report physical pixels, matching what the screen grabber captures on scaled displays.
        // SAFETY: no preconditions.
        unsafe {
            SetProcessDPIAware();
        }
        WindowsBackend { known: Vec::new() }
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for WindowsBackend {
    fn name(&self) -> String {
        "Windows (gdigrab)".into()
    }

    fn check_supported(&self, src: &Source) -> Result<Option<String>, String> {
        if let Source::Window { id, title } = src {
            let same = self.known.iter().filter(|(i, t)| t == title && i != id).count();
            if same > 0 {
                return Err(tr!(
                    "توجد نوافذ أخرى بنفس العنوان «{}»؛ يلتقط FFmpeg النافذة بعنوانها فقط، لذا قد يُسجَّل غير المقصود. غيّر عنوان إحداها أو أغلق المكررة.",
                    "other windows share the title \"{}\"; FFmpeg captures a window by title only, so the wrong one could be recorded. Rename or close the duplicates.",
                    title
                ));
            }
        }
        Ok(None)
    }

    fn windows(&mut self) -> Result<Vec<WindowEntry>, String> {
        let mut handles: Vec<isize> = Vec::new();
        // SAFETY: the callback only pushes into `handles`, which outlives the call.
        let ok = unsafe { EnumWindows(Some(collect_window), &mut handles as *mut Vec<isize> as isize) };
        if ok == 0 {
            return Err(tr!("تعذّر سرد النوافذ", "cannot enumerate windows"));
        }
        let mut out = Vec::new();
        for h in handles {
            // SAFETY: simple queries on a handle that may have died in the meantime (they then just fail).
            let (visible, tool) = unsafe { (IsWindowVisible(h) != 0, GetWindowLongW(h, GWL_EXSTYLE) & WS_EX_TOOLWINDOW != 0) };
            if !visible || tool || is_cloaked(h) {
                continue;
            }
            let title = title_of(h);
            if title.is_empty() {
                continue;
            }
            let Some(r) = client_rect(h) else { continue };
            let hidden = unsafe { IsIconic(h) != 0 };
            if r.w < 2 || r.h < 2 {
                continue;
            }
            out.push(WindowEntry { id: h as u64, title, class: String::new(), size: r.size(), hidden });
        }
        self.known = out.iter().map(|w| (w.id, w.title.clone())).collect();
        Ok(out)
    }

    fn monitors(&mut self) -> Result<Vec<MonitorEntry>, String> {
        let m = monitors_raw();
        if m.is_empty() { Err(tr!("لم تُكتشف أي شاشة", "no monitor was detected")) } else { Ok(m) }
    }

    fn probe(&mut self, src: &Source) -> Availability {
        match src {
            Source::Region(r) => Availability::Visible(*r),
            Source::Desktop => Availability::Visible(virtual_desktop()),
            Source::Monitor(i) => monitors_raw().get(*i).map(|m| Availability::Visible(m.rect)).unwrap_or(Availability::Gone),
            Source::Window { id, .. } => {
                let h = *id as isize;
                // SAFETY: simple queries; an invalid handle just yields 0.
                unsafe {
                    if IsWindow(h) == 0 {
                        return Availability::Gone;
                    }
                    if IsIconic(h) != 0 || IsWindowVisible(h) == 0 {
                        return Availability::Hidden;
                    }
                }
                match client_rect(h) {
                    Some(r) if r.w >= 2 && r.h >= 2 => Availability::Visible(r),
                    Some(_) => Availability::Hidden,
                    None => Availability::Gone,
                }
            }
        }
    }

    fn video_input(&self, src: &Source, rect: Rect, fps: u32, draw_mouse: bool) -> Vec<String> {
        let mut a: Vec<String> = ["-f", "gdigrab", "-framerate"].iter().map(|s| s.to_string()).collect();
        a.push(fps.to_string());
        a.extend(["-draw_mouse".to_string(), if draw_mouse { "1" } else { "0" }.to_string()]);
        match src {
            Source::Desktop => a.extend(["-i".to_string(), "desktop".to_string()]),
            Source::Monitor(_) | Source::Region(_) => a.extend([
                "-offset_x".to_string(),
                rect.x.to_string(),
                "-offset_y".to_string(),
                rect.y.to_string(),
                "-video_size".to_string(),
                format!("{}x{}", rect.w, rect.h),
                "-i".to_string(),
                "desktop".to_string(),
            ]),
            Source::Window { title, .. } => a.extend(["-i".to_string(), format!("title={}", title)]),
        }
        a
    }
}

/// Free bytes available to the current user on the volume of `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    let wide: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and the out-pointers are valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok != 0 { Some(avail) } else { None }
}

/// Local wall-clock time.
pub fn local_time() -> Option<DateTime> {
    let mut t = SystemTime::default();
    // SAFETY: `t` is a valid SYSTEMTIME (eight WORDs).
    unsafe { GetLocalTime(&mut t) };
    if t.year < 2000 {
        return None;
    }
    Some(DateTime { year: t.year as i32, month: t.month as u32, day: t.day as u32, hour: t.hour as u32, min: t.min as u32, sec: t.sec as u32 })
}

#[allow(dead_code)]
fn _size_check(_: Size) {}
