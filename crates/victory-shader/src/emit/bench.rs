//! B-38: synthetic compute kernels built with this crate's own DXIL
//! emitter, so the `rawBufferLoad` alignment operand (which HLSL compilers
//! always set to 4) and the load shape can be chosen. Used by `membench`
//! to test whether D3D12 reaches the measured Vulkan buffer-device-address
//! speed when the DXIL promises wider alignment. Independently authored.
//!
//! Bindings (root descriptors, `ROOT_CBV_SPACE`):
//! - `t0`: the data (raw buffer, `n16` elements of 16 bytes);
//! - `u1`: results, one dword per thread;
//! - `t2`: a page table, per 64 KiB page two dwords (unused, byte offset of
//!   the page in the data), for [`Access::Table`];
//! - `t3`: parameters: `n16` (a power of two: random indices are masked,
//!   not divided, so integer division does not dominate), `iters`, `total`.

use super::bitcode::{Attr, Bin, Cast, Term, pred};
use super::shader::Shader;

/// What the kernel reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Every 16-byte element once, coalesced.
    Stream,
    /// `iters` random 16-byte elements per thread.
    Random,
    /// `iters` dependent reads: each element's first dword is the next.
    Chase,
}

/// How the element is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One `rawBufferLoad` of four dwords (mask 0xF).
    Vector,
    /// Four `rawBufferLoad`s of one dword each.
    Scalar,
}

/// How the byte offset is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The offset in the data buffer directly.
    Direct,
    /// Through the page table at `t2` (as the raw-address directory).
    Table,
}

/// One kernel's choices.
#[derive(Debug, Clone, Copy)]
pub struct Kernel {
    pub pattern: Pattern,
    pub shape: Shape,
    pub access: Access,
    /// The alignment operand of every data load: 4, 8 or 16.
    pub align: u32,
}

