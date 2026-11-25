//! D5: CPU→GPU forwarding, coalesced and budgeted.
//!
//! The input is a set of dirty 4 KiB units of one VRAM region (only
//! `CpuOwned` pages are forwarded). The output is a list of runs, each one
//! copy command from a staging ring into the region, packed in the ring
//! back to back.
//!
//! - Adjacent units become one run.
//! - Optionally ([`Planner::with_gap_filling`], off by default) a gap of
//!   clean units between two runs is copied too (one command fewer) when the
//!   cost model says the extra bytes are cheaper than the command, and only
//!   if the caller allows it for every unit in the gap: copying a clean unit
//!   is only correct where the CPU view is the authority for it. Off by
//!   default because it measured slower in every run (`membench --suite
//!   forward`, RTX 3070: 1-unit gaps filled took 3.9–4.0 ms against 2.7 ms
//!   at 4,096 units, and 7.7–15.7 ms against 5.3–5.7 ms at 16,384): for
//!   ordered small copies the GPU's cost per command is below the model's
//!   0.6 µs.
//! - The plan always forwards everything: a consumer must not read stale
//!   bytes. A plan over budget is flagged instead, because a region that
//!   needs more than the budget is a contract problem (it should not be
//!   VRAM-homed), not a tuning problem.

use crate::UNIT;

/// The costs a plan is estimated with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostModel {
    /// Recording plus executing one copy command, µs (measured ≈0.6 µs for
    /// scattered pages, `membench --suite gen2`, RTX 3070; of that, recording
    /// alone is ≈0.28 µs of CPU time, `--suite forward`).
    pub per_command_us: f64,
    /// GPU copy throughput, bytes per µs (measured ≈26 GB/s).
    pub gpu_bytes_per_us: f64,
    /// CPU copy into the staging ring, bytes per µs (measured ≈17 GB/s).
    pub cpu_bytes_per_us: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        CostModel { per_command_us: 0.6, gpu_bytes_per_us: 26_000.0, cpu_bytes_per_us: 17_000.0 }
    }
}

impl CostModel {
    /// The cost of moving `bytes` (CPU pack + GPU copy), µs.
    pub fn bytes_us(&self, bytes: u64) -> f64 {
        bytes as f64 * (1.0 / self.gpu_bytes_per_us + 1.0 / self.cpu_bytes_per_us)
    }

    /// The estimated cost of `commands` copies moving `bytes` in all, µs.
    pub fn estimate_us(&self, commands: usize, bytes: u64) -> f64 {
        commands as f64 * self.per_command_us + self.bytes_us(bytes)
    }

    /// The widest gap, in units, that is cheaper to copy than to skip with
    /// an extra command.
    pub fn profitable_gap_units(&self) -> u64 {
        (self.per_command_us / self.bytes_us(UNIT)).floor() as u64
    }
}

/// One copy: `len` bytes from `staging` in the ring to `dst` in the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Byte offset in the region.
    pub dst: u64,
    /// Byte offset in the staging ring.
    pub staging: u64,
    pub len: u64,
}

/// A forwarding plan for one region.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub runs: Vec<Run>,
    /// Dirty units asked for (after removing repeats).
    pub dirty_units: u64,
    /// Units copied, including filled gaps.
    pub copied_units: u64,
    pub estimate_us: f64,
    pub over_budget: bool,
}

impl Plan {
    /// Bytes copied.
    pub fn bytes(&self) -> u64 {
        self.copied_units * UNIT
    }

    /// Staging ring bytes the plan needs.
    pub fn staging_bytes(&self) -> u64 {
        self.runs.last().map_or(0, |r| r.staging + r.len)
    }

    /// The plan as a telemetry event.
    pub fn event(&self, region: u32) -> sixth_telemetry::Event {
        sixth_telemetry::Event::Forward {
            region,
            runs: self.runs.len() as u32,
            units: self.copied_units,
            bytes: self.bytes(),
            estimate_us: self.estimate_us,
            over_budget: self.over_budget,
        }
    }
}

/// Makes plans.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Planner {
    pub model: CostModel,
    /// The per-frame forwarding budget, µs (D5: 1 ms).
    pub budget_us: f64,
    /// Merge adjacent units into runs. Off only to measure the difference.
    pub coalesce: bool,
    /// Copy profitable clean gaps where allowed (off by default: see the
    /// module notes).
    pub fill_gaps: bool,
}

impl Default for Planner {
    fn default() -> Self {
        Planner { model: CostModel::default(), budget_us: 1000.0, coalesce: true, fill_gaps: false }
    }
}

impl Planner {
    /// One command per dirty unit: the uncoalesced baseline.
    pub fn per_unit() -> Planner {
        Planner { coalesce: false, fill_gaps: false, ..Planner::default() }
    }

    /// Coalescing plus gap filling, for measurements.
    pub fn with_gap_filling() -> Planner {
        Planner { fill_gaps: true, ..Planner::default() }
    }

