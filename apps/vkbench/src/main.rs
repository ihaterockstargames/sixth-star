//! Gen 2 research: membench's workloads on Vulkan, on the same GPU, with
//! the same data, sizes and dispatch shapes, so D3D12 and Vulkan compare
//! 1:1. Three ways to reach the bytes:
//!
//! - `desc`: a descriptor-bound storage buffer;
//! - `bda`: a buffer device address (a native 64-bit GPU pointer);
//! - `tab`: a page table of device addresses (one per 64 KiB page), then
//!   the pointer, as shadPS4's device-address page table does.
//!
//! Data in device-local memory (VRAM) and in host-visible memory (system).
//! GPU times from timestamps, median of `--runs` after a warm-up. The
//! results are checked: every pattern's output must equal the same
//! pattern's output through the descriptor path.
//!
//! Usage: `vkbench [--mib N] [--runs N] [--csv path]`.

use std::ffi::CStr;
use std::path::PathBuf;

use ash::vk;

type R<T> = Result<T, String>;

fn e<E: std::fmt::Debug>(x: E) -> String {
    format!("{x:?}")
}

const HLSL: &str = include_str!("bench.hlsl");
const THREADS: u32 = 256 * 1024;
const CHASE_THREADS: u32 = 256 * 256;
const RANDOM_ITERS: u32 = 64;
const CHASE_ITERS: u32 = 256;
const PAGE: u64 = 1 << 16;

