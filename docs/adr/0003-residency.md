# ADR 0003: residency instead of a cut-off

Status: accepted, first stage implemented (2026-11-26, `121cf0d`, `009dd7a`). Extends ADR 0002 §4.2 and §4.8.

## 1. The problem

The card has 8 GB. Windows gives the process a budget of 6.7 to 7.1 GiB, depending on what else runs: 7.08 GiB was read on 2026-11-26 in the morning, 6.72 GiB in the afternoon with other programs open.

The inherited rule was one threshold (`resource::room`): past nine tenths of that budget, the pass that makes textures picked by index made none. Their descriptor slots then showed nothing for as long as the card stayed full. That is a resource made silently invalid by the budget, which this ADR rules out.

What fills the card, and what the GPU reads over the bus instead, was not known. Counters existed only for what was ever made.

## 2. Where host memory lives (audit of every allocation site, 2026-11-26)

| What | Pool | Why it is there | CPU access needed? |
|---|---|---|---|
| Title-buffer copies under 16 MiB (`shadow.rs`, `shared_buffer`) | PC memory, custom heap L0, write-back | the CPU refreshes them and compares them after the GPU wrote | yes, for refresh and copy-back; the GPU reads and writes them over PCIe |
| Title-buffer copies of 16 MiB or more | card (card side) plus PC memory (CPU side) | `cards`, up to 512 MB in all, while a fifth of the budget is free | only the CPU side |
| Windows of title memory outside buffers | PC memory | `VICTORY_CARD_WINDOWS_MB` defaults to 0 | yes, for refresh. Shaders write none of them (an earlier run: 0 MB compared), and an earlier run had 3.5 GB of them, read by every draw over the bus |
| Title textures' host copies | card, default heap | sampled | no (uploaded through upload buffers) |
| Depth twins, stencil buffers | card | shaders write depth | no |
| Upload and read-back buffers | PC memory | transfers | yes |
| Title memory opened as a heap (`inplace`) | title memory itself | no copy at all | the CPU's own memory |
| Queries, indirect arguments, layer pass | both | internal | some |

The proportions are not known yet. The ledger (§4) prints them on the frame line in the maintainer's next run.

## 3. Measured (synthetic, RTX 3070, 2026-11-26)

- **Sampling from the PC's memory:** 64 reads of a 2048x2048 RGBA8 texture took 4.4 ms from the card and 41.1 ms from the PC's memory (custom heap, L0, no CPU page): **9.3×**. ADR 0001 D1 measured 17–27× for buffers, depending on the access pattern. Test: `residency::tests::the_card_filled_to_its_target_places_textures_in_the_pcs_memory`, ignored by default.
- **Correctness:** a texture in that heap, filled by a copy and read by a shader, gives every texel back exactly (`a_texture_in_the_pcs_memory_is_filled_and_sampled`, in the normal suite).
- **The card filled to 90%** with 64 MiB render targets (97 of them, 6.08 of 6.72 GiB):
  - a title-filled texture is placed in the PC's memory and a render target on the card;
  - once the filler is gone, the pressure comes back down within the hysteresis.

## 4. Decisions

- **R1. Account first.** `ledger.rs` books every host resource at creation by category and pool, with logical and allocated bytes.
  - The device's destruction callback (`ID3DDestructionNotifier`) takes it out again, so no owning structure changes.
  - The frame line shows what is alive and the churn per category, which answers "what fills the card, and what is duplicated".
- **R2. Pressure, not a threshold.** Four levels with hysteresis: Low < 60%, High ≥ 90%, Critical ≥ 97%, each left only 5 points below its mark (`sixth_memory::residency`).
- **R3. Never silently invalid.** A new GPU-read resource that finds no room on the card goes to the PC's memory: slower (×9 to ×27), but there and correct. A GPU-written one (render target, depth, unordered access) stays on the card. Refusal is the last resort, only when the PC's shared memory is also past 75% of its budget, and it is counted with its reason.
- **R4. Evict what can be remade, fence-aware, least recently used first.** Only textures the title fills, with nothing holding them (no list, no slot), not used for a second, and only until the card is back under its target less the margin.
  - The release goes through `host::retire`, after the submissions running now.
  - What a reclaim freed counts as gone for a second, so one shortage does not evict twice.
- **R5. No `Evict`/`MakeResident` yet.** D3D12's `Evict` is a promise that nothing will use the object until it is made resident again. Today a slot or a list may use a texture without asking, so that promise cannot be kept. The registry (ADR 0002 §4.3) makes every use known first; until then, release-and-remake is the only safe eviction.
- **R6. The fallback must not become the steady state.** A texture in the PC's memory is read at a tenth of the speed. It returns to the card when it is remade with room there.

## 5. What is not done (next steps, in order)

1. **Promotion back to the card.** When the pressure is Low or Normal, remake textures in the PC's memory on the card. Those a slot holds need their slot views remade (the `descriptor::host::remapped` path). The residency line counts them, so the maintainer's run shows whether they matter.
2. **VRAM-first for GPU-hot copies.** Windows and title-buffer copies that shaders only read get a card side by heat (submissions using them per second) within a share of the budget. This replaces the fixed 16 MiB and 512 MB limits and the disabled window switch. The card-copy machinery (`Card`) already carries the bytes up; the residency decides who gets it.
3. **Suballocation** of small copies (each is its own committed resource today), following D3D12MA's model of placed resources in large heaps.
4. **Registry-tracked residency.** Every use known before submission, then `Evict`/`MakeResident` with LRU by submission (D3DX12Residency's model) instead of release-and-remake.

## 6. Gates

- **Before any default changes:** the frame line's `host memory alive`, `residency:` and `INCOMPLETE FRAMES` lines from the maintainer's run.
- **Fallback:** `VICTORY_RESIDENCY=0` restores the inherited rule (no texture made past nine tenths of the budget, none given up early, none in the PC's memory) for an A/B. A missing texture is a correctness bug, so the fallback stays the default unless it causes worse ones.
