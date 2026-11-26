//! CPU time by kind of work and by thread, in every build (ADR 0002 §4.5).
//!
//! The frame line said how long the submitting thread prepared, waited and
//! copied back; it did not say which work inside "preparing" costs, nor
//! what the title's own threads spend in our code while they record. Each
//! measured stretch of work adds its nanoseconds to its thread's counters:
//! one relaxed add, no lock (a thread takes the registry's lock once, when
//! it is first measured). The frame line sums them per kind and names the
//! busiest threads, so the critical path (the thread whose work the frame
//! waits for) shows.
//!
//! `VICTORY_CPU_PROFILE=0` measures nothing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Kinds of CPU work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Work {
    /// Asking the write watch which pages the title wrote.
    WriteWatch = 0,
    /// Guarding pages (protecting them) and opening them on a write.
    Guards = 1,
    /// Bringing host copies of buffers up to date (what changed, then the
    /// bytes).
    Refresh = 2,
    /// Textures brought up to date: reading, hashing, detiling, uploading.
    Textures = 3,
    /// Hashing or comparing bytes to find what changed (ours, not the
    /// title's).
    Compare = 4,
    /// Making descriptors and the passes over the view heap.
    Descriptors = 5,
    /// Recording the title's draws and dispatches into host lists.
    Recording = 6,
    /// Copying what shaders wrote back into title memory.
    CopyBack = 7,
    /// Letting go of objects no list holds any more, and other upkeep
    /// between submissions.
    Upkeep = 8,
}

const KINDS: usize = 9;

const NAMES: [&str; KINDS] = [
    "write watch",
    "guards",
    "buffer refresh",
    "textures",
    "compares",
    "descriptors",
    "recording",
    "copy-back",
    "upkeep",
];

pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CPU_PROFILE").is_none_or(|v| v != "0"))
}

/// One thread's counters: nanoseconds per kind, and calls per kind.
struct Counters {
    thread: u32,
    name: String,
    ns: [AtomicU64; KINDS],
    calls: [AtomicU64; KINDS],
}

static THREADS: Mutex<Vec<Arc<Counters>>> = Mutex::new(Vec::new());

thread_local! {
    static MINE: Arc<Counters> = {
        let c = Arc::new(Counters {
            thread: thread_id(),
            name: std::thread::current().name().unwrap_or("").to_owned(),
            ns: [const { AtomicU64::new(0) }; KINDS],
            calls: [const { AtomicU64::new(0) }; KINDS],
        });
        THREADS.lock().unwrap_or_else(|e| e.into_inner()).push(c.clone());
        c
    };
}

