# ADR 0002 — From 218 ms to 25 ms: a structural roadmap

Status: **proposed** (2026-11-26). Builds on [ADR 0001](0001-gen2-architecture.md); where the two differ, this one is newer. Every number is labelled *measured* (with its source), *observed* or *hypothesis*. Figures from the title are measured by the maintainer from the runner's frame line; no title is run by anyone else for this document.

## 1. Where the frame goes today

Gameplay, RTX 3070 8 GB, Ryzen 7 5800X, 1080p, ReBAR on. Source: *measured by the maintainer from the runner's frame line on 2026-11-26* (also in `STATUS.md`).

| Part of the frame | ms | What it is |
|---|---|---|
| CPU preparing | 82.3 | finding what title code and shaders changed (write watch, guards, hashes), bringing host copies up to date, making descriptors, recording |
| Waiting for the GPU | 86.6 | wall time from `ExecuteCommandLists` to the fence, **per submission group, one after another** |
| Copying back | 44.7 | what shaders wrote, moved from host copies into title memory before the title may see the GPU finished |
| **Frame** | **217.9** | the three add up to 213.6: **nothing overlaps** |

Three facts decide the strategy:

1. **The path is serial.** `host.rs` submits a group, waits on its fence (`WaitForSingleObject`, `INFINITE`), and only then starts the next. CPU and GPU never work at the same time. Overlap alone, with no other change, would bound the frame at max(82.3 + 44.7, 86.6) = 127 ms: ~8 FPS. Overlap is necessary, not sufficient.
2. **"86.6 ms on the GPU" is not the title's rendering time.** It holds our own uploads of host copies, our hash and compare passes over copies in the card's memory, copy-back staging, the gaps between waited-for groups, and the title's passes. How much is the title's own work is **not measured**. That is the first thing to measure (§4.5), because it decides whether 25 ms is reachable at all.
3. **Moving bytes is not the cost; noticing them is.** *Measured* 2026-11-26 (`plain_copy_benchmark`, `host.rs`): the kernel's copy moves 24.8 GB/s at 64 KiB and 11.6–11.9 GB/s at 32 MiB; a plain in-process copy 3.0 and 21.4–21.6. The intro cutscene moves ~32 MB per frame, which is ~2.7 ms through the kernel, yet the frame line books ~30 ms of "copies". The rest is detection and bookkeeping. A faster copy saves ≤ 1.3 ms. **This is why the inherited memory model has to go, not be tuned.** `VICTORY_PLAIN_COPY` stays off as a recorded experiment.

The other projects built on the same base agree (`EXTERNAL_RESEARCH.md` §9): the GPU is 30–75% busy, and one CPU thread that notices, moves and waits is the wall. The fork that went furthest (280 → 46–65 ms) did it by making each cost narrower, wider or later. It never removed one. This roadmap removes them.

### The budget

