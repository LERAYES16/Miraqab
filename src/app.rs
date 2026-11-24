//! Front-end independent application logic shared by the command line and the
//! browser control panel: argument parsing, status/outcome text, system check.

use crate::audio;
use crate::capture;
use crate::ffmpeg::Ffmpeg;
use crate::recorder::{Outcome, OutcomeKind, State, Status};
use crate::settings::{self, config_dir};
use crate::storage;
use crate::tr;

#[derive(Default, Debug)]
pub struct Parsed {
    pub pairs: Vec<(String, String)>,
    pub source: Option<String>,
    pub duration: Option<f64>,
    pub test_tone: Option<u32>,
    pub lang: Option<String>,
    pub positional: Vec<String>,
}

const VALUE_OPTS: &[&str] = &[
    "profile", "fps", "res", "resolution", "bitrate", "preset", "format", "codec", "mode", "name", "countdown", "out", "system-device", "mic-device",
    "system-volume", "mic-volume", "source", "duration", "test-tone", "lang",
];
const FLAG_OPTS: &[&str] = &["system-audio", "mic", "separate-tracks", "adaptive-resources", "mouse"];

pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        let Some(body) = a.strip_prefix("--") else {
            p.positional.push(a.clone());
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (body.to_string(), None),
        };
        let (neg, base) = match name.strip_prefix("no-") {
            Some(b) => (true, b.to_string()),
            None => (false, name.clone()),
        };
        let mut push = |k: &str, v: String| match k {
            "source" => p.source = Some(v),
            "duration" => p.duration = v.parse().ok(),
            "test-tone" => p.test_tone = v.parse().ok(),
            "lang" => p.lang = Some(v),
            _ => p.pairs.push((k.to_string(), v)),
        };
        if FLAG_OPTS.contains(&base.as_str()) {
            let v = inline.unwrap_or_else(|| if neg { "off".into() } else { "on".into() });
            push(&base, v);
        } else if neg && base == "countdown" {
            push("countdown", "0".into());
        } else if VALUE_OPTS.contains(&name.as_str()) {
            let v = match inline {
                Some(v) => v,
                None => {
                    let v = args.get(i).cloned().ok_or_else(|| tr!("الخيار --{} يحتاج قيمة", "option --{} needs a value", name))?;
                    i += 1;
                    v
                }
            };
            if name == "duration" && v.parse::<f64>().map(|d| d <= 0.0).unwrap_or(true) {
                return Err(tr!("المدة يجب أن تكون عددًا موجبًا بالثواني", "duration must be a positive number of seconds"));
            }
            push(&name, v);
        } else {
            return Err(tr!("خيار غير معروف: --{} (اكتب miraqab help)", "unknown option: --{} (run miraqab help)", name));
        }
    }
    Ok(p)
}

/// Saved config first, then the command-line pairs on top.
pub fn effective_settings(pairs: &[(String, String)]) -> Result<settings::Settings, Vec<String>> {
    let (mut s, problems) = settings::load_config();
    let mut errs = problems;
    errs.extend(settings::apply_pairs(&mut s, pairs));
    if errs.is_empty() { Ok(s) } else { Err(errs) }
}

pub fn status_line(s: &Status) -> String {
    let state = match s.state {
        State::Recording => tr!("● يسجّل", "● recording"),
        State::Paused => tr!("‖ متوقف مؤقتًا", "‖ paused"),
        State::AutoPaused => tr!("‖ إيقاف تلقائي (المصدر مخفي)", "‖ auto-paused (source hidden)"),
        State::Finalizing => tr!("… جارٍ إنهاء الملف", "… finalizing"),
    };
    let mut parts = vec![format!("{} {}", state, storage::human_duration(s.recorded_secs)), tr!("{} إطار/ث", "{} fps", s.fps), tr!("الحجم {}", "size {}", storage::human_bytes(s.bytes))];
    if s.dropped + s.duplicated > 0 {
        parts.push(tr!("إطارات مُسقَطة {} / مكرَّرة {}", "dropped {} / duplicated {}", s.dropped, s.duplicated));
    }
    if let Some(c) = s.ffmpeg_cpu_percent {
        parts.push(format!("CPU {:.0}%", c));
    }
    if let Some(f) = s.free_bytes {
        parts.push(tr!("المتاح {}", "free {}", storage::human_bytes(f)));
    }
    parts.join("  |  ")
}

