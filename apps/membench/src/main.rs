//! B-30 / Gen 2 Phase 2: synthetic GPU memory benchmark. Independently
//! authored; no game data. GPU times come from timestamps, CPU times from
//! the wall clock; every reported number is the median of `--runs` runs
//! after one warm-up.
//!
//! Suites (`--suite backings|gen2|align|forward|all`, default all):
//!
//! - `backings`: the same data in six backings (VRAM, system memory
//!   write-back, upload, process memory opened as a heap, a SEC_COMMIT
//!   section heap, a reserved buffer on VRAM tiles), read three ways
//!   (stream, random 16 B, dependent chase).
//! - `gen2`: direct vs page-table-indirected access (table in VRAM and in
//!   system memory), dirty-set forwarding (small/medium/large), the CPU
//!   write → GPU read round trip, sparse remapping cost, aliasing
//!   correctness, and GPU upload heaps (where the device has them).
//! - `forward`: D5, CPU→GPU forwarding through `sixth-memory`'s planner:
//!   one copy per dirty 4 KiB unit vs coalesced runs vs runs with filled
//!   gaps, for scattered, clustered, gappy and contiguous dirty sets. The
//!   whole VRAM buffer is compared with the CPU view after every variant.
//!
//! Usage: `membench [--suite S] [--mib N] [--runs N] [--csv path]`.

