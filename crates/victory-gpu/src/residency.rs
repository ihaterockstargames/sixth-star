//! The host's residency decisions (`sixth_memory::residency`), fed with the
//! budget the system reports and acted on by the texture code: where a new
//! host texture goes, and which idle ones to give up when the card is full.
//!
//! Before, past nine tenths of the budget the pass that makes textures
//! picked by index made none, and their descriptor slots showed nothing for
//! as long as the card stayed full (the missing textures). Now, under
//! pressure, textures nothing holds are given up first, least recently used
//! first (`crate::resource::reclaim`), and a texture the title fills that
//! still finds no room on the card is made in the PC's memory, which the
//! GPU reads over the bus: slower, but there and correct. Render targets,
//! depth and unordered-access textures stay on the card. Only when the PC's
//! shared memory is past its share too is a texture refused, and that is
//! counted with its reason on the frame line.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use sixth_memory::residency::{Budget, Class, Placement, Pressure, Residency};
use windows::Win32::Graphics::Direct3D12::*;

static STATE: Mutex<Option<Residency>> = Mutex::new(None);

/// Whether the residency decisions are used (`VICTORY_RESIDENCY=0`: the
/// inherited rule, no texture made past nine tenths of the budget, none
/// given up early, none in the PC's memory). On by default.
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RESIDENCY").is_none_or(|v| v != "0"))
}

/// Since the start: host textures made in the PC's memory for lack of room
/// on the card; textures given up under pressure and their bytes; texture
/// passes that found no room and nothing to fall back to.
pub(crate) static TEXTURES_IN_SYSTEM: AtomicU64 = AtomicU64::new(0);
pub(crate) static RECLAIMED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
pub(crate) static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Since the start: host textures in the PC's memory given up to be made
/// again on the card once it had room (`crate::resource::promote`), and
/// their bytes.
pub(crate) static PROMOTED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// Since the start: descriptor slots made again because they showed such a
/// copy.
pub(crate) static SLOTS_REMADE: AtomicU64 = AtomicU64::new(0);

fn with<R>(f: impl FnOnce(&mut Residency) -> R) -> R {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(state.get_or_insert_with(Residency::default))
}

/// The budget the system gives the process now, if it reports one.
pub(crate) fn budget() -> Option<Budget> {
    let [local_budget, local_used, system_budget, system_used] = crate::host::host()?.video_memory()?;
    Some(Budget {
        local_budget,
        local_used,
        system_budget,
        system_used,
    })
}

/// The bytes to give up to bring the card under its target, and the
/// pressure, now.
pub(crate) fn need() -> (u64, Pressure) {
    let Some(b) = budget() else {
        return (0, Pressure::Normal);
    };
    with(|r| {
        let p = r.observe(&b);
        (r.need(&b), p)
    })
}

