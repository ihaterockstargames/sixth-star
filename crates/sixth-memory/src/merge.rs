//! D4: a late CPU write to a GPU-owned page, without blind overwrites.
//!
//! A dirty bit says the CPU wrote *somewhere* in a 4 KiB unit, not which
//! bytes. Copying the CPU's dirty units over the GPU's bytes (the rejected
//! Part 3 §3.2 rule) can destroy bytes the GPU wrote in the same unit. So:
//! 1. the page is read back whole after a fence;
//! 2. with a last-forwarded snapshot, the CPU view is diffed against it
//!    byte by byte: a byte that differs is one the CPU wrote, and it wins;
//!    every other byte keeps the GPU's value ([`merge_with_snapshot`]);
//! 3. without a snapshot, the GPU's bytes are the authority and the CPU
//!    write is a contract violation to investigate ([`gpu_authority`]).
//!
//! The one thing a snapshot diff cannot see is a CPU write of the value the
//! byte already had in the snapshot. The GPU's value stands for such a
//! byte. That is only wrong when the GPU changed the byte too, so every
//! possible case is among the report's `gpu_bytes`; nothing can tell them
//! apart from genuine GPU writes.

/// What a merge found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeReport {
    /// Bytes the CPU wrote (differ from the snapshot): taken from the CPU.
    pub cpu_bytes: usize,
    /// Bytes the GPU wrote (differ from the snapshot, CPU did not change them).
    pub gpu_bytes: usize,
    /// Bytes both changed, to different values: the CPU's write wins.
    pub conflicts: usize,
}

/// Merges one read-back page: `gpu` (read back), `cpu` (the CPU view now)
/// and `snapshot` (what was last forwarded), into `out`. All four slices
/// have the same length.
pub fn merge_with_snapshot(gpu: &[u8], cpu: &[u8], snapshot: &[u8], out: &mut [u8]) -> MergeReport {
    assert!(gpu.len() == cpu.len() && cpu.len() == snapshot.len() && snapshot.len() == out.len());
    let mut r = MergeReport::default();
    for i in 0..out.len() {
        let (g, c, s) = (gpu[i], cpu[i], snapshot[i]);
        if c != s {
            out[i] = c;
            r.cpu_bytes += 1;
            if g != s && g != c {
                r.conflicts += 1;
            }
        } else {
            out[i] = g;
            if g != s {
                r.gpu_bytes += 1;
            }
        }
    }
    r
}

/// Without a snapshot: the GPU's bytes win. Returns how many bytes of the
/// CPU view differ from them (the size of the violation to report).
pub fn gpu_authority(gpu: &[u8], cpu: &[u8], out: &mut [u8]) -> usize {
    assert!(gpu.len() == cpu.len() && cpu.len() == out.len());
    out.copy_from_slice(gpu);
    gpu.iter().zip(cpu).filter(|(g, c)| g != c).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CPU write in one half of a unit and a GPU write in the other half
    /// both survive: the case the rejected whole-unit overwrite destroys.
    #[test]
    fn cpu_and_gpu_writes_in_one_unit_both_survive() {
        let snapshot = vec![0u8; 4096];
        let mut cpu = snapshot.clone();
        let mut gpu = snapshot.clone();
        cpu[10] = 1;
        gpu[3000] = 2;
        let mut out = vec![0xFFu8; 4096];
        let r = merge_with_snapshot(&gpu, &cpu, &snapshot, &mut out);
        assert_eq!((out[10], out[3000]), (1, 2));
        assert_eq!(r, MergeReport { cpu_bytes: 1, gpu_bytes: 1, conflicts: 0 });
        // The rejected rule: the CPU's dirty unit copied whole over the GPU's page.
        let blind = cpu.clone();
        assert_eq!(blind[3000], 0, "a whole-unit overwrite loses the GPU's byte");
    }

    /// Every byte of the result follows the rule, on random pages.
    #[test]
    fn every_byte_follows_the_rule() {
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..64 {
            let n = 65536;
            let snapshot: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            let mut cpu = snapshot.clone();
            let mut gpu = snapshot.clone();
            for _ in 0..(next() % 2000) {
                let i = (next() % n as u64) as usize;
                cpu[i] = next() as u8;
            }
            for _ in 0..(next() % 2000) {
                let i = (next() % n as u64) as usize;
                gpu[i] = next() as u8;
            }
            let mut out = vec![0u8; n];
            let r = merge_with_snapshot(&gpu, &cpu, &snapshot, &mut out);
            let mut want = MergeReport::default();
            for i in 0..n {
                let expect = if cpu[i] != snapshot[i] { cpu[i] } else { gpu[i] };
                assert_eq!(out[i], expect, "byte {i}");
                want.cpu_bytes += usize::from(cpu[i] != snapshot[i]);
                want.gpu_bytes += usize::from(cpu[i] == snapshot[i] && gpu[i] != snapshot[i]);
                want.conflicts += usize::from(cpu[i] != snapshot[i] && gpu[i] != snapshot[i] && gpu[i] != cpu[i]);
            }
            assert_eq!(r, want);
        }
    }

    /// Without a snapshot the GPU's page is kept and the difference counted.
    #[test]
    fn without_a_snapshot_the_gpu_wins_and_the_difference_is_counted() {
        let gpu = vec![7u8; 64];
        let mut cpu = gpu.clone();
        cpu[1] = 0;
        cpu[63] = 0;
        let mut out = vec![0u8; 64];
        assert_eq!(gpu_authority(&gpu, &cpu, &mut out), 2);
        assert_eq!(out, gpu);
    }
}
