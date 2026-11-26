//! GPU timestamps per host list and per pass, in every build.
//!
//! The frame line's "on the GPU" is the wall time the submitting thread
//! waits for the fence. That holds the title's passes, our own work (state
//! preludes, compares on the card) and the time the queue is idle or the
//! driver latency between our lists. This module measures what the GPU
//! itself spends: each list writes a timestamp when it starts, at every
//! pass boundary (a change of render targets, between draws and dispatches,
//! a copy) and when it ends, into one shared query heap. The stamps are
//! resolved by the list itself, and read **after** the wait the submission
//! makes anyway, so measuring adds no wait (ADR 0002 §4.5).
//!
//! `VICTORY_GPU_PROFILE=0` records no stamps.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// Stamps a list may write: its start, up to `BLOCK - 2` boundaries, its end.
pub(crate) const BLOCK: u32 = 64;
/// Blocks in the heap. A list takes one when it starts recording; a list
/// still recording or waiting to run when the ring has come round again
/// (1,024 lists later) is not measured (its block is reported as lost).
const BLOCKS: u32 = 1024;
const CAPACITY: u32 = BLOCK * BLOCKS;

/// What the GPU does between two stamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    /// Commands of the title's list before its first draw, dispatch or copy
    /// (state, clears, barriers).
    Setup = 0,
    /// The title's draws.
    Graphics = 1,
    /// The title's dispatches.
    Compute = 2,
    /// The title's copies.
    Copy = 3,
    /// Our state prelude before a list (transitions, depth/colour twins).
    Prelude = 4,
    /// Our compare passes over copies in the card's memory.
    CardCheck = 5,
}

/// The kinds, in the order the frame line names them.
pub(crate) const KINDS: [Kind; 6] = [Kind::Setup, Kind::Graphics, Kind::Compute, Kind::Copy, Kind::Prelude, Kind::CardCheck];

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Setup => "setup",
            Kind::Graphics => "graphics",
            Kind::Compute => "compute",
            Kind::Copy => "copies",
            Kind::Prelude => "our preludes",
            Kind::CardCheck => "our card compares",
        }
    }

    /// Whether the title asked for this work (else it is ours).
    pub(crate) fn titles(self) -> bool {
        matches!(self, Kind::Setup | Kind::Graphics | Kind::Compute | Kind::Copy)
    }
}

pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GPU_PROFILE").is_none_or(|v| v != "0"))
}

struct Heap {
    heap: ID3D12QueryHeap,
    back: ID3D12Resource,
    /// The read-back buffer, mapped for the process's life.
    data: *const u64,
}

// SAFETY: the heap and buffer are free-threaded D3D12 objects; `data` is
// only read, after the GPU wrote it and a fence said so.
unsafe impl Send for Heap {}
unsafe impl Sync for Heap {}

static HEAP: OnceLock<Option<Heap>> = OnceLock::new();

/// Blocks handed out so far (the ring position and each block's generation).
static NEXT: AtomicU64 = AtomicU64::new(0);

fn heap(device: &ID3D12Device) -> Option<&'static Heap> {
    HEAP.get_or_init(|| {
        let mut heap: Option<ID3D12QueryHeap> = None;
        let mut back: Option<ID3D12Resource> = None;
        let readback = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: u64::from(CAPACITY) * 8,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        // SAFETY: plain object creation and a map of our own read-back buffer.
        unsafe {
            device
                .CreateQueryHeap(
                    &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: CAPACITY, NodeMask: 0 },
                    &mut heap,
                )
                .ok()?;
            device
                .CreateCommittedResource(&readback, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut back)
                .ok()?;
            let back = back?;
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).ok()?;
            Some(Heap { heap: heap?, back, data: data.cast() })
        }
    })
    .as_ref()
}

/// A list's stamps: its block of the heap and the kind of work that starts
/// at each stamp.
#[derive(Default)]
pub(crate) struct Stamps {
    /// The first query of the block, and the block's number (generation).
    block: Option<(u32, u64)>,
    /// `kinds[i]`: what runs from stamp `i` to stamp `i + 1`.
    kinds: Vec<Kind>,
    /// Whether the end stamp and the resolve are recorded.
    ended: bool,
}

