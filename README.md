# مِرقاب · Miraqab

**مسجّل شاشة خفيف بواجهة عربية — Lightweight, Arabic-first screen recorder built on FFmpeg.**

برنامج صغير (≈ 0.8 MB، بلا مكتبات خارجية) مكتوب بـ Rust. يسجّل الشاشة أو نافذة أو منطقة، مع صوت النظام والميكروفون،
ويعمل FFmpeg كعملية خارجية. لوحة التحكم عربية من اليمين إلى اليسار وتُفتح في المتصفح؛ والتسجيل نفسه يجري في مِرقاب/FFmpeg لا في المتصفح.

> English summary below. **Read "What is and is not verified" before trusting any claim.**

## التثبيت · Install

| النظام | الطريقة |
|---|---|
| Debian / Ubuntu / Linux Mint | `sudo apt install ./miraqab_*.deb` (يسحب `ffmpeg` تلقائيًا) |
| أي توزيعة Linux | فُكّ الأرشيف `miraqab-*-linux-*.tar.gz` ثم `./install.sh` (بلا root) وثبّت `ffmpeg` من مدير الحزم |
| Windows 10/11 | `miraqab-windows-x86_64.zip` (يحوي `miraqab.exe` و`ffmpeg.exe`) — **غير مُختبَر على Windows، انظر أدناه** |
| من المصدر | `cargo build --release` (لا حاجة لأي مكتبة؛ Rust ≥ 1.70) |

الملفات تُبنى بواسطة `.github/workflows/release.yml` (لم يُشغَّل بعد) أو بالأمر `scripts/package-linux.sh`.

## الاستخدام · Usage

```
miraqab                      # لوحة التحكم العربية في المتصفح
miraqab doctor               # ما الذي يدعمه جهازك فعلًا
miraqab windows              # النوافذ المتاحة
miraqab record --source window:0x4a00007 --profile low --system-audio --mic
miraqab record --source region:0,0,1280x720 --fps 15 --res 480p --duration 60
miraqab recover              # استرجاع تسجيل انقطع (انقطاع كهرباء، انهيار)
```

- **المصادر:** `desktop`, `monitor:N`, `window:ID`, `title:نص`, `region:X,Y,WxH`.
- **الأنماط الجاهزة:** Ultra Light (10fps/240p) · Low (15/480p) · Balanced (30/720p) · High (30/1080p) · Custom.
- **FPS:** 5/10/15/20/24/30/60 أو أي قيمة 1–120. **الدقة:** 144p…1080p، الأصلية، أو `WxH`.
- **تغيّر حجم النافذة:** `--mode fixed` (الافتراضي: حجم فيديو ثابت مع أشرطة سوداء بلا تمطيط) أو `--mode adaptive` (يتبع حجم النافذة؛ تُحفظ المقاطع وتُدمج بأكبر حجم).
- **الصوت:** لا يدّعي مِرقاب وجود صوت إلا بعد قياس عيّنات حقيقية في الملف النهائي؛ المسار الصامت يُبلَّغ عنه.
- **الحفظ:** `~/Videos/Miraqab`؛ لا يُكتب فوق ملف موجود أبدًا؛ التسجيل المعطوب يُسمّى `-partial`.
- **مدير الموارد التكيّفي (اختياري):** `--adaptive-resources` يخفّض الإعدادات عند ثقل الحمل ويُعلمك بكل تغيير.

## ما الذي تحقّقنا منه وما لم نتحقق · What is and is not verified

**Verified here (Linux, Xvfb + real FFmpeg 6.1, 75 automated tests):** region/monitor/window capture, fixed and adaptive
resize modes, window shrink/close survival, pause/resume, segment join with verification, never-overwrite, crash recovery,
audio verification (with a synthetic test tone), invalid input rejected before recording, panel security (token, Host check),
Arabic RTL panel in Chromium.

**NOT verified — please do not assume it works:**
- **The Windows `.exe`.** No Windows machine or toolchain was available. `src/winapi.rs` was only type-checked on Linux. Real window listing, `gdigrab` capture, console handler and audio devices on Windows are untested. See `docs/windows-support.md`.
- **System audio on Windows.** No WASAPI loopback. Only a DirectShow device such as "Stereo Mix" or a virtual cable, if present.
- **Real PulseAudio / PipeWire audio.** Only the parsing and a synthetic test tone were tested.
- **Wayland.** Not supported: a Wayland-only session is refused with an explanation (XWayland windows are warned about).
- **Linux Mint, older hardware (e.g. i3-4005U).** Not tested. Performance numbers in `docs/performance.md` come from a 2-vCPU VM and say so.
- Every GUI feature beyond the browser panel: no tray icon, no global hotkeys, no native window.
- A short gap (a fraction of a second) can occur at each segment boundary (resize in fixed mode, pause, auto-adaptation).

## Docs
`docs/architecture.md` · `docs/recording-pipeline.md` · `docs/linux-support.md` · `docs/windows-support.md` ·
`docs/audio-capture.md` · `docs/performance.md` · `docs/arabic-interface.md` · `docs/licensing.md` · `CHANGELOG.md` · `CONTRIBUTING.md`

License: MIT (this code). FFmpeg is separate software with its own license — see `docs/licensing.md`.
