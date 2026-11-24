//! Audio devices: discovery, validation before recording and level measurement.
//!
//! Linux: PulseAudio-compatible sources (also served by PipeWire through
//! `pipewire-pulse`). System sound is the `.monitor` source of the output device.
//! Windows: DirectShow devices. FFmpeg has no WASAPI loopback input, so system
//! sound there needs a "Stereo Mix"-style device (see docs/windows-support.md).
//!
//! Nothing here ever claims that audio is being recorded; that is only said
//! after [`analyze_file`] has found real, non-silent samples in the result.

use crate::ffmpeg::{AudioInput, Ffmpeg};
use crate::settings::Settings;
use crate::tr;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
    /// Captures what the computer plays (output monitor / loopback / Stereo Mix).
    pub monitor: bool,
    pub default: bool,
}

/// Parse `ffmpeg -sources pulse`.
pub fn parse_pulse_sources(text: &str) -> Vec<AudioDevice> {
    // Only lines after the header are devices; FFmpeg's own error text must never be mistaken for one.
    let Some(start) = text.lines().position(|l| l.starts_with("Auto-detected sources")) else { return Vec::new() };
    text.lines()
        .skip(start + 1)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let default = l.trim_start().starts_with('*');
            let body = l.trim_start().trim_start_matches('*').trim();
            let (id, rest) = body.split_once(' ').unwrap_or((body, ""));
            if id.is_empty() {
                return None;
            }
            let label = rest.trim().trim_start_matches('[').trim_end_matches(']').to_string();
            Some(AudioDevice { monitor: id.ends_with(".monitor"), id: id.to_string(), label: if label.is_empty() { id.to_string() } else { label }, default })
        })
        .collect()
}

/// Parse `ffmpeg -list_devices true -f dshow -i dummy` (both the old sectioned and the new `(audio)` formats).
pub fn parse_dshow_audio(stderr: &str) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    let mut in_audio_section = false;
    for l in stderr.lines() {
        let body = l.split_once(']').map(|x| x.1).unwrap_or(l).trim();
        if body.contains("DirectShow audio devices") {
            in_audio_section = true;
            continue;
        }
        if body.contains("DirectShow video devices") {
            in_audio_section = false;
            continue;
        }
        if body.starts_with("Alternative name") || !body.starts_with('"') {
            continue;
        }
        let Some(end) = body[1..].find('"') else { continue };
        let name = &body[1..1 + end];
        let tail = &body[end + 2..];
        if tail.contains("(audio)") || (in_audio_section && !tail.contains("(video)")) {
            let low = name.to_lowercase();
            let monitor = low.contains("stereo mix") || low.contains("what u hear") || low.contains("virtual-audio-capturer") || low.contains("loopback") || low.contains("مزيج ستيريو");
            out.push(AudioDevice { id: name.to_string(), label: name.to_string(), monitor, default: false });
        }
    }
    out
}

pub fn list_devices(ff: &Ffmpeg) -> Result<Vec<AudioDevice>, String> {
    if cfg!(windows) {
        let out = ff.command().args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"]).output().map_err(|e| e.to_string())?;
        let v = parse_dshow_audio(&String::from_utf8_lossy(&out.stderr));
        return Ok(v);
    }
    let out = ff.command().args(["-hide_banner", "-sources", "pulse"]).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v = parse_pulse_sources(&text);
    if v.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(tr!(
            "لا يوجد خادم صوت متاح (PulseAudio أو PipeWire-pulse). {}",
            "no sound server is available (PulseAudio or PipeWire-pulse). {}",
            err.lines().last().unwrap_or("").trim()
        ));
    }
    Ok(v)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub mean_db: f64,
    pub max_db: f64,
}

impl Level {
    /// -91 dB is where 16-bit digital silence lands in `volumedetect`.
    pub fn is_silent(&self) -> bool {
        self.max_db <= -80.0
    }
}