impl Stamps {
    pub(crate) const EMPTY: Stamps = Stamps { block: None, kinds: Vec::new(), ended: false };

    /// The list starts recording (just made or reset): a block and the first
    /// stamp. Earlier stamps are forgotten.
    pub(crate) fn begin(&mut self, device: &ID3D12Device, list: &ID3D12GraphicsCommandList, kind: Kind) {
        *self = Stamps::default();
        if !on() {
            return;
        }
        let Some(h) = heap(device) else { return };
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let first = (n % u64::from(BLOCKS)) as u32 * BLOCK;
        // SAFETY: an open list and our heap.
        unsafe { list.EndQuery(&h.heap, D3D12_QUERY_TYPE_TIMESTAMP, first) };
        self.block = Some((first, n));
        self.kinds.push(kind);
    }

    /// Work of `kind` starts here. A new stamp is written when the kind
    /// changes, or always with `new_pass` (render targets changed); past the
    /// block's room the work counts to the stamp before.
    pub(crate) fn mark(&mut self, list: &ID3D12GraphicsCommandList, kind: Kind, new_pass: bool) {
        let Some((first, _)) = self.block else { return };
        if self.ended || (!new_pass && self.kinds.last() == Some(&kind)) || self.kinds.len() as u32 >= BLOCK - 1 {
            return;
        }
        let Some(Some(h)) = HEAP.get() else { return };
        // SAFETY: an open list and our heap; the index is inside the block.
        unsafe { list.EndQuery(&h.heap, D3D12_QUERY_TYPE_TIMESTAMP, first + self.kinds.len() as u32) };
        self.kinds.push(kind);
    }

    /// The list is about to close: the end stamp, and the block resolved
    /// into the read-back buffer at the block's place.
    pub(crate) fn end(&mut self, list: &ID3D12GraphicsCommandList) {
        let Some((first, _)) = self.block else { return };
        if self.ended {
            return;
        }
        let Some(Some(h)) = HEAP.get() else { return };
        let n = self.kinds.len() as u32;
        // SAFETY: an open list, our heap and read-back buffer, in range.
        unsafe {
            list.EndQuery(&h.heap, D3D12_QUERY_TYPE_TIMESTAMP, first + n);
            list.ResolveQueryData(&h.heap, D3D12_QUERY_TYPE_TIMESTAMP, first, n + 1, &h.back, u64::from(first) * 8);
        }
        self.ended = true;
    }

    /// After the list ran (a fence said so): what ran between its stamps, as
    /// (kind, start tick, end tick). `None` if nothing was stamped, or the
    /// block was handed out again since (then counted as lost).
    pub(crate) fn read(&self) -> Option<Vec<(Kind, u64, u64)>> {
        let (first, generation) = self.block?;
        if !self.ended {
            return None;
        }
        if NEXT.load(Ordering::Relaxed).saturating_sub(generation) > u64::from(BLOCKS) {
            LOST.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let h = HEAP.get()?.as_ref()?;
        let ticks: Vec<u64> = (0..=self.kinds.len())
            // SAFETY: inside the mapped buffer; the GPU finished writing it.
            .map(|i| unsafe { std::ptr::read_volatile(h.data.add(first as usize + i)) })
            .collect();
        Some(segments(&self.kinds, &ticks))
    }
}

/// The segments between consecutive stamps. A stamp earlier than the one
/// before (which a driver may give for work it reordered) counts as no time.
pub(crate) fn segments(kinds: &[Kind], ticks: &[u64]) -> Vec<(Kind, u64, u64)> {
    kinds
        .iter()
        .enumerate()
        .filter_map(|(i, &k)| Some((k, *ticks.get(i)?, (*ticks.get(i + 1)?).max(ticks[i]))))
        .collect()
}

/// Lists whose block was handed out again before they were read.
static LOST: AtomicU64 = AtomicU64::new(0);

/// Since the last frame line, in nanoseconds: GPU time by [`Kind`]; then the
/// sum of the submissions' busy spans (first stamp to last); the walls the
/// submitting thread waited for them; passes; submissions measured; and
/// submissions not measured.
pub(crate) static NS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
pub(crate) static SPAN_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static WALL_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static PASSES: AtomicU64 = AtomicU64::new(0);
pub(crate) static MEASURED: AtomicU64 = AtomicU64::new(0);
pub(crate) static UNMEASURED: AtomicU64 = AtomicU64::new(0);
/// The longest pass since the last frame line (ns) and its kind.
pub(crate) static LONGEST: AtomicU64 = AtomicU64::new(0);
pub(crate) static LONGEST_KIND: AtomicU64 = AtomicU64::new(0);

/// What one submission's lists measured, summed for the frame line.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Summary {
    pub(crate) by_kind: [u64; 6],
    /// First stamp to last over all the submission's lists, in ticks.
    pub(crate) span: u64,
    pub(crate) passes: u64,
    pub(crate) longest: (u64, Option<Kind>),
}

