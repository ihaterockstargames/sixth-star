//! What the copy engine costs the CPU per frame, against the fence timeline
//! (ADR 0002 §4.1), on a synthetic workload with real operations: Windows
//! write watch on a real reservation, real copies and compares. No game data.
//!
//! The model, per frame:
//!
//! - title memory: a write-watched reservation. The CPU writes some pages of
//!   its CPU-written part (not timed: that is the title's own work);
//! - the GPU's copy: a second buffer of the same size. What shaders write is
//!   modelled by changing pages of the declared-writable part (not timed:
//!   that is GPU work);
//! - **inherited engine** (as `host.rs` does it): ask the write watch about
//!   all of title memory and copy the written pages up; snapshot every
//!   declared-writable byte before the GPU runs; afterwards compare all of
//!   them with the snapshot and copy the differing pages back; reset the
//!   watch over what the copy-back wrote;
//! - **timeline, guarded** (the replacement): ask the write watch only about
//!   memory whose contract says the CPU writes it, forward those pages, keep
//!   the timeline's books, and fetch only the GPU-written bytes the CPU then
//!   reads;
//! - **timeline, unguarded**: the same, but with nothing guarding the GPU's
//!   bytes, every declared-writable byte comes back at retire (the
//!   conservative default).
//!
//! The default workload follows the intro measured on 2026-11-26 (about
//! 32 MB copied up and 7.8 MB copied back per frame); the others vary what is
//! not measured yet. These are CPU costs of a model, not frame rates.

#[cfg(not(windows))]
fn main() {
    eprintln!("enginebench runs on Windows only");
}

#[cfg(windows)]
fn main() {
    bench::main();
}

