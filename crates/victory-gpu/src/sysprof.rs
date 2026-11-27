//! Video memory and processor use, on the frame line of every build.
//!
//! Two questions the frame line could not answer: is the card's memory
//! full (an 8 GB card under a title made for a larger budget; past nine
//! tenths of the system's budget no more texture copies are made, and the
//! slots that wait for them read nothing), and does the work use the
//! machine's cores, or does one thread set the pace while the others idle?
//! Everything here is a plain query of counters the system keeps; nothing
//! waits.
//!
//! The PC's memory too (since 2026-11-27): a run whose process kept only
//! about one core busy while every part of the frame grew several times
//! longer, with freezes of over ten seconds, is what a system that pages
//! looks like; how much is free, the system's commit against its limit,
//! and the process's own commit, working set and page faults tell it.

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
    /// The process's page faults so far (soft and hard).
    faults: u32,
}

/// The PC's memory now, in bytes: total, free, the system's commit and its
/// limit; and the process's commit (private bytes), working set and page
/// faults so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ram {
    pub total: u64,
    pub free: u64,
    pub commit: u64,
    pub commit_limit: u64,
    pub private: u64,
    pub working_set: u64,
    pub faults: u32,
}

/// The lowest free physical memory seen, in bytes (for the session; 0 =
/// not read yet).
pub(crate) static LOWEST_RAM_FREE: AtomicU64 = AtomicU64::new(0);

/// Reads the PC's memory and this process's.
pub(crate) fn ram() -> Option<Ram> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetPerformanceInfo, K32GetProcessMemoryInfo, PERFORMANCE_INFORMATION, PROCESS_MEMORY_COUNTERS,
        PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let mut perf = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: plain queries into our own values, their sizes set.
    let ok = unsafe {
        GlobalMemoryStatusEx(&mut status) != 0
            && K32GetPerformanceInfo(&mut perf, perf.cb) != 0
            && K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut process).cast::<PROCESS_MEMORY_COUNTERS>(), process.cb) != 0
    };
    ok.then(|| Ram {
        total: status.ullTotalPhys,
        free: status.ullAvailPhys,
        commit: perf.CommitTotal as u64 * perf.PageSize as u64,
        commit_limit: perf.CommitLimit as u64 * perf.PageSize as u64,
        private: process.PrivateUsage as u64,
        working_set: process.WorkingSetSize as u64,
        faults: process.PageFaultCount,
    })
}

/// The memory part of the line: `ram` now, and the page faults per second
/// over `seconds` since `faults_before`.
pub(crate) fn ram_text(ram: &Ram, faults_before: Option<u32>, seconds: f64) -> String {
    let mut text = format!(
        "; the PC's memory: {:.1} of {:.1} GiB in use, {:.1} GiB free; the system's commit {:.1} of {:.1} GiB; this process committed {:.2} GiB, {:.2} GiB of it in RAM (working set)",
        gib(ram.total.saturating_sub(ram.free)),
        gib(ram.total),
        gib(ram.free),
        gib(ram.commit),
        gib(ram.commit_limit),
        gib(ram.private),
        gib(ram.working_set)
    );
    if let Some(before) = faults_before.filter(|_| seconds > 0.0) {
        text.push_str(&format!(", {:.0} page faults/s", f64::from(ram.faults.wrapping_sub(before)) / seconds));
    }
    text
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
        faults: 0,
    };
    let memory = ram();
    let now = Reading {
        faults: memory.map_or(0, |m| m.faults),
        ..now
    };
    if let Some(m) = memory {
        let lowest = LOWEST_RAM_FREE.load(Ordering::Relaxed);
        if lowest == 0 || m.free < lowest {
            LOWEST_RAM_FREE.store(m.free.max(1), Ordering::Relaxed);
        }
    }
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
        if let Some(m) = &memory {
            text.push_str(&ram_text(m, None, 0.0));
        }
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
    if let Some(m) = &memory {
        text.push_str(&ram_text(m, Some(b.faults).filter(|&f| f != 0), wall as f64 / 1e7));
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
        assert!(text.contains("the PC's memory: ") && text.contains("working set"), "{text}");
    }

    /// The memory part: what is in use is total less free; faults are per
    /// second over the time given, and left out on the first reading.
    #[test]
    fn the_memory_part_reads_in_gib_and_per_second() {
        const G: u64 = 1 << 30;
        let m = Ram {
            total: 32 * G,
            free: 2 * G,
            commit: 40 * G,
            commit_limit: 45 * G,
            private: 20 * G,
            working_set: 12 * G,
            faults: 1500,
        };
        let text = ram_text(&m, Some(500), 2.0);
        assert!(text.contains("30.0 of 32.0 GiB in use, 2.0 GiB free"), "{text}");
        assert!(text.contains("commit 40.0 of 45.0 GiB"), "{text}");
        assert!(text.contains("commit 40.0 of 45.0 GiB; this process committed 20.00 GiB, 12.00 GiB of it in RAM"), "{text}");
        assert!(text.ends_with(", 500 page faults/s"), "{text}");
        assert!(!ram_text(&m, None, 0.0).contains("faults"));
        // The fault counter wraps.
        assert!(ram_text(&Ram { faults: 5, ..m }, Some(u32::MAX - 4), 1.0).ends_with(", 10 page faults/s"));
        let live = ram().expect("the PC's memory reads");
        assert!(live.total > live.free && live.total > 0 && live.working_set > 0, "{live:?}");
    }
}
