//! The session record: how long the title has run, how fast, and how
//! smoothly, in one line every minute (ADR 0002 §4.10).
//!
//! A test report needs exact figures for the same machine and settings:
//! the frame rate over the session, its worst second, the longest time
//! between two presents, how long the session lasted, and whether frames
//! did less than the title asked. The line is written every minute, so the
//! last one in a log says how far a session got even when it ended in a
//! crash; a freeze is reported by the watchdog with the seconds since the
//! first present.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// What the session measured so far.
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct Record {
    /// Frames and seconds over the frame line's windows (about one second each).
    pub(crate) frames: f64,
    pub(crate) seconds: f64,
    /// The slowest window's frames per second.
    pub(crate) worst_fps: Option<f64>,
    /// Windows in which draws or dispatches were not run, and all windows.
    pub(crate) incomplete: u64,
    pub(crate) windows: u64,
}

impl Record {
    /// One frame-line window: `frames` presented in `seconds`, with or
    /// without draws dropped.
    pub(crate) fn window(&mut self, frames: f64, seconds: f64, dropped: bool) {
        if seconds <= 0.0 {
            return;
        }
        self.frames += frames;
        self.seconds += seconds;
        let fps = frames / seconds;
        self.worst_fps = Some(self.worst_fps.map_or(fps, |w| w.min(fps)));
        self.windows += 1;
        if dropped {
            self.incomplete += 1;
        }
    }

    pub(crate) fn fps(&self) -> f64 {
        if self.seconds > 0.0 { self.frames / self.seconds } else { 0.0 }
    }
}

static RECORD: Mutex<Record> =
    Mutex::new(Record { frames: 0.0, seconds: 0.0, worst_fps: None, incomplete: 0, windows: 0 });

/// The longest time between two presents so far (µs), when the first
/// present was (`crate::host::now_us`, plus one; 0: none yet), and the last
/// minute the session line was logged at.
static LONGEST_GAP: AtomicU64 = AtomicU64::new(0);
static FIRST: AtomicU64 = AtomicU64::new(0);
static LOGGED: AtomicU64 = AtomicU64::new(0);

/// A present `gap_us` after the one before (0 for the first).
pub(crate) fn presented(now_us: u64, gap_us: u64) {
    if FIRST.compare_exchange(0, now_us + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        tracing::info!("{}", startup_line(process_age_s(), &crate::cpuprof::totals()));
    }
    LONGEST_GAP.fetch_max(gap_us, Ordering::Relaxed);
}

/// Seconds since the process was created (`GetProcessTimes`).
fn process_age_s() -> Option<f64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user, mut now) = (zero, zero, zero, zero, zero);
    // SAFETY: plain queries into our own out values.
    let ok = unsafe {
        let ok = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) != 0;
        GetSystemTimeAsFileTime(&mut now);
        ok
    };
    if !ok {
        return None;
    }
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Some(t(now).saturating_sub(t(created)) as f64 / 1e7)
}

/// The start-up line: time to the first present, and our code's share of
/// it by kind (all threads, so it can exceed the wall time).
pub(crate) fn startup_line(age_s: Option<f64>, totals: &[(&str, f64, u64)]) -> String {
    let parts: Vec<String> = totals
        .iter()
        .filter(|(_, ms, _)| *ms >= 0.05)
        .map(|(name, ms, calls)| format!("{name} {:.2} s ({calls})", ms / 1000.0))
        .collect();
    format!(
        "startup: the first present {} after the process started; our code until then, all threads: {}",
        age_s.map_or_else(|| "(age unknown)".to_owned(), |s| format!("{s:.1} s")),
        if parts.is_empty() { "nothing measured".to_owned() } else { parts.join(", ") }
    )
}

/// Seconds since the first present.
pub(crate) fn running_s(now_us: u64) -> f64 {
    match FIRST.load(Ordering::Relaxed) {
        0 => 0.0,
        first => now_us.saturating_sub(first - 1) as f64 / 1e6,
    }
}

/// A frame-line window ended; every minute the session line is logged.
pub(crate) fn window(frames: f64, seconds: f64, dropped: bool) {
    let record = {
        let mut r = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        r.window(frames, seconds, dropped);
        r.clone()
    };
    let running = running_s(crate::host::now_us());
    let minute = (running / 60.0) as u64;
    if minute > LOGGED.load(Ordering::Relaxed) {
        LOGGED.store(minute, Ordering::Relaxed);
        tracing::info!("{}", line(&record, running, LONGEST_GAP.load(Ordering::Relaxed)));
    }
}

