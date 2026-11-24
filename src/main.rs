//! Miraqab (مِرقاب) command line.

use miraqab::app::{self, Parsed};
use miraqab::audio;
use miraqab::capture;
use miraqab::ffmpeg::Ffmpeg;
use miraqab::i18n;
use miraqab::recorder::{self, AdaptiveConfig, Cmd, Event, OutcomeKind, Request, State};
use miraqab::settings::{self, Settings};
use miraqab::sig;
use miraqab::storage;
use miraqab::tr;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::mpsc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    sig::install();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // language: --lang ar|en anywhere, or MIRAQAB_LANG; Arabic is the default
    if let Ok(v) = std::env::var("MIRAQAB_LANG") {
        i18n::set_arabic(!v.to_lowercase().starts_with("en"));
    }
    if let Some(i) = args.iter().position(|a| a == "--lang" || a.starts_with("--lang=")) {
        let v = match args[i].split_once('=') {
            Some((_, v)) => v.to_string(),
            None => args.get(i + 1).cloned().unwrap_or_default(),
        };
        i18n::set_arabic(!v.to_lowercase().starts_with("en"));
        args.remove(i);
        if !args[i..].is_empty() && !args[i].starts_with("--") && !args[i].contains(':') && matches!(args[i].as_str(), "ar" | "en") {
            args.remove(i);
        }
    }
    let code = run(args);
    std::process::exit(code);
}

fn run(args: Vec<String>) -> i32 {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("gui");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match cmd {
        "help" | "--help" | "-h" | "مساعدة" => {
            println!("{}", help_text());
            0
        }
        "version" | "--version" | "-V" => {
            println!("miraqab {}", VERSION);
            0
        }
        "record" | "تسجيل" => with_parsed(rest, cmd_record),
        "windows" | "نوافذ" => cmd_windows(),
        "monitors" | "شاشات" => cmd_monitors(),
        "audio" | "صوت" => cmd_audio(),
        "audio-test" => with_parsed(rest, cmd_audio_test),
        "doctor" | "فحص" => cmd_doctor(),
        "recover" | "استرجاع" => with_parsed(rest, cmd_recover),
        "history" | "سجل" => cmd_history(),
        "config" | "إعدادات" => cmd_config(rest),
        "gui" | "واجهة" => with_parsed(rest, cmd_gui),
        other => {
            eprintln!("{}", tr!("أمر غير معروف: {}\nاكتب miraqab help لعرض المساعدة.", "unknown command: {}\nRun miraqab help.", other));
            2
        }
    }
}

fn with_parsed(rest: &[String], f: fn(Parsed) -> i32) -> i32 {
    match app::parse_args(rest) {
        Ok(p) => f(p),
        Err(e) => {
            eprintln!("{}", e);
            2
        }
    }
}

fn locate_ffmpeg() -> Result<Ffmpeg, i32> {
    Ffmpeg::locate().map_err(|e| {
        eprintln!("{}", e);
        1
    })
}

fn cmd_windows() -> i32 {
    let mut b = match capture::default_backend() {
        Ok(b) => b,
        Err(e) => return fail(&e),
    };
    match b.windows() {
        Ok(list) if list.is_empty() => println!("{}", tr!("لا توجد نوافذ قابلة للتسجيل.", "no recordable windows found.")),
        Ok(list) => {
            println!("{}", tr!("النوافذ المتاحة (استعمل المعرّف مع --source window:ID أو جزءًا من العنوان مع --source title:TEXT):", "Available windows (use the id with --source window:ID, or part of the title with --source title:TEXT):"));
            for w in list {
                let hidden = if w.hidden { tr!(" [مخفية]", " [hidden]") } else { String::new() };
                println!("  0x{:<9x} {:>9}  {}{}{}", w.id, w.size.to_string(), w.title, if w.class.is_empty() { String::new() } else { format!("  ({})", w.class) }, hidden);
            }
        }
        Err(e) => return fail(&e),
    }
    0
}

