//! The title address space: regions, the page table, and the ownership
//! rules that run on it.
//!
//! **Page table.** One entry per 64 KiB page, filled when a region is
//! mapped and removed when it is freed. Each page number carries a
//! generation that grows on every free, so a [`PageRef`] taken before a
//! free never matches the entry of a later mapping at the same address
//! (the inherited directory was fill-only: DeepSeek round 2).
//!
//! **Ownership rules** (Part 3 §3.2 as amended by ADR 0001 D3/D4), applied
//! when the backend reports a CPU write (OS dirty bits) or a GPU write set:
//!
//! | Page | CPU write | GPU write |
//! |---|---|---|
//! | `CpuOwned` | forward the dirty units | migrate to coherent before the write, → `Mixed` |
//! | `GpuOwned` | read back; merge byte-exactly with a snapshot, else the GPU's bytes win and a violation is logged; → `Mixed` | — |
//! | `ReadShared` | → `CpuOwned`, forward | → `GpuOwned` |
//! | `Unknown`, `Mixed` | — (coherent) | — (coherent) |
//!
//! `Mixed` is sticky. `Unknown` pages are never reclassified by watching
//! them (D3): after `proposal_frames` frames of one-sided use a
//! [`Event::Proposal`] is logged, and only [`AddressSpace::adopt`] with a
//! contract changes anything.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use sixth_telemetry::{Event, Telemetry};

use crate::{Contract, Home, Ownership, PAGE, UNIT, UNITS_PER_PAGE};

/// A mapped region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionId(pub u32);

#[derive(Debug, Clone, PartialEq)]
struct Region {
    id: RegionId,
    base: u64,
    len: u64,
    contract: Contract,
}

/// A page-table entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PageEntry {
    pub region: RegionId,
    /// The page's index within its region.
    pub index: u32,
    pub generation: u32,
    pub ownership: Ownership,
    pub home: Home,
    /// A copy of the bytes last forwarded exists, so a late CPU write can be
    /// merged byte-exactly (D4). Set by the backend that keeps it.
    pub has_snapshot: bool,
    cpu_frames: u32,
    gpu_frames: u32,
    cpu_now: bool,
    gpu_now: bool,
    proposed: Option<Ownership>,
}

/// A reference to one page as it was when the reference was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageRef {
    /// Page number: address / [`PAGE`].
    pub page: u64,
    pub generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    Unaligned { base: u64, len: u64 },
    Empty,
    Overlaps { base: u64, region: RegionId },
    UnknownRegion(RegionId),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Unaligned { base, len } => write!(f, "{base:#x}+{len:#x} is not 64 KiB aligned"),
            MapError::Empty => write!(f, "an empty region"),
            MapError::Overlaps { base, region } => write!(f, "{base:#x} overlaps region {}", region.0),
            MapError::UnknownRegion(r) => write!(f, "no region {}", r.0),
        }
    }
}

impl std::error::Error for MapError {}

/// What the backend has to do, in the order returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Copy these dirty units (bit i = unit i of the page) to VRAM before
    /// the next submission that may read the page.
    Forward { page: u64, units: u16 },
    /// The page enters a GPU write set while CPU-owned: retire its pending
    /// consumers, then move it to coherent memory, before the writing
    /// submission runs.
    MigrateBeforeGpuWrite { page: u64 },
    /// A late CPU write on a GPU-owned page with a snapshot: fence, read the
    /// page back, merge with [`crate::merge::merge_with_snapshot`], then move
    /// it to coherent memory.
    ReadBackAndMerge { page: u64, units: u16 },
    /// The same without a snapshot: the read-back bytes win
    /// ([`crate::merge::gpu_authority`]); the write was logged as a violation.
    ReadBackGpuWins { page: u64, units: u16 },
}

