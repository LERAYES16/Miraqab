//! Recording settings: profiles, FPS, resolution, bitrate, formats, audio options.
//!
//! One function ([`Settings::apply`]) is used by both the command line and the
//! config file, so the two can never disagree about what a key means.

use crate::geometry::Size;
use crate::tr;
use std::path::PathBuf;

pub const FPS_PRESETS: [u32; 7] = [5, 10, 15, 20, 24, 30, 60];
pub const FPS_MIN: u32 = 1;
pub const FPS_MAX: u32 = 120;
pub const BITRATE_MIN_KBPS: u32 = 50;
pub const BITRATE_MAX_KBPS: u32 = 100_000;
/// Height preset -> 16:9 canvas width. Explicit table: no rounding surprises.
pub const HEIGHT_PRESETS: [(u32, u32); 6] =
    [(144, 256), (240, 426), (360, 640), (480, 854), (720, 1280), (1080, 1920)];
pub const X264_PRESETS: [&str; 6] = ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    UltraLight,
    Low,
    Balanced,
    High,
    Custom,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Profile> {
        match s.trim().to_lowercase().replace(['_', ' '], "-").as_str() {
            "ultralight" | "ultra-light" | "ultra" | "خفيف-جدا" | "خفيف-جدًا" => Some(Profile::UltraLight),
            "low" | "منخفض" => Some(Profile::Low),
            "balanced" | "متوازن" => Some(Profile::Balanced),
            "high" | "عالي" | "عالٍ" => Some(Profile::High),
            "custom" | "مخصص" => Some(Profile::Custom),
            _ => None,
        }
    }
    pub fn id(&self) -> &'static str {
        match self {
            Profile::UltraLight => "ultralight",
            Profile::Low => "low",
            Profile::Balanced => "balanced",
            Profile::High => "high",
            Profile::Custom => "custom",
        }
    }
    pub fn label(&self) -> String {
        match self {
            Profile::UltraLight => tr!("خفيف جدًا", "Ultra Light"),
            Profile::Low => tr!("منخفض", "Low"),
            Profile::Balanced => tr!("متوازن", "Balanced"),
            Profile::High => tr!("عالٍ", "High"),
            Profile::Custom => tr!("مخصص", "Custom"),
        }
    }
    pub const ALL: [Profile; 5] =
        [Profile::UltraLight, Profile::Low, Profile::Balanced, Profile::High, Profile::Custom];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Canvas = size of the source when recording starts.
    Original,
    /// A 16:9 canvas of this height (144, 240, 360, 480, 720, 1080).
    Height(u32),
    /// Explicit canvas.
    Custom(Size),
}

impl Resolution {
    pub fn parse(s: &str) -> Result<Resolution, String> {
        let t = s.trim().to_lowercase();
        if matches!(t.as_str(), "original" | "source" | "أصلية" | "الأصلية" | "أصلي") {
            return Ok(Resolution::Original);
        }
        let bad = || tr!("دقة غير معروفة: {} (أمثلة: 480p أو 720p أو 1280x720 أو original)", "unknown resolution: {} (examples: 480p, 720p, 1280x720, original)", s);
        if let Some(h) = t.strip_suffix('p') {
            let h: u32 = h.parse().map_err(|_| bad())?;
            return if HEIGHT_PRESETS.iter().any(|(ph, _)| *ph == h) {
                Ok(Resolution::Height(h))
            } else {
                Err(tr!("الارتفاع {}p غير مدعوم كإعداد جاهز (المتاح: 144 240 360 480 720 1080). للقيم الأخرى استخدم WxH", "{}p is not a preset (available: 144 240 360 480 720 1080). Use WxH for other values", h))
            };
        }
        let norm = t.replace(['×', 'X'], "x");
        if let Some((w, h)) = norm.split_once('x') {
            let w: u32 = w.parse().map_err(|_| bad())?;
            let h: u32 = h.parse().map_err(|_| bad())?;
            if !(64..=7680).contains(&w) || !(64..=7680).contains(&h) {
                return Err(tr!("أبعاد الدقة يجب أن تكون بين 64 و7680 بكسل", "resolution sides must be between 64 and 7680 pixels"));
            }
            if w % 2 != 0 || h % 2 != 0 {
                return Err(tr!("أبعاد الدقة يجب أن تكون أعدادًا زوجية (مثل {}x{})", "resolution sides must be even numbers (e.g. {}x{})", w & !1, h & !1));
            }
            return Ok(Resolution::Custom(Size::new(w, h)));
        }
        if let Ok(h) = t.parse::<u32>() {
            return Resolution::parse(&format!("{}p", h));
        }
        Err(bad())
    }