/// The kernel's DXIL container (compute, 256 threads per group).
pub fn kernel(k: &Kernel) -> Vec<u8> {
    let mut s = Shader::new(false);
    s.threads = Some([256, 1, 1]);
    let entry = s.entry;
    let i32t = s.t.i32;
    let res = s.t.res_ret;
    let data = s.root_srv_handle(0);
    let out = s.root_uav_handle(1);
    let table = s.root_srv_handle(2);
    let params = s.root_srv_handle(3);
    let iu = s.m.undef(i32t);
    let k4 = s.m.i32(4);
    // Parameters.
    let zero = s.m.i32(0);
    let m7 = s.m.i8(7);
    let p = s.op(entry, "rawBufferLoad.f32", Attr::ReadOnly, res, 139, &[params, zero, iu, m7, k4]);
    let as_int = |s: &mut Shader, b: usize, v| s.m.cast(b, Cast::Bitcast, v, s.t.i32);
    let n16 = s.m.extract(entry, p, 0);
    let n16 = as_int(&mut s, entry, n16);
    let iters = s.m.extract(entry, p, 1);
    let iters = as_int(&mut s, entry, iters);
    let total = s.m.extract(entry, p, 2);
    let total = as_int(&mut s, entry, total);
    let c0 = s.m.i32(0);
    let id = s.op(entry, "threadId.i32", Attr::ReadNone, i32t, 93, &[c0]);
    let align = s.m.i32(k.align);
    let k16 = s.m.i32(16);

    // The byte offset of element `e`, directly or through the table.
    let offset = |s: &mut Shader, b: usize, e| {
        let byte = s.m.bin(b, Bin::Mul, e, k16);
        match k.access {
            Access::Direct => byte,
            Access::Table => {
                let sh = s.m.i32(16);
                let page = s.m.bin(b, Bin::LShr, byte, sh);
                let k8 = s.m.i32(8);
                let at = s.m.bin(b, Bin::Mul, page, k8);
                let both = s.m.i8(3);
                let k4 = s.m.i32(4);
                let iu = s.m.undef(s.t.i32);
                let entry = s.op(b, "rawBufferLoad.f32", Attr::ReadOnly, s.t.res_ret, 139, &[table, at, iu, both, k4]);
                let base = s.m.extract(b, entry, 1);
                let base = s.m.cast(b, Cast::Bitcast, base, s.t.i32);
                let mask = s.m.i32(0xFFFF);
                let within = s.m.bin(b, Bin::And, byte, mask);
                s.m.bin(b, Bin::Add, base, within)
            }
        }
    };
    // Four dwords at byte `off` (or the first only, for `first`).
    let load = |s: &mut Shader, b: usize, off, first: bool| -> Vec<super::bitcode::V> {
        let iu = s.m.undef(s.t.i32);
        match (k.shape, first) {
            (Shape::Vector, false) => {
                let m = s.m.i8(15);
                let r = s.op(b, "rawBufferLoad.f32", Attr::ReadOnly, s.t.res_ret, 139, &[data, off, iu, m, align]);
                (0..4)
                    .map(|c| {
                        let x = s.m.extract(b, r, c);
                        s.m.cast(b, Cast::Bitcast, x, s.t.i32)
                    })
                    .collect()
            }
            (_, first) => {
                let n = if first { 1 } else { 4 };
                (0..n)
                    .map(|c| {
                        let d = s.m.i32(4 * c);
                        let at = s.m.bin(b, Bin::Add, off, d);
                        let m = s.m.i8(1);
                        // A dword at a 16-byte element's start keeps the
                        // element's alignment; the others are 4-aligned.
                        let a = if c == 0 { align } else { s.m.i32(4) };
                        let r = s.op(b, "rawBufferLoad.f32", Attr::ReadOnly, s.t.res_ret, 139, &[data, at, iu, m, a]);
                        let x = s.m.extract(b, r, 0);
                        s.m.cast(b, Cast::Bitcast, x, s.t.i32)
                    })
                    .collect()
            }
        }
    };

    // The loop: element index `e`, accumulators, counter.
    let (head, body, exit) = (s.m.new_block(), s.m.new_block(), s.m.new_block());
    // Start values.
    let (e0, step) = match k.pattern {
        Pattern::Stream => (id, total),
        Pattern::Random => {
            let a = s.m.i32(2_654_435_761);
            let one = s.m.i32(1);
            let x = s.m.bin(entry, Bin::Mul, id, a);
            (s.m.bin(entry, Bin::Add, x, one), zero)
        }
        Pattern::Chase => {
            let p = s.m.i32(7919);
            let x = s.m.bin(entry, Bin::Mul, id, p);
            (s.m.bin(entry, Bin::URem, x, n16), zero)
        }
    };
    s.m.terminate(entry, Term::Br(head));
    let ei = s.m.phi(head, i32t);
    let ki = s.m.phi(head, i32t);
    let acc: Vec<_> = (0..4).map(|_| s.m.phi(head, i32t)).collect();
    let go = match k.pattern {
        Pattern::Stream => s.m.cmp(head, pred::IULT, ei, n16),
        _ => s.m.cmp(head, pred::IULT, ki, iters),
    };
    s.m.terminate(head, Term::CondBr(go, body, exit));
    // Body.
    let (element, e_next) = match k.pattern {
        Pattern::Stream => {
            let next = s.m.bin(body, Bin::Add, ei, step);
            (ei, next)
        }
        Pattern::Random => {
            let a = s.m.i32(1_664_525);
            let c = s.m.i32(1_013_904_223);
            let x = s.m.bin(body, Bin::Mul, ei, a);
            let x = s.m.bin(body, Bin::Add, x, c);
            let one = s.m.i32(1);
            let mask = s.m.bin(body, Bin::Sub, n16, one);
            let e = s.m.bin(body, Bin::And, x, mask);
            (e, x)
        }
        Pattern::Chase => (ei, ei),
    };
    let off = offset(&mut s, body, element);
    let mut acc_next = Vec::new();
    let e_next = if k.pattern == Pattern::Chase {
        let v = load(&mut s, body, off, true);
        for (i, &a) in acc.iter().enumerate() {
            acc_next.push(if i == 0 { v[0] } else { a });
        }
        v[0]
    } else {
        let v = load(&mut s, body, off, false);
        for (a, x) in acc.iter().zip(&v) {
            acc_next.push(s.m.bin(body, Bin::Add, *a, *x));
        }
        e_next
    };
    let one = s.m.i32(1);
    let k_next = s.m.bin(body, Bin::Add, ki, one);
    s.m.terminate(body, Term::Br(head));
    s.m.set_phi(ei, vec![(e0, entry), (e_next, body)]);
    s.m.set_phi(ki, vec![(zero, entry), (k_next, body)]);
    for (a, n) in acc.iter().zip(&acc_next) {
        s.m.set_phi(*a, vec![(zero, entry), (*n, body)]);
    }
    // Exit: the result (chase: the last element; else the xor of lanes).
    let r = if k.pattern == Pattern::Chase {
        acc[0]
    } else {
        let x = s.m.bin(exit, Bin::Xor, acc[0], acc[1]);
        let y = s.m.bin(exit, Bin::Xor, acc[2], acc[3]);
        s.m.bin(exit, Bin::Xor, x, y)
    };
    let at = s.m.bin(exit, Bin::Mul, id, k4);
    let void = s.t.void;
    let m1 = s.m.i8(1);
    s.op(exit, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &[out, at, iu, r, iu, iu, iu, m1, k4]);
    s.m.terminate(exit, Term::Ret);
    s.finish()
}