fn thread_id() -> u32 {
    // SAFETY: a plain query.
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

/// Adds `ns` of `work` to the calling thread.
pub(crate) fn add(work: Work, ns: u64) {
    let _ = MINE.try_with(|c| {
        c.ns[work as usize].fetch_add(ns, Ordering::Relaxed);
        c.calls[work as usize].fetch_add(1, Ordering::Relaxed);
    });
}

/// Measures from now until dropped, as `work`. Nested measurements are
/// exclusive: the time of an inner one is taken out of the outer one, so
/// the kinds add up to the time measured, never more.
pub(crate) struct Timed(Work, Option<std::time::Instant>);

thread_local! {
    /// Per open measurement on this thread, the time its inner ones took.
    static INNER: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for Timed {
    fn drop(&mut self) {
        let Some(at) = self.1 else { return };
        let all = at.elapsed().as_nanos() as u64;
        let inner = INNER.with(|s| {
            let mut s = s.borrow_mut();
            let inner = s.pop().unwrap_or(0);
            if let Some(outer) = s.last_mut() {
                *outer += all;
            }
            inner
        });
        add(self.0, all.saturating_sub(inner));
    }
}

/// `let _t = cpuprof::timed(Work::Guards);` measures the rest of the scope.
#[must_use]
pub(crate) fn timed(work: Work) -> Timed {
    if !on() {
        return Timed(work, None);
    }
    INNER.with(|s| s.borrow_mut().push(0));
    Timed(work, Some(std::time::Instant::now()))
}

/// The totals since the last call, per kind (ms, calls) and per thread
/// (id, name, ms, busiest kind), the counters reset. Threads that measured
/// nothing are left out.
pub(crate) struct Taken {
    pub(crate) by_kind: [(f64, u64); KINDS],
    pub(crate) by_thread: Vec<(u32, String, f64, Work)>,
}

const ALL: [Work; KINDS] = [
    Work::WriteWatch,
    Work::Guards,
    Work::Refresh,
    Work::Textures,
    Work::Compare,
    Work::Descriptors,
    Work::Recording,
    Work::CopyBack,
    Work::Upkeep,
];

pub(crate) fn take() -> Taken {
    let threads = THREADS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut by_kind = [(0.0, 0u64); KINDS];
    let mut by_thread = Vec::new();
    for t in &threads {
        let ns: Vec<u64> = t.ns.iter().map(|a| a.swap(0, Ordering::Relaxed)).collect();
        let calls: Vec<u64> = t.calls.iter().map(|a| a.swap(0, Ordering::Relaxed)).collect();
        let total: u64 = ns.iter().sum();
        if total == 0 {
            continue;
        }
        for k in 0..KINDS {
            by_kind[k].0 += ns[k] as f64 / 1e6;
            by_kind[k].1 += calls[k];
        }
        let busiest = (0..KINDS).max_by_key(|&k| ns[k]).unwrap_or(0);
        by_thread.push((t.thread, t.name.clone(), total as f64 / 1e6, ALL[busiest]));
    }
    by_thread.sort_by(|a, b| b.2.total_cmp(&a.2));
    Taken { by_kind, by_thread }
}

/// The frame line's CPU line for `frames` frames.
pub(crate) fn line(frames: f64) -> Option<String> {
    if !on() {
        return None;
    }
    let t = take();
    let total: f64 = t.by_kind.iter().map(|k| k.0).sum();
    if total == 0.0 {
        return None;
    }
    let kinds: Vec<String> = t
        .by_kind
        .iter()
        .zip(NAMES)
        .filter(|(k, _)| k.1 > 0)
        .map(|((ms, calls), name)| format!("{name} {:.1} ({:.0} calls)", ms / frames, *calls as f64 / frames))
        .collect();
    let threads: Vec<String> = t
        .by_thread
        .iter()
        .take(4)
        .map(|(id, name, ms, busiest)| {
            let who = if name.is_empty() { format!("thread {id}") } else { format!("{name} ({id})") };
            format!("{who} {:.1}, mostly {}", ms / frames, NAMES[*busiest as usize])
        })
        .collect();
    Some(format!(
        "CPU time per frame in measured work (ms, all threads): {}; the busiest threads: {}",
        kinds.join(", "),
        threads.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests read and reset the same counters: one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Time measured on two threads is summed per kind, named per thread,
    /// and the counters start again after a take.
    #[test]
    fn work_is_summed_by_kind_and_thread() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let worker = std::thread::Builder::new()
            .name("cpuprof-test-worker".into())
            .spawn(|| {
                add(Work::Guards, 3_000_000);
                add(Work::Guards, 1_000_000);
            })
            .unwrap();
        worker.join().unwrap();
        add(Work::Descriptors, 2_000_000);
        let t = take();
        assert!(t.by_kind[Work::Guards as usize].0 >= 4.0);
        assert!(t.by_kind[Work::Guards as usize].1 >= 2);
        assert!(t.by_kind[Work::Descriptors as usize].0 >= 2.0);
        let worker = t.by_thread.iter().find(|x| x.1 == "cpuprof-test-worker").expect("the worker is named");
        assert_eq!(worker.3, Work::Guards);
        assert!((worker.2 - 4.0).abs() < 1e-9);
        let again = take();
        assert!(again.by_thread.iter().all(|x| x.1 != "cpuprof-test-worker"), "reset after a take");
    }

    /// An inner measurement's time is taken out of the outer one's.
    #[test]
    fn nested_measurements_are_exclusive() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let worker = std::thread::spawn(|| {
            let outer = timed(Work::Refresh);
            {
                let _inner = timed(Work::WriteWatch);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            drop(outer);
            MINE.with(|c| {
                [Work::Refresh, Work::WriteWatch].map(|w| c.ns[w as usize].load(Ordering::Relaxed) as f64 / 1e6)
            })
        });
        let [refresh, watch] = worker.join().unwrap();
        assert!(watch >= 20.0, "inner {watch}");
        assert!((5.0..20.0).contains(&refresh), "outer without the inner: {refresh}");
    }

    /// A measurement costs well under a microsecond: 100,000 nested pairs
    /// in under 100 ms even in a test build on a busy machine.
    #[test]
    fn measuring_is_cheap() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let started = std::time::Instant::now();
        for _ in 0..100_000 {
            let _a = timed(Work::Recording);
            let _b = timed(Work::Descriptors);
        }
        let ms = started.elapsed().as_secs_f64() * 1e3;
        eprintln!("200,000 measurements: {ms:.1} ms");
        assert!(ms < 100.0, "{ms} ms");
        take();
    }

    #[test]
    fn a_timed_scope_counts_once() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        {
            let _t = timed(Work::Upkeep);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let t = take();
        assert!(t.by_kind[Work::Upkeep as usize].0 >= 2.0);
        assert!(t.by_kind[Work::Upkeep as usize].1 >= 1);
    }
}
