//! Residency: which host resources live in the card's memory, decided
//! against the budget the system gives the process.
//!
//! The inherited rule was one threshold: past nine tenths of the budget no
//! more texture copies were made, and the descriptor slots waiting for one
//! showed nothing for as long as the card stayed full. This module replaces
//! that cut-off with three decisions, each a pure function of figures the
//! caller supplies (the backend acts; nothing here touches a GPU):
//!
//! - **pressure** ([`Residency::observe`]): the card's use against its
//!   budget, in four levels with hysteresis, so a frame at the edge does not
//!   flip placement back and forth;
//! - **eviction** ([`Residency::evict`]): which resources to give up to
//!   bring the use back under the target, only among those that can be made
//!   again from their source (a texture the title fills: its bytes are in
//!   title memory), that nothing holds (no list in flight, no view) and
//!   whose last use the GPU has finished (fence-aware); lowest priority
//!   first, then least recently used, then largest;
//! - **placement** ([`Residency::place`]): where a new resource goes. In the
//!   card's memory while there is room; if not, a resource the GPU only
//!   reads may go to the PC's memory, GPU-visible (slower to read, but
//!   there and correct); one the GPU writes or renders to stays on the card
//!   (the system pages it if it must). A refusal is the last resort, only
//!   when neither pool has room, and it carries its reason, so it is never
//!   silent.
//!
//! Sizes are the device's allocation sizes (alignment included), not the
//! bytes a resource holds: residency is about what the card reserves.

/// The budget the system gives the process, and its use, in bytes: the
/// card's own memory, and the PC's memory shared with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub local_budget: u64,
    pub local_used: u64,
    pub system_budget: u64,
    pub system_used: u64,
}

impl Budget {
    /// The card's use as a share of its budget (1.0 = all of it; more when
    /// over it). 0 without a budget.
    pub fn local_share(&self) -> f64 {
        if self.local_budget == 0 {
            return 0.0;
        }
        self.local_used as f64 / self.local_budget as f64
    }
}

/// How full the card is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    /// Below the low mark: room to bring resources back to the card.
    Low,
    Normal,
    /// At or past the target: new GPU-read resources go to the PC's memory,
    /// and resources nothing needs are given up.
    High,
    /// Near or over the budget: as `High`, and eviction takes resources
    /// idle for a single frame.
    Critical,
}

/// What a resource is, for its priority and for where it may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Can be made again from its source and is only read by the GPU (a
    /// texture the title fills): the first to give up; may live in the
    /// PC's memory.
    Streamed,
    /// Only read by the GPU, but with no other copy of its bytes once made
    /// (or costly to make again): may live in the PC's memory; never
    /// evicted here.
    Read,
    /// Written or rendered to by the GPU (render targets, depth, unordered
    /// access, shader-written buffers): on the card; never evicted here.
    Written,
}

/// Where a resource goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The card's memory.
    Local,
    /// The PC's memory, GPU-visible, because the card has no room.
    System,
    /// Neither: not made now. The reason is reported.
    Refuse(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The card is past its target and the PC's shared memory is too.
    BothPoolsFull,
}

/// A resource that eviction may consider, as the backend sees it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub class: Class,
    /// Allocated bytes.
    pub bytes: u64,
    /// The frame it was last used in.
    pub last_frame: u64,
    /// The submission (fence value) that last used it; it may go once the
    /// GPU has completed that one.
    pub last_fence: u64,
    /// Held by something that would read it without asking again (a list
    /// not yet run, a descriptor slot): not to be given up.
    pub held: bool,
}

/// The thresholds, as shares of the card's budget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    /// Below this the pressure is `Low` (and `Normal` comes back below it
    /// plus the margin).
    pub low: f64,
    /// `High` from here (the inherited cut-off).
    pub target: f64,
    /// `Critical` from here.
    pub critical: f64,
    /// A level is left only this far below the mark that entered it.
    pub margin: f64,
    /// The PC's shared memory a resource may take as a fallback, as a share
    /// of its budget.
    pub system_target: f64,
    /// Frames a streamed resource must have been idle to be evicted, at
    /// `High` (at `Critical`: one).
    pub idle_frames: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            low: 0.60,
            target: 0.90,
            critical: 0.97,
            margin: 0.05,
            system_target: 0.75,
            idle_frames: 4,
        }
    }
}

/// The state kept between decisions: the pressure level (for hysteresis)
/// and what has been decided so far.
#[derive(Clone, Debug)]
pub struct Residency {
    pub policy: Policy,
    pressure: Pressure,
    /// Since the start: placements in each pool, refusals, evictions and
    /// their bytes.
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub local: u64,
    pub system: u64,
    pub refused: u64,
    pub evicted: u64,
    pub evicted_bytes: u64,
}

impl Default for Residency {
    fn default() -> Self {
        Residency::new(Policy::default())
    }
}

