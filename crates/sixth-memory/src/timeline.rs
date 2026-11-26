//! Who wrote which bytes last, by fence: the replacement for copying back
//! everything after every submission (ADR 0002 §4.1, ADR 0001 D4, D7).
//!
//! The inherited engine waited for every submission and then copied every
//! range its shaders might have written back into title memory (44.7 ms of a
//! 217.9 ms gameplay frame). Here a submission declares what it reads and
//! writes; when its fence is reached its writes become **GPU-authoritative**
//! bytes, and they are fetched only:
//!
//! - **eagerly**, for ranges the CPU cannot be stopped from reading (no
//!   contract says otherwise, or the backend cannot guard them): the
//!   conservative default, which is what the inherited engine does for all;
//! - **on demand**, for ranges the backend guards: the first CPU access
//!   asks [`Timeline::cpu_access`], which says which submission to wait for
//!   (only the ones that touch those bytes, not the whole queue) and which
//!   bytes to fetch first;
//! - **never**, for bytes the GPU writes again before the CPU looks.
//!
//! CPU writes are tracked the same way: what the CPU wrote since the GPU
//! last saw it is forwarded before the next submission that reads it, and
//! only those bytes.
//!
//! Nothing here touches a GPU: the backend acts on what it returns.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::ranges::RangeSet;

/// A submission's place on the queue's timeline (its fence value).
pub type Submission = u64;

/// What a submission must have done before it runs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubmitPlan {
    /// Bytes the CPU wrote that this submission reads (or partly writes):
    /// forward them to the GPU's copy first.
    pub forward: Vec<Range<u64>>,
    /// Earlier submissions still in flight that this one must follow, with
    /// the hazard (read after write, write after write, write after read).
    /// On one in-order queue these are barriers; across queues, waits.
    pub after: Vec<(Submission, Hazard)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hazard {
    ReadAfterWrite,
    WriteAfterWrite,
    WriteAfterRead,
}

/// What a CPU access needs first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CpuPlan {
    /// The newest submission in flight that touches the bytes in a way the
    /// access must wait for (`None`: no wait).
    pub wait_for: Option<Submission>,
    /// GPU-authoritative bytes to read back before the access proceeds.
    pub fetch: Vec<Range<u64>>,
}

/// The timeline of one address space.
#[derive(Default, Debug)]
pub struct Timeline {
    /// In flight: what each submission may write and reads.
    writes: BTreeMap<Submission, RangeSet>,
    reads: BTreeMap<Submission, RangeSet>,
    completed: Submission,
    /// Bytes whose newest value is on the GPU only.
    gpu_newest: RangeSet,
    /// Bytes the CPU wrote that the GPU's copy has not seen.
    cpu_newest: RangeSet,
    /// Bytes the backend can guard against CPU access, so they may be fetched
    /// on demand; everything else is fetched as soon as it is retired.
    guarded: RangeSet,
}

/// What retiring submissions leaves to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Retired {
    /// Bytes to read back now: GPU writes to ranges that are not guarded.
    pub fetch_now: Vec<Range<u64>>,
    /// Bytes left on the GPU until the CPU asks (guarded ranges).
    pub left_on_gpu: u64,
}

impl Timeline {
    pub fn new() -> Timeline {
        Timeline::default()
    }

    /// The backend guards `r` (a CPU access will reach [`Timeline::cpu_access`]
    /// first) from now on, or no longer.
    pub fn set_guarded(&mut self, r: Range<u64>, guarded: bool) {
        if guarded {
            self.guarded.insert(r);
        } else {
            self.guarded.remove(r.clone());
            // Its GPU-only bytes must come back now: nothing will ask.
            // (They are fetched at the next retire.)
        }
    }

    /// The CPU wrote `r` (reported by the write watch, a guard, or a
    /// declared upload).
    pub fn cpu_wrote(&mut self, r: Range<u64>) {
        self.cpu_newest.insert(r);
    }

