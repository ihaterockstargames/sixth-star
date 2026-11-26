//! On the device: buffer views cut to their host copy
//! (`descriptor::host::fit_to_copy`) read, write and do atomics as the
//! console's record-count bounds would, for typed, structured and raw views;
//! and the null view they replaced reads 0 everywhere.
//!
//! The title's view asks for more elements than its host copy holds. The
//! fixture makes the view as the runtime now does (cut), runs one compute
//! pass over the title's whole element count, and checks every element:
//! inside the copy the bytes, past it 0; writes and atomics past it dropped;
//! nothing around the view touched; the device still alive.

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use crate::descriptor::host::fit_to_copy;

/// Dwords in each test buffer (1 KiB).
const DWORDS: u32 = 256;
/// Elements the title's views ask for.
const ASKED: u32 = 100;

const SHADER: &str = r"
Buffer<uint> typed : register(t0);
StructuredBuffer<uint2> structured : register(t1);
ByteAddressBuffer raw : register(t2);
RWBuffer<uint> written : register(u0);
RWByteAddressBuffer result : register(u1);

[numthreads(1, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    result.Store(i * 4, typed[i]);
    result.Store((100 + i) * 4, structured[i].y);
    result.Store((200 + i) * 4, raw.Load(i * 4));
    written[i] = 0xC0DE0000u + i;
    uint was;
    InterlockedAdd(written[i], 1u, was);
}
";

fn buffer(device: &ID3D12Device, kind: D3D12_HEAP_TYPE, bytes: u64, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> ID3D12Resource {
    let props = D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: bytes,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    };
    let mut r: Option<ID3D12Resource> = None;
    // SAFETY: plain object creation.
    unsafe { device.CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut r) }.expect("a test buffer");
    r.expect("a test buffer")
}

/// The views of one run: cut (as the runtime makes them) or null (as before).
struct Run {
    /// What the shader read through the typed, structured and raw views.
    typed: Vec<u32>,
    structured: Vec<u32>,
    raw: Vec<u32>,
    /// The written buffer after the pass.
    written: Vec<u32>,
}

