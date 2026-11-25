//! Title memory by address: the raw-address directory (`shader.rs`
//! `SLOT_MEMORY`), accesses that cross a 64 KiB page, and the `global_*`
//! and `s_load_*` instructions that read and write title memory through it.
//! Part of [`super::Lower`]; split out of `lower.rs` only to keep files a
//! reviewable size.

use super::*;

impl<'a> Lower<'a> {
    /// `lo`, `hi` plus the instruction's byte offset `off`, with the carry.
    pub(crate) fn address_plus(&mut self, lo: V, hi: V, off: i32) -> (V, V) {
        if off == 0 {
            return (lo, hi);
        }
        let c = self.cur;
        let i32t = self.s.t.i32;
        let k = self.s.m.i32(off as u32);
        let sum = self.s.m.bin(c, Bin::Add, lo, k);
        // Up: a carry when the sum wrapped below `lo`. Down: a borrow when
        // it wrapped above.
        let wrapped = self
            .s
            .m
            .cmp(c, if off > 0 { pred::IULT } else { pred::IUGT }, sum, lo);
        let one = self.s.m.cast(c, Cast::ZExt, wrapped, i32t);
        let hi = self.s.m.bin(c, if off > 0 { Bin::Add } else { Bin::Sub }, hi, one);
        (sum, hi)
    }

    /// Title memory at the 40-bit address `lo`, `hi` (`shader.rs`
    /// `SLOT_MEMORY`): the handle of the raw view of the host copy holding
    /// it and the byte in that view. A page without a copy reads zeros
    /// (and drops writes) through the null view at the first slot, and its
    /// number is left for the host, for lanes that run (`guard`).
    pub(crate) fn memory(&mut self, lo: V, hi: V, guard: Option<V>) -> (V, V) {
        use super::super::shader::{MEMORY_FAULTS, MEMORY_PAGES, SLOT_FAULTS, SLOT_MEMORY, USER_MEMORY_BASE};
        let c = self.cur;
        let (i32t, res) = (self.s.t.i32, self.s.t.res_ret);
        let k = |l: &mut Self, v: u32| l.s.m.i32(v);
        let (k16, k8) = (k(self, 16), k(self, 0xFF));
        let low = self.s.m.bin(c, Bin::LShr, lo, k16);
        let high = self.s.m.bin(c, Bin::And, hi, k8);
        let high = self.s.m.bin(c, Bin::Shl, high, k16);
        let page = self.s.m.bin(c, Bin::Or, low, high);
        // The address's own page (bits 16..39), for the fault record: an
        // address past the directory is reported as itself, not as the
        // clamped last entry.
        // An address at or past 2^40 (any of `hi`'s bits 8..31 set) is not
        // title memory: its page bits would alias a real page, so it goes to
        // the sink and is reported as page 0xFFFF_FFFF.
        let k_hi = k(self, 0xFFFF_FF00);
        let hi_rest = self.s.m.bin(c, Bin::And, hi, k_hi);
        let zero_hi = k(self, 0);
        let above = self.s.m.cmp(c, pred::NE, hi_rest, zero_hi);
        let all_ones = k(self, u32::MAX);
        let raw_page = self.s.m.select(c, above, all_ones, page);
        // Inside the directory whatever the address (lanes that do not run
        // compute one too); its last entry is never filled.
        let last = k(self, MEMORY_PAGES - 1);
        let page = self
            .s
            .op(c, "binary.i32", Attr::ReadNone, i32t, 40, &[page, last]);
        self.s.m.tag(page, Tag::Clamp);
        let page = self.s.m.select(c, above, last, page);
        self.s.m.tag(page, Tag::Clamp);
        let k3 = k(self, 3);
        let at = self.s.m.bin(c, Bin::Shl, page, k3);
        let directory = self.s.root_srv_handle(SLOT_MEMORY);
        let (iu, both, align) = (self.s.m.undef(i32t), self.s.m.i8(3), k(self, 4));
        let entry = self.s.op(
            c,
            "rawBufferLoad.f32",
            Attr::ReadOnly,
            res,
            139,
            &[directory, at, iu, both, align],
        );
        let (copy, start) = (self.s.m.extract(c, entry, 0), self.s.m.extract(c, entry, 1));
        let (copy, start) = (self.as_int(copy), self.as_int(start));
        // A page without a copy: its number, at a place by the number, or
        // at the sink for lanes that do not run and pages that have one.
        let zero = k(self, 0);
        let mut missing = self.s.m.cmp(c, pred::EQ, copy, zero);
        if let Some(g) = guard {
            missing = self.s.m.bin(c, Bin::And, missing, g);
        }
        let places = k(self, MEMORY_FAULTS - 1);
        let place = self.s.m.bin(c, Bin::URem, raw_page, places);
        let one = k(self, 1);
        let place = self.s.m.bin(c, Bin::Add, place, one);
        let k2 = k(self, 2);
        let place = self.s.m.bin(c, Bin::Shl, place, k2);
        let place = self.s.m.select(c, missing, place, zero);
        let faults = self.s.root_uav_handle(SLOT_FAULTS);
        let void = self.s.t.void;
        let mask = self.s.m.i8(1);
        self.s.op(
            c,
            "rawBufferStore.i32",
            Attr::NoUnwind,
            void,
            140,
            &[faults, place, iu, raw_page, iu, iu, iu, mask, align],
        );
        let base = self.host_word(RootType::Other(USER_MEMORY_BASE), 0, 0);
        let slot = self.s.m.bin(c, Bin::Add, base, copy);
        let h = self.s.heap_handle(c, slot, false, true, (11 | 0x1000, 0));
        let k64k = k(self, 0xFFFF);
        let within = self.s.m.bin(c, Bin::And, lo, k64k);
        let byte = self.s.m.bin(c, Bin::Add, start, within);
        (h, byte)
    }

