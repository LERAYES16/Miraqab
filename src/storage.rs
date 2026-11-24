//! Output files: names, free space, the crash-recovery manifest and the history log.

use crate::settings::config_dir;
use crate::tr;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
}

/// Civil date/time from Unix seconds (UTC). Howard Hinnant's algorithm.
pub fn utc_from_secs(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    DateTime {
        year: (y + if m <= 2 { 1 } else { 0 }) as i32,
        month: m as u32,
        day: d as u32,
        hour: (rem / 3600) as u32,
        min: ((rem % 3600) / 60) as u32,
        sec: (rem % 60) as u32,
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Local wall-clock time, falling back to UTC if the platform call fails.
pub fn local_now() -> DateTime {
    #[cfg(unix)]
    {
        use std::os::raw::{c_char, c_int, c_long};
        #[repr(C)]
        struct Tm {
            sec: c_int,
            min: c_int,
            hour: c_int,
            mday: c_int,
            mon: c_int,
            year: c_int,
            wday: c_int,
            yday: c_int,
            isdst: c_int,
            gmtoff: c_long,
            zone: *const c_char,
        }
        extern "C" {
            fn localtime_r(t: *const c_long, out: *mut Tm) -> *mut Tm;
        }
        let t = unix_now() as c_long;
        let mut tm = Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0, gmtoff: 0, zone: std::ptr::null() };
        // SAFETY: `tm` is a properly sized, writable `struct tm` (glibc/musl layout) and `t` is valid.
        let ok = unsafe { !localtime_r(&t, &mut tm).is_null() };
        if ok && tm.year > 0 {
            return DateTime { year: tm.year + 1900, month: (tm.mon + 1) as u32, day: tm.mday as u32, hour: tm.hour as u32, min: tm.min as u32, sec: tm.sec as u32 };
        }
    }
    #[cfg(windows)]
    {
        if let Some(d) = crate::winapi::local_time() {
            return d;
        }
    }
    utc_from_secs(unix_now())
}

pub fn default_name(dt: DateTime) -> String {
    format!("Miraqab-{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", dt.year, dt.month, dt.day, dt.hour, dt.min, dt.sec)
}

/// Make a user-supplied name safe on Linux and Windows file systems.
pub fn sanitize_name(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let t = cleaned.trim().trim_matches('.').trim();
    let t: String = t.chars().take(120).collect();
    if t.is_empty() { "Miraqab".to_string() } else { t }
}

/// `dir/base.ext`, or `dir/base-2.ext`, `base-3.ext`… — never an existing path.
pub fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{}.{}", base, ext));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{}-{}.{}", base, n, ext))).find(|p| !p.exists()).unwrap()
}

/// Create the output directory and prove we can write to it *before* recording starts.
pub fn prepare_output_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| tr!("تعذّر إنشاء مجلد الحفظ {}: {}", "cannot create the output folder {}: {}", dir.display(), e))?;
    let probe = dir.join(format!(".miraqab-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"x").map_err(|e| tr!("لا يمكن الكتابة في المجلد {}: {}", "cannot write to {}: {}", dir.display(), e))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Free bytes on the file system holding `dir`, if the platform can tell us.
pub fn free_space_bytes(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("df").arg("-Pk").arg(dir).output().ok()?;
        parse_df(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(windows)]
    {
        crate::winapi::free_space(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        None
    }
}

/// `df -Pk` output -> available bytes.
pub fn parse_df(text: &str) -> Option<u64> {
    let line = text.lines().nth(1)?;
    let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb * 1024)
}

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{} B", b) } else { format!("{:.1} {}", v, U[i]) }
}

pub fn human_duration(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

// ---- crash-recovery manifest -----------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub final_path: PathBuf,
    pub container: String,
    pub mode: String,
    pub started: i64,
    pub pid: u32,
}

impl Manifest {
    pub fn to_text(&self) -> String {
        format!("miraqab-session=1\nfinal={}\ncontainer={}\nmode={}\nstarted={}\npid={}\n", self.final_path.display(), self.container, self.mode, self.started, self.pid)
    }
    pub fn parse(text: &str) -> Option<Manifest> {
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{}=", k))).map(|v| v.to_string());
        get("miraqab-session")?;
        Some(Manifest {
            final_path: PathBuf::from(get("final")?),
            container: get("container")?,
            mode: get("mode").unwrap_or_else(|| "fixed".into()),
            started: get("started")?.parse().ok()?,
            pid: get("pid").and_then(|p| p.parse().ok()).unwrap_or(0),
        })
    }
}

pub const MANIFEST_NAME: &str = "session.txt";

/// Is a process with this id running? (Linux only; elsewhere we cannot tell, so say "unknown" = false.)
pub fn process_alive(pid: u32) -> bool {
    cfg!(target_os = "linux") && pid != 0 && Path::new(&format!("/proc/{}", pid)).exists()
}

/// Leftover session directories (`.miraqab-*-tmp`) of interrupted recordings in `dir`.
pub fn find_interrupted(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with(".miraqab-") && n.ends_with("-tmp")).unwrap_or(false)
                && p.join(MANIFEST_NAME).is_file()
        })
        .filter(|p| {
            let m = std::fs::read_to_string(p.join(MANIFEST_NAME)).ok().and_then(|t| Manifest::parse(&t));
            m.map(|m| !process_alive(m.pid)).unwrap_or(true)
        })
        .collect();
    v.sort();
    v
}