#[cfg(test)]
thread_local! {
    /// Tests: textures this thread makes that may go to the PC's memory go
    /// there ([`force_system`]).
    static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests: until the guard goes, textures this thread makes that may go to
/// the PC's memory go there, whatever the card's budget.
#[cfg(test)]
pub(crate) fn force_system() -> impl Drop {
    struct Forced;
    impl Drop for Forced {
        fn drop(&mut self) {
            FORCED.with(|f| f.set(false));
        }
    }
    FORCED.with(|f| f.set(true));
    Forced
}

/// Where a new host texture of `bytes` (allocated) goes: `class` from what
/// the GPU does with it.
pub(crate) fn place(class: Class, bytes: u64) -> Placement {
    #[cfg(test)]
    if class != Class::Written && FORCED.with(std::cell::Cell::get) {
        return Placement::System;
    }
    let Some(b) = budget() else {
        return Placement::Local;
    };
    with(|r| {
        r.observe(&b);
        r.place(class, bytes, &b)
    })
}

/// Where a resource of `class` and `bytes` would go now, without counting
/// it as placed (a pass asking whether to make any).
pub(crate) fn place_probe(class: Class, bytes: u64) -> Placement {
    let Some(b) = budget() else {
        return Placement::Local;
    };
    with(|r| {
        r.observe(&b);
        let mut probe = r.clone();
        probe.place(class, bytes, &b)
    })
}

/// Which of `candidates` to give up to free `need` bytes (frame now; every
/// candidate's last use finished, see `crate::resource::reclaim`).
pub(crate) fn evict(need: u64, candidates: &[sixth_memory::residency::Candidate]) -> Vec<usize> {
    let frame = crate::frame::current();
    with(|r| r.evict(need, candidates, frame, u64::MAX))
}

/// The bytes that may come back to the card from the PC's memory now
/// (`sixth_memory::residency::Residency::promotable`).
pub(crate) fn promotable() -> u64 {
    let Some(b) = budget() else {
        return 0;
    };
    with(|r| {
        r.observe(&b);
        r.promotable(&b)
    })
}

/// Which of `candidates` (host textures in the PC's memory) to make again
/// on the card now.
pub(crate) fn promote(candidates: &[sixth_memory::residency::Candidate]) -> Vec<usize> {
    let Some(b) = budget() else {
        return Vec::new();
    };
    with(|r| {
        r.observe(&b);
        r.promote(candidates, &b)
    })
}

/// Whether `resource` was made in the PC's memory (a custom heap whose
/// pool is the system's), by what the device says of it.
pub(crate) fn in_system(resource: &ID3D12Resource) -> bool {
    let mut properties = D3D12_HEAP_PROPERTIES::default();
    // SAFETY: a live committed resource; the out-pointer is ours.
    let asked = unsafe { resource.GetHeapProperties(Some(&mut properties), None) };
    asked.is_ok() && properties.Type == D3D12_HEAP_TYPE_CUSTOM && properties.MemoryPoolPreference == D3D12_MEMORY_POOL_L0
}

/// The heap a host texture of the given placement is made in: the card's
/// default heap, or the PC's memory, GPU access only (custom heap, pool L0,
/// no CPU page).
pub(crate) fn texture_heap(placement: Placement) -> D3D12_HEAP_PROPERTIES {
    match placement {
        Placement::System => D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE,
            MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
            ..Default::default()
        },
        _ => D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        },
    }
}