impl Residency {
    pub fn new(policy: Policy) -> Self {
        Residency {
            policy,
            pressure: Pressure::Normal,
            stats: Stats::default(),
        }
    }

    pub fn pressure(&self) -> Pressure {
        self.pressure
    }

    /// The pressure for `budget`: up a level when the share reaches its
    /// mark, down only once the share is `margin` clear of it (and up from
    /// `Low` only `margin` past the low mark), so a frame at an edge does
    /// not flip placement. Without a budget (none reported): `Normal`.
    pub fn observe(&mut self, budget: &Budget) -> Pressure {
        if budget.local_budget == 0 {
            self.pressure = Pressure::Normal;
            return self.pressure;
        }
        let (s, p) = (budget.local_share(), &self.policy);
        let mut level = self.pressure;
        loop {
            level = match level {
                Pressure::Low if s >= p.low + p.margin => Pressure::Normal,
                Pressure::Normal if s >= p.target => Pressure::High,
                Pressure::High if s >= p.critical => Pressure::Critical,
                _ => break,
            };
        }
        if level == self.pressure {
            loop {
                level = match level {
                    Pressure::Critical if s < p.critical - p.margin => Pressure::High,
                    Pressure::High if s < p.target - p.margin => Pressure::Normal,
                    Pressure::Normal if s < p.low => Pressure::Low,
                    _ => break,
                };
            }
        }
        self.pressure = level;
        level
    }

    /// The bytes to give up to bring the card's use back under the target
    /// less the margin (0 when it is there already).
    pub fn need(&self, budget: &Budget) -> u64 {
        let p = &self.policy;
        let permille = ((p.target - p.margin) * 1000.0).round().max(0.0) as u128;
        let goal = (u128::from(budget.local_budget) * permille / 1000) as u64;
        budget.local_used.saturating_sub(goal)
    }

    /// Which of `candidates` to give up to free `need` bytes, as indices
    /// into it: only streamed resources that nothing holds, whose last use
    /// the GPU has completed (`completed_fence`), idle for the policy's
    /// frames (one at `Critical`); lowest class first, then least recently
    /// used, then largest. Fewer than `need` bytes when there are not more.
    /// Counted in [`Stats`].
    pub fn evict(&mut self, need: u64, candidates: &[Candidate], frame: u64, completed_fence: u64) -> Vec<usize> {
        if need == 0 {
            return Vec::new();
        }
        let idle = if self.pressure == Pressure::Critical { 1 } else { self.policy.idle_frames };
        let mut eligible: Vec<usize> = (0..candidates.len())
            .filter(|&i| {
                let c = &candidates[i];
                c.class == Class::Streamed
                    && !c.held
                    && c.last_fence <= completed_fence
                    && frame.saturating_sub(c.last_frame) >= idle
            })
            .collect();
        eligible.sort_by(|&a, &b| {
            let (a, b) = (&candidates[a], &candidates[b]);
            (a.class, a.last_frame, std::cmp::Reverse(a.bytes)).cmp(&(b.class, b.last_frame, std::cmp::Reverse(b.bytes)))
        });
        let mut freed = 0u64;
        let mut chosen = Vec::new();
        for i in eligible {
            if freed >= need {
                break;
            }
            freed += candidates[i].bytes;
            chosen.push(i);
        }
        self.stats.evicted += chosen.len() as u64;
        self.stats.evicted_bytes += freed;
        chosen
    }

