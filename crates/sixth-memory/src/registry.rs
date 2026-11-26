//! One registry of what title memory holds: resources (buffers, textures)
//! over virtual ranges, and the physical pages behind those ranges
//! (ADR 0002 §4.3).
//!
//! The inherited runtime answers "what is at this address, and what shows
//! it?" in six places (host copies, staging textures, descriptor tables,
//! in-place windows, the address notes and the raw-address directory), and
//! finds mapping changes by polling: texture slots were looked at again
//! after 1, 2, 4… frames to see whether their memory had arrived. Here a
//! change of mapping is an **event**: [`Registry::map`] and
//! [`Registry::unmap`] return exactly the resources whose bytes changed, with
//! new generations, so the views made from them can be remade at once and a
//! view made before the change can never be mistaken for a current one.
//!
//! Aliases: two resources over the same virtual bytes, or two virtual pages
//! over one physical page, are found by [`Registry::aliases`]; a write
//! through one is a write to the others.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

/// The unit of physical mapping (the console's page).
pub const PHYS_PAGE: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Buffer,
    Texture,
}

/// A resource as a view holder sees it: which, and which generation of its
/// bytes. A view keeps the `Version` it was made from; [`Registry::current`]
/// says whether it still is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    pub id: ResourceId,
    pub generation: u32,
}

#[derive(Debug, Clone)]
struct Resource {
    kind: Kind,
    range: Range<u64>,
    generation: u32,
}

/// The registry.
#[derive(Default, Debug)]
pub struct Registry {
    resources: HashMap<ResourceId, Resource>,
    /// Resources by start address (several may start at one address).
    by_start: BTreeMap<u64, BTreeSet<ResourceId>>,
    /// The longest resource, to bound searches by start.
    longest: u64,
    /// Virtual page (address / PHYS_PAGE) → physical page.
    mapped: BTreeMap<u64, u64>,
    /// Physical page → the virtual pages that show it.
    shown_at: HashMap<u64, BTreeSet<u64>>,
    next: u32,
}

