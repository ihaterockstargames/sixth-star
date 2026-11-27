//! Submissions in flight (`crate::host::late`, `crate::host::relay`),
//! through the title's own calls: queues, lists, copies between title
//! buffers, fences, releases. The switches are read once per process, so
//! each mode runs this file's scenario in a child process of the test
//! binary with its variables set ([`modes`]); the scenario itself is an
//! ignored test that does nothing unless it is such a child.
//!
//! What it checks, in every mode:
//! - read after write across submissions: a chain of copies, each
//!   submission reading the buffer the one before it wrote;
//! - write after write: two submissions write the same buffer, the later
//!   one's bytes stay;
//! - lifetime: a title buffer released right after the submission that
//!   reads it, while that submission may still be in the air;
//! - synchronisation: the title's fence reaches its value only once all of
//!   it is in title memory, and no device is lost on the way.

use crate::object::This;
use crate::resource::tests::title_buffer;

/// The method at byte `offset` of a title object's table.
///
/// # Safety
/// `object` is one of our title objects and `F` the method's type.
unsafe fn method<F: Copy>(object: This, offset: usize) -> F {
    // SAFETY: a title object starts with its table; the caller names the type.
    unsafe {
        let table = *(object as *const *const u8);
        std::ptr::read(table.add(offset).cast::<F>())
    }
}

type Close = extern "system" fn(This) -> u32;
type CopyBuffer = extern "system" fn(This, This, u64, This, u64, u64);
type Execute = extern "system" fn(This, u32, *const This);
type Signal = extern "system" fn(This, This, u64) -> i32;
type Release = extern "system" fn(This) -> u32;

use crate::object::guid as iid;

/// A title list ready to record, on an allocator of its own.
fn new_list() -> This {
    new_pair().1
}

/// A title allocator and a list on it, ready to record.
fn new_pair() -> (This, This) {
    let null = std::ptr::null();
    let allocator_iid = iid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
    let list_iid = iid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
    let (mut allocator, mut list) = (null, null);
    assert_eq!(crate::list::create_allocator(null, 0, &allocator_iid, &mut allocator), 0);
    assert_eq!(crate::list::create_list(null, 0, 0, allocator, null, &list_iid, &mut list), 0);
    (allocator, list)
}

/// After the title's fence said its last run is done: the allocator and the
/// list reset for the next frame, as a title reuses them.
fn reuse(pair: (This, This)) {
    type ResetAllocator = extern "system" fn(This) -> i32;
    type ResetList = extern "system" fn(This, This, This) -> i32;
    // SAFETY: Reset of a title allocator (slot 8) and of a title list (slot 10).
    unsafe {
        assert_eq!(method::<ResetAllocator>(pair.0, 0x40)(pair.0), 0);
        assert_eq!(method::<ResetList>(pair.1, 0x50)(pair.1, pair.0, std::ptr::null()), 0);
    }
}

/// One submission of one list holding `copies` (to, from, bytes).
fn submit(queue: This, copies: &[(This, This, u64)]) {
    let list = new_list();
    for &(to, from, bytes) in copies {
        // SAFETY: CopyBufferRegion of a title list.
        (unsafe { method::<CopyBuffer>(list, 0x78) })(list, to, 0, from, 0, bytes);
    }
    // SAFETY: Close of a title list, ExecuteCommandLists of a title queue.
    unsafe {
        method::<Close>(list, 0x48)(list);
        method::<Execute>(queue, 0x50)(queue, 1, &list);
    }
}

fn bytes_of(buffer: (This, u64), len: usize) -> &'static [u8] {
    // SAFETY: a title buffer's own memory, `len` bytes or more.
    unsafe { std::slice::from_raw_parts(buffer.1 as *const u8, len) }
}


/// In a child, with the debug layer on: what it reported, printed for the
/// parent (and the run fails on an error when `SIXTH_DEBUG_STRICT` is set).
fn debug_layer_report() {
    if std::env::var_os("VICTORY_D3D12_DEBUG").is_none() {
        return;
    }
    let errors = crate::host::debug_errors();
    println!("DEBUG-LAYER {} errors (layer on: {})", errors.len(), crate::host::debug_layer_on());
    for e in errors.iter().take(8) {
        println!("DEBUG-LAYER error: {e}");
    }
    if std::env::var_os("SIXTH_DEBUG_STRICT").is_some() {
        assert!(errors.is_empty(), "the debug layer reported errors");
    }
}

/// The scenario, in a child process (`SIXTH_FLIGHTS_CHILD=1`).
#[test]
#[ignore = "run by `submissions_in_flight_keep_order_and_lifetime` in a child process"]
fn flights_child() {
    if std::env::var_os("SIXTH_FLIGHTS_CHILD").is_none() || crate::host::host().is_none() {
        return;
    }
    const LEN: usize = 64 << 10;
    const CHAIN: usize = 48;
    let null = std::ptr::null();
    let queue_iid = iid(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);
    let fence_iid = iid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
    let mut queue = null;
    let desc = [0u32; 4];
    assert_eq!(crate::queue::create(null, desc.as_ptr().cast(), &queue_iid, &mut queue), 0);
    let mut fence = null;
    assert_eq!(crate::fence::create(null, 0, 0, &fence_iid, &mut fence), 0);

    // Read after write: X0 -> X1 -> ... -> X(CHAIN), one submission each.
    let chain: Vec<(This, u64)> = (0..=CHAIN).map(|_| title_buffer(LEN as u64, 0)).collect();
    // SAFETY: the first buffer's own memory.
    let first = unsafe { std::slice::from_raw_parts_mut(chain[0].1 as *mut u8, LEN) };
    for (i, b) in first.iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    let pattern = first.to_vec();
    for i in 0..CHAIN {
        submit(queue, &[(chain[i + 1].0, chain[i].0, LEN as u64)]);
        // Lifetime: every eighth source is released at once, while the
        // submission that reads it may still be in the air.
        if i % 8 == 1 {
            // SAFETY: Release of a title buffer we made.
            (unsafe { method::<Release>(chain[i].0, 0x10) })(chain[i].0);
        }
    }
    // Write after write: P then Q into Y, in two submissions.
    let (p, q, y) = (title_buffer(LEN as u64, 0), title_buffer(LEN as u64, 0), title_buffer(LEN as u64, 0));
    // SAFETY: the buffers' own memory.
    unsafe {
        std::slice::from_raw_parts_mut(p.1 as *mut u8, LEN).fill(0x11);
        std::slice::from_raw_parts_mut(q.1 as *mut u8, LEN).fill(0x22);
    }
    submit(queue, &[(y.0, p.0, LEN as u64)]);
    submit(queue, &[(y.0, q.0, LEN as u64)]);
    // The title's fence, behind all of it.
    // SAFETY: Signal of a title queue.
    assert_eq!(unsafe { method::<Signal>(queue, 0x70) }(queue, fence, 1), 0);
    let began = std::time::Instant::now();
    while crate::fence::completed(fence) < 1 {
        assert!(began.elapsed().as_secs() < 20, "the fence never reached its value");
        std::thread::yield_now();
    }
    assert_eq!(crate::descriptor::host::device_removed(), None, "the device was lost");
    let last = bytes_of(chain[CHAIN], LEN);
    let wrong = last.iter().zip(&pattern).filter(|(a, b)| a != b).count();
    assert_eq!(wrong, 0, "the chain's last buffer differs in {wrong} bytes");
    assert!(bytes_of(y, LEN).iter().all(|&b| b == 0x22), "the later write stays");
    // Copies in the card's memory (title buffers of 16 MiB or more), as in
    // the failure seen on another card with the late wait: a ping-pong of
    // copies between two of them, and the title writing the source between
    // its own fence waits (write after read, done right).
    const BIG: usize = 16 << 20;
    let (s, d1, d2) = (title_buffer(BIG as u64, 0), title_buffer(BIG as u64, 0), title_buffer(BIG as u64, 0));
    // SAFETY: the source's own memory.
    let source = unsafe { std::slice::from_raw_parts_mut(s.1 as *mut u8, BIG) };
    for (value, round) in (2u64..).zip(0..6u8) {
        // The title writes a few scattered pages of the source, after the
        // fence said the last round's copies are done.
        for page in (0..BIG / 4096).step_by(97) {
            source[page * 4096 + usize::from(round)] = round.wrapping_mul(31).wrapping_add(page as u8);
        }
        submit(queue, &[(d1.0, s.0, BIG as u64)]);
        submit(queue, &[(d2.0, d1.0, BIG as u64)]);
        submit(queue, &[(d1.0, d2.0, BIG as u64)]);
        // SAFETY: Signal of a title queue.
        assert_eq!((unsafe { method::<Signal>(queue, 0x70) })(queue, fence, value), 0);
        let began = std::time::Instant::now();
        while crate::fence::completed(fence) < value {
            assert!(began.elapsed().as_secs() < 30, "round {round}: the fence never reached {value}");
            std::thread::yield_now();
        }
        assert_eq!(crate::descriptor::host::device_removed(), None, "round {round}: the device was lost");
        for (name, d) in [("first", d1), ("second", d2)] {
            let got = bytes_of(d, BIG);
            let wrong = got.iter().zip(source.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(wrong, 0, "round {round}: the {name} copy differs from the source in {wrong} bytes");
        }
    }
    let carded = crate::shadow::find(d1.1, 256).is_some_and(|c| c.carded());
    println!("FLIGHTS-CARD {carded}");
    // The mode asked for is the one that ran: in a late mode, submissions
    // went to the card without a wait.
    let (sent, settles) = crate::host::late_counts();
    let late_asked = std::env::var_os("VICTORY_LATE_WAIT").is_some() || std::env::var_os("VICTORY_RELAY").is_some();
    assert_eq!(crate::host::late(), late_asked, "the late wait is as asked");
    if std::env::var_os("VICTORY_RELAY").is_some() {
        assert!(crate::host::relay(), "the relay is on");
    }
    if late_asked {
        assert!(sent >= CHAIN as u64, "only {sent} submissions sent without a wait");
    }
    debug_layer_report();
    println!("FLIGHTS-CHILD-OK sent ahead {sent}, settles {settles}, late {}, relay {}", crate::host::late(), crate::host::relay());
}

/// The modes the scenario runs in, each a child process.
fn modes() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("synchronous", vec![]),
        ("late wait 1", vec![("VICTORY_LATE_WAIT", "1")]),
        ("late wait 2", vec![("VICTORY_LATE_WAIT", "2")]),
        ("late wait 3", vec![("VICTORY_LATE_WAIT", "3")]),
        ("relay 1", vec![("VICTORY_RELAY", "1")]),
        ("relay 2", vec![("VICTORY_RELAY", "2")]),
    ]
}

/// Every mode of submissions in flight keeps the title's order, its
/// buffers' lifetime and its fence's meaning (the scenario above).
#[test]
fn submissions_in_flight_keep_order_and_lifetime() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    let exe = std::env::current_exe().expect("the test binary");
    let mut failed = Vec::new();
    for (name, vars) in modes() {
        let mut child = std::process::Command::new(&exe);
        child
            .args(["--exact", "flights::flights_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SIXTH_FLIGHTS_CHILD", "1")
            .env_remove("VICTORY_LATE_WAIT")
            .env_remove("VICTORY_RELAY")
            .env_remove("VICTORY_GPU_TIMING");
        for (k, v) in vars {
            child.env(k, v);
        }
        let out = child.output().expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !text.contains("FLIGHTS-CHILD-OK") {
            let why: String = text.lines().filter(|l| l.contains("panicked") || l.contains("assert")).collect::<Vec<_>>().join(" | ");
            failed.push(format!("{name}: {why}"));
        }
    }
    assert!(failed.is_empty(), "modes that failed: {failed:#?}");
}

