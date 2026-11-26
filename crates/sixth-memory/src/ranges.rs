//! Sets of byte ranges, kept sorted, disjoint and merged.
//!
//! The building block for event-driven tracking (ADR 0002 §4.1): what the
//! CPU wrote since the last forward, what a submission's shaders may write,
//! what is waiting to be read back. Every operation costs in proportion to
//! the ranges it touches, not to the memory they cover, so a frame that
//! changes little costs little; the inherited engine instead asked the
//! write watch about all of its copies at every submission.

use std::collections::BTreeMap;
use std::ops::Range;

/// Disjoint, non-touching half-open ranges, by start.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct RangeSet {
    /// start → end.
    ranges: BTreeMap<u64, u64>,
}

impl RangeSet {
    pub fn new() -> RangeSet {
        RangeSet::default()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Ranges in the set.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Bytes covered.
    pub fn bytes(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = Range<u64>> + '_ {
        self.ranges.iter().map(|(&s, &e)| s..e)
    }

    /// Adds `r`, merging with the ranges it overlaps or touches.
    pub fn insert(&mut self, r: Range<u64>) {
        if r.start >= r.end {
            return;
        }
        let (mut start, mut end) = (r.start, r.end);
        // The range before that reaches `start`, then every one starting
        // inside `[start, end]`.
        if let Some((&s, &e)) = self.ranges.range(..=start).next_back()
            && e >= start
        {
            start = s;
            end = end.max(e);
        }
        let inside: Vec<u64> = self.ranges.range(start..=end).map(|(&s, _)| s).collect();
        for s in inside {
            let e = self.ranges.remove(&s).expect("listed");
            end = end.max(e);
        }
        self.ranges.insert(start, end);
    }

    /// Removes `r` from the set (cutting ranges that cross its ends).
    pub fn remove(&mut self, r: Range<u64>) {
        if r.start >= r.end {
            return;
        }
        for (s, e) in self.overlapping(r.clone()).collect::<Vec<_>>().into_iter().map(|x| (x.start, x.end)) {
            self.ranges.remove(&s);
            if s < r.start {
                self.ranges.insert(s, r.start);
            }
            if e > r.end {
                self.ranges.insert(r.end, e);
            }
        }
    }

    /// The ranges of the set that share a byte with `r`, whole.
    pub fn overlapping(&self, r: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
        let first = self
            .ranges
            .range(..=r.start)
            .next_back()
            .filter(|(_, e)| **e > r.start)
            .map(|(&s, _)| s)
            .unwrap_or(r.start);
        self.ranges
            .range(first..r.end.max(first))
            .map(|(&s, &e)| s..e)
            .filter(move |x| x.start < r.end && x.end > r.start)
    }

    /// Whether any byte of `r` is in the set.
    pub fn intersects(&self, r: Range<u64>) -> bool {
        self.overlapping(r).next().is_some()
    }

    /// Whether every byte of `r` is in the set.
    pub fn covers(&self, r: Range<u64>) -> bool {
        r.start >= r.end
            || self
                .ranges
                .range(..=r.start)
                .next_back()
                .is_some_and(|(_, &e)| e >= r.end)
    }

    /// The parts of `r` in the set, clipped to `r`, and removes them.
    pub fn take(&mut self, r: Range<u64>) -> Vec<Range<u64>> {
        let parts: Vec<Range<u64>> =
            self.overlapping(r.clone()).map(|x| x.start.max(r.start)..x.end.min(r.end)).collect();
        self.remove(r);
        parts
    }

    /// Empties the set, returning what it held.
    pub fn take_all(&mut self) -> Vec<Range<u64>> {
        std::mem::take(&mut self.ranges).into_iter().map(|(s, e)| s..e).collect()
    }
}

#[cfg(test)]
// Lists of one range are what these tests compare against.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn set(rs: &[Range<u64>]) -> RangeSet {
        let mut s = RangeSet::new();
        for r in rs {
            s.insert(r.clone());
        }
        s
    }

    fn list(s: &RangeSet) -> Vec<Range<u64>> {
        s.iter().collect()
    }

    #[test]
    fn inserts_merge_overlaps_and_neighbours() {
        let s = set(&[10..20, 30..40, 20..25, 50..60, 35..55, 0..0]);
        assert_eq!(list(&s), [10..25, 30..60]);
        assert_eq!(s.bytes(), 45);
        let s = set(&[5..10, 0..100]);
        assert_eq!(list(&s), [0..100]);
    }

    #[test]
    fn removal_cuts_and_splits() {
        let mut s = set(&[0..100, 200..300]);
        s.remove(50..250);
        assert_eq!(list(&s), [0..50, 250..300]);
        s.remove(10..20);
        assert_eq!(list(&s), [0..10, 20..50, 250..300]);
        s.remove(0..1000);
        assert!(s.is_empty());
    }

    #[test]
    fn queries_see_only_shared_bytes() {
        let s = set(&[10..20, 30..40]);
        assert!(s.intersects(15..16));
        assert!(!s.intersects(20..30), "touching is not sharing");
        assert!(s.intersects(0..11));
        assert!(s.covers(30..40));
        assert!(!s.covers(15..35));
        assert_eq!(s.overlapping(19..31).collect::<Vec<_>>(), [10..20, 30..40]);
    }

    #[test]
    fn take_clips_and_removes() {
        let mut s = set(&[0..100]);
        assert_eq!(s.take(40..60), [40..60]);
        assert_eq!(list(&s), [0..40, 60..100]);
        assert_eq!(s.take(30..70), [30..40, 60..70]);
        assert_eq!(list(&s), [0..30, 70..100]);
        assert_eq!(s.take_all(), [0..30, 70..100]);
    }

    /// Against a byte map, over many random operations.
    #[test]
    fn matches_a_byte_map() {
        let mut s = RangeSet::new();
        let mut bytes = [false; 512];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..5_000 {
            let a = next(512);
            let b = (a + 1 + next(64)).min(512);
            match next(3) {
                0 | 1 => {
                    s.insert(a..b);
                    bytes[a as usize..b as usize].fill(true);
                }
                _ => {
                    s.remove(a..b);
                    bytes[a as usize..b as usize].fill(false);
                }
            }
            let mut want = RangeSet::new();
            let mut i = 0;
            while i < 512 {
                if bytes[i] {
                    let j = (i..512).find(|&j| !bytes[j]).unwrap_or(512);
                    want.insert(i as u64..j as u64);
                    i = j;
                } else {
                    i += 1;
                }
            }
            assert_eq!(s, want);
        }
    }
}
