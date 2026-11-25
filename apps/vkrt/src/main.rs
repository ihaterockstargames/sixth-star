//! Gen 2 research: native ray tracing on Vulkan (ray query), with exactly
//! dxr-proto's synthetic scenes, rays and independent fp64 reference
//! (shared source: `apps/dxr-proto/src/common.rs`), so DXR and Vulkan
//! compare on identical work. GPU times from timestamps, median of `--runs`
//! after a warm-up. Independently authored; no game data.
//!
//! Usage: `vkrt [--runs N] [--rays N] [--csv path]`.

#[path = "../../dxr-proto/src/common.rs"]
mod common;

use std::ffi::CStr;
use std::path::PathBuf;

use ash::vk;
use common::{WorldScene, bytes, identity, median, placed, rays, soup, validate, world};

type R<T> = Result<T, String>;
fn e<E: std::fmt::Debug>(x: E) -> String {
    format!("{x:?}")
}

const HLSL: &str = include_str!("trace.hlsl");

fn dxc() -> PathBuf {
    std::env::var_os("SIXTH_DXC_SPIRV").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../tools/dxc-1.9.2609/bin/x64/dxc.exe")),
        PathBuf::from,
    )
}

fn compile() -> R<Vec<u32>> {
    let dir = std::env::temp_dir().join("sixth-vkrt");
    std::fs::create_dir_all(&dir).map_err(e)?;
    let (src, out) = (dir.join("trace.hlsl"), dir.join("trace.spv"));
    std::fs::write(&src, HLSL).map_err(e)?;
    let ok = std::process::Command::new(dxc())
        .args(["-nologo", "-spirv", "-T", "cs_6_5", "-O3", "-fspv-target-env=vulkan1.2", "-E", "trace", "-Fo"])
        .arg(&out)
        .arg(&src)
        .status()
        .map_err(e)?
        .success();
    if !ok {
        return Err("dxc failed".into());
    }
    let b = std::fs::read(&out).map_err(e)?;
    Ok(b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect())
}

struct Buf {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    address: u64,
    mapped: *mut u8,
}

struct Rt {
    instance: ash::Instance,
    device: ash::Device,
    asx: ash::khr::acceleration_structure::Device,
    queue: vk::Queue,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    queries: vk::QueryPool,
    period_ns: f64,
    mem: vk::PhysicalDeviceMemoryProperties,
    scratch_align: u64,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    dpool: vk::DescriptorPool,
}

/// One acceleration structure and its storage.
struct As {
    handle: vk::AccelerationStructureKHR,
    storage: Buf,
    address: u64,
    size: u64,
}