pub fn parse_volumedetect(stderr: &str) -> Option<Level> {
    let grab = |key: &str| -> Option<f64> {
        let l = stderr.lines().find(|l| l.contains(key))?;
        l.split(key).nth(1)?.trim().trim_end_matches("dB").trim().parse().ok()
    };
    Some(Level { mean_db: grab("mean_volume:")?, max_db: grab("max_volume:")? })
}

/// Measure the level of an input for `secs` seconds without recording anything.
pub fn measure_input(ff: &Ffmpeg, input: &AudioInput, secs: f64) -> Result<Option<Level>, String> {
    let mut cmd = ff.command();
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "info"]).args(input.ffmpeg_input());
    cmd.args(["-t", &format!("{:.2}", secs), "-af", "volumedetect", "-f", "null", "-"]);
    let out = cmd.output().map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ffmpeg failed").trim().to_string());
    }
    Ok(parse_volumedetect(&err))
}

/// Measure the audio actually stored in a recording (all audio streams mixed by FFmpeg's `-map 0:a:0`).
pub fn analyze_file(ff: &Ffmpeg, path: &Path, audio_stream: usize) -> Option<Level> {
    let out = ff
        .command()
        .args(["-hide_banner", "-nostdin", "-loglevel", "info", "-i"])
        .arg(path)
        .args(["-map", &format!("0:a:{}", audio_stream), "-vn", "-af", "volumedetect", "-f", "null", "-"])
        .output()
        .ok()?;
    parse_volumedetect(&String::from_utf8_lossy(&out.stderr))
}