/// The address space.
pub struct AddressSpace {
    telemetry: Arc<Telemetry>,
    regions: BTreeMap<u64, Region>,
    bases: HashMap<RegionId, u64>,
    pages: HashMap<u64, PageEntry>,
    generations: HashMap<u64, u32>,
    touched: Vec<u64>,
    next_id: u32,
    /// Frames of one-sided use before a proposal is logged (default 4, as
    /// the old rule's N; here it only proposes).
    pub proposal_frames: u32,
}

impl AddressSpace {
    pub fn new(telemetry: Arc<Telemetry>) -> AddressSpace {
        AddressSpace {
            telemetry,
            regions: BTreeMap::new(),
            bases: HashMap::new(),
            pages: HashMap::new(),
            generations: HashMap::new(),
            touched: Vec::new(),
            next_id: 1,
            proposal_frames: 4,
        }
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Maps `len` bytes at `base` (both 64 KiB aligned) under `contract`.
    pub fn map(&mut self, base: u64, len: u64, contract: Contract) -> Result<RegionId, MapError> {
        if len == 0 {
            return Err(MapError::Empty);
        }
        if !base.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) || base.checked_add(len).is_none() {
            return Err(MapError::Unaligned { base, len });
        }
        let end = base + len;
        if let Some(r) = self.regions.range(..end).next_back().map(|(_, r)| r).filter(|r| r.base + r.len > base) {
            return Err(MapError::Overlaps { base, region: r.id });
        }
        let id = RegionId(self.next_id);
        self.next_id += 1;
        let (ownership, home) = (contract.initial_ownership(), contract.initial_home());
        for (index, page) in (base / PAGE..end / PAGE).enumerate() {
            let generation = *self.generations.get(&page).unwrap_or(&0);
            self.pages.insert(
                page,
                PageEntry {
                    region: id,
                    index: index as u32,
                    generation,
                    ownership,
                    home,
                    has_snapshot: false,
                    cpu_frames: 0,
                    gpu_frames: 0,
                    cpu_now: false,
                    gpu_now: false,
                    proposed: None,
                },
            );
        }
        self.telemetry.record(Event::RegionMapped { region: id.0, base, len, home: home.name(), contract: contract.name() });
        self.regions.insert(base, Region { id, base, len, contract });
        self.bases.insert(id, base);
        Ok(id)
    }

    /// Frees a region: its entries go, and its page numbers get a new
    /// generation.
    pub fn unmap(&mut self, id: RegionId) -> Result<(), MapError> {
        let base = self.bases.remove(&id).ok_or(MapError::UnknownRegion(id))?;
        let region = self.regions.remove(&base).expect("bases and regions agree");
        for page in base / PAGE..(base + region.len) / PAGE {
            self.pages.remove(&page);
            *self.generations.entry(page).or_insert(0) += 1;
        }
        self.telemetry.record(Event::RegionFreed { region: id.0, base, len: region.len });
        Ok(())
    }

    /// The entry for `addr`, and a reference to it.
    pub fn lookup(&self, addr: u64) -> Option<(PageRef, &PageEntry)> {
        let page = addr / PAGE;
        self.pages.get(&page).map(|e| (PageRef { page, generation: e.generation }, e))
    }

    /// The entry `r` was taken from, if it is still mapped.
    pub fn entry(&self, r: PageRef) -> Option<&PageEntry> {
        self.pages.get(&r.page).filter(|e| e.generation == r.generation)
    }

    pub fn is_current(&self, r: PageRef) -> bool {
        self.entry(r).is_some()
    }

    /// The region's contract.
    pub fn contract(&self, id: RegionId) -> Option<&Contract> {
        self.bases.get(&id).map(|b| &self.regions[b].contract)
    }

    /// Whether the backend keeps a last-forwarded snapshot of `page`.
    pub fn set_snapshot(&mut self, page: u64, kept: bool) {
        if let Some(e) = self.pages.get_mut(&page) {
            e.has_snapshot = kept;
        }
    }

