//! A stand-in title that renders, written by us from the runtime's own
//! interface tables (`victory-gpu`: device, queue, list, descriptor heap,
//! fence, resource). No console SDK, no game code or data.
//!
//! It creates the device through `d3d12_xs.dll`, a queue, a list, a fence,
//! a render target and a buffer in title memory. Then, every frame, for a
//! set time, it:
//!
//! - rewrites the whole buffer from the CPU (a fixed grid of colours and a
//!   moving bar), as a title streams data;
//! - clears the top band of the target, then copies the buffer into the
//!   rest (`CopyTextureRegion`, buffer to texture), with the barriers
//!   between;
//! - submits, signals, presents (`PresentX`) and waits for the fence.
//!
//! A correct runtime shows exactly the picture this code describes, so a
//! screenshot tells a missing or wrong copy apart (the runner,
//! `scripts/autotest.ps1`, checks the grid's colours). The title prints
//! `TEST-TITLE` lines and exits with 0, or with a code naming the step that
//! failed (see `Step`).
//!
//! Settings, from the environment: `TEST_TITLE_SECONDS` (how long to run;
//! default 30), `TEST_TITLE_FRAMES` (stop after this many frames; default
//! none) and `TEST_TITLE_COPIES` (buffers rewritten and copied into the
//! target each frame, 1 to 32; default 1: more load on the runtime's paths
//! for title memory than the 60 Hz present hides).

#![no_std]
#![no_main]

use core::ffi::c_void;

type Hr = i32;
type This = *mut c_void;

// ---- the imports --------------------------------------------------------------

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> *mut c_void;
    fn WriteFile(file: *mut c_void, buffer: *const u8, length: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
    fn CreateEventW(attributes: *const c_void, manual: i32, initial: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn QueryPerformanceCounter(count: *mut i64) -> i32;
    fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    fn GetEnvironmentVariableA(name: *const u8, buffer: *mut u8, size: u32) -> u32;
    fn ExitProcess(code: u32) -> !;
}

#[link(name = "d3d12_xs", kind = "raw-dylib")]
unsafe extern "system" {
    fn D3D12XboxCreateDevice(adapter: *const c_void, parameters: *const u32, iid: *const Guid, out: *mut This) -> Hr;
}

// ---- no C runtime: the routines the compiler may call -------------------------

/// # Safety
/// `dst` has room for `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, value: i32, n: usize) -> *mut c_void {
    let d = dst.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: inside the caller's `n` bytes; volatile, so that this
        // loop is not turned into a call to itself.
        unsafe { core::ptr::write_volatile(d.add(i), value as u8) };
        i += 1;
    }
    dst
}

/// # Safety
/// `dst` and `src` hold `n` bytes and do not overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (d, s) = (dst.cast::<u8>(), src.cast::<u8>());
    let mut i = 0;
    while i < n {
        // SAFETY: inside the caller's `n` bytes (volatile: see `memset`).
        unsafe { core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i))) };
        i += 1;
    }
    dst
}

/// # Safety
/// `dst` and `src` hold `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (d, s) = (dst.cast::<u8>(), src.cast::<u8>());
    if (dst as usize) <= (src as usize) {
        // SAFETY: forwards, as the caller asked.
        return unsafe { memcpy(dst, src, n) };
    }
    let mut i = n;
    while i > 0 {
        i -= 1;
        // SAFETY: inside the caller's `n` bytes, backwards.
        unsafe { core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i))) };
    }
    dst
}

/// # Safety
/// `a` and `b` hold `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    let mut i = 0;
    while i < n {
        // SAFETY: inside the caller's `n` bytes.
        let (x, y) = unsafe { (core::ptr::read_volatile(a.add(i)), core::ptr::read_volatile(b.add(i))) };
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        i += 1;
    }
    0
}

/// The linker asks for this in a program that uses floating point.
#[unsafe(no_mangle)]
pub static _fltused: i32 = 0;