#[cfg(not(windows))]
fn main() {
    eprintln!("membench runs on Windows only");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::main() {
        eprintln!("membench: {e}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::time::Instant;

    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MapViewOfFile, PAGE_READWRITE, SEC_COMMIT,
        UnmapViewOfFile, VirtualAlloc, VirtualFree,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::core::Interface;

    type R<T> = Result<T, String>;

    fn e(x: windows::core::Error) -> String {
        x.message().to_string()
    }

    const HLSL: &str = include_str!("bench.hlsl");
    const ENTRIES: [&str; 11] = ["stream", "random", "chase", "stream_dir", "random_dir", "chase_dir", "probe", "poke", "stream_sb", "random_sb", "chase_sb"];
    const THREADS: u32 = 256 * 1024;
    const CHASE_THREADS: u32 = 256 * 256;
    const RANDOM_ITERS: u32 = 64;
    const CHASE_ITERS: u32 = 256;
    const PAGE: u64 = 1 << 16;

    struct Args {
        suite: String,
        mib: u64,
        runs: usize,
        csv: Option<PathBuf>,
    }

    fn args() -> R<Args> {
        let mut a = Args { suite: "all".into(), mib: 256, runs: 5, csv: None };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let mut v = || it.next().ok_or(format!("{k} needs a value"));
            match k.as_str() {
                "--suite" => a.suite = v()?,
                "--mib" => a.mib = v()?.parse().map_err(|_| "--mib: a number")?,
                "--runs" => a.runs = v()?.parse().map_err(|_| "--runs: a number")?,
                "--csv" => a.csv = Some(v()?.into()),
                _ => return Err(format!("unknown argument {k}")),
            }
        }
        if a.mib == 0 || a.mib >= 4096 {
            return Err("--mib must be 1..4095".into());
        }
        if !matches!(a.suite.as_str(), "all" | "backings" | "gen2" | "align" | "forward") {
            return Err("--suite: all, backings, gen2, align or forward".into());
        }
        Ok(a)
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

    fn compile(entry: &str) -> R<Vec<u8>> {
        let dir = std::env::temp_dir().join("sixth-membench");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let src = dir.join("bench.hlsl");
        std::fs::write(&src, HLSL).map_err(|e| e.to_string())?;
        let out = dir.join(format!("{entry}.cso"));
        let status = std::process::Command::new(dxc()?)
            .args(["-nologo", "-T", "cs_6_6", "-O3", "-E", entry, "-Fo"])
            .arg(&out)
            .arg(&src)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("dxc failed for {entry}"));
        }
        std::fs::read(&out).map_err(|e| e.to_string())
    }

    fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
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

    fn sys_wb() -> D3D12_HEAP_PROPERTIES {
        D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
            MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
            ..Default::default()
        }
    }

    struct Gpu {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        done: u64,
        event: HANDLE,
        stamps: ID3D12QueryHeap,
        back: ID3D12Resource,
        frequency: u64,
        root: Option<ID3D12RootSignature>,
        pso: HashMap<&'static str, ID3D12PipelineState>,
    }

    impl Gpu {
        fn new() -> R<(Gpu, String)> {
            // SAFETY: plain D3D12/DXGI object creation with valid out pointers.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).map_err(e)?;
                let device = device.ok_or("no device")?;
                let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(e)?;
                let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device.GetAdapterLuid()).map_err(e)?;
                let desc = adapter.GetDesc1().map_err(e)?;
                let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
                let record = format!(
                    "{name} (vendor {:#06x}, device {:#06x}, {} MiB dedicated, {} MiB shared)",
                    desc.VendorId,
                    desc.DeviceId,
                    desc.DedicatedVideoMemory >> 20,
                    desc.SharedSystemMemory >> 20
                );
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                    .map_err(e)?;
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).map_err(e)?;
                let list: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).map_err(e)?;
                list.Close().map_err(e)?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(e)?;
                let event = CreateEventW(None, false, false, None).map_err(e)?;
                let mut stamps: Option<ID3D12QueryHeap> = None;
                device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 },
                        &mut stamps,
                    )
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
                let mut gpu = Gpu {
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
                    root: None,
                    pso: HashMap::new(),
                };
                for entry in ENTRIES {
                    let bytes = compile(entry)?;
                    if gpu.root.is_none() {
                        gpu.root = Some(gpu.device.CreateRootSignature(0, &bytes).map_err(e)?);
                    }
                    let pso: ID3D12PipelineState = gpu
                        .device
                        .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                            pRootSignature: std::mem::transmute_copy(gpu.root.as_ref().unwrap()),
                            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
                            ..Default::default()
                        })
                        .map_err(e)?;
                    gpu.pso.insert(entry, pso);
                }
                Ok((gpu, record))
            }
        }

        /// Records `record` between two timestamps, runs it, waits; the
        /// GPU milliseconds between the stamps.
        fn timed(&mut self, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> R<f64> {
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
                self.wait()?;
                let mut data: *mut c_void = std::ptr::null_mut();
                self.back.Map(0, None, Some(&mut data)).map_err(e)?;
                let t = std::slice::from_raw_parts(data.cast::<u64>(), 2).to_vec();
                self.back.Unmap(0, None);
                Ok((t[1].saturating_sub(t[0])) as f64 * 1000.0 / self.frequency as f64)
            }
        }

        /// Signals the queue and waits for it.
        fn wait(&mut self) -> R<()> {
            // SAFETY: our own queue, fence and event.
            unsafe {
                self.done += 1;
                self.queue.Signal(&self.fence, self.done).map_err(e)?;
                self.fence.SetEventOnCompletion(self.done, self.event).map_err(e)?;
                if WaitForSingleObject(self.event, 60_000).0 != 0 {
                    return Err("the GPU did not finish within 60 s".into());
                }
                self.device.GetDeviceRemovedReason().map_err(e)
            }
        }

        fn committed(&self, props: D3D12_HEAP_PROPERTIES, size: u64, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> R<ID3D12Resource> {
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: plain creation.
            unsafe {
                self.device
                    .CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &buffer_desc(size, flags), state, None, &mut r)
                    .map_err(e)?;
            }
            r.ok_or("no resource".into())
        }

        /// One dispatch of `entry` with the constants, source, destination
        /// and page table given.
        fn dispatch(
            &mut self,
            entry: &str,
            c: [u32; 4],
            src: u64,
            dst: u64,
            dir: u64,
            groups: u32,
        ) -> R<f64> {
            let pso = self.pso[entry].clone();
            let root = self.root.clone().unwrap();
            self.timed(|l| unsafe {
                l.SetComputeRootSignature(&root);
                l.SetPipelineState(&pso);
                l.SetComputeRoot32BitConstants(0, 4, c.as_ptr().cast(), 0);
                l.SetComputeRootShaderResourceView(1, src);
                l.SetComputeRootUnorderedAccessView(2, dst);
                l.SetComputeRootShaderResourceView(3, dir);
                l.SetComputeRootShaderResourceView(4, src);
                l.Dispatch(groups, 1, 1);
            })
        }
    }

    fn fill(buffer: &ID3D12Resource, at: usize, data: &[u8]) -> R<()> {
        // SAFETY: a mappable buffer of at least `at + data.len()` bytes.
        unsafe {
            let mut p: *mut c_void = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut p)).map_err(e)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast::<u8>().add(at), data.len());
            buffer.Unmap(0, None);
        }
        Ok(())
    }

    fn read(buffer: &ID3D12Resource, at: usize, len: usize) -> R<Vec<u8>> {
        // SAFETY: a mappable buffer of at least `at + len` bytes.
        unsafe {
            let mut p: *mut c_void = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut p)).map_err(e)?;
            let v = std::slice::from_raw_parts(p.cast::<u8>().add(at), len).to_vec();
            buffer.Unmap(0, None);
            Ok(v)
        }
    }

    /// The test data: element i's first dword is the next element of one
    /// random cycle (Sattolo), the rest arbitrary.
    fn data(bytes: u64) -> Vec<u8> {
        let n = (bytes / 16) as usize;
        let mut next: Vec<u32> = (0..n as u32).collect();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = |m: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % m as u64) as usize
        };
        for i in (1..n).rev() {
            let j = rnd(i);
            next.swap(i, j);
        }
        let mut out = vec![0u8; bytes as usize];
        for (i, chunk) in out.as_chunks_mut::<16>().0.iter_mut().enumerate() {
            chunk[0..4].copy_from_slice(&next[i].to_le_bytes());
            chunk[4..8].copy_from_slice(&(i as u32).to_le_bytes());
        }
        out
    }

    /// An identity page table for `bytes`: entry p = (1, p · 64 KiB).
    fn page_table(bytes: u64) -> Vec<u8> {
        let mut t = Vec::new();
        for p in 0..bytes / PAGE {
            t.extend(1u32.to_le_bytes());
            t.extend(((p * PAGE) as u32).to_le_bytes());
        }
        t
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    struct Ctx {
        gpu: Gpu,
        a: Args,
        bytes: u64,
        content: Vec<u8>,
        upload: ID3D12Resource,
        vram: ID3D12Resource,
        dst: ID3D12Resource,
        dir_vram: ID3D12Resource,
        dir_sys: ID3D12Resource,
        csv: Vec<String>,
        adapter: String,
    }

    impl Ctx {
        fn note(&mut self, test: &str, variant: &str, metric: &str, value: f64) {
            self.csv.push(format!("\"{}\",{test},{variant},{metric},{value:.4}", self.adapter));
        }

        /// Median over `runs` of the three access patterns on `src`, with
        /// the page table `dir` (only the `_dir` kernels read it).
        fn patterns(&mut self, src: u64, dir: u64, suffix: &str) -> R<(f64, f64, f64)> {
            let n16 = (self.bytes / 16) as u32;
            let out = unsafe { self.dst.GetGPUVirtualAddress() };
            let runs = self.a.runs;
            let go = |gpu: &mut Gpu, entry: &str, iters: u32, total: u32| -> R<f64> {
                let mut t = Vec::new();
                for _ in 0..=runs {
                    t.push(gpu.dispatch(entry, [n16, iters, total, 0], src, out, dir, total / 256)?);
                }
                Ok(median(t[1..].to_vec()))
            };
            let s = go(&mut self.gpu, &format!("stream{suffix}"), 0, THREADS)?;
            let r = go(&mut self.gpu, &format!("random{suffix}"), RANDOM_ITERS, THREADS)?;
            let c = go(&mut self.gpu, &format!("chase{suffix}"), CHASE_ITERS, CHASE_THREADS)?;
            Ok((
                self.bytes as f64 / (s / 1000.0) / 1e9,
                (u64::from(THREADS) * u64::from(RANDOM_ITERS) * 16) as f64 / (r / 1000.0) / 1e9,
                c * 1000.0 / f64::from(CHASE_ITERS),
            ))
        }
    }

    pub fn main() -> R<()> {
        let a = args()?;
        let bytes = a.mib << 20;
        let (gpu, adapter) = Gpu::new()?;
        println!("adapter: {adapter}");
        println!("buffer: {} MiB, runs: {} (median after one warm-up)", a.mib, a.runs);
        let content = data(bytes);
        let upload = gpu.committed(heap(D3D12_HEAP_TYPE_UPLOAD), bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ)?;
        fill(&upload, 0, &content)?;
        let vram = gpu.committed(heap(D3D12_HEAP_TYPE_DEFAULT), bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        let dst = gpu.committed(
            heap(D3D12_HEAP_TYPE_DEFAULT),
            u64::from(THREADS) * 4,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        let table = page_table(bytes);
        let dir_sys = gpu.committed(sys_wb(), table.len() as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        fill(&dir_sys, 0, &table)?;
        let dir_up = gpu.committed(heap(D3D12_HEAP_TYPE_UPLOAD), table.len() as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ)?;
        fill(&dir_up, 0, &table)?;
        let dir_vram = gpu.committed(heap(D3D12_HEAP_TYPE_DEFAULT), table.len() as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        let mut c = Ctx { gpu, a, bytes, content, upload, vram, dst, dir_vram, dir_sys, csv: Vec::new(), adapter };
        // SAFETY: live buffers of the sizes given.
        c.gpu.timed(|l| unsafe {
            l.CopyBufferRegion(&c.vram, 0, &c.upload, 0, bytes);
            l.CopyBufferRegion(&c.dir_vram, 0, &dir_up, 0, table.len() as u64);
        })?;
        drop(dir_up);
        if matches!(c.a.suite.as_str(), "all" | "backings") {
            backings(&mut c)?;
        }
        if matches!(c.a.suite.as_str(), "all" | "align") {
            align_suite(&mut c)?;
        }
        if matches!(c.a.suite.as_str(), "all" | "forward") {
            forwarding(&mut c)?;
        }
        if matches!(c.a.suite.as_str(), "all" | "gen2") {
            indirection(&mut c)?;
            dirty_sets(&mut c)?;
            round_trip(&mut c)?;
            remapping(&mut c)?;
            aliasing(&mut c)?;
            upload_heap(&mut c)?;
        }
        if let Some(path) = c.a.csv.clone() {
            let mut out = String::from("adapter,test,variant,metric,value\n");
            out += &c.csv.join("\n");
            out.push('\n');
            std::fs::write(&path, out).map_err(|e| e.to_string())?;
            println!("\ncsv: {}", path.display());
        }
        Ok(())
    }

    /// The same data in six backings, three access patterns.
    fn backings(c: &mut Ctx) -> R<()> {
        let bytes = c.bytes;
        let sys = c.gpu.committed(sys_wb(), bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        fill(&sys, 0, &c.content)?;
        // SAFETY: a fresh allocation of `bytes`, released at the end.
        let title_mem = unsafe { VirtualAlloc(None, bytes as usize, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        if title_mem.is_null() {
            return Err("VirtualAlloc failed".into());
        }
        // SAFETY: the allocation holds `bytes` bytes.
        unsafe { std::ptr::copy_nonoverlapping(c.content.as_ptr(), title_mem.cast::<u8>(), c.content.len()) };
        let placed = |gpu: &Gpu, h: &ID3D12Heap| -> R<ID3D12Resource> {
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: a live heap of at least `bytes`.
            unsafe {
                gpu.device
                    .CreatePlacedResource(
                        h,
                        0,
                        &buffer_desc(bytes, D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS),
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut r,
                    )
                    .map_err(e)?;
            }
            r.ok_or("no placed buffer".into())
        };
        let title = (|| -> R<ID3D12Resource> {
            // SAFETY: the base of a whole committed allocation.
            let h: ID3D12Heap = unsafe { c.gpu.device.cast::<ID3D12Device3>().map_err(e)?.OpenExistingHeapFromAddress(title_mem).map_err(e)? };
            placed(&c.gpu, &h)
        })();
        let section = (|| -> R<(ID3D12Resource, HANDLE)> {
            // SAFETY: a fresh pagefile-backed section; released at the end.
            unsafe {
                let h = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE | SEC_COMMIT, (bytes >> 32) as u32, bytes as u32, None)
                    .map_err(e)?;
                let view = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, bytes as usize);
                if view.Value.is_null() {
                    return Err("MapViewOfFile failed".into());
                }
                std::ptr::copy_nonoverlapping(c.content.as_ptr(), view.Value.cast::<u8>(), c.content.len());
                let _ = UnmapViewOfFile(view);
                let heap: ID3D12Heap = c.gpu.device.cast::<ID3D12Device3>().map_err(e)?.OpenExistingHeapFromFileMapping(h).map_err(e)?;
                Ok((placed(&c.gpu, &heap)?, h))
            }
        })();
        let sparse = sparse_buffer(c, bytes);
        if let Ok((r, _)) = &sparse {
            let up = c.upload.clone();
            // SAFETY: two live buffers of `bytes` bytes.
            c.gpu.timed(|l| unsafe { l.CopyBufferRegion(r, 0, &up, 0, bytes) })?;
        }
        let mut list: Vec<(&str, ID3D12Resource)> =
            vec![("vram", c.vram.clone()), ("sys-wb", sys.clone()), ("upload", c.upload.clone())];
        for (name, made) in [
            ("title", title.as_ref().cloned()),
            ("section", section.as_ref().map(|(r, _)| r.clone())),
            ("sparse", sparse.as_ref().map(|(r, _)| r.clone())),
        ] {
            match made {
                Ok(r) => list.push((name, r)),
                Err(why) => println!("{name}: not available: {why}"),
            }
        }
        println!("\n[backings] {:<8} {:>12} {:>12} {:>18}", "backing", "stream GB/s", "random GB/s", "chase us/step");
        let dir = unsafe { c.dir_vram.GetGPUVirtualAddress() };
        for (name, r) in list {
            let va = unsafe { r.GetGPUVirtualAddress() };
            let (s, rd, ch) = c.patterns(va, dir, "")?;
            println!("[backings] {name:<8} {s:>12.1} {rd:>12.2} {ch:>18.1}");
            c.note("backings", name, "stream_gbs", s);
            c.note("backings", name, "random_gbs", rd);
            c.note("backings", name, "chase_us_per_step", ch);
        }
        drop(title);
        drop(sparse);
        if let Ok((r, h)) = section {
            drop(r);
            // SAFETY: our own handle.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        // SAFETY: our own allocation; the heap opened on it is gone.
        unsafe {
            let _ = VirtualFree(title_mem, 0, MEM_RELEASE);
        }
        Ok(())
    }

    /// A reserved buffer of `bytes` whose tiles all map onto one DEFAULT heap.
    fn sparse_buffer(c: &mut Ctx, bytes: u64) -> R<(ID3D12Resource, ID3D12Heap)> {
        // SAFETY: plain creation; the mapping covers the whole buffer.
        unsafe {
            let mut r: Option<ID3D12Resource> = None;
            c.gpu
                .device
                .CreateReservedResource(&buffer_desc(bytes, D3D12_RESOURCE_FLAG_NONE), D3D12_RESOURCE_STATE_COMMON, None, &mut r)
                .map_err(e)?;
            let r = r.ok_or("no reserved buffer")?;
            let mut h: Option<ID3D12Heap> = None;
            c.gpu
                .device
                .CreateHeap(
                    &D3D12_HEAP_DESC {
                        SizeInBytes: bytes,
                        Properties: heap(D3D12_HEAP_TYPE_DEFAULT),
                        Alignment: 0,
                        Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS,
                    },
                    &mut h,
                )
                .map_err(e)?;
            let h = h.ok_or("no heap")?;
            map_tiles(&c.gpu, &r, &h, 0, 0, (bytes / PAGE) as u32);
            Ok((r, h))
        }
    }

    /// Maps `count` tiles of `r` from tile `first` onto `h` from tile `at`.
    fn map_tiles(gpu: &Gpu, r: &ID3D12Resource, h: &ID3D12Heap, first: u32, at: u32, count: u32) {
        let start = D3D12_TILED_RESOURCE_COORDINATE { X: first, ..Default::default() };
        let size = D3D12_TILE_REGION_SIZE { NumTiles: count, ..Default::default() };
        let flags = D3D12_TILE_RANGE_FLAG_NONE;
        // SAFETY: tiles inside the resource and the heap.
        unsafe {
            gpu.queue.UpdateTileMappings(
                r,
                1,
                Some(&start),
                Some(&size),
                h,
                1,
                Some(&flags),
                Some(&at),
                Some(&count),
                D3D12_TILE_MAPPING_FLAG_NONE,
            );
        }
    }

    /// Direct access vs every access through a page table, with the table
    /// in VRAM and in system memory; data in VRAM.
    fn indirection(c: &mut Ctx) -> R<()> {
        let src = unsafe { c.vram.GetGPUVirtualAddress() };
        let (dv, ds) = unsafe { (c.dir_vram.GetGPUVirtualAddress(), c.dir_sys.GetGPUVirtualAddress()) };
        println!("\n[indirection] data in VRAM; {:<22} {:>12} {:>12} {:>18}", "access", "stream GB/s", "random GB/s", "chase us/step");
        for (name, dir, suffix) in [("direct", dv, ""), ("direct, 16-byte elements", dv, "_sb"), ("table in VRAM", dv, "_dir"), ("table in system mem", ds, "_dir")] {
            let (s, r, ch) = c.patterns(src, dir, suffix)?;
            println!("[indirection] {:<35} {s:>12.1} {r:>12.2} {ch:>18.1}", name);
            c.note("indirection", name, "stream_gbs", s);
            c.note("indirection", name, "random_gbs", r);
            c.note("indirection", name, "chase_us_per_step", ch);
        }
        Ok(())
    }

    /// CPU writes a dirty set of 4 KiB pages into staging, then the GPU
    /// copies exactly those pages into VRAM: CPU ms, GPU ms, both totals.
    fn dirty_sets(c: &mut Ctx) -> R<()> {
        println!("\n[dirty sets] forwarding N scattered 4 KiB pages CPU -> upload -> VRAM");
        println!("[dirty sets] {:>8} {:>10} {:>14} {:>14} {:>14}", "pages", "MiB", "CPU write ms", "record ms", "GPU copy ms");
        let pages_total = (c.bytes / 4096) as usize;
        let up = c.upload.clone();
        let vram = c.vram.clone();
        let mut mapped: *mut c_void = std::ptr::null_mut();
        // SAFETY: the upload buffer stays mapped for this test.
        unsafe { up.Map(0, None, Some(&mut mapped)).map_err(e)? };
        let page_bytes = vec![0xA5u8; 4096];
        for n in [16usize, 256, 4096, 16384] {
            if n > pages_total {
                continue;
            }
            // A fixed scattered set: every (total/n)-th page.
            let step = pages_total / n;
            let set: Vec<usize> = (0..n).map(|i| i * step).collect();
            let (mut cpu, mut rec, mut gpu_ms) = (Vec::new(), Vec::new(), Vec::new());
            for _ in 0..=c.a.runs {
                let t = Instant::now();
                for &p in &set {
                    // SAFETY: inside the mapped upload buffer.
                    unsafe { std::ptr::copy_nonoverlapping(page_bytes.as_ptr(), mapped.cast::<u8>().add(p * 4096), 4096) };
                }
                cpu.push(t.elapsed().as_secs_f64() * 1000.0);
                let mut record_ms = 0.0;
                let g = c.gpu.timed(|l| {
                    let t = Instant::now();
                    for &p in &set {
                        let at = (p * 4096) as u64;
                        // SAFETY: both ranges inside their buffers.
                        unsafe { l.CopyBufferRegion(&vram, at, &up, at, 4096) };
                    }
                    record_ms = t.elapsed().as_secs_f64() * 1000.0;
                })?;
                rec.push(record_ms);
                gpu_ms.push(g);
            }
            let (cp, rc, gp) = (median(cpu[1..].to_vec()), median(rec[1..].to_vec()), median(gpu_ms[1..].to_vec()));
            println!("[dirty sets] {n:>8} {:>10.2} {cp:>14.3} {rc:>14.3} {gp:>14.3}", (n * 4096) as f64 / 1048576.0);
            let v = format!("{n}_pages");
            c.note("dirty_sets", &v, "cpu_write_ms", cp);
            c.note("dirty_sets", &v, "record_ms", rc);
            c.note("dirty_sets", &v, "gpu_copy_ms", gp);
        }
        // SAFETY: mapped above.
        unsafe { up.Unmap(0, None) };
        // Restore VRAM content for later tests.
        let (v2, u2) = (c.vram.clone(), c.upload.clone());
        fill(&u2, 0, &c.content)?;
        let b = c.bytes;
        // SAFETY: two live buffers of `b` bytes.
        c.gpu.timed(|l| unsafe { l.CopyBufferRegion(&v2, 0, &u2, 0, b) })?;
        Ok(())
    }

    /// D5 (ADR 0001): forwarding dirty 4 KiB units from the CPU view (a host
    /// copy of the data, standing in for title memory) through a packed
    /// staging ring (the upload buffer) into VRAM, with the copies planned by
    /// `sixth_memory::Planner`: one per unit, coalesced, coalesced with gaps
    /// filled. After every variant the whole VRAM buffer is read back and
    /// compared with the CPU view; any difference fails the suite.
    fn forwarding(c: &mut Ctx) -> R<()> {
        use sixth_memory::{Planner, UNIT};
        println!("\n[forward] dirty 4 KiB units: CPU view -> packed staging ring -> VRAM (D5)");
        println!(
            "[forward] {:>10} {:>6} {:>10} {:>8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>6}",
            "pattern", "units", "planner", "copies", "MiB", "pack ms", "rec ms", "GPU ms", "est ms", "check"
        );
        let total = c.bytes / UNIT;
        let up = c.upload.clone();
        let vram = c.vram.clone();
        let back = c.gpu.committed(heap(D3D12_HEAP_TYPE_READBACK), c.bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        // VRAM starts equal to the CPU view.
        let mut view = c.content.clone();
        fill(&up, 0, &view)?;
        let b = c.bytes;
        // SAFETY: two live buffers of `b` bytes.
        c.gpu.timed(|l| unsafe { l.CopyBufferRegion(&vram, 0, &up, 0, b) })?;
        let mut ring: *mut c_void = std::ptr::null_mut();
        // SAFETY: the upload buffer stays mapped for this test.
        unsafe { up.Map(0, None, Some(&mut ring)).map_err(e)? };
        let planners = [
            ("per-unit", Planner::per_unit()),
            ("coalesced", Planner::default()),
            ("filled", Planner::with_gap_filling()),
        ];
        let mut failures = 0;
        let mut tag = 0u8;
        for n in [256u64, 4096, 16384] {
            if n * 4 > total {
                continue;
            }
            let clusters = n / 8;
            let patterns: [(&str, Vec<u64>); 4] = [
                ("scattered", (0..n).map(|i| i * (total / n)).collect()),
                ("clustered", (0..n).map(|i| (i / 8) * (total / clusters) + i % 8).collect()),
                ("gappy", (0..n).map(|i| (i / 2) * (total / (n / 2)) + (i % 2) * 2).collect()),
                ("contiguous", (0..n).collect()),
            ];
            for (pattern, units) in &patterns {
                for (name, planner) in planners {
                    // The whole view is CPU-owned here, so any gap may be filled.
                    let plan = planner.plan(units, |_| true);
                    let (mut pack, mut rec, mut gpu_ms) = (Vec::new(), Vec::new(), Vec::new());
                    for _ in 0..=c.a.runs {
                        tag = tag.wrapping_add(1).max(1);
                        for &u in units {
                            let at = (u * UNIT) as usize;
                            view[at..at + UNIT as usize].fill(tag ^ (u as u8));
                        }
                        let t = Instant::now();
                        for r in &plan.runs {
                            // SAFETY: the plan's staging bytes fit the mapped ring (at most the whole buffer).
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    view.as_ptr().add(r.dst as usize),
                                    ring.cast::<u8>().add(r.staging as usize),
                                    r.len as usize,
                                )
                            };
                        }
                        pack.push(t.elapsed().as_secs_f64() * 1000.0);
                        let mut record_ms = 0.0;
                        let g = c.gpu.timed(|l| {
                            let t = Instant::now();
                            for r in &plan.runs {
                                // SAFETY: both ranges inside their buffers.
                                unsafe { l.CopyBufferRegion(&vram, r.dst, &up, r.staging, r.len) };
                            }
                            record_ms = t.elapsed().as_secs_f64() * 1000.0;
                        })?;
                        rec.push(record_ms);
                        gpu_ms.push(g);
                    }
                    // SAFETY: two live buffers of `b` bytes.
                    c.gpu.timed(|l| unsafe { l.CopyBufferRegion(&back, 0, &vram, 0, b) })?;
                    let got = read(&back, 0, b as usize)?;
                    let wrong = got.iter().zip(&view).filter(|(g, v)| g != v).count();
                    failures += usize::from(wrong != 0);
                    let (pk, rc, gp) = (median(pack[1..].to_vec()), median(rec[1..].to_vec()), median(gpu_ms[1..].to_vec()));
                    let mib = plan.bytes() as f64 / 1048576.0;
                    let est = plan.estimate_us / 1000.0;
                    let check = if wrong == 0 { "ok".to_string() } else { format!("{wrong} B") };
                    println!(
                        "[forward] {pattern:>10} {n:>6} {name:>10} {:>8} {mib:>9.2} {pk:>9.3} {rc:>9.3} {gp:>9.3} {est:>9.3} {check:>6}",
                        plan.runs.len()
                    );
                    let v = format!("{pattern}_{n}_{name}");
                    c.note("forward", &v, "copies", plan.runs.len() as f64);
                    c.note("forward", &v, "copied_mib", mib);
                    c.note("forward", &v, "pack_ms", pk);
                    c.note("forward", &v, "record_ms", rc);
                    c.note("forward", &v, "gpu_ms", gp);
                    c.note("forward", &v, "estimate_ms", est);
                    c.note("forward", &v, "wrong_bytes", wrong as f64);
                }
            }
        }
        // SAFETY: mapped above.
        unsafe { up.Unmap(0, None) };
        // Restore the upload and VRAM content for later tests.
        fill(&up, 0, &c.content)?;
        // SAFETY: two live buffers of `b` bytes.
        c.gpu.timed(|l| unsafe { l.CopyBufferRegion(&vram, 0, &up, 0, b) })?;
        if failures > 0 {
            return Err(format!("forward: {failures} variant(s) left VRAM different from the CPU view"));
        }
        Ok(())
    }

    /// Wall time from a CPU write to the CPU seeing a GPU read of it, with
    /// the value (a) in coherent system memory read directly and (b)
    /// forwarded to VRAM by a copy first. Checks the value too.
    fn round_trip(c: &mut Ctx) -> R<()> {
        println!("
[round trip] CPU writes a dword, GPU reads it, CPU reads the GPU's result (wall time, includes submit and fence wait)");
        let sys = c.gpu.committed(sys_wb(), 4096, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        let small_vram = c.gpu.committed(heap(D3D12_HEAP_TYPE_DEFAULT), 4096, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        let result = c.gpu.committed(
            heap(D3D12_HEAP_TYPE_DEFAULT),
            4096,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;
        let out_back = c.gpu.committed(heap(D3D12_HEAP_TYPE_READBACK), 16, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        let (sva, vva, rva) = unsafe { (sys.GetGPUVirtualAddress(), small_vram.GetGPUVirtualAddress(), result.GetGPUVirtualAddress()) };
        let dir = unsafe { c.dir_vram.GetGPUVirtualAddress() };
        let root = c.gpu.root.clone().unwrap();
        let probe = c.gpu.pso["probe"].clone();
        let up = c.upload.clone();
        let transition = |r: &ID3D12Resource, from: D3D12_RESOURCE_STATES, to: D3D12_RESOURCE_STATES| D3D12_RESOURCE_BARRIER {
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
        };
        let mut wrong = 0u32;
        for (name, via_vram) in [("coherent system memory", false), ("forwarded to VRAM", true)] {
            let mut t = Vec::new();
            for k in 0..=(c.a.runs * 4) as u32 {
                let value = 0x5100_0000 + k;
                let start = Instant::now();
                if via_vram {
                    fill(&up, 0, &value.to_le_bytes())?;
                } else {
                    fill(&sys, 0, &value.to_le_bytes())?;
                }
                let src = if via_vram { vva } else { sva };
                c.gpu.timed(|l| unsafe {
                    if via_vram {
                        l.ResourceBarrier(&[transition(&small_vram, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]);
                        l.CopyBufferRegion(&small_vram, 0, &up, 0, 4);
                        l.ResourceBarrier(&[transition(&small_vram, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE)]);
                    }
                    l.SetComputeRootSignature(&root);
                    l.SetPipelineState(&probe);
                    let cst = [0u32, 0, 0, 0];
                    l.SetComputeRoot32BitConstants(0, 4, cst.as_ptr().cast(), 0);
                    l.SetComputeRootShaderResourceView(1, src);
                    l.SetComputeRootUnorderedAccessView(2, rva);
                    l.SetComputeRootShaderResourceView(3, dir);
                    l.SetComputeRootShaderResourceView(4, src);
                    l.Dispatch(1, 1, 1);
                    l.ResourceBarrier(&[transition(&result, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                    l.CopyBufferRegion(&out_back, 0, &result, 0, 4);
                    l.ResourceBarrier(&[transition(&result, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
                    if via_vram {
                        l.ResourceBarrier(&[transition(&small_vram, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_COMMON)]);
                    }
                })?;
                let got = u32::from_le_bytes(read(&out_back, 0, 4)?.try_into().unwrap());
                t.push(start.elapsed().as_secs_f64() * 1e6);
                if got != value {
                    wrong += 1;
                }
            }
            let m = median(t[1..].to_vec());
            println!("[round trip] {name:<24} {m:>10.1} us");
            c.note("round_trip", name, "wall_us", m);
        }
        println!("[round trip] values wrong: {wrong} (must be 0)");
        c.note("round_trip", "all", "wrong_values", f64::from(wrong));
        if wrong != 0 {
            return Err(format!("round trip read {wrong} wrong values"));
        }
        Ok(())
    }

    /// Cost of UpdateTileMappings: remap N tiles of a reserved buffer to
    /// other heap tiles and back (CPU call time and queue time).
    fn remapping(c: &mut Ctx) -> R<()> {
        println!("\n[remapping] UpdateTileMappings of N contiguous 64 KiB tiles (CPU call + queue completion)");
        let bytes = c.bytes;
        let (r, h) = sparse_buffer(c, bytes)?;
        c.gpu.wait()?;
        let tiles = (bytes / PAGE) as u32;
        for n in [1u32, 64, 1024, tiles] {
            if n > tiles {
                continue;
            }
            let (mut call, mut total) = (Vec::new(), Vec::new());
            for k in 0..=c.a.runs {
                // Alternate between identity and a shifted mapping.
                let at = if k % 2 == 0 { tiles - n } else { 0 };
                let t = Instant::now();
                map_tiles(&c.gpu, &r, &h, 0, at, n);
                call.push(t.elapsed().as_secs_f64() * 1e3);
                c.gpu.wait()?;
                total.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let (cm, tm) = (median(call[1..].to_vec()), median(total[1..].to_vec()));
            println!("[remapping] {n:>6} tiles: call {cm:>8.3} ms, until done {tm:>8.3} ms");
            let v = format!("{n}_tiles");
            c.note("remapping", &v, "call_ms", cm);
            c.note("remapping", &v, "complete_ms", tm);
        }
        Ok(())
    }

    /// Two buffers placed at the same offset of one heap: a GPU write
    /// through one, after an aliasing barrier, is read through the other.
    fn aliasing(c: &mut Ctx) -> R<()> {
        let size = 1 << 20;
        let mut h: Option<ID3D12Heap> = None;
        // SAFETY: plain creation.
        unsafe {
            c.gpu
                .device
                .CreateHeap(
                    &D3D12_HEAP_DESC { SizeInBytes: size, Properties: heap(D3D12_HEAP_TYPE_DEFAULT), Alignment: 0, Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS },
                    &mut h,
                )
                .map_err(e)?;
        }
        let h = h.ok_or("no heap")?;
        let place = |gpu: &Gpu| -> R<ID3D12Resource> {
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: inside the heap.
            unsafe {
                gpu.device
                    .CreatePlacedResource(&h, 0, &buffer_desc(size, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS), D3D12_RESOURCE_STATE_COMMON, None, &mut r)
                    .map_err(e)?;
            }
            r.ok_or("no placed".into())
        };
        let (a, b) = (place(&c.gpu)?, place(&c.gpu)?);
        let back = c.gpu.committed(heap(D3D12_HEAP_TYPE_READBACK), 16, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        let root = c.gpu.root.clone().unwrap();
        let poke = c.gpu.pso["poke"].clone();
        let (ava, dir, src) = unsafe { (a.GetGPUVirtualAddress(), c.dir_vram.GetGPUVirtualAddress(), c.vram.GetGPUVirtualAddress()) };
        let value = 0xA11A_5ED0u32;
        c.gpu.timed(|l| unsafe {
            l.SetComputeRootSignature(&root);
            l.SetPipelineState(&poke);
            let cst = [0u32, value, 3, 0];
            l.SetComputeRoot32BitConstants(0, 4, cst.as_ptr().cast(), 0);
            l.SetComputeRootShaderResourceView(1, src);
            l.SetComputeRootUnorderedAccessView(2, ava);
            l.SetComputeRootShaderResourceView(3, dir);
            l.SetComputeRootShaderResourceView(4, src);
            l.Dispatch(1, 1, 1);
            let alias = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_ALIASING,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Aliasing: std::mem::ManuallyDrop::new(D3D12_RESOURCE_ALIASING_BARRIER {
                        pResourceBefore: std::mem::transmute_copy(&a),
                        pResourceAfter: std::mem::transmute_copy(&b),
                    }),
                },
                ..Default::default()
            };
            l.ResourceBarrier(&[alias]);
            l.CopyBufferRegion(&back, 0, &b, 12, 4);
        })?;
        let got = u32::from_le_bytes(read(&back, 0, 4)?.try_into().unwrap());
        let ok = got == value;
        println!("\n[aliasing] write through buffer A, read through buffer B at the same heap offset: {} (got {got:#x}, wanted {value:#x})", if ok { "OK" } else { "WRONG" });
        c.note("aliasing", "placed_same_offset", "correct", if ok { 1.0 } else { 0.0 });
        Ok(())
    }

    /// B-38: kernels from Sixth Star's own DXIL emitter, with the alignment
    /// operand of every data load set to 4, 8 or 16, vector vs scalar
    /// loads, direct vs page-table addressing. Every output is checked
    /// against the CPU reference.
    fn align_suite(c: &mut Ctx) -> R<()> {
        use victory_shader::emit::bench::{self, Access, Kernel, Pattern, Shape};
        // Root signature: SRV t0, UAV u1, SRV t2, SRV t3, all in space 1.
        let params = [
            (D3D12_ROOT_PARAMETER_TYPE_SRV, 0u32),
            (D3D12_ROOT_PARAMETER_TYPE_UAV, 1),
            (D3D12_ROOT_PARAMETER_TYPE_SRV, 2),
            (D3D12_ROOT_PARAMETER_TYPE_SRV, 3),
        ]
        .map(|(t, r)| D3D12_ROOT_PARAMETER {
            ParameterType: t,
            Anonymous: D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: r, RegisterSpace: 1 } },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        });
        let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 4, pParameters: params.as_ptr(), ..Default::default() };
        let mut blob: Option<windows::Win32::Graphics::Direct3D::ID3DBlob> = None;
        // SAFETY: a valid description; the blob outlives its use.
        let root: ID3D12RootSignature = unsafe {
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).map_err(e)?;
            let b = blob.ok_or("no root signature blob")?;
            let bytes = std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize());
            c.gpu.device.CreateRootSignature(0, bytes).map_err(e)?
        };
        let n16 = (c.bytes / 16) as u32;
        let up_params = |gpu: &Gpu, p: [u32; 4]| -> R<ID3D12Resource> {
            let b = gpu.committed(heap(D3D12_HEAP_TYPE_UPLOAD), 256, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ)?;
            let bytes: Vec<u8> = p.iter().flat_map(|x| x.to_le_bytes()).collect();
            fill(&b, 0, &bytes)?;
            Ok(b)
        };
        let back = c.gpu.committed(heap(D3D12_HEAP_TYPE_READBACK), u64::from(THREADS) * 4, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        // SAFETY: live buffers.
        let (src, out, dir) = unsafe { (c.vram.GetGPUVirtualAddress(), c.dst.GetGPUVirtualAddress(), c.dir_vram.GetGPUVirtualAddress()) };
        let dst = c.dst.clone();
        println!("\n[align] own DXIL emitter, data in VRAM: pattern / shape / access / align -> result, check");
        let mut wrong = 0;
        for (pattern, iters, total) in [(Pattern::Stream, 0u32, THREADS), (Pattern::Random, RANDOM_ITERS, THREADS), (Pattern::Chase, CHASE_ITERS, CHASE_THREADS)] {
            let expect = bench::reference(pattern, &c.content, iters, total);
            let pb = up_params(&c.gpu, [n16, iters, total, 0])?;
            // SAFETY: a live buffer.
            let pva = unsafe { pb.GetGPUVirtualAddress() };
            for access in [Access::Direct, Access::Table] {
                for shape in [Shape::Vector, Shape::Scalar] {
                    for align in [4u32, 8, 16] {
                        let dxil = bench::kernel(&Kernel { pattern, shape, access, align });
                        // SAFETY: a validated container and its root signature.
                        let pso: ID3D12PipelineState = unsafe {
                            c.gpu
                                .device
                                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                                    pRootSignature: std::mem::transmute_copy(&root),
                                    CS: D3D12_SHADER_BYTECODE { pShaderBytecode: dxil.as_ptr().cast(), BytecodeLength: dxil.len() },
                                    ..Default::default()
                                })
                                .map_err(e)?
                        };
                        let mut t = Vec::new();
                        for _ in 0..=c.a.runs {
                            t.push(c.gpu.timed(|l| unsafe {
                                l.SetComputeRootSignature(&root);
                                l.SetPipelineState(&pso);
                                l.SetComputeRootShaderResourceView(0, src);
                                l.SetComputeRootUnorderedAccessView(1, out);
                                l.SetComputeRootShaderResourceView(2, dir);
                                l.SetComputeRootShaderResourceView(3, pva);
                                l.Dispatch(total / 256, 1, 1);
                            })?);
                        }
                        let ms = median(t[1..].to_vec());
                        let n = u64::from(total) * 4;
                        let b2 = back.clone();
                        c.gpu.timed(|l| unsafe {
                            let tr = |from, to| D3D12_RESOURCE_BARRIER {
                                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                                    Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                                        pResource: std::mem::transmute_copy(&dst),
                                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                                        StateBefore: from,
                                        StateAfter: to,
                                    }),
                                },
                                ..Default::default()
                            };
                            l.ResourceBarrier(&[tr(D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                            l.CopyBufferRegion(&b2, 0, &dst, 0, n);
                            l.ResourceBarrier(&[tr(D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
                        })?;
                        let got: Vec<u32> = read(&back, 0, n as usize)?.as_chunks::<4>().0.iter().map(|x| u32::from_le_bytes(*x)).collect();
                        let ok = got == expect;
                        if !ok {
                            wrong += 1;
                        }
                        let (metric, unit) = match pattern {
                            Pattern::Stream => (c.bytes as f64 / (ms / 1000.0) / 1e9, "GB/s"),
                            Pattern::Random => ((u64::from(THREADS) * u64::from(RANDOM_ITERS) * 16) as f64 / (ms / 1000.0) / 1e9, "GB/s"),
                            Pattern::Chase => (ms * 1000.0 / f64::from(CHASE_ITERS), "us/step"),
                        };
                        let name = format!("{pattern:?} {shape:?} {access:?} {align}");
                        println!("[align] {name:<28} {metric:>9.2} {unit:<7} {}", if ok { "ok" } else { "WRONG" });
                        c.note("align", &name, unit, metric);
                    }
                }
            }
        }
        println!("[align] outputs differing from the CPU reference: {wrong} (must be 0)");
        c.note("align", "all", "wrong", f64::from(wrong));
        if wrong != 0 {
            return Err(format!("{wrong} kernels differ from the CPU reference"));
        }
        Ok(())
    }

    /// GPU upload heaps (VRAM the CPU writes directly), when the device
    /// reports them: CPU write bandwidth into them and GPU stream reads.
    fn upload_heap(c: &mut Ctx) -> R<()> {
        let mut o16 = D3D12_FEATURE_DATA_D3D12_OPTIONS16::default();
        // SAFETY: a plain feature query.
        let ok = unsafe {
            c.gpu
                .device
                .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS16, (&raw mut o16).cast(), size_of_val(&o16) as u32)
                .is_ok()
        };
        let supported = ok && o16.GPUUploadHeapSupported.as_bool();
        println!("\n[gpu upload heap] OPTIONS16 query {}, GPUUploadHeapSupported = {supported}", if ok { "ok" } else { "refused" });
        c.note("gpu_upload_heap", "capability", "supported", if supported { 1.0 } else { 0.0 });
        if !supported {
            println!("[gpu upload heap] not measured (needs ReBAR and a runtime/driver that exposes it)");
            return Ok(());
        }
        let bytes = c.bytes;
        let gu = c.gpu.committed(heap(D3D12_HEAP_TYPE_GPU_UPLOAD), bytes, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON)?;
        let t = Instant::now();
        fill(&gu, 0, &c.content)?;
        let w = t.elapsed().as_secs_f64();
        let va = unsafe { gu.GetGPUVirtualAddress() };
        let dir = unsafe { c.dir_vram.GetGPUVirtualAddress() };
        let (s, r, ch) = c.patterns(va, dir, "")?;
        println!("[gpu upload heap] CPU write {:.1} GB/s; GPU stream {s:.1} GB/s, random {r:.2} GB/s, chase {ch:.1} us/step", bytes as f64 / w / 1e9);
        c.note("gpu_upload_heap", "cpu_write", "gbs", bytes as f64 / w / 1e9);
        c.note("gpu_upload_heap", "gpu", "stream_gbs", s);
        Ok(())
    }
}