fn dxc() -> PathBuf {
    if let Some(p) = std::env::var_os("SIXTH_DXC_SPIRV") {
        return p.into();
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../tools/dxc-1.9.2609/bin/x64/dxc.exe"))
}

fn compile(entry: &str) -> R<Vec<u32>> {
    let dir = std::env::temp_dir().join("sixth-vkbench");
    std::fs::create_dir_all(&dir).map_err(e)?;
    let (src, out) = (dir.join("bench.hlsl"), dir.join(format!("{entry}.spv")));
    std::fs::write(&src, HLSL).map_err(e)?;
    let ok = std::process::Command::new(dxc())
        .args(["-nologo", "-spirv", "-T", "cs_6_6", "-O3", "-fspv-target-env=vulkan1.2", "-E", entry, "-Fo"])
        .arg(&out)
        .arg(&src)
        .status()
        .map_err(|x| format!("dxc ({}): {x}", dxc().display()))?
        .success();
    if !ok {
        return Err(format!("dxc failed for {entry}"));
    }
    let bytes = std::fs::read(&out).map_err(e)?;
    Ok(bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect())
}

/// The same chain data as membench: element i's first dword is the next
/// element of one random cycle (Sattolo, same seed and generator).
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

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

struct Buf {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    address: u64,
    size: u64,
    mapped: *mut u8,
}

struct Vk {
    _entry: ash::Entry,
    instance: ash::Instance,
    pd: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    queries: vk::QueryPool,
    period_ns: f64,
    mem: vk::PhysicalDeviceMemoryProperties,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    dpool: vk::DescriptorPool,
    stats: Option<ash::khr::pipeline_executable_properties::Device>,
}

impl Vk {
    fn new() -> R<(Vk, String)> {
        // SAFETY: Vulkan calls with valid create infos; objects live as long
        // as `Vk`.
        unsafe {
            let entry = ash::Entry::load().map_err(e)?;
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
            let instance = entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None).map_err(e)?;
            let pds = instance.enumerate_physical_devices().map_err(e)?;
            let pd = *pds
                .iter()
                .find(|&&p| instance.get_physical_device_properties(p).device_type == vk::PhysicalDeviceType::DISCRETE_GPU)
                .or(pds.first())
                .ok_or("no Vulkan device")?;
            let props = instance.get_physical_device_properties(pd);
            let name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy().into_owned();
            let record = format!(
                "{name} (vendor {:#06x}, device {:#06x}, Vulkan {}.{}.{}, driver {:#x})",
                props.vendor_id,
                props.device_id,
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version),
                props.driver_version
            );
            let family = instance
                .get_physical_device_queue_family_properties(pd)
                .iter()
                .position(|q| q.queue_flags.contains(vk::QueueFlags::COMPUTE) && q.timestamp_valid_bits > 0)
                .ok_or("no compute queue with timestamps")? as u32;
            let prio = [1.0f32];
            let qinfo = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let mut v12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true);
            let features = vk::PhysicalDeviceFeatures::default().shader_int64(true);
            // Driver statistics of compiled pipelines (registers, instructions),
            // where the driver has the extension.
            let has_stats = instance
                .enumerate_device_extension_properties(pd)
                .map_err(e)?
                .iter()
                .any(|x| CStr::from_ptr(x.extension_name.as_ptr()) == ash::khr::pipeline_executable_properties::NAME);
            let ext = [ash::khr::pipeline_executable_properties::NAME.as_ptr()];
            let mut pep = vk::PhysicalDevicePipelineExecutablePropertiesFeaturesKHR::default().pipeline_executable_info(true);
            let mut dinfo = vk::DeviceCreateInfo::default().queue_create_infos(&qinfo).enabled_features(&features).push_next(&mut v12);
            if has_stats {
                dinfo = dinfo.enabled_extension_names(&ext).push_next(&mut pep);
            }
            let device = instance.create_device(pd, &dinfo, None).map_err(e)?;
            let queue = device.get_device_queue(family, 0);
            let pool = device
                .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)
                .map_err(e)?;
            let cmd = device
                .allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1))
                .map_err(e)?[0];
            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(e)?;
            let queries = device
                .create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(2), None)
                .map_err(e)?;
            let bindings = [
                vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(e)?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).offset(0).size(32)];
            let layouts = [set_layout];
            let layout = device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&ranges), None)
                .map_err(e)?;
            let sizes = [vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(16)];
            let dpool = device.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(8).pool_sizes(&sizes), None).map_err(e)?;
            let mem = instance.get_physical_device_memory_properties(pd);
            let stats = has_stats.then(|| ash::khr::pipeline_executable_properties::Device::new(&instance, &device));
            Ok((
                Vk {
                    _entry: entry,
                    instance,
                    pd,
                    device,
                    queue,
                    pool,
                    cmd,
                    fence,
                    queries,
                    period_ns: f64::from(props.limits.timestamp_period),
                    stats,
                    mem,
                    set_layout,
                    layout,
                    dpool,
                },
                record,
            ))
        }
    }

    /// A memory type with all of `want` and none of `avoid`.
    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags, avoid: vk::MemoryPropertyFlags) -> R<u32> {
        (0..self.mem.memory_type_count)
            .find(|&i| {
                let f = self.mem.memory_types[i as usize].property_flags;
                bits & (1 << i) != 0 && f.contains(want) && !f.intersects(avoid)
            })
            .ok_or_else(|| format!("no memory type with {want:?} without {avoid:?}"))
    }

    fn buffer(&self, size: u64, want: vk::MemoryPropertyFlags, avoid: vk::MemoryPropertyFlags) -> R<Buf> {
        // SAFETY: plain creation; freed in `free`.
        unsafe {
            let usage = vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::TRANSFER_SRC;
            let buffer = self.device.create_buffer(&vk::BufferCreateInfo::default().size(size).usage(usage), None).map_err(e)?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let index = self.memory_type(req.memory_type_bits, want, avoid)?;
            let mut flags = vk::MemoryAllocateFlagsInfo::default().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
            let memory = self
                .device
                .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(index).push_next(&mut flags), None)
                .map_err(e)?;
            self.device.bind_buffer_memory(buffer, memory, 0).map_err(e)?;
            let address = self.device.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer));
            let mapped = if want.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                self.device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()).map_err(e)?.cast::<u8>()
            } else {
                std::ptr::null_mut()
            };
            Ok(Buf { buffer, memory, address, size, mapped })
        }
    }

    fn free(&self, b: Buf) {
        // SAFETY: our own objects, not in use (every submission is waited for).
        unsafe {
            if !b.mapped.is_null() {
                self.device.unmap_memory(b.memory);
            }
            self.device.destroy_buffer(b.buffer, None);
            self.device.free_memory(b.memory, None);
        }
    }

    /// Records `f` between two timestamps, submits, waits; GPU ms.
    fn timed(&self, f: impl FnOnce(&ash::Device, vk::CommandBuffer)) -> R<f64> {
        // SAFETY: our own command buffer, one submission at a time.
        unsafe {
            let d = &self.device;
            d.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty()).map_err(e)?;
            d.begin_command_buffer(self.cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(e)?;
            d.cmd_reset_query_pool(self.cmd, self.queries, 0, 2);
            d.cmd_write_timestamp(self.cmd, vk::PipelineStageFlags::TOP_OF_PIPE, self.queries, 0);
            f(d, self.cmd);
            d.cmd_write_timestamp(self.cmd, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.queries, 1);
            d.end_command_buffer(self.cmd).map_err(e)?;
            let cmds = [self.cmd];
            d.queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], self.fence).map_err(e)?;
            d.wait_for_fences(&[self.fence], true, 60_000_000_000).map_err(e)?;
            d.reset_fences(&[self.fence]).map_err(e)?;
            let mut t = [0u64; 2];
            d.get_query_pool_results(self.queries, 0, &mut t, vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT).map_err(e)?;
            Ok(t[1].saturating_sub(t[0]) as f64 * self.period_ns / 1e6)
        }
    }

    fn pipeline(&self, entry: &str) -> R<vk::Pipeline> {
        let code = compile(entry)?;
        // SAFETY: valid SPIR-V from dxc; the module is destroyed after use.
        unsafe {
            let module = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(e)?;
            let name = std::ffi::CString::new(entry).unwrap();
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(module).name(&name);
            let flags = if self.stats.is_some() { vk::PipelineCreateFlags::CAPTURE_STATISTICS_KHR } else { vk::PipelineCreateFlags::empty() };
            let info = [vk::ComputePipelineCreateInfo::default().flags(flags).stage(stage).layout(self.layout)];
            let p = self.device.create_compute_pipelines(vk::PipelineCache::null(), &info, None).map_err(|(_, r)| e(r))?[0];
            self.device.destroy_shader_module(module, None);
            Ok(p)
        }
    }

    /// The driver's statistics of `pipeline` (name = value), if any.
    fn statistics(&self, pipeline: vk::Pipeline) -> Vec<(String, String)> {
        let Some(st) = &self.stats else { return Vec::new() };
        let mut out = Vec::new();
        // SAFETY: a pipeline created with CAPTURE_STATISTICS.
        unsafe {
            let Ok(execs) = st.get_pipeline_executable_properties(&vk::PipelineInfoKHR::default().pipeline(pipeline)) else { return out };
            for i in 0..execs.len() as u32 {
                let info = vk::PipelineExecutableInfoKHR::default().pipeline(pipeline).executable_index(i);
                let Ok(list) = st.get_pipeline_executable_statistics(&info) else { continue };
                for v in list {
                    let name = CStr::from_ptr(v.name.as_ptr()).to_string_lossy().into_owned();
                    let val = match v.format {
                        vk::PipelineExecutableStatisticFormatKHR::BOOL32 => format!("{}", v.value.b32),
                        vk::PipelineExecutableStatisticFormatKHR::INT64 => format!("{}", v.value.i64),
                        vk::PipelineExecutableStatisticFormatKHR::UINT64 => format!("{}", v.value.u64),
                        _ => format!("{:.2}", v.value.f64),
                    };
                    out.push((name, val));
                }
            }
        }
        out
    }

    fn descriptor_set(&self, src: &Buf, dst: &Buf) -> R<vk::DescriptorSet> {
        // SAFETY: a set from our pool, written with live buffers.
        unsafe {
            let layouts = [self.set_layout];
            let set = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.dpool).set_layouts(&layouts)).map_err(e)?[0];
            let i0 = [vk::DescriptorBufferInfo::default().buffer(src.buffer).offset(0).range(vk::WHOLE_SIZE)];
            let i1 = [vk::DescriptorBufferInfo::default().buffer(dst.buffer).offset(0).range(vk::WHOLE_SIZE)];
            let writes = [
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&i0),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&i1),
            ];
            self.device.update_descriptor_sets(&writes, &[]);
            Ok(set)
        }
    }

    /// Copies `data` into a device-local buffer through a staging buffer.
    fn upload(&self, dst: &Buf, data: &[u8]) -> R<()> {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let staging = self.buffer(data.len() as u64, host, vk::MemoryPropertyFlags::empty())?;
        // SAFETY: mapped, `data.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), staging.mapped, data.len()) };
        let n = data.len() as u64;
        self.timed(|d, c| unsafe {
            d.cmd_copy_buffer(c, staging.buffer, dst.buffer, &[vk::BufferCopy::default().size(n)]);
        })?;
        self.free(staging);
        Ok(())
    }
}