/// Decide which inputs to record and check that each one can really be opened.
/// Returns inputs in the order system, microphone; or every problem found.
pub fn resolve_inputs(ff: &Ffmpeg, st: &Settings, test_tone: Option<u32>) -> Result<Vec<(AudioInput, u32)>, Vec<String>> {
    if !st.system_audio && !st.mic {
        return Ok(Vec::new());
    }
    if let Some(hz) = test_tone {
        // diagnostics / automated tests only
        let mut v = Vec::new();
        if st.system_audio {
            v.push((AudioInput::TestTone(hz), st.system_volume));
        }
        if st.mic {
            v.push((AudioInput::TestTone(hz * 2), st.mic_volume));
        }
        return Ok(v);
    }
    let devices = list_devices(ff).map_err(|e| vec![e])?;
    let mut inputs = Vec::new();
    let mut errs = Vec::new();

    let make = |id: &str| if cfg!(windows) { AudioInput::Dshow(id.to_string()) } else { AudioInput::Pulse(id.to_string()) };

    if st.system_audio {
        let pick = match &st.system_device {
            Some(d) => devices.iter().find(|x| x.id == *d || x.label == *d).map(|x| x.id.clone()).ok_or_else(|| tr!("جهاز صوت النظام «{}» غير موجود", "system audio device \"{}\" not found", d)),
            None if cfg!(windows) => devices.iter().find(|x| x.monitor).map(|x| x.id.clone()).ok_or_else(|| {
                tr!(
                    "لا يوجد جهاز لالتقاط صوت النظام. على Windows فعّل «Stereo Mix» (خصائص الصوت ← التسجيل) أو ثبّت virtual-audio-capturer. الالتقاط المباشر عبر WASAPI غير منجز بعد.",
                    "no device can capture system sound. On Windows enable \"Stereo Mix\" (Sound settings > Recording) or install virtual-audio-capturer. Direct WASAPI loopback is not implemented yet."
                )
            }),
            None => {
                if devices.iter().any(|x| x.monitor) {
                    Ok("@DEFAULT_MONITOR@".to_string())
                } else {
                    Err(tr!("لا يوجد مصدر «monitor» لالتقاط صوت النظام في خادم الصوت", "the sound server exposes no monitor source to capture system sound"))
                }
            }
        };
        match pick {
            Ok(id) => inputs.push((make(&id), st.system_volume)),
            Err(e) => errs.push(e),
        }
    }
    if st.mic {
        let pick = match &st.mic_device {
            Some(d) => devices.iter().find(|x| !x.monitor && (x.id == *d || x.label == *d)).map(|x| x.id.clone()).ok_or_else(|| tr!("الميكروفون «{}» غير موجود", "microphone \"{}\" not found", d)),
            None => devices
                .iter()
                .find(|x| !x.monitor && x.default)
                .or_else(|| devices.iter().find(|x| !x.monitor))
                .map(|x| if cfg!(windows) { x.id.clone() } else { "@DEFAULT_SOURCE@".to_string() })
                .ok_or_else(|| tr!("لا يوجد ميكروفون متصل", "no microphone is connected")),
        };
        match pick {
            Ok(id) => inputs.push((make(&id), st.mic_volume)),
            Err(e) => errs.push(e),
        }
    }
    // Prove each input opens. This costs ~0.3 s per input and avoids a recording that
    // starts and then silently lacks audio.
    for (inp, _) in &inputs {
        if let Err(e) = measure_input(ff, inp, 0.2) {
            errs.push(tr!("تعذّر فتح مصدر الصوت: {}", "cannot open the audio source: {}", e));
        }
    }
    if errs.is_empty() { Ok(inputs) } else { Err(errs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_listing() {
        let t = "Auto-detected sources for pulse:\n* alsa_output.pci-0000_00_1f.3.analog-stereo.monitor [Monitor of Built-in Audio Analog Stereo]\n  alsa_input.pci-0000_00_1f.3.analog-stereo [Built-in Audio Analog Stereo]\n";
        let d = parse_pulse_sources(t);
        assert_eq!(d.len(), 2);
        assert!(d[0].monitor && d[0].default);
        assert!(!d[1].monitor && !d[1].default);
        assert_eq!(d[1].label, "Built-in Audio Analog Stereo");
        assert!(parse_pulse_sources("").is_empty());
        // regression: with no sound server FFmpeg prints an error on stdout; it is not a device
        assert!(parse_pulse_sources("Cannot list sources: Generic error in an external library\n").is_empty());
    }

    #[test]
    fn dshow_listing_new_and_old_formats() {
        let new = "[dshow @ 0000] \"Integrated Camera\" (video)\n[dshow @ 0000]   Alternative name \"@device_pnp\"\n[dshow @ 0000] \"Microphone (Realtek Audio)\" (audio)\n[dshow @ 0000]   Alternative name \"@device_cm_{1}\"\n[dshow @ 0000] \"Stereo Mix (Realtek Audio)\" (audio)\n";
        let d = parse_dshow_audio(new);
        assert_eq!(d.len(), 2);
        assert!(!d[0].monitor && d[1].monitor);
        let old = "[dshow @ 0] DirectShow video devices\n[dshow @ 0]  \"Cam\"\n[dshow @ 0] DirectShow audio devices\n[dshow @ 0]  \"Mic\"\n[dshow @ 0]  \"virtual-audio-capturer\"\n";
        let o = parse_dshow_audio(old);
        assert_eq!(o.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["Mic", "virtual-audio-capturer"]);
        assert!(o[1].monitor);
    }

    #[test]
    fn volumedetect_parsing_and_silence() {
        let t = "[Parsed_volumedetect_0 @ 0x1] n_samples: 96000\n[Parsed_volumedetect_0 @ 0x1] mean_volume: -23.4 dB\n[Parsed_volumedetect_0 @ 0x1] max_volume: -9.0 dB\n";
        let l = parse_volumedetect(t).unwrap();
        assert_eq!((l.mean_db, l.max_db), (-23.4, -9.0));
        assert!(!l.is_silent());
        let s = parse_volumedetect("mean_volume: -91.0 dB\nmax_volume: -91.0 dB\n").unwrap();
        assert!(s.is_silent());
        assert!(parse_volumedetect("no audio here").is_none());
    }
}
