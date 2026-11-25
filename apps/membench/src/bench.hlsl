// B-30 synthetic workloads. Independently authored; no game data.
// src: the buffer under test (raw). dst: results in VRAM (raw).
#define RS "RootConstants(num32BitConstants=4, b0), SRV(t0), UAV(u0), SRV(t1), SRV(t0, space=1)"

cbuffer C : register(b0) {
    uint n16;   // elements of 16 bytes in src
    uint iters; // steps per thread (random, chase)
    uint total; // threads in the dispatch
    uint pad;
};
ByteAddressBuffer src : register(t0);
RWByteAddressBuffer dst : register(u0);
// A page table like the raw-address directory: per 64 KiB page, two
// dwords (region, byte offset of the page in src). Identity here: the cost
// measured is the lookup, not a remap.
ByteAddressBuffer dir : register(t1);
// The source viewed as 16-byte elements: element loads are 16-byte aligned.
StructuredBuffer<uint4> src16 : register(t0, space1);

uint through_dir(uint addr) {
    uint2 e = dir.Load2((addr >> 16) * 8);
    return e.y + (addr & 0xFFFF);
}

// Streaming read: every 16-byte element once, coalesced.
[RootSignature(RS)]
[numthreads(256, 1, 1)]
void stream(uint id : SV_DispatchThreadID) {
    uint4 acc = 0;
    for (uint i = id; i < n16; i += total) {
        acc += src.Load4(i * 16);
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

// Random 16-byte reads (independent, so latency can overlap).
[RootSignature(RS)]
[numthreads(256, 1, 1)]
void random(uint id : SV_DispatchThreadID) {
    uint x = id * 2654435761u + 1u;
    uint4 acc = 0;
    for (uint k = 0; k < iters; k++) {
        x = x * 1664525u + 1013904223u;
        acc += src.Load4((x & (n16 - 1)) * 16);
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

// Dependent reads: each element's first dword is the next element
// (a single random cycle), as a BVH traversal chases node pointers.
[RootSignature(RS)]
[numthreads(256, 1, 1)]
void chase(uint id : SV_DispatchThreadID) {
    uint p = (id * 7919u) % n16;
    for (uint k = 0; k < iters; k++) {
        p = src.Load(p * 16);
    }
    dst.Store(id * 4, p);
}

// The same three patterns, every access through the page table.
[RootSignature(RS)]
[numthreads(256, 1, 1)]
void stream_dir(uint id : SV_DispatchThreadID) {
    uint4 acc = 0;
    for (uint i = id; i < n16; i += total) {
        acc += src.Load4(through_dir(i * 16));
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void random_dir(uint id : SV_DispatchThreadID) {
    uint x = id * 2654435761u + 1u;
    uint4 acc = 0;
    for (uint k = 0; k < iters; k++) {
        x = x * 1664525u + 1013904223u;
        acc += src.Load4(through_dir((x & (n16 - 1)) * 16));
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void chase_dir(uint id : SV_DispatchThreadID) {
    uint p = (id * 7919u) % n16;
    for (uint k = 0; k < iters; k++) {
        p = src.Load(through_dir(p * 16));
    }
    dst.Store(id * 4, p);
}

// Reads one dword (round trip: what the CPU just wrote).
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void probe(uint id : SV_DispatchThreadID) {
    dst.Store(0, src.Load(iters * 4));
}

// Writes `iters` at dword `total` of dst (aliasing test).
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void poke(uint id : SV_DispatchThreadID) {
    dst.Store(total * 4, iters);
}

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void stream_sb(uint id : SV_DispatchThreadID) {
    uint4 acc = 0;
    for (uint i = id; i < n16; i += total) {
        acc += src16[i];
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void random_sb(uint id : SV_DispatchThreadID) {
    uint x = id * 2654435761u + 1u;
    uint4 acc = 0;
    for (uint k = 0; k < iters; k++) {
        x = x * 1664525u + 1013904223u;
        acc += src16[x & (n16 - 1)];
    }
    dst.Store(id * 4, acc.x ^ acc.y ^ acc.z ^ acc.w);
}

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void chase_sb(uint id : SV_DispatchThreadID) {
    uint p = (id * 7919u) % n16;
    for (uint k = 0; k < iters; k++) {
        p = src16[p].x;
    }
    dst.Store(id * 4, p);
}