/// Synthetic: the trade-off of hot copies on the card, through the host's
/// own paths: the copy lookup (with its moves), the copies' write-watch
/// refreshes and sends to the card at submission, and a submission of our
/// own run as the title's are (`Host::execute`), whose compute pass reads
/// every copy whole through a root view, as draws read title memory. Per
/// frame: 512 read-only title buffers of 4 MiB (2 GiB read by the GPU),
/// the title rewriting 1% or 25% of their pages, at hot budgets of 0, 256,
/// 512, 1024 and 2048 MiB. Prints a table; run with
/// `cargo test -p victory-gpu --release --lib residency_bench -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (a few minutes, about 6 GiB of memory)"]
fn residency_bench() {
    use windows::Win32::Graphics::Direct3D12::*;
    let Some(host) = crate::host::host() else {
        return;
    };
    const LEN: usize = 4 << 20;
    const SOURCES: usize = 512;
    const FRAMES: u64 = 48;
    const COUNTED: u64 = 12;
    // SAFETY: object creation on the host's device.
    let (root, pipeline, out) = unsafe {
        let device: &ID3D12Device = &host.device;
        let shader = "ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 uint4 v = t.Load4(id.x * 16);
                 o.Store((id.x & 0x3fff) * 4, v.x ^ v.y ^ v.z ^ v.w);
             }";
        let code = crate::host::compile(shader, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 2, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pipeline: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .expect("pipeline");
        (root, pipeline, host.uav_buffer(64 << 10).expect("out"))
    };
    let mut frame_no = 200_000u64;
    println!("| title rewrites | hot budget MiB | copies on the card | ms/frame | preparing | on the GPU |");
    println!("|---|---|---|---|---|---|");
    for dirty_percent in [1usize, 25] {
        for budget in [0u64, 256, 512, 1024, 2048] {
            // Title memory as the runtime maps it: write-watched, so that a
            // refresh copies only the pages the title wrote.
            // SAFETY: a fresh reservation of our own, released below.
            let region = unsafe {
                windows_sys::Win32::System::Memory::VirtualAlloc(
                    std::ptr::null(),
                    SOURCES * LEN,
                    windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
                    windows_sys::Win32::System::Memory::PAGE_READWRITE,
                )
            } as u64;
            assert_ne!(region, 0, "a write-watched reservation");
            let sources: Vec<(This, u64)> = (0..SOURCES as u64)
                .map(|i| {
                    let at = region + i * LEN as u64;
                    (crate::resource::tests::title_buffer_at(at, LEN as u64), at)
                })
                .collect();
            let pages = LEN / crate::watch::PAGE;
            let mut times = Vec::new();
            let mut split = [0u64; 2];
            for f in 0..FRAMES {
                frame_no += 1;
                crate::shadow::test_hot((budget > 0).then_some(budget), Some(frame_no));
                for (i, s) in sources.iter().enumerate() {
                    for page in 0..pages {
                        if (page * 131 + i * 17 + f as usize * 7) % 100 < dirty_percent {
                            // SAFETY: inside the source's own memory.
                            unsafe { ((s.1 as usize + page * crate::watch::PAGE) as *mut u8).write(f as u8) };
                        }
                    }
                }
                let _ = crate::host::take_submission_us();
                let began = std::time::Instant::now();
                let list = host.new_list().expect("a host list");
                // SAFETY: recording on our own list, with objects that live
                // through its submission.
                unsafe {
                    list.list.SetComputeRootSignature(&root);
                    list.list.SetPipelineState(&pipeline);
                    list.list.SetComputeRootUnorderedAccessView(1, out.GetGPUVirtualAddress());
                }
                for s in &sources {
                    let copy = crate::shadow::find(s.1, LEN as u64).expect("a copy");
                    copy.refresh(&list, s.1, LEN as u64);
                    list.buffer_state(&copy.buffer, crate::host::BUFFER_READ);
                    // SAFETY: as above; the copy's address of the buffer's start.
                    unsafe {
                        list.list.SetComputeRootShaderResourceView(0, copy.gpu(s.1));
                        list.list.Dispatch((LEN / 16 / 256) as u32, 1, 1);
                    }
                }
                list.close();
                host.execute(&[&list]);
                let took = began.elapsed().as_secs_f64() * 1e3;
                let us = crate::host::take_submission_us();
                if f >= FRAMES - COUNTED {
                    times.push(took);
                    split[0] += us[0];
                    split[1] += us[1];
                }
            }
            assert_eq!(crate::descriptor::host::device_removed(), None);
            times.sort_by(f64::total_cmp);
            let carded = sources.iter().filter(|s| crate::shadow::find(s.1, 256).is_some_and(|c| c.carded())).count();
            let per = |us: u64| us as f64 / 1e3 / COUNTED as f64;
            println!(
                "| {dirty_percent}% | {budget} | {carded} of {SOURCES} | {:.1} | {:.1} | {:.1} |",
                times[times.len() / 2],
                per(split[0]),
                per(split[1])
            );
            crate::shadow::test_hot(None, None);
            for s in &sources {
                // SAFETY: Release of a title buffer we made.
                (unsafe { method::<Release>(s.0, 0x10) })(s.0);
            }
            // The copies go once no submission runs; then the memory.
            drop(sources);
            crate::shadow::let_go_idle();
            // SAFETY: our reservation; nothing of the host reads it now
            // (its copies were refreshed from it, not mapped onto it).
            unsafe { windows_sys::Win32::System::Memory::VirtualFree(region as *mut _, 0, windows_sys::Win32::System::Memory::MEM_RELEASE) };
        }
    }
}

/// Synthetic: what the copy-back of shader writes costs, through the host's
/// own paths: writable title buffers in write-watched memory, a compute
/// pass of our own that writes a share of each through a root view,
/// declared as draws declare it (`Shadow::gpu_writes`), and the submission
/// run as the title's are, whose copy-back finds and merges what changed.
/// Copies of 4 MiB are in the PC's memory (compared on the CPU against a
/// snapshot), of 16 MiB on the card (compared on the card). 512 MiB
/// declared per frame, 1% or 10% of its pages written. Run with
/// `cargo test -p victory-gpu --release --lib copyback_bench -- --ignored --nocapture`,
/// and again with `VICTORY_WIDE_BACK=1` for the copy-back on several threads.
#[test]
#[ignore = "a benchmark (about a minute, 2 GiB of memory)"]
fn copyback_bench() {
    use windows::Win32::Graphics::Direct3D12::*;
    let Some(host) = crate::host::host() else {
        return;
    };
    const DECLARED: usize = 512 << 20;
    const FRAMES: u64 = 24;
    const COUNTED: u64 = 10;
    // SAFETY: object creation on the host's device.
    let (root, pipeline) = unsafe {
        let device: &ID3D12Device = &host.device;
        // Root views are not bounds-checked: the last group's spare
        // threads must not write past the buffer.
        let shader = "cbuffer c : register(b0) { uint stride; uint frame; uint threads; }; RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 if (id.x < threads) { o.Store(id.x * stride, id.x ^ frame); }
             }";
        let code = crate::host::compile(shader, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: 3 },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 2, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pipeline: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .expect("pipeline");
        (root, pipeline)
    };
    println!("| copies | pages written | ms/frame | preparing | on the GPU | copying back |");
    println!("|---|---|---|---|---|---|");
    for len in [4usize << 20, 16 << 20] {
        for written_permille in [10u32, 100] {
            let count = DECLARED / len;
            // SAFETY: a fresh reservation of our own, released below.
            let region = unsafe {
                windows_sys::Win32::System::Memory::VirtualAlloc(
                    std::ptr::null(),
                    DECLARED,
                    windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
                    windows_sys::Win32::System::Memory::PAGE_READWRITE,
                )
            } as u64;
            assert_ne!(region, 0);
            let buffers: Vec<(This, u64)> = (0..count as u64)
                .map(|i| {
                    let at = region + i * len as u64;
                    (crate::resource::tests::title_buffer_at_flags(at, len as u64, 4), at)
                })
                .collect();
            // Threads that each write one dword, `stride` bytes apart, over
            // the whole buffer: that share of its 4 KiB pages holds a write.
            let stride = 4096 * 1000 / written_permille;
            let threads = (len as u32 / stride).max(1);
            let mut times = Vec::new();
            let mut split = [0u64; 3];
            // `SIXTH_BENCH_HOT_MB`: hot copies move on (the unified rule).
            let hot = std::env::var("SIXTH_BENCH_HOT_MB").ok().and_then(|v| v.parse::<u64>().ok());
            for f in 0..FRAMES {
                crate::shadow::test_hot(hot, Some(300_000 + f + len as u64));
                let _ = crate::host::take_submission_us();
                let began = std::time::Instant::now();
                let list = host.new_list().expect("a host list");
                // SAFETY: recording on our own list.
                unsafe {
                    list.list.SetComputeRootSignature(&root);
                    list.list.SetPipelineState(&pipeline);
                    list.list.SetComputeRoot32BitConstants(0, 3, [stride, f as u32, threads].as_ptr().cast(), 0);
                }
                for b in &buffers {
                    let copy = crate::shadow::find(b.1, len as u64).expect("a copy");
                    copy.refresh(&list, b.1, len as u64);
                    list.buffer_state(&copy.buffer, crate::host::BUFFER_WRITE);
                    copy.gpu_writes(&list, b.1, len as u64);
                    // SAFETY: as above.
                    unsafe {
                        list.list.SetComputeRootUnorderedAccessView(1, copy.gpu(b.1));
                        list.list.Dispatch(threads.div_ceil(256), 1, 1);
                    }
                }
                list.close();
                host.execute(&[&list]);
                let took = began.elapsed().as_secs_f64() * 1e3;
                let us = crate::host::take_submission_us();
                if f >= FRAMES - COUNTED {
                    times.push(took);
                    for k in 0..3 {
                        split[k] += us[k];
                    }
                }
            }
            assert_eq!(crate::descriptor::host::device_removed(), None);
            // What the shaders wrote last is in title memory.
            let last = (FRAMES - 1) as u32;
            // SAFETY: inside the first buffer's own memory.
            let got = unsafe { ((buffers[0].1 as usize + stride as usize) as *const u32).read() };
            assert_eq!(got, 1 ^ last, "the shader's write came back");
            times.sort_by(f64::total_cmp);
            let carded = crate::shadow::find(buffers[0].1, 256).is_some_and(|c| c.carded());
            let per = |us: u64| us as f64 / 1e3 / COUNTED as f64;
            println!(
                "| {count} x {} MiB ({}) | {:.0}% | {:.1} | {:.1} | {:.1} | {:.1} |",
                len >> 20,
                if carded { "card" } else { "PC memory" },
                f64::from(written_permille) / 10.0,
                times[times.len() / 2],
                per(split[0]),
                per(split[1]),
                per(split[2])
            );
            for b in &buffers {
                // SAFETY: Release of a title buffer we made.
                (unsafe { method::<Release>(b.0, 0x10) })(b.0);
            }
            drop(buffers);
            crate::shadow::let_go_idle();
            // SAFETY: our reservation, no longer read by anything of ours.
            unsafe {
                windows_sys::Win32::System::Memory::VirtualFree(region as *mut _, 0, windows_sys::Win32::System::Memory::MEM_RELEASE)
            };
        }
    }
}

