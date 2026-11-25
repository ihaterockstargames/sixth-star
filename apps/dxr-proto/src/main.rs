//! Gen 2 Phase 4: native DXR on original synthetic scenes.
//!
//! Measures, with GPU timestamps (median of `--runs` after a warm-up):
//! bottom-level builds (fast-trace / fast-build), compaction, refit after
//! the vertices move, top-level build and update over many instances, and
//! inline (`RayQuery`) traversal of coherent and incoherent rays, closest
//! hit and any hit; plus the memory each structure takes.
//!
//! Correctness is not inferred from timing or images: every validation
//! ray is re-traced by an independent fp64 brute-force CPU reference, and
//! disagreements are accepted only inside per-ray forward-error bounds
//! (see [`Bounds`]). Independently authored; no game data.
//!
//! Usage: `dxr-proto [--runs N] [--rays N] [--csv path]`.

#[cfg(not(windows))]
fn main() {
    eprintln!("dxr-proto runs on Windows only");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::main() {
        eprintln!("dxr-proto: {e}");
        std::process::exit(1);
    }
}


mod common;

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use super::common::{WorldScene, bytes, identity, jitter, median, placed, rays, soup, validate, world};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32B32_FLOAT, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::core::Interface;

    type R<T> = Result<T, String>;
    fn e(x: windows::core::Error) -> String {
        x.message().to_string()
    }

    const HLSL: &str = include_str!("trace.hlsl");

    struct Args {
        runs: usize,
        rays: u32,
        csv: Option<PathBuf>,
    }

    fn args() -> R<Args> {
        let mut a = Args { runs: 5, rays: 1 << 20, csv: None };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let mut v = || it.next().ok_or(format!("{k} needs a value"));
            match k.as_str() {
                "--runs" => a.runs = v()?.parse().map_err(|_| "--runs: a number")?,
                "--rays" => a.rays = v()?.parse().map_err(|_| "--rays: a number")?,
                "--csv" => a.csv = Some(v()?.into()),
                _ => return Err(format!("unknown argument {k}")),
            }
        }
        Ok(a)
    }







    fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size.max(256),
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        }
    }

    fn heap(kind: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
        D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() }
    }

    fn uav_barrier(r: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                // SAFETY: a borrowed pointer for the barrier's lifetime.
                UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: unsafe { std::mem::transmute_copy(r) } }),
            },
            ..Default::default()
        }
    }

    fn transition(r: &ID3D12Resource, from: D3D12_RESOURCE_STATES, to: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    // SAFETY: a borrowed pointer for the barrier's lifetime.
                    pResource: unsafe { std::mem::transmute_copy(r) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: from,
                    StateAfter: to,
                }),
            },
            ..Default::default()
        }
    }

    struct Gpu {
        device: ID3D12Device5,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList4,
        fence: ID3D12Fence,
        done: u64,
        event: HANDLE,
        stamps: ID3D12QueryHeap,
        back: ID3D12Resource,
        frequency: u64,
        root: ID3D12RootSignature,
        pso: ID3D12PipelineState,
    }

    fn dxc() -> R<PathBuf> {
        if let Some(p) = std::env::var_os("SIXTH_DXC") {
            return Ok(p.into());
        }
        let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&kits)
            .map_err(|e| format!("{}: {e}", kits.display()))?
            .filter_map(Result::ok)
            .map(|d| d.path().join(r"x64\dxc.exe"))
            .filter(|p| p.exists())
            .collect();
        found.sort();
        found.pop().ok_or("no dxc.exe in the Windows SDK (set SIXTH_DXC)".into())
    }

    fn compile() -> R<Vec<u8>> {
        let dir = std::env::temp_dir().join("sixth-dxr-proto");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (src, out) = (dir.join("trace.hlsl"), dir.join("trace.cso"));
        std::fs::write(&src, HLSL).map_err(|e| e.to_string())?;
        let ok = std::process::Command::new(dxc()?)
            .args(["-nologo", "-T", "cs_6_5", "-O3", "-E", "trace", "-Fo"])
            .arg(&out)
            .arg(&src)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !ok {
            return Err("dxc failed".into());
        }
        std::fs::read(&out).map_err(|e| e.to_string())
    }

    impl Gpu {
        fn new() -> R<(Gpu, String)> {
            // SAFETY: plain D3D12/DXGI object creation with valid out pointers.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).map_err(e)?;
                let device = device.ok_or("no device")?;
                let mut o5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
                device
                    .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS5, (&raw mut o5).cast(), size_of_val(&o5) as u32)
                    .map_err(e)?;
                if o5.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_1.0 {
                    return Err(format!("raytracing tier {:#x}: inline ray tracing needs tier 1.1", o5.RaytracingTier.0));
                }
                let device: ID3D12Device5 = device.cast().map_err(e)?;
                let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(e)?;
                let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device.GetAdapterLuid()).map_err(e)?;
                let desc = adapter.GetDesc1().map_err(e)?;
                let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
                let record = format!("{name} (vendor {:#06x}, device {:#06x}, raytracing tier {:#x})", desc.VendorId, desc.DeviceId, o5.RaytracingTier.0);
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                    .map_err(e)?;
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).map_err(e)?;
                let list: ID3D12GraphicsCommandList4 =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).map_err(e)?;
                list.Close().map_err(e)?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(e)?;
                let event = CreateEventW(None, false, false, None).map_err(e)?;
                let mut stamps: Option<ID3D12QueryHeap> = None;
                device
                    .CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 }, &mut stamps)
                    .map_err(e)?;
                let mut back: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
                        &heap(D3D12_HEAP_TYPE_READBACK),
                        D3D12_HEAP_FLAG_NONE,
                        &buffer_desc(16, D3D12_RESOURCE_FLAG_NONE),
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        None,
                        &mut back,
                    )
                    .map_err(e)?;
                let frequency = queue.GetTimestampFrequency().map_err(e)?;
                let cso = compile()?;
                let root: ID3D12RootSignature = device.CreateRootSignature(0, &cso).map_err(e)?;
                let pso: ID3D12PipelineState = device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&root),
                        CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cso.as_ptr().cast(), BytecodeLength: cso.len() },
                        ..Default::default()
                    })
                    .map_err(e)?;
                Ok((
                    Gpu {
                        device,
                        queue,
                        allocator,
                        list,
                        fence,
                        done: 0,
                        event,
                        stamps: stamps.ok_or("no query heap")?,
                        back: back.ok_or("no readback")?,
                        frequency,
                        root,
                        pso,
                    },
                    record,
                ))
            }
        }

        fn timed(&mut self, record: impl FnOnce(&ID3D12GraphicsCommandList4)) -> R<f64> {
            // SAFETY: our own list, queue and buffers; one run at a time.
            unsafe {
                self.allocator.Reset().map_err(e)?;
                self.list.Reset(&self.allocator, None).map_err(e)?;
                self.list.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
                record(&self.list);
                self.list.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
                self.list.ResolveQueryData(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &self.back, 0);
                self.list.Close().map_err(e)?;
                self.queue.ExecuteCommandLists(&[Some(self.list.cast().map_err(e)?)]);
                self.done += 1;
                self.queue.Signal(&self.fence, self.done).map_err(e)?;
                self.fence.SetEventOnCompletion(self.done, self.event).map_err(e)?;
                if WaitForSingleObject(self.event, 120_000).0 != 0 {
                    return Err("the GPU did not finish within 120 s".into());
                }
                self.device.GetDeviceRemovedReason().map_err(e)?;
                let mut data: *mut c_void = std::ptr::null_mut();
                self.back.Map(0, None, Some(&mut data)).map_err(e)?;
                let t = std::slice::from_raw_parts(data.cast::<u64>(), 2).to_vec();
                self.back.Unmap(0, None);
                Ok((t[1].saturating_sub(t[0])) as f64 * 1000.0 / self.frequency as f64)
            }
        }

        fn buffer(&self, kind: D3D12_HEAP_TYPE, size: u64, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> R<ID3D12Resource> {
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: plain creation.
            unsafe {
                self.device.CreateCommittedResource(&heap(kind), D3D12_HEAP_FLAG_NONE, &buffer_desc(size, flags), state, None, &mut r).map_err(e)?;
            }
            r.ok_or("no buffer".into())
        }

        fn upload(&self, data: &[u8]) -> R<ID3D12Resource> {
            let b = self.buffer(D3D12_HEAP_TYPE_UPLOAD, data.len() as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ)?;
            // SAFETY: a mappable buffer of at least `data.len()` bytes.
            unsafe {
                let mut p: *mut c_void = std::ptr::null_mut();
                b.Map(0, None, Some(&mut p)).map_err(e)?;
                std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast::<u8>(), data.len());
                b.Unmap(0, None);
            }
            Ok(b)
        }

        /// A DEFAULT-heap buffer holding `data` (copied from staging).
        fn resident(&mut self, data: &[u8], flags: D3D12_RESOURCE_FLAGS) -> R<ID3D12Resource> {
            let up = self.upload(data)?;
            let b = self.buffer(D3D12_HEAP_TYPE_DEFAULT, data.len() as u64, flags, D3D12_RESOURCE_STATE_COMMON)?;
            let n = data.len() as u64;
            // SAFETY: two live buffers of `n` bytes.
            self.timed(|l| unsafe { l.CopyBufferRegion(&b, 0, &up, 0, n) })?;
            Ok(b)
        }

        fn read(&mut self, src: &ID3D12Resource, state: D3D12_RESOURCE_STATES, size: u64) -> R<Vec<u8>> {
            let back = self.buffer(D3D12_HEAP_TYPE_READBACK, size, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
            // SAFETY: live buffers of at least `size` bytes.
            self.timed(|l| unsafe {
                l.ResourceBarrier(&[transition(src, state, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                l.CopyBufferRegion(&back, 0, src, 0, size);
                l.ResourceBarrier(&[transition(src, D3D12_RESOURCE_STATE_COPY_SOURCE, state)]);
            })?;
            // SAFETY: mappable, `size` bytes.
            unsafe {
                let mut p: *mut c_void = std::ptr::null_mut();
                back.Map(0, None, Some(&mut p)).map_err(e)?;
                let v = std::slice::from_raw_parts(p.cast::<u8>(), size as usize).to_vec();
                back.Unmap(0, None);
                Ok(v)
            }
        }
    }

    /// One acceleration structure and what it took.
    struct As {
        result: ID3D12Resource,
        scratch: ID3D12Resource,
        info: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO,
    }

    fn geometry(vb: u64, vcount: u32, ib: u64, icount: u32) -> D3D12_RAYTRACING_GEOMETRY_DESC {
        D3D12_RAYTRACING_GEOMETRY_DESC {
            Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
            Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
            Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                    Transform3x4: 0,
                    IndexFormat: DXGI_FORMAT_R32_UINT,
                    VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                    IndexCount: icount,
                    VertexCount: vcount,
                    IndexBuffer: ib,
                    VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: vb, StrideInBytes: 12 },
                },
            },
        }
    }

    fn blas_inputs(g: &D3D12_RAYTRACING_GEOMETRY_DESC, flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS) -> D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
            Flags: flags,
            NumDescs: 1,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: g },
        }
    }

    fn tlas_inputs(instances: u64, n: u32, flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS) -> D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: flags,
            NumDescs: n,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: instances },
        }
    }

    /// Allocates and builds; returns the structure and the GPU ms of the build.
    fn build(gpu: &mut Gpu, inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS, compacted: Option<&ID3D12Resource>) -> R<(As, f64)> {
        let mut info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        // SAFETY: valid inputs.
        unsafe { gpu.device.GetRaytracingAccelerationStructurePrebuildInfo(inputs, &mut info) };
        let result = gpu.buffer(
            D3D12_HEAP_TYPE_DEFAULT,
            info.ResultDataMaxSizeInBytes,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;
        let scratch = gpu.buffer(
            D3D12_HEAP_TYPE_DEFAULT,
            info.ScratchDataSizeInBytes.max(info.UpdateScratchDataSizeInBytes),
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;
        let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            // SAFETY: live buffers.
            DestAccelerationStructureData: unsafe { result.GetGPUVirtualAddress() },
            Inputs: *inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: unsafe { scratch.GetGPUVirtualAddress() },
        };
        let post = compacted.map(|c| D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
            // SAFETY: a live buffer in the unordered-access state.
            DestBuffer: unsafe { c.GetGPUVirtualAddress() },
            InfoType: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE,
        });
        let ms = gpu.timed(|l| unsafe {
            l.BuildRaytracingAccelerationStructure(&desc, post.as_ref().map(std::slice::from_ref));
            l.ResourceBarrier(&[uav_barrier(&result)]);
        })?;
        Ok((As { result, scratch, info }, ms))
    }

    /// Rebuilds in place from new inputs (an update: `PERFORM_UPDATE`).
    fn update(gpu: &mut Gpu, a: &As, inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS) -> R<f64> {
        let mut inputs = *inputs;
        inputs.Flags |= D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE;
        // SAFETY: live buffers.
        let va = unsafe { a.result.GetGPUVirtualAddress() };
        let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: va,
            Inputs: inputs,
            SourceAccelerationStructureData: va,
            ScratchAccelerationStructureData: unsafe { a.scratch.GetGPUVirtualAddress() },
        };
        let r = a.result.clone();
        gpu.timed(|l| unsafe {
            l.BuildRaytracingAccelerationStructure(&desc, None);
            l.ResourceBarrier(&[uav_barrier(&r)]);
        })
    }

    fn instance(blas: u64, transform: [f32; 12], id: u32, mask: u8) -> D3D12_RAYTRACING_INSTANCE_DESC {
        D3D12_RAYTRACING_INSTANCE_DESC {
            Transform: transform,
            _bitfield1: (id & 0x00FF_FFFF) | u32::from(mask) << 24,
            _bitfield2: 0,
            AccelerationStructure: blas,
        }
    }




    /// Traces `rays` against the TLAS; the GPU ms and the hits.
    fn trace(gpu: &mut Gpu, tlas: &ID3D12Resource, ray_buf: &ID3D12Resource, hits: &ID3D12Resource, n: u32, anyhit: bool, mask: u32) -> R<f64> {
        // SAFETY: live buffers; the root parameters match the shader.
        let (tv, rv, hv) = unsafe { (tlas.GetGPUVirtualAddress(), ray_buf.GetGPUVirtualAddress(), hits.GetGPUVirtualAddress()) };
        let (root, pso) = (gpu.root.clone(), gpu.pso.clone());
        let c = [n, u32::from(anyhit), mask, 0];
        let h = hits.clone();
        gpu.timed(|l| unsafe {
            l.SetComputeRootSignature(&root);
            l.SetPipelineState(&pso);
            l.SetComputeRoot32BitConstants(0, 4, c.as_ptr().cast(), 0);
            l.SetComputeRootShaderResourceView(1, tv);
            l.SetComputeRootShaderResourceView(2, rv);
            l.SetComputeRootUnorderedAccessView(3, hv);
            l.Dispatch(n.div_ceil(64), 1, 1);
            l.ResourceBarrier(&[uav_barrier(&h)]);
        })
    }

    struct Out {
        csv: Vec<String>,
        adapter: String,
    }
    impl Out {
        fn note(&mut self, test: &str, variant: &str, metric: &str, value: f64) {
            self.csv.push(format!("\"{}\",{test},{variant},{metric},{value:.4}", self.adapter));
            println!("  {test:<12} {variant:<34} {metric:<22} {value:>12.3}");
        }
    }




    pub fn main() -> R<()> {
        let a = args()?;
        let (mut gpu, adapter) = Gpu::new()?;
        println!("adapter: {adapter}");
        println!("runs: {} (median after one warm-up), rays: {}", a.runs, a.rays);
        let mut out = Out { csv: Vec::new(), adapter };
        let uav = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;

        // ---------------- 1. Correctness against the fp64 reference -------------
        println!("\n[validate] GPU RayQuery vs fp64 brute-force reference");
        {
            // V1: 20,000 random triangles, one instance.
            let mesh = soup(20_000, 100.0, 11);
            let vb = gpu.resident(bytes(&mesh.v), D3D12_RESOURCE_FLAG_NONE)?;
            let ib = gpu.resident(bytes(&mesh.i), D3D12_RESOURCE_FLAG_NONE)?;
            let g = unsafe { geometry(vb.GetGPUVirtualAddress(), mesh.v.len() as u32, ib.GetGPUVirtualAddress(), mesh.i.len() as u32) };
            let (blas, _) = build(&mut gpu, &blas_inputs(&g, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE), None)?;
            // V2: 6×6 instances of a 3,000-triangle blob, masks 1 and 2.
            let blob = soup(3_000, 20.0, 12);
            let vb2 = gpu.resident(bytes(&blob.v), D3D12_RESOURCE_FLAG_NONE)?;
            let ib2 = gpu.resident(bytes(&blob.i), D3D12_RESOURCE_FLAG_NONE)?;
            let g2 = unsafe { geometry(vb2.GetGPUVirtualAddress(), blob.v.len() as u32, ib2.GetGPUVirtualAddress(), blob.i.len() as u32) };
            let (blas2, _) = build(&mut gpu, &blas_inputs(&g2, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE), None)?;
            let mut placements = Vec::new();
            for gy in 0..6 {
                for gx in 0..6 {
                    let m = placed(0.37 * (gx * 6 + gy) as f32, [(gx as f32 - 2.5) * 40.0, (gy as f32 - 2.5) * 40.0, 0.0]);
                    placements.push((m, if (gx + gy) % 2 == 0 { 1u8 } else { 2u8 }));
                }
            }
            let scenes: Vec<(&str, Vec<D3D12_RAYTRACING_INSTANCE_DESC>, WorldScene)> = vec![
                ("v1 20k triangles", vec![instance(unsafe { blas.result.GetGPUVirtualAddress() }, identity(), 0, 0xFF)], world(&mesh, &[(identity(), 0xFF)])),
                (
                    "v2 36 instances x 3k, masks",
                    placements.iter().enumerate().map(|(i, (m, k))| instance(unsafe { blas2.result.GetGPUVirtualAddress() }, *m, i as u32, *k)).collect(),
                    world(&blob, &placements),
                ),
            ];
            for (name, insts, w) in &scenes {
                let ib = gpu.upload(bytes(insts))?;
                let (tlas, _) = build(&mut gpu, &tlas_inputs(unsafe { ib.GetGPUVirtualAddress() }, insts.len() as u32, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE), None)?;
                for (coherent, label) in [(true, "coherent"), (false, "incoherent")] {
                    let rs = rays(2048, 100.0, coherent, 21 + u64::from(coherent));
                    let rb = gpu.resident(bytes(&rs), D3D12_RESOURCE_FLAG_NONE)?;
                    let hits = gpu.buffer(D3D12_HEAP_TYPE_DEFAULT, 2048 * 16, uav, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
                    for (anyhit, mask) in [(false, 0xFFu8), (true, 0xFF), (false, 0x01)] {
                        trace(&mut gpu, &tlas.result, &rb, &hits, 2048, anyhit, u32::from(mask))?;
                        let raw = gpu.read(&hits, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, 2048 * 16)?;
                        let h: Vec<[u32; 4]> = raw.as_chunks::<16>().0.iter().map(|c| std::array::from_fn(|k| u32::from_le_bytes(c[4 * k..4 * k + 4].try_into().unwrap()))).collect();
                        let hit_count = h.iter().filter(|x| x[0] != 0).count();
                        let (n, tol, fail, first) = validate(w, &rs, &h, anyhit, mask);
                        let variant = format!("{name} {label} {} mask {mask:#x}", if anyhit { "any" } else { "closest" });
                        println!("[validate] {variant:<62} rays {n}, GPU hits {hit_count}, tolerated {tol}, FAILED {fail}{}", if first.is_empty() { String::new() } else { format!(" ({first})") });
                        out.csv.push(format!("\"{}\",validate,{variant},failed,{fail}", out.adapter));
                        out.csv.push(format!("\"{}\",validate,{variant},tolerated,{tol}", out.adapter));
                        if fail != 0 {
                            return Err(format!("validation failed: {variant}: {fail} rays"));
                        }
                    }
                }
            }
        }

        // ---------------- 2. Performance ----------------
        println!("\n[perf] GPU ms (median), memory in MiB");
        for &tris in &[100_000usize, 1_000_000] {
            let mesh = soup(tris, 100.0, 31);
            let moved = jitter(&mesh, 0.5, 32);
            let vb = gpu.resident(bytes(&mesh.v), D3D12_RESOURCE_FLAG_NONE)?;
            let vb_moved = gpu.resident(bytes(&moved), D3D12_RESOURCE_FLAG_NONE)?;
            let ib = gpu.resident(bytes(&mesh.i), D3D12_RESOURCE_FLAG_NONE)?;
            let (vv, vm, iv) = unsafe { (vb.GetGPUVirtualAddress(), vb_moved.GetGPUVirtualAddress(), ib.GetGPUVirtualAddress()) };
            let g = geometry(vv, mesh.v.len() as u32, iv, mesh.i.len() as u32);
            let gm = geometry(vm, mesh.v.len() as u32, iv, mesh.i.len() as u32);
            let label = format!("{}k tris", tris / 1000);
            for (fl, name) in [
                (D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE, "build fast-trace"),
                (D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD, "build fast-build"),
            ] {
                let mut t = Vec::new();
                let mut size = 0;
                for _ in 0..=a.runs {
                    let (s, ms) = build(&mut gpu, &blas_inputs(&g, fl), None)?;
                    size = s.info.ResultDataMaxSizeInBytes;
                    t.push(ms);
                }
                out.note("blas", &format!("{label} {name}"), "gpu_ms", median(t[1..].to_vec()));
                out.note("blas", &format!("{label} {name}"), "max_size_mib", size as f64 / 1048576.0);
            }
            // Compaction.
            let post = gpu.buffer(D3D12_HEAP_TYPE_DEFAULT, 8, uav, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
            let (full, _) = build(
                &mut gpu,
                &blas_inputs(&g, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE | D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_COMPACTION),
                Some(&post),
            )?;
            let size = u64::from_le_bytes(gpu.read(&post, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, 8)?.try_into().unwrap());
            let small = gpu.buffer(D3D12_HEAP_TYPE_DEFAULT, size, uav, D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE)?;
            let (src, dst) = unsafe { (full.result.GetGPUVirtualAddress(), small.GetGPUVirtualAddress()) };
            let ms = gpu.timed(|l| unsafe { l.CopyRaytracingAccelerationStructure(dst, src, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT) })?;
            out.note("blas", &format!("{label} compaction"), "gpu_ms", ms);
            out.note("blas", &format!("{label} compaction"), "compacted_mib", size as f64 / 1048576.0);
            // Refit after every vertex moved.
            let upd = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE | D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE;
            let (refit_as, _) = build(&mut gpu, &blas_inputs(&g, upd), None)?;
            let mut t = Vec::new();
            for k in 0..=a.runs {
                let gk = if k % 2 == 0 { &gm } else { &g };
                t.push(update(&mut gpu, &refit_as, &blas_inputs(gk, upd))?);
            }
            out.note("blas", &format!("{label} refit (moved vertices)"), "gpu_ms", median(t[1..].to_vec()));

            // TLAS of one instance; traversal.
            let insts = [instance(unsafe { small.GetGPUVirtualAddress() }, identity(), 0, 0xFF)];
            let ibuf = gpu.upload(bytes(&insts))?;
            let (tlas, _) = build(&mut gpu, &tlas_inputs(unsafe { ibuf.GetGPUVirtualAddress() }, 1, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE), None)?;
            for (coherent, cl) in [(true, "coherent"), (false, "incoherent")] {
                let rs = rays(a.rays, 100.0, coherent, 41);
                let rb = gpu.resident(bytes(&rs), D3D12_RESOURCE_FLAG_NONE)?;
                let hits = gpu.buffer(D3D12_HEAP_TYPE_DEFAULT, u64::from(a.rays) * 16, uav, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
                for (anyhit, al) in [(false, "closest"), (true, "any")] {
                    let mut t = Vec::new();
                    for _ in 0..=a.runs {
                        t.push(trace(&mut gpu, &tlas.result, &rb, &hits, a.rays, anyhit, 0xFF)?);
                    }
                    let m = median(t[1..].to_vec());
                    out.note("trace", &format!("{label} {cl} {al}"), "gpu_ms", m);
                    out.note("trace", &format!("{label} {cl} {al}"), "mrays_per_s", f64::from(a.rays) / (m / 1000.0) / 1e6);
                }
            }
        }

        // Instanced scene: TLAS build and update over many instances.
        {
            let blob = soup(10_000, 20.0, 51);
            let vb = gpu.resident(bytes(&blob.v), D3D12_RESOURCE_FLAG_NONE)?;
            let ib = gpu.resident(bytes(&blob.i), D3D12_RESOURCE_FLAG_NONE)?;
            let g = unsafe { geometry(vb.GetGPUVirtualAddress(), blob.v.len() as u32, ib.GetGPUVirtualAddress(), blob.i.len() as u32) };
            let (blas, _) = build(&mut gpu, &blas_inputs(&g, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE), None)?;
            let bva = unsafe { blas.result.GetGPUVirtualAddress() };
            for side in [32u32, 100] {
                let n = side * side;
                let make = |shift: f32| -> Vec<D3D12_RAYTRACING_INSTANCE_DESC> {
                    (0..n)
                        .map(|i| {
                            let (x, y) = ((i % side) as f32, (i / side) as f32);
                            instance(bva, placed(0.1 * i as f32 + shift, [(x - side as f32 / 2.0) * 45.0, (y - side as f32 / 2.0) * 45.0, shift * 10.0]), i, 0xFF)
                        })
                        .collect()
                };
                let (i0, i1) = (gpu.upload(bytes(&make(0.0)))?, gpu.upload(bytes(&make(0.3)))?);
                let flags = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE | D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE;
                let mut t = Vec::new();
                let mut tl = None;
                for _ in 0..=a.runs {
                    let (s, ms) = build(&mut gpu, &tlas_inputs(unsafe { i0.GetGPUVirtualAddress() }, n, flags), None)?;
                    t.push(ms);
                    tl = Some(s);
                }
                let tl = tl.unwrap();
                let label = format!("{n} instances of 10k tris");
                out.note("tlas", &format!("{label} build"), "gpu_ms", median(t[1..].to_vec()));
                out.note("tlas", &format!("{label} build"), "max_size_mib", tl.info.ResultDataMaxSizeInBytes as f64 / 1048576.0);
                let mut t = Vec::new();
                for k in 0..=a.runs {
                    let src = if k % 2 == 0 { &i1 } else { &i0 };
                    t.push(update(&mut gpu, &tl, &tlas_inputs(unsafe { src.GetGPUVirtualAddress() }, n, flags))?);
                }
                out.note("tlas", &format!("{label} update (all moved)"), "gpu_ms", median(t[1..].to_vec()));
                let rs = rays(a.rays, (side as f32) * 22.0, false, 61);
                let rb = gpu.resident(bytes(&rs), D3D12_RESOURCE_FLAG_NONE)?;
                let hits = gpu.buffer(D3D12_HEAP_TYPE_DEFAULT, u64::from(a.rays) * 16, uav, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
                let mut t = Vec::new();
                for _ in 0..=a.runs {
                    t.push(trace(&mut gpu, &tl.result, &rb, &hits, a.rays, false, 0xFF)?);
                }
                let m = median(t[1..].to_vec());
                out.note("trace", &format!("{label} incoherent closest"), "gpu_ms", m);
                out.note("trace", &format!("{label} incoherent closest"), "mrays_per_s", f64::from(a.rays) / (m / 1000.0) / 1e6);
            }
        }

        if let Some(p) = a.csv {
            let mut s = String::from("adapter,test,variant,metric,value\n");
            s += &out.csv.join("\n");
            s.push('\n');
            std::fs::write(&p, s).map_err(|e| e.to_string())?;
            println!("\ncsv: {}", p.display());
        }
        Ok(())
    }
}
