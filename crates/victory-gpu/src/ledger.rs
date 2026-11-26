//! What the host has allocated and where, alive now: every host resource
//! booked here at creation by what it is for (its category) and where it
//! lives (the card's memory, the PC's, title memory opened as a heap), with
//! its logical size (what it holds) and its allocation (what the device
//! reserves for it, alignment included). The device tells us when a
//! resource is destroyed (`ID3DDestructionNotifier`), so its bytes leave
//! the ledger then, wherever the last reference was dropped: nothing in the
//! owning structures changes, and nothing here is guessed from counters of
//! what was ever made.
//!
//! Observe only: it answers "what fills the card, what does the GPU read
//! over the bus, which kinds of host objects pile up" on the frame line,
//! and gives the residency decisions (`crate::residency`) their figures.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D::ID3DDestructionNotifier;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

/// What a host resource is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Cat {
    /// A title buffer's host copy (the side shaders see, or the CPU's side
    /// of a copy in the card's memory).
    BufferCopy,
    /// A copy of a window of title memory outside buffers.
    WindowCopy,
    /// The card side of a copy in the card's memory (buffer or window).
    CardSide,
    /// The "before" twin of a card side, for the compare on the card.
    CardBefore,
    /// A title texture's host copy.
    Texture,
    /// A colour twin of a depth texture, or its stencil buffer.
    DepthTwin,
    /// Upload buffers (staging of texture uploads, refreshes).
    Upload,
    /// Read-back buffers.
    Readback,
    /// Title memory opened as a heap, a buffer placed on it.
    InPlace,
    /// Everything else of ours (queries, tables, the layer pass, tests).
    Internal,
}

/// Where a host resource lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Pool {
    /// The card's own memory.
    Card,
    /// The PC's memory, read and written by the GPU over the bus.
    System,
    /// Reserved (tiled) or not known.
    Other,
}

const CATS: usize = 10;
const POOLS: usize = 3;
const CAT_NAMES: [&str; CATS] = [
    "title-buffer copies",
    "window copies",
    "card sides",
    "card befores",
    "textures",
    "depth twins",
    "upload",
    "read-back",
    "title memory in place",
    "internal",
];

/// Alive per category and pool: resources, logical bytes, allocated bytes.
static ALIVE: [[[AtomicU64; 3]; POOLS]; CATS] = [const { [const { [const { AtomicU64::new(0) }; 3] }; POOLS] }; CATS];
/// Since the start, per category: booked, and destroyed.
static CHURN: [[AtomicU64; 2]; CATS] = [const { [const { AtomicU64::new(0) }; 2] }; CATS];

/// What a booking took, for its release.
struct Booking {
    cat: usize,
    pool: usize,
    logical: u64,
    allocated: u64,
}

thread_local! {
    /// A category the next bookings on this thread take instead of the one
    /// their helper names ([`booking_as`]).
    static AS: std::cell::Cell<Option<Cat>> = const { std::cell::Cell::new(None) };
}

/// Until dropped, resources booked on this thread count as `cat` (for a
/// caller of a shared helper such as `Host::shared_buffer`, which only knows
/// it makes a buffer).
pub(crate) struct BookingAs(Option<Cat>);

pub(crate) fn booking_as(cat: Cat) -> BookingAs {
    BookingAs(AS.with(|a| a.replace(Some(cat))))
}

impl Drop for BookingAs {
    fn drop(&mut self) {
        AS.with(|a| a.set(self.0));
    }
}

/// Where a resource lives, from its heap's properties.
fn pool_of(resource: &ID3D12Resource) -> Pool {
    let mut props = D3D12_HEAP_PROPERTIES::default();
    // SAFETY: a plain query into our structure; reserved resources fail it.
    if unsafe { resource.GetHeapProperties(Some(&mut props), None) }.is_err() {
        return Pool::Other;
    }
    pool_from(props.Type, props.MemoryPoolPreference, props.CPUPageProperty)
}