/// Synthetic: one frame shaped like the game's measured one, through the
/// host's own paths, under each combination of switches (each in a child
/// process, since the switches are read once): 1 GiB of read-only title
/// buffers (4 MiB each) read whole by a compute pass, 2% of their pages
/// rewritten by the title each frame; 64 writable 4 MiB and 8 writable
/// 16 MiB buffers declared written, 2% of their pages written by shaders;
/// every list kept until two frames later, as the title keeps its lists
/// until it resets them (so copies are never alone). Run with
/// `cargo test -p victory-gpu --release --lib frame_bench -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (several minutes, about 4 GiB of memory)"]
fn frame_bench() {
    if crate::host::host().is_none() {
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let configs: [(&str, &[(&str, &str)]); 10] = [
        ("defaults", &[]),
        ("hot 1024", &[("SIXTH_BENCH_HOT_MB", "1024")]),
        ("card buffers from 1 MiB", &[("VICTORY_CARD_BUFFERS_MIN_MB", "1"), ("VICTORY_CARD_BUFFERS_MB", "1024")]),
        ("card 1 MiB + hot 1024", &[("VICTORY_CARD_BUFFERS_MIN_MB", "1"), ("VICTORY_CARD_BUFFERS_MB", "1024"), ("SIXTH_BENCH_HOT_MB", "1024")]),
        ("wide refresh", &[("VICTORY_WIDE_REFRESH", "1")]),
        ("keep snapshots + wide back", &[("VICTORY_KEEP_SNAPSHOTS", "1"), ("VICTORY_WIDE_BACK", "1")]),
        (
            "card 1 MiB + hot 1024 + wide refresh",
            &[("VICTORY_CARD_BUFFERS_MIN_MB", "1"), ("VICTORY_CARD_BUFFERS_MB", "1024"), ("SIXTH_BENCH_HOT_MB", "1024"), ("VICTORY_WIDE_REFRESH", "1")],
        ),
        ("late wait 3", &[("VICTORY_LATE_WAIT", "3")]),
        (
            "hot 1024 + keep snapshots + wide back",
            &[("SIXTH_BENCH_HOT_MB", "1024"), ("VICTORY_KEEP_SNAPSHOTS", "1"), ("VICTORY_WIDE_BACK", "1")],
        ),
        (
            "card 1 MiB + hot 1024 + keep snapshots + wide back",
            &[
                ("VICTORY_CARD_BUFFERS_MIN_MB", "1"),
                ("VICTORY_CARD_BUFFERS_MB", "1024"),
                ("SIXTH_BENCH_HOT_MB", "1024"),
                ("VICTORY_KEEP_SNAPSHOTS", "1"),
                ("VICTORY_WIDE_BACK", "1"),
            ],
        ),
    ];
    let mut cpu_lines = Vec::new();
    println!("| switches | ms/frame | preparing | on the GPU | copying back | copies on the card |");
    println!("|---|---|---|---|---|---|");
    for (name, vars) in configs {
        let mut child = std::process::Command::new(&exe);
        child
            .args(["--exact", "flights::frame_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SIXTH_FRAME_CHILD", "1");
        for v in [
            "VICTORY_LATE_WAIT",
            "VICTORY_RELAY",
            "VICTORY_CARD_BUFFERS_MIN_MB",
            "VICTORY_CARD_BUFFERS_MB",
            "VICTORY_WIDE_REFRESH",
            "VICTORY_WIDE_BACK",
            "VICTORY_KEEP_SNAPSHOTS",
            "SIXTH_BENCH_HOT_MB",
        ] {
            child.env_remove(v);
        }
        for (k, v) in vars {
            child.env(k, v);
        }
        let out = child.output().expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((_, cpu)) = text.lines().find_map(|l| l.split_once("FRAME-CPU ")) {
            cpu_lines.push(format!("{name}: {cpu}"));
        }
        match text.lines().find_map(|l| l.split_once("FRAME-ROW ")) {
            Some((_, row)) => println!("| {name} | {row} |"),
            None => println!("| {name} | failed: {} |", String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("panicked") || l.contains("failure")).collect::<Vec<_>>().join(" ")),
        }
    }
    println!();
    for line in cpu_lines {
        println!("{line}");
    }
}


/// Prints the frame times' median, 95th percentile and worst (ms) after
/// `tag`, for the parent that reads the child's output.
fn print_distribution(tag: &str, times: &mut [f64]) {
    if times.is_empty() {
        return;
    }
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!("{tag} {} frames: median {:.1}, p95 {:.1}, worst {:.1}", times.len(), at(0.5), at(0.95), times[times.len() - 1]);
}

/// [`frame_bench`]'s frame, in a child process.
#[test]
#[ignore = "run by frame_bench in a child process"]
fn frame_child() {
    use windows::Win32::Graphics::Direct3D12::*;
    if std::env::var_os("SIXTH_FRAME_CHILD").is_none() {
        return;
    }
    let Some(host) = crate::host::host() else {
        return;
    };
    const READ: usize = 256;
    const SMALL: usize = 64;
    const BIG: usize = 8;
    // `SIXTH_BENCH_FRAMES`: more frames, for a soak (the host's memory is
    // then checked not to grow).
    let frames: u64 = std::env::var("SIXTH_BENCH_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    const COUNTED: u64 = 12;
    let mut alive_at_start = None;
    let mut all_times: Vec<f64> = Vec::new();
    let hot = std::env::var("SIXTH_BENCH_HOT_MB").ok().and_then(|v| v.parse::<u64>().ok());
    // SAFETY: object creation on the host's device.
    let (root, read_pass, write_pass) = unsafe {
        let device: &ID3D12Device = &host.device;
        let read = "cbuffer c : register(b0) { uint stride; uint frame; uint threads; }; ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 if (id.x < threads) { uint4 v = t.Load4(id.x * 16); o.Store((id.x & 0x3fff) * 4, v.x ^ v.y ^ v.z ^ v.w); }
             }";
        let write = "cbuffer c : register(b0) { uint stride; uint frame; uint threads; }; ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 if (id.x < threads) { o.Store(id.x * stride, (id.x + 1) * (frame + 1)); }
             }";
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: 3 },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 3, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pass = |src: &str| -> ID3D12PipelineState {
            let code = crate::host::compile(src, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
            device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                    ..Default::default()
                })
                .expect("pipeline")
        };
        (root.clone(), pass(read), pass(write))
    };
    let out = host.uav_buffer(64 << 10).expect("out");
    // Title memory, write-watched, as the runtime maps it.
    const MIB4: usize = 4 << 20;
    const MIB16: usize = 16 << 20;
    let total = READ * MIB4 + SMALL * MIB4 + BIG * MIB16;
    // SAFETY: a fresh reservation of our own, never freed (the process ends).
    let region = unsafe {
        windows_sys::Win32::System::Memory::VirtualAlloc(
            std::ptr::null(),
            total,
            windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
            windows_sys::Win32::System::Memory::PAGE_READWRITE,
        )
    } as u64;
    assert_ne!(region, 0);
    let mut at = region;
    let mut make = |len: usize, flags: u32| {
        let b = (crate::resource::tests::title_buffer_at_flags(at, len as u64, flags), at, len);
        at += len as u64;
        b
    };
    let reads: Vec<(This, u64, usize)> = (0..READ).map(|_| make(MIB4, 0)).collect();
    let mut writes: Vec<(This, u64, usize)> = (0..SMALL).map(|_| make(MIB4, 4)).collect();
    writes.extend((0..BIG).map(|_| make(MIB16, 4)));
    // Shaders write 2% of each buffer: one dense block from its start (as
    // shader output is), or with `SIXTH_BENCH_SPARSE=1` one dword every
    // 200 KiB.
    let sparse = std::env::var_os("SIXTH_BENCH_SPARSE").is_some();
    let stride = if sparse { 200u32 << 10 } else { 4 };
    let mut held: std::collections::VecDeque<crate::host::HostList> = std::collections::VecDeque::new();
    let (mut times, mut split) = (Vec::new(), [0u64; 3]);
    for f in 0..frames {
        if f == frames - COUNTED {
            // The CPU time by kind from here (`crate::cpuprof`), and the
            // bytes refreshes copied.
            let _ = crate::cpuprof::take();
            crate::host::PREPARING[1].store(0, std::sync::atomic::Ordering::Relaxed);
        }
        crate::shadow::test_hot(hot, Some(400_000 + f));
        // The title's writes: 2% of the read buffers' pages.
        for (i, b) in reads.iter().enumerate() {
            for page in 0..b.2 / crate::watch::PAGE {
                if (page * 131 + i * 17 + f as usize * 7) % 100 < 2 {
                    // SAFETY: inside the buffer's own memory.
                    unsafe { ((b.1 as usize + page * crate::watch::PAGE) as *mut u8).write(f as u8) };
                }
            }
        }
        let _ = crate::host::take_submission_us();
        let began = std::time::Instant::now();
        let list = host.new_list().expect("a host list");
        // SAFETY: recording on our own list; what it names lives on.
        unsafe {
            list.list.SetComputeRootSignature(&root);
            list.list.SetComputeRootUnorderedAccessView(2, out.GetGPUVirtualAddress());
            list.list.SetPipelineState(&read_pass);
        }
        for b in &reads {
            let copy = crate::shadow::find(b.1, b.2 as u64).expect("a copy");
            copy.refresh(&list, b.1, b.2 as u64);
            list.buffer_state(&copy.buffer, crate::host::BUFFER_READ);
            let threads = (b.2 / 16) as u32;
            // SAFETY: as above.
            unsafe {
                list.list.SetComputeRoot32BitConstants(0, 3, [0u32, f as u32, threads].as_ptr().cast(), 0);
                list.list.SetComputeRootShaderResourceView(1, copy.gpu(b.1));
                list.list.Dispatch(threads.div_ceil(256), 1, 1);
            }
        }
        // SAFETY: as above.
        unsafe { list.list.SetPipelineState(&write_pass) };
        for b in &writes {
            let copy = crate::shadow::find(b.1, b.2 as u64).expect("a copy");
            copy.refresh(&list, b.1, b.2 as u64);
            list.buffer_state(&copy.buffer, crate::host::BUFFER_WRITE);
            copy.gpu_writes(&list, b.1, b.2 as u64);
            let threads = if sparse { b.2 as u32 / stride } else { b.2 as u32 / 50 / 4 };
            // SAFETY: as above.
            unsafe {
                list.list.SetComputeRoot32BitConstants(0, 3, [stride, f as u32, threads].as_ptr().cast(), 0);
                list.list.SetComputeRootUnorderedAccessView(2, copy.gpu(b.1));
                list.list.Dispatch(threads.div_ceil(256), 1, 1);
                list.list.SetComputeRootUnorderedAccessView(2, out.GetGPUVirtualAddress());
            }
        }
        list.close();
        host.execute(&[&list]);
        if crate::host::late() {
            // The frame's end, as a present is: everything in the air lands.
            let done = host.new_list().expect("a host list");
            done.close();
            host.execute(&[&done]);
        }
        let took = began.elapsed().as_secs_f64() * 1e3;
        let us = crate::host::take_submission_us();
        if f == 50.min(frames / 2) {
            alive_at_start = Some(crate::ledger::alive_total());
        }
        // Kept two frames, as the title keeps its lists until it resets them.
        held.push_back(list);
        if held.len() > 2 {
            held.pop_front();
        }
        // After the warm-up: the first frames move copies to the card.
        if f >= (frames / 10).max(40) {
            all_times.push(took);
        }
        if f >= frames - COUNTED {
            times.push(took);
            for k in 0..3 {
                split[k] += us[k];
            }
        }
    }
    assert_eq!(crate::descriptor::host::device_removed(), None);
    if frames >= 500
        && let Some((count, bytes)) = alive_at_start
    {
        drop(held);
        crate::shadow::let_go_idle();
        let (now_count, now_bytes) = crate::ledger::alive_total();
        println!("FRAME-SOAK alive from frame 50 to {frames}: {count} -> {now_count} resources, {} -> {} MiB", bytes >> 20, now_bytes >> 20);
        assert!(now_bytes <= bytes + (64 << 20), "the host's memory grew from {} to {} MiB", bytes >> 20, now_bytes >> 20);
    }
    // The last frame's shader writes are in title memory.
    let last = (frames - 1) as u32;
    for b in &writes {
        // SAFETY: inside the buffer's own memory.
        let got = unsafe { ((b.1 as usize + stride as usize) as *const u32).read() };
        assert_eq!(got, 2 * (last + 1), "a shader write came back");
    }
    times.sort_by(f64::total_cmp);
    let carded = reads
        .iter()
        .chain(&writes)
        .filter(|b| crate::shadow::find(b.1, 256).is_some_and(|c| c.carded()))
        .count();
    let per = |us: u64| us as f64 / 1e3 / COUNTED as f64;
    print_distribution("FRAME-DIST", &mut all_times);
    if let Some(cpu) = crate::cpuprof::line(COUNTED as f64) {
        let refreshed = crate::host::PREPARING[1].load(std::sync::atomic::Ordering::Relaxed) as f64 / COUNTED as f64 / f64::from(1u32 << 20);
        println!(
            "FRAME-CPU {cpu}; refreshes copied {refreshed:.1} MiB a frame; pages noted current after copy-back {}",
            crate::shadow::BACK_NOTED.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
    debug_layer_report();
    println!(
        "FRAME-ROW {:.1} | {:.1} | {:.1} | {:.1} | {carded} of {}",
        times[times.len() / 2],
        per(split[0]),
        per(split[1]),
        per(split[2]),
        reads.len() + writes.len()
    );
}

/// Synthetic: submissions in flight, through the title's own queue: eight
/// submissions a frame, each copying eight read-only 4 MiB title buffers
/// into title textures (GPU work in the host, reading the buffers' host
/// copies after their refresh), the title rewriting 25% of the buffers'
/// pages before each submission, and the frame ending in a wait for the
/// title's fence, as a frame's end does. Each mode in a child process (the
/// switches are read once). Run with
/// `cargo test -p victory-gpu --release --lib late_bench -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (about a minute)"]
fn late_bench() {
    if crate::host::host().is_none() {
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let modes: [(&str, &[(&str, &str)]); 6] = [
        ("synchronous", &[]),
        ("late wait 1", &[("VICTORY_LATE_WAIT", "1")]),
        ("late wait 3", &[("VICTORY_LATE_WAIT", "3")]),
        ("relay 1", &[("VICTORY_RELAY", "1")]),
        ("relay 2", &[("VICTORY_RELAY", "2")]),
        ("relay 1 + hot 1024", &[("VICTORY_RELAY", "1"), ("SIXTH_BENCH_HOT_MB", "1024")]),
    ];
    println!("| mode | ms/frame | preparing | on the GPU | submissions sent ahead |");
    println!("|---|---|---|---|---|");
    for (name, vars) in modes {
        let mut child = std::process::Command::new(&exe);
        child
            .args(["--exact", "flights::late_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SIXTH_LATE_CHILD", "1")
            .env_remove("VICTORY_LATE_WAIT")
            .env_remove("VICTORY_RELAY")
            .env_remove("SIXTH_BENCH_HOT_MB");
        for (k, v) in vars {
            child.env(k, v);
        }
        let out = child.output().expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        match text.lines().find_map(|l| l.split_once("LATE-ROW ")) {
            Some((_, row)) => println!("| {name} | {row} |"),
            None => println!(
                "| {name} | failed: {} |",
                String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("panicked") || l.contains("failure")).collect::<Vec<_>>().join(" ")
            ),
        }
    }
}

/// [`late_bench`]'s frames, in a child process.
#[test]
#[ignore = "run by late_bench in a child process"]
fn late_child() {
    if std::env::var_os("SIXTH_LATE_CHILD").is_none() || crate::host::host().is_none() {
        return;
    }
    #[repr(C)]
    struct Footprint {
        offset: u64,
        format: u32,
        width: u32,
        height: u32,
        depth: u32,
        row_pitch: u32,
    }
    #[repr(C)]
    struct Location {
        resource: This,
        kind: u32,
        footprint: Footprint,
    }
    type CopyTexture = extern "system" fn(This, *const Location, u32, u32, u32, *const Location, *const u8);
    const SIDE: u32 = 1024;
    const LEN: usize = (SIDE * SIDE * 4) as usize;
    const PER: usize = 8;
    const SUBMISSIONS: usize = 8;
    // `SIXTH_BENCH_FRAMES`: more frames, for a soak (the host's memory is
    // then checked not to grow).
    let frames: u64 = std::env::var("SIXTH_BENCH_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    const COUNTED: u64 = 10;
    let hot = std::env::var("SIXTH_BENCH_HOT_MB").ok().and_then(|v| v.parse::<u64>().ok());
    let mut alive_at_start = None;
    let mut all_times: Vec<f64> = Vec::new();
    let null = std::ptr::null();
    let queue_iid = iid(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);
    let fence_iid = iid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
    let mut queue = null;
    let desc = [0u32; 4];
    assert_eq!(crate::queue::create(null, desc.as_ptr().cast(), &queue_iid, &mut queue), 0);
    let mut fence = null;
    assert_eq!(crate::fence::create(null, 0, 0, &fence_iid, &mut fence), 0);
    let count = PER * SUBMISSIONS;
    // SAFETY: a fresh write-watched reservation of our own, never freed.
    let region = unsafe {
        windows_sys::Win32::System::Memory::VirtualAlloc(
            std::ptr::null(),
            count * LEN,
            windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
            windows_sys::Win32::System::Memory::PAGE_READWRITE,
        )
    } as u64;
    assert_ne!(region, 0);
    let buffers: Vec<(This, u64)> = (0..count as u64)
        .map(|i| {
            let at = region + i * LEN as u64;
            (crate::resource::tests::title_buffer_at(at, LEN as u64), at)
        })
        .collect();
    let textures: Vec<This> = (0..count).map(|_| crate::resource::tests::title_texture(SIDE, SIDE)).collect();
    let pages = LEN / crate::watch::PAGE;
    let (mut times, mut split) = (Vec::new(), [0u64; 2]);
    let mut value = 0u64;
    let (sent_before, _) = crate::host::late_counts();
    let mut sent_counted = 0;
    // One allocator and list per submission of the frame, reset and used
    // again every frame, as a title does.
    let pairs: Vec<(This, This)> = (0..SUBMISSIONS).map(|_| new_pair()).collect();
    for f in 0..frames {
        if f > 0 {
            pairs.iter().for_each(|&p| reuse(p));
        }
        if f == frames - COUNTED {
            let _ = crate::cpuprof::take();
        }
        crate::shadow::test_hot(hot, Some(500_000 + f));
        let _ = crate::host::take_submission_us();
        let (sent0, _) = crate::host::late_counts();
        let began = std::time::Instant::now();
        for s in 0..SUBMISSIONS {
            // The title's writes before this submission: 25% of the pages
            // of the buffers it copies.
            for (i, b) in buffers.iter().enumerate().skip(s * PER).take(PER) {
                for page in 0..pages {
                    if (page * 131 + i * 17 + f as usize * 7).is_multiple_of(4) {
                        // SAFETY: inside the buffer's own memory.
                        unsafe { ((b.1 as usize + page * crate::watch::PAGE) as *mut u8).write(f as u8) };
                    }
                }
            }
            let list = pairs[s].1;
            for i in s * PER..(s + 1) * PER {
                let rows = Footprint { offset: 0, format: 28, width: SIDE, height: SIDE, depth: 1, row_pitch: SIDE * 4 };
                let to = Location { resource: textures[i], kind: 0, footprint: Footprint { offset: 0, ..rows } };
                let from = Location { resource: buffers[i].0, kind: 1, footprint: rows };
                // SAFETY: CopyTextureRegion of a title list.
                (unsafe { method::<CopyTexture>(list, 0x80) })(list, &to, 0, 0, 0, &from, null.cast());
            }
            // SAFETY: Close of a title list, ExecuteCommandLists of a title queue.
            unsafe {
                method::<Close>(list, 0x48)(list);
                method::<Execute>(queue, 0x50)(queue, 1, &list);
            }
        }
        value += 1;
        // SAFETY: Signal of a title queue.
        assert_eq!((unsafe { method::<Signal>(queue, 0x70) })(queue, fence, value), 0);
        let waited = std::time::Instant::now();
        while crate::fence::completed(fence) < value {
            assert!(waited.elapsed().as_secs() < 30, "the fence never reached {value}");
            std::thread::yield_now();
        }
        let took = began.elapsed().as_secs_f64() * 1e3;
        let us = crate::host::take_submission_us();
        if f == 50.min(frames / 2) {
            alive_at_start = Some(crate::ledger::alive_total());
        }
        // After the warm-up: the first frames move copies to the card.
        if f >= (frames / 10).max(40) {
            all_times.push(took);
        }
        if f >= frames - COUNTED {
            times.push(took);
            split[0] += us[0];
            split[1] += us[1];
            sent_counted += crate::host::late_counts().0 - sent0;
        }
    }
    assert_eq!(crate::descriptor::host::device_removed(), None);
    let _ = sent_before;
    // Over a soak, what the host has alive does not grow (count, bytes).
    if frames >= 500
        && let Some((count, bytes)) = alive_at_start
    {
        let (now_count, now_bytes) = crate::ledger::alive_total();
        println!("LATE-SOAK alive from frame 50 to {frames}: {count} -> {now_count} resources, {} -> {} MiB", bytes >> 20, now_bytes >> 20);
        assert!(now_bytes <= bytes + (64 << 20), "the host's memory grew from {} to {} MiB", bytes >> 20, now_bytes >> 20);
    }
    // The textures hold the buffers' bytes as the title left them: eight of
    // them read back from the card, every row compared.
    {
        use windows::Win32::Graphics::Direct3D12::*;
        use windows::core::Interface;
        let host = crate::host::host().expect("a host");
        for i in (0..count).step_by(count / 8) {
            let texture = textures[i];
            let image = crate::resource::host_resource(texture).expect("a host texture").clone();
            let back = host.read_back_buffer(LEN as u64).expect("a read-back buffer");
            let list = host.new_list().expect("a host list");
            let before = crate::resource::host_transition(texture, 0x800).map_or(0x800, |(_, before)| before);
            let rows = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
                    Width: SIDE,
                    Height: SIDE,
                    Depth: 1,
                    RowPitch: SIDE * 4,
                },
            };
            // SAFETY: an open list of ours, live resources.
            unsafe {
                if before != 0x800 {
                    list.list.ResourceBarrier(&[crate::host::transition(
                        &image,
                        D3D12_RESOURCE_STATES(before as i32),
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    )]);
                }
                list.list.CopyTextureRegion(
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&back),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: rows },
                    },
                    0,
                    0,
                    0,
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&image),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                    },
                    None,
                );
            }
            list.close();
            host.execute(&[&list]);
            let mut mapped = std::ptr::null_mut();
            // SAFETY: the copy has run (execute waits); LEN bytes.
            let got = unsafe {
                back.Map(0, None, Some(&mut mapped)).expect("mapped");
                std::slice::from_raw_parts(mapped.cast::<u8>(), LEN)
            };
            // SAFETY: the buffer's own memory, LEN bytes.
            let want = unsafe { std::slice::from_raw_parts(buffers[i].1 as *const u8, LEN) };
            let wrong = got.iter().zip(want).filter(|(a, b)| a != b).count();
            assert_eq!(wrong, 0, "texture {i} differs from its buffer in {wrong} bytes");
            let _ = image.as_raw();
        }
    }
    times.sort_by(f64::total_cmp);
    let per = |us: u64| us as f64 / 1e3 / COUNTED as f64;
    print_distribution("LATE-DIST", &mut all_times);
    if let Some(cpu) = crate::cpuprof::line(COUNTED as f64) {
        println!("LATE-CPU {cpu}");
    }
    debug_layer_report();
    println!(
        "LATE-ROW {:.1} | {:.1} | {:.1} | {:.1} a frame",
        times[times.len() / 2],
        per(split[0]),
        per(split[1]),
        sent_counted as f64 / COUNTED as f64
    );
}