    /// For an access of `n` dwords at `lo`: whether this lane's last dword
    /// lies past the 64 KiB page of its first (`(lo & 0xFFFF) + 4(n−1) ≥
    /// 0x10000`), and per dword `k` in `0..n` whether that dword does. Such
    /// dwords are found through their own page's directory entry: the next
    /// page may be in another copy, or in none.
    pub(crate) fn page_crossings(&mut self, lo: V, n: u8) -> (V, Vec<V>) {
        let c = self.cur;
        let k64k = self.s.m.i32(0xFFFF);
        let within = self.s.m.bin(c, Bin::And, lo, k64k);
        let page_end = self.s.m.i32(0x1_0000);
        let mut past = Vec::new();
        for k in 0..n {
            let four_k = self.s.m.i32(4 * u32::from(k));
            let at = self.s.m.bin(c, Bin::Add, within, four_k);
            past.push(self.s.m.cmp(c, pred::IUGE, at, page_end));
        }
        (past[usize::from(n - 1)], past)
    }

    /// `n` dwords (1 to 4) of title memory at `lo`, `hi`, as integers.
    /// Lanes whose access stays in one page take one vector load (as
    /// before). Lanes that cross the page's end (a branch only they enter)
    /// take each dword alone, at its own page's entry: no load there
    /// starts outside its copy, and the common path pays nothing more.
    pub(crate) fn memory_load(&mut self, lo: V, hi: V, guard: Option<V>, n: u8) -> Vec<V> {
        let (i32t, res) = (self.s.t.i32, self.s.t.res_ret);
        let (iu, align) = (self.s.m.undef(i32t), self.s.m.i32(4));
        let (h, byte) = self.memory(lo, hi, guard);
        let c = self.cur;
        let mask = self.s.m.i8((1u8 << n) - 1);
        let r = self.s.op(c, "rawBufferLoad.f32", Attr::ReadOnly, res, 139, &[h, byte, iu, mask, align]);
        let fast: Vec<V> = (0..n)
            .map(|k| {
                let x = self.s.m.extract(c, r, u32::from(k));
                self.as_int(x)
            })
            .collect();
        if n == 1 {
            return fast;
        }
        let (crosses, past) = self.page_crossings(lo, n);
        let at = self.begin_guarded(Some(crosses));
        let (lo2, hi2) = self.address_plus(lo, hi, 4 * i32::from(n - 1));
        let (h2, byte2) = self.memory(lo2, hi2, guard);
        let c = self.cur;
        let one = self.s.m.i8(1);
        let mut slow = Vec::new();
        for k in 0..n {
            let four_k = self.s.m.i32(4 * u32::from(k));
            let here = self.s.m.bin(c, Bin::Add, byte, four_k);
            let a = self.s.op(c, "rawBufferLoad.f32", Attr::ReadOnly, res, 139, &[h, here, iu, one, align]);
            let back = self.s.m.i32(4 * u32::from(n - 1 - k));
            let there = self.s.m.bin(c, Bin::Sub, byte2, back);
            let b = self.s.op(c, "rawBufferLoad.f32", Attr::ReadOnly, res, 139, &[h2, there, iu, one, align]);
            let (a, b) = (self.s.m.extract(c, a, 0), self.s.m.extract(c, b, 0));
            let (a, b) = (self.as_int(a), self.as_int(b));
            slow.push(self.s.m.select(c, past[usize::from(k)], b, a));
        }
        let pairs: Vec<(V, V)> = slow.into_iter().zip(fast).collect();
        self.end_guarded(at, &pairs)
    }

