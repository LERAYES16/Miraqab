//! Localization. Arabic is the default language; English is optional.
//!
//! Every user-visible message is written as an Arabic/English pair next to the
//! code that produces it, via the [`tr!`] macro, so a translation can never
//! drift away from the situation it describes.

use std::sync::atomic::{AtomicBool, Ordering};

static ARABIC: AtomicBool = AtomicBool::new(true);

pub fn set_arabic(on: bool) {
    ARABIC.store(on, Ordering::Relaxed);
}

pub fn is_arabic() -> bool {
    ARABIC.load(Ordering::Relaxed)
}

/// `tr!("نص عربي {}", "english text {}", arg)` -> `String`
#[macro_export]
macro_rules! tr {
    ($ar:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_arabic() { format!($ar $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}

/// Unicode isolates so that Latin technical terms (720p, H.264, MP4, paths)
/// embedded in Arabic text keep their own direction in bidi-aware terminals.
pub fn ltr(s: &str) -> String {
    if is_arabic() {
        format!("\u{2066}{}\u{2069}", s)
    } else {
        s.to_string()
    }
}

/// Detect the interface language from the environment (`LC_ALL`, `LANG`).
/// Returns `None` when nothing is set so the default (Arabic) applies.
pub fn detect_from_env() -> Option<bool> {
    for k in ["MIRAQAB_LANG", "LC_ALL", "LANGUAGE", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.to_lowercase();
            if v.is_empty() || v == "c" || v == "posix" {
                continue;
            }
            if v.starts_with("ar") {
                return Some(true);
            }
            if v.starts_with("en") {
                return Some(false);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, not two: the language is a process-wide global, and cargo runs
    // tests in parallel threads.
    #[test]
    fn macro_and_isolates_follow_language() {
        set_arabic(true);
        assert_eq!(tr!("مرحبا {}", "hello {}", 5), "مرحبا 5");
        assert!(ltr("720p").starts_with('\u{2066}'));
        set_arabic(false);
        assert_eq!(tr!("مرحبا {}", "hello {}", 5), "hello 5");
        assert_eq!(ltr("720p"), "720p");
        set_arabic(true);
    }
}
