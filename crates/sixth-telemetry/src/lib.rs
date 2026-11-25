//! The Gen 2 event log (ADR 0001, "Planned crates": telemetry first, because
//! every later decision needs its data).
//!
//! It records what the memory model did and why: regions mapped and freed,
//! ownership transitions, proposals made from observed usage (which D3 says
//! may only be proposed, never applied silently), contract violations, and
//! forwarding plans with their estimated and measured costs.
//!
//! The log is bounded: when it is full the oldest records go and are
//! counted, so a long run never grows without limit and the loss is visible.
//! Per-kind counters are never dropped. The crate knows nothing of the
//! memory model's types; events carry plain numbers and static names.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// One thing that happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A region of title address space was mapped.
    RegionMapped { region: u32, base: u64, len: u64, home: &'static str, contract: &'static str },
    /// A region was freed; its page-table entries were invalidated.
    RegionFreed { region: u32, base: u64, len: u64 },
    /// A page's ownership changed.
    Transition { page: u64, from: &'static str, to: &'static str, reason: &'static str },
    /// Observed usage suggests another ownership. Logged for review; the
    /// memory model does not act on it (D3).
    Proposal { page: u64, to: &'static str, frames: u32 },
    /// Memory was used against its contract.
    Violation { page: u64, what: &'static str, bytes: u64 },
    /// A forwarding plan: what was copied and its estimated cost.
    Forward { region: u32, runs: u32, units: u64, bytes: u64, estimate_us: f64, over_budget: bool },
    /// A measured transfer.
    Transfer { what: &'static str, bytes: u64, micros: f64 },
}

/// The kinds, for counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    RegionMapped,
    RegionFreed,
    Transition,
    Proposal,
    Violation,
    Forward,
    Transfer,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::RegionMapped,
        Kind::RegionFreed,
        Kind::Transition,
        Kind::Proposal,
        Kind::Violation,
        Kind::Forward,
        Kind::Transfer,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::RegionMapped => "region_mapped",
            Kind::RegionFreed => "region_freed",
            Kind::Transition => "transition",
            Kind::Proposal => "proposal",
            Kind::Violation => "violation",
            Kind::Forward => "forward",
            Kind::Transfer => "transfer",
        }
    }
}

impl Event {
    pub fn kind(&self) -> Kind {
        match self {
            Event::RegionMapped { .. } => Kind::RegionMapped,
            Event::RegionFreed { .. } => Kind::RegionFreed,
            Event::Transition { .. } => Kind::Transition,
            Event::Proposal { .. } => Kind::Proposal,
            Event::Violation { .. } => Kind::Violation,
            Event::Forward { .. } => Kind::Forward,
            Event::Transfer { .. } => Kind::Transfer,
        }
    }
}

/// An event and the frame it happened in.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub frame: u64,
    pub event: Event,
}

impl fmt::Display for Record {
    /// One CSV line: `frame,kind,key=value;...`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},", self.frame, self.event.kind().name())?;
        match &self.event {
            Event::RegionMapped { region, base, len, home, contract } => {
                write!(f, "region={region};base={base:#x};len={len:#x};home={home};contract={contract}")
            }
            Event::RegionFreed { region, base, len } => write!(f, "region={region};base={base:#x};len={len:#x}"),
            Event::Transition { page, from, to, reason } => {
                write!(f, "page={page:#x};from={from};to={to};reason={reason}")
            }
            Event::Proposal { page, to, frames } => write!(f, "page={page:#x};to={to};frames={frames}"),
            Event::Violation { page, what, bytes } => write!(f, "page={page:#x};what={what};bytes={bytes}"),
            Event::Forward { region, runs, units, bytes, estimate_us, over_budget } => write!(
                f,
                "region={region};runs={runs};units={units};bytes={bytes};estimate_us={estimate_us:.3};over_budget={over_budget}"
            ),
            Event::Transfer { what, bytes, micros } => write!(f, "what={what};bytes={bytes};micros={micros:.3}"),
        }
    }
}