    /// Stores `vals` (1 to 4 dwords) at title memory `lo`, `hi`, for lanes
    /// that run (`guard`). Lanes inside one page: one vector store, as
    /// before. Lanes that cross the page's end: each dword alone, at the
    /// entry of the page it falls in (nothing is written into a copy that
    /// is not that page's).
    pub(crate) fn memory_store(&mut self, lo: V, hi: V, guard: Option<V>, vals: &[V]) {
        let n = vals.len() as u8;
        let (i32t, void) = (self.s.t.i32, self.s.t.void);
        let (iu, align) = (self.s.m.undef(i32t), self.s.m.i32(4));
        let (h, byte) = self.memory(lo, hi, guard);
        let vector = |l: &mut Self| {
            let mut args = vec![h, byte, iu];
            args.extend(vals);
            args.resize(3 + 4, iu);
            args.push(l.s.m.i8((1u8 << n) - 1));
            args.push(align);
            l.s.op(l.cur, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &args);
        };
        if n == 1 {
            let at = self.begin_guarded(guard);
            vector(self);
            self.end_guarded(at, &[]);
            return;
        }
        let (crosses, past) = self.page_crossings(lo, n);
        let c = self.cur;
        let t = self.s.m.bool(true);
        let stays = self.s.m.bin(c, Bin::Xor, crosses, t);
        let and_guard = |l: &mut Self, x: V| match guard {
            Some(g) => {
                let c = l.cur;
                l.s.m.bin(c, Bin::And, x, g)
            }
            None => x,
        };
        let fast_when = and_guard(self, stays);
        let at = self.begin_guarded(Some(fast_when));
        vector(self);
        self.end_guarded(at, &[]);
        let slow_when = and_guard(self, crosses);
        let at = self.begin_guarded(Some(slow_when));
        let (lo2, hi2) = self.address_plus(lo, hi, 4 * i32::from(n - 1));
        let (h2, byte2) = self.memory(lo2, hi2, guard);
        let one = self.s.m.i8(1);
        for k in 0..n {
            let c = self.cur;
            let four_k = self.s.m.i32(4 * u32::from(k));
            let here = self.s.m.bin(c, Bin::Add, byte, four_k);
            let back = self.s.m.i32(4 * u32::from(n - 1 - k));
            let there = self.s.m.bin(c, Bin::Sub, byte2, back);
            let next = past[usize::from(k)];
            let inside = self.s.m.bin(c, Bin::Xor, next, t);
            let v = vals[usize::from(k)];
            for (when, handle, off) in [(next, h2, there), (inside, h, here)] {
                let at = self.begin_guarded(Some(when));
                let args = [handle, off, iu, v, iu, iu, iu, one, align];
                self.s
                    .op(self.cur, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &args);
                self.end_guarded(at, &[]);
            }
        }
        self.end_guarded(at, &[]);
    }