/// [`pool_of`] for a discrete card: default heaps in its memory, upload and
/// read-back in the PC's; custom heaps by their pool (unknown: by whether
/// the CPU can map them).
pub(crate) fn pool_from(kind: D3D12_HEAP_TYPE, pool: D3D12_MEMORY_POOL, cpu: D3D12_CPU_PAGE_PROPERTY) -> Pool {
    match kind {
        D3D12_HEAP_TYPE_DEFAULT => Pool::Card,
        D3D12_HEAP_TYPE_UPLOAD | D3D12_HEAP_TYPE_READBACK => Pool::System,
        // The GPU upload heap (ReBAR): the card's memory, mapped by the CPU.
        D3D12_HEAP_TYPE_GPU_UPLOAD => Pool::Card,
        D3D12_HEAP_TYPE_CUSTOM => match pool {
            D3D12_MEMORY_POOL_L1 => Pool::Card,
            D3D12_MEMORY_POOL_L0 => Pool::System,
            _ if cpu == D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE => Pool::Card,
            _ => Pool::System,
        },
        _ => Pool::Other,
    }
}

/// The bytes a resource holds (buffers: their width; textures: all
/// subresources laid out for a copy) and the device's allocation for it.
fn sizes(resource: &ID3D12Resource) -> (u64, u64) {
    let Some(host) = crate::host::host() else {
        return (0, 0);
    };
    // SAFETY: plain queries on a live resource and device.
    unsafe {
        let desc = resource.GetDesc();
        let allocated = host.device.GetResourceAllocationInfo(0, &[desc]).SizeInBytes;
        let logical = if desc.Dimension == D3D12_RESOURCE_DIMENSION_BUFFER {
            desc.Width
        } else {
            let subresources = u32::from(desc.MipLevels.max(1))
                * if desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D { 1 } else { u32::from(desc.DepthOrArraySize.max(1)) };
            let mut total = 0u64;
            host.device.GetCopyableFootprints(&desc, 0, subresources, 0, None, None, None, Some(&mut total));
            total
        };
        (logical, if allocated == u64::MAX { logical } else { allocated })
    }
}

unsafe extern "system" fn destroyed(data: *mut core::ffi::c_void) {
    // SAFETY: the box [`book`] leaked for this resource, given back once.
    let b = unsafe { Box::from_raw(data.cast::<Booking>()) };
    let alive = &ALIVE[b.cat][b.pool];
    alive[0].fetch_sub(1, Relaxed);
    alive[1].fetch_sub(b.logical, Relaxed);
    alive[2].fetch_sub(b.allocated, Relaxed);
    CHURN[b.cat][1].fetch_add(1, Relaxed);
}

/// Books `resource` as `cat` (or what [`booking_as`] says on this thread)
/// until the device destroys it. Book each resource once, right after it
/// is made.
pub(crate) fn book(resource: &ID3D12Resource, cat: Cat) {
    let cat = AS.with(std::cell::Cell::get).unwrap_or(cat);
    let pool = pool_of(resource);
    let (logical, allocated) = sizes(resource);
    let Ok(notifier) = resource.cast::<ID3DDestructionNotifier>() else {
        return; // not counted: we would never learn it went
    };
    let booking = Box::into_raw(Box::new(Booking {
        cat: cat as usize,
        pool: pool as usize,
        logical,
        allocated,
    }));
    // SAFETY: the callback gets the box back exactly once, when the
    // resource is destroyed.
    if unsafe { notifier.RegisterDestructionCallback(Some(destroyed), booking.cast()) }.is_err() {
        // SAFETY: not given to the device; ours again.
        drop(unsafe { Box::from_raw(booking) });
        return;
    }
    let alive = &ALIVE[cat as usize][pool as usize];
    alive[0].fetch_add(1, Relaxed);
    alive[1].fetch_add(logical, Relaxed);
    alive[2].fetch_add(allocated, Relaxed);
    CHURN[cat as usize][0].fetch_add(1, Relaxed);
}

/// Alive now in `cat` and `pool`: resources, logical bytes, allocated bytes.
pub(crate) fn alive(cat: Cat, pool: Pool) -> [u64; 3] {
    let a = &ALIVE[cat as usize][pool as usize];
    [a[0].load(Relaxed), a[1].load(Relaxed), a[2].load(Relaxed)]
}

/// Allocated bytes alive in `pool`, all categories.
pub(crate) fn pool_bytes(pool: Pool) -> u64 {
    (0..CATS).map(|c| ALIVE[c][pool as usize][2].load(Relaxed)).sum()
}

fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