| Target | ms |
|---|---|
| 40 FPS | 25.0 per frame, **with CPU and GPU overlapped** |
| GPU, title passes only | ≤ ~20 (*hypothesis*: the console GPU runs the title at 30 FPS, so ≤ 33 ms on 12 TFLOPS at its own resolution; this card has more raw FP32 but 8 GB instead of a 13.5 GB budget) |
| Our GPU overhead (uploads, forwarding, barriers) | ≤ 2 (D5's ≤ 1 ms forwarding budget plus barriers) |
| CPU on the submitting path | ≤ 20, off the GPU's critical path |
| Copy-back on the critical path | 0 in the common case (demand-driven, §4.1) |

If §4.5 shows the title's own passes take well over 25 ms on this card, 40 FPS needs shader work (§4.7) and possibly a lower render scale. That will be said plainly then, not hidden by doing less work.

## 2. What "success" means

- **The same work, correct output.** A frame-rate gain counts only if the renderer runs every draw and dispatch the title submitted, with the bytes the console would have given them. Frame lines must show `dropped 0` and `views left null 0` (or an explained, generic reason) before a speed-up is claimed in `STATUS.md`.
- **No local skip lists in measured runs.** The local launcher outside the repository sets `SKIP_LIST` when a private skip file exists, so measured runs to date skipped at least one pipeline (the intro frame line showed `dropped 1` per frame). Measurements for `STATUS.md` are taken without it, or say that it was on.
- **Every structural change lands with a synthetic regression test.** These are independently authored fixtures; no title content is used.
- **Switches are for A/B during a change**, not a destination. Each one is removed when its change becomes the only path, or dropped when it loses.

## 3. Correctness findings (generic; traced in our tree 2026-11-26)

| # | Situation | What the renderer does now | What the console does | Visible as |
|---|---|---|---|---|
| C1 | **Buffer view past its host copy** | the whole view was **left null**: every element read 0 (442 such views in the intro) | elements inside the view's record count read memory; past it, 0 | geometry or data silently zero |
| C2 | **Texture picked by index, upload pending** | slot made a **null view** while queued (`descriptor/host.rs` `make`); ~12,500 slots waiting in the intro | the texture shows its memory as soon as the view is written | black or garbage surfaces, flicker as slots go null ↔ valid |
| C3 | **Texture or buffer memory not mapped** at the moment we look | buffer refresh writes zeros for unreadable pages (`host.rs` `read_pieces`); texture slots are "passed over unmapped" (~17,800 in the intro) and retried after 1, 2, 4… frames | the title only binds memory it mapped; once mapped it is visible at once | late or missing streamed detail; stale zeros |
| C4 | **No host pipeline** (translation failed, still compiling, or on a skip list) | the draw or dispatch is **silently dropped** (`draw.rs`, `DROPPED`) | it runs | missing objects or passes; speed that is not real |

**C1 is fixed in part in this change** (`descriptor/host.rs` `fit_to_copy`): a view that runs past its copy is cut to the elements in the copy, so those read their bytes and the rest read 0, which is the host's bounds check. Fixture: `buffer_views_past_their_copy_are_cut_not_nulled`. `VICTORY_CUT_VIEWS=0` restores the old behaviour for one A/B. What remains (elements past the copy that the console would read from neighbouring memory) needs the registry (§4.3), which grows backings to cover their views.

C2–C4 are addressed in §4.8, §4.3 and §4.9. Each has fixtures listed there.

## 4. The ten workstreams

Each item lists: what work it eliminates, which legacy parts become unnecessary, the expected impact and its evidence, correctness requirements, steps, and synthetic regression tests. The order in §5 follows dependencies.

### 4.1 Eliminate routine shadow-buffer synchronisation

**The idea.** Today every buffer a GPU command touches has a host copy that is re-checked and refreshed at every submission, and everything shaders may have written is copied back before the title sees the fence. Instead, **bytes are synchronised only where an access contract says the other side reads them, and only when it does.**

- GPU-only memory (render targets, GPU intermediates, acceleration structures, static geometry once uploaded) lives in VRAM and is **never scanned or copied back**.
- CPU-written, GPU-read memory (constants, dynamic vertices) is **forwarded**: only the dirty runs, coalesced, inside a ≤ 1 ms budget (D5). With ReBAR the forward is a direct write into the GPU upload heap, with no copy command (D5b).
- GPU-written, CPU-read memory (queries, readbacks, results used by CPU code) is fetched **on demand**. Pages the GPU owns are kept no-access in title memory; the first CPU touch faults, and the handler waits for the producing work, reads the page back and lets the access go on (D4 rules: GPU bytes are the authority; a snapshot diff finds the CPU's own writes).

**Work eliminated.** Per-submission write-watch askings and guard resets over all copied memory; hash and compare passes over copies in the card's memory; eager copy-back (44.7 ms); refreshing copies that nothing changed; most of the 82.3 ms of preparing.

**Legacy that becomes unnecessary.** `shadow.rs` (host copies and card copies), `watch.rs` journal and guards (kept only for contract regions), `host.rs` `Batch`, `hand_back`/`declared_writes`, the flights and late-wait machinery, `inplace.rs`, and the `VICTORY_WIDE_*`, `JOIN_COPIES`, `CARD_REHASH_RUNS` and `PLAIN_COPY` switches.

**Expected impact and evidence.**
- *Measured* (`membench`): forwarding costs ≈0.3 µs record + ≈0.3 µs GPU per copy command, and coalescing divides that by the run length (`--suite forward`, 72/72 byte-exact). ReBAR upload heap: CPU writes 9.8 GB/s, GPU reads it at VRAM speed (420 GB/s).
- *Measured*: CPU write → GPU read → CPU sees it, ≈100–112 µs round trip. That is the cost of one demand fault batch.
- *Hypothesis*: preparing 82 → ≤ 15 ms, copy-back 45 → ~0 on the critical path, plus the share of the 86.6 ms that is our own compare and upload work. To be confirmed by the census of §4.2 (how much memory is really CPU-written each frame).

**Correctness requirements.** No byte the console would have shown may differ. The D3 rule applies: no ownership from observation, only from a declared or proven contract, otherwise `Mixed` (which keeps today's safe path). Late CPU writes to GPU-owned pages follow D4. Fault handling must not deadlock with the title's threads, so the handler never takes a lock the submitting thread holds while it waits on the GPU.

**Steps.**
1. Observe-only telemetry in the runner (`VICTORY_GEN2_TELEMETRY=1`): every allocation, D3D12 heap and resource, view, map and unmap, as `sixth-telemetry` events with the declared contract. No behaviour change.
2. Contract census report: bytes per frame by contract class, and how much of today's refreshed and copied-back memory is actually written by the other side.
3. GPU-owned class first, the largest expected win: VRAM backing, no copy-back, demand fault-in. `Mixed` stays on the legacy path.
4. CPU-written class: dirty runs from contract-scoped write watch, forwarded through the planner; then D5b direct writes.
5. Remove the legacy path per class once its fixtures and an A/B on the title (run by the maintainer) agree.

**Measured 2026-11-27 (stand-in title, `6133e9d`).** A texture copy from a title buffer is a *fill* (title memory into the list's own upload memory), not a refresh, so `VICTORY_WIDE_REFRESH=1` did not reach it. Fills in a row now run on several threads under that switch (cut into 1 MB pieces, done before the next step of another kind). 16 copies of 3 MB a frame: CPU preparing 4.2 -> 2.7 ms; the default (switch off) is unchanged until the title is measured. This makes the copies faster; it does not make them fewer, which stays the aim of this workstream.

**Measured 2026-11-28 (stand-in title, `51cab58`).** Committed buffers whose memory the default allocator takes from the host heap cannot be write-watched, so every refresh copied them whole. `VICTORY_WATCHED_ALLOC=1` makes the default allocator's large blocks (64 KiB and up) write-watched reservations of their own. With 10% of each of 8 buffers of 3 MB rewritten a frame, `texcopy` CPU preparing fell 2.5 -> 0.3 ms and the copies 24.6 -> 2.6 MB a frame (two runs). With whole buffers rewritten it rose 2.6 -> 3.2 ms, and the stand-in's own frame 10.8 -> 15.6 ms: the watch is asked more often, and writes after each reset cost more. The switch stays off by default. The next step is to stop asking, and resetting, the watch for buffers that are found wholly written frame after frame; that is fewer bytes without the reset cost.

**Measured 2026-11-28 (stand-in title, `d130c10`).** `VICTORY_HOT_SKIP=1` is that step: a range found wholly written in two askings in a row is answered "all written" for 8 askings without the kernel, then asked again (the first answer after the skips holds all their writes and is not judged). The answer is a superset, so it cannot miss a write. With whole buffers and `VICTORY_WATCHED_ALLOC=1`, the write watch fell from 8 askings and 0.35 ms to 2 askings and 0.08 ms a frame, CPU preparing 3.3 -> 3.0 ms (two runs), the stand-in's frame 15.8 -> 12.0 ms; with 10% rewritten nothing changed (0.3 ms). Both switches stay off by default: whole buffers still cost 3.0 ms against 2.6 without the watch, and the gap is in the copies themselves (2.8-2.9 ms for the same 24.6 MB against 2.6 ms; already one piece per run of written pages, so the cause is still to be found, perhaps the memory of the watched reservations).

**Default since 2026-11-27 (`ae1ceaf`).** The fills have their own switch, `VICTORY_WIDE_FILLS` (`6334ca1`), and are on by default; `VICTORY_WIDE_FILLS=0` turns them off. `VICTORY_WIDE_REFRESH` (the refreshes) stays off. Stand-in title, CPU preparing, `baseline` against `fills`: 16 copies 3.8 -> 2.6 and 3.9 -> 2.6 ms, 8 copies 2.0 -> 1.4 ms, 2 copies 0.5 -> 0.4 ms. With the new default against `nofills`: 8 copies 1.4 against 2.1 ms, 16 copies 2.7 against 4.2 ms. GPU work and picture unchanged; fills of less than 2 MiB in all stay on the submitting thread. Not measured on the test title yet.

**Synthetic regression tests.**
- *Producer/consumer*: a compute pass writes a buffer and CPU code reads one dword of it. The value is right, exactly one page is fetched, and no other page is copied.
- *Untouched GPU memory*: 64 MiB written by the GPU and never read by the CPU means 0 bytes of copy-back.
- *Late CPU write to a GPU-owned page* (D4): CPU and GPU write different bytes of one page, and the merge is byte-exact against `merge_with_snapshot`.
- *Forwarding*: scattered and contiguous dirty patterns. The VRAM bytes equal the CPU view, and the command count equals the planner's run count.
- *Fault handler*: CPU reads of GPU-owned pages from 8 threads while the submitting thread waits on the GPU. No deadlock (timeout test) and all values right.

### 4.2 Persistent VRAM residency and explicit access contracts

**The idea.** Each title memory region has a **home** (VRAM, upload heap, or system memory) and a **contract** (who reads and writes it) decided once from declared sources: D3D12 heap type, console memory-type bits on `XMemAlloc`/physical pages, DirectStorage destinations, and explicit copies. It stays resident across frames. A residency manager keeps the working set inside the 8 GB card's budget.

**Work eliminated.** Re-uploading memory every frame because nothing says it is still valid; system-memory backings for GPU-hot data (D1: 17–27× slower GPU reads); the guesswork of the "card copies".

**Legacy that becomes unnecessary.** Host copies as the source of truth; `inplace.rs` as a backing (D1 measured it as the slowest); `resource::room` heuristics.

**Expected impact and evidence.** *Measured* (`membench`, D1): VRAM 428–432 GB/s stream, 34 GB/s random, 13.2 µs chase step, against system memory at 23–26 GB/s, 1.25–1.30 GB/s and 244–246 µs. Sparse VRAM costs about nothing over plain VRAM. `UpdateTileMappings` of 4,096 tiles takes 0.64 ms to completion. Any GPU-hot buffer still in system memory today costs the title's passes up to 19× in dependent loads. §4.5 will show which.

**Correctness requirements.** The contract is never inferred from a few frames of observation (D3). Eviction must never discard bytes that have no other copy. The 8 GB card is below the console's budget, so over-commit must degrade gracefully (evict by priority, then a lower texture mip budget), not crash or remove the device.

**Steps.** (1) Contracts from declared sources in `sixth-memory::contract` (exists), fed by the telemetry hooks of §4.1. (2) VRAM arenas: reserved resources on DEFAULT-heap tiles over the title's GPU-visible ranges, suballocated through D3D12MA (MIT) behind `sixth-memory`. (3) Residency manager: `QueryVideoMemoryInfo` budget, `SetResidencyPriority`, `MakeResident`/`Evict`, LRU by last GPU use. (4) `membench --suite pressure`: allocate past the budget and measure eviction and re-touch cost.

**Synthetic regression tests.** Contract derivation per declared source (table test). Residency: allocate 1.5× the budget in priority classes; high priority stays resident and nothing is lost after eviction and re-touch (byte check). Tile mapping: map, unmap and remap a reserved buffer, and GPU reads see each generation's bytes.

### 4.3 One resource registry for buffers, textures, aliases and physical mappings

**The idea.** Today the same question ("what is at title address X, and what shows it on the host?") is answered by six structures: `shadow::find`, the texture `STAGING` map, descriptor slot tables, `inplace` windows, `addr::note_title_range`, and the raw-address directory in `memory.rs`, which is filled when shaders fault. Replace them with **one interval registry in `sixth-memory`** keyed by title virtual address, with generation tags. Each entry holds kind (buffer, texture, alias, physical mapping), backing (VRAM allocation and offset, or system memory), contract, and the views made from it.

**Work eliminated.** Polling for mappings ("passed over unmapped", retried after 1, 2, 4… frames; C3); searching several maps per view; views that cannot be made because the copy is smaller than the view (C1); stale views after a remap.

**Legacy that becomes unnecessary.** The five lookup structures above; the fault-filled directory; `descriptor::host::remapped`, `inplace::views_gone` and `STALE` bookkeeping as separate paths.

**Expected impact and evidence.** A map or unmap becomes an **event** that invalidates exactly the dependent views and schedules exactly the dependent uploads. *Measured* (intro frame line): 17,800 slot looks passed over unmapped and 12,500 waiting, all re-polled. The AMD fork's largest single step (280 → ~124 ms) was descriptor records left to the draws that use them. A registry gives the same effect structurally: a view is resolved when it is used, through one lookup.

**Correctness requirements.** Generation tags on every lookup, so a view made before a remap never reads the new mapping's bytes as if they were the old ones, and the reverse. Aliases (two resources over one range) share one backing. Writes through one are visible through the other after an aliasing barrier, which `membench` *measured* correct. Physical-page mappings (one physical page at several virtual addresses) resolve to one backing page.

**Steps.** (1) Registry in `sixth-memory` (`AddressSpace` already has page entries with generations). (2) xmem's map, unmap and alias paths (`XMemMapPhysicalPages`, `map_alias`, frees) post events to it, replacing `title_memory_remapped`. (3) Descriptor creation resolves through it, and backings grow to cover a view (the rest of C1). (4) Retire the old maps one at a time, each behind its fixtures.

**Synthetic regression tests.**
- View made, memory remapped, view used: it reads the new mapping, and a view of the old generation is never used.
- Alias pair: write through A, aliasing barrier, read through B, and the bytes are equal.
- One physical page mapped at two addresses: a CPU write through one is seen by a GPU read through the other.
- A view larger than its first-known resource: every element inside the view reads its memory (C1 complete).
- Map after the view is written (C3): contents visible in the next submission, with no polling delay.

### 4.4 Dependency-aware command scheduling

**The idea.** Record the title's command lists into `sixth-command-ir`, with explicit resource uses (range or subresource, read or write). `sixth-scheduler` derives the hazards (RAW, WAR, WAW per range and resource), places barriers, and submits on timelines **without waiting for each group**. Title-visible completion (fences, bottom-of-pipe memory writes) is signalled from the timeline, ordered after any copy-back the contracts require (D7).

**Work eliminated.** The per-group `WaitForSingleObject`; CPU and GPU idling in turn; the submitting thread being the only place where work is prepared.

**Legacy that becomes unnecessary.** The synchronous loop in `host.rs` (`submit` and `settle`), the late-wait, flights and relay-copy-queue switches, and `BufferStates` guesswork.

**Expected impact and evidence.** *Arithmetic from §1*: overlap alone bounds the frame by its longest part instead of the sum (218 → ≤ 127 ms before §4.1 removes anything; with §4.1, the GPU part becomes the bound). *Measured elsewhere* (AMD fork): preparing on its own thread took 103 → 75 ms, and recording on 8 worker threads took 75 → 68 ms. The scheduler is also where recording spreads over threads safely (ADR 0001 D7).

**Correctness requirements.** A guest-visible completion is never signalled before the GPU work and the copy-back it covers. No resource is read before its producing write (the AMD fork's device loss: a texture shown before its upload ran). The inherited late-wait mode produced device removals, so hazards come from declared uses and are never guessed from timing.

**Steps.** (1) IR types with resource-use lists, plus a recorder fed by the existing translation (`list.rs`, `draw.rs`). (2) A hazard tracker over ranges, unit-tested apart from D3D12. (3) A replay backend: IR → D3D12, with the synchronous backend as the reference. (4) Asynchronous timelines, with title fences mapped to timeline values. (5) Parallel recording on workers, with submission order kept.

**Synthetic regression tests.** Hazard tables (RAW, WAR, WAW, overlapping and disjoint ranges, subresources). Ordering fixture: upload, then draw sampling it, then a title fence; the fence value is seen only after the draw's output is right. Replay equivalence: a captured synthetic stream gives byte-identical results through the synchronous and asynchronous backends. A stress test of 10,000 submissions with random dependencies, with no lost or early completions.

### 4.5 GPU timestamp profiling per pass (first, because it decides everything else)

**The idea.** An always-available, low-overhead profiler. A timestamp query pair around every title pass (a run of draws to one set of targets, or a dispatch group) and around **each of our own operations** (uploads, forwards, compares, copy-back staging, barriers). Results are resolved one or two frames late, without stalling. The frame line gains one more line: `GPU: title passes X ms (top 5: …), ours Y ms (uploads, compares, copy-back), idle Z ms`.

**Work eliminated.** Guessing. Today "on the GPU" mixes the title's work, our overhead and idle gaps. Per-command timing exists only in a `diagnostics` build (`VICTORY_GPU_TIMING`), which waits on every group.

**Legacy that becomes unnecessary.** The diagnostics-only `slowest()` path and the `card_clock` estimate.

**Expected impact.** None on speed by itself (≤ 1% overhead, a requirement). It is the gate for §1's question: if the title's own passes are near 20 ms, §4.1 + §4.4 can reach 25 ms. If they are 60 ms, §4.7 and render scale are on the critical path.

**Correctness requirements.** Timestamps on the same queue as the work, with frequency from `GetTimestampFrequency`; no added waits; and overhead measured and reported.

**Steps.** (1) A query heap ring with readback, resolved two frames late. (2) Pass boundaries from the recorder (target-set changes, dispatch runs), with names by pipeline hash. (3) Our operations wrapped by category. (4) A frame-line summary and an optional CSV to a user-chosen file outside the repository. (5) PIX event markers with the same names.

**Synthetic regression tests.** A spin shader with N iterations: measured time is proportional to N within 5%, and monotonic. Two passes A and B with known costs, attributed to the right names. Overhead with the profiler on versus off over 1,000 submissions is ≤ 1%.

### 4.6 Native hardware ray tracing

**The idea.** Replace the translated software traversal (`image_bvh_intersect_ray` emulated in DXIL) with DXR inline `RayQuery`, per traversal program, accepted only on differential tests (D8).

**Work eliminated.** Software BVH traversal: dependent random loads through raw addresses, the slowest access pattern measured (13 µs per chase step even in VRAM, and 245 µs in system memory).

**Legacy that becomes unnecessary.** The emulation stays as the **reference**, not the runtime path, for each program that is substituted.

**Expected impact and evidence.** *Measured* (`dxr-proto`, RTX 3070): 1M rays against 1M triangles in 0.6–1.8 ms; TLAS update of 10,000 instances 0.06 ms; 1M-triangle BLAS refit 2.95 ms; 12/12 validation variants against an fp64 reference with 0 failures; and the same on Vulkan (`vkrt`). The emulated path is **not measured yet** (`swtrace`, below). The gain is unknown until it is.

**Correctness requirements.** Hit decisions match the fp64 reference within derived error bounds (already done for DXR). Geometry fed to the BLAS must be the title's own, through the registry (§4.3). First, check the dispatch flag that means "counts are threads, not groups": at the AMD fork every ray missed until it was fixed.

**Steps.** (1) The dispatch-flag check with a fixture. (2) `asvalidate`: a CPU validator for console BVH nodes per GPURT's MIT node definitions. (3) `swtrace`: the emulated traversal on `dxr-proto`'s scenes, ray by ray against DXR and fp64. (4) Recognise traversal programs and build a BLAS/TLAS from the title's node data. (5) Substitute per program, behind a differential test.

**Synthetic regression tests.** The dispatch-count fixture; the 12 `dxr-proto` variants; `swtrace` equivalence; BLAS-from-nodes round trip (nodes → BLAS → same hits as the node traversal).

### 4.7 Shader IR optimisation and address specialisation

**The idea.** `sixth-shader-ir` with memory tiers as distinct intrinsics (D6): **direct** resource access when the translator proves one resource; **region-relative** when it proves one region; the **page table** only for general pointers. Plus a known-bits and alignment pass, and the usual SSA clean-ups.

**Work eliminated.** Page-table lookups and bounds work on accesses that are provably direct; scalar splits of vector loads; repeated constant loads.

**Legacy that becomes unnecessary.** The raw-address directory as the default path (`memory.rs`); fault-driven filling.

**Expected impact and evidence.** *Measured* (`membench`, `EXTERNAL_RESEARCH.md` §3.3–3.4): the page table costs 17–18% on D3D12 random reads; vector loads are 1.7–1.8× scalar; our own DXIL is 1.53× DXC's HLSL output for the same kernel; the DXIL alignment operand is ignored by this NVIDIA driver. The share of the title's GPU time that is memory-bound is unknown until §4.5.

**Correctness requirements.** A tier is chosen only on proof. When proof fails, the page table is used, never a guess. Every translated shader passes the DXIL validator. Differential execution against the current translation gives identical outputs on synthetic inputs.

**Steps.** (1) IR with tiered memory intrinsics, lowering from the current translator. (2) Descriptor-provenance analysis (V#/T# from constant or user data to one resource). (3) Known-bits and alignment. (4) Emit tiers. (5) A pass-level A/B with §4.5 timestamps.

**Synthetic regression tests.** Independently authored console-ISA shader fixtures for each tier (provable direct, provable region, general pointer). Their outputs match the reference interpreter or current translation byte for byte. Validator on every fixture. A negative fixture, where proof must fail, stays on the page table.

### 4.8 Texture residency and descriptor lifecycle

**The idea.** A texture view the title writes is **valid from the moment it is used**: it shows the texture's current memory, or, while its larger mips are not mapped, its mapped mips (a view clamped with `MostDetailedMip`). It is never null because our upload queue has not reached it. Descriptor slots carry the registry generation of what they show. A remap invalidates exactly those slots.

**Work eliminated.** The texture queue's budgeted catch-up (C2); recheck passes after 4, 12, 40, 120 and 400 frames; slot sweeps; re-hashing whole textures to find changes. Uploads are driven by registry events and contracts instead.

**Legacy that becomes unnecessary.** `TextureQueue` priorities and census, `HEAP_RECHECK`, `KEPT_VIEWS` as a special case (it becomes the general rule), and the null-on-pending path in `descriptor/host.rs` `make`.

**Expected impact and evidence.** Correctness first: the black and flickering surfaces fit C2 exactly (the intro frame line shows 12,400–12,600 slots waiting). Speed second: the slot sweeps and heap-buffer passes are 7–9 ms per frame of preparing in the intro.

**Correctness requirements.** A draw that samples a slot written earlier in the same frame sees that texture (or its mapped mips), never zeros. A slot written again with more mips keeps showing the old view until the new one is uploaded (today's `KEPT_VIEWS`, made general). Detiling stays byte-exact against the reference.

**Steps.** (1) First use uploads synchronously within a per-frame budget, and beyond the budget a mip-clamped view of the mapped mips. Never null for a texture that has memory. (2) Slots record a registry generation. (3) Map events trigger uploads, replacing polling. (4) Residency of textures through §4.2's manager (mip budget under VRAM pressure).

**Synthetic regression tests.**
- Write a view, draw sampling it in the same submission: the texels equal the reference (no zero texels).
- Partially mapped texture (small mips only): sampling a coarse mip gives the mapped mip's texels, not zero.
- Slot rewritten with more mips: the old texels are shown until the upload, then the new ones; never zero in between.
- Remap under a slot: the next draw sees the new contents.
- Flicker test: 100 frames of a static scene give identical texel output every frame.

### 4.9 Pipeline coverage and validation

**The idea.** Every draw and dispatch the title submits runs. A missing pipeline is a **correctness failure that is counted and reported**, not a silent skip.

**Work eliminated.** Silent drops (C4); skip lists in measured runs.

**Legacy that becomes unnecessary.** `VICTORY_SKIP_PIPELINES` and `SKIP_LIST` as anything other than a debugging tool. The local launcher's automatic skip list (outside the repository) should not be on by default.

**Expected impact.** Speed may go **down** when dropped work comes back. That is correct and will be reported as such.

**Correctness requirements.** `dropped` and its reasons (translation failed, compile pending, skipped, refused by the device) are on the frame line and in `STATUS.md`. A run with drops is labelled as such. While a pipeline compiles, the draw waits for it (bounded), or the frame is marked incomplete. It is never shown as complete.

**Steps.** (1) The reasons census on the frame line in normal builds (today it is diagnostics-only). (2) Offline warm-up: translate and compile the shader cache before the first frame, which also addresses slow start-up (§4.10). (3) A DXIL validator and WARP PSO creation over the whole translated corpus in CI, using synthetic fixtures. (4) A triage list of untranslatable instructions by generic opcode, each with a fixture.

**Synthetic regression tests.** A fixture shader whose PSO creation fails must show in the census with its reason, and the frame line says `dropped 1`. A compile-pending pipeline is not shown as drawn. A corpus test where every fixture validates and creates on WARP.

### 4.10 Start-up and runtime stability

**The idea.** Start-up does no work that can be cached, and a stall is diagnosed by the runtime itself.

**Work eliminated.** Translating and compiling pipelines on first use during loading (slow boot); guessing at freezes.

**Legacy that becomes unnecessary.** Nothing removed. This adds instruments.

**Expected impact and evidence.** *Observed by the maintainer*: start-up is slow, and the intro cutscene froze (the picture stopped; the last frame lines showed ~22 FPS, then no more presents). *Measured* in the inherited test suite (ADR 0001): `victory-gpu` tests crashed in 5/12 parallel runs, a stability defect of its own.

**Correctness requirements.** A watchdog never changes behaviour; it only reports. Caches are keyed by translator version and shader hash, so a stale cache can never give wrong code.

**Steps.**
1. **Present watchdog**: when nothing is presented for 5 seconds, log once what our threads and the HLE wait objects are blocked on (fence values outstanding, events not signalled, the submitting thread's state). This turns "it froze" into a generic, reportable line.
2. Pipeline and translation cache warm-up at start (shared with §4.9).
3. Fix the parallel-test access violation (B-32), so CI can run tests in parallel.
4. Integration titles: SimpleTriangle and SDL3 `testgdk` (freely licensed GDK programs) through `sixth-star run` in CI.
5. A 30-minute soak on synthetic workloads, then the maintainer's 30-minute title run for "playable".

**Synthetic regression tests.** Watchdog: a test that holds a fence unsignalled for 6 s gets exactly one report naming the fence. Cache: a translator version bump invalidates. Integration titles reach N presents with the expected image hash.

## 5. Order of work and exit criteria

| Phase | Work | Exit criterion |
|---|---|---|
| 0 | §4.5 GPU timestamps; §4.10 watchdog; §4.9 drop census on the frame line; C1 (done in part) | the 86.6 ms is split into title passes, our work and idle time, from the maintainer's run; drops and null views on the frame line |
| 1 | §4.1 step 1–2 telemetry and contract census; §4.3 registry, observe-only | the census says how many bytes per frame each contract class really needs |
| 2 | §4.3 registry live (C1 complete, C3); §4.8 texture lifecycle (C2) | the C1–C3 fixtures pass; no null views from pending uploads; flicker fixture stable |
| 3 | §4.1 GPU-owned class: VRAM, no copy-back, demand fault-in; then CPU-written forwarding | copy-back off the critical path; preparing ≤ 20 ms (the maintainer's frame line) |
| 4 | §4.4 IR and scheduler with asynchronous submission | frame ≈ max(CPU, GPU), not the sum |
| 5 | §4.7 shader tiers; §4.6 native RT; §4.2 residency under pressure | per-pass GPU time down where §4.5 shows memory-bound passes |

The legacy backend stays selectable until each phase's fixtures pass and the maintainer's A/B agrees (ADR 0001 D9). Then it is removed, not kept behind a switch.

## 6. What is not claimed

- That 40 FPS is reachable on an 8 GB RTX 3070. That depends on the title's own GPU time, which is unmeasured until Phase 0.
- Any speed for the emulated ray traversal, AMD hardware, or VRAM eviction under the title's load: not measured.
- That C1's cut is complete. Elements past the host copy still read 0 until the registry grows backings.

## 7. Measured since: the engines on a synthetic frame (2026-11-26)

`cargo run -p enginebench --release` models one frame with real operations on this machine (Ryzen 7 5800X): a 512 MiB write-watched title reservation, the CPU writing 32 MiB of it in scattered 4 KiB pages, shaders declared to write 64 MiB (or 174 MiB) of which they write 8 MiB, and the CPU reading part of what they wrote. GPU work is not timed; only the CPU work around it is. Medians of 25 frames, three runs, which agreed within about 10%:

| Workload | Inherited engine | Timeline, guarded | Timeline, unguarded (blind copy) |
|---|---|---|---|
| intro-like: 64 MiB declared, 8 MiB written, 256 KiB read by the CPU | 13.2–14.3 ms | 4.6–4.8 ms | 21.7–22.7 ms |
| 174 MiB declared, 8 MiB written | 25.3–28.4 ms | 4.5–4.8 ms | 51.0–52.8 ms |
| CPU reads 10% of what shaders wrote | 12.8–14.1 ms | 4.8–5.0 ms | 21.6–22.5 ms |
| CPU reads all of it | 13.4–14.2 ms | 7.5–8.5 ms | 22.2–22.7 ms |

What it shows:

1. **With guards the timeline removes 37–84% of this CPU work** (37–47% when the CPU reads everything shaders wrote, 64–84% otherwise). Bringing the GPU's writes back drops from 5.7–13.9 ms to 0.1–0.3 ms when the CPU reads little of them, and the snapshot (3.3–9.9 ms) disappears. What is left is mostly copying the CPU's own 32 MiB up (3.0–3.6 ms in both engines), which only a smaller CPU-written region or direct writes into the upload heap (D5b) reduce.
2. **Without guards, copying the declared writes back blindly is three to four times slower than the inherited compare.** The conservative path for unguarded memory therefore stays the inherited snapshot-and-compare. `sixth_memory::timeline::Retired::fetch_now` says so. The gain depends on guarding GPU-written memory, which makes the guard (fault-driven demand fetch, D4) the first thing to build in the runner, not an option.
3. **The inherited algorithm at ideal speed costs 13–28 ms; the maintainer's gameplay frame spends 82.3 ms preparing and 44.7 ms copying back.** Most of the real cost is therefore not the algorithm but what surrounds it: kernel calls per range, many small write-watch askings, guards and page protections, whole-texture hashes, heap passes, card copies, several submissions per frame. The CPU profiler now in the build (`cpuprof`) names those shares in the next run. This is a model with chosen parameters, not a frame-rate prediction.