/// Differential: whatever copy the GPU reads, it reads the title's bytes.
/// Read-only title buffers in write-watched memory, the title writing
/// random words into them every frame, hot copies moving to the card
/// (twins kept for the ones a "slot" still shows, then let go), and a
/// compute pass reading words at offsets that change every frame, through
/// the copy the lookup gives and, for the slot-shown ones, through the
/// buffer the slot keeps. What it read comes back to title memory through
/// the production copy-back of a writable title buffer, and every word is
/// compared with title memory as the title left it.
#[test]
fn what_the_gpu_reads_is_the_titles_bytes_whichever_copy_it_reads() {
    let _one = crate::shadow::HOT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    use windows::Win32::Graphics::Direct3D12::*;
    let Some(host) = crate::host::host() else {
        return; // no host GPU here
    };
    const LEN: usize = 256 << 10;
    const SOURCES: usize = 24;
    const SLOTTED: usize = 8;
    const READS: u32 = 64;
    const FRAMES: u64 = 32;
    // SAFETY: object creation on the host's device.
    let (root, pipeline) = unsafe {
        let device: &ID3D12Device = &host.device;
        let shader = "cbuffer c : register(b0) { uint seed; uint words; uint out_at; }; ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(64, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 uint at = ((id.x * 2654435761u + seed) % words) * 4;
                 o.Store(out_at + id.x * 4, t.Load(at));
             }";
        let code = crate::host::compile(shader, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: 3 },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 3, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pipeline: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .expect("pipeline");
        (root, pipeline)
    };
    // Title memory, write-watched: the sources, then the output.
    let out_len = (SOURCES + SLOTTED) * READS as usize * 4;
    let total = SOURCES * LEN + out_len.next_multiple_of(64 << 10);
    // SAFETY: a fresh reservation of our own, never freed (other tests'
    // copies may outlive this one's).
    let region = unsafe {
        windows_sys::Win32::System::Memory::VirtualAlloc(
            std::ptr::null(),
            total,
            windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
            windows_sys::Win32::System::Memory::PAGE_READWRITE,
        )
    } as u64;
    assert_ne!(region, 0);
    let sources: Vec<(This, u64)> = (0..SOURCES as u64)
        .map(|i| {
            let at = region + i * LEN as u64;
            (crate::resource::tests::title_buffer_at(at, LEN as u64), at)
        })
        .collect();
    let out_at = region + (SOURCES * LEN) as u64;
    let _out = crate::resource::tests::title_buffer_at_flags(out_at, out_len as u64, 4);
    let word = |address: u64| -> u32 {
        // SAFETY: inside our reservation.
        unsafe { (address as *const u32).read_unaligned() }
    };
    // The slots: the buffer the copy of each of the first SLOTTED sources
    // had when the slot was made, kept as a slot keeps it.
    let mut slots: Vec<Option<ID3D12Resource>> = vec![None; SLOTTED];
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let (mut moved, mut twinned, mut checked) = (0usize, 0usize, 0usize);
    for f in 0..FRAMES {
        crate::shadow::test_hot(Some(64), Some(600_000 + f));
        // The title writes random words of every source.
        for s in &sources {
            for _ in 0..32 {
                let at = s.1 + (next() % (LEN as u64 / 4)) * 4;
                // SAFETY: inside the source's own memory.
                unsafe { (at as *mut u32).write_unaligned(next() as u32) };
            }
        }
        if f == FRAMES / 2 {
            // The slots let go: their twins go at the next sweep.
            slots.iter_mut().for_each(|s| *s = None);
        }
        let list = host.new_list().expect("a host list");
        let out_copy = crate::shadow::find(out_at, out_len as u64).expect("the output's copy");
        out_copy.refresh(&list, out_at, out_len as u64);
        list.buffer_state(&out_copy.buffer, crate::host::BUFFER_WRITE);
        out_copy.gpu_writes(&list, out_at, out_len as u64);
        // SAFETY: recording on our own list; what it names lives on.
        unsafe {
            list.list.SetComputeRootSignature(&root);
            list.list.SetPipelineState(&pipeline);
            list.list.SetComputeRootUnorderedAccessView(2, out_copy.gpu(out_at));
        }
        let seed = (f as u32).wrapping_mul(0x9e37_79b9);
        let mut reads: Vec<(u64, usize)> = Vec::new(); // (source address, output slot)
        for (i, s) in sources.iter().enumerate() {
            let copy = crate::shadow::find(s.1, LEN as u64).expect("a copy");
            if i < SLOTTED && f < FRAMES / 2 && slots[i].is_none() {
                slots[i] = Some(copy.buffer.clone());
            }
            copy.refresh(&list, s.1, LEN as u64);
            list.buffer_state(&copy.buffer, crate::host::BUFFER_READ);
            let mut through = vec![copy.gpu(s.1)];
            // Through the slot's buffer too, while it is the replaced copy's.
            if let Some(slot) = slots.get(i).and_then(Option::as_ref)
                && copy.replaced(slot)
            {
                list.buffer_state(slot, crate::host::BUFFER_READ);
                // SAFETY: a live buffer; the slot keeps it.
                through.push(unsafe { slot.GetGPUVirtualAddress() });
                twinned += 1;
            }
            if copy.carded() {
                moved += 1;
            }
            for gpu in through {
                let slot_at = reads.len();
                reads.push((s.1, slot_at));
                // SAFETY: as above.
                unsafe {
                    list.list.SetComputeRoot32BitConstants(0, 3, [seed ^ i as u32, (LEN / 4) as u32, (slot_at as u32) * READS * 4].as_ptr().cast(), 0);
                    list.list.SetComputeRootShaderResourceView(1, gpu);
                    list.list.Dispatch(1, 1, 1);
                }
            }
        }
        list.close();
        host.execute(&[&list]);
        drop(list);
        crate::shadow::let_go_idle();
        // What the GPU read, back in title memory, against title memory.
        for &(source, slot_at) in &reads {
            let i = sources.iter().position(|s| s.1 == source).expect("a source");
            for k in 0..READS {
                let at = ((u64::from(k).wrapping_mul(2_654_435_761) as u32).wrapping_add(seed ^ i as u32) % (LEN as u32 / 4)) as u64 * 4;
                let got = word(out_at + ((slot_at as u64) * u64::from(READS) + u64::from(k)) * 4);
                assert_eq!(got, word(source + at), "frame {f}, source {i}, read {k} through copy {slot_at}");
                checked += 1;
            }
        }
    }
    crate::shadow::test_hot(None, None);
    assert_eq!(crate::descriptor::host::device_removed(), None);
    assert!(moved > 0, "copies moved to the card");
    assert!(twinned > 0, "twins were read through");
    assert!(checked > 0);
}

