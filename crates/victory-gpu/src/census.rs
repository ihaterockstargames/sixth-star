//! Observe-only Gen 2 census: what kind of title memory our copies move
//! (ADR 0002 §4.1–4.2, phase 1).
//!
//! The console declares a kind for the memory a title reserves: cached,
//! write-combined (the CPU writes it but reads it only very slowly, so a
//! title hardly reads it), uncached, or physical pages mapped into views.
//! ADR 0001 D3 allows a contract only from a declaration like this, never
//! from watching a few frames. Before any copy-back is moved to "on demand",
//! this census says how much of what we copy back and refresh lies in each
//! kind, from the title's own run. It changes nothing.
//!
//! Views of physical pages are counted apart, and among them those that lie
//! in a reservation the title asked write-combined: such a reservation's
//! protection is what says the CPU does not read it, views mapped into it
//! included (as foxhound by readmet3xt-lab, GPL-3.0-or-later, reads it for
//! its `VICTORY_WC_NO_BACK`; the idea only, written anew here).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// The console's kind of a title memory range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Cached = 0,
    WriteCombined = 1,
    Uncached = 2,
    /// A view of physical pages (their own kind is set per page; counted
    /// apart), in a reservation not asked write-combined or in none seen.
    PhysicalView = 3,
    /// A view of physical pages in a reservation the title asked
    /// write-combined.
    PhysicalViewWc = 4,
    /// Memory the runtime did not see reserved (another allocator, or before
    /// the census).
    Unknown = 5,
}

const KINDS: usize = 6;
const NAMES: [&str; KINDS] = [
    "cached",
    "write-combined",
    "uncached",
    "physical-page views",
    "physical-page views (write-combined reservation)",
    "unknown",
];

/// What moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Bytes compared to find what shaders wrote, before copying back.
    BackCompared = 0,
    /// Bytes copied back into title memory (they differed).
    BackWritten = 1,
    /// Bytes of host copies brought up to date from title memory.
    Refreshed = 2,
}

const FLOWS: usize = 3;

/// The console's protection bits that say the kind (beside the host's
/// `PAGE_*` bits, which are below 0x200).
const NOCACHE: u32 = 0x200;
const WRITECOMBINE: u32 = 0x400;

pub(crate) fn kind_of_protect(console_protect: u32) -> Kind {
    if console_protect & WRITECOMBINE != 0 {
        Kind::WriteCombined
    } else if console_protect & NOCACHE != 0 {
        Kind::Uncached
    } else {
        Kind::Cached
    }
}

type Map = BTreeMap<usize, (usize, Kind)>;

/// Title ranges by start: (end, kind).
static RANGES: Mutex<Map> = Mutex::new(BTreeMap::new());

/// The reservations alone, as [`RANGES`] holds them before views of
/// physical pages are mapped into them: the kind a view takes from the
/// reservation it lies in. Locked after [`RANGES`] when both are.
static RESERVED: Mutex<Map> = Mutex::new(BTreeMap::new());

static BYTES: [[AtomicU64; KINDS]; FLOWS] = [const { [const { AtomicU64::new(0) }; KINDS] }; FLOWS];

/// The runtime reserved `[at, at + len)` with the console's protection.
pub fn title_memory_kind(at: usize, len: usize, console_protect: u32) {
    let kind = kind_of_protect(console_protect);
    put(&mut RANGES.lock().unwrap_or_else(|e| e.into_inner()), at, len, kind);
    put(&mut RESERVED.lock().unwrap_or_else(|e| e.into_inner()), at, len, kind);
}

/// The runtime mapped a view of physical pages at `[at, at + len)`.
pub(crate) fn physical_view(at: usize, len: usize) {
    put(&mut RANGES.lock().unwrap_or_else(|e| e.into_inner()), at, len, Kind::PhysicalView);
}

/// `[at, at + len)` is `kind` in `r`.
fn put(r: &mut Map, at: usize, len: usize, kind: Kind) {
    if len == 0 {
        return;
    }
    let end = at.saturating_add(len);
    // What it covers is replaced; ranges crossing its ends are cut.
    let crossing: Vec<(usize, usize, Kind)> = r
        .range(..end)
        .rev()
        .take_while(|(_, (e, _))| *e > at)
        .map(|(&s, &(e, k))| (s, e, k))
        .collect();
    for (s, e, k) in crossing {
        r.remove(&s);
        if s < at {
            r.insert(s, (at, k));
        }
        if e > end {
            r.insert(end, (e, k));
        }
    }
    r.insert(at, (end, kind));
}

/// Splits `[at, at + len)` by kind: (kind, bytes).
pub(crate) fn split(at: usize, len: usize) -> Vec<(Kind, u64)> {
    let end = at.saturating_add(len);
    let r = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    let reserved = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    let mut known = 0u64;
    let first = r.range(..=at).next_back().map_or(at, |(&s, _)| s);
    for (&s, &(e, k)) in r.range(first..end) {
        let (lo, hi) = (s.max(at), e.min(end));
        if lo < hi {
            let bytes = (hi - lo) as u64;
            if k == Kind::PhysicalView {
                // Views in write-combined reservations apart.
                let wc = covered(&reserved, lo, hi, Kind::WriteCombined);
                if wc > 0 {
                    out.push((Kind::PhysicalViewWc, wc));
                }
                if bytes > wc {
                    out.push((Kind::PhysicalView, bytes - wc));
                }
            } else {
                out.push((k, bytes));
            }
            known += bytes;
        }
    }
    if (len as u64) > known {
        out.push((Kind::Unknown, len as u64 - known));
    }
    out
}