/// What a kernel writes, computed on the CPU from the same data: the
/// reference every GPU result is checked against.
pub fn reference(pattern: Pattern, data: &[u8], iters: u32, total: u32) -> Vec<u32> {
    let n16 = (data.len() / 16) as u32;
    let word = |e: u32, c: u32| -> u32 {
        let o = (e * 16 + 4 * c) as usize;
        u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
    };
    (0..total)
        .map(|id| match pattern {
            Pattern::Stream => {
                let mut acc = [0u32; 4];
                let mut e = id;
                while e < n16 {
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a = a.wrapping_add(word(e, c as u32));
                    }
                    e += total;
                }
                acc[0] ^ acc[1] ^ acc[2] ^ acc[3]
            }
            Pattern::Random => {
                let mut acc = [0u32; 4];
                let mut x = id.wrapping_mul(2_654_435_761).wrapping_add(1);
                for _ in 0..iters {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let e = x & (n16 - 1);
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a = a.wrapping_add(word(e, c as u32));
                    }
                }
                acc[0] ^ acc[1] ^ acc[2] ^ acc[3]
            }
            Pattern::Chase => {
                let mut p = id.wrapping_mul(7919) % n16;
                for _ in 0..iters {
                    p = word(p, 0);
                }
                p
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant builds and, where the validator is installed, the
    /// module validates (lowering and validation only: no GPU here).
    #[test]
    fn every_bench_kernel_builds_and_validates() {
        for pattern in [Pattern::Stream, Pattern::Random, Pattern::Chase] {
            for shape in [Shape::Vector, Shape::Scalar] {
                for access in [Access::Direct, Access::Table] {
                    for align in [4, 8, 16] {
                        let dxil = kernel(&Kernel { pattern, shape, access, align });
                        #[cfg(windows)]
                        if let Ok(mut v) = super::super::validator::Validator::load(super::super::validator::Validator::default_path().as_deref()) {
                            v.validate(&mut dxil.clone()).unwrap_or_else(|e| panic!("{pattern:?} {shape:?} {access:?} {align}: {e}"));
                        }
                    }
                }
            }
        }
    }

    /// The CPU reference follows the chain: a 4-element cycle 0→2→3→1→0.
    #[test]
    fn the_reference_follows_the_chain() {
        let mut data = vec![0u8; 64];
        for (e, next) in [(0u32, 2u32), (1, 0), (2, 3), (3, 1)] {
            data[(e * 16) as usize..(e * 16 + 4) as usize].copy_from_slice(&next.to_le_bytes());
        }
        // Thread 0 starts at 0; two steps: 0 → 2 → 3.
        assert_eq!(reference(Pattern::Chase, &data, 2, 1), vec![3]);
    }
}