/// Differential, for copies shaders write: writable title buffers whose
/// first half the title writes and whose second half a shader writes
/// whole every frame (declared written), with the title also writing a
/// word into the second half now and then between frames (after its fence,
/// as a title may). Copies move to the card when nothing else holds them,
/// and pages a copy-back wrote whole are noted current. Every frame a pass
/// reads words all over each buffer before the shader writes it: each must
/// be what title memory held when the frame was submitted, the title's
/// words in the second half included (a page noted current wrongly would
/// hide them). After the frame the second half must hold the shader's
/// words in title memory.
#[test]
fn writes_back_and_title_writes_both_reach_whichever_copy_is_read() {
    let _one = crate::shadow::HOT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    use windows::Win32::Graphics::Direct3D12::*;
    let Some(host) = crate::host::host() else {
        return; // no host GPU here
    };
    const LEN: usize = 512 << 10;
    const HALF: usize = LEN / 2;
    const BUFFERS: usize = 8;
    const READS: u32 = 64;
    const FRAMES: u64 = 30;
    // SAFETY: object creation on the host's device.
    let (root, read_pass, write_pass) = unsafe {
        let device: &ID3D12Device = &host.device;
        let read = "cbuffer c : register(b0) { uint seed; uint words; uint out_at; }; ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(64, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 uint at = ((id.x * 2654435761u + seed) % words) * 4;
                 o.Store(out_at + id.x * 4, t.Load(at));
             }";
        let write = "cbuffer c : register(b0) { uint seed; uint words; uint out_at; }; ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 if (id.x < words) { o.Store(id.x * 4, (id.x + 1) * (seed + 1)); }
             }";
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: 3 },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 3, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pass = |src: &str| -> ID3D12PipelineState {
            let code = crate::host::compile(src, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
            device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                    ..Default::default()
                })
                .expect("pipeline")
        };
        (root.clone(), pass(read), pass(write))
    };
    let out_len = BUFFERS * READS as usize * 4;
    let total = BUFFERS * LEN + out_len.next_multiple_of(64 << 10);
    // SAFETY: a fresh write-watched reservation of our own, never freed.
    let region = unsafe {
        windows_sys::Win32::System::Memory::VirtualAlloc(
            std::ptr::null(),
            total,
            windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
            windows_sys::Win32::System::Memory::PAGE_READWRITE,
        )
    } as u64;
    assert_ne!(region, 0);
    let buffers: Vec<(This, u64)> = (0..BUFFERS as u64)
        .map(|i| {
            let at = region + i * LEN as u64;
            (crate::resource::tests::title_buffer_at_flags(at, LEN as u64, 4), at)
        })
        .collect();
    let out_at = region + (BUFFERS * LEN) as u64;
    let _out = crate::resource::tests::title_buffer_at_flags(out_at, out_len as u64, 4);
    let word = |address: u64| -> u32 {
        // SAFETY: inside our reservation.
        unsafe { (address as *const u32).read_unaligned() }
    };
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let (mut moved, mut checked) = (0usize, 0usize);
    let noted_before = crate::shadow::BACK_NOTED.load(std::sync::atomic::Ordering::Relaxed);
    for f in 0..FRAMES {
        crate::shadow::test_hot(Some(64), Some(700_000 + f));
        for b in &buffers {
            for _ in 0..16 {
                let at = b.1 + (next() % (HALF as u64 / 4)) * 4;
                // SAFETY: inside the buffer's own memory.
                unsafe { (at as *mut u32).write_unaligned(next() as u32) };
            }
            if f % 3 == 1 {
                // The title writes the shaders' half too, after its fence.
                let at = b.1 + HALF as u64 + (next() % (HALF as u64 / 4)) * 4;
                // SAFETY: as above.
                unsafe { (at as *mut u32).write_unaligned(0xdead_0000 | f as u32) };
            }
        }
        // Title memory as submitted: what the reads must find.
        let expected: Vec<Vec<u32>> = buffers
            .iter()
            // SAFETY: the buffers' own memory, LEN bytes each.
            .map(|b| unsafe { std::slice::from_raw_parts(b.1 as *const u32, LEN / 4) }.to_vec())
            .collect();
        let list = host.new_list().expect("a host list");
        let out_copy = crate::shadow::find(out_at, out_len as u64).expect("the output's copy");
        out_copy.refresh(&list, out_at, out_len as u64);
        list.buffer_state(&out_copy.buffer, crate::host::BUFFER_WRITE);
        out_copy.gpu_writes(&list, out_at, out_len as u64);
        // SAFETY: recording on our own list; what it names lives on.
        unsafe {
            list.list.SetComputeRootSignature(&root);
            list.list.SetPipelineState(&read_pass);
            list.list.SetComputeRootUnorderedAccessView(2, out_copy.gpu(out_at));
        }
        let seed = (f as u32).wrapping_mul(0x85eb_ca6b);
        let mut copies = Vec::new();
        for (i, b) in buffers.iter().enumerate() {
            let copy = crate::shadow::find(b.1, LEN as u64).expect("a copy");
            if copy.carded() {
                moved += 1;
            }
            copy.refresh(&list, b.1, LEN as u64);
            list.buffer_state(&copy.buffer, crate::host::BUFFER_READ);
            // SAFETY: as above.
            unsafe {
                list.list.SetComputeRoot32BitConstants(0, 3, [seed ^ i as u32, (LEN / 4) as u32, (i as u32) * READS * 4].as_ptr().cast(), 0);
                list.list.SetComputeRootShaderResourceView(1, copy.gpu(b.1));
                list.list.Dispatch(1, 1, 1);
            }
            copies.push(copy);
        }
        // SAFETY: as above.
        unsafe { list.list.SetPipelineState(&write_pass) };
        for (b, copy) in buffers.iter().zip(&copies) {
            let second = b.1 + HALF as u64;
            list.buffer_state(&copy.buffer, crate::host::BUFFER_WRITE);
            copy.gpu_writes(&list, second, HALF as u64);
            // SAFETY: as above.
            unsafe {
                list.list.SetComputeRoot32BitConstants(0, 3, [f as u32, (HALF / 4) as u32, 0].as_ptr().cast(), 0);
                list.list.SetComputeRootUnorderedAccessView(2, copy.gpu(second));
                list.list.Dispatch((HALF / 4).div_ceil(256) as u32, 1, 1);
                list.list.SetComputeRootUnorderedAccessView(2, out_copy.gpu(out_at));
            }
        }
        list.close();
        host.execute(&[&list]);
        drop(list);
        drop(copies);
        crate::shadow::let_go_idle();
        for i in 0..BUFFERS {
            for k in 0..READS {
                let at = (k.wrapping_mul(2_654_435_761).wrapping_add(seed ^ i as u32) % (LEN as u32 / 4)) as usize;
                let got = word(out_at + ((i as u64) * u64::from(READS) + u64::from(k)) * 4);
                assert_eq!(got, expected[i][at], "frame {f}, buffer {i}, read {k} (word {at})");
                checked += 1;
            }
            // The shader's words are in title memory now.
            for w in [0usize, 1, HALF / 4 / 2, HALF / 4 - 1] {
                let got = word(buffers[i].1 + (HALF + w * 4) as u64);
                assert_eq!(got, (w as u32 + 1).wrapping_mul(f as u32 + 1), "frame {f}, buffer {i}: shader word {w} came back");
            }
        }
    }
    crate::shadow::test_hot(None, None);
    assert_eq!(crate::descriptor::host::device_removed(), None);
    assert!(moved > 0, "writable copies moved to the card");
    assert!(crate::shadow::BACK_NOTED.load(std::sync::atomic::Ordering::Relaxed) > noted_before, "pages were noted");
    assert!(checked > 0);
}

