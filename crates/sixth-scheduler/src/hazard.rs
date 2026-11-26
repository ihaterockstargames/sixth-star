//! Barriers and dependencies from declared uses.
//!
//! Each command declares what it uses: a resource, which part of it
//! (subresources of a texture, bytes of a buffer) and how ([`Access`]). The
//! [`Tracker`] keeps, per resource and part, the last access and the last
//! writer, and answers for a new command: which transitions it needs, which
//! unordered-access barriers (write after write in the same state), and
//! which earlier commands it must follow.
//!
//! Parts are tracked as ranges, so two commands that write different bytes
//! of one buffer (or different mips of one texture) do not wait for each
//! other: the inherited path tracked whole buffers and serialised them.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

/// How a command touches a part of a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// Read by shaders.
    ShaderRead,
    /// Read as vertex/index data or indirect arguments.
    InputRead,
    /// The source of a copy.
    CopySource,
    /// The destination of a copy.
    CopyDest,
    /// Read and written by shaders (unordered access).
    Unordered,
    /// A render target.
    RenderTarget,
    /// Depth written.
    DepthWrite,
    /// Depth read only.
    DepthRead,
}

impl Access {
    pub fn writes(self) -> bool {
        matches!(self, Access::CopyDest | Access::Unordered | Access::RenderTarget | Access::DepthWrite)
    }

    /// Reads that can share one state without a transition between them.
    fn compatible(self, other: Access) -> bool {
        self == other && !self.writes()
            || matches!((self, other), (Access::ShaderRead, Access::InputRead) | (Access::InputRead, Access::ShaderRead))
    }
}

/// One declared use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub resource: u32,
    /// Bytes of a buffer, or subresource indices of a texture.
    pub part: Range<u64>,
    pub access: Access,
}

/// What must happen before a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Barrier {
    /// The part moves from one access state to another.
    Transition { resource: u32, part: Range<u64>, from: Access, to: Access },
    /// Unordered writes, then unordered access again: the earlier writes
    /// must be finished and visible.
    Unordered { resource: u32 },
}

/// The state of one tracked part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    access: Access,
    /// The last command that wrote this part, if any.
    writer: Option<usize>,
    /// The last command that read it since that write.
    reader: Option<usize>,
}

/// The tracker for one queue's commands in order.
#[derive(Default, Debug)]
pub struct Tracker {
    /// resource → part start → (part end, state); parts are disjoint.
    parts: HashMap<u32, BTreeMap<u64, (u64, State)>>,
    /// Barriers and dependencies of every command so far.
    pub dependencies: Vec<Vec<usize>>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// Splits the tracked parts of `resource` at the ends of `r`, so that the
    /// parts inside `r` can be changed alone.
    fn split(map: &mut BTreeMap<u64, (u64, State)>, r: &Range<u64>) {
        for cut in [r.start, r.end] {
            if let Some((&s, &(e, st))) = map.range(..cut).next_back()
                && e > cut
            {
                map.insert(s, (cut, st));
                map.insert(cut, (e, st));
            }
        }
    }

    /// Records command `cmd` (numbered in order) with `uses`; returns the
    /// barriers to put before it. Its dependencies (earlier commands whose
    /// results it must see, or whose reads it must not overwrite) are kept
    /// in [`Tracker::dependencies`].
    pub fn record(&mut self, cmd: usize, uses: &[Use]) -> Vec<Barrier> {
        let mut barriers = Vec::new();
        let mut deps = std::collections::BTreeSet::new();
        for u in uses {
            if u.part.start >= u.part.end {
                continue;
            }
            let map = self.parts.entry(u.resource).or_default();
            Self::split(map, &u.part);
            // Parts inside the range: change each; gaps never used are new.
            let mut at = u.part.start;
            let inside: Vec<(u64, u64, State)> =
                map.range(u.part.start..u.part.end).map(|(&s, &(e, st))| (s, e, st)).collect();
            let mut fresh = Vec::new();
            for (s, e, st) in inside {
                if s > at {
                    fresh.push(at..s);
                }
                at = e;
                // Dependencies.
                if let Some(w) = st.writer {
                    deps.insert(w); // RAW or WAW
                }
                if u.access.writes()
                    && let Some(r) = st.reader
                {
                    deps.insert(r); // WAR
                }
                // Barriers.
                if st.access != u.access && !st.access.compatible(u.access) {
                    barriers.push(Barrier::Transition { resource: u.resource, part: s..e, from: st.access, to: u.access });
                } else if st.access == Access::Unordered && u.access == Access::Unordered && st.writer.is_some() {
                    barriers.push(Barrier::Unordered { resource: u.resource });
                }
                let next = if u.access.writes() {
                    State { access: u.access, writer: Some(cmd), reader: None }
                } else {
                    State { access: u.access, writer: st.writer, reader: Some(cmd) }
                };
                map.insert(s, (e, next));
            }
            if at < u.part.end {
                fresh.push(at..u.part.end);
            }
            for r in fresh {
                let st = if u.access.writes() {
                    State { access: u.access, writer: Some(cmd), reader: None }
                } else {
                    State { access: u.access, writer: None, reader: Some(cmd) }
                };
                map.insert(r.start, (r.end, st));
            }
            // Neighbouring parts in the same state become one again.
            Self::merge(map, &u.part);
        }
        // One unordered barrier per resource is enough.
        barriers.dedup();
        deps.remove(&cmd);
        if self.dependencies.len() <= cmd {
            self.dependencies.resize(cmd + 1, Vec::new());
        }
        self.dependencies[cmd] = deps.into_iter().collect();
        barriers
    }