/// The log. Shared by reference; every method takes `&self`.
pub struct Telemetry {
    capacity: usize,
    records: Mutex<VecDeque<Record>>,
    frame: AtomicU64,
    dropped: AtomicU64,
    counts: [AtomicU64; Kind::ALL.len()],
}

impl Telemetry {
    /// A log keeping at most `capacity` records (at least 1).
    pub fn new(capacity: usize) -> Telemetry {
        Telemetry {
            capacity: capacity.max(1),
            records: Mutex::new(VecDeque::new()),
            frame: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            counts: Default::default(),
        }
    }

    /// Records `event` in the current frame.
    pub fn record(&self, event: Event) {
        self.counts[event.kind() as usize].fetch_add(1, Ordering::Relaxed);
        let frame = self.frame();
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        if records.len() == self.capacity {
            records.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        records.push_back(Record { frame, event });
    }

    /// The current frame number.
    pub fn frame(&self) -> u64 {
        self.frame.load(Ordering::Relaxed)
    }

    /// Starts the next frame; returns its number.
    pub fn next_frame(&self) -> u64 {
        self.frame.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// How many events of `kind` were ever recorded (dropped ones included).
    pub fn count(&self, kind: Kind) -> u64 {
        self.counts[kind as usize].load(Ordering::Relaxed)
    }

    /// How many records were dropped because the log was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Takes the records kept so far, oldest first.
    pub fn drain(&self) -> Vec<Record> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }

    /// The records kept so far, oldest first, without taking them.
    pub fn snapshot(&self) -> Vec<Record> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    /// One line with every counter, for a periodic report.
    pub fn summary(&self) -> String {
        let mut s = format!("telemetry frame {}", self.frame());
        for kind in Kind::ALL {
            s += &format!(" {}={}", kind.name(), self.count(kind));
        }
        s + &format!(" dropped={}", self.dropped())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(page: u64) -> Event {
        Event::Violation { page, what: "test", bytes: 1 }
    }

    /// Records come back in order with the frame they were made in.
    #[test]
    fn records_keep_their_order_and_frame() {
        let t = Telemetry::new(16);
        t.record(violation(1));
        assert_eq!(t.next_frame(), 1);
        t.record(violation(2));
        let r = t.drain();
        assert_eq!(r, vec![Record { frame: 0, event: violation(1) }, Record { frame: 1, event: violation(2) }]);
        assert!(t.drain().is_empty(), "drained once");
    }

    /// A full log drops the oldest and says how many; counters keep all.
    #[test]
    fn a_full_log_drops_the_oldest_and_counts_it() {
        let t = Telemetry::new(3);
        for p in 0..5 {
            t.record(violation(p));
        }
        let pages: Vec<u64> = t
            .snapshot()
            .iter()
            .map(|r| match r.event {
                Event::Violation { page, .. } => page,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(pages, vec![2, 3, 4]);
        assert_eq!(t.dropped(), 2);
        assert_eq!(t.count(Kind::Violation), 5);
        assert_eq!(t.count(Kind::Forward), 0);
        assert!(t.summary().contains("violation=5") && t.summary().contains("dropped=2"));
    }

    /// The CSV form names every field.
    #[test]
    fn a_record_prints_as_one_csv_line() {
        let r = Record {
            frame: 7,
            event: Event::Forward { region: 2, runs: 3, units: 40, bytes: 163840, estimate_us: 8.25, over_budget: false },
        };
        assert_eq!(r.to_string(), "7,forward,region=2;runs=3;units=40;bytes=163840;estimate_us=8.250;over_budget=false");
    }

    /// Several threads may record at once; nothing is lost below capacity.
    #[test]
    fn threads_can_record_together() {
        let t = Telemetry::new(10_000);
        std::thread::scope(|s| {
            for k in 0..4u64 {
                let t = &t;
                s.spawn(move || {
                    for i in 0..1000 {
                        t.record(violation(k * 1000 + i));
                    }
                });
            }
        });
        assert_eq!(t.count(Kind::Violation), 4000);
        assert_eq!(t.drain().len(), 4000);
    }
}
