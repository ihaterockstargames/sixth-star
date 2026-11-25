//! The title address range shaders can reach through the raw-address
//! directory (`victory_shader::emit::SLOT_MEMORY`): pages at or past
//! [`DIRECTORY_END`] read zeros and drop stores, so every path that makes
//! title memory GPU-visible reports a range that leaves it.
//!
//! Diagnostic only: a range outside is counted and logged, never refused,
//! so the title runs as before while runs show whether it happens.

use std::sync::atomic::{AtomicU64, Ordering};

pub use victory_shader::emit::{DIRECTORY_END, directory_covers};

/// Ranges seen outside the directory since start.
static VIOLATIONS: AtomicU64 = AtomicU64::new(0);
/// How many of them are logged one by one.
const LOGGED: u64 = 32;

/// Notes GPU-visible title memory `[address, address + len)` made by
/// `what`; counts and logs it when the directory does not reach all of it.
/// Returns whether it lies inside.
pub fn note_title_range(address: u64, len: u64, what: &str) -> bool {
    if directory_covers(address, len) {
        return true;
    }
    let n = VIOLATIONS.fetch_add(1, Ordering::Relaxed);
    if n < LOGGED {
        tracing::warn!(
            what,
            address = format_args!("{address:#x}"),
            len = format_args!("{len:#x}"),
            limit = format_args!("{DIRECTORY_END:#x}"),
            "title memory outside the raw-address directory: shaders reading it by address get zeros"
        );
    }
    false
}

/// How many ranges [`note_title_range`] found outside the directory.
pub fn title_range_violations() -> u64 {
    VIOLATIONS.load(Ordering::Relaxed)
}

/// Reservations the capped range could not hold, made in the console's
/// range instead (`victory-hle` xmem).
static CAPPED_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// Counts one reservation that fell back past the directory's end;
/// returns how many there were before.
pub fn note_capped_fallback() -> u64 {
    CAPPED_FALLBACKS.fetch_add(1, Ordering::Relaxed)
}

/// How many reservations fell back past the directory's end.
pub fn capped_fallbacks() -> u64 {
    CAPPED_FALLBACKS.load(Ordering::Relaxed)
}

/// Faults shaders reported past the directory's end (`memory.rs`).
pub(crate) static OUTSIDE_FAULTS: AtomicU64 = AtomicU64::new(0);

/// A line a layer above adds to the periodic report (the memory census
/// of `victory-hle`, which this crate cannot call).
static REPORTER: std::sync::OnceLock<fn() -> String> = std::sync::OnceLock::new();

/// Registers the census line (once).
pub fn set_reporter(report: fn() -> String) {
    let _ = REPORTER.set(report);
}

/// The periodic report's line on title memory by address: G-ADDR and
/// G-FAULT read these numbers.
pub(crate) fn report() -> String {
    let extra = REPORTER.get().map(|r| r()).unwrap_or_default();
    format!(
        "title memory by address: {} ranges past the directory's end, {} reservations past the cap, {} faults past it{}{}",
        title_range_violations(),
        capped_fallbacks(),
        OUTSIDE_FAULTS.load(Ordering::Relaxed),
        if extra.is_empty() { "" } else { "; " },
        extra
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inside: not counted; outside, empty or overflowing: counted.
    #[test]
    fn ranges_outside_the_directory_are_counted() {
        let before = title_range_violations();
        assert!(note_title_range(1 << 32, 1 << 16, "test"));
        assert!(!note_title_range(DIRECTORY_END, 1, "test"));
        assert!(!note_title_range(u64::MAX - 1, 4, "test"));
        assert!(!note_title_range(1 << 33, 0, "test"));
        assert_eq!(title_range_violations() - before, 3);
    }
}