fn main() {
    if let Err(x) = run() {
        eprintln!("vkbench: {x}");
        std::process::exit(1);
    }
}

fn run() -> R<()> {
    let (mut mib, mut runs, mut csv) = (256u64, 5usize, None::<PathBuf>);
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--mib" => mib = v()?.parse().map_err(|_| "--mib: a number")?,
            "--runs" => runs = v()?.parse().map_err(|_| "--runs: a number")?,
            "--csv" => csv = Some(v()?.into()),
            _ => return Err(format!("unknown argument {k}")),
        }
    }
    let bytes = mib << 20;
    let n16 = (bytes / 16) as u32;
    let (vk, adapter) = Vk::new()?;
    println!("adapter: {adapter}");
    println!("buffer: {mib} MiB, runs: {runs} (median after one warm-up)");
    let content = data(bytes);
    let device_local = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;

    let vram = vk.buffer(bytes, device_local, vk::MemoryPropertyFlags::HOST_VISIBLE)?;
    vk.upload(&vram, &content)?;
    let sys = vk.buffer(bytes, host, device_local)?;
    // SAFETY: mapped, `bytes` long.
    unsafe { std::ptr::copy_nonoverlapping(content.as_ptr(), sys.mapped, content.len()) };
    let dst = vk.buffer(u64::from(THREADS) * 4, device_local, vk::MemoryPropertyFlags::empty())?;
    let readback = vk.buffer(u64::from(THREADS) * 4, host, vk::MemoryPropertyFlags::empty())?;
    // Page tables of device addresses for each backing (identity layout).
    let table_for = |b: &Buf| -> R<Buf> {
        let mut t = Vec::new();
        for p in 0..b.size / PAGE {
            t.extend((b.address + p * PAGE).to_le_bytes());
        }
        let tb = vk.buffer(t.len() as u64, device_local, vk::MemoryPropertyFlags::HOST_VISIBLE)?;
        vk.upload(&tb, &t)?;
        Ok(tb)
    };
    let (tab_vram, tab_sys) = (table_for(&vram)?, table_for(&sys)?);

    let modes = ["desc", "bda", "tab", "bda4"];
    let patterns = [("stream", 0u32, THREADS), ("random", RANDOM_ITERS, THREADS), ("chase", CHASE_ITERS, CHASE_THREADS)];
    let mut pipes = std::collections::HashMap::new();
    for (p, _, _) in patterns {
        for m in modes {
            let name = format!("{p}_{m}");
            pipes.insert(name.clone(), vk.pipeline(&name)?);
        }
    }

    let mut lines = vec![String::from("adapter,api,backing,mode,pattern,metric,value")];
    if vk.stats.is_none() {
        println!("driver statistics: VK_KHR_pipeline_executable_properties not available");
    } else {
        println!("\ndriver statistics per pipeline (VK_KHR_pipeline_executable_properties):");
        let mut names: Vec<_> = pipes.keys().cloned().collect();
        names.sort();
        for n in names {
            let st = vk.statistics(pipes[&n]);
            let text: Vec<String> = st.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("  {n:<14} {}", text.join(", "));
            for (k, v) in st {
                lines.push(format!("\"{adapter}\",vulkan,-,{n},stats,{},{}", k.replace(',', ";"), v));
            }
        }
    }
    let mut mismatches = 0;
    println!("\n{:<8} {:<6} {:>12} {:>12} {:>16}", "backing", "mode", "stream GB/s", "random GB/s", "chase us/step");
    for (bname, src, tab) in [("vram", &vram, &tab_vram), ("sys", &sys, &tab_sys)] {
        let set = vk.descriptor_set(src, &dst)?;
        let mut reference: std::collections::HashMap<&str, Vec<u8>> = Default::default();
        let mut row = Vec::new();
        for m in modes {
            let mut vals = Vec::new();
            for (p, iters, total) in patterns {
                let pipe = pipes[&format!("{p}_{m}")];
                let mut push = Vec::with_capacity(32);
                push.extend(src.address.to_le_bytes());
                push.extend(tab.address.to_le_bytes());
                for x in [n16, iters, total, 0] {
                    push.extend(x.to_le_bytes());
                }
                let mut t = Vec::new();
                for _ in 0..=runs {
                    t.push(vk.timed(|d, c| unsafe {
                        d.cmd_bind_pipeline(c, vk::PipelineBindPoint::COMPUTE, pipe);
                        d.cmd_bind_descriptor_sets(c, vk::PipelineBindPoint::COMPUTE, vk.layout, 0, &[set], &[]);
                        d.cmd_push_constants(c, vk.layout, vk::ShaderStageFlags::COMPUTE, 0, &push);
                        d.cmd_dispatch(c, total / 256, 1, 1);
                    })?);
                }
                let ms = median(t[1..].to_vec());
                // The output must equal the descriptor path's for this pattern.
                let n = u64::from(total) * 4;
                vk.timed(|d, c| unsafe {
                    let b = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::TRANSFER_READ)];
                    d.cmd_pipeline_barrier(c, vk::PipelineStageFlags::COMPUTE_SHADER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &b, &[], &[]);
                    d.cmd_copy_buffer(c, dst.buffer, readback.buffer, &[vk::BufferCopy::default().size(n)]);
                })?;
                // SAFETY: mapped, coherent, `n` bytes after the wait.
                let out = unsafe { std::slice::from_raw_parts(readback.mapped, n as usize).to_vec() };
                match reference.get(p) {
                    None => {
                        reference.insert(p, out);
                    }
                    Some(r) if *r != out => mismatches += 1,
                    Some(_) => {}
                }
                let metric = match p {
                    "stream" => bytes as f64 / (ms / 1000.0) / 1e9,
                    "random" => (u64::from(THREADS) * u64::from(RANDOM_ITERS) * 16) as f64 / (ms / 1000.0) / 1e9,
                    _ => ms * 1000.0 / f64::from(CHASE_ITERS),
                };
                let unit = if p == "chase" { "us_per_step" } else { "gbs" };
                lines.push(format!("\"{adapter}\",vulkan,{bname},{m},{p},{unit},{metric:.4}"));
                vals.push(metric);
            }
            row.push((m, vals));
        }
        for (m, v) in row {
            println!("{bname:<8} {m:<6} {:>12.1} {:>12.2} {:>16.1}", v[0], v[1], v[2]);
        }
    }
    println!("\noutputs differing from the descriptor path: {mismatches} (must be 0)");
    lines.push(format!("\"{adapter}\",vulkan,all,all,all,mismatches,{mismatches}"));
    if let Some(p) = csv {
        std::fs::write(&p, lines.join("\n") + "\n").map_err(e)?;
        println!("csv: {}", p.display());
    }
    for b in [vram, sys, dst, readback, tab_vram, tab_sys] {
        vk.free(b);
    }
    // SAFETY: everything submitted has finished.
    unsafe {
        for (_, p) in pipes {
            vk.device.destroy_pipeline(p, None);
        }
        vk.device.destroy_descriptor_pool(vk.dpool, None);
        vk.device.destroy_pipeline_layout(vk.layout, None);
        vk.device.destroy_descriptor_set_layout(vk.set_layout, None);
        vk.device.destroy_query_pool(vk.queries, None);
        vk.device.destroy_fence(vk.fence, None);
        vk.device.destroy_command_pool(vk.pool, None);
        vk.device.destroy_device(None);
        vk.instance.destroy_instance(None);
    }
    let _ = vk.pd;
    if mismatches != 0 {
        return Err(format!("{mismatches} outputs differ from the descriptor path"));
    }
    Ok(())
}
