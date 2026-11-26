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
    let null = std::ptr::null();
    let allocator_iid = iid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
    let list_iid = iid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
    let (mut allocator, mut list) = (null, null);
    assert_eq!(crate::list::create_allocator(null, 0, &allocator_iid, &mut allocator), 0);
    assert_eq!(crate::list::create_list(null, 0, 0, allocator, null, &list_iid, &mut list), 0);
    list
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
            for f in 0..FRAMES {
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