/// Memory pressure: the card filled past its target must not make anything
/// missing. With the card past nine tenths of its budget: a texture the
/// title fills is still made (in the PC's memory), hot copies do not move
/// up, and every copy still reads as title memory; once the filler goes,
/// the texture comes back to the card and the hot copies move. Ignored:
/// filling the card would change what other tests running beside it see.
/// Run with `cargo test -p victory-gpu --release --lib pressure_stress -- --ignored --nocapture`.
/// The texture fallback is off by default (`crate::residency::on`): run it
/// with `VICTORY_RESIDENCY=1` as well to check the fallback; without, the
/// texture is made on the card as the inherited rule makes it.
#[test]
#[ignore = "fills the card's memory"]
fn pressure_stress() {
    let _one = crate::shadow::HOT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(host) = crate::host::host() else {
        return;
    };
    let share = || {
        let b = crate::residency::budget().expect("a budget");
        (b.local_used as f64 / b.local_budget as f64, b.local_used >> 20, b.local_budget >> 20)
    };
    let (before, used, budget) = share();
    println!("before: {:.0}% of the budget ({used} of {budget} MiB)", before * 100.0);
    // Filler on the card until past the target (90%), in 64 MiB pieces.
    let mut filler = Vec::new();
    while share().0 < 0.93 {
        let Some(b) = host.uav_buffer(64 << 20) else {
            break;
        };
        filler.push(b);
        assert!(filler.len() < 400, "the card never filled");
    }
    let (full, used, _) = share();
    let (need, pressure) = crate::residency::need();
    println!("filled: {:.0}% ({used} MiB, {} pieces), pressure {pressure:?}, {} MiB to give up", full * 100.0, filler.len(), need >> 20);
    assert!(pressure >= sixth_memory::residency::Pressure::High);
    // A texture the title fills: made; in the PC's memory with the
    // fallback, on the card (the inherited rule) without.
    let fallback = crate::residency::on();
    println!("texture fallback (VICTORY_RESIDENCY): {}", if fallback { "on" } else { "off, the default" });
    let texture = crate::resource::tests::placed_rgba();
    let first = crate::resource::host_resource(texture).expect("made under pressure, not refused").clone();
    assert_eq!(crate::residency::in_system(&first), fallback, "in the PC's memory while the card is full only with the fallback");
    // Hot copies do not move up while the card is full.
    const LEN: usize = 4 << 20;
    // SAFETY: a fresh write-watched reservation of our own, never freed.
    let region = unsafe {
        windows_sys::Win32::System::Memory::VirtualAlloc(
            std::ptr::null(),
            8 * LEN,
            windows_sys::Win32::System::Memory::MEM_RESERVE | windows_sys::Win32::System::Memory::MEM_COMMIT | 0x20_0000,
            windows_sys::Win32::System::Memory::PAGE_READWRITE,
        )
    } as u64;
    let buffers: Vec<(This, u64)> = (0..8u64)
        .map(|i| {
            let at = region + i * LEN as u64;
            (crate::resource::tests::title_buffer_at(at, LEN as u64), at)
        })
        .collect();
    for f in 0..12 {
        crate::shadow::test_hot(Some(256), Some(800_000 + f));
        for b in &buffers {
            let copy = crate::shadow::find(b.1, LEN as u64).expect("a copy");
            assert!(!copy.carded(), "frame {f}: a hot copy moved up with the card full");
        }
    }
    // The filler goes; the pressure comes down.
    drop(filler);
    let began = std::time::Instant::now();
    while crate::residency::need().1 >= sixth_memory::residency::Pressure::High {
        assert!(began.elapsed().as_secs() < 10, "the pressure stayed high");
        std::thread::yield_now();
    }
    let (after, used, _) = share();
    println!("released: {:.0}% ({used} MiB), pressure {:?}", after * 100.0, crate::residency::need().1);
    // The texture comes back to the card (with the fallback; without, it
    // never left).
    if fallback {
        assert!(crate::resource::promote_idle(0, 0) >= 1, "given up in the PC's memory");
    }
    let back = crate::resource::host_resource(texture).expect("made again").clone();
    assert!(!crate::residency::in_system(&back), "on the card");
    // The hot copies move now.
    let mut moved = 0;
    for f in 12..24 {
        crate::shadow::test_hot(Some(256), Some(800_000 + f));
        moved = buffers
            .iter()
            .filter(|b| crate::shadow::find(b.1, LEN as u64).is_some_and(|c| c.carded()))
            .count();
    }
    println!("hot copies on the card after the release: {moved} of {}", buffers.len());
    assert_eq!(moved, buffers.len());
    // Filled again: the reclaim puts the hot copies back into the PC's
    // memory before textures have to go there.
    let mut filler = Vec::new();
    while share().0 < 0.93 {
        let Some(b) = host.uav_buffer(64 << 20) else {
            break;
        };
        filler.push(b);
        assert!(filler.len() < 400, "the card never filled");
    }
    let _ = crate::residency::need();
    crate::resource::reclaim();
    let demoted = buffers
        .iter()
        .filter(|b| crate::shadow::find(b.1, LEN as u64).is_some_and(|c| !c.carded()))
        .count();
    println!("filled again to {:.0}%: hot copies back in the PC's memory: {demoted} of {}", share().0 * 100.0, buffers.len());
    assert_eq!(demoted, buffers.len());
    drop(filler);
    crate::shadow::test_hot(None, None);
    assert_eq!(crate::descriptor::host::device_removed(), None);
}

