// Gen 2 research: membench's workloads on Vulkan, three ways to reach the
// same bytes. Compiled to SPIR-V (dxc -spirv, Vulkan 1.2). Independently
// authored; no game data.

struct Push {
    uint64_t base;  // device address of the data (BDA modes)
    uint64_t table; // device address of the page table (table mode)
    uint n16;       // elements of 16 bytes
    uint iters;     // steps per thread (random, chase)
    uint total;     // threads in the dispatch
    uint pad;
};
[[vk::push_constant]] Push pc;

[[vk::binding(0, 0)]] ByteAddressBuffer src;   // descriptor mode
[[vk::binding(1, 0)]] RWByteAddressBuffer dst; // results, every mode

// Descriptor-bound storage buffer.
uint4 load4_desc(uint off) { return src.Load4(off); }
uint load1_desc(uint off) { return src.Load(off); }

// Buffer device address: a native 64-bit GPU pointer.
uint4 load4_bda(uint off) { return vk::RawBufferLoad<uint4>(pc.base + off, 16); }
uint load1_bda(uint off) { return vk::RawBufferLoad<uint>(pc.base + off, 4); }

// The same pointer, promising only 4-byte alignment (as a raw buffer
// load does): isolates the effect of the alignment from the API.
uint4 load4_bda4(uint off) { return vk::RawBufferLoad<uint4>(pc.base + off, 4); }
uint load1_bda4(uint off) { return vk::RawBufferLoad<uint>(pc.base + off, 4); }

// Page table of device addresses, one per 64 KiB page (as shadPS4's BDA
// page table, per sparse block): one lookup, then the native pointer.
uint64_t page_of(uint off) { return vk::RawBufferLoad<uint64_t>(pc.table + (off >> 16) * 8, 8); }
uint4 load4_tab(uint off) { return vk::RawBufferLoad<uint4>(page_of(off) + (off & 0xFFFF), 16); }
uint load1_tab(uint off) { return vk::RawBufferLoad<uint>(page_of(off) + (off & 0xFFFF), 4); }

#define KERNELS(M)                                                          \
[numthreads(256, 1, 1)] void stream_##M(uint id : SV_DispatchThreadID) {   \
    uint4 acc = 0;                                                          \
    for (uint i = id; i < pc.n16; i += pc.total) { acc += load4_##M(i * 16); } \
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);                        \
}                                                                           \
[numthreads(256, 1, 1)] void random_##M(uint id : SV_DispatchThreadID) {   \
    uint x = id * 2654435761u + 1u;                                         \
    uint4 acc = 0;                                                          \
    for (uint k = 0; k < pc.iters; k++) {                                   \
        x = x * 1664525u + 1013904223u;                                     \
        acc += load4_##M((x & (pc.n16 - 1)) * 16);                                \
    }                                                                       \
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);                        \
}                                                                           \
[numthreads(256, 1, 1)] void chase_##M(uint id : SV_DispatchThreadID) {    \
    uint p = (id * 7919u) % pc.n16;                                         \
    for (uint k = 0; k < pc.iters; k++) { p = load1_##M(p * 16); }          \
    dst.Store(id * 4, p);                                                   \
}

KERNELS(desc)
KERNELS(bda)
KERNELS(tab)
KERNELS(bda4)
