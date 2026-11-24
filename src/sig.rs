//! Ctrl-C / termination handling without any crate: a flag the main loop polls.
//! The recorder is then stopped through its normal, safe shutdown path.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
static HITS: AtomicUsize = AtomicUsize::new(0);

pub fn requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// How many times the user asked (a second request means "stop waiting, quit now").
pub fn hits() -> usize {
    HITS.load(Ordering::SeqCst)
}

fn flag() {
    STOP.store(true, Ordering::SeqCst);
    HITS.fetch_add(1, Ordering::SeqCst);
}

#[cfg(unix)]
pub fn install() {
    extern "C" fn on_signal(_sig: i32) {
        flag(); // atomics only: async-signal-safe
    }
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SAFETY: installing a handler that only touches atomics. SIGINT = 2, SIGTERM = 15, SIGHUP = 1 on Linux.
    unsafe {
        let h = on_signal as extern "C" fn(i32) as usize;
        signal(2, h);
        signal(15, h);
        signal(1, h);
    }
}

#[cfg(any(windows, miraqab_wincheck))]
mod win {
    use super::flag;
    #[link(name = "kernel32")]
    extern "system" {
        pub fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        pub fn SetConsoleOutputCP(cp: u32) -> i32;
    }
    pub unsafe extern "system" fn on_ctrl(_kind: u32) -> i32 {
        flag();
        1 // handled: do not terminate the process, let the recorder finish safely
    }
}

#[cfg(windows)]
pub fn install() {
    // SAFETY: plain Win32 calls; the handler only touches atomics. 65001 = UTF-8 console output (Arabic).
    unsafe {
        win::SetConsoleOutputCP(65001);
        win::SetConsoleCtrlHandler(Some(win::on_ctrl), 1);
    }
}

#[cfg(not(any(unix, windows)))]
pub fn install() {}

/// Used by tests and the GUI to request a stop programmatically.
pub fn request() {
    flag();
}

pub fn reset() {
    STOP.store(false, Ordering::SeqCst);
    HITS.store(0, Ordering::SeqCst);
}