/// Title copies from buffers into textures keep the buffers' bytes in the
/// textures, read back from the card, with the copies made from the host
/// copies or from uploads, synchronously, with the late wait or with the
/// relay, while hot copies move ([`late_child`] in child processes).
#[test]
fn texture_copies_in_flight_hold_their_buffers_bytes() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    let exe = std::env::current_exe().expect("the test binary");
    let modes: [&[(&str, &str)]; 4] = [
        &[("VICTORY_LATE_WAIT", "1")],
        &[("VICTORY_TEXTURE_FROM_COPY", "1")],
        &[("VICTORY_TEXTURE_FROM_COPY", "1"), ("SIXTH_BENCH_HOT_MB", "1024"), ("VICTORY_LATE_WAIT", "3")],
        &[("VICTORY_TEXTURE_FROM_COPY", "1"), ("SIXTH_BENCH_HOT_MB", "1024"), ("VICTORY_RELAY", "1")],
    ];
    for vars in modes {
        let mut child = std::process::Command::new(&exe);
        child
            .args(["--exact", "flights::late_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SIXTH_LATE_CHILD", "1");
        for v in ["VICTORY_LATE_WAIT", "VICTORY_RELAY", "VICTORY_TEXTURE_FROM_COPY", "SIXTH_BENCH_HOT_MB"] {
            child.env_remove(v);
        }
        for (k, v) in vars {
            child.env(k, v);
        }
        let out = child.output().expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("LATE-ROW"), "{vars:?}: {text}");
    }
}

/// The time now, UTC, as `yyyy-mm-dd hh:mm`.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", rest / 3600, rest % 3600 / 60)
}

/// `ab.bat`'s configurations, as `scripts/ab.ps1` sets their switches.
const AB_CONFIGS: [(&str, &[(&str, &str)]); 5] = [
    ("baseline", &[]),
    (
        "best",
        &[
            ("VICTORY_CARD_BUFFERS_MIN_MB", "1"),
            ("VICTORY_CARD_BUFFERS_MB", "1024"),
            ("VICTORY_HOT_WINDOWS_MB", "1024"),
            ("VICTORY_TEXTURE_FROM_COPY", "1"),
        ],
    ),
    (
        "bestlate",
        &[
            ("VICTORY_CARD_BUFFERS_MIN_MB", "1"),
            ("VICTORY_CARD_BUFFERS_MB", "1024"),
            ("VICTORY_HOT_WINDOWS_MB", "1024"),
            ("VICTORY_TEXTURE_FROM_COPY", "1"),
            ("VICTORY_LATE_WAIT", "3"),
        ],
    ),
    ("hot1024", &[("VICTORY_HOT_WINDOWS_MB", "1024")]),
    ("cardbuf1", &[("VICTORY_CARD_BUFFERS_MIN_MB", "1"), ("VICTORY_CARD_BUFFERS_MB", "1024")]),
];

/// Every switch a configuration may set, cleared for the others.
const AB_SWITCHES: [&str; 12] = [
    "VICTORY_CARD_BUFFERS_MIN_MB",
    "VICTORY_CARD_BUFFERS_MB",
    "VICTORY_HOT_WINDOWS_MB",
    "VICTORY_TEXTURE_FROM_COPY",
    "VICTORY_LATE_WAIT",
    "VICTORY_RELAY",
    "VICTORY_WIDE_REFRESH",
    "VICTORY_WIDE_BACK",
    "VICTORY_KEEP_SNAPSHOTS",
    "VICTORY_BACK_NOTE",
    "VICTORY_RESIDENCY",
    "SIXTH_BENCH_HOT_MB",
];

/// One child run of `test` with `vars` and `frames`: its output, and
/// whether it passed (its checks of every byte included).
fn ab_child(test: &str, gate: &str, vars: &[(&str, &str)], frames: u64) -> (bool, String) {
    let mut child = std::process::Command::new(std::env::current_exe().expect("the test binary"));
    child
        .args(["--exact", test, "--ignored", "--nocapture", "--test-threads", "1"])
        .env(gate, "1")
        .env("SIXTH_BENCH_FRAMES", frames.to_string());
    for v in AB_SWITCHES {
        child.env_remove(v);
    }
    for (k, v) in vars {
        child.env(k, v);
    }
    let out = child.output().expect("the child ran");
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

/// The rest of the first line in `text` holding `tag`.
fn after<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.split_once(tag).map(|(_, rest)| rest.trim()))
}

/// Synthetic A/B: `ab.bat`'s five configurations with their real switches,
/// each in child processes, over two workloads through the runtime's own
/// paths, with every byte checked: the game-shaped frame (`frame_child`)
/// and texture streaming with submissions in flight through the title's
/// queue (`late_child`). Writes a Markdown report to `SIXTH_AB_REPORT`
/// (default `target/synthetic-ab.md`). `SIXTH_AB_FRAMES` sets the frames
/// of the frame workload (default 300; the streaming one runs twice as
/// many). Run with
/// `cargo test -p victory-gpu --release --lib synthetic_ab -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (several minutes, about 6 GiB of memory)"]
fn synthetic_ab() {
    let Some(host) = crate::host::host() else {
        return;
    };
    let frames: u64 = std::env::var("SIXTH_AB_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let only: Option<Vec<String>> = std::env::var("SIXTH_AB_CONFIGS").ok().map(|v| v.split(',').map(str::to_owned).collect());
    let mut out = vec![
        "# Synthetic A/B through the runtime's own paths".to_owned(),
        String::new(),
        format!(
            "Written {} (UTC). `ab.bat`'s configurations with their real switches, each in child processes. Not the title: two synthetic workloads through the runtime's own code, every byte they produce checked. Frame times are taken after a warm-up of 40 frames (or a tenth of the run), in which hot copies move to the card. GPU: {}.",
            utc_now(),
            crate::host::adapter_text()
        ),
        String::new(),
        format!("- **frame**: a frame shaped like the game's measured one: 1 GiB of read-only title buffers read whole by the GPU (2% rewritten by the title each frame), 64 writable 4 MiB and 8 writable 16 MiB buffers 2% written by shaders, lists kept two frames; {frames} frames."),
        format!("- **streaming**: eight title submissions a frame, each copying eight 4 MiB title buffers into textures (25% of the buffers rewritten before each), ending in a wait for the title's fence; {} frames.", 2 * frames),
        String::new(),
        "| configuration | frame: median ms (p95, worst) | frame: preparing / GPU / copy-back | streaming: median ms (p95, worst) | streaming: preparing | passed, memory |".to_owned(),
        "|---|---|---|---|---|---|".to_owned(),
    ];
    let mut details = Vec::new();
    for (name, vars) in AB_CONFIGS {
        if only.as_ref().is_some_and(|o| !o.iter().any(|c| c == name)) {
            continue;
        }
        let (frame_ok, frame) = ab_child("flights::frame_child", "SIXTH_FRAME_CHILD", vars, frames);
        let (late_ok, late) = ab_child("flights::late_child", "SIXTH_LATE_CHILD", vars, 2 * frames);
        let dist = |text: &str, tag: &str| {
            after(text, tag)
                .and_then(|d| {
                    let num = |key: &str| d.split(key).nth(1).and_then(|r| r.split([',', ' ']).find(|x| !x.is_empty())).map(str::to_owned);
                    Some(format!("{} ({}, {})", num("median ")?, num("p95 ")?, num("worst ")?))
                })
                .unwrap_or_else(|| "failed".to_owned())
        };
        let row = |text: &str, tag: &str| -> Vec<String> {
            after(text, tag).map(|r| r.split('|').map(|x| x.trim().to_owned()).collect()).unwrap_or_default()
        };
        let f = row(&frame, "FRAME-ROW ");
        let l = row(&late, "LATE-ROW ");
        let memory = [after(&frame, "FRAME-SOAK "), after(&late, "LATE-SOAK ")]
            .iter()
            .zip(["frame", "streaming"])
            .filter_map(|(m, which)| m.map(|m| format!("{which} {}", m.split(": ").nth(1).unwrap_or(m))))
            .collect::<Vec<_>>()
            .join("; ");
        out.push(format!(
            "| {name} | {} | {} / {} / {} | {} | {} | {}{} |",
            dist(&frame, "FRAME-DIST "),
            f.get(1).map_or("-", String::as_str),
            f.get(2).map_or("-", String::as_str),
            f.get(3).map_or("-", String::as_str),
            dist(&late, "LATE-DIST "),
            l.get(1).map_or("-", String::as_str),
            if frame_ok && late_ok { "yes" } else { "**NO**" },
            if memory.trim_matches([';', ' ']).is_empty() { String::new() } else { format!(", alive {memory}") }
        ));
        let settings = if vars.is_empty() {
            "defaults".to_owned()
        } else {
            vars.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
        };
        details.push(format!("- **{name}**: {settings}"));
        for (text, tags) in [(&frame, ["FRAME-CPU ", "FRAME-ROW "]), (&late, ["LATE-CPU ", "LATE-ROW "])] {
            for tag in tags {
                if let Some(line) = after(text, tag) {
                    details.push(format!("  - {}`{line}`", tag.trim_end().to_lowercase().replace('-', " ") + ": "));
                }
            }
        }
        if !(frame_ok && late_ok) {
            let why: Vec<&str> = frame.lines().chain(late.lines()).filter(|l| l.contains("panicked") || l.contains("failure")).collect();
            details.push(format!("  - failed: {}", why.join(" | ")));
        }
        println!("{}", out.last().expect("a row"));
    }
    out.push(String::new());
    out.push("## Details".to_owned());
    out.push(String::new());
    out.extend(details);
    let path = std::env::var("SIXTH_AB_REPORT").unwrap_or_else(|_| "target/synthetic-ab.md".to_owned());
    std::fs::write(&path, out.join("\n") + "\n").expect("the report written");
    println!("report: {path}");
    let _ = host;
}

/// Synthetic: does the GPU reading from the PC's memory slow the CPU's own
/// memory work? On the title (2026-11-27), with about 6 GiB of textures in
/// the PC's memory, not only the GPU's part of the frame grew but also the
/// CPU's preparing and copy-back, several times over. Both use the PC's
/// memory: the GPU over the bus, the CPU directly. This measures the CPU's
/// copy rate (four threads, 128 MiB each, as copy-back and refreshes copy)
/// alone, while the GPU reads 1 GiB in the PC's memory over and over, and
/// while it reads 1 GiB on the card instead (the control: a busy GPU that
/// does not touch the PC's memory). Prints a table; run with
/// `cargo test -p victory-gpu --release --lib pcie_contention_bench -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (about half a minute, 3 GiB of memory)"]
fn pcie_contention_bench() {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::core::Interface;
    let Some(host) = crate::host::host() else {
        return;
    };
    const SRC: u64 = 1 << 30;
    const CHUNK: u64 = 64 << 20;
    let device: &ID3D12Device = &host.device;
    // SAFETY: object creation on the host's device.
    let (root, pipeline, queue, fence) = unsafe {
        let shader = "ByteAddressBuffer t : register(t0); RWByteAddressBuffer o : register(u0);
             [numthreads(256, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
                 uint4 v = t.Load4(id.x * 16);
                 if ((v.x ^ v.y ^ v.z ^ v.w) == 0x9e3779b9u) o.Store((id.x & 0x3fff) * 4, v.x);
             }";
        let code = crate::host::compile(shader, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 2, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let pipeline: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .expect("pipeline");
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_COMPUTE, ..Default::default() })
            .expect("a queue of our own");
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).expect("fence");
        (root, pipeline, queue, fence)
    };
    let (system, at) = host.upload_buffer(SRC).expect("1 GiB in the PC's memory");
    // SAFETY: the mapped upload buffer is SRC bytes long; written once so its
    // pages are in RAM, not untouched commit.
    unsafe { std::ptr::write_bytes(at, 0x5a, SRC as usize) };
    let card = host.uav_buffer(SRC).expect("1 GiB on the card");
    let out = host.uav_buffer(64 << 10).expect("out");
    let value = std::cell::Cell::new(0u64);
    // Starts `passes` reads of the whole of `source` on the GPU; returns a
    // waiter for their end, which gives the time they took.
    let run = |source: &ID3D12Resource, passes: u32| {
        // SAFETY: recording on objects of our own; the allocator and list
        // live in the waiter until the GPU is done with them.
        unsafe {
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE).expect("allocator");
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COMPUTE, &allocator, &pipeline).expect("list");
            list.SetComputeRootSignature(&root);
            for _ in 0..passes {
                for chunk in 0..SRC / CHUNK {
                    list.SetComputeRootShaderResourceView(0, source.GetGPUVirtualAddress() + chunk * CHUNK);
                    list.SetComputeRootUnorderedAccessView(1, out.GetGPUVirtualAddress());
                    list.Dispatch((CHUNK / 16 / 256) as u32, 1, 1);
                }
            }
            list.Close().expect("closed");
            let started = std::time::Instant::now();
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().expect("a command list"))]);
            value.set(value.get() + 1);
            queue.Signal(&fence, value.get()).expect("signal");
            let target = value.get();
            let fence = fence.clone();
            move || {
                let event = CreateEventW(None, false, false, None).expect("event");
                fence.SetEventOnCompletion(target, event).expect("on completion");
                WaitForSingleObject(event, u32::MAX);
                let _ = windows::Win32::Foundation::CloseHandle(event);
                drop((allocator, list));
                started.elapsed()
            }
        }
    };
    // The CPU's copy rate over `seconds`: four threads, each copying 128 MiB
    // of its own, already in RAM.
    let cpu_rate = |seconds: f64| {
        let begun = std::time::Instant::now();
        let deadline = begun + std::time::Duration::from_secs_f64(seconds);
        let copied: u64 = std::thread::scope(|s| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(move || {
                        let src = vec![7u8; 128 << 20];
                        let mut dst = vec![0u8; 128 << 20];
                        dst.copy_from_slice(&src);
                        let mut n = 0u64;
                        while std::time::Instant::now() < deadline {
                            dst.copy_from_slice(&src);
                            std::hint::black_box(&mut dst);
                            n += src.len() as u64;
                        }
                        n
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().expect("a copier")).sum()
        });
        copied as f64 / begun.elapsed().as_secs_f64() / 1e9
    };
    // The GPU's read rates alone, to size its loads to about three seconds.
    let rate = |d: std::time::Duration, passes: u32| f64::from(passes) * SRC as f64 / d.as_secs_f64() / 1e9;
    let pc_alone = rate(run(&system, 2)(), 2);
    let card_alone = rate(run(&card, 8)(), 8);
    let pc_passes = ((3.0 * pc_alone).ceil() as u32).clamp(2, 400);
    let card_passes = ((3.0 * card_alone).ceil() as u32).clamp(8, 2000);
    let cpu_alone = [cpu_rate(1.0), cpu_rate(1.0), cpu_rate(1.0)];
    let mut with_pc = Vec::new();
    let mut with_card = Vec::new();
    let mut pc_under = Vec::new();
    for _ in 0..3 {
        let wait = run(&system, pc_passes);
        std::thread::sleep(std::time::Duration::from_millis(100));
        with_pc.push(cpu_rate(1.0));
        pc_under.push(rate(wait(), pc_passes));
        let wait = run(&card, card_passes);
        std::thread::sleep(std::time::Duration::from_millis(100));
        with_card.push(cpu_rate(1.0));
        let _ = wait();
    }
    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let alone = median(&cpu_alone);
    let (pc, card_case) = (median(&with_pc), median(&with_card));
    println!("| the GPU meanwhile | CPU copy rate, GB/s (4 threads) | against the CPU alone | the GPU's read rate, GB/s |");
    println!("|---|---|---|---|");
    println!("| idle | {alone:.1} | (the reference) | - |");
    println!("| reads 1 GiB on the card | {card_case:.1} | {:+.0}% | {card_alone:.0} alone |", (card_case / alone - 1.0) * 100.0);
    println!(
        "| reads 1 GiB in the PC's memory | {pc:.1} | {:+.0}% | {pc_alone:.1} alone, {:.1} while the CPU copies (3 s of reads, 1 s of copies) |",
        (pc / alone - 1.0) * 100.0,
        median(&pc_under)
    );
    println!(
        "CONTENTION-ROW {alone:.2}|{card_case:.2}|{pc:.2}|{pc_alone:.2}|{:.2}|{card_alone:.1}",
        median(&pc_under)
    );
    drop((system, card, out));
}