impl Rt {
    fn new() -> R<(Rt, String)> {
        // SAFETY: Vulkan calls with valid create infos; objects live as long
        // as `Rt` (the process ends after the run).
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
            let mut asp = vk::PhysicalDeviceAccelerationStructurePropertiesKHR::default();
            let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut asp);
            instance.get_physical_device_properties2(pd, &mut p2);
            let scratch_align = u64::from(asp.min_acceleration_structure_scratch_offset_alignment).max(256);
            let record = format!(
                "{name} (vendor {:#06x}, device {:#06x}, Vulkan {}.{}.{})",
                props.vendor_id,
                props.device_id,
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version)
            );
            let family = instance
                .get_physical_device_queue_family_properties(pd)
                .iter()
                .position(|q| q.queue_flags.contains(vk::QueueFlags::COMPUTE) && q.timestamp_valid_bits > 0)
                .ok_or("no compute queue")? as u32;
            let prio = [1.0f32];
            let qinfo = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let exts = [
                ash::khr::acceleration_structure::NAME.as_ptr(),
                ash::khr::ray_query::NAME.as_ptr(),
                ash::khr::deferred_host_operations::NAME.as_ptr(),
            ];
            let mut v12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true);
            let mut asf = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default().acceleration_structure(true);
            let mut rq = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);
            let dinfo = vk::DeviceCreateInfo::default()
                .queue_create_infos(&qinfo)
                .enabled_extension_names(&exts)
                .push_next(&mut v12)
                .push_next(&mut asf)
                .push_next(&mut rq);
            let device = instance.create_device(pd, &dinfo, None).map_err(e)?;
            let asx = ash::khr::acceleration_structure::Device::new(&instance, &device);
            let queue = device.get_device_queue(family, 0);
            let pool = device
                .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)
                .map_err(e)?;
            let cmd = device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1)).map_err(e)?[0];
            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(e)?;
            let queries = device.create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(2), None).map_err(e)?;
            let bindings = [
                vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(2).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(e)?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).size(16)];
            let sl = [set_layout];
            let layout = device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sl).push_constant_ranges(&ranges), None).map_err(e)?;
            let code = compile()?;
            let module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(e)?;
            let ename = std::ffi::CString::new("trace").unwrap();
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(module).name(&ename);
            let pipeline = device
                .create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None)
                .map_err(|(_, r)| e(r))?[0];
            let sizes = [
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).descriptor_count(64),
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(128),
            ];
            let dpool = device
                .create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(64).pool_sizes(&sizes).flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET), None)
                .map_err(e)?;
            let mem = instance.get_physical_device_memory_properties(pd);
            Ok((
                Rt {
                    instance,
                    device,
                    asx,
                    queue,
                    cmd,
                    fence,
                    queries,
                    period_ns: f64::from(props.limits.timestamp_period),
                    mem,
                    scratch_align,
                    set_layout,
                    layout,
                    pipeline,
                    dpool,
                },
                record,
            ))
        }
    }

    fn buffer(&self, size: u64, usage: vk::BufferUsageFlags, host: bool) -> R<Buf> {
        // SAFETY: plain creation.
        unsafe {
            let usage = usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC;
            let buffer = self.device.create_buffer(&vk::BufferCreateInfo::default().size(size.max(256)).usage(usage), None).map_err(e)?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let want = if host {
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
            } else {
                vk::MemoryPropertyFlags::DEVICE_LOCAL
            };
            let index = (0..self.mem.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0 && self.mem.memory_types[i as usize].property_flags.contains(want))
                .ok_or("no memory type")?;
            let mut flags = vk::MemoryAllocateFlagsInfo::default().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
            let memory = self
                .device
                .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(index).push_next(&mut flags), None)
                .map_err(e)?;
            self.device.bind_buffer_memory(buffer, memory, 0).map_err(e)?;
            let address = self.device.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer));
            let mapped = if host { self.device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()).map_err(e)?.cast() } else { std::ptr::null_mut() };
            Ok(Buf { buffer, memory, address, mapped })
        }
    }

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
            let c = [self.cmd];
            d.queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&c)], self.fence).map_err(e)?;
            d.wait_for_fences(&[self.fence], true, 120_000_000_000).map_err(e)?;
            d.reset_fences(&[self.fence]).map_err(e)?;
            let mut t = [0u64; 2];
            d.get_query_pool_results(self.queries, 0, &mut t, vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT).map_err(e)?;
            Ok(t[1].saturating_sub(t[0]) as f64 * self.period_ns / 1e6)
        }
    }

    /// A device-local buffer holding `data` (through a staging copy).
    fn resident(&self, data: &[u8], usage: vk::BufferUsageFlags) -> R<Buf> {
        let staging = self.buffer(data.len() as u64, vk::BufferUsageFlags::empty(), true)?;
        // SAFETY: mapped, at least `data.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), staging.mapped, data.len()) };
        let b = self.buffer(data.len() as u64, usage, false)?;
        let n = data.len() as u64;
        self.timed(|d, c| unsafe { d.cmd_copy_buffer(c, staging.buffer, b.buffer, &[vk::BufferCopy::default().size(n)]) })?;
        Ok(b)
    }

    /// Builds an acceleration structure from one geometry; the GPU ms.
    fn build(&self, ty: vk::AccelerationStructureTypeKHR, geometry: vk::AccelerationStructureGeometryKHR, count: u32, flags: vk::BuildAccelerationStructureFlagsKHR) -> R<(As, f64)> {
        let geoms = [geometry];
        let mut info = vk::AccelerationStructureBuildGeometryInfoKHR::default().ty(ty).flags(flags).mode(vk::BuildAccelerationStructureModeKHR::BUILD).geometries(&geoms);
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: valid build info and counts.
        unsafe {
            self.asx.get_acceleration_structure_build_sizes(vk::AccelerationStructureBuildTypeKHR::DEVICE, &info, &[count], &mut sizes);
        }
        let storage = self.buffer(sizes.acceleration_structure_size, vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR, false)?;
        let scratch = self.buffer(sizes.build_scratch_size + self.scratch_align, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
        // SAFETY: storage of the size asked for.
        let handle = unsafe {
            self.asx
                .create_acceleration_structure(&vk::AccelerationStructureCreateInfoKHR::default().buffer(storage.buffer).size(sizes.acceleration_structure_size).ty(ty), None)
                .map_err(e)?
        };
        let scratch_at = scratch.address.next_multiple_of(self.scratch_align);
        info = info.dst_acceleration_structure(handle).scratch_data(vk::DeviceOrHostAddressKHR { device_address: scratch_at });
        let range = [vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(count)];
        let ms = self.timed(|d, c| unsafe {
            self.asx.cmd_build_acceleration_structures(c, std::slice::from_ref(&info), &[&range]);
            let b = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR).dst_access_mask(vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR | vk::AccessFlags::SHADER_READ)];
            d.cmd_pipeline_barrier(
                c,
                vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR,
                vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &b,
                &[],
                &[],
            );
        })?;
        // SAFETY: a built structure.
        let address = unsafe { self.asx.get_acceleration_structure_device_address(&vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(handle)) };
        Ok((As { handle, storage, address, size: sizes.acceleration_structure_size }, ms))
    }

    fn blas(&self, v: &Buf, nv: u32, i: &Buf, ntri: u32, flags: vk::BuildAccelerationStructureFlagsKHR) -> R<(As, f64)> {
        let tri = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_data(vk::DeviceOrHostAddressConstKHR { device_address: v.address })
            .vertex_stride(12)
            .max_vertex(nv - 1)
            .index_type(vk::IndexType::UINT32)
            .index_data(vk::DeviceOrHostAddressConstKHR { device_address: i.address });
        let g = vk::AccelerationStructureGeometryKHR::default()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .geometry(vk::AccelerationStructureGeometryDataKHR { triangles: tri })
            .flags(vk::GeometryFlagsKHR::OPAQUE);
        self.build(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL, g, ntri, flags)
    }

    fn tlas(&self, instances: &Buf, n: u32) -> R<(As, f64)> {
        let data = vk::AccelerationStructureGeometryInstancesDataKHR::default().array_of_pointers(false).data(vk::DeviceOrHostAddressConstKHR { device_address: instances.address });
        let g = vk::AccelerationStructureGeometryKHR::default().geometry_type(vk::GeometryTypeKHR::INSTANCES).geometry(vk::AccelerationStructureGeometryDataKHR { instances: data });
        self.build(vk::AccelerationStructureTypeKHR::TOP_LEVEL, g, n, vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
    }

    /// Traces `n` rays from `rays` against `tlas` into `hits`; GPU ms.
    fn trace(&self, tlas: &As, rays: &Buf, hits: &Buf, n: u32, anyhit: bool, mask: u32) -> R<f64> {
        // SAFETY: a set from our pool written with live objects; freed after.
        unsafe {
            let sl = [self.set_layout];
            let set = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.dpool).set_layouts(&sl)).map_err(e)?[0];
            let structures = [tlas.handle];
            let mut asw = vk::WriteDescriptorSetAccelerationStructureKHR::default().acceleration_structures(&structures);
            let r = [vk::DescriptorBufferInfo::default().buffer(rays.buffer).range(vk::WHOLE_SIZE)];
            let h = [vk::DescriptorBufferInfo::default().buffer(hits.buffer).range(vk::WHOLE_SIZE)];
            let writes = [
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).descriptor_count(1).push_next(&mut asw),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&r),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(2).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&h),
            ];
            self.device.update_descriptor_sets(&writes, &[]);
            let push: Vec<u8> = [n, u32::from(anyhit), mask, 0].iter().flat_map(|x| x.to_le_bytes()).collect();
            let ms = self.timed(|d, c| {
                d.cmd_bind_pipeline(c, vk::PipelineBindPoint::COMPUTE, self.pipeline);
                d.cmd_bind_descriptor_sets(c, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[set], &[]);
                d.cmd_push_constants(c, self.layout, vk::ShaderStageFlags::COMPUTE, 0, &push);
                d.cmd_dispatch(c, n.div_ceil(64), 1, 1);
            })?;
            self.device.free_descriptor_sets(self.dpool, &[set]).map_err(e)?;
            Ok(ms)
        }
    }

    fn read_hits(&self, hits: &Buf, n: u32) -> R<Vec<[u32; 4]>> {
        let back = self.buffer(u64::from(n) * 16, vk::BufferUsageFlags::empty(), true)?;
        let size = u64::from(n) * 16;
        self.timed(|d, c| unsafe {
            let b = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::TRANSFER_READ)];
            d.cmd_pipeline_barrier(c, vk::PipelineStageFlags::COMPUTE_SHADER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &b, &[], &[]);
            d.cmd_copy_buffer(c, hits.buffer, back.buffer, &[vk::BufferCopy::default().size(size)]);
        })?;
        // SAFETY: mapped, coherent, `size` bytes after the wait.
        let raw = unsafe { std::slice::from_raw_parts(back.mapped, size as usize) };
        Ok(raw.as_chunks::<16>().0.iter().map(|c| std::array::from_fn(|k| u32::from_le_bytes(c[4 * k..4 * k + 4].try_into().unwrap()))).collect())
    }
}