fn cmd_monitors() -> i32 {
    let mut b = match capture::default_backend() {
        Ok(b) => b,
        Err(e) => return fail(&e),
    };
    match b.monitors() {
        Ok(list) => {
            for m in list {
                println!("  {}  {}  {}x{} @ ({},{}){}", m.index + 1, m.name, m.rect.w, m.rect.h, m.rect.x, m.rect.y, if m.primary { tr!("  [رئيسية]", "  [primary]") } else { String::new() });
            }
            println!("{}", tr!("استعمل --source monitor:N لتسجيل شاشة واحدة.", "Use --source monitor:N to record one monitor."));
            0
        }
        Err(e) => fail(&e),
    }
}

fn cmd_audio() -> i32 {
    let ff = match locate_ffmpeg() {
        Ok(f) => f,
        Err(c) => return c,
    };
    match audio::list_devices(&ff) {
        Ok(list) => {
            for d in list {
                let kind = if d.monitor { tr!("صوت النظام", "system sound") } else { tr!("ميكروفون/إدخال", "microphone/input") };
                println!("  {}{}  [{}]  {}", if d.default { "*" } else { " " }, d.id, kind, d.label);
            }
            println!("{}", tr!("استعمل --system-device و --mic-device مع اسم الجهاز.", "Use --system-device and --mic-device with a device name."));
            0
        }
        Err(e) => fail(&e),
    }
}