pub fn segment_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("seg") && n.ends_with(".mkv")).unwrap_or(false))
        .collect();
    v.sort();
    v
}

// ---- history -----------------------------------------------------------------

pub fn history_path() -> PathBuf {
    config_dir().join("history.tsv")
}

pub fn history_line(ts: i64, status: &str, path: &Path, secs: f64, bytes: u64) -> String {
    let clean = |s: String| s.replace(['\t', '\n'], " ");
    format!("{}\t{}\t{}\t{:.1}\t{}\n", ts, status, clean(path.display().to_string()), secs, bytes)
}

pub fn append_history(status: &str, path: &Path, secs: f64, bytes: u64) {
    use std::io::Write;
    let p = history_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = f.write_all(history_line(unix_now(), status, path, secs, bytes).as_bytes());
    }
}

pub fn read_history() -> Vec<(i64, String, String, f64, u64)> {
    let text = std::fs::read_to_string(history_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 5 {
                return None;
            }
            Some((f[0].parse().ok()?, f[1].to_string(), f[2].to_string(), f[3].parse().ok()?, f[4].parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miraqab-storage-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn utc_conversion_known_dates() {
        assert_eq!(utc_from_secs(0), DateTime { year: 1970, month: 1, day: 1, hour: 0, min: 0, sec: 0 });
        // 2026-11-11 17:30:00 UTC
        assert_eq!(utc_from_secs(1_794_418_200), DateTime { year: 2026, month: 11, day: 11, hour: 17, min: 30, sec: 0 });
        // leap day
        assert_eq!(utc_from_secs(1_709_164_800), DateTime { year: 2024, month: 2, day: 29, hour: 0, min: 0, sec: 0 });
        assert_eq!(default_name(utc_from_secs(0)), "Miraqab-1970-01-01_00-00-00");
    }

    #[test]
    fn local_time_is_plausible() {
        let t = local_now();
        assert!(t.year >= 2024 && (1..=12).contains(&t.month) && (1..=31).contains(&t.day) && t.hour < 24);
    }

    #[test]
    fn names_are_sanitised() {
        assert_eq!(sanitize_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_name("  .. "), "Miraqab");
        assert_eq!(sanitize_name("تسجيل الدرس"), "تسجيل الدرس");
        assert!(sanitize_name(&"x".repeat(500)).chars().count() <= 120);
    }

    #[test]
    fn never_overwrites_existing_files() {
        let d = tmp("unique");
        let a = unique_path(&d, "rec", "mp4");
        assert_eq!(a, d.join("rec.mp4"));
        std::fs::write(&a, b"1").unwrap();
        let b = unique_path(&d, "rec", "mp4");
        assert_eq!(b, d.join("rec-2.mp4"));
        std::fs::write(&b, b"2").unwrap();
        assert_eq!(unique_path(&d, "rec", "mp4"), d.join("rec-3.mp4"));
        assert_eq!(std::fs::read(&a).unwrap(), b"1");
    }

    #[test]
    fn output_dir_validation() {
        let d = tmp("outdir");
        assert!(prepare_output_dir(&d.join("a/b/c")).is_ok());
        // a path under a regular file can never be created
        let f = d.join("file");
        std::fs::write(&f, b"x").unwrap();
        assert!(prepare_output_dir(&f.join("sub")).is_err());
    }

    #[test]
    fn df_parsing() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/vda 263174212 12345678 31457280 29% /\n";
        assert_eq!(parse_df(out), Some(31_457_280 * 1024));
        assert_eq!(parse_df("garbage"), None);
        assert!(free_space_bytes(&std::env::temp_dir()).is_some());
    }

    #[test]
    fn humanising() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_duration(3725.9), "01:02:05");
    }

    #[test]
    fn manifest_roundtrip_and_discovery() {
        let d = tmp("manifest");
        let m = Manifest { final_path: d.join("x.mp4"), container: "mp4".into(), mode: "fixed".into(), started: 5, pid: 4_000_000_000 };
        assert_eq!(Manifest::parse(&m.to_text()), Some(m.clone()));
        assert_eq!(Manifest::parse("junk"), None);
        let s = d.join(".miraqab-x-tmp");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join(MANIFEST_NAME), m.to_text()).unwrap();
        std::fs::write(s.join("seg0002.mkv"), b"b").unwrap();
        std::fs::write(s.join("seg0001.mkv"), b"a").unwrap();
        std::fs::create_dir_all(d.join(".miraqab-nomanifest-tmp")).unwrap();
        assert_eq!(find_interrupted(&d), vec![s.clone()]);
        let segs = segment_files(&s);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].ends_with("seg0001.mkv"), "sorted by name");
        // a live process (ourselves) is not "interrupted"
        let live = Manifest { pid: std::process::id(), ..m };
        std::fs::write(s.join(MANIFEST_NAME), live.to_text()).unwrap();
        assert!(find_interrupted(&d).is_empty() || !cfg!(target_os = "linux"));
    }

    #[test]
    fn history_lines_are_one_record_each() {
        let l = history_line(7, "ok", Path::new("/a/b\tc.mp4"), 12.34, 99);
        assert_eq!(l.matches('\n').count(), 1);
        assert_eq!(l.split('\t').count(), 5);
    }
}