/// The bytes of `[lo, hi)` that `r` holds as `kind`.
fn covered(r: &Map, lo: usize, hi: usize, kind: Kind) -> u64 {
    let first = r.range(..=lo).next_back().map_or(lo, |(&s, _)| s);
    r.range(first..hi)
        .filter(|(_, (_, k))| *k == kind)
        .map(|(&s, &(e, _))| e.min(hi).saturating_sub(s.max(lo)) as u64)
        .sum()
}

/// `len` bytes at `at` moved as `flow`.
pub(crate) fn note(flow: Flow, at: usize, len: usize) {
    for (k, b) in split(at, len) {
        BYTES[flow as usize][k as usize].fetch_add(b, Ordering::Relaxed);
    }
}

/// `bytes` of `[at, at + len)` moved as `flow`, shared out by the kinds of
/// the range in proportion (for counts known only per range).
pub(crate) fn note_part(flow: Flow, at: usize, len: usize, bytes: usize) {
    if len == 0 || bytes == 0 {
        return;
    }
    for (k, b) in split(at, len) {
        let share = (u128::from(b) * bytes as u128 / len as u128) as u64;
        BYTES[flow as usize][k as usize].fetch_add(share, Ordering::Relaxed);
    }
}

/// The frame line's census line, the counters reset.
pub(crate) fn line(frames: f64) -> Option<String> {
    let take = |f: Flow| -> Vec<f64> {
        BYTES[f as usize].iter().map(|a| a.swap(0, Ordering::Relaxed) as f64 / 1e6 / frames).collect()
    };
    let (compared, written, refreshed) = (take(Flow::BackCompared), take(Flow::BackWritten), take(Flow::Refreshed));
    if compared.iter().chain(&written).chain(&refreshed).all(|&v| v == 0.0) {
        return None;
    }
    let show = |v: &[f64]| {
        v.iter()
            .zip(NAMES)
            .filter(|(b, _)| **b > 0.0)
            .map(|(b, n)| format!("{n} {b:.1}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(format!(
        "Gen 2 census, MB per frame by the console's memory kind: copy-back compared [{}], copied back [{}]; host copies \
         refreshed [{}]",
        show(&compared),
        show(&written),
        show(&refreshed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Addresses far from anything a real run reserves, one block per test.
    const BASE: usize = 0x7a00_0000_0000;

    #[test]
    fn kinds_come_from_the_console_protection() {
        assert_eq!(kind_of_protect(0x04 | 0x400 | 0x2000), Kind::WriteCombined);
        assert_eq!(kind_of_protect(0x04 | 0x200), Kind::Uncached);
        assert_eq!(kind_of_protect(0x04), Kind::Cached);
    }

    #[test]
    fn ranges_split_by_kind_and_later_reservations_replace() {
        let b = BASE;
        title_memory_kind(b, 0x3000, 0x404);
        title_memory_kind(b + 0x1000, 0x1000, 0x04);
        physical_view(b + 0x5000, 0x1000);
        assert_eq!(
            split(b, 0x6000),
            [
                (Kind::WriteCombined, 0x1000),
                (Kind::Cached, 0x1000),
                (Kind::WriteCombined, 0x1000),
                (Kind::PhysicalView, 0x1000),
                (Kind::Unknown, 0x2000),
            ]
        );
    }

    /// A view of physical pages counts as one in a write-combined
    /// reservation for the part of it that lies in one, and as a plain view
    /// for the rest; what is around it keeps its reservation's kind.
    #[test]
    fn views_in_write_combined_reservations_are_counted_apart() {
        let b = BASE + 0x20_0000;
        title_memory_kind(b, 0x4000, 0x2404);
        title_memory_kind(b + 0x4000, 0x2000, 0x04);
        physical_view(b + 0x1000, 0x4000);
        assert_eq!(
            split(b, 0x7000),
            [
                (Kind::WriteCombined, 0x1000),
                (Kind::PhysicalViewWc, 0x3000),
                (Kind::PhysicalView, 0x1000),
                (Kind::Cached, 0x1000),
                (Kind::Unknown, 0x1000),
            ]
        );
        // A new reservation over a view gives its range its own kind again.
        title_memory_kind(b + 0x1000, 0x1000, 0x04);
        assert_eq!(split(b + 0x1000, 0x2000), [(Kind::Cached, 0x1000), (Kind::PhysicalViewWc, 0x1000)]);
        assert_eq!(covered(&RESERVED.lock().unwrap(), b, b + 0x4000, Kind::WriteCombined), 0x3000);
    }

    #[test]
    fn notes_add_up_per_kind() {
        let b = BASE + 0x10_0000;
        title_memory_kind(b, 0x1000, 0x404);
        title_memory_kind(b + 0x1000, 0x1000, 0x04);
        // Proportional shares of a count known per range.
        let before = BYTES[Flow::BackWritten as usize][Kind::WriteCombined as usize].load(Ordering::Relaxed);
        note_part(Flow::BackWritten, b, 0x2000, 100);
        let after = BYTES[Flow::BackWritten as usize][Kind::WriteCombined as usize].load(Ordering::Relaxed);
        assert!(after - before >= 50, "half of 100 bytes to the write-combined half");
    }
}