/// Sums the segments of a submission's lists (in ticks).
pub(crate) fn summarize(lists: &[Vec<(Kind, u64, u64)>]) -> Summary {
    let mut s = Summary::default();
    let (mut lo, mut hi) = (u64::MAX, 0);
    for segs in lists {
        for &(k, a, b) in segs {
            let t = b - a;
            s.by_kind[k as usize] += t;
            s.passes += 1;
            if t > s.longest.0 {
                s.longest = (t, Some(k));
            }
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    if hi > lo {
        s.span = hi - lo;
    }
    s
}

/// Ticks per second of the queue's clock, set once by the host.
static FREQUENCY: AtomicU64 = AtomicU64::new(0);

pub(crate) fn set_frequency(queue: &ID3D12CommandQueue) {
    if FREQUENCY.load(Ordering::Relaxed) == 0 {
        // SAFETY: a plain query.
        if let Ok(f) = unsafe { queue.GetTimestampFrequency() } {
            FREQUENCY.store(f, Ordering::Relaxed);
        }
    }
}

fn ns(ticks: u64) -> u64 {
    let f = FREQUENCY.load(Ordering::Relaxed).max(1);
    (u128::from(ticks) * 1_000_000_000 / u128::from(f)) as u64
}

/// A submission waited for: its lists' stamps (each `None` if not
/// measured) and the wall time the submitting thread waited.
pub(crate) fn submission_done(lists: Vec<Option<Vec<(Kind, u64, u64)>>>, wall: std::time::Duration) {
    if !on() {
        return;
    }
    if lists.is_empty() || lists.iter().any(Option::is_none) {
        UNMEASURED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let lists: Vec<_> = lists.into_iter().flatten().collect();
    let s = summarize(&lists);
    for (i, t) in s.by_kind.iter().enumerate() {
        NS[i].fetch_add(ns(*t), Ordering::Relaxed);
    }
    SPAN_NS.fetch_add(ns(s.span), Ordering::Relaxed);
    WALL_NS.fetch_add(wall.as_nanos() as u64, Ordering::Relaxed);
    PASSES.fetch_add(s.passes, Ordering::Relaxed);
    MEASURED.fetch_add(1, Ordering::Relaxed);
    let longest = ns(s.longest.0);
    if longest > LONGEST.load(Ordering::Relaxed) {
        LONGEST.store(longest, Ordering::Relaxed);
        LONGEST_KIND.store(s.longest.1.map_or(0, |k| k as u64), Ordering::Relaxed);
    }
}

/// The frame line's GPU line for `frames` frames, and the counters reset.
pub(crate) fn line(frames: f64) -> Option<String> {
    if !on() {
        return None;
    }
    let ms = |a: &AtomicU64| a.swap(0, Ordering::Relaxed) as f64 / 1e6 / frames;
    let by: Vec<f64> = NS.iter().map(ms).collect();
    let (span, wall) = (ms(&SPAN_NS), ms(&WALL_NS));
    let passes = PASSES.swap(0, Ordering::Relaxed) as f64 / frames;
    let (measured, unmeasured) = (MEASURED.swap(0, Ordering::Relaxed), UNMEASURED.swap(0, Ordering::Relaxed));
    let longest = LONGEST.swap(0, Ordering::Relaxed) as f64 / 1e6;
    let longest_kind = KINDS.get(LONGEST_KIND.load(Ordering::Relaxed) as usize).map_or("?", |k| k.name());
    if measured + unmeasured == 0 {
        return None;
    }
    let title: f64 = KINDS.iter().filter(|k| k.titles()).map(|&k| by[k as usize]).sum();
    let ours: f64 = KINDS.iter().filter(|k| !k.titles()).map(|&k| by[k as usize]).sum();
    let parts: Vec<String> = KINDS.iter().map(|&k| format!("{} {:.1}", k.name(), by[k as usize])).collect();
    Some(format!(
        "GPU timestamps per frame (ms): the title's work {title:.1}, ours {ours:.1} ({}); busy span {span:.1} of {wall:.1} \
         waited for the fence (so {:.1} the GPU was idle or the driver latent while we waited); {passes:.0} passes, the \
         longest {longest:.1} ({longest_kind}); submissions measured {measured}, not measured {unmeasured}, stamp blocks \
         lost {}",
        parts.join(", "),
        (wall - span).max(0.0),
        LOST.swap(0, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::Interface;

    #[test]
    fn segments_follow_the_stamps_and_never_go_back() {
        let kinds = [Kind::Setup, Kind::Graphics, Kind::Compute];
        let s = segments(&kinds, &[100, 110, 150, 145]);
        assert_eq!(s, [(Kind::Setup, 100, 110), (Kind::Graphics, 110, 150), (Kind::Compute, 150, 150)]);
        // A list whose end stamp is missing gives the segments it has.
        assert_eq!(segments(&kinds, &[1, 2]).len(), 1);
    }

    #[test]
    fn a_submission_sums_by_kind_and_spans_its_lists() {
        let a = vec![(Kind::Prelude, 0, 5), (Kind::Graphics, 5, 25)];
        let b = vec![(Kind::Setup, 40, 42), (Kind::Compute, 42, 60), (Kind::Graphics, 60, 61)];
        let s = summarize(&[a, b]);
        assert_eq!(s.by_kind[Kind::Graphics as usize], 21);
        assert_eq!(s.by_kind[Kind::Compute as usize], 18);
        assert_eq!(s.by_kind[Kind::Prelude as usize], 5);
        assert_eq!(s.span, 61, "first stamp to last, gaps between lists included");
        assert_eq!(s.passes, 5);
        assert_eq!(s.longest, (20, Some(Kind::Graphics)));
    }

    /// On the real device: a list that copies 64 MiB takes longer between its
    /// stamps than one that copies 1 MiB, and the stamps are read after the
    /// fence the test waits for anyway.
    #[test]
    fn stamps_on_the_device_grow_with_the_work() {
        let Some(host) = crate::host::host() else {
            return; // no host GPU
        };
        set_frequency(&host.queue);
        let device: &ID3D12Device = &host.device;
        let buffer = |bytes: u64| {
            let props = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: bytes,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: plain object creation.
            unsafe { device.CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut r) }
                .unwrap();
            r.unwrap()
        };
        let (src, dst) = (buffer(64 << 20), buffer(64 << 20));
        let mut took = Vec::new();
        for bytes in [1u64 << 20, 64 << 20] {
            // SAFETY: plain object creation, recording and a fence wait.
            unsafe {
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
                let list: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
                let mut stamps = Stamps::default();
                stamps.begin(device, &list, Kind::Setup);
                stamps.mark(&list, Kind::Copy, false);
                for _ in 0..8 {
                    list.CopyBufferRegion(&dst, 0, &src, 0, bytes);
                }
                stamps.end(&list);
                list.Close().unwrap();
                host.queue.ExecuteCommandLists(&[Some(list.cast().unwrap())]);
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
                host.queue.Signal(&fence, 1).unwrap();
                while fence.GetCompletedValue() < 1 {
                    std::thread::yield_now();
                }
                let segs = stamps.read().expect("stamps read after the fence");
                assert_eq!(segs.len(), 2);
                assert_eq!(segs[1].0, Kind::Copy);
                took.push(ns(segs[1].2 - segs[1].1));
            }
        }
        assert!(took[1] > took[0], "64 MiB copies ({} ns) not longer than 1 MiB ({} ns)", took[1], took[0]);
    }
}
