// Gen 2: inline ray tracing on Vulkan (VK_KHR_ray_query), the same work as
// dxr-proto's trace.hlsl. Compiled with dxc -spirv. Independently authored.

struct Push {
    uint count;  // rays
    uint anyhit; // 1: first hit ends the search
    uint mask;   // instance mask
    uint pad;
};
[[vk::push_constant]] Push pc;

[[vk::binding(0, 0)]] RaytracingAccelerationStructure scene;
// Per ray two float4: origin.xyz + tmax, direction.xyz + unused.
[[vk::binding(1, 0)]] StructuredBuffer<float4> rays;
// Per ray: primitive + 1 (0: miss), instance index, t (bits), u (bits).
[[vk::binding(2, 0)]] RWStructuredBuffer<uint4> hits;

[numthreads(64, 1, 1)]
void trace(uint id : SV_DispatchThreadID) {
    if (id >= pc.count) {
        return;
    }
    float4 o = rays[id * 2];
    float4 d = rays[id * 2 + 1];
    RayDesc r;
    r.Origin = o.xyz;
    r.TMin = 0.0;
    r.Direction = d.xyz;
    r.TMax = o.w;
    uint4 result = uint4(0, 0, 0, 0);
    if (pc.anyhit != 0) {
        RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> q;
        q.TraceRayInline(scene, 0, pc.mask, r);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            result = uint4(q.CommittedPrimitiveIndex() + 1, q.CommittedInstanceIndex(), asuint(q.CommittedRayT()), 0);
        }
    } else {
        RayQuery<RAY_FLAG_FORCE_OPAQUE> q;
        q.TraceRayInline(scene, 0, pc.mask, r);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            float2 b = q.CommittedTriangleBarycentrics();
            result = uint4(q.CommittedPrimitiveIndex() + 1, q.CommittedInstanceIndex(), asuint(q.CommittedRayT()), asuint(b.x));
        }
    }
    hits[id] = result;
}