    /// A submission is about to run, reading `reads` and writing `writes`.
    pub fn submit(&mut self, s: Submission, reads: &[Range<u64>], writes: &[Range<u64>]) -> SubmitPlan {
        let mut plan = SubmitPlan::default();
        // The CPU's newest bytes that the GPU is about to read, or to write
        // in part (a shader that writes some bytes of a range must not lose
        // the CPU's other bytes when the range comes back).
        for r in reads.iter().chain(writes) {
            plan.forward.extend(self.cpu_newest.take(r.clone()));
        }
        plan.forward.sort_by_key(|r| r.start);
        let mut hazards = std::collections::BTreeSet::new();
        for (&earlier, w) in &self.writes {
            if reads.iter().any(|r| w.intersects(r.clone())) {
                hazards.insert((earlier, Hazard::ReadAfterWrite));
            }
            if writes.iter().any(|r| w.intersects(r.clone())) {
                hazards.insert((earlier, Hazard::WriteAfterWrite));
            }
        }
        for (&earlier, rd) in &self.reads {
            if writes.iter().any(|r| rd.intersects(r.clone())) {
                hazards.insert((earlier, Hazard::WriteAfterRead));
            }
        }
        plan.after = hazards.into_iter().collect();
        let (mut w, mut rd) = (RangeSet::new(), RangeSet::new());
        for r in writes {
            w.insert(r.clone());
        }
        for r in reads {
            rd.insert(r.clone());
        }
        self.writes.insert(s, w);
        self.reads.insert(s, rd);
        plan
    }

    /// The fence reached `completed`: the submissions up to it are done.
    pub fn retire(&mut self, completed: Submission) -> Retired {
        self.completed = self.completed.max(completed);
        let done: Vec<Submission> = self.writes.range(..=completed).map(|(&s, _)| s).collect();
        for s in done {
            let w = self.writes.remove(&s).expect("listed");
            self.reads.remove(&s);
            for r in w.iter() {
                // The GPU's bytes are newer than any CPU write made before
                // the submission ran (those were forwarded at submit).
                self.gpu_newest.insert(r);
            }
        }
        let reads_done: Vec<Submission> = self.reads.range(..=completed).map(|(&s, _)| s).collect();
        for s in reads_done {
            self.reads.remove(&s);
        }
        // What nothing guards comes back now.
        let mut out = Retired::default();
        let all: Vec<Range<u64>> = self.gpu_newest.iter().collect();
        for r in all {
            let mut rest = RangeSet::new();
            rest.insert(r.clone());
            for g in self.guarded.overlapping(r.clone()).collect::<Vec<_>>() {
                rest.remove(g.start.max(r.start)..g.end.min(r.end));
            }
            for f in rest.iter() {
                self.gpu_newest.remove(f.clone());
                out.fetch_now.push(f);
            }
        }
        out.fetch_now.sort_by_key(|r| r.start);
        out.left_on_gpu = self.gpu_newest.bytes();
        out
    }

    /// The CPU is about to read (`write` false) or write `r`.
    pub fn cpu_access(&mut self, r: Range<u64>, write: bool) -> CpuPlan {
        let mut plan = CpuPlan::default();
        // In flight: a write to these bytes must land first; for a CPU
        // write, a read of them must also have been made (the GPU must not
        // see the CPU's new bytes early).
        for (&s, w) in &self.writes {
            if w.intersects(r.clone()) {
                plan.wait_for = plan.wait_for.max(Some(s));
            }
        }
        if write {
            for (&s, rd) in &self.reads {
                if rd.intersects(r.clone()) {
                    plan.wait_for = plan.wait_for.max(Some(s));
                }
            }
        }
        // After that wait, the in-flight writes retire into `gpu_newest`;
        // the plan's fetch covers both.
        let mut fetch = RangeSet::new();
        for g in self.gpu_newest.overlapping(r.clone()) {
            fetch.insert(g.start.max(r.start)..g.end.min(r.end));
        }
        if plan.wait_for.is_some() {
            for w in self.writes.values() {
                for x in w.overlapping(r.clone()) {
                    fetch.insert(x.start.max(r.start)..x.end.min(r.end));
                }
            }
        }
        plan.fetch = fetch.iter().collect();
        plan
    }

    /// The backend did what [`Timeline::cpu_access`] asked (waited and
    /// fetched): those bytes are the CPU's again, and a write makes them
    /// the CPU's newest.
    pub fn cpu_accessed(&mut self, r: Range<u64>, write: bool) {
        self.gpu_newest.remove(r.clone());
        if write {
            self.cpu_newest.insert(r);
        }
    }

    /// Bytes on the GPU only, and bytes the CPU wrote that the GPU has not seen.
    pub fn pending_bytes(&self) -> (u64, u64) {
        (self.gpu_newest.bytes(), self.cpu_newest.bytes())
    }

    /// Submissions in flight.
    pub fn in_flight(&self) -> usize {
        self.writes.len()
    }
}