    /// `global_load_*`, `global_store_*` and `global_atomic_*` on title
    /// memory by address (`resource::MEMORY`): the address is the VGPR
    /// pair, each lane its own.
    pub(crate) fn memory_flat(&mut self, inst: &Inst, b: usize, i: usize) -> Result<(), Why> {
        let Fields::Flat {
            dst,
            addr,
            data,
            saddr,
            offset,
            lds,
            cache,
        } = &inst.fields
        else {
            unreachable!()
        };
        if *lds || !matches!(saddr, None | Some(Operand::Scalar { code: NULL, .. })) {
            return Err("a global address with a scalar base");
        }
        let Some(Operand::Vector { reg: areg, .. }) = *addr else {
            return Err("a global address not in VGPRs");
        };
        let word = |l: &mut Self, k: u8| -> Result<V, Why> {
            let v = inst.read(Reg::V(areg + k)).ok_or("a global address")?;
            l.i32_of(v)
        };
        let (lo, hi) = (word(self, 0)?, word(self, 1)?);
        let (lo, hi) = self.address_plus(lo, hi, i32::from(*offset));
        let guard = self.guard(b, i)?;
        let c = self.cur;
        let (i32t, res) = (self.s.t.i32, self.s.t.res_ret);
        let iu = self.s.m.undef(i32t);
        let align = self.s.m.i32(4);
        let name = inst.name();
        let count = |rest: &str| match rest {
            "dword" => Ok(1u8),
            "dwordx2" => Ok(2),
            "dwordx3" => Ok(3),
            "dwordx4" => Ok(4),
            _ => Err(name),
        };
        if let Some(rest) = name.strip_prefix("global_load_") {
            let n = count(rest)?;
            let Some(Operand::Vector { reg, .. }) = *dst else {
                return Err("a global load result");
            };
            let xs = self.memory_load(lo, hi, guard, n);
            for (k, x) in (0..n).zip(xs) {
                self.write(inst, Reg::V(reg + k), x, guard);
            }
            return Ok(());
        }
        if let Some(rest) = name.strip_prefix("global_store_") {
            let n = count(rest)?;
            let d = data.as_ref().ok_or("global store data")?;
            let vals = self.vgprs(inst, d, n)?;
            self.memory_store(lo, hi, guard, &vals);
            return Ok(());
        }
        let Some(op) = name.strip_prefix("global_atomic_") else {
            return Err(name);
        };
        let d = data.as_ref().ok_or("global atomic data")?;
        let v = self.vgprs(inst, d, 1)?[0];
        // With `glc` the value from before comes back, in the result
        // register the instruction names.
        let back = match (*dst, cache.glc) {
            (Some(Operand::Vector { reg, .. }), true) => Some(reg),
            _ => None,
        };
        let old = match back.and_then(|reg| inst.read(Reg::V(reg))) {
            Some(x) => self.i32_of(x)?,
            None => self.s.m.i32(0),
        };
        let (h, byte) = self.memory(lo, hi, guard);
        // DXIL's atomic operations: add 0, and 1, or 2, xor 3, signed
        // minimum 4 and maximum 5, unsigned 6 and 7, exchange 8.
        let simple = match op {
            "add" => Some((0, v)),
            "sub" => {
                let zero = self.s.m.i32(0);
                Some((0, self.s.m.bin(c, Bin::Sub, zero, v)))
            }
            "and" => Some((1, v)),
            "or" => Some((2, v)),
            "xor" => Some((3, v)),
            "smin" => Some((4, v)),
            "smax" => Some((5, v)),
            "umin" => Some((6, v)),
            "umax" => Some((7, v)),
            "swap" => Some((8, v)),
            "fmin" | "fmax" => None,
            _ => return Err(name),
        };
        let at = self.begin_guarded(guard);
        let was = match simple {
            Some((code, x)) => {
                let code = self.s.m.i32(code);
                self.s.op(
                    self.cur,
                    "atomicBinOp.i32",
                    Attr::NoUnwind,
                    i32t,
                    78,
                    &[h, code, byte, iu, iu, x],
                )
            }
            None => {
                // A float minimum or maximum has no atomic operation on
                // the host: the dword is exchanged for the smaller (larger)
                // of itself and the value until no other thread got in
                // between. Each failed pass means another one's succeeded;
                // the limit only keeps a broken card from hanging.
                let head = self.cur;
                let mask = self.s.m.i8(1);
                let first = self.s.op(
                    head,
                    "rawBufferLoad.f32",
                    Attr::ReadOnly,
                    res,
                    139,
                    &[h, byte, iu, mask, align],
                );
                let first = self.s.m.extract(head, first, 0);
                let first = self.as_int(first);
                let (pass, done) = (self.s.m.new_block(), self.s.m.new_block());
                self.s.m.terminate(head, Term::Br(pass));
                self.cur = pass;
                let seen = self.s.m.phi(pass, i32t);
                let n = self.s.m.phi(pass, i32t);
                let (f32t, fv) = (self.s.t.f32, self.as_float(v));
                let fseen = self.as_float(seen);
                // `dx.op.binary`: FMax 35, FMin 36.
                let pick = if op == "fmin" { 36 } else { 35 };
                let wanted = self
                    .s
                    .op(pass, "binary.f32", Attr::ReadNone, f32t, pick, &[fseen, fv]);
                let wanted = self.as_int(wanted);
                let found = self.s.op(
                    pass,
                    "atomicCompareExchange.i32",
                    Attr::NoUnwind,
                    i32t,
                    79,
                    &[h, byte, iu, iu, seen, wanted],
                );
                let one = self.s.m.i32(1);
                let n1 = self.s.m.bin(pass, Bin::Add, n, one);
                let lost = self.s.m.cmp(pass, pred::NE, found, seen);
                let limit = self.s.m.i32(1 << 16);
                let more = self.s.m.cmp(pass, pred::IULT, n1, limit);
                let again = self.s.m.bin(pass, Bin::And, lost, more);
                self.s.m.terminate(pass, Term::CondBr(again, pass, done));
                let zero = self.s.m.i32(0);
                self.s.m.set_phi(seen, vec![(first, head), (found, pass)]);
                self.s.m.set_phi(n, vec![(zero, head), (n1, pass)]);
                self.cur = done;
                found
            }
        };
        let was = self.end_guarded(at, &[(was, old)])[0];
        if let Some(reg) = back {
            self.write(inst, Reg::V(reg), was, None);
        }
        Ok(())
    }

