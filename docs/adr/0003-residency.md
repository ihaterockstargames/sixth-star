# ADR 0003: residency instead of a cut-off

Status: accepted. First stage implemented on 2026-11-26 (`121cf0d`, `009dd7a`); promotion back to the card (`e2453a4`) and hot windows on the card (`58a30ed`, off by default) added the same day (§5). Extends ADR 0002 §4.2 and §4.8.

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

## 5. Next steps

1. **Promotion back to the card (done).** `resource::promote`, from the texture pass, at most every half second:
   - only while the pressure is Low or Normal, and only up to the target less twice the margin (80%), so that what comes back does not itself raise the pressure (`Residency::promotable`);
   - most recently used first; not one a list not yet reset holds, one handed out in the last second, or one draws read through a table;
   - the copy in the PC's memory is given up, and the slots on its memory are made due (`descriptor::host::remapped`);
   - the texture pass makes each such slot again from the new copy on the card (`HostHeap::shows_stale`, `HostHeap::remake`). Until then the slot holds and shows the old copy, so nothing reads a texture that has gone.
   - `VICTORY_PROMOTE=0` turns it off. The residency line counts the textures that came back and the slots made again.
   - Tests on the device: `a_texture_made_in_the_pcs_memory_comes_back_to_the_card` and `a_slot_showing_a_copy_given_up_is_made_again_from_the_new_one`.
2. **Hot windows on the card (done, off by default).** `VICTORY_HOT_WINDOWS_MB=<MB>`:
   - a window outside buffers that draws asked for in 8 frames in a row is made again with a card side (the `Card` machinery title buffers of 16 MiB or more use);
   - only while the card has room (`promotable`), at most 64 MiB a frame, and only if no descriptor slot views the window (a slot would keep showing the old copy);
   - lists recorded with the old copy keep it until they are reset.
   - The cost: what the title writes in such a window goes up to the card once before each submission, where draws read it over the bus as often as they read it. A window the title rewrites whole every frame and draws read once gains nothing. Hence off until the maintainer's A/B.
   - Test on the device: `a_window_asked_for_frame_after_frame_moves_to_the_card`.
   - Still to do: title-buffer copies under 16 MiB by the same rule (they need the card compare, since shaders write some), and windows a slot views (their slots made again, as for textures).
3. **Suballocation** of small copies (each is its own committed resource today), following D3D12MA's model of placed resources in large heaps.
4. **Registry-tracked residency.** Every use known before submission, then `Evict`/`MakeResident` with LRU by submission (D3DX12Residency's model) instead of release-and-remake.

## 6. Gates

- **Before any default changes:** the frame line's `host memory alive`, `residency:` and `INCOMPLETE FRAMES` lines from the maintainer's run.
- **Hot windows:** `VICTORY_HOT_WINDOWS_MB` becomes a default only after an A/B in the maintainer's run (same scene, with and without), comparing `ms/frame`, `on the GPU`, the copies line and `INCOMPLETE FRAMES`.
- **Fallback:** `VICTORY_RESIDENCY=0` restores the inherited rule (no texture made past nine tenths of the budget, none given up early, none in the PC's memory) for an A/B. A missing texture is a correctness bug, so the fallback stays the default unless it causes worse ones.