fn cmd_audio_test(p: Parsed) -> i32 {
    let ff = match locate_ffmpeg() {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut st = match app::effective_settings(&p.pairs) {
        Ok(s) => s,
        Err(e) => return fail(&e.join("\n")),
    };
    if !st.system_audio && !st.mic {
        st.system_audio = true;
    }
    let inputs = match audio::resolve_inputs(&ff, &st, p.test_tone) {
        Ok(i) => i,
        Err(e) => return fail(&e.join("\n")),
    };
    println!("{}", tr!("أقيس مستوى الصوت 3 ثوانٍ (شغّل صوتًا الآن)…", "Measuring the level for 3 seconds (play some sound now)…"));
    let mut code = 0;
    for (i, (inp, _)) in inputs.iter().enumerate() {
        let what = if i == 0 && st.system_audio { tr!("صوت النظام", "system audio") } else { tr!("الميكروفون", "microphone") };
        match audio::measure_input(&ff, inp, 3.0) {
            Ok(Some(l)) if !l.is_silent() => println!("  ✔ {}: {}", what, tr!("يُلتقط فعلًا (الذروة {:.1} dB، المتوسط {:.1} dB)", "really captured (peak {:.1} dB, mean {:.1} dB)", l.max_db, l.mean_db)),
            Ok(Some(_)) => {
                println!("  ⚠ {}: {}", what, tr!("الجهاز يعمل لكن الصوت صامت.", "the device opens but the signal is silent."));
                code = 3;
            }
            Ok(None) => {
                println!("  ⚠ {}: {}", what, tr!("لم تصل عيّنات صوتية.", "no samples arrived."));
                code = 3;
            }
            Err(e) => {
                println!("  ✘ {}: {}", what, e);
                code = 1;
            }
        }
    }
    code
}

fn cmd_doctor() -> i32 {
    println!("Miraqab {} — {}", VERSION, tr!("فحص النظام", "system check"));
    let mut bad = 0;
    for (ok, line) in app::doctor_report() {
        println!(" {} {}", if ok { "✔" } else { "✘" }, line);
        if !ok {
            bad += 1;
        }
    }
    if bad > 0 {
        println!("{}", tr!("\nبعض الميزات غير متاحة على هذا الجهاز (✘). لن يدّعي مِرقاب دعمها.", "\nSome features are unavailable on this machine (✘). Miraqab will not pretend otherwise."));
    }
    0
}

fn cmd_history() -> i32 {
    let h = storage::read_history();
    if h.is_empty() {
        println!("{}", tr!("لا توجد تسجيلات في السجل.", "no recordings in the history yet."));
    }
    for (ts, status, path, secs, bytes) in h.iter().rev().take(20) {
        let d = storage::utc_from_secs(*ts);
        println!("  {:04}-{:02}-{:02} {:02}:{:02} UTC  {:<8} {}  {}  {}", d.year, d.month, d.day, d.hour, d.min, status, storage::human_duration(*secs), storage::human_bytes(*bytes), path);
    }
    0
}

fn cmd_recover(p: Parsed) -> i32 {
    let ff = match locate_ffmpeg() {
        Ok(f) => f,
        Err(c) => return c,
    };
    let dir = match p.positional.first() {
        Some(d) => std::path::PathBuf::from(d),
        None => match app::effective_settings(&p.pairs) {
            Ok(s) => s.output_dir,
            Err(e) => return fail(&e.join("\n")),
        },
    };
    let found = storage::find_interrupted(&dir);
    if found.is_empty() {
        println!("{}", tr!("لا توجد تسجيلات منقطعة في {}", "no interrupted recordings in {}", dir.display()));
        return 0;
    }
    let mut code = 0;
    for d in found {
        println!("{}", tr!("أسترجع: {}", "recovering: {}", d.display()));
        match recorder::recover_dir(&ff, &d) {
            Ok(out) => println!("  ✔ {}", tr!("تم: {}", "done: {}", out.display())),
            Err(e) => {
                println!("  ✘ {}", e);
                code = 1;
            }
        }
    }
    code
}

fn cmd_config(rest: &[String]) -> i32 {
    match rest.first().map(|s| s.as_str()) {
        Some("path") => {
            println!("{}", settings::config_dir().join("config.conf").display());
            0
        }
        Some("set") if rest.len() >= 3 => match settings::save_config_value(&rest[1], &rest[2]) {
            Ok(p) => {
                println!("{}", tr!("حُفظ في {}", "saved to {}", p.display()));
                0
            }
            Err(e) => fail(&e),
        },
        None | Some("show") => match app::effective_settings(&[]) {
            Ok(s) => {
                print_settings(&s);
                0
            }
            Err(e) => fail(&e.join("\n")),
        },
        _ => {
            eprintln!("{}", tr!("الاستعمال: miraqab config show | path | set KEY VALUE", "usage: miraqab config show | path | set KEY VALUE"));
            2
        }
    }
}

fn print_settings(s: &Settings) {
    let onoff = |b: bool| if b { "on" } else { "off" };
    println!("  profile = {}  ({})", s.profile.id(), s.profile.label());
    println!("  fps = {}\n  res = {}\n  bitrate = {}", s.fps, s.resolution.label(), s.bitrate_kbps.map(|b| format!("{}k", b)).unwrap_or_else(|| "auto".into()));
    println!("  format = {}  codec = {}  mode = {}", s.container.ext(), s.effective_codec().name(), if s.resize_mode == settings::ResizeMode::Fixed { "fixed" } else { "adaptive" });
    println!("  system-audio = {}  mic = {}  separate-tracks = {}", onoff(s.system_audio), onoff(s.mic), onoff(s.separate_tracks));
    println!("  system-volume = {}  mic-volume = {}  mouse = {}", s.system_volume, s.mic_volume, onoff(s.draw_mouse));
    println!("  countdown = {}  adaptive-resources = {}", s.countdown, onoff(s.adaptive_resources));
    println!("  out = {}", s.output_dir.display());
}

fn fail(msg: &str) -> i32 {
    eprintln!("✘ {}", msg);
    1
}

fn cmd_gui(p: Parsed) -> i32 {
    let port = p.positional.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    match miraqab::gui::run(port) {
        Ok(()) => 0,
        Err(e) => fail(&e),
    }
}

// ---- record ----------------------------------------------------------------------

fn cmd_record(p: Parsed) -> i32 {
    let ff = match locate_ffmpeg() {
        Ok(f) => f,
        Err(c) => return c,
    };
    let st = match app::effective_settings(&p.pairs) {
        Ok(s) => s,
        Err(e) => return fail(&e.join("\n")),
    };
    let mut backend = match capture::default_backend() {
        Ok(b) => b,
        Err(e) => return fail(&e),
    };
    let src = match capture::parse_source(p.source.as_deref().unwrap_or("desktop"), backend.as_mut()) {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    println!("{}", tr!("المصدر: {}", "source: {}", src.describe()));

    for i in (1..=st.countdown).rev() {
        print!("\r{}", tr!("يبدأ التسجيل بعد {} …  ", "recording starts in {} …  ", i));
        let _ = std::io::stdout().flush();
        for _ in 0..10 {
            if sig::requested() {
                println!("\n{}", tr!("أُلغي.", "cancelled."));
                return 130;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if st.countdown > 0 {
        println!();
    }

    let req = Request { settings: st.clone(), source: src, max_seconds: p.duration, test_tone: p.test_tone, adaptive: AdaptiveConfig::default() };
    let (h, info) = match recorder::start(req, backend, ff) {
        Ok(x) => x,
        Err(errs) => {
            for e in errs {
                eprintln!("✘ {}", e);
            }
            return 1;
        }
    };
    println!("{}", tr!("الملف: {}", "file: {}", i18n::ltr(&info.final_path.display().to_string())));
    println!(
        "{}",
        tr!(
            "الإخراج {} | {} إطار/ث | {} | معدل البت {} kbps | تقدير الحجم ≈ {:.0} MB/دقيقة",
            "output {} | {} fps | {} | {} kbps | estimated ≈ {:.0} MB/minute",
            info.canvas,
            info.fps,
            info.codec,
            info.kbps,
            info.est_mb_per_min
        )
    );
    if info.audio_labels.is_empty() {
        println!("{}", tr!("الصوت: بلا صوت", "audio: none"));
    } else {
        println!("{}", tr!("الصوت: {} (يُتحقَّق منه بعد الانتهاء)", "audio: {} (verified after the recording ends)", info.audio_labels.join(" | ")));
    }
    for w in &info.warnings {
        println!("⚠ {}", w);
    }
    println!("{}", tr!("اضغط  p  ثم Enter للإيقاف المؤقت/الاستئناف، و s ثم Enter للإيقاف (أو Ctrl+C).", "Type p + Enter to pause/resume, s + Enter to stop (or press Ctrl+C)."));

    let (ktx, krx) = mpsc::channel::<String>();
    if std::io::stdin().is_terminal() {
        std::thread::spawn(move || {
            for l in std::io::stdin().lock().lines().map_while(Result::ok) {
                if ktx.send(l.trim().to_lowercase()).is_err() {
                    break;
                }
            }
        });
    }
    let tty = std::io::stdout().is_terminal();
    let (mut stopping, mut paused, mut n) = (false, false, 0u32);
    loop {
        match h.events.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Notice(m)) => println!("{}• {}", if tty { "\r\x1b[K" } else { "" }, m),
            Ok(Event::Warning(m)) => println!("{}⚠ {}", if tty { "\r\x1b[K" } else { "" }, m),
            Ok(Event::Status(s)) => {
                paused = matches!(s.state, State::Paused | State::AutoPaused);
                n += 1;
                if tty {
                    print!("\r\x1b[K{}", app::status_line(&s));
                    let _ = std::io::stdout().flush();
                } else if n % 5 == 1 {
                    println!("{}", app::status_line(&s));
                }
            }
            Ok(Event::Started(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(k) = krx.try_recv() {
            match k.as_str() {
                "p" | "ح" => h.send(if paused { Cmd::Resume } else { Cmd::Pause }),
                "s" | "q" | "س" => {
                    stopping = true;
                    h.send(Cmd::Stop)
                }
                _ => {}
            }
        }
        if sig::requested() && !stopping {
            stopping = true;
            println!("\n{}", tr!("جارٍ الإيقاف وإنهاء الملف بأمان… (اضغط Ctrl+C مرة أخرى للخروج فورًا)", "stopping and finalizing safely… (press Ctrl+C again to quit immediately)"));
            h.send(Cmd::Stop);
        }
        if sig::hits() >= 2 {
            eprintln!("{}", tr!("خروج فوري. إن بقيت مقاطع فشغّل: miraqab recover", "quitting now. If segments remain, run: miraqab recover"));
            return 130;
        }
        if h.is_finished() {
            break;
        }
    }
    if tty {
        println!();
    }
    let o = h.wait();
    for l in app::outcome_lines(&o) {
        println!("{}", l);
    }
    match o.kind {
        OutcomeKind::Complete => 0,
        OutcomeKind::Partial(_) => 2,
        OutcomeKind::Failed(_) => 1,
    }
}

fn help_text() -> String {
    if i18n::is_arabic() {
        r#"مِرقاب — مسجّل شاشة خفيف بواجهة عربية

الأوامر:
  miraqab                       فتح لوحة التحكم العربية في المتصفح (miraqab gui)
  miraqab record [خيارات]        تسجيل من سطر الأوامر
  miraqab windows               عرض النوافذ المتاحة للتسجيل
  miraqab monitors              عرض الشاشات
  miraqab audio                 عرض أجهزة الصوت
  miraqab audio-test [--mic]    قياس مستوى الصوت 3 ثوانٍ
  miraqab doctor                فحص الجهاز وما يدعمه فعلًا
  miraqab recover [المجلد]       استرجاع تسجيل انقطع فجأة
  miraqab history               سجل التسجيلات
  miraqab config show|path|set  عرض الإعدادات أو حفظ قيمة

خيارات التسجيل:
  --source desktop|monitor:N|window:ID|title:نص|region:X,Y,WxH
  --profile ultralight|low|balanced|high|custom
  --fps 5|10|15|20|24|30|60 (أو أي قيمة من 1 إلى 120)
  --res 144p|240p|360p|480p|720p|1080p|original|WxH
  --bitrate 2500        (كيلوبت/ث، أو auto)
  --format mp4|mkv|webm      --codec h264|vp9|av1
  --mode fixed|adaptive
  --system-audio  --mic  --separate-tracks  --system-volume 100  --mic-volume 100
  --system-device الاسم  --mic-device الاسم
  --no-mouse  --no-countdown  --countdown 3  --adaptive-resources
  --out المجلد  --name الاسم  --duration ثوانٍ  --lang ar|en

الفرق بين نمطَي تغيّر حجم النافذة:
  fixed (الافتراضي، «الدقة الثابتة»): يبقى حجم الفيديو كما اخترته مهما تغيّر حجم النافذة؛
      تُكبَّر الصورة أو تُصغَّر داخل الإطار مع الحفاظ على نسبتها (أشرطة سوداء عند اللزوم، دون تمطيط).
      ملف واحد مستقر. عند تغيّر الحجم يحدث انقطاع قصير بين مقطعين.
  adaptive («الدقة المتكيّفة»): يبدأ مقطعًا جديدًا بحجم النافذة الأصلي كلما تغيّر حجمها،
      ثم تُدمج المقاطع في النهاية بإعادة ترميز واحدة إلى أكبر حجم (يستهلك معالجًا ووقتًا عند الإنهاء).
      تبقى المقاطع الأصلية محفوظة في مجلد _segments للاسترجاع.

لا يضمن مِرقاب معدل إطارات أو دقة معيّنة على كل جهاز؛ راقب خانة «إطارات مُسقَطة» أثناء التسجيل."#
            .to_string()
    } else {
        r#"Miraqab — lightweight, Arabic-first screen recorder

Commands:
  miraqab                       open the Arabic control panel in your browser (miraqab gui)
  miraqab record [options]      record from the command line
  miraqab windows | monitors | audio | audio-test [--mic] | doctor | history
  miraqab recover [DIR]         recover an interrupted recording
  miraqab config show|path|set KEY VALUE

Record options:
  --source desktop|monitor:N|window:ID|title:TEXT|region:X,Y,WxH
  --profile ultralight|low|balanced|high|custom
  --fps 5|10|15|20|24|30|60 (or 1-120)   --res 144p..1080p|original|WxH   --bitrate KBPS|auto
  --format mp4|mkv|webm   --codec h264|vp9|av1   --mode fixed|adaptive
  --system-audio --mic --separate-tracks --system-volume N --mic-volume N
  --system-device NAME --mic-device NAME --no-mouse --no-countdown --adaptive-resources
  --out DIR --name NAME --duration SECONDS --lang ar|en

Window-resize modes:
  fixed (default): the video keeps the chosen size; the picture is scaled inside it keeping its aspect
      ratio (black bars when needed, never stretched). One stable file; a short gap occurs at each resize.
  adaptive: a new segment starts at the window's own size on every change; segments are joined at the
      end with one re-encode to the largest size (costs CPU/time at the end). Originals are kept in _segments.

Miraqab does not promise any FPS/resolution on every machine; watch the dropped-frames counter."#
            .to_string()
    }
}