/// For the frame line, since the last call.
pub(crate) fn line() -> Option<String> {
    static LAST: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
    let now = [
        TEXTURES_IN_SYSTEM.load(Relaxed),
        RECLAIMED[0].load(Relaxed),
        RECLAIMED[1].load(Relaxed),
        REFUSED.load(Relaxed),
        PROMOTED[0].load(Relaxed),
        PROMOTED[1].load(Relaxed),
        SLOTS_REMADE.load(Relaxed),
        crate::shadow::HOT_WINDOWS[0].load(Relaxed),
        crate::shadow::HOT_WINDOWS[1].load(Relaxed),
    ];
    let d: Vec<u64> = now.iter().zip(&LAST).map(|(n, l)| n - l.swap(*n, Relaxed)).collect();
    let pressure = with(|r| r.pressure());
    let [alive, _, bytes] = crate::ledger::alive(crate::ledger::Cat::Texture, crate::ledger::Pool::System);
    Some(format!(
        "residency: pressure {pressure:?}; since the last line {} textures made in the PC's memory for lack of room on the card \
         ({} since the start, {alive} alive, {:.0} MiB), {} idle textures given up ({:.0} MiB), {} back on the card ({:.0} MiB, {} slots made again), {} texture passes with no room in either pool; \
         {} hot windows moved to the card ({:.0} MiB)",
        d[0],
        now[0],
        bytes as f64 / f64::from(1u32 << 20),
        d[1],
        d[2] as f64 / f64::from(1u32 << 20),
        d[4],
        d[5] as f64 / f64::from(1u32 << 20),
        d[6],
        d[3],
        d[7],
        d[8] as f64 / f64::from(1u32 << 20)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::core::Interface;

    /// A `w` by `w` R8G8B8A8_UINT texture in `placement`'s heap, filled by
    /// a copy with `texels`, then read by a compute pass `passes` times
    /// over: what the pass read the last time, and the wall time of the
    /// passes (submitted once, waited for).
    fn sample(placement: Placement, w: u32, texels: &[u32], passes: u32) -> Option<(Vec<u32>, f64)> {
        let host = crate::host::host()?;
        let device: &ID3D12Device = &host.device;
        // SAFETY: object creation, recording and a wait on objects made
        // here, on the host's queue.
        unsafe {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(w),
                Height: w,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UINT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            let mut texture: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&texture_heap(placement), D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut texture)
                .expect("a texture in that heap");
            let texture = texture.expect("made");
            let pitch = w * 4;
            let (upload, data) = host.upload_buffer(u64::from(pitch * w)).expect("upload");
            std::ptr::copy_nonoverlapping(texels.as_ptr(), data.cast::<u32>(), texels.len());
            let bytes = u64::from(w * w * 4);
            let out = host.uav_buffer(bytes).expect("out");
            let back = host.read_back_buffer(bytes).expect("back");
            let shader = format!(
                "Texture2D<uint4> t : register(t0); RWByteAddressBuffer o : register(u0);
                 [numthreads(8, 8, 1)] void main(uint3 id : SV_DispatchThreadID) {{
                     uint4 v = t.Load(int3(id.xy, 0));
                     o.Store((id.y * {w} + id.x) * 4, v.x | (v.y << 8) | (v.z << 16) | (v.w << 24));
                 }}"
            );
            let code = crate::host::compile(&shader, windows::core::s!("main"), windows::core::s!("cs_5_1")).expect("compiled");
            let ranges = [D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 1,
                ..Default::default()
            }];
            let params = [
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: ranges.as_ptr() },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                    Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: 0 } },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
            ];
            let root_desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                ..Default::default()
            };
            let mut blob = None;
            D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).expect("root signature");
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
            let heap: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .expect("heap");
            device.CreateShaderResourceView(&texture, None, heap.GetCPUDescriptorHandleForHeapStart());
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).expect("fence");
            let run = |value: u64, record: &dyn Fn(&ID3D12GraphicsCommandList)| -> f64 {
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).expect("allocator");
                let list: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).expect("list");
                record(&list);
                list.Close().expect("closed");
                let started = std::time::Instant::now();
                host.queue.ExecuteCommandLists(&[Some(list.cast().expect("a list"))]);
                host.queue.Signal(&fence, value).expect("signal");
                while fence.GetCompletedValue() < value {
                    assert!(started.elapsed().as_secs() < 30, "the GPU finished");
                    std::thread::yield_now();
                }
                started.elapsed().as_secs_f64() * 1000.0
            };
            let barrier = |r: &ID3D12Resource, from, to| D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: std::mem::transmute_copy(r),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: from,
                        StateAfter: to,
                    }),
                },
                ..Default::default()
            };
            run(1, &|list| {
                let src = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&upload),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                            Offset: 0,
                            Footprint: D3D12_SUBRESOURCE_FOOTPRINT { Format: desc.Format, Width: w, Height: w, Depth: 1, RowPitch: pitch },
                        },
                    },
                };
                let dst = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&texture),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                list.ResourceBarrier(&[
                    barrier(&texture, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE),
                    barrier(&out, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
                ]);
            });
            let ms = run(2, &|list| {
                list.SetDescriptorHeaps(&[Some(heap.clone())]);
                list.SetComputeRootSignature(&root);
                list.SetPipelineState(&pipeline);
                list.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                list.SetComputeRootUnorderedAccessView(1, out.GetGPUVirtualAddress());
                for _ in 0..passes {
                    list.Dispatch(w / 8, w / 8, 1);
                    let uav = D3D12_RESOURCE_BARRIER {
                        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                        Anonymous: D3D12_RESOURCE_BARRIER_0 {
                            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: std::mem::transmute_copy(&out) }),
                        },
                        ..Default::default()
                    };
                    list.ResourceBarrier(&[uav]);
                }
            });
            run(3, &|list| {
                list.ResourceBarrier(&[barrier(&out, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                list.CopyBufferRegion(&back, 0, &out, 0, bytes);
            });
            assert!(device.GetDeviceRemovedReason().is_ok(), "device alive");
            let mut p = std::ptr::null_mut();
            back.Map(0, None, Some(&mut p)).expect("mapped");
            let read = std::slice::from_raw_parts(p.cast::<u32>(), texels.len()).to_vec();
            back.Unmap(0, None);
            Some((read, ms))
        }
    }

    fn pattern(w: u32) -> Vec<u32> {
        (0..w * w).map(|i| 0x0100_0000 * (i % 251) + i).collect()
    }

    /// On the device: a texture in the PC's memory (the fallback heap) is
    /// filled by a copy and sampled by a shader like one on the card: the
    /// shader reads every texel as written.
    #[test]
    fn a_texture_in_the_pcs_memory_is_filled_and_sampled() {
        if crate::host::host().is_none() {
            return; // no host GPU
        }
        assert_eq!(
            crate::ledger::pool_from(D3D12_HEAP_TYPE_CUSTOM, D3D12_MEMORY_POOL_L0, D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE),
            crate::ledger::Pool::System
        );
        let texels = pattern(64);
        let (read, _) = sample(Placement::System, 64, &texels, 1).expect("sampled");
        assert_eq!(read, texels, "every texel as written");
    }

    /// The combined path under real pressure, on the device (ignored by
    /// default: it fills the card to its target; run with
    /// `cargo test -p victory-gpu --release -- --ignored --nocapture
    /// the_card_filled`): textures on the card until the pressure is
    /// `High`; then a texture the title fills is placed in the PC's memory,
    /// reads right, and its cost against the same on the card is printed;
    /// a render target still goes to the card; and once the filler is
    /// gone the pressure comes back down.
    #[test]
    #[ignore = "fills the card's memory to its target"]
    fn the_card_filled_to_its_target_places_textures_in_the_pcs_memory() {
        let Some(host) = crate::host::host() else {
            return;
        };
        let device: &ID3D12Device = &host.device;
        let w = 2048u32;
        let texels = pattern(w);
        let (card, card_ms) = sample(Placement::Local, w, &texels, 64).expect("on the card");
        assert_eq!(card, texels);
        // Fill the card with 64 MiB textures until the pressure is High.
        let mut filler = Vec::new();
        let filler_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 4096,
            Height: 4096,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        while need().1 < Pressure::High {
            assert!(filler.len() < 256 && started.elapsed().as_secs() < 120, "the card filled");
            let mut t: Option<ID3D12Resource> = None;
            // SAFETY: plain creation.
            unsafe {
                device
                    .CreateCommittedResource(&texture_heap(Placement::Local), D3D12_HEAP_FLAG_NONE, &filler_desc, D3D12_RESOURCE_STATE_RENDER_TARGET, None, &mut t)
                    .expect("a filler texture");
            }
            let t = t.expect("made");
            crate::ledger::book(&t, crate::ledger::Cat::Internal);
            filler.push(t);
        }
        let b = budget().expect("a budget");
        eprintln!(
            "filled: {} textures of 64 MiB, the card at {:.2} of {:.2} GiB ({:.0}%)",
            filler.len(),
            b.local_used as f64 / f64::from(1u32 << 30),
            b.local_budget as f64 / f64::from(1u32 << 30),
            b.local_share() * 100.0
        );
        let bytes = 16u64 << 20;
        assert_eq!(place_probe(Class::Streamed, bytes), Placement::System, "a texture the title fills: the PC's memory");
        assert_eq!(place_probe(Class::Written, bytes), Placement::Local, "a render target: the card");
        let (system, system_ms) = sample(Placement::System, w, &texels, 64).expect("in the PC's memory");
        assert_eq!(system, texels, "right where it lives");
        eprintln!(
            "64 reads of a 2048x2048 texture: {card_ms:.1} ms on the card, {system_ms:.1} ms in the PC's memory ({:.1}x)",
            system_ms / card_ms.max(1e-3)
        );
        drop(filler);
        let started = std::time::Instant::now();
        while need().1 >= Pressure::High {
            assert!(started.elapsed().as_secs() < 10, "the pressure came down once the filler went");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