    /// Where a new resource of `class` and `bytes` goes, at the pressure of
    /// the last [`Residency::observe`] and `budget` now. Counted in [`Stats`].
    pub fn place(&mut self, class: Class, bytes: u64, budget: &Budget) -> Placement {
        let p = self.policy;
        let local_room = budget.local_budget == 0
            || (budget.local_used.saturating_add(bytes) as f64) < budget.local_budget as f64 * p.target;
        let placement = if self.pressure < Pressure::High && local_room {
            Placement::Local
        } else if class == Class::Written {
            // The GPU writes it: in the card's memory whatever the
            // pressure (the system pages the card's memory if it must).
            Placement::Local
        } else if (budget.system_used.saturating_add(bytes) as f64) < budget.system_budget as f64 * p.system_target {
            Placement::System
        } else {
            Placement::Refuse(Refusal::BothPoolsFull)
        };
        match placement {
            Placement::Local => self.stats.local += 1,
            Placement::System => self.stats.system += 1,
            Placement::Refuse(_) => self.stats.refused += 1,
        }
        placement
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn budget(local_used_tenths: u64, system_used_gib: u64) -> Budget {
        Budget {
            local_budget: 10 * GIB,
            local_used: local_used_tenths * GIB,
            system_budget: 16 * GIB,
            system_used: system_used_gib * GIB,
        }
    }

    fn at(share_percent: u64) -> Budget {
        Budget {
            local_budget: 100 * GIB,
            local_used: share_percent * GIB,
            system_budget: 16 * GIB,
            system_used: 0,
        }
    }

    #[test]
    fn pressure_has_levels_and_hysteresis() {
        let mut r = Residency::default();
        assert_eq!(r.observe(&at(50)), Pressure::Low);
        assert_eq!(r.observe(&at(70)), Pressure::Normal);
        assert_eq!(r.observe(&at(90)), Pressure::High, "the inherited cut-off");
        // Just under the target: still High (within the margin).
        assert_eq!(r.observe(&at(88)), Pressure::High);
        assert_eq!(r.observe(&at(86)), Pressure::High);
        assert_eq!(r.observe(&at(84)), Pressure::Normal, "clear of the target by the margin");
        assert_eq!(r.observe(&at(98)), Pressure::Critical);
        assert_eq!(r.observe(&at(95)), Pressure::Critical, "within the margin of critical");
        assert_eq!(r.observe(&at(91)), Pressure::High, "clear of critical, still past the target");
        assert_eq!(r.observe(&at(80)), Pressure::Normal, "clear of both at once");
        assert_eq!(r.observe(&at(120)), Pressure::Critical, "over the budget");
        // No budget reported: nothing is decided from it.
        assert_eq!(r.observe(&Budget::default()), Pressure::Normal);
    }

    #[test]
    fn placement_falls_back_instead_of_refusing() {
        let mut r = Residency::default();
        r.observe(&budget(5, 0));
        assert_eq!(r.place(Class::Streamed, GIB / 2, &budget(5, 0)), Placement::Local);
        // A resource that would take the card past the target, at Normal
        // pressure: the PC's memory for a read-only one, the card for one
        // the GPU writes.
        r.observe(&budget(8, 0));
        assert_eq!(r.place(Class::Streamed, 2 * GIB, &budget(8, 0)), Placement::System);
        assert_eq!(r.place(Class::Read, 2 * GIB, &budget(8, 0)), Placement::System);
        assert_eq!(r.place(Class::Written, 2 * GIB, &budget(8, 0)), Placement::Local);
        // At High pressure even a small read-only resource goes to the PC.
        r.observe(&budget(9, 0));
        assert_eq!(r.pressure(), Pressure::High);
        assert_eq!(r.place(Class::Streamed, 1 << 20, &budget(9, 0)), Placement::System);
        // Both pools full: refused, with a reason, and counted.
        assert_eq!(r.place(Class::Streamed, 1 << 20, &budget(9, 12)), Placement::Refuse(Refusal::BothPoolsFull));
        assert_eq!(r.place(Class::Written, 1 << 20, &budget(9, 12)), Placement::Local, "never refused");
        assert_eq!(r.stats, Stats { local: 3, system: 3, refused: 1, evicted: 0, evicted_bytes: 0 });
    }

    fn c(class: Class, mib: u64, last_frame: u64, last_fence: u64, held: bool) -> Candidate {
        Candidate { class, bytes: mib << 20, last_frame, last_fence, held }
    }

    #[test]
    fn eviction_is_fence_aware_and_least_recently_used_first() {
        let mut r = Residency::default();
        r.observe(&at(92));
        let candidates = [
            c(Class::Streamed, 64, 90, 10, false),  // 0: used 10 frames ago
            c(Class::Streamed, 16, 50, 5, false),   // 1: oldest
            c(Class::Streamed, 32, 50, 5, false),   // 2: as old, larger: first of the two
            c(Class::Streamed, 128, 40, 30, false), // 3: old, but the GPU is not done with it
            c(Class::Streamed, 256, 10, 1, true),   // 4: held by a slot
            c(Class::Read, 512, 1, 1, false),       // 5: no other copy: never here
            c(Class::Written, 512, 1, 1, false),    // 6: a render target: never here
            c(Class::Streamed, 8, 99, 20, false),   // 7: used last frame: not idle
        ];
        // Fence 20 completed, frame 100.
        assert_eq!(r.evict(40 << 20, &candidates, 100, 20), [2, 1], "LRU, larger first among equals; stops once enough");
        assert_eq!(r.evict(1 << 40, &candidates, 100, 20), [2, 1, 0], "all that may go, and no more");
        assert_eq!(r.evict(0, &candidates, 100, 20), [] as [usize; 0]);
        // At Critical, idle for one frame is enough; and with fence 30 done
        // the one the GPU was using may go too.
        r.observe(&at(99));
        let all = r.evict(1 << 40, &candidates, 100, 30);
        assert_eq!(all, [3, 2, 1, 0, 7]);
        assert_eq!(r.stats.evicted, 2 + 3 + 5);
    }

    #[test]
    fn need_brings_the_use_under_the_target_less_the_margin() {
        let r = Residency::default();
        assert_eq!(r.need(&at(80)), 0);
        assert_eq!(r.need(&at(85)), 0);
        assert_eq!(r.need(&at(95)), 10 * GIB);
    }
}