#[cfg(windows)]
// One declared range per side is the model; the frame's state is passed as is.
#[allow(clippy::single_range_in_vec_init, clippy::too_many_arguments)]
mod bench {
    use sixth_memory::Timeline;
    use std::time::Instant;
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, ResetWriteWatch,
        VirtualAlloc, VirtualFree,
    };

    /// Not in windows-sys' memory module.
    const MEM_WRITE_WATCH: u32 = 0x20_0000;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;

    const PAGE: usize = 4096;
    const MB: usize = 1 << 20;

    struct Workload {
        name: &'static str,
        /// All of title memory the inherited engine watches.
        heap_mb: usize,
        /// The part whose contract says the CPU writes it.
        cpu_region_mb: usize,
        /// Pages the CPU writes per frame.
        cpu_dirty_mb: usize,
        /// Bytes declared writable by the frame's shaders.
        declared_mb: usize,
        /// Bytes they really write.
        shader_mb: usize,
        /// GPU-written bytes the CPU reads afterwards.
        cpu_reads_kb: usize,
    }

    const WORKLOADS: [Workload; 4] = [
        Workload {
            name: "intro-like (32 MB up, 8 MB written back)",
            heap_mb: 512,
            cpu_region_mb: 64,
            cpu_dirty_mb: 32,
            declared_mb: 64,
            shader_mb: 8,
            cpu_reads_kb: 256,
        },
        Workload {
            name: "large declared heap (174 MB declared, 8 MB written)",
            heap_mb: 512,
            cpu_region_mb: 64,
            cpu_dirty_mb: 32,
            declared_mb: 174,
            shader_mb: 8,
            cpu_reads_kb: 256,
        },
        Workload {
            name: "CPU reads 10% of what shaders wrote",
            heap_mb: 512,
            cpu_region_mb: 64,
            cpu_dirty_mb: 32,
            declared_mb: 64,
            shader_mb: 8,
            cpu_reads_kb: 800,
        },
        Workload {
            name: "CPU reads all that shaders wrote",
            heap_mb: 512,
            cpu_region_mb: 64,
            cpu_dirty_mb: 32,
            declared_mb: 64,
            shader_mb: 8,
            cpu_reads_kb: 8 * 1024,
        },
    ];

    struct Region {
        at: *mut u8,
        len: usize,
    }

    impl Region {
        fn new(len: usize, watched: bool) -> Region {
            let kind = MEM_RESERVE | MEM_COMMIT | if watched { MEM_WRITE_WATCH } else { 0 };
            // SAFETY: a fresh private allocation.
            let at =
                unsafe { VirtualAlloc(std::ptr::null(), len, kind, PAGE_READWRITE) }.cast::<u8>();
            assert!(!at.is_null(), "allocation of {len} bytes");
            // Touch every page once so that page faults are not timed.
            // SAFETY: inside the allocation.
            unsafe { std::ptr::write_bytes(at, 0, len) };
            Region { at, len }
        }

        fn page(&self, i: usize) -> *mut u8 {
            assert!((i + 1) * PAGE <= self.len);
            // SAFETY: checked above.
            unsafe { self.at.add(i * PAGE) }
        }
    }

    impl Drop for Region {
        fn drop(&mut self) {
            // SAFETY: our allocation.
            unsafe { VirtualFree(self.at.cast(), 0, MEM_RELEASE) };
        }
    }

    /// The write watch's answer for `[from, from + len)` of `title`, reset:
    /// page indices of the region.
    fn ask(
        title: &Region,
        from: usize,
        len: usize,
        out: &mut Vec<*mut std::ffi::c_void>,
    ) -> Vec<usize> {
        out.resize(len / PAGE, std::ptr::null_mut());
        let mut count = out.len();
        let mut granularity = 0u32;
        // SAFETY: a watched region of ours; `out` holds `count` entries.
        let r = unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                title.at.add(from).cast(),
                len,
                out.as_mut_ptr(),
                &mut count,
                &mut granularity,
            )
        };
        assert_eq!(r, 0, "GetWriteWatch");
        out[..count]
            .iter()
            .map(|&p| (p as usize - title.at as usize) / PAGE)
            .collect()
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        /// `n` distinct page indices in `[from, from + span)`.
        fn pages(&mut self, from: usize, span: usize, n: usize) -> Vec<usize> {
            let mut set = std::collections::BTreeSet::new();
            while set.len() < n.min(span) {
                set.insert(from + self.below(span));
            }
            set.into_iter().collect()
        }
    }

    #[derive(Default, Clone, Copy)]
    struct Costs {
        /// Asking the write watch.
        watch: f64,
        /// Copying what the CPU wrote up to the GPU's copy.
        up: f64,
        /// The inherited snapshot, or the timeline's books.
        books: f64,
        /// Bringing the GPU's writes back (compare and copy, or fetch).
        back: f64,
    }

    impl Costs {
        fn total(&self) -> f64 {
            self.watch + self.up + self.books + self.back
        }
    }

    /// Consecutive page indices as runs `[first, end)`.
    fn runs(pages: &[usize]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &p in pages {
            match out.last_mut() {
                Some(r) if r.1 == p => r.1 = p + 1,
                _ => out.push((p, p + 1)),
            }
        }
        out
    }

    fn ms(since: Instant) -> f64 {
        since.elapsed().as_secs_f64() * 1e3
    }

    fn copy_page(from: *const u8, to: *mut u8) {
        // SAFETY: two distinct pages of our regions.
        unsafe { std::ptr::copy_nonoverlapping(from, to, PAGE) };
    }

    /// One frame's title and shader writes (not timed): returns the CPU's
    /// dirty pages and the shaders' pages.
    fn title_and_shaders(
        w: &Workload,
        title: &Region,
        gpu: &Region,
        rng: &mut Random,
        frame: u8,
    ) -> (Vec<usize>, Vec<usize>) {
        let cpu_pages = rng.pages(0, w.cpu_region_mb * MB / PAGE, w.cpu_dirty_mb * MB / PAGE);
        for &p in &cpu_pages {
            // SAFETY: a page of title memory.
            unsafe { title.page(p).write_bytes(frame, 64) };
        }
        let declared_from = w.cpu_region_mb * MB / PAGE;
        let shader_pages = rng.pages(
            declared_from,
            w.declared_mb * MB / PAGE,
            w.shader_mb * MB / PAGE,
        );
        let _ = gpu;
        (cpu_pages, shader_pages)
    }

    fn inherited(
        w: &Workload,
        title: &Region,
        gpu: &Region,
        snap: &Region,
        frame: u8,
        rng: &mut Random,
        scratch: &mut Vec<*mut std::ffi::c_void>,
    ) -> Costs {
        let (_, shader_pages) = title_and_shaders(w, title, gpu, rng, frame);
        let mut c = Costs::default();
        let declared_from = w.cpu_region_mb * MB;
        let declared = w.declared_mb * MB;
        // Ask about all of title memory; copy what was written up.
        let t = Instant::now();
        let dirty = ask(title, 0, w.heap_mb * MB, scratch);
        c.watch = ms(t);
        let t = Instant::now();
        for p in dirty {
            copy_page(title.page(p), gpu.page(p));
        }
        c.up = ms(t);
        // Snapshot every declared-writable byte.
        let t = Instant::now();
        // SAFETY: inside both regions.
        unsafe { std::ptr::copy_nonoverlapping(gpu.at.add(declared_from), snap.at, declared) };
        c.books = ms(t);
        // The shaders run (not timed).
        for &p in &shader_pages {
            // SAFETY: a page of the GPU's copy.
            unsafe { gpu.page(p).write_bytes(frame.wrapping_add(1), PAGE) };
        }
        // Compare everything declared; copy back what differs; reset the watch
        // over what the copy-back wrote.
        let t = Instant::now();
        for i in 0..declared / PAGE {
            let p = declared_from / PAGE + i;
            // SAFETY: pages of our regions.
            let differs = unsafe {
                std::slice::from_raw_parts(gpu.page(p), PAGE)
                    != std::slice::from_raw_parts(snap.page(i), PAGE)
            };
            if differs {
                copy_page(gpu.page(p), title.page(p));
            }
        }
        // SAFETY: a watched region of ours.
        unsafe { ResetWriteWatch(title.at.add(declared_from).cast(), declared) };
        c.back = ms(t);
        c
    }

    fn timeline(
        w: &Workload,
        guarded: bool,
        tl: &mut Timeline,
        title: &Region,
        gpu: &Region,
        frame: u64,
        rng: &mut Random,
        scratch: &mut Vec<*mut std::ffi::c_void>,
    ) -> Costs {
        let (_, shader_pages) = title_and_shaders(w, title, gpu, rng, frame as u8);
        let mut c = Costs::default();
        let base = title.at as u64;
        let cpu_region = w.cpu_region_mb * MB;
        let declared_from = cpu_region as u64;
        let declared = (w.declared_mb * MB) as u64;
        if frame == 1 && guarded {
            tl.set_guarded(base + declared_from..base + declared_from + declared, true);
        }
        // Ask only about CPU-written memory; forward what was written.
        let t = Instant::now();
        let dirty = ask(title, 0, cpu_region, scratch);
        c.watch = ms(t);
        let t = Instant::now();
        for (a, b) in runs(&dirty) {
            tl.cpu_wrote(base + (a * PAGE) as u64..base + (b * PAGE) as u64);
        }
        let plan = tl.submit(
            frame,
            &[base..base + cpu_region as u64],
            &[base + declared_from..base + declared_from + declared],
        );
        c.books = ms(t);
        let t = Instant::now();
        for r in &plan.forward {
            let (a, b) = (
                (r.start - base) as usize / PAGE,
                (r.end - base) as usize / PAGE,
            );
            for p in a..b {
                copy_page(title.page(p), gpu.page(p));
            }
        }
        c.up = ms(t);
        // The shaders run (not timed).
        for &p in &shader_pages {
            // SAFETY: a page of the GPU's copy.
            unsafe { gpu.page(p).write_bytes((frame as u8).wrapping_add(1), PAGE) };
        }
        let t = Instant::now();
        let retired = tl.retire(frame);
        for r in &retired.fetch_now {
            let (a, b) = (
                (r.start - base) as usize / PAGE,
                (r.end - base).div_ceil(PAGE as u64) as usize,
            );
            for p in a..b {
                copy_page(gpu.page(p), title.page(p));
            }
        }
        // The CPU reads some of what shaders wrote: fetched on demand.
        let reads = (w.cpu_reads_kb * 1024 / PAGE).min(shader_pages.len());
        for &p in shader_pages.iter().take(reads) {
            let r = base + (p * PAGE) as u64..base + ((p + 1) * PAGE) as u64;
            let need = tl.cpu_access(r.clone(), false);
            for f in &need.fetch {
                let (a, b) = (
                    (f.start - base) as usize / PAGE,
                    (f.end - base).div_ceil(PAGE as u64) as usize,
                );
                for q in a..b {
                    copy_page(gpu.page(q), title.page(q));
                }
            }
            tl.cpu_accessed(r, false);
        }
        // SAFETY: a watched region of ours (the fetches wrote title memory).
        unsafe {
            ResetWriteWatch(
                title.at.add(declared_from as usize).cast(),
                declared as usize,
            )
        };
        c.back = ms(t);
        c
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }

    fn summary(costs: &[Costs]) -> Costs {
        Costs {
            watch: median(costs.iter().map(|c| c.watch).collect()),
            up: median(costs.iter().map(|c| c.up).collect()),
            books: median(costs.iter().map(|c| c.books).collect()),
            back: median(costs.iter().map(|c| c.back).collect()),
        }
    }

    pub fn main() {
        const FRAMES: usize = 25;
        const WARM: usize = 3;
        println!(
            "enginebench: CPU ms per frame, median of {FRAMES} frames after {WARM} warm-up frames"
        );
        println!("workload | engine | write watch | copy up | snapshot / books | back | total");
        for w in &WORKLOADS {
            let title = Region::new(w.heap_mb * MB, true);
            let gpu = Region::new(w.heap_mb * MB, false);
            let snap = Region::new(w.declared_mb * MB, false);
            let mut scratch = Vec::new();
            let mut rows: Vec<(&str, Costs)> = Vec::new();
            let mut rng = Random(0x2545_f491_4f6c_dd1d);
            let mut runs = Vec::new();
            for f in 0..FRAMES + WARM {
                let c = inherited(w, &title, &gpu, &snap, f as u8, &mut rng, &mut scratch);
                if f >= WARM {
                    runs.push(c);
                }
            }
            rows.push(("inherited", summary(&runs)));
            for guarded in [true, false] {
                let mut tl = Timeline::new();
                let mut rng = Random(0x2545_f491_4f6c_dd1d);
                let mut runs = Vec::new();
                for f in 1..=(FRAMES + WARM) as u64 {
                    let c = timeline(w, guarded, &mut tl, &title, &gpu, f, &mut rng, &mut scratch);
                    if f as usize > WARM {
                        runs.push(c);
                    }
                }
                rows.push((
                    if guarded {
                        "timeline, guarded"
                    } else {
                        "timeline, unguarded"
                    },
                    summary(&runs),
                ));
            }
            for (engine, c) in rows {
                println!(
                    "{} | {engine} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2}",
                    w.name,
                    c.watch,
                    c.up,
                    c.books,
                    c.back,
                    c.total()
                );
            }
        }
    }
}