    /// The output canvas for a recording whose source is `initial` when it starts.
    pub fn canvas(&self, initial: Size) -> Size {
        match self {
            Resolution::Original => initial.even(),
            Resolution::Height(h) => {
                let w = HEIGHT_PRESETS.iter().find(|(ph, _)| ph == h).map(|(_, w)| *w).unwrap_or(1280);
                Size::new(w, *h)
            }
            Resolution::Custom(s) => *s,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Resolution::Original => tr!("الأصلية", "Original"),
            Resolution::Height(h) => format!("{}p", h),
            Resolution::Custom(s) => s.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mkv,
    WebM,
}

impl Container {
    pub fn parse(s: &str) -> Option<Container> {
        match s.trim().to_lowercase().as_str() {
            "mp4" => Some(Container::Mp4),
            "mkv" | "matroska" => Some(Container::Mkv),
            "webm" => Some(Container::WebM),
            _ => None,
        }
    }
    pub fn ext(&self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mkv => "mkv",
            Container::WebM => "webm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Vp9,
    Av1,
}

impl Codec {
    pub fn parse(s: &str) -> Option<Codec> {
        match s.trim().to_lowercase().replace('.', "").as_str() {
            "h264" | "x264" | "avc" => Some(Codec::H264),
            "vp9" => Some(Codec::Vp9),
            "av1" => Some(Codec::Av1),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
        }
    }
    pub fn allowed_in(&self, c: Container) -> bool {
        match c {
            Container::Mkv => true,
            Container::Mp4 => matches!(self, Codec::H264 | Codec::Av1),
            Container::WebM => matches!(self, Codec::Vp9 | Codec::Av1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeMode {
    /// Mode A: constant output size; the picture is scaled into it (default).
    Fixed,
    /// Mode B: a new segment whenever the source size changes; joined at the end when possible.
    Adaptive,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub profile: Profile,
    pub fps: u32,
    pub resolution: Resolution,
    pub bitrate_kbps: Option<u32>,
    pub preset: Option<String>,
    pub container: Container,
    pub codec: Option<Codec>,
    pub resize_mode: ResizeMode,
    pub system_audio: bool,
    pub mic: bool,
    pub system_device: Option<String>,
    pub mic_device: Option<String>,
    pub system_volume: u32,
    pub mic_volume: u32,
    pub separate_tracks: bool,
    pub draw_mouse: bool,
    pub countdown: u32,
    pub output_dir: PathBuf,
    pub name: Option<String>,
    pub adaptive_resources: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        let mut s = Settings {
            profile: Profile::Balanced,
            fps: 30,
            resolution: Resolution::Height(720),
            bitrate_kbps: None,
            preset: None,
            container: Container::Mp4,
            codec: None,
            resize_mode: ResizeMode::Fixed,
            system_audio: false,
            mic: false,
            system_device: None,
            mic_device: None,
            system_volume: 100,
            mic_volume: 100,
            separate_tracks: false,
            draw_mouse: true,
            countdown: 3,
            output_dir: default_output_dir(),
            name: None,
            adaptive_resources: false,
        };
        s.set_profile(Profile::Balanced);
        s
    }
}

pub fn default_output_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    home.unwrap_or_else(|| PathBuf::from(".")).join("Videos").join("Miraqab")
}

/// Heuristic bits-per-pixel rate for screen content: ~0.07 bpp, clamped.
pub fn suggest_bitrate(canvas: Size, fps: u32) -> u32 {
    let kbps = (canvas.w as u64 * canvas.h as u64 * fps as u64 * 7) / 100 / 1000;
    (kbps as u32).clamp(150, 20_000)
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "y" | "نعم" | "تشغيل" => Some(true),
        "0" | "false" | "no" | "off" | "n" | "لا" | "إيقاف" => Some(false),
        _ => None,
    }
}

impl Settings {
    /// Load a profile's defaults. `Custom` keeps the current values.
    pub fn set_profile(&mut self, p: Profile) {
        let (fps, res, kbps, preset) = match p {
            Profile::UltraLight => (10, Resolution::Height(240), Some(250), "ultrafast"),
            Profile::Low => (15, Resolution::Height(480), Some(600), "ultrafast"),
            Profile::Balanced => (30, Resolution::Height(720), Some(2000), "superfast"),
            Profile::High => (30, Resolution::Height(1080), Some(6000), "veryfast"),
            Profile::Custom => {
                self.profile = Profile::Custom;
                return;
            }
        };
        self.profile = p;
        self.fps = fps;
        self.resolution = res;
        self.bitrate_kbps = kbps;
        self.preset = Some(preset.to_string());
    }

    pub fn effective_codec(&self) -> Codec {
        self.codec.unwrap_or(match self.container {
            Container::WebM => Codec::Vp9,
            _ => Codec::H264,
        })
    }

    pub fn x264_preset(&self) -> String {
        self.preset.clone().unwrap_or_else(|| "superfast".to_string())
    }

    pub fn effective_bitrate(&self, canvas: Size) -> u32 {
        self.bitrate_kbps.unwrap_or_else(|| suggest_bitrate(canvas, self.fps))
    }

    pub fn audio_kbps(&self) -> u32 {
        let n = (self.system_audio as u32) + (self.mic as u32);
        if n == 0 {
            0
        } else if self.separate_tracks {
            n * 128
        } else {
            128
        }
    }

    /// Rough size estimate in MB per minute (video + audio target rates; actual files vary).
    pub fn estimate_mb_per_minute(&self, canvas: Size) -> f64 {
        (self.effective_bitrate(canvas) + self.audio_kbps()) as f64 * 60.0 / 8.0 / 1024.0
    }

    /// Apply one `key = value` pair (from the CLI or the config file).
    /// Keys are case-insensitive and `_` equals `-`. Explicit quality overrides
    /// turn the profile into `Custom` so the status never lies about what is used.
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), String> {
        let k = key.trim().to_lowercase().replace('_', "-");
        let v = value.trim();
        let flag = |v: &str| parse_bool(v).ok_or_else(|| tr!("قيمة منطقية غير صالحة للخيار {}: {} (استخدم on أو off)", "invalid on/off value for {}: {} (use on or off)", key, v));
        let num = |v: &str| v.parse::<u32>().map_err(|_| tr!("قيمة رقمية غير صالحة للخيار {}: {}", "invalid number for {}: {}", key, v));
        match k.as_str() {
            "profile" => {
                let p = Profile::parse(v).ok_or_else(|| tr!("ملف تسجيل غير معروف: {} (المتاح: ultralight low balanced high custom)", "unknown profile: {} (available: ultralight low balanced high custom)", v))?;
                self.set_profile(p);
            }
            "fps" => {
                let f = num(v)?;
                if !(FPS_MIN..=FPS_MAX).contains(&f) {
                    return Err(tr!("معدل الإطارات {} غير مدعوم (المسموح من {} إلى {})", "FPS {} is not supported (allowed {} to {})", f, FPS_MIN, FPS_MAX));
                }
                self.fps = f;
                self.profile = Profile::Custom;
            }
            "res" | "resolution" => {
                self.resolution = Resolution::parse(v)?;
                self.profile = Profile::Custom;
            }
            "bitrate" => {
                if v.eq_ignore_ascii_case("auto") || v == "تلقائي" {
                    self.bitrate_kbps = None;
                } else {
                    let b = num(v.trim_end_matches(['k', 'K']))?;
                    if !(BITRATE_MIN_KBPS..=BITRATE_MAX_KBPS).contains(&b) {
                        return Err(tr!("معدل البت {} كيلوبت/ث خارج المسموح ({} إلى {})", "bitrate {} kbps is out of range ({} to {})", b, BITRATE_MIN_KBPS, BITRATE_MAX_KBPS));
                    }
                    self.bitrate_kbps = Some(b);
                }
                self.profile = Profile::Custom;
            }
            "preset" => {
                if !X264_PRESETS.contains(&v) {
                    return Err(tr!("إعداد السرعة {} غير معروف (المتاح: {})", "unknown speed preset {} (available: {})", v, X264_PRESETS.join(" ")));
                }
                self.preset = Some(v.to_string());
                self.profile = Profile::Custom;
            }
            "format" | "container" => {
                self.container = Container::parse(v).ok_or_else(|| tr!("صيغة غير معروفة: {} (المتاح: mp4 mkv webm)", "unknown format: {} (available: mp4 mkv webm)", v))?;
            }
            "codec" => {
                self.codec = Some(Codec::parse(v).ok_or_else(|| tr!("ترميز غير معروف: {} (المتاح: h264 vp9 av1)", "unknown codec: {} (available: h264 vp9 av1)", v))?);
            }
            "mode" | "resize-mode" => {
                self.resize_mode = match v.to_lowercase().as_str() {
                    "fixed" | "a" | "ثابت" => ResizeMode::Fixed,
                    "adaptive" | "b" | "متكيف" => ResizeMode::Adaptive,
                    _ => return Err(tr!("نمط تغيير الحجم غير معروف: {} (المتاح: fixed adaptive)", "unknown resize mode: {} (available: fixed adaptive)", v)),
                };
            }
            "system-audio" => self.system_audio = flag(v)?,
            "mic" => self.mic = flag(v)?,
            "system-device" => self.system_device = if v.is_empty() { None } else { Some(v.to_string()) },
            "mic-device" => self.mic_device = if v.is_empty() { None } else { Some(v.to_string()) },
            "system-volume" => self.system_volume = num(v)?.min(300),
            "mic-volume" => self.mic_volume = num(v)?.min(300),
            "separate-tracks" => self.separate_tracks = flag(v)?,
            "mouse" => self.draw_mouse = flag(v)?,
            "countdown" => self.countdown = num(v)?.min(60),
            "out" | "output-dir" => self.output_dir = PathBuf::from(v),
            "name" => self.name = if v.is_empty() { None } else { Some(v.to_string()) },
            "adaptive-resources" => self.adaptive_resources = flag(v)?,
            _ => return Err(tr!("خيار غير معروف: {}", "unknown option: {}", key)),
        }
        Ok(())
    }

    /// Cross-field checks. Returns every problem found, not just the first.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut e = Vec::new();
        let codec = self.effective_codec();
        if !codec.allowed_in(self.container) {
            e.push(tr!("صيغة {} لا تدعم الترميز {}", "the {} container does not support the {} codec", self.container.ext().to_uppercase(), codec.name()));
        }
        if self.container == Container::WebM && self.effective_codec() == Codec::H264 {
            e.push(tr!("صيغة WebM تتطلب VP9 أو AV1", "WebM requires VP9 or AV1"));
        }
        if self.separate_tracks && !(self.system_audio && self.mic) {
            e.push(tr!("المسارات الصوتية المنفصلة تتطلب تفعيل صوت النظام والميكروفون معًا", "separate audio tracks need both system audio and microphone enabled"));
        }
        if self.output_dir.as_os_str().is_empty() {
            e.push(tr!("مجلد الحفظ غير محدد", "output directory is not set"));
        }
        if let Some(n) = &self.name {
            if n.contains(['/', '\\', '\0']) || n == "." || n == ".." {
                e.push(tr!("اسم الملف لا يجوز أن يحتوي على مسارات", "file name must not contain path separators"));
            }
        }
        if e.is_empty() { Ok(()) } else { Err(e) }
    }
}

// ---- config file ----------------------------------------------------------

pub fn config_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("Miraqab")
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        PathBuf::from(x).join("miraqab")
    } else {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".config").join("miraqab")
    }
}