    /// The CPU wrote `[addr, addr + len)` (dirty bits, explicit writers).
    pub fn cpu_wrote(&mut self, addr: u64, len: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        for (page, units) in units_by_page(addr, len) {
            if let Some(a) = self.cpu_wrote_page(page, units) {
                actions.push(a);
            }
        }
        actions
    }

    /// A submission may write `[addr, addr + len)`.
    pub fn gpu_writes(&mut self, addr: u64, len: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        for (page, _) in units_by_page(addr, len) {
            if let Some(a) = self.gpu_writes_page(page) {
                actions.push(a);
            }
        }
        actions
    }

    /// A submission may write anywhere in the region (a raw-address store
    /// whose reach is not narrowed: Part 3 §3.2, conservative write sets).
    pub fn gpu_writes_region(&mut self, id: RegionId) -> Result<Vec<Action>, MapError> {
        let base = *self.bases.get(&id).ok_or(MapError::UnknownRegion(id))?;
        let len = self.regions[&base].len;
        Ok(self.gpu_writes(base, len))
    }

    fn cpu_wrote_page(&mut self, page: u64, units: u16) -> Option<Action> {
        let e = self.pages.get_mut(&page)?;
        if !e.cpu_now && !e.gpu_now {
            self.touched.push(page);
        }
        e.cpu_now = true;
        match e.ownership {
            Ownership::CpuOwned => Some(Action::Forward { page, units }),
            Ownership::ReadShared => {
                self.transition(page, Ownership::CpuOwned, Home::Vram, "first_write_cpu");
                Some(Action::Forward { page, units })
            }
            Ownership::GpuOwned => {
                let action = if e.has_snapshot {
                    Action::ReadBackAndMerge { page, units }
                } else {
                    let bytes = u64::from(units.count_ones()) * UNIT;
                    self.telemetry.record(Event::Violation { page, what: "late_cpu_write_without_snapshot", bytes });
                    Action::ReadBackGpuWins { page, units }
                };
                self.transition(page, Ownership::Mixed, Home::Coherent, "late_cpu_write_on_gpu_owned");
                Some(action)
            }
            Ownership::Unknown | Ownership::Mixed => None,
        }
    }

    fn gpu_writes_page(&mut self, page: u64) -> Option<Action> {
        let e = self.pages.get_mut(&page)?;
        if !e.cpu_now && !e.gpu_now {
            self.touched.push(page);
        }
        e.gpu_now = true;
        match e.ownership {
            Ownership::CpuOwned => {
                self.transition(page, Ownership::Mixed, Home::Coherent, "gpu_write_on_cpu_owned");
                Some(Action::MigrateBeforeGpuWrite { page })
            }
            Ownership::ReadShared => {
                self.transition(page, Ownership::GpuOwned, Home::Vram, "first_write_gpu");
                None
            }
            Ownership::GpuOwned | Ownership::Unknown | Ownership::Mixed => None,
        }
    }

    fn transition(&mut self, page: u64, to: Ownership, home: Home, reason: &'static str) {
        let e = self.pages.get_mut(&page).expect("caller looked it up");
        let from = e.ownership;
        e.ownership = to;
        e.home = home;
        self.telemetry.record(Event::Transition { page, from: from.name(), to: to.name(), reason });
    }

    /// Ends the frame: counts one-sided use of unclassified pages and logs
    /// proposals. Changes no ownership (D3).
    pub fn end_frame(&mut self) {
        for page in std::mem::take(&mut self.touched) {
            let Some(e) = self.pages.get_mut(&page) else { continue };
            let (cpu, gpu) = (e.cpu_now, e.gpu_now);
            e.cpu_now = false;
            e.gpu_now = false;
            if e.ownership != Ownership::Unknown {
                continue;
            }
            match (cpu, gpu) {
                (true, false) => (e.cpu_frames, e.gpu_frames) = (e.cpu_frames + 1, 0),
                (false, true) => (e.cpu_frames, e.gpu_frames) = (0, e.gpu_frames + 1),
                _ => (e.cpu_frames, e.gpu_frames, e.proposed) = (0, 0, None),
            }
            let suggest = if e.cpu_frames >= self.proposal_frames {
                Some((Ownership::CpuOwned, e.cpu_frames))
            } else if e.gpu_frames >= self.proposal_frames {
                Some((Ownership::GpuOwned, e.gpu_frames))
            } else {
                None
            };
            if let Some((to, frames)) = suggest.filter(|(to, _)| e.proposed != Some(*to)) {
                e.proposed = Some(to);
                self.telemetry.record(Event::Proposal { page, to: to.name(), frames });
            }
        }
        self.telemetry.next_frame();
    }