/// The unwinding tables of the precompiled `core` name the C++ frame
/// handler; with `panic = "abort"` nothing unwinds, so it is never called.
#[unsafe(no_mangle)]
pub extern "C" fn __CxxFrameHandler3() -> i32 {
    // SAFETY: ends the process (an unwind reached a frame: not expected).
    unsafe { ExitProcess(102) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write(b"TEST-TITLE panic\n");
    // SAFETY: ends the process.
    unsafe { ExitProcess(101) }
}

// ---- output -------------------------------------------------------------------

fn write(text: &[u8]) {
    // SAFETY: the standard output handle and our own bytes.
    unsafe {
        let out = GetStdHandle(0xFFFF_FFF5);
        let mut written = 0;
        WriteFile(out, text.as_ptr(), text.len() as u32, &mut written, core::ptr::null_mut());
    }
}

/// A line put together without allocation.
struct Line {
    bytes: [u8; 200],
    len: usize,
}

impl Line {
    fn new() -> Line {
        Line { bytes: [0; 200], len: 0 }
    }
    fn text(&mut self, s: &[u8]) -> &mut Line {
        for &b in s {
            if self.len < self.bytes.len() - 1 {
                self.bytes[self.len] = b;
                self.len += 1;
            }
        }
        self
    }
    fn number(&mut self, mut v: u64) -> &mut Line {
        let mut digits = [0u8; 20];
        let mut n = 0;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            let d = digits[n];
            self.text(&[d]);
        }
        self
    }
    fn hex(&mut self, v: u32) -> &mut Line {
        self.text(b"0x");
        for i in (0..8).rev() {
            let d = ((v >> (i * 4)) & 0xf) as u8;
            self.text(&[if d < 10 { b'0' + d } else { b'a' + d - 10 }]);
        }
        self
    }
    fn end(&mut self) {
        self.text(b"\n");
        write(&self.bytes[..self.len]);
    }
}

fn setting(name: &[u8], default: u64) -> u64 {
    let mut value = [0u8; 32];
    // SAFETY: a zero-terminated name and our buffer.
    let n = unsafe { GetEnvironmentVariableA(name.as_ptr(), value.as_mut_ptr(), value.len() as u32) } as usize;
    if n == 0 || n >= value.len() {
        return default;
    }
    let mut v = 0u64;
    for &b in &value[..n] {
        if !b.is_ascii_digit() {
            return default;
        }
        v = v * 10 + u64::from(b - b'0');
    }
    v
}

fn now(frequency: i64) -> u64 {
    let mut t = 0;
    // SAFETY: fills our value.
    unsafe { QueryPerformanceCounter(&mut t) };
    (t as u128 * 1_000_000 / frequency.max(1) as u128) as u64
}

// ---- the interfaces, as the runtime lays them out -----------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const IID_DEVICE: Guid = Guid { d1: 0x189819f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
const IID_QUEUE: Guid = Guid { d1: 0x0ec870a6, d2: 0x5d7e, d3: 0x4c22, d4: [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed] };
const IID_ALLOCATOR: Guid = Guid { d1: 0x6102dee4, d2: 0xaf59, d3: 0x4b09, d4: [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24] };
const IID_LIST: Guid = Guid { d1: 0x5b160d0f, d2: 0xac1b, d3: 0x4185, d4: [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55] };
const IID_FENCE: Guid = Guid { d1: 0x0a753dcf, d2: 0xc4d8, d3: 0x4b91, d4: [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76] };
const IID_HEAP: Guid = Guid { d1: 0x8efb471d, d2: 0x616c, d3: 0x4f49, d4: [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51] };
const IID_RESOURCE: Guid = Guid { d1: 0x696442be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };

#[repr(C)]
struct QueueDesc {
    kind: u32,
    priority: i32,
    flags: u32,
    nodes: u32,
}

/// A descriptor heap's description, with room after it: the runtime reads
/// what follows as a hint (newer titles pass a placement there).
#[repr(C)]
struct HeapDesc {
    kind: u32,
    count: u32,
    flags: u32,
    nodes: u32,
    after: [u64; 8],
}

#[repr(C)]
struct HeapProperties {
    kind: u32,
    cpu_page: u32,
    pool: u32,
    creation_nodes: u32,
    visible_nodes: u32,
}

#[repr(C)]
struct ResourceDesc {
    dimension: u32,
    alignment: u64,
    width: u64,
    height: u32,
    depth_or_array: u16,
    mips: u16,
    format: u32,
    sample_count: u32,
    sample_quality: u32,
    layout: u32,
    flags: u32,
}

#[repr(C)]
struct ClearValue {
    format: u32,
    color: [f32; 4],
}

#[repr(C)]
struct Barrier {
    kind: u32,
    flags: u32,
    resource: This,
    subresource: u32,
    before: u32,
    after: u32,
}

#[repr(C)]
struct Footprint {
    format: u32,
    width: u32,
    height: u32,
    depth: u32,
    row_pitch: u32,
}

#[repr(C)]
struct PlacedFootprint {
    offset: u64,
    footprint: Footprint,
}

/// A copy's end: a texture's subresource (`kind` 0; the index is in
/// `footprint.offset`) or a buffer's placed footprint (`kind` 1).
#[repr(C)]
struct CopyLocation {
    resource: This,
    kind: u32,
    footprint: PlacedFootprint,
}

