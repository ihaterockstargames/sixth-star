# ADR 0001 — Sixth Star Gen 2: architecture reset, evidence so far

Status: **proposed** (2026-11-25). Evidence-backed where marked *measured*; everything else is a hypothesis with a named experiment.

## Context

Gen 1 is the VIctory/KLBF memory model: per-buffer host copies in system memory, write-watch and guard detection, CPU compare-and-merge of GPU writes, synchronous submission, and a raw-address directory filled on shader faults. Measured costs of that model and its alternatives are in `CODEX_HANDOFF.md` Parts 4–6. The objective is 60+ rendered FPS with correct hardware ray tracing, lighting, physics, streaming and memory semantics, so Gen 2 does not assume the inherited GPU architecture is suitable.

## Measurements this ADR rests on

RTX 3070, driver 32.0.16.1088, `apps/membench`, median of 5 after a warm-up, 256 MiB buffers.

| Question | Result |
|---|---|
| GPU read speed by backing | VRAM 428–432 GB/s stream, 34 GB/s random 16 B, 13.2 µs per chase step (65,536 threads). **Every system-memory backing** (custom write-back, upload, process memory opened as a heap, SEC_COMMIT section): 23–26 GB/s, 1.25–1.30 GB/s, 244–246 µs/step |
| Sparse VRAM (reserved buffer on DEFAULT-heap tiles) | 429–432 GB/s, 31.9 GB/s random, 13.1 µs/step: ≈ plain VRAM |
| Page-table indirection (data in VRAM, 32 KB table) | stream −3…4%, random −17%, chase unchanged. Table in VRAM vs system memory: **no difference at this table size** (it stays in GPU cache) |
| Forwarding scattered 4 KiB pages (CPU write / record / GPU copy) | 16 pages: ≈0.01 ms total. 256 pages: 0.04 / 0.08 / 0.08 ms. 4,096 pages (16 MiB): 0.72 / 1.1–1.2 / 1.5 ms. 16,384 pages (64 MiB): 2.9–3.1 / 4.7 / 4.5–4.9 ms. **The cost is per copy command (≈0.3 µs record + ≈0.3 µs GPU per page)**, not bandwidth |
| CPU write → GPU read → CPU sees the result | ≈100–112 µs wall for coherent system memory and forwarded-to-VRAM alike (submit + fence dominate). Values correct (0 wrong of 42) |
| `UpdateTileMappings` | 1 tile: 0.1 ms to completion. 1,024 tiles: 0.2 ms. 4,096 tiles (256 MiB): 0.44 ms call, 0.64 ms to completion |
| Aliasing (two placed buffers at one heap offset, aliasing barrier) | correct |
| GPU upload heaps | **not available** on this machine/runtime (`GPUUploadHeapSupported = false`) |
| Inherited test stability | `victory-gpu` unit tests crash (access violation) in 5/12 parallel runs **in unmodified Lab**; 0/5 single-threaded |

Raw CSV: `membench-rtx3070.csv`, `membench-gen2-rtx3070.csv` (kept outside the repository).

## Decisions

### D1 — Physical residency: VRAM for GPU-hot pages, by default
*Measured:* system memory in every form tried is 17–27× slower for GPU reads and 19× slower for dependent chases. Sparse VRAM costs about nothing over plain VRAM.

**Decision:** GPU-hot memory (acceleration structures, geometry, large GPU heaps, GPU intermediates) lives in sparse VRAM regions. Coherent system memory is only for memory whose **access contract** says the CPU reads it often, or both sides write it. Pure "title memory opened in place" is **rejected as the default**: it measured as slow as any system memory.

### D2 — Separate five concerns
Virtual addressing (title address → page-table entry), physical residency (VRAM tile / system page / none), access contract (who may read and write, declared or proven), ownership (the current authority for the bytes), and observed usage (telemetry) are separate data in `sixth-memory`. No field doubles for another.

### D3 — No ownership from a few frames of observation
"N frames without a CPU dirty bit ⇒ GPU-owned" is **rejected**. Ownership comes from one of three sources:
- **a declared contract**: API semantics (upload/readback/default heap type), console memory-type bits, DirectStorage destinations, explicit copies;
- **a proven contract**: static or dynamic analysis of an authorized or independently authored test binary;
- **the conservative default `Mixed`**.

Observed usage only **proposes** a contract change, which is logged and reviewable. It never silently applies one.

### D4 — No blind GPU-owned → Mixed overwrite
Part 3 §3.2's "read VRAM back, then apply the CPU's dirty 4 KiB pages on top" is **rejected**. A dirty page bit does not say which bytes the CPU wrote, so this can overwrite GPU-written bytes on the same page.

On a late CPU write to a GPU-owned page:
1. Stop, fence, and read the whole page back.
2. Where a last-forwarded snapshot exists, diff the CPU view against it at byte granularity to find the CPU's actual writes.
3. Without a snapshot, the page becomes `Mixed` with the **GPU's bytes as authority**, and the CPU write is logged as a contract violation for investigation.