/// What a mapping change did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changed {
    /// Resources whose bytes changed, with their new versions: views of the
    /// old versions are stale.
    pub resources: Vec<Version>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Registers a resource over `range` of title memory.
    pub fn create(&mut self, kind: Kind, range: Range<u64>) -> Version {
        self.next += 1;
        let id = ResourceId(self.next);
        self.longest = self.longest.max(range.end - range.start);
        self.by_start.entry(range.start).or_default().insert(id);
        self.resources.insert(id, Resource { kind, range, generation: 0 });
        Version { id, generation: 0 }
    }

    /// The resource is gone; views of it are stale.
    pub fn destroy(&mut self, id: ResourceId) {
        if let Some(r) = self.resources.remove(&id)
            && let Some(set) = self.by_start.get_mut(&r.range.start)
        {
            set.remove(&id);
            if set.is_empty() {
                self.by_start.remove(&r.range.start);
            }
        }
    }

    /// Resources that share a byte with `r`, in address order.
    pub fn overlapping(&self, r: Range<u64>) -> Vec<ResourceId> {
        let from = r.start.saturating_sub(self.longest);
        let mut out: Vec<ResourceId> = self
            .by_start
            .range(from..r.end)
            .flat_map(|(_, ids)| ids.iter().copied())
            .filter(|id| {
                let x = &self.resources[id].range;
                x.start < r.end && x.end > r.start
            })
            .collect();
        out.sort_by_key(|id| (self.resources[id].range.start, *id));
        out
    }

    /// The resource that covers all of `r` (the smallest if several do):
    /// what a view of those bytes is made on.
    pub fn resolve(&self, r: Range<u64>) -> Option<Version> {
        self.overlapping(r.clone())
            .into_iter()
            .filter(|id| {
                let x = &self.resources[id].range;
                x.start <= r.start && x.end >= r.end
            })
            .min_by_key(|id| self.resources[id].range.end - self.resources[id].range.start)
            .map(|id| self.version(id).expect("listed"))
    }

    pub fn version(&self, id: ResourceId) -> Option<Version> {
        self.resources.get(&id).map(|r| Version { id, generation: r.generation })
    }

    pub fn kind(&self, id: ResourceId) -> Option<Kind> {
        self.resources.get(&id).map(|r| r.kind)
    }

    /// Whether a view made from `v` still shows the resource's bytes.
    pub fn current(&self, v: Version) -> bool {
        self.resources.get(&v.id).is_some_and(|r| r.generation == v.generation)
    }

    /// Maps physical pages `phys` at virtual `at` (one per [`PHYS_PAGE`]), in
    /// place of what was there.
    pub fn map(&mut self, at: u64, phys: &[u64]) -> Changed {
        let range = at..at + phys.len() as u64 * PHYS_PAGE;
        for (i, &p) in phys.iter().enumerate() {
            let page = at / PHYS_PAGE + i as u64;
            if let Some(old) = self.mapped.insert(page, p) {
                self.unshow(old, page);
            }
            self.shown_at.entry(p).or_default().insert(page);
        }
        self.bump(range)
    }

    /// Unmaps `len` bytes at virtual `at`.
    pub fn unmap(&mut self, at: u64, len: u64) -> Changed {
        let pages = at / PHYS_PAGE..(at + len).div_ceil(PHYS_PAGE);
        for page in pages {
            if let Some(old) = self.mapped.remove(&page) {
                self.unshow(old, page);
            }
        }
        self.bump(at..at + len)
    }

    /// Title memory `r` now holds other bytes without a mapping change (a
    /// free and a new allocation there, a decommit): the same as a remap
    /// for the views over it.
    pub fn replaced(&mut self, r: Range<u64>) -> Changed {
        self.bump(r)
    }

    fn unshow(&mut self, phys: u64, page: u64) {
        if let Some(set) = self.shown_at.get_mut(&phys) {
            set.remove(&page);
            if set.is_empty() {
                self.shown_at.remove(&phys);
            }
        }
    }

    fn bump(&mut self, r: Range<u64>) -> Changed {
        let ids = self.overlapping(r);
        let mut out = Changed::default();
        for id in ids {
            let res = self.resources.get_mut(&id).expect("listed");
            res.generation += 1;
            out.resources.push(Version { id, generation: res.generation });
        }
        out
    }

    /// The physical page behind virtual `addr`, if mapped by [`Registry::map`].
    pub fn physical(&self, addr: u64) -> Option<u64> {
        self.mapped.get(&(addr / PHYS_PAGE)).copied()
    }

    /// Every virtual byte range that shows the same bytes as `r`: `r` itself,
    /// and the other virtual pages over the same physical pages (cut to the
    /// part of `r` they alias), merged and sorted.
    pub fn aliases_of_range(&self, r: Range<u64>) -> Vec<Range<u64>> {
        let mut out = crate::ranges::RangeSet::new();
        out.insert(r.clone());
        let mut page = r.start / PHYS_PAGE;
        while page * PHYS_PAGE < r.end {
            let lo = r.start.max(page * PHYS_PAGE);
            let hi = r.end.min((page + 1) * PHYS_PAGE);
            if let Some(phys) = self.mapped.get(&page) {
                for &other in self.shown_at.get(phys).into_iter().flatten() {
                    if other != page {
                        let base = other * PHYS_PAGE;
                        out.insert(base + (lo - page * PHYS_PAGE)..base + (hi - page * PHYS_PAGE));
                    }
                }
            }
            page += 1;
        }
        out.iter().collect()
    }

    /// The resources a write to `r` also changes: those over `r` and over
    /// every alias of it.
    pub fn aliases(&self, r: Range<u64>) -> Vec<ResourceId> {
        let mut ids: BTreeSet<ResourceId> = BTreeSet::new();
        for a in self.aliases_of_range(r) {
            ids.extend(self.overlapping(a));
        }
        ids.into_iter().collect()
    }

    pub fn len(&self) -> usize {
        self.resources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

#[cfg(test)]
// Lists of one range are what these tests compare against.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    const P: u64 = PHYS_PAGE;

    /// A view made before a remap is stale after it, and the remap names
    /// exactly the resources over the remapped bytes: no polling.
    #[test]
    fn a_remap_names_exactly_the_resources_it_changes() {
        let mut reg = Registry::new();
        let a = reg.create(Kind::Texture, 0..2 * P);
        let b = reg.create(Kind::Buffer, 4 * P..5 * P);
        let changed = reg.map(P, &[100]);
        assert_eq!(changed.resources, [Version { id: a.id, generation: 1 }]);
        assert!(!reg.current(a), "the view made before the map is stale");
        assert!(reg.current(b), "untouched: still current");
        assert!(reg.current(reg.version(a.id).unwrap()));
        let changed = reg.unmap(0, 8 * P);
        assert_eq!(changed.resources.len(), 2);
    }

    #[test]
    fn resolve_finds_the_smallest_resource_that_covers() {
        let mut reg = Registry::new();
        let heap = reg.create(Kind::Buffer, 0..16 * P);
        let inner = reg.create(Kind::Buffer, 2 * P..3 * P);
        assert_eq!(reg.resolve(2 * P + 10..2 * P + 20), Some(inner));
        assert_eq!(reg.resolve(2 * P + 10..3 * P + 1), Some(heap), "past the inner one");
        assert_eq!(reg.resolve(16 * P..16 * P + 1), None);
        reg.destroy(inner.id);
        assert_eq!(reg.resolve(2 * P + 10..2 * P + 20), Some(heap));
        assert!(!reg.current(inner));
    }

    /// Two resources over the same bytes, and two virtual pages over one
    /// physical page: a write through any is a write to all.
    #[test]
    fn aliases_through_shared_bytes_and_shared_physical_pages() {
        let mut reg = Registry::new();
        let a = reg.create(Kind::Buffer, 0..P);
        let b = reg.create(Kind::Texture, 0..P);
        let c = reg.create(Kind::Buffer, 10 * P..11 * P);
        let d = reg.create(Kind::Buffer, 20 * P..21 * P);
        reg.map(0, &[7]);
        reg.map(10 * P, &[7]);
        reg.map(20 * P, &[8]);
        assert_eq!(reg.aliases(100..200), [a.id, b.id, c.id]);
        assert_eq!(reg.aliases_of_range(100..200), [100..200, 10 * P + 100..10 * P + 200]);
        assert_eq!(reg.aliases(20 * P..20 * P + 1), [d.id]);
        // Remapping page 10 elsewhere ends the alias.
        reg.map(10 * P, &[9]);
        assert_eq!(reg.aliases(100..200), [a.id, b.id]);
        assert_eq!(reg.physical(10 * P + 5), Some(9));
    }

    /// Lookups stay right with many resources of mixed sizes (checked
    /// against a plain scan).
    #[test]
    fn overlapping_matches_a_scan() {
        let mut reg = Registry::new();
        let mut all = Vec::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..300 {
            let start = next(1 << 20);
            let big = next(10) == 0;
            let len = 1 + next(if big { 1 << 18 } else { 4096 });
            let v = reg.create(Kind::Buffer, start..start + len);
            all.push((v.id, start..start + len));
        }
        for _ in 0..300 {
            let a = next(1 << 20);
            let r = a..a + 1 + next(8192);
            let mut want: Vec<ResourceId> =
                all.iter().filter(|(_, x)| x.start < r.end && x.end > r.start).map(|(id, _)| *id).collect();
            want.sort_by_key(|id| (all[(id.0 - 1) as usize].1.start, *id));
            assert_eq!(reg.overlapping(r), want);
        }
    }
}