#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// One plane of `PresentX`.
#[repr(C)]
struct Plane {
    token: u64,
    count: u32,
    resources: *const This,
    source_rects: *const [i32; 4],
    placements: *const u8,
    colour_space: u32,
    flags: u32,
}

const _: () = assert!(size_of::<ResourceDesc>() == 56);
const _: () = assert!(size_of::<Barrier>() == 32);
const _: () = assert!(size_of::<CopyLocation>() == 48);
const _: () = assert!(size_of::<Plane>() == 0x30);

/// The method at byte `offset` of `object`'s table.
///
/// # Safety
/// `object` is a live interface whose table has a method of type `F` there.
unsafe fn method<F: Copy>(object: This, offset: usize) -> F {
    // SAFETY: the object's first field is its table (the caller's promise).
    unsafe {
        let table = *(object as *const *const usize);
        let f = *table.add(offset / 8);
        core::mem::transmute_copy::<usize, F>(&f)
    }
}

// ---- the picture --------------------------------------------------------------

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// The top band, cleared (blue).
const BAND: u32 = 120;
/// The rest, copied from the buffer: a grid of 8 x 4 blocks and, at its
/// foot, an 8-row strip with a white bar that moves 16 pixels a frame.
const COPIED: u32 = HEIGHT - BAND;
const STRIP: u32 = 8;
const PITCH: u32 = WIDTH * 4;
const BUFFER_BYTES: u64 = PITCH as u64 * COPIED as u64;
const MAX_COPIES: usize = 32;

/// The grid's colours (R, G, B), block (row r, column c) taking
/// `PALETTE[(r * 3 + c) % 8]`; the runner checks the same table.
const PALETTE: [[u8; 3]; 8] = [
    [230, 25, 75],
    [60, 180, 75],
    [255, 225, 25],
    [0, 130, 200],
    [245, 130, 48],
    [145, 30, 180],
    [70, 240, 240],
    [240, 50, 230],
];

fn rgba(c: [u8; 3]) -> u32 {
    u32::from(c[0]) | (u32::from(c[1]) << 8) | (u32::from(c[2]) << 16) | 0xff00_0000
}

/// Writes frame `frame`'s copied part into `data` (`BUFFER_BYTES` bytes).
fn fill(data: *mut u8, frame: u64) {
    let bar = ((frame * 16) % u64::from(WIDTH)) as u32;
    for y in 0..COPIED {
        // SAFETY: row `y` of the buffer, inside its `BUFFER_BYTES`.
        let row = unsafe { data.add((y * PITCH) as usize).cast::<u32>() };
        let strip = y >= COPIED - STRIP;
        let block_row = (y / 150).min(3);
        for x in 0..WIDTH {
            let value = if strip {
                if x >= bar && x < bar + 64 { 0xffff_ffff } else { 0xff00_0000 }
            } else {
                rgba(PALETTE[((block_row * 3 + x / 160) % 8) as usize])
            };
            // SAFETY: inside the row; volatile, so that the loop stays a
            // store per pixel (the title's own writes, which the runtime
            // must find).
            unsafe { core::ptr::write_volatile(row.add(x as usize), value) };
        }
    }
}

// ---- the run ------------------------------------------------------------------

/// The step that failed, as the exit code.
#[derive(Clone, Copy)]
#[repr(u32)]
enum Step {
    Device = 10,
    Queue,
    Allocator,
    List,
    Fence,
    Heap,
    Target,
    Buffer,
    Map,
    Event,
    AllocatorReset,
    ListReset,
    Close,
    Signal,
    Present,
    WaitEvent,
    Hang,
}

fn check(hr: Hr, step: Step) -> Result<(), u32> {
    if hr >= 0 {
        return Ok(());
    }
    Line::new().text(b"TEST-TITLE failed step ").number(step as u64).text(b" hr=").hex(hr as u32).end();
    Err(step as u32)
}