/// Synthetic: does making pipelines on worker threads hold up the thread
/// that records and submits? On the title (2026-11-27, the stability run)
/// gameplay frames took 5.5 s while the process kept 7 cores busy and 1,443
/// draws per frame waited for their pipelines. This measures, on the host's
/// device, the submitting thread's loop (record a small dispatch, submit,
/// wait for it) alone and while 2, 4 or 8 threads at the workers' priority
/// make pipelines from shaders the driver has never seen (each one unique,
/// so its compiler runs). Prints a table; run with
/// `cargo test -p victory-gpu --release --lib pipeline_contention_bench -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark (a minute or two)"]
fn pipeline_contention_bench() {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::core::Interface;
    let Some(host) = crate::host::host() else {
        return;
    };
    let device: ID3D12Device = host.device.clone();
    // A tag unique to this run, so that no shader below is in the driver's
    // cache from an earlier run.
    let tag = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) as u32;
    let source = |k: u32| {
        format!(
            "RWByteAddressBuffer o : register(u0);
             [numthreads(64, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {{
                 float4 a = float4(id.x, {k}.0, 1.0, 2.0);
                 [unroll] for (int i = 0; i < 96; i++) {{ a = a * 1.0001f + sin(a.yzwx * {k}.0e-9 + i); }}
                 o.Store(id.x * 4, asuint(a.x + a.y + a.z + a.w + {tag}.0));
             }}"
        )
    };
    // SAFETY: object creation on the host's device.
    let (root, small, queue, fence) = unsafe {
        let params = [D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
            Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        }];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 1, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
        let blob = blob.expect("blob");
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .expect("root");
        let code = crate::host::compile(
            "RWByteAddressBuffer o : register(u0); [numthreads(64, 1, 1)] void main(uint3 id : SV_DispatchThreadID) { o.Store(id.x * 4, id.x); }",
            windows::core::s!("main"),
            windows::core::s!("cs_5_1"),
        )
        .expect("compiled");
        let small: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .expect("pipeline");
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
            .expect("a queue of our own");
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).expect("fence");
        (root, small, queue, fence)
    };
    let out = host.uav_buffer(64 << 10).expect("out");
    // Unique shaders, compiled before anything is measured: 48 per maker.
    let mut next = 0u32;
    let mut batch = |n: usize| {
        (0..n)
            .map(|_| {
                next += 1;
                crate::host::compile(&source(next), windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled")
            })
            .collect::<Vec<_>>()
    };
    // One make of a pipeline from `code`, its time.
    let make = |device: &ID3D12Device, root: &ID3D12RootSignature, code: &[u8]| {
        let began = std::time::Instant::now();
        // SAFETY: creation on the device; the root outlives the call.
        let made: windows::core::Result<ID3D12PipelineState> = unsafe {
            device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
        };
        made.expect("a pipeline");
        began.elapsed()
    };
    // The submitting thread's loop for as long as `busy` holds: per turn,
    // the time to record and submit, and the whole turn with the wait.
    let submit_loop = |busy: &dyn Fn() -> bool, at_least: std::time::Duration| {
        let mut submit = Vec::new();
        let mut turn = Vec::new();
        let mut value = 0u64;
        let begun = std::time::Instant::now();
        // SAFETY: objects of our own; each list is waited for before the
        // allocator is reset.
        unsafe {
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).expect("allocator");
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, &small).expect("list");
            list.Close().expect("closed");
            let event = CreateEventW(None, false, false, None).expect("event");
            while busy() || begun.elapsed() < at_least {
                let t0 = std::time::Instant::now();
                allocator.Reset().expect("reset");
                list.Reset(&allocator, &small).expect("list reset");
                list.SetComputeRootSignature(&root);
                list.SetComputeRootUnorderedAccessView(0, out.GetGPUVirtualAddress());
                list.Dispatch(16, 1, 1);
                list.Close().expect("closed");
                queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().expect("a command list"))]);
                value += 1;
                queue.Signal(&fence, value).expect("signal");
                submit.push(t0.elapsed().as_secs_f64() * 1e3);
                fence.SetEventOnCompletion(value, event).expect("on completion");
                WaitForSingleObject(event, u32::MAX);
                turn.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            let _ = windows::Win32::Foundation::CloseHandle(event);
        }
        (submit, turn)
    };
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
        (at(0.5), at(0.99), *v.last().unwrap_or(&0.0))
    };
    println!("| pipeline makers (below normal) | pipelines made, ms each (median, worst) | submitting turns | record+submit ms (median, p99, worst) | turn with the wait ms (median, p99, worst) |");
    println!("|---|---|---|---|---|");
    let (mut s0, mut t0) = submit_loop(&|| false, std::time::Duration::from_secs(2));
    let n0 = t0.len();
    let (a, b, c) = stats(&mut s0);
    let (d, e, f) = stats(&mut t0);
    println!("| none | - | {n0} | {a:.3}, {b:.3}, {c:.2} | {d:.3}, {e:.3}, {f:.2} |");
    for makers in [2usize, 4, 8] {
        let codes: Vec<Vec<Vec<u8>>> = (0..makers).map(|_| batch(48)).collect();
        let left = std::sync::atomic::AtomicUsize::new(makers);
        let times = std::sync::Mutex::new(Vec::new());
        let (mut s, mut t) = std::thread::scope(|scope| {
            for mine in &codes {
                let (device, root, left, times) = (&device, &root, &left, &times);
                scope.spawn(move || {
                    // SAFETY: a plain call on our own thread (the workers' priority).
                    unsafe {
                        use windows::Win32::System::Threading::*;
                        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                    for code in mine {
                        let took = make(device, root, code);
                        times.lock().unwrap_or_else(|e| e.into_inner()).push(took.as_secs_f64() * 1e3);
                    }
                    left.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
            submit_loop(&|| left.load(std::sync::atomic::Ordering::Relaxed) > 0, std::time::Duration::ZERO)
        });
        let mut made = times.into_inner().unwrap_or_else(|e| e.into_inner());
        let count = made.len();
        let (pm, _, pw) = stats(&mut made);
        let n = t.len();
        let (a, b, c) = stats(&mut s);
        let (d, e, f) = stats(&mut t);
        println!("| {makers} | {count}: {pm:.1}, {pw:.1} | {n} | {a:.3}, {b:.3}, {c:.2} | {d:.3}, {e:.3}, {f:.2} |");
        println!("PSO-ROW {makers}|{pm:.2}|{pw:.2}|{a:.3}|{b:.3}|{c:.2}|{d:.3}|{e:.3}|{f:.2}");
    }
    drop(out);
}