    /// Plans the copies for the dirty `units` (unit indices in the region,
    /// any order, repeats allowed). `may_fill(u)` says whether clean unit
    /// `u` may be copied as part of a gap.
    pub fn plan(&self, units: &[u64], may_fill: impl Fn(u64) -> bool) -> Plan {
        let mut sorted = units.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let max_gap = if self.coalesce && self.fill_gaps { self.model.profitable_gap_units() } else { 0 };
        // Runs as [first, last] unit ranges.
        let mut spans: Vec<(u64, u64)> = Vec::new();
        for &u in &sorted {
            if let Some(last) = spans.last_mut().filter(|_| self.coalesce) {
                let gap = u - last.1 - 1;
                if gap == 0 || (gap <= max_gap && (last.1 + 1..u).all(&may_fill)) {
                    last.1 = u;
                    continue;
                }
            }
            spans.push((u, u));
        }
        let mut runs = Vec::with_capacity(spans.len());
        let mut staging = 0;
        let mut copied_units = 0;
        for (first, last) in spans {
            let len = (last - first + 1) * UNIT;
            runs.push(Run { dst: first * UNIT, staging, len });
            staging += len;
            copied_units += last - first + 1;
        }
        let estimate_us = self.model.estimate_us(runs.len(), copied_units * UNIT);
        Plan { runs, dirty_units: sorted.len() as u64, copied_units, estimate_us, over_budget: estimate_us > self.budget_us }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units_of(plan: &Plan) -> Vec<u64> {
        plan.runs.iter().flat_map(|r| (r.dst / UNIT)..(r.dst + r.len) / UNIT).collect()
    }

    /// Adjacent units become one run; the ring is packed back to back.
    #[test]
    fn adjacent_units_become_one_run() {
        let p = Planner::default().plan(&[7, 5, 6, 6, 20], |_| false);
        assert_eq!(
            p.runs,
            vec![Run { dst: 5 * UNIT, staging: 0, len: 3 * UNIT }, Run { dst: 20 * UNIT, staging: 3 * UNIT, len: UNIT }]
        );
        assert_eq!((p.dirty_units, p.copied_units, p.staging_bytes()), (4, 4, 4 * UNIT));
    }

    /// The baseline makes one command per unit.
    #[test]
    fn the_baseline_makes_one_command_per_unit() {
        let p = Planner::per_unit().plan(&[1, 2, 3], |_| true);
        assert_eq!(p.runs.len(), 3);
        assert_eq!(units_of(&p), vec![1, 2, 3]);
    }

    /// A small clean gap is copied only where the caller allows it and the
    /// model finds it cheaper than a command.
    #[test]
    fn gaps_are_filled_only_when_allowed_and_cheaper() {
        let model = CostModel::default();
        let g = model.profitable_gap_units();
        assert!(g >= 1, "with the measured costs one 4 KiB unit is cheaper than a command (g = {g})");
        let filling = Planner::with_gap_filling();
        let p = filling.plan(&[0, 1 + g], |_| true);
        assert_eq!(p.runs.len(), 1, "a gap of {g} unit(s) is filled");
        assert_eq!(p.copied_units, 2 + g);
        let p = filling.plan(&[0, 2 + g], |_| true);
        assert_eq!(p.runs.len(), 2, "a wider gap is not");
        assert_eq!(Planner::default().plan(&[0, 1 + g], |_| true).runs.len(), 2, "not filled by default");
        let p = filling.plan(&[0, 1 + g], |_| false);
        assert_eq!(p.runs.len(), 2, "a gap the caller does not allow is never copied");
        assert_eq!(p.copied_units, 2);
    }

    /// Every dirty unit is copied, runs never overlap, and without gap
    /// filling exactly the dirty units are copied (random sets).
    #[test]
    fn plans_cover_the_dirty_set_and_nothing_else() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..200 {
            let n = (next() % 500) as usize;
            let units: Vec<u64> = (0..n).map(|_| next() % 2000).collect();
            let mut want = units.clone();
            want.sort_unstable();
            want.dedup();
            let allowed = |u: u64| !u.is_multiple_of(3);
            for planner in [Planner::with_gap_filling(), Planner::default(), Planner::per_unit()] {
                let p = planner.plan(&units, allowed);
                let got = units_of(&p);
                assert!(got.windows(2).all(|w| w[0] < w[1]), "runs are ordered and disjoint");
                assert!(want.iter().all(|u| got.binary_search(u).is_ok()), "every dirty unit is copied");
                for u in &got {
                    assert!(want.binary_search(u).is_ok() || (planner.fill_gaps && allowed(*u)), "unit {u} copied without leave");
                }
                if !planner.fill_gaps {
                    assert_eq!(got, want);
                }
                let mut at = 0;
                for r in &p.runs {
                    assert_eq!(r.staging, at, "the ring is packed");
                    at += r.len;
                }
            }
        }
    }

    /// A plan over the budget still forwards everything and says so.
    #[test]
    fn a_plan_over_budget_is_complete_and_flagged() {
        let scattered: Vec<u64> = (0..5000).map(|i| i * 10).collect();
        let p = Planner::default().plan(&scattered, |_| false);
        assert_eq!(p.copied_units, 5000);
        assert!(p.over_budget, "5000 commands at 0.6 µs exceed 1 ms: {:.0} µs", p.estimate_us);
        // CPU pack and GPU copy are added (not overlapped): ≈10 MB per ms.
        let p = Planner::default().plan(&(0..2048).collect::<Vec<_>>(), |_| false);
        assert_eq!(p.runs.len(), 1);
        assert!(!p.over_budget, "8 MiB in one run fits: {:.0} µs", p.estimate_us);
    }
}