/// The frame line's account of host memory: per pool, each category with
/// resources alive, allocated MiB (and logical MiB where alignment adds
/// more than a tenth), and the bookings and destructions since the last
/// line.
pub(crate) fn line() -> String {
    static LAST: [[AtomicU64; 2]; CATS] = [const { [const { AtomicU64::new(0) }; 2] }; CATS];
    let mut text = String::from("host memory alive");
    for (pool, name) in [(Pool::Card, "on the card"), (Pool::System, "in the PC's memory, GPU-visible"), (Pool::Other, "other")] {
        let total = pool_bytes(pool);
        if total == 0 && pool == Pool::Other {
            continue;
        }
        text.push_str(&format!("; {name} {:.0} MiB:", mib(total)));
        let mut any = false;
        for (c, cat) in CAT_NAMES.iter().enumerate() {
            let a = &ALIVE[c][pool as usize];
            let (n, logical, allocated) = (a[0].load(Relaxed), a[1].load(Relaxed), a[2].load(Relaxed));
            if n == 0 {
                continue;
            }
            text.push_str(&format!("{} {cat} {n} ({:.0} MiB", if any { "," } else { "" }, mib(allocated)));
            if allocated > logical + logical / 10 {
                text.push_str(&format!(", {:.0} held", mib(logical)));
            }
            text.push(')');
            any = true;
        }
        if !any {
            text.push_str(" none");
        }
    }
    let mut churn = String::new();
    for (c, cat) in CAT_NAMES.iter().enumerate() {
        let made = CHURN[c][0].load(Relaxed);
        let gone = CHURN[c][1].load(Relaxed);
        let made_since = made - LAST[c][0].swap(made, Relaxed);
        let gone_since = gone - LAST[c][1].swap(gone, Relaxed);
        if made_since + gone_since > 0 {
            churn.push_str(&format!("{} {cat} +{made_since}/-{gone_since}", if churn.is_empty() { "" } else { "," }));
        }
    }
    if !churn.is_empty() {
        text.push_str(&format!("; made/destroyed since the last line:{churn}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools_follow_the_heap() {
        use D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE as NA;
        use D3D12_CPU_PAGE_PROPERTY_WRITE_BACK as WB;
        assert_eq!(pool_from(D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN, NA), Pool::Card);
        assert_eq!(pool_from(D3D12_HEAP_TYPE_UPLOAD, D3D12_MEMORY_POOL_UNKNOWN, NA), Pool::System);
        assert_eq!(pool_from(D3D12_HEAP_TYPE_READBACK, D3D12_MEMORY_POOL_UNKNOWN, NA), Pool::System);
        assert_eq!(pool_from(D3D12_HEAP_TYPE_CUSTOM, D3D12_MEMORY_POOL_L0, WB), Pool::System, "shared_buffer");
        assert_eq!(pool_from(D3D12_HEAP_TYPE_CUSTOM, D3D12_MEMORY_POOL_L0, NA), Pool::System, "GPU-only PC memory");
        assert_eq!(pool_from(D3D12_HEAP_TYPE_CUSTOM, D3D12_MEMORY_POOL_L1, NA), Pool::Card);
    }

    /// On the device: a booked buffer and texture are counted where they
    /// live, by what they are for, and leave the ledger when the last
    /// reference goes.
    #[test]
    fn bookings_leave_when_the_device_destroys_them() {
        static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(host) = crate::host::host() else {
            return; // no host GPU
        };
        // `Internal` and `Readback` are booked by other tests too: use
        // categories nothing else books in this crate's tests at the moment
        // of the check (`CardBefore` on the card, `WindowCopy` in the PC's
        // memory), and compare before and after.
        let before_card = alive(Cat::CardBefore, Pool::Card);
        let before_system = alive(Cat::WindowCopy, Pool::System);
        let card = {
            let _as = booking_as(Cat::CardBefore);
            host.uav_buffer(100 << 10).expect("a card buffer")
        };
        let (system, _) = {
            let _as = booking_as(Cat::WindowCopy);
            host.shared_buffer(3 << 16).expect("a PC-memory buffer")
        };
        let now_card = alive(Cat::CardBefore, Pool::Card);
        let now_system = alive(Cat::WindowCopy, Pool::System);
        assert_eq!(now_card[0], before_card[0] + 1);
        assert_eq!(now_card[1], before_card[1] + (100 << 10), "logical: the width");
        assert!(now_card[2] >= before_card[2] + (100 << 10), "allocated: at least the width");
        assert_eq!(now_system[0], before_system[0] + 1, "booked by shared_buffer, as the caller said");
        assert!(line().contains("card befores"), "{}", line());
        drop(card);
        drop(system);
        assert_eq!(alive(Cat::CardBefore, Pool::Card), before_card, "gone with the buffer");
        assert_eq!(alive(Cat::WindowCopy, Pool::System), before_system);
    }
}