/// The session line.
pub(crate) fn line(r: &Record, running_s: f64, longest_gap_us: u64) -> String {
    let fps = r.fps();
    let res = crate::resolution::get();
    format!(
        "session: {} min {} s since the first present, {:.0} frames, {fps:.2} FPS average ({:.1} ms/frame), \
         worst second {:.2} FPS, longest time between two presents {:.2} s; {} of {} seconds with draws not run; \
         {}, VRAM peak {:.2} GiB, the PC's free memory at its lowest {}; display {}x{}{}; {}",
        (running_s / 60.0) as u64,
        (running_s % 60.0) as u64,
        r.frames,
        if fps > 0.0 { 1000.0 / fps } else { 0.0 },
        r.worst_fps.unwrap_or(0.0),
        longest_gap_us as f64 / 1e6,
        r.incomplete,
        r.windows,
        crate::host::adapter_text(),
        crate::sysprof::PEAK_VRAM.load(Ordering::Relaxed) as f64 / (1u64 << 30) as f64,
        match crate::sysprof::LOWEST_RAM_FREE.load(Ordering::Relaxed) {
            0 => "not read".to_owned(),
            b => format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64),
        },
        res.display.0,
        res.display.1,
        res.output.map_or(String::new(), |(w, h)| format!(", output {w}x{h}, scale {}", res.scale)),
        settings_text()
    )
}

/// The `VICTORY_*` settings in effect, without paths (folders and tables
/// are named only as set) and with a skip list as its size.
pub(crate) fn settings_text() -> String {
    let mut set: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("VICTORY_"))
        .map(|(k, v)| {
            let path = k.ends_with("_DIR") || k.ends_with("_TABLES") || k.ends_with("_PATH") || v.contains(['\\', '/']);
            let shown = if path {
                "set".to_owned()
            } else if k == "VICTORY_SKIP_PIPELINES" {
                format!("{} pipelines", v.split(',').filter(|s| !s.trim().is_empty()).count())
            } else {
                v
            };
            (k, shown)
        })
        .collect();
    set.sort();
    if set.is_empty() {
        return "settings: all defaults".into();
    }
    let parts: Vec<String> = set.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("settings: {}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_is_exact_over_its_windows() {
        let mut r = Record::default();
        r.window(5.0, 1.0, false);
        r.window(4.0, 1.0, true);
        r.window(6.0, 1.5, false);
        assert_eq!(r.frames, 15.0);
        assert_eq!(r.seconds, 3.5);
        assert!((r.fps() - 15.0 / 3.5).abs() < 1e-12);
        assert_eq!(r.worst_fps, Some(4.0));
        assert_eq!((r.incomplete, r.windows), (1, 3));
        // An empty window changes nothing.
        r.window(0.0, 0.0, true);
        assert_eq!(r.windows, 3);
    }

    #[test]
    fn the_session_line_says_what_a_report_needs() {
        let mut r = Record::default();
        for _ in 0..90 {
            r.window(4.6, 1.0, false);
        }
        let line = line(&r, 754.0, 2_400_000);
        assert!(line.starts_with("session: 12 min 34 s since the first present"), "{line}");
        assert!(line.contains("4.60 FPS average (217.4 ms/frame)"), "{line}");
        assert!(line.contains("worst second 4.60 FPS"), "{line}");
        assert!(line.contains("longest time between two presents 2.40 s"), "{line}");
        assert!(line.contains("0 of 90 seconds with draws not run"), "{line}");
        assert!(line.contains("the PC's free memory at its lowest "), "{line}");
    }

    /// On the real device the line names the GPU and its driver version.
    #[test]
    fn the_adapter_is_named_with_its_driver() {
        if crate::host::host().is_none() {
            return; // no host GPU
        }
        let text = crate::host::adapter_text();
        eprintln!("{text}");
        assert!(text.starts_with("GPU ") && text.contains(", driver "), "{text}");
        assert!(!text.contains("GPU unknown"), "{text}");
    }

    #[test]
    fn the_startup_line_names_where_the_time_went() {
        let line = startup_line(Some(93.4), &[("shaders", 41_000.0, 2600), ("pipelines", 22_500.0, 900), ("upkeep", 0.01, 3)]);
        assert_eq!(
            line,
            "startup: the first present 93.4 s after the process started; our code until then, all threads: \
             shaders 41.00 s (2600), pipelines 22.50 s (900)"
        );
        assert!(process_age_s().is_some_and(|s| s > 0.0 && s < 86_400.0));
    }

    #[test]
    fn settings_hide_paths_and_count_skip_lists() {
        // SAFETY: test-only variables that no other test reads.
        unsafe {
            std::env::set_var("VICTORY_SESSION_TEST_DIR", "C:\\somewhere");
            std::env::set_var("VICTORY_SESSION_TEST_MODE", "1");
        }
        let text = settings_text();
        assert!(text.contains("VICTORY_SESSION_TEST_DIR=set"), "{text}");
        assert!(text.contains("VICTORY_SESSION_TEST_MODE=1"), "{text}");
        assert!(!text.contains("somewhere"), "{text}");
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("VICTORY_SESSION_TEST_DIR");
            std::env::remove_var("VICTORY_SESSION_TEST_MODE");
        }
    }
}