fn run() -> Result<(), u32> {
    let seconds = setting(b"TEST_TITLE_SECONDS\0", 30);
    let max_frames = setting(b"TEST_TITLE_FRAMES\0", u64::MAX);
    let copies = setting(b"TEST_TITLE_COPIES\0", 1).clamp(1, MAX_COPIES as u64) as usize;
    let mut frequency = 0;
    // SAFETY: fills our value.
    unsafe { QueryPerformanceFrequency(&mut frequency) };
    Line::new().text(b"TEST-TITLE start seconds=").number(seconds).text(b" copies=").number(copies as u64).end();

    let null: This = core::ptr::null_mut();
    // The device: the interface version the runtime checks, the rest zero.
    let mut parameters = [0u32; 16];
    parameters[0] = 0x0002_0004;
    let mut device = null;
    // SAFETY: from here on, every call is one of the runtime's methods with
    // the arguments its tables take, on objects it handed out.
    unsafe {
        check(D3D12XboxCreateDevice(core::ptr::null(), parameters.as_ptr(), &IID_DEVICE, &mut device), Step::Device)?;
        type Create<D> = extern "system" fn(This, *const D, *const Guid, *mut This) -> Hr;
        let mut queue = null;
        let desc = QueueDesc { kind: 0, priority: 0, flags: 0, nodes: 0 };
        check(method::<Create<QueueDesc>>(device, 0x40)(device, &desc, &IID_QUEUE, &mut queue), Step::Queue)?;
        let mut allocator = null;
        let create_allocator = method::<extern "system" fn(This, u32, *const Guid, *mut This) -> Hr>(device, 0x48);
        check(create_allocator(device, 0, &IID_ALLOCATOR, &mut allocator), Step::Allocator)?;
        let mut list = null;
        let create_list = method::<extern "system" fn(This, u32, u32, This, This, *const Guid, *mut This) -> Hr>(device, 0x60);
        check(create_list(device, 0, 0, allocator, null, &IID_LIST, &mut list), Step::List)?;
        let mut fence = null;
        let create_fence = method::<extern "system" fn(This, u64, u32, *const Guid, *mut This) -> Hr>(device, 0x120);
        check(create_fence(device, 0, 0, &IID_FENCE, &mut fence), Step::Fence)?;
        let mut heap = null;
        let heap_desc = HeapDesc { kind: 2, count: 1, flags: 0, nodes: 0, after: [0; 8] };
        check(method::<Create<HeapDesc>>(device, 0x70)(device, &heap_desc, &IID_HEAP, &mut heap), Step::Heap)?;
        let rtv = method::<extern "system" fn(This) -> u64>(heap, 0x48)(heap);

        type Committed = extern "system" fn(This, *const HeapProperties, u32, *const ResourceDesc, u32, *const ClearValue, *const Guid, *mut This) -> Hr;
        let create_committed = method::<Committed>(device, 0xd8);
        let on_card = HeapProperties { kind: 1, cpu_page: 0, pool: 0, creation_nodes: 1, visible_nodes: 1 };
        let target_desc = ResourceDesc {
            dimension: 3,
            alignment: 0,
            width: u64::from(WIDTH),
            height: HEIGHT,
            depth_or_array: 1,
            mips: 1,
            format: 28,
            sample_count: 1,
            sample_quality: 0,
            layout: 0,
            flags: 1,
        };
        let clear = ClearValue { format: 28, color: [0.0, 0.0, 0.0, 1.0] };
        let mut target = null;
        check(create_committed(device, &on_card, 0, &target_desc, 0, &clear, &IID_RESOURCE, &mut target), Step::Target)?;
        method::<extern "system" fn(This, This, *const c_void, u64)>(device, 0xa0)(device, target, core::ptr::null(), rtv);

        let upload = HeapProperties { kind: 2, cpu_page: 0, pool: 0, creation_nodes: 1, visible_nodes: 1 };
        let buffer_desc = ResourceDesc {
            dimension: 1,
            alignment: 0,
            width: BUFFER_BYTES,
            height: 1,
            depth_or_array: 1,
            mips: 1,
            format: 0,
            sample_count: 1,
            sample_quality: 0,
            layout: 1,
            flags: 0,
        };
        let mut buffers = [null; MAX_COPIES];
        let mut data = [core::ptr::null_mut::<u8>(); MAX_COPIES];
        for i in 0..copies {
            check(
                create_committed(device, &upload, 0, &buffer_desc, 0xac3, core::ptr::null(), &IID_RESOURCE, &mut buffers[i]),
                Step::Buffer,
            )?;
            let map = method::<extern "system" fn(This, u32, *const c_void, *mut *mut u8) -> Hr>(buffers[i], 0x40);
            check(map(buffers[i], 0, core::ptr::null(), &mut data[i]), Step::Map)?;
            if data[i].is_null() {
                return Err(Step::Map as u32);
            }
        }
        let event = CreateEventW(core::ptr::null(), 0, 0, core::ptr::null());
        if event.is_null() {
            return Err(Step::Event as u32);
        }

        let close = method::<extern "system" fn(This) -> Hr>(list, 0x48);
        let reset = method::<extern "system" fn(This, This, This) -> Hr>(list, 0x50);
        let copy_region = method::<extern "system" fn(This, *const CopyLocation, u32, u32, u32, *const CopyLocation, *const c_void)>(list, 0x80);
        let barrier = method::<extern "system" fn(This, u32, *const Barrier)>(list, 0xd0);
        let clear_rtv = method::<extern "system" fn(This, u64, *const [f32; 4], u32, *const Rect)>(list, 0x1a0);
        let reset_allocator = method::<extern "system" fn(This) -> Hr>(allocator, 0x40);
        let execute = method::<extern "system" fn(This, u32, *const This)>(queue, 0x50);
        let signal = method::<extern "system" fn(This, This, u64) -> Hr>(queue, 0x70);
        let present = method::<extern "system" fn(This, u32, *const Plane, *const u8) -> Hr>(queue, 0xf0);
        let on_completion = method::<extern "system" fn(This, u64, *mut c_void) -> Hr>(fence, 0x48);
        check(close(list), Step::Close)?;

        let transition = |before: u32, after: u32| Barrier { kind: 0, flags: 0, resource: target, subresource: 0xffff_ffff, before, after };
        let band = Rect { left: 0, top: 0, right: WIDTH as i32, bottom: BAND as i32 };
        let blue = [0.0f32, 0.0, 1.0, 1.0];
        let dst = CopyLocation {
            resource: target,
            kind: 0,
            footprint: PlacedFootprint { offset: 0, footprint: Footprint { format: 0, width: 0, height: 0, depth: 0, row_pitch: 0 } },
        };
        let source = |buffer: This| CopyLocation {
            resource: buffer,
            kind: 1,
            footprint: PlacedFootprint {
                offset: 0,
                footprint: Footprint { format: 28, width: WIDTH, height: COPIED, depth: 1, row_pitch: PITCH },
            },
        };
        let shown = [target];
        let plane = Plane {
            token: 0,
            count: 1,
            resources: shown.as_ptr(),
            source_rects: core::ptr::null(),
            placements: core::ptr::null(),
            colour_space: 0,
            flags: 0,
        };

        let begun = now(frequency);
        let deadline = begun + seconds * 1_000_000;
        let (mut frame, mut worst, mut window_begun, mut window_frames) = (0u64, 0u64, begun, 0u64);
        while frame < max_frames && now(frequency) < deadline {
            let t0 = now(frequency);
            frame += 1;
            for d in &data[..copies] {
                fill(*d, frame);
            }
            check(reset_allocator(allocator), Step::AllocatorReset)?;
            check(reset(list, allocator, null), Step::ListReset)?;
            barrier(list, 1, &transition(0, 4));
            clear_rtv(list, rtv, &blue, 1, &band);
            barrier(list, 1, &transition(4, 0x400));
            // Every buffer holds the same picture: the last copy is the one
            // shown, and each one is the runtime's to bring up to date.
            for b in &buffers[..copies] {
                copy_region(list, &dst, 0, BAND, 0, &source(*b), core::ptr::null());
            }
            barrier(list, 1, &transition(0x400, 0));
            check(close(list), Step::Close)?;
            execute(queue, 1, &list);
            check(signal(queue, fence, frame), Step::Signal)?;
            check(present(queue, 1, &plane, core::ptr::null()), Step::Present)?;
            check(on_completion(fence, frame, event), Step::WaitEvent)?;
            if WaitForSingleObject(event, 10_000) != 0 {
                Line::new().text(b"TEST-TITLE hang frame=").number(frame).end();
                return Err(Step::Hang as u32);
            }
            let took = now(frequency) - t0;
            worst = worst.max(took);
            window_frames += 1;
            let at = now(frequency);
            if at - window_begun >= 1_000_000 {
                Line::new()
                    .text(b"TEST-TITLE second frames=")
                    .number(frame)
                    .text(b" fps_x100=")
                    .number(window_frames * 100_000_000 / (at - window_begun).max(1))
                    .text(b" worst_us=")
                    .number(worst)
                    .end();
                (window_begun, window_frames) = (at, 0);
            }
        }
        let total = now(frequency) - begun;
        Line::new()
            .text(b"TEST-TITLE done frames=")
            .number(frame)
            .text(b" us=")
            .number(total)
            .text(b" us_per_frame=")
            .number(total / frame.max(1))
            .text(b" worst_us=")
            .number(worst)
            .end();
    }
    Ok(())
}

/// The title's entry (see build.rs): its exit code.
#[unsafe(no_mangle)]
pub extern "system" fn title_main() -> u32 {
    match run() {
        Ok(()) => 0,
        Err(code) => code,
    }
}