#[cfg(test)]
// Lists of one range are what these tests compare against.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    /// Unguarded memory behaves as the inherited engine: what the GPU wrote
    /// comes back when the submission is done.
    #[test]
    fn unguarded_writes_come_back_at_retire() {
        let mut t = Timeline::new();
        t.submit(1, &[], &[0..100]);
        let r = t.retire(1);
        assert_eq!(r.fetch_now, [0..100]);
        assert_eq!(r.left_on_gpu, 0);
    }

    /// Guarded memory stays on the GPU until the CPU asks, and then only
    /// the bytes asked for come back; bytes the GPU writes again before the
    /// CPU looks are never fetched in between.
    #[test]
    fn guarded_writes_come_back_on_demand_only() {
        let mut t = Timeline::new();
        t.set_guarded(0..1 << 20, true);
        t.submit(1, &[], &[0..1 << 20]);
        assert!(t.retire(1).fetch_now.is_empty());
        t.submit(2, &[], &[0..1 << 20]);
        assert!(t.retire(2).fetch_now.is_empty(), "written twice, fetched never");
        let plan = t.cpu_access(4096..8192, false);
        assert_eq!(plan, CpuPlan { wait_for: None, fetch: vec![4096..8192] });
        t.cpu_accessed(4096..8192, false);
        assert_eq!(t.pending_bytes().0, (1 << 20) - 4096);
        // A second read of the same bytes needs nothing.
        assert_eq!(t.cpu_access(4096..8192, false), CpuPlan::default());
    }

    /// A CPU access waits only for the submission that touches its bytes.
    #[test]
    fn cpu_waits_only_for_submissions_that_touch_its_bytes() {
        let mut t = Timeline::new();
        t.set_guarded(0..1000, true);
        t.submit(5, &[], &[0..100]);
        t.submit(6, &[200..300], &[400..500]);
        t.submit(7, &[], &[600..700]);
        assert_eq!(t.cpu_access(50..60, false).wait_for, Some(5));
        assert_eq!(t.cpu_access(250..260, false).wait_for, None, "a GPU read does not delay a CPU read");
        assert_eq!(t.cpu_access(250..260, true).wait_for, Some(6), "a CPU write waits for the GPU's read");
        assert_eq!(t.cpu_access(800..900, true).wait_for, None, "untouched bytes: no wait");
        let p = t.cpu_access(0..1000, false);
        assert_eq!(p.wait_for, Some(7));
        assert_eq!(p.fetch, [0..100, 400..500, 600..700]);
    }

    /// The CPU's writes go up before the submission that reads them, once,
    /// and only the bytes it reads.
    #[test]
    fn cpu_writes_are_forwarded_once_before_their_reader() {
        let mut t = Timeline::new();
        t.cpu_wrote(0..4096);
        t.cpu_wrote(65536..69632);
        let p = t.submit(1, &[0..1024], &[]);
        assert_eq!(p.forward, [0..1024]);
        let p = t.submit(2, &[0..65536 + 8192], &[]);
        assert_eq!(p.forward, [1024..4096, 65536..69632]);
        assert!(t.submit(3, &[0..1 << 20], &[]).forward.is_empty());
    }

    #[test]
    fn hazards_name_the_earlier_submissions() {
        let mut t = Timeline::new();
        t.submit(1, &[0..10], &[100..200]);
        let p = t.submit(2, &[150..160], &[5..6]);
        assert_eq!(p.after, [(1, Hazard::ReadAfterWrite), (1, Hazard::WriteAfterRead)]);
        let p = t.submit(3, &[], &[100..101]);
        assert_eq!(p.after, [(1, Hazard::WriteAfterWrite)]);
        t.retire(3);
        assert_eq!(t.in_flight(), 0);
        assert!(t.submit(4, &[100..200], &[0..10]).after.is_empty(), "retired: nothing to follow");
    }

    /// A late CPU write to a range a submission in flight reads: the CPU
    /// waits, so the GPU never sees bytes newer than its submission.
    #[test]
    fn a_cpu_write_never_overtakes_a_gpu_read() {
        let mut t = Timeline::new();
        t.cpu_wrote(0..64);
        assert_eq!(t.submit(1, &[0..64], &[]).forward, [0..64]);
        assert_eq!(t.cpu_access(0..64, true).wait_for, Some(1));
        t.retire(1);
        assert_eq!(t.cpu_access(0..64, true), CpuPlan::default());
        t.cpu_accessed(0..64, true);
        assert_eq!(t.submit(2, &[0..64], &[]).forward, [0..64]);
    }

    /// Stopping the guard brings the GPU's bytes back at the next retire.
    #[test]
    fn unguarding_brings_bytes_back() {
        let mut t = Timeline::new();
        t.set_guarded(0..100, true);
        t.submit(1, &[], &[0..100]);
        assert!(t.retire(1).fetch_now.is_empty());
        t.set_guarded(0..50, false);
        assert_eq!(t.retire(1).fetch_now, [0..50]);
        assert_eq!(t.pending_bytes().0, 50);
    }
}