pub fn parse_config(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Apply pairs; `profile` first so explicit values override it. Returns the problems found.
pub fn apply_pairs(s: &mut Settings, pairs: &[(String, String)]) -> Vec<String> {
    let mut errs = Vec::new();
    let mut ordered: Vec<&(String, String)> = pairs.iter().filter(|(k, _)| k == "profile").collect();
    ordered.extend(pairs.iter().filter(|(k, _)| k != "profile"));
    for (k, v) in ordered {
        if let Err(e) = s.apply(k, v) {
            errs.push(e);
        }
    }
    errs
}

pub fn load_config() -> (Settings, Vec<String>) {
    let mut s = Settings::default();
    let path = config_dir().join("config.conf");
    let mut problems = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for p in apply_pairs(&mut s, &parse_config(&text)) {
            problems.push(format!("{}: {}", path.display(), p));
        }
    }
    (s, problems)
}

/// Persist one key. The value is validated first; the file is replaced atomically.
pub fn save_config_value(key: &str, value: &str) -> Result<PathBuf, String> {
    Settings::default().apply(key, value)?;
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| tr!("تعذّر إنشاء مجلد الإعدادات {}: {}", "cannot create config dir {}: {}", dir.display(), e))?;
    let path = dir.join("config.conf");
    let mut pairs = std::fs::read_to_string(&path).map(|t| parse_config(&t)).unwrap_or_default();
    let k = key.trim().to_lowercase().replace('_', "-");
    pairs.retain(|(pk, _)| *pk != k);
    pairs.push((k, value.trim().to_string()));
    let body: String = pairs.iter().map(|(k, v)| format!("{} = {}\n", k, v)).collect();
    let tmp = dir.join("config.conf.tmp");
    std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| tr!("تعذّر حفظ الإعدادات: {}", "cannot save config: {}", e))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_expected_values() {
        let mut s = Settings::default();
        s.set_profile(Profile::UltraLight);
        assert_eq!((s.fps, s.resolution), (10, Resolution::Height(240)));
        s.set_profile(Profile::Low);
        assert_eq!((s.fps, s.resolution), (15, Resolution::Height(480)));
        s.set_profile(Profile::High);
        assert_eq!(s.resolution, Resolution::Height(1080));
        assert_eq!(Settings::default().profile, Profile::Balanced);
    }

    #[test]
    fn fps_presets_and_limits() {
        let mut s = Settings::default();
        for f in FPS_PRESETS {
            s.apply("fps", &f.to_string()).unwrap();
            assert_eq!(s.fps, f);
        }
        s.apply("fps", "45").unwrap(); // custom value inside limits
        assert!(s.apply("fps", "0").is_err());
        assert!(s.apply("fps", "121").is_err());
        assert!(s.apply("fps", "abc").is_err());
        assert_eq!(s.fps, 45);
    }

    #[test]
    fn explicit_override_turns_profile_into_custom() {
        let mut s = Settings::default();
        s.apply("profile", "low").unwrap();
        assert_eq!(s.profile, Profile::Low);
        s.apply("fps", "20").unwrap();
        assert_eq!(s.profile, Profile::Custom);
    }

    #[test]
    fn resolution_parsing() {
        assert_eq!(Resolution::parse("720p").unwrap(), Resolution::Height(720));
        assert_eq!(Resolution::parse("480").unwrap(), Resolution::Height(480));
        assert_eq!(Resolution::parse("Original").unwrap(), Resolution::Original);
        assert_eq!(Resolution::parse("1280x720").unwrap(), Resolution::Custom(Size::new(1280, 720)));
        assert_eq!(Resolution::parse("1280×720").unwrap(), Resolution::Custom(Size::new(1280, 720)));
        assert!(Resolution::parse("721p").is_err());
        assert!(Resolution::parse("1281x720").is_err()); // odd
        assert!(Resolution::parse("10x10").is_err()); // too small
        assert!(Resolution::parse("banana").is_err());
    }

    #[test]
    fn canvas_for_each_preset() {
        let src = Size::new(1366, 768);
        assert_eq!(Resolution::Height(720).canvas(src), Size::new(1280, 720));
        assert_eq!(Resolution::Height(480).canvas(src), Size::new(854, 480));
        assert_eq!(Resolution::Original.canvas(Size::new(641, 481)), Size::new(640, 480));
        for (h, w) in HEIGHT_PRESETS {
            let c = Resolution::Height(h).canvas(src);
            assert_eq!((c.w % 2, c.h % 2), (0, 0));
            assert_eq!(c, Size::new(w, h));
        }
    }

    #[test]
    fn bitrate_rules() {
        let mut s = Settings::default();
        assert!(s.apply("bitrate", "49").is_err());
        assert!(s.apply("bitrate", "100001").is_err());
        s.apply("bitrate", "3000k").unwrap();
        assert_eq!(s.bitrate_kbps, Some(3000));
        s.apply("bitrate", "auto").unwrap();
        assert_eq!(s.bitrate_kbps, None);
        assert_eq!(suggest_bitrate(Size::new(1280, 720), 30), 1935);
        assert_eq!(suggest_bitrate(Size::new(256, 144), 5), 150); // clamped up
    }

    #[test]
    fn container_codec_combinations() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.apply("format", "webm").unwrap();
        assert_eq!(s.effective_codec(), Codec::Vp9); // sensible default
        assert!(s.validate().is_ok());
        s.apply("codec", "h264").unwrap();
        assert!(s.validate().is_err());
        s.apply("format", "mkv").unwrap();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn separate_tracks_need_both_sources() {
        let mut s = Settings::default();
        s.apply("separate-tracks", "on").unwrap();
        assert!(s.validate().is_err());
        s.apply("system-audio", "on").unwrap();
        s.apply("mic", "on").unwrap();
        assert!(s.validate().is_ok());
        assert_eq!(s.audio_kbps(), 256);
    }

    #[test]
    fn invalid_names_and_keys() {
        let mut s = Settings::default();
        s.apply("name", "../evil").unwrap();
        assert!(s.validate().is_err());
        assert!(s.apply("nonsense", "1").is_err());
        assert!(s.apply("mic", "maybe").is_err());
        assert!(s.apply("preset", "warp9").is_err());
    }

    #[test]
    fn config_roundtrip_and_profile_first() {
        let pairs = parse_config("# c\nfps = 20\nprofile = low\n\nsystem_audio = on\n");
        let mut s = Settings::default();
        assert!(apply_pairs(&mut s, &pairs).is_empty());
        assert_eq!(s.fps, 20); // explicit fps wins over profile regardless of order
        assert_eq!(s.resolution, Resolution::Height(480));
        assert!(s.system_audio);
        assert_eq!(s.profile, Profile::Custom);
    }

    #[test]
    fn size_estimate_is_positive_and_scales() {
        let mut s = Settings::default();
        let c = Size::new(1280, 720);
        let a = s.estimate_mb_per_minute(c);
        s.system_audio = true;
        assert!(s.estimate_mb_per_minute(c) > a);
    }
}