    /// `s_load_dword*` from title memory by address (`resource::MEMORY`):
    /// the SGPR pair plus the instruction's offsets.
    pub(crate) fn memory_smem(&mut self, inst: &Inst) -> Result<(), Why> {
        let Fields::Smem {
            data: Some(Operand::Scalar { code, .. }),
            base: Operand::Scalar { code: base, .. },
            offset,
            soffset,
            ..
        } = inst.fields
        else {
            return Err("a scalar load from memory");
        };
        let count = match inst.name().strip_prefix("s_load_dword") {
            Some("") => 1u8,
            Some("x2") => 2,
            Some("x4") => 4,
            Some("x8") => 8,
            Some("x16") => 16,
            _ => return Err("a scalar memory access by address"),
        };
        let word = |l: &mut Self, k: u8| -> Result<V, Why> {
            let v = inst.read(Reg::S(base + k)).ok_or("an address not in SGPRs")?;
            l.i32_of(v)
        };
        let (mut lo, mut hi) = (word(self, 0)?, word(self, 1)?);
        let c = self.cur;
        let i32t = self.s.t.i32;
        if !matches!(soffset, Operand::Scalar { code: NULL, .. }) {
            // An offset register counts up from the address.
            let so = self.operand(inst, &soffset)?;
            let sum = self.s.m.bin(c, Bin::Add, lo, so);
            let carry = self.s.m.cmp(c, pred::IULT, sum, lo);
            let carry = self.s.m.cast(c, Cast::ZExt, carry, i32t);
            hi = self.s.m.bin(c, Bin::Add, hi, carry);
            lo = sum;
        }
        for first in (0..count).step_by(4) {
            let n = (count - first).min(4);
            let (lo, hi) = self.address_plus(lo, hi, offset + 4 * i32::from(first));
            let xs = self.memory_load(lo, hi, None, n);
            for (k, x) in (0..n).zip(xs) {
                self.write(inst, Reg::S(code + first + k), x, None);
            }
        }
        Ok(())
    }
}