/// Runs the pass with views of `source` cut to it (`cut`), or null.
fn run(cut: bool) -> Option<Run> {
    let host = crate::host::host()?;
    let device: &ID3D12Device = &host.device;
    let bytes = u64::from(DWORDS) * 4;
    let width = bytes;
    // The source: dword i holds i + 1, so 0 is never real data.
    let data: Vec<u32> = (1..=DWORDS).collect();
    let upload = buffer(device, D3D12_HEAP_TYPE_UPLOAD, 2 * bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ);
    // SAFETY: an upload buffer of 2 KiB, mapped for writing.
    unsafe {
        let mut p = std::ptr::null_mut();
        upload.Map(0, None, Some(&mut p)).ok()?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast::<u32>(), data.len());
        // The written buffer starts as 0xEEEEEEEE everywhere.
        std::ptr::write_bytes(p.cast::<u8>().add(bytes as usize), 0xEE, bytes as usize);
        upload.Unmap(0, None);
    }
    let flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    let source = buffer(device, D3D12_HEAP_TYPE_DEFAULT, bytes, flags, D3D12_RESOURCE_STATE_COMMON);
    let target = buffer(device, D3D12_HEAP_TYPE_DEFAULT, bytes, flags, D3D12_RESOURCE_STATE_COMMON);
    let result = buffer(device, D3D12_HEAP_TYPE_DEFAULT, 4 * 300, flags, D3D12_RESOURCE_STATE_COMMON);
    let back = buffer(device, D3D12_HEAP_TYPE_READBACK, bytes + 4 * 300, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST);

    // The title's views: typed R32_UINT from element 200, structured (8-byte
    // elements) from element 100, raw from dword 200; each asks for 100.
    let typed_count = fit_to_copy(DXGI_FORMAT_R32_UINT, 200, ASKED, 0, false, width);
    let structured_count = fit_to_copy(DXGI_FORMAT_UNKNOWN, 100, ASKED, 8, false, width);
    let raw_count = fit_to_copy(DXGI_FORMAT_R32_TYPELESS, 200, ASKED, 0, true, width);
    assert_eq!((typed_count, structured_count, raw_count), (56, 28, 56), "cut to the copy");

    // SAFETY: plain object creation and recording on objects made here;
    // the queue is the host's, waited for before returning.
    unsafe {
        let code = crate::host::compile(SHADER, windows::core::s!("main"), windows::core::s!("cs_5_1")).ok()?;
        let ranges = [
            D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 3,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            },
            D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                NumDescriptors: 2,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 3,
            },
        ];
        let params = [D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 2, pDescriptorRanges: ranges.as_ptr() },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        }];
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 1, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).ok()?;
        let blob = blob?;
        let root: ID3D12RootSignature = device
            .CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()))
            .ok()?;
        let pso: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })
            .ok()?;
        let heap: ID3D12DescriptorHeap = device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 5,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })
            .ok()?;
        let step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
        let at = |i: usize| {
            let mut h = heap.GetCPUDescriptorHandleForHeapStart();
            h.ptr += i * step;
            h
        };
        let srv = |format, first, count, stride, flags| D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_SRV { FirstElement: first, NumElements: count, StructureByteStride: stride, Flags: flags },
            },
        };
        let views = [
            srv(DXGI_FORMAT_R32_UINT, 200, typed_count, 0, D3D12_BUFFER_SRV_FLAG_NONE),
            srv(DXGI_FORMAT_UNKNOWN, 100, structured_count, 8, D3D12_BUFFER_SRV_FLAG_NONE),
            srv(DXGI_FORMAT_R32_TYPELESS, 200, raw_count, 0, D3D12_BUFFER_SRV_FLAG_RAW),
        ];
        for (i, v) in views.iter().enumerate() {
            if cut {
                device.CreateShaderResourceView(&source, Some(v), at(i));
            } else {
                // Null: the view kept, no resource (what a refused view got).
                device.CreateShaderResourceView(None, Some(v), at(i));
            }
        }
        let typed_uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_UINT,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV { FirstElement: 200, NumElements: fit_to_copy(DXGI_FORMAT_R32_UINT, 200, ASKED, 0, false, width), ..Default::default() },
            },
        };
        if cut {
            device.CreateUnorderedAccessView(&target, None, Some(&typed_uav), at(3));
        } else {
            device.CreateUnorderedAccessView(None, None, Some(&typed_uav), at(3));
        }
        let result_uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: 300, Flags: D3D12_BUFFER_UAV_FLAG_RAW, ..Default::default() },
            },
        };
        device.CreateUnorderedAccessView(&result, None, Some(&result_uav), at(4));

        let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
        let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).ok()?;
        let barrier = |r: &ID3D12Resource, from, to| D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: std::mem::transmute_copy(r),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: from,
                    StateAfter: to,
                }),
            },
        };
        list.CopyBufferRegion(&source, 0, &upload, 0, bytes);
        list.CopyBufferRegion(&target, 0, &upload, bytes, bytes);
        list.ResourceBarrier(&[
            barrier(&source, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE),
            barrier(&target, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
            barrier(&result, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
        ]);
        list.SetComputeRootSignature(&root);
        list.SetPipelineState(&pso);
        list.SetDescriptorHeaps(&[Some(heap.clone())]);
        list.SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
        list.Dispatch(ASKED, 1, 1);
        list.ResourceBarrier(&[
            barrier(&target, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE),
            barrier(&result, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE),
        ]);
        list.CopyBufferRegion(&back, 0, &target, 0, bytes);
        list.CopyBufferRegion(&back, bytes, &result, 0, 4 * 300);
        list.Close().ok()?;
        host.queue.ExecuteCommandLists(&[Some(list.cast().ok()?)]);
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
        host.queue.Signal(&fence, 1).ok()?;
        while fence.GetCompletedValue() < 1 {
            if device.GetDeviceRemovedReason().is_err() {
                panic!("the device was removed by the pass");
            }
            std::thread::yield_now();
        }
        assert!(device.GetDeviceRemovedReason().is_ok(), "the device survives the pass");
        let mut p = std::ptr::null_mut();
        back.Map(0, None, Some(&mut p)).ok()?;
        let all = std::slice::from_raw_parts(p.cast::<u32>(), DWORDS as usize + 300).to_vec();
        back.Unmap(0, None);
        let (written, result) = all.split_at(DWORDS as usize);
        Some(Run {
            typed: result[..100].to_vec(),
            structured: result[100..200].to_vec(),
            raw: result[200..300].to_vec(),
            written: written.to_vec(),
        })
    }
}

#[test]
fn cut_views_read_write_and_add_within_the_copy_only() {
    let Some(cut) = run(true) else {
        return; // no host GPU
    };
    // Typed and raw from element/dword 200: 56 real, then 0.
    for i in 0..100u32 {
        let want = if i < 56 { 201 + i } else { 0 };
        assert_eq!(cut.typed[i as usize], want, "typed element {i}");
        assert_eq!(cut.raw[i as usize], want, "raw dword {i}");
    }
    // Structured uint2 from element 100: element k holds dwords 2k, 2k+1;
    // .y of element 100 + i is dword 201 + 2i, holding 202 + 2i.
    for i in 0..100u32 {
        let want = if i < 28 { 202 + 2 * i } else { 0 };
        assert_eq!(cut.structured[i as usize], want, "structured element {i}");
    }
    // The written view: dwords 200..256 written then added to, the rest
    // untouched; writes and atomics past the copy dropped (nothing to see,
    // and the device alive).
    for d in 0..DWORDS {
        let want = if d >= 200 { 0xC0DE_0000 + (d - 200) + 1 } else { 0xEEEE_EEEE };
        assert_eq!(cut.written[d as usize], want, "written dword {d}");
    }
}

/// The view left null, as before the cut: every element reads 0, also the
/// ones the copy holds, and nothing is written.
#[test]
fn null_views_read_nothing_at_all() {
    let Some(null) = run(false) else {
        return; // no host GPU
    };
    assert!(null.typed.iter().chain(&null.structured).chain(&null.raw).all(|&v| v == 0));
    assert!(null.written.iter().all(|&v| v == 0xEEEE_EEEE));
}
