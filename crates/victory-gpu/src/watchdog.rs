//! The freeze watchdog: when nothing has been presented for a while, one
//! report of where things stand, so that "the picture froze" becomes a
//! line that says what was waited for (ADR 0002 §4.10).
//!
//! It only reads. Every lock it takes is a `try_lock`, so a report can never
//! wait behind (or for) a stuck thread, and nothing it does changes when
//! anything else runs. `VICTORY_WATCHDOG_S` sets the seconds without a
//! present before a report (5; 0 turns it off).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// When the last present was (`crate::host::now_us`), and how many so far.
static LAST: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);

fn seconds() -> u64 {
    static S: OnceLock<u64> = OnceLock::new();
    *S.get_or_init(|| std::env::var("VICTORY_WATCHDOG_S").ok().and_then(|v| v.parse().ok()).unwrap_or(5))
}

/// A present happened. Starts the watchdog's thread at the first one.
pub(crate) fn presented() {
    LAST.store(crate::host::now_us(), Ordering::Relaxed);
    PRESENTS.fetch_add(1, Ordering::Relaxed);
    static START: std::sync::Once = std::sync::Once::new();
    if seconds() == 0 {
        return;
    }
    START.call_once(|| {
        if let Err(e) = std::thread::Builder::new().name("victory-watchdog".into()).spawn(watch) {
            tracing::warn!(error = %e, "no thread for the freeze watchdog");
        }
    });
}

/// Whether a stall of `idle_us` since the last present deserves a report,
/// given the stall already reported (`reported`, the present count then).
pub(crate) fn due(idle_us: u64, limit_s: u64, presents: u64, reported: Option<u64>) -> bool {
    idle_us >= limit_s * 1_000_000 && reported != Some(presents)
}

fn watch() {
    let mut reported: Option<u64> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let presents = PRESENTS.load(Ordering::Relaxed);
        let idle = crate::host::now_us().saturating_sub(LAST.load(Ordering::Relaxed));
        if let Some(at) = reported
            && presents != at
        {
            tracing::warn!("watchdog: presents resumed after the stall reported above");
            reported = None;
        }
        if due(idle, seconds(), presents, reported) {
            reported = Some(presents);
            tracing::warn!("watchdog: {}", report(idle));
        }
    }
}

/// What the report says (also for tests): nothing in it waits.
pub(crate) fn report(idle_us: u64) -> String {
    format!(
        "nothing presented for {:.1} s (after {} presents). {}. {}. {}. Pipelines being made: {}.",
        idle_us as f64 / 1e6,
        PRESENTS.load(Ordering::Relaxed),
        crate::host::submission_report(),
        crate::queue::held_report(),
        crate::fence::waits_report().unwrap_or_else(|| "title fence waits: (busy)".into()),
        crate::pipeline::making(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_is_reported_once_until_presents_resume() {
        // 4.9 s: not yet; 5 s: yes; the same stall again: no; after another
        // present a new stall: yes.
        assert!(!due(4_900_000, 5, 10, None));
        assert!(due(5_000_000, 5, 10, None));
        assert!(!due(9_000_000, 5, 10, Some(10)));
        assert!(due(5_000_000, 5, 11, Some(10)));
    }

    /// The report is made while every lock it reads is held elsewhere: it
    /// says "busy" and returns instead of waiting.
    #[test]
    fn the_report_never_waits_for_a_lock() {
        let (tx, rx) = std::sync::mpsc::channel();
        let held = crate::host::hold_report_locks_for_test();
        std::thread::spawn(move || {
            let _ = tx.send(report(6_000_000));
        });
        let text = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the report waited for a lock");
        drop(held);
        assert!(text.contains("nothing presented for 6.0 s"), "{text}");
        assert!(text.contains("busy"), "{text}");
    }
}