pub fn outcome_lines(o: &Outcome) -> Vec<String> {
    let mut v = Vec::new();
    match &o.kind {
        OutcomeKind::Complete => v.push(tr!("✔ اكتمل التسجيل.", "✔ Recording complete.")),
        OutcomeKind::Partial(why) => {
            v.push(tr!("⚠ تسجيل غير مكتمل: {}", "⚠ INCOMPLETE recording: {}", why));
            v.push(tr!("  حُفظ ما أمكن حفظه وسُمّي الملف بلاحقة -partial.", "  What could be saved was saved; the file name carries the -partial suffix."));
        }
        OutcomeKind::Failed(why) => v.push(tr!("✘ فشل التسجيل: {}", "✘ Recording FAILED: {}", why)),
    }
    if let Some(p) = &o.path {
        v.push(tr!("  الملف: {}", "  file: {}", crate::i18n::ltr(&p.display().to_string())));
        v.push(tr!(
            "  المدة {} | الحجم {} | المقاطع {}",
            "  duration {} | size {} | segments {}",
            storage::human_duration(o.duration),
            storage::human_bytes(o.bytes),
            o.segments
        ));
        v.push(tr!("  إطارات مُسقَطة {} | مكرَّرة {}", "  dropped frames {} | duplicated {}", o.dropped, o.duplicated));
    }
    for a in &o.audio {
        v.push(match a.level {
            Some(l) if !l.is_silent() => tr!("  الصوت «{}»: مُلتقَط فعلًا (الذروة {:.1} dB)", "  audio \"{}\": really captured (peak {:.1} dB)", a.label, l.max_db),
            Some(_) => tr!("  ⚠ الصوت «{}»: المسار موجود لكنه صامت — لم يكن هناك صوت أو اختير جهاز خاطئ.", "  ⚠ audio \"{}\": the track exists but is SILENT — nothing was playing or the wrong device was chosen.", a.label),
            None => tr!("  ⚠ الصوت «{}»: تعذّر التحقق من وجود عيّنات صوتية.", "  ⚠ audio \"{}\": could not verify that samples were captured.", a.label),
        });
    }
    if let Some(d) = &o.segments_dir {
        v.push(tr!("  المقاطع الأصلية محفوظة في: {}", "  original segments kept in: {}", crate::i18n::ltr(&d.display().to_string())));
    }
    v
}