    fn merge(map: &mut BTreeMap<u64, (u64, State)>, r: &Range<u64>) {
        let keys: Vec<u64> = map.range(..=r.end).rev().take_while(|(_, (e, _))| *e >= r.start).map(|(&s, _)| s).collect();
        let mut keys = keys;
        keys.sort_unstable();
        let mut i = 0;
        while i + 1 < keys.len() {
            let (a, b) = (keys[i], keys[i + 1]);
            let (ea, sa) = map[&a];
            let (eb, sb) = map[&b];
            if ea == b && sa == sb {
                map.remove(&b);
                map.insert(a, (eb, sa));
                keys.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }

    /// The tracked parts of a resource (for tests and reports).
    pub fn parts(&self, resource: u32) -> Vec<(Range<u64>, Access)> {
        self.parts
            .get(&resource)
            .map(|m| m.iter().map(|(&s, &(e, st))| (s..e, st.access)).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
// Lists of one range are what these tests compare against.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use Access::*;

    fn u(resource: u32, part: Range<u64>, access: Access) -> Use {
        Use { resource, part, access }
    }

    #[test]
    fn a_copy_then_a_read_needs_one_transition_and_one_dependency() {
        let mut t = Tracker::new();
        assert!(t.record(0, &[u(1, 0..256, CopyDest)]).is_empty(), "first use: the prelude's state");
        let b = t.record(1, &[u(1, 0..256, ShaderRead)]);
        assert_eq!(b, [Barrier::Transition { resource: 1, part: 0..256, from: CopyDest, to: ShaderRead }]);
        assert_eq!(t.dependencies[1], [0]);
        // Reading again: nothing.
        assert!(t.record(2, &[u(1, 0..256, ShaderRead)]).is_empty());
        assert_eq!(t.dependencies[2], [0], "still the writer's result");
    }

    /// Writes to different bytes of one buffer do not depend on each other
    /// (the inherited path tracked whole buffers).
    #[test]
    fn disjoint_bytes_do_not_serialise() {
        let mut t = Tracker::new();
        t.record(0, &[u(1, 0..100, Unordered)]);
        let b = t.record(1, &[u(1, 100..200, Unordered)]);
        assert!(b.is_empty());
        assert!(t.dependencies[1].is_empty());
        // Overlapping bytes do: write after write, an unordered barrier.
        let b = t.record(2, &[u(1, 50..150, Unordered)]);
        assert_eq!(b, [Barrier::Unordered { resource: 1 }]);
        assert_eq!(t.dependencies[2], [0, 1]);
    }

    #[test]
    fn write_after_read_follows_the_reader() {
        let mut t = Tracker::new();
        t.record(0, &[u(2, 0..1, RenderTarget)]);
        t.record(1, &[u(2, 0..1, ShaderRead)]);
        t.record(2, &[u(2, 0..1, ShaderRead)]);
        let b = t.record(3, &[u(2, 0..1, RenderTarget)]);
        assert_eq!(b, [Barrier::Transition { resource: 2, part: 0..1, from: ShaderRead, to: RenderTarget }]);
        assert_eq!(t.dependencies[3], [0, 2], "the last writer and the last reader since it");
    }

    /// Mips of one texture are parts of their own: rendering into mip 1
    /// while sampling mip 0 needs transitions of the right mips only.
    #[test]
    fn subresources_are_tracked_apart() {
        let mut t = Tracker::new();
        t.record(0, &[u(3, 0..4, CopyDest)]);
        let b = t.record(1, &[u(3, 0..1, ShaderRead), u(3, 1..2, RenderTarget)]);
        assert_eq!(
            b,
            [
                Barrier::Transition { resource: 3, part: 0..1, from: CopyDest, to: ShaderRead },
                Barrier::Transition { resource: 3, part: 1..2, from: CopyDest, to: RenderTarget },
            ]
        );
        assert_eq!(t.parts(3), [(0..1, ShaderRead), (1..2, RenderTarget), (2..4, CopyDest)]);
    }

    #[test]
    fn shader_and_input_reads_share_a_state() {
        let mut t = Tracker::new();
        t.record(0, &[u(4, 0..64, ShaderRead)]);
        assert!(t.record(1, &[u(4, 0..64, InputRead)]).is_empty());
    }

    /// Parts split by partial uses merge again when they end in one state
    /// (same access, writer and reader); parts read by different commands
    /// stay apart, so a later write still follows each of its readers.
    #[test]
    fn parts_merge_back_only_when_equal() {
        let mut t = Tracker::new();
        t.record(0, &[u(5, 0..300, CopyDest)]);
        t.record(1, &[u(5, 100..200, ShaderRead)]);
        assert_eq!(t.parts(5).len(), 3);
        t.record(2, &[u(5, 0..100, ShaderRead), u(5, 200..300, ShaderRead)]);
        assert_eq!(t.parts(5), [(0..100, ShaderRead), (100..200, ShaderRead), (200..300, ShaderRead)]);
        t.record(3, &[u(5, 0..300, CopyDest)]);
        assert_eq!(t.dependencies[3], [0, 1, 2], "the writer and both readers");
        assert_eq!(t.parts(5), [(0..300, CopyDest)]);
    }
}