fn instance(blas: u64, m: [f32; 12], id: u32, mask: u8) -> vk::AccelerationStructureInstanceKHR {
    vk::AccelerationStructureInstanceKHR {
        transform: vk::TransformMatrixKHR { matrix: m },
        instance_custom_index_and_mask: vk::Packed24_8::new(id, mask),
        instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(0, 0),
        acceleration_structure_reference: vk::AccelerationStructureReferenceKHR { device_handle: blas },
    }
}

fn main() {
    if let Err(x) = run() {
        eprintln!("vkrt: {x}");
        std::process::exit(1);
    }
}

fn run() -> R<()> {
    let (mut runs, mut nrays, mut csv) = (5usize, 1u32 << 20, None::<PathBuf>);
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--runs" => runs = v()?.parse().map_err(|_| "--runs: a number")?,
            "--rays" => nrays = v()?.parse().map_err(|_| "--rays: a number")?,
            "--csv" => csv = Some(v()?.into()),
            _ => return Err(format!("unknown argument {k}")),
        }
    }
    let (rt, adapter) = Rt::new()?;
    println!("adapter: {adapter}");
    println!("runs: {runs} (median after one warm-up), rays: {nrays}");
    let mut lines = vec![String::from("adapter,api,test,variant,metric,value")];
    let mut note = |test: &str, variant: &str, metric: &str, value: f64| {
        println!("  {test:<9} {variant:<40} {metric:<14} {value:>12.3}");
        lines.push(format!("\"{adapter}\",vulkan,{test},{variant},{metric},{value:.4}"));
    };
    let geom_usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR | vk::BufferUsageFlags::STORAGE_BUFFER;
    let inst_usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;

    // ---- Correctness: the same scenes, rays and reference as dxr-proto ----
    println!("\n[validate] Vulkan ray query vs fp64 brute-force reference");
    let mesh = soup(20_000, 100.0, 11);
    let vb = rt.resident(bytes(&mesh.v), geom_usage)?;
    let ib = rt.resident(bytes(&mesh.i), geom_usage)?;
    let (b1, _) = rt.blas(&vb, mesh.v.len() as u32, &ib, (mesh.i.len() / 3) as u32, vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)?;
    let blob = soup(3_000, 20.0, 12);
    let vb2 = rt.resident(bytes(&blob.v), geom_usage)?;
    let ib2 = rt.resident(bytes(&blob.i), geom_usage)?;
    let (b2, _) = rt.blas(&vb2, blob.v.len() as u32, &ib2, (blob.i.len() / 3) as u32, vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)?;
    let mut placements = Vec::new();
    for gy in 0..6 {
        for gx in 0..6 {
            let m = placed(0.37 * (gx * 6 + gy) as f32, [(gx as f32 - 2.5) * 40.0, (gy as f32 - 2.5) * 40.0, 0.0]);
            placements.push((m, if (gx + gy) % 2 == 0 { 1u8 } else { 2u8 }));
        }
    }
    let scenes: Vec<(&str, Vec<vk::AccelerationStructureInstanceKHR>, WorldScene)> = vec![
        ("v1 20k triangles", vec![instance(b1.address, identity(), 0, 0xFF)], world(&mesh, &[(identity(), 0xFF)])),
        (
            "v2 36 instances x 3k, masks",
            placements.iter().enumerate().map(|(i, (m, k))| instance(b2.address, *m, i as u32, *k)).collect(),
            world(&blob, &placements),
        ),
    ];
    let mut failed_total = 0;
    for (name, insts, w) in &scenes {
        // SAFETY: the instance records are plain data of the documented layout.
        let ib = rt.resident(unsafe { std::slice::from_raw_parts(insts.as_ptr().cast::<u8>(), std::mem::size_of_val(insts.as_slice())) }, inst_usage)?;
        let (tlas, _) = rt.tlas(&ib, insts.len() as u32)?;
        for (coherent, label) in [(true, "coherent"), (false, "incoherent")] {
            let rs = rays(2048, 100.0, coherent, 21 + u64::from(coherent));
            let rb = rt.resident(bytes(&rs), vk::BufferUsageFlags::STORAGE_BUFFER)?;
            let hits = rt.buffer(2048 * 16, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
            for (anyhit, mask) in [(false, 0xFFu8), (true, 0xFF), (false, 0x01)] {
                rt.trace(&tlas, &rb, &hits, 2048, anyhit, u32::from(mask))?;
                let h = rt.read_hits(&hits, 2048)?;
                let hit_count = h.iter().filter(|x| x[0] != 0).count();
                let (n, tol, fail, first) = validate(w, &rs, &h, anyhit, mask);
                failed_total += fail;
                let variant = format!("{name} {label} {} mask {mask:#x}", if anyhit { "any" } else { "closest" });
                println!(
                    "[validate] {variant:<62} rays {n}, GPU hits {hit_count}, tolerated {tol}, FAILED {fail}{}",
                    if first.is_empty() { String::new() } else { format!(" ({first})") }
                );
                note("validate", &variant, "failed", fail as f64);
            }
        }
    }
    if failed_total != 0 {
        return Err(format!("validation failed on {failed_total} rays"));
    }

    // ---- Performance: as dxr-proto (BLAS builds, 1M rays) ----
    println!("\n[perf] GPU ms (median), memory in MiB");
    for &tris in &[100_000usize, 1_000_000] {
        let m = soup(tris, 100.0, 31);
        let vb = rt.resident(bytes(&m.v), geom_usage)?;
        let ib = rt.resident(bytes(&m.i), geom_usage)?;
        let label = format!("{}k tris", tris / 1000);
        let mut fast = None;
        for (fl, name) in [(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE, "build fast-trace"), (vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD, "build fast-build")] {
            let mut t = Vec::new();
            let mut size = 0;
            for _ in 0..=runs {
                let (a, ms) = rt.blas(&vb, m.v.len() as u32, &ib, tris as u32, fl)?;
                size = a.size;
                t.push(ms);
                if fl == vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE {
                    fast = Some(a);
                }
            }
            note("blas", &format!("{label} {name}"), "gpu_ms", median(t[1..].to_vec()));
            note("blas", &format!("{label} {name}"), "size_mib", size as f64 / 1048576.0);
        }
        let blas = fast.unwrap();
        let insts = [instance(blas.address, identity(), 0, 0xFF)];
        // SAFETY: plain data.
        let ibuf = rt.resident(unsafe { std::slice::from_raw_parts(insts.as_ptr().cast::<u8>(), std::mem::size_of_val(&insts)) }, inst_usage)?;
        let (tlas, _) = rt.tlas(&ibuf, 1)?;
        for (coherent, cl) in [(true, "coherent"), (false, "incoherent")] {
            let rs = rays(nrays, 100.0, coherent, 41);
            let rb = rt.resident(bytes(&rs), vk::BufferUsageFlags::STORAGE_BUFFER)?;
            let hits = rt.buffer(u64::from(nrays) * 16, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
            for (anyhit, al) in [(false, "closest"), (true, "any")] {
                let mut t = Vec::new();
                for _ in 0..=runs {
                    t.push(rt.trace(&tlas, &rb, &hits, nrays, anyhit, 0xFF)?);
                }
                let ms = median(t[1..].to_vec());
                note("trace", &format!("{label} {cl} {al}"), "gpu_ms", ms);
                note("trace", &format!("{label} {cl} {al}"), "mrays_per_s", f64::from(nrays) / (ms / 1000.0) / 1e6);
            }
        }
    }
    {
        let blob = soup(10_000, 20.0, 51);
        let vb = rt.resident(bytes(&blob.v), geom_usage)?;
        let ib = rt.resident(bytes(&blob.i), geom_usage)?;
        let (blas, _) = rt.blas(&vb, blob.v.len() as u32, &ib, 10_000, vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)?;
        for side in [32u32, 100] {
            let n = side * side;
            let insts: Vec<_> = (0..n)
                .map(|i| {
                    let (x, y) = ((i % side) as f32, (i / side) as f32);
                    instance(blas.address, placed(0.1 * i as f32, [(x - side as f32 / 2.0) * 45.0, (y - side as f32 / 2.0) * 45.0, 0.0]), i, 0xFF)
                })
                .collect();
            // SAFETY: plain data.
            let ibuf = rt.resident(unsafe { std::slice::from_raw_parts(insts.as_ptr().cast::<u8>(), std::mem::size_of_val(insts.as_slice())) }, inst_usage)?;
            let mut t = Vec::new();
            let mut tl = None;
            for _ in 0..=runs {
                let (a, ms) = rt.tlas(&ibuf, n)?;
                t.push(ms);
                tl = Some(a);
            }
            let tl = tl.unwrap();
            let label = format!("{n} instances of 10k tris");
            note("tlas", &format!("{label} build"), "gpu_ms", median(t[1..].to_vec()));
            note("tlas", &format!("{label} build"), "size_mib", tl.size as f64 / 1048576.0);
            let rs = rays(nrays, (side as f32) * 22.0, false, 61);
            let rb = rt.resident(bytes(&rs), vk::BufferUsageFlags::STORAGE_BUFFER)?;
            let hits = rt.buffer(u64::from(nrays) * 16, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
            let mut t = Vec::new();
            for _ in 0..=runs {
                t.push(rt.trace(&tl, &rb, &hits, nrays, false, 0xFF)?);
            }
            let ms = median(t[1..].to_vec());
            note("trace", &format!("{label} incoherent closest"), "gpu_ms", ms);
            note("trace", &format!("{label} incoherent closest"), "mrays_per_s", f64::from(nrays) / (ms / 1000.0) / 1e6);
        }
    }
    if let Some(p) = csv {
        std::fs::write(&p, lines.join("\n") + "\n").map_err(e)?;
        println!("\ncsv: {}", p.display());
    }
    // Objects are released with the process (a short-lived benchmark).
    let _ = (&rt.instance, &rt.storage_note());
    Ok(())
}

impl Rt {
    /// Nothing to release explicitly: the process ends after the run.
    fn storage_note(&self) {}
}

impl Drop for As {
    fn drop(&mut self) {
        let _ = (&self.storage.memory, &self.storage.mapped);
    }
}
