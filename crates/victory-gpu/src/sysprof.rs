//! Video memory and processor use, on the frame line of every build.
//!
//! Two questions the frame line could not answer: is the card's memory
//! full (an 8 GB card under a title made for a larger budget; past nine
//! tenths of the system's budget no more texture copies are made, and the
//! slots that wait for them read nothing), and does the work use the
//! machine's cores, or does one thread set the pace while the others idle?
//! Everything here is a plain query of counters the system keeps; nothing
//! waits.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The thread that prepares and submits (set at every submission).
static SUBMITTER: AtomicU32 = AtomicU32::new(0);

/// The highest use of the card's own memory seen, in bytes (for the session).
pub(crate) static PEAK_VRAM: AtomicU64 = AtomicU64::new(0);

/// A submission starts on the calling thread.
pub(crate) fn submitting_here() {
    let id = crate::host::thread_id();
    if SUBMITTER.load(Ordering::Relaxed) != id {
        SUBMITTER.store(id, Ordering::Relaxed);
    }
}

/// The last reading: wall time, process CPU time, the submitter's CPU time
/// (all 100 ns units), which submitter, and the texture passes refused for
/// room so far.
#[derive(Default, Clone, Copy)]
struct Reading {
    wall: u64,
    process: u64,
    thread: u64,
    submitter: u32,
    refused: u64,
}

static LAST: Mutex<Option<Reading>> = Mutex::new(None);

fn filetime(f: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

fn zero() -> windows_sys::Win32::Foundation::FILETIME {
    windows_sys::Win32::Foundation::FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }
}

/// CPU time (kernel + user, 100 ns units) of this process.
fn process_time() -> Option<u64> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut a, mut b, mut k, mut u) = (zero(), zero(), zero(), zero());
    // SAFETY: plain query into our own values.
    (unsafe { GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u) } != 0).then(|| filetime(k) + filetime(u))
}

/// CPU time of thread `id` (100 ns units).
fn thread_time(id: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
    if id == 0 {
        return None;
    }
    // SAFETY: a handle we open and close; plain query into our values.
    unsafe {
        let h = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, id);
        if h.is_null() {
            return None;
        }
        let (mut a, mut b, mut k, mut u) = (zero(), zero(), zero(), zero());
        let ok = GetThreadTimes(h, &mut a, &mut b, &mut k, &mut u) != 0;
        CloseHandle(h);
        ok.then(|| filetime(k) + filetime(u))
    }
}

fn wall_now() -> u64 {
    let mut f = zero();
    // SAFETY: fills our value.
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut f) };
    filetime(f)
}

/// Cores kept busy on average over `wall` (100 ns units) of `cpu` time, and
/// that as a share of all logical cores.
pub(crate) fn cores(cpu: u64, wall: u64, logical: usize) -> (f64, f64) {
    if wall == 0 {
        return (0.0, 0.0);
    }
    let busy = cpu as f64 / wall as f64;
    (busy, busy / logical.max(1) as f64 * 100.0)
}

fn gib(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

/// The frame line's line for the time since the last call.
pub(crate) fn line() -> Option<String> {
    let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
    let now = Reading {
        wall: wall_now(),
        process: process_time()?,
        submitter: SUBMITTER.load(Ordering::Relaxed),
        thread: thread_time(SUBMITTER.load(Ordering::Relaxed)).unwrap_or(0),
        refused: crate::draw::HEAP_TEXTURES_FULL.load(Ordering::Relaxed),
    };
    let before = LAST.lock().unwrap_or_else(|e| e.into_inner()).replace(now);
    let mut text = String::new();
    if let Some([budget, used, shared_budget, shared]) = crate::host::host().and_then(|h| h.video_memory()) {
        PEAK_VRAM.fetch_max(used, Ordering::Relaxed);
        text.push_str(&format!(
            "VRAM {:.2} of {:.2} GiB budget ({:.0}%), shared GPU memory {:.2} of {:.2} GiB",
            gib(used),
            gib(budget),
            used as f64 / budget.max(1) as f64 * 100.0,
            gib(shared),
            gib(shared_budget)
        ));
    }
    let Some(b) = before else {
        return (!text.is_empty()).then_some(text);
    };
    let wall = now.wall.saturating_sub(b.wall);
    let (process_cores, process_share) = cores(now.process.saturating_sub(b.process), wall, logical);
    text.push_str(&format!(
        "; texture passes that found the card past its target {} (see residency:); the process kept {process_cores:.2} of {logical} logical cores busy \
         ({process_share:.0}%)",
        now.refused.saturating_sub(b.refused)
    ));
    if now.submitter != 0 && now.submitter == b.submitter {
        let (thread, _) = cores(now.thread.saturating_sub(b.thread), wall, logical);
        text.push_str(&format!(
            ", the submitting thread ({}) was busy {:.0}% of the time (the rest it waited)",
            now.submitter,
            (thread * 100.0).min(100.0)
        ));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cores_are_cpu_time_over_wall_time() {
        assert_eq!(cores(0, 0, 16), (0.0, 0.0));
        let (c, share) = cores(30_000_000, 10_000_000, 16);
        assert!((c - 3.0).abs() < 1e-12 && (share - 18.75).abs() < 1e-12);
    }

    /// On this machine: a spinning thread's CPU time over the wall time
    /// shows it busy, and the line carries the card's memory and the cores
    /// (other tests submit in parallel, so the line's submitter may be theirs).
    #[test]
    fn the_line_reads_the_machine() {
        if crate::host::host().is_none() {
            return; // no host GPU
        }
        let me = crate::host::thread_id();
        let (cpu0, wall0) = (thread_time(me).expect("own thread time"), wall_now());
        let started = std::time::Instant::now();
        let mut x = 0u64;
        while started.elapsed() < std::time::Duration::from_millis(200) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        let (busy, _) = cores(thread_time(me).unwrap() - cpu0, wall_now() - wall0, 16);
        assert!(busy > 0.5, "a spinning thread is busy: {busy}");
        let _ = line();
        let text = line().expect("a line");
        eprintln!("{text}");
        assert!(text.starts_with("VRAM "), "{text}");
        assert!(text.contains("logical cores busy"), "{text}");
    }
}
