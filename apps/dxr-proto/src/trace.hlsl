// Gen 2 Phase 4: inline ray tracing (DXR 1.1 RayQuery). Independently
// authored; no game data.
#define RS "RootConstants(num32BitConstants=4, b0), SRV(t0), SRV(t1), UAV(u0)"

cbuffer C : register(b0) {
    uint count;  // rays
    uint anyhit; // 1: first hit ends the search (occlusion)
    uint mask;   // instance mask
    uint pad;
};
RaytracingAccelerationStructure scene : register(t0);
// Per ray two float4: origin.xyz + tmax, direction.xyz + unused.
StructuredBuffer<float4> rays : register(t1);
// Per ray: primitive + 1 (0: miss), instance index, t (bits), u (bits).
RWStructuredBuffer<uint4> hits : register(u0);

[RootSignature(RS)]
[numthreads(64, 1, 1)]
void trace(uint id : SV_DispatchThreadID) {
    if (id >= count) {
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
    if (anyhit != 0) {
        RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> q;
        q.TraceRayInline(scene, 0, mask, r);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            result = uint4(q.CommittedPrimitiveIndex() + 1, q.CommittedInstanceIndex(), asuint(q.CommittedRayT()), 0);
        }
    } else {
        RayQuery<RAY_FLAG_FORCE_OPAQUE> q;
        q.TraceRayInline(scene, 0, mask, r);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            float2 b = q.CommittedTriangleBarycentrics();
            result = uint4(q.CommittedPrimitiveIndex() + 1, q.CommittedInstanceIndex(), asuint(q.CommittedRayT()), asuint(b.x));
        }
    }
    hits[id] = result;
}
