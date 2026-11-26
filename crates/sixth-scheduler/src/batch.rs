//! Uploads gathered into few copies, each before its first reader.
//!
//! The inherited path brings every host copy up to date at the start of a
//! submission, range by range, whether or not a command of it reads that
//! range, and records one copy per range. Here each pending upload goes
//! just before the first command that reads its bytes (so the commands
//! before it can already run), adjacent bytes of one destination become one
//! copy, and uploads that no command of the submission reads stay pending.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::hazard::Use;

/// A copy to record: into `resource`, `bytes`, before command `before`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub before: usize,
    pub resource: u32,
    pub bytes: Range<u64>,
}

/// Places the pending uploads (`(resource, bytes)`) against the commands'
/// uses, in command order. Returns the copies to record and the uploads
/// that no command reads (left pending for a later submission).
pub fn plan_uploads(pending: &[(u32, Range<u64>)], commands: &[Vec<Use>]) -> (Vec<Upload>, Vec<(u32, Range<u64>)>) {
    let mut out = Vec::new();
    let mut left = Vec::new();
    // Coalesce per resource first: adjacent or overlapping bytes, one copy.
    let mut by_resource: BTreeMap<u32, Vec<Range<u64>>> = BTreeMap::new();
    for (r, b) in pending {
        if b.start < b.end {
            by_resource.entry(*r).or_default().push(b.clone());
        }
    }
    for (resource, mut ranges) in by_resource {
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<u64>> = Vec::new();
        for r in ranges {
            match merged.last_mut() {
                Some(m) if r.start <= m.end => m.end = m.end.max(r.end),
                _ => merged.push(r),
            }
        }
        for bytes in merged {
            let first = commands.iter().position(|uses| {
                uses.iter().any(|u| u.resource == resource && u.part.start < bytes.end && u.part.end > bytes.start)
            });
            match first {
                Some(before) => out.push(Upload { before, resource, bytes }),
                None => left.push((resource, bytes)),
            }
        }
    }
    out.sort_by_key(|u| (u.before, u.resource, u.bytes.start));
    (out, left)
}

#[cfg(test)]
// Lists of one range are what these tests compare against.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::hazard::Access;

    fn reads(resource: u32, part: Range<u64>) -> Vec<Use> {
        vec![Use { resource, part, access: Access::ShaderRead }]
    }

    #[test]
    fn uploads_go_before_their_first_reader_and_coalesce() {
        let pending = [(1, 0..100), (1, 100..200), (1, 500..600), (2, 0..10), (3, 0..10)];
        let commands = vec![reads(9, 0..1), reads(1, 150..160), reads(2, 0..4), reads(1, 550..551)];
        let (uploads, left) = plan_uploads(&pending, &commands);
        assert_eq!(
            uploads,
            [
                Upload { before: 1, resource: 1, bytes: 0..200 },
                Upload { before: 2, resource: 2, bytes: 0..10 },
                Upload { before: 3, resource: 1, bytes: 500..600 },
            ]
        );
        assert_eq!(left, [(3, 0..10)], "no command reads it: it stays pending");
    }

    #[test]
    fn nothing_pending_nothing_recorded() {
        let (u, l) = plan_uploads(&[], &[reads(1, 0..1)]);
        assert!(u.is_empty() && l.is_empty());
    }
}