Correctness comes before speed: `Mixed` pages move to the coherent backing.

### D5 — Forwarding is coalesced and budgeted
*Measured:* per-page copy commands cost ≈0.6 µs each (record + GPU).

**Decision:** forwarding coalesces adjacent dirty pages into runs, uses one staging ring per frame, and has a budget of ≤ 1 ms per frame. That is roughly a few thousand scattered pages, or tens of MB in runs. A region that needs more is a contract problem (it should not be V-class), not a tuning problem.

### D6 — Three shader memory-access tiers
*Measured:* a page-table lookup costs 3–4% of streaming and 17% of random-read throughput. With a small hot table, its location did not matter.

**Decision:** the shader memory model offers three tiers:
1. **direct** resource access, when the translator can prove the target is one resource;
2. **region-relative** access (base + bounded offset), when it can prove one region;
3. the **page table**, for general pointers.

Every access uses the cheapest tier that is provably correct. The 16 MiB Lab directory, with its sparse use pattern, must be measured separately (B-33) before it is moved.

### D7 — Asynchronous submission is a new component, not a flag
The inherited late-wait mode is coupled to copy-and-merge and produced device removals.

**Decision:** `sixth-scheduler` owns timelines, hazards (RAW/WAR/WAW per region range and per resource), barriers, retirement vectors and guest-visible completion (Part 3 §3.3–3.4). The legacy synchronous path remains the comparison backend.

### D8 — Native DXR is a first-class track
*Measured (`apps/dxr-proto`, RTX 3070):* inline `RayQuery` passes 12/12 validation variants against an independent fp64 brute-force reference (derived error bounds, including the instance-transform term; 0 failures). 1M rays against a 1M-triangle BLAS take 0.77–1.79 ms; a 10,000-instance TLAS updates in 0.06 ms; a 1M-triangle BLAS refit takes 2.95 ms. Details are in `EXTERNAL_RESEARCH.md` §3.2.
The title's software traversal is kept as the **reference**. A standalone native DXR prototype on synthetic scenes measures build, refit, instance updates, traversal and memory (`apps/dxr-proto`). In-title substitution is decided per traversal program, by differential tests only.

### D9 — The legacy backend stays intact
The inherited GPU path stays buildable and selectable until the Gen 2 path passes correctness, stability and cross-vendor gates. Nothing is deleted before that.

## Subsystem disposition

| Inherited subsystem | Decision | Evidence / experiment |
|---|---|---|
| PE loader, HLE modules (kernelx, xgameruntime, xaudio2, gameinput, dstorage) | **Retain** | Lab reaches gameplay. No measured cost problem |
| `xmem` allocation (address ranges, placeholders, physical pages) | **Refactor** into `sixth-memory` (D2) | Parts 4–5 defects: address cap, census, SEC_RESERVE pool |
| Copy engine (`shadow.rs`, `watch.rs`, copy-back) | **Isolate as legacy backend** | Measured cost (E10/E11). D1 makes it unnecessary for the V class |
| `inplace.rs` (title memory as a heap) | **Isolate**; keep as probe/fallback only | The slowest backing measured (D1). Its WARP tests coincide with the parallel-test crash (B-32) |
| Raw-address directory (`memory.rs`) | **Replace** with the `sixth-memory` page table (fill on map, invalidate on free, generation tags) | DeepSeek R2 (fill-only), D6 |
| Shader translator (`victory-shader`) | **Retain**; **refactor** its memory lowering into tiers (D6) | Coverage is high; the memory path is the issue |
| Synchronous host submission (`host.rs`) | **Isolate as legacy**; new `sixth-scheduler` | D7 |
| Texture detiling/upload | **Retain**; page-granular later | No measured blocker yet |
| RT emulation (`image_bvh_intersect_ray` in DXIL) | **Retain as reference** | D8 |

## Planned crates (narrow interfaces, built one at a time with tests)

`sixth-telemetry` (event log: allocations, lifetimes, aliases, writers/readers, dirty ranges, transfer costs) → `sixth-memory` (address space, page table, residency, contracts) → `sixth-command-ir` (recorded commands with explicit resource uses, barriers and visibility) → `sixth-scheduler` (timelines, hazards, retirement) → `sixth-d3d12` (IR → D3D12) → `sixth-shader-ir` (memory tiers) → `sixth-raytracing` (DXR path + reference).

The order follows the dependencies. Telemetry comes first because every later decision needs its data.

## Not yet measured

AMD hardware; VRAM pressure and eviction on an 8 GB card; GPU upload heaps on a ReBAR-capable runtime; a large sparse directory (16 MiB, scattered) instead of a 32 KB one; coalesced vs per-page forwarding; native DXR build, refit and traversal.