    /// Adopts `contract` for a region: the reviewed way a proposal (or a new
    /// proof) takes effect. `Mixed` pages stay `Mixed` unless the contract
    /// is a proof. Returns the number of pages that changed.
    pub fn adopt(&mut self, id: RegionId, contract: Contract) -> Result<usize, MapError> {
        let base = *self.bases.get(&id).ok_or(MapError::UnknownRegion(id))?;
        let len = self.regions[&base].len;
        let (to, home) = (contract.initial_ownership(), contract.initial_home());
        let proven = matches!(contract, Contract::Proven { .. });
        let mut changed = 0;
        for page in base / PAGE..(base + len) / PAGE {
            let from = self.pages[&page].ownership;
            if from == to || (from == Ownership::Mixed && !proven) {
                continue;
            }
            self.transition(page, to, home, "contract_adopted");
            changed += 1;
        }
        self.regions.get_mut(&base).expect("present").contract = contract;
        Ok(changed)
    }
}

/// `[addr, addr + len)` as (page number, dirty-unit mask) pairs.
fn units_by_page(addr: u64, len: u64) -> Vec<(u64, u16)> {
    let mut out: Vec<(u64, u16)> = Vec::new();
    if len == 0 {
        return out;
    }
    let first = addr / UNIT;
    let last = (addr + len - 1) / UNIT;
    for unit in first..=last {
        let page = unit / u64::from(UNITS_PER_PAGE);
        let bit = 1u16 << (unit % u64::from(UNITS_PER_PAGE));
        match out.last_mut() {
            Some((p, mask)) if *p == page => *mask |= bit,
            _ => out.push((page, bit)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Declared;
    use sixth_telemetry::Kind;

    const BASE: u64 = 0x10_0000_0000;

    fn space() -> AddressSpace {
        AddressSpace::new(Arc::new(Telemetry::new(1024)))
    }

    fn declared(d: Declared) -> Contract {
        Contract::Declared(d)
    }

    /// Mapping fills the table, freeing empties it, and a reference taken
    /// before the free never matches a later mapping at the same address.
    #[test]
    fn the_page_table_is_filled_invalidated_and_generation_tagged() {
        let mut s = space();
        let r = s.map(BASE, 4 * PAGE, Contract::Default).unwrap();
        let (old, e) = s.lookup(BASE + 2 * PAGE + 123).unwrap();
        assert_eq!((e.region, e.index), (r, 2));
        assert!(s.is_current(old));
        s.unmap(r).unwrap();
        assert!(s.lookup(BASE).is_none(), "invalidated on free");
        assert!(!s.is_current(old));
        let r2 = s.map(BASE, 4 * PAGE, Contract::Default).unwrap();
        let (new, _) = s.lookup(BASE + 2 * PAGE).unwrap();
        assert_ne!(r, r2);
        assert_eq!(new.generation, old.generation + 1);
        assert!(!s.is_current(old), "a stale reference stays stale after a remap");
        assert!(s.is_current(new));
        assert_eq!(s.telemetry().count(Kind::RegionMapped), 2);
        assert_eq!(s.telemetry().count(Kind::RegionFreed), 1);
    }

    /// Bad requests are refused by name.
    #[test]
    fn bad_mappings_are_refused() {
        let mut s = space();
        assert_eq!(s.map(BASE + 1, PAGE, Contract::Default), Err(MapError::Unaligned { base: BASE + 1, len: PAGE }));
        assert_eq!(s.map(BASE, 0, Contract::Default), Err(MapError::Empty));
        let r = s.map(BASE, 2 * PAGE, Contract::Default).unwrap();
        assert_eq!(s.map(BASE + PAGE, 2 * PAGE, Contract::Default), Err(MapError::Overlaps { base: BASE + PAGE, region: r }));
        assert_eq!(s.map(BASE - PAGE, 2 * PAGE, Contract::Default), Err(MapError::Overlaps { base: BASE - PAGE, region: r }));
        assert!(s.map(BASE + 2 * PAGE, PAGE, Contract::Default).is_ok(), "adjacent is fine");
        assert_eq!(s.unmap(RegionId(99)), Err(MapError::UnknownRegion(RegionId(99))));
    }

    /// D3: watching an unclassified page never reclassifies it; it logs one
    /// proposal after the configured number of frames.
    #[test]
    fn observed_usage_only_proposes() {
        let mut s = space();
        s.map(BASE, PAGE, Contract::Default).unwrap();
        for _ in 0..10 {
            assert!(s.cpu_wrote(BASE, 16).is_empty(), "Unknown behaves as Mixed: nothing to forward");
            s.end_frame();
        }
        assert_eq!(s.lookup(BASE).unwrap().1.ownership, Ownership::Unknown);
        assert_eq!(s.telemetry().count(Kind::Transition), 0);
        assert_eq!(s.telemetry().count(Kind::Proposal), 1);
        let p = s.telemetry().snapshot().into_iter().find(|r| r.event.kind() == Kind::Proposal).unwrap();
        assert_eq!(p.event, Event::Proposal { page: BASE / PAGE, to: "cpu_owned", frames: 4 });
        // Use from both sides resets it; a later one-sided streak proposes again.
        s.cpu_wrote(BASE, 16);
        s.gpu_writes(BASE, 16);
        s.end_frame();
        for _ in 0..4 {
            s.gpu_writes(BASE, 16);
            s.end_frame();
        }
        assert_eq!(s.telemetry().count(Kind::Proposal), 2);
    }

    /// CPU-owned pages forward exactly the dirty units, split per page.
    #[test]
    fn cpu_writes_on_cpu_owned_pages_are_forwarded_by_unit() {
        let mut s = space();
        s.map(BASE, 2 * PAGE, declared(Declared::CpuWritesGpuReads)).unwrap();
        let a = s.cpu_wrote(BASE + 3 * UNIT, 2 * UNIT);
        assert_eq!(a, vec![Action::Forward { page: BASE / PAGE, units: 0b11000 }]);
        // Across the page boundary: the last unit of page 0, the first of page 1.
        let a = s.cpu_wrote(BASE + PAGE - 1, 2);
        assert_eq!(
            a,
            vec![Action::Forward { page: BASE / PAGE, units: 1 << 15 }, Action::Forward { page: BASE / PAGE + 1, units: 1 }]
        );
    }

    /// A GPU write on a CPU-owned page migrates it first and makes it Mixed.
    #[test]
    fn a_gpu_write_on_a_cpu_owned_page_migrates_it_first() {
        let mut s = space();
        s.map(BASE, PAGE, declared(Declared::UploadHeap)).unwrap();
        assert_eq!(s.gpu_writes(BASE, 4), vec![Action::MigrateBeforeGpuWrite { page: BASE / PAGE }]);
        let e = s.lookup(BASE).unwrap().1;
        assert_eq!((e.ownership, e.home), (Ownership::Mixed, Home::Coherent));
        assert_eq!(s.telemetry().count(Kind::Transition), 1);
    }

    /// D4: a late CPU write on a GPU-owned page is read back and merged
    /// with a snapshot, or the GPU wins and a violation is logged without.
    #[test]
    fn late_cpu_writes_on_gpu_owned_pages_follow_d4() {
        let mut s = space();
        s.map(BASE, 2 * PAGE, declared(Declared::GpuWritesOnly)).unwrap();
        s.set_snapshot(BASE / PAGE, true);
        assert_eq!(s.cpu_wrote(BASE, 1), vec![Action::ReadBackAndMerge { page: BASE / PAGE, units: 1 }]);
        assert_eq!(s.telemetry().count(Kind::Violation), 0);
        assert_eq!(
            s.cpu_wrote(BASE + PAGE, 3 * UNIT),
            vec![Action::ReadBackGpuWins { page: BASE / PAGE + 1, units: 0b111 }]
        );
        assert_eq!(s.telemetry().count(Kind::Violation), 1);
        for p in [BASE, BASE + PAGE] {
            let e = s.lookup(p).unwrap().1;
            assert_eq!((e.ownership, e.home), (Ownership::Mixed, Home::Coherent));
        }
    }

    /// Mixed is sticky: nothing more happens, and only a proof undoes it.
    #[test]
    fn mixed_is_sticky_unless_proven_otherwise() {
        let mut s = space();
        let r = s.map(BASE, PAGE, declared(Declared::UploadHeap)).unwrap();
        s.gpu_writes(BASE, 4);
        let transitions = s.telemetry().count(Kind::Transition);
        assert!(s.cpu_wrote(BASE, 4).is_empty());
        assert!(s.gpu_writes(BASE, 4).is_empty());
        assert_eq!(s.adopt(r, declared(Declared::CpuWritesGpuReads)).unwrap(), 0, "a declaration does not undo Mixed");
        assert_eq!(s.telemetry().count(Kind::Transition), transitions);
        let proof = Contract::Proven { ownership: Ownership::CpuOwned, cpu_reads_often: false, evidence: "test".into() };
        assert_eq!(s.adopt(r, proof).unwrap(), 1);
        assert_eq!(s.lookup(BASE).unwrap().1.ownership, Ownership::CpuOwned);
    }

    /// An unnarrowed raw-address store reaches the whole region.
    #[test]
    fn a_region_wide_gpu_write_reaches_every_page() {
        let mut s = space();
        let r = s.map(BASE, 3 * PAGE, declared(Declared::CpuWritesGpuReads)).unwrap();
        assert_eq!(s.gpu_writes_region(r).unwrap().len(), 3);
        assert!((0..3).all(|i| s.lookup(BASE + i * PAGE).unwrap().1.ownership == Ownership::Mixed));
    }

    /// Adopting a reviewed proposal classifies unknown pages.
    #[test]
    fn adopting_a_contract_classifies_unknown_pages() {
        let mut s = space();
        let r = s.map(BASE, 2 * PAGE, Contract::Default).unwrap();
        assert_eq!(s.adopt(r, declared(Declared::GpuWritesOnly)).unwrap(), 2);
        let e = s.lookup(BASE).unwrap().1;
        assert_eq!((e.ownership, e.home), (Ownership::GpuOwned, Home::Vram));
        assert_eq!(s.contract(r), Some(&declared(Declared::GpuWritesOnly)));
    }

    /// ReadShared follows its first writer.
    #[test]
    fn read_shared_follows_its_first_writer() {
        let shared = Contract::Proven { ownership: Ownership::ReadShared, cpu_reads_often: false, evidence: "test".into() };
        let mut s = space();
        s.map(BASE, 2 * PAGE, shared).unwrap();
        assert_eq!(s.cpu_wrote(BASE, 8), vec![Action::Forward { page: BASE / PAGE, units: 1 }]);
        assert_eq!(s.lookup(BASE).unwrap().1.ownership, Ownership::CpuOwned);
        assert!(s.gpu_writes(BASE + PAGE, 8).is_empty());
        assert_eq!(s.lookup(BASE + PAGE).unwrap().1.ownership, Ownership::GpuOwned);
    }
}