/// (ok, text) lines describing what this machine can do. Nothing is assumed.
pub fn doctor_report() -> Vec<(bool, String)> {
    let mut r: Vec<(bool, String)> = Vec::new();
    let sess = capture::detect_session();
    r.push((sess != capture::SessionKind::Unknown, tr!("الجلسة الرسومية: {:?}", "graphical session: {:?}", sess)));
    if sess == capture::SessionKind::Wayland {
        r.push((false, capture::wayland_only_message()));
    }
    let mut backend = capture::default_backend();
    match &mut backend {
        Ok(b) => {
            r.push((true, tr!("واجهة الالتقاط: {}", "capture backend: {}", b.name())));
            match b.monitors() {
                Ok(m) => r.push((!m.is_empty(), tr!("الشاشات المكتشفة: {}", "monitors found: {}", m.len()))),
                Err(e) => r.push((false, tr!("تعذّر قراءة الشاشات: {}", "cannot read monitors: {}", e))),
            }
            match b.windows() {
                Ok(w) => r.push((true, tr!("النوافذ المكتشفة: {}", "windows found: {}", w.len()))),
                Err(e) => r.push((false, tr!("تعذّر قراءة النوافذ: {}", "cannot read windows: {}", e))),
            }
        }
        Err(e) => r.push((false, e.clone())),
    }
    match Ffmpeg::locate() {
        Err(e) => r.push((false, e)),
        Ok(ff) => {
            r.push((true, format!("FFmpeg: {} ({})", ff.version_text, ff.path.display())));
            let caps = ff.caps();
            let has = |n: &str| caps.encoders.contains(n);
            r.push((has("libx264"), "H.264 (libx264)".to_string()));
            r.push((has("libvpx-vp9"), "VP9 (libvpx-vp9)".to_string()));
            r.push((has("libsvtav1") || has("libaom-av1"), tr!("AV1 (اختياري)", "AV1 (optional)")));
            r.push((has("aac") && has("libopus"), "AAC + Opus".to_string()));
            let dev = if cfg!(windows) { "gdigrab" } else { "x11grab" };
            r.push((caps.devices.contains(dev), tr!("التقاط الشاشة في FFmpeg ({})", "FFmpeg screen capture ({})", dev)));
            match audio::list_devices(&ff) {
                Ok(d) => r.push((d.iter().any(|x| x.monitor), tr!("أجهزة الصوت: {} (منها لصوت النظام: {})", "audio devices: {} (system-sound capable: {})", d.len(), d.iter().filter(|x| x.monitor).count()))),
                Err(e) => r.push((false, e)),
            }
        }
    }
    let out = settings::default_output_dir();
    r.push((true, tr!("مجلد الحفظ الافتراضي: {}", "default output folder: {}", out.display())));
    if let Some(f) = storage::free_space_bytes(&std::env::temp_dir()) {
        r.push((f > 1 << 30, tr!("المساحة الحرة: {}", "free space: {}", storage::human_bytes(f))));
    }
    r.push((true, tr!("ملف الإعدادات: {}", "config folder: {}", config_dir().display())));
    if let Some(m) = crate::recorder::self_rss_mb() {
        r.push((true, tr!("ذاكرة مِرقاب الآن: {:.1} MB", "Miraqab memory now: {:.1} MB", m)));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_values_flags_and_negations() {
        let p = parse_args(&a("--source window:0x1a --fps 20 --res=480p --system-audio --no-mouse --no-countdown --duration 5 pos")).unwrap();
        assert_eq!(p.source.as_deref(), Some("window:0x1a"));
        assert_eq!(p.duration, Some(5.0));
        assert!(p.pairs.contains(&("fps".into(), "20".into())));
        assert!(p.pairs.contains(&("res".into(), "480p".into())));
        assert!(p.pairs.contains(&("system-audio".into(), "on".into())));
        assert!(p.pairs.contains(&("mouse".into(), "off".into())));
        assert!(p.pairs.contains(&("countdown".into(), "0".into())));
        assert_eq!(p.positional, vec!["pos"]);
    }

    #[test]
    fn rejects_unknown_and_incomplete_options() {
        assert!(parse_args(&a("--bogus")).is_err());
        assert!(parse_args(&a("--fps")).is_err());
        assert!(parse_args(&a("--duration -3")).is_err());
        assert!(parse_args(&a("--duration abc")).is_err());
    }

    #[test]
    fn status_line_mentions_drops_only_when_there_are_some() {
        let mut s = Status { state: State::Recording, recorded_secs: 65.0, frames: 0, dropped: 0, duplicated: 0, speed: None, bytes: 2048, segment: 1, capture: Default::default(), canvas: Default::default(), fps: 30, ffmpeg_cpu_percent: Some(12.4), ffmpeg_rss_mb: None, free_bytes: None };
        let l = status_line(&s);
        assert!(l.contains("00:01:05") && l.contains("CPU 12%") && !l.contains("مُسقَطة"));
        s.dropped = 3;
        assert!(status_line(&s).contains("مُسقَطة"));
    }
}
