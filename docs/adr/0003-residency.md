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
2. **One rule for host copies (done, off by default: `VICTORY_HOT_WINDOWS_MB=<MB>`).** `sixth_memory::residency::Residency::copy_home` decides for windows and title-buffer copies alike (`274f873`, `9a969c0`):
   - a copy moves to the card once it has been used 8 frames in a row, while the card has room (`promotable`), at most 64 MiB a frame, up to the MB given;
   - never title memory opened in place, never while submissions may be in flight (late wait);
   - a copy shaders may write (an unordered-access title buffer, or writes were ever declared) moves only when nothing but the map has it.
   - **Slots.** A draw finds a slot's copy afresh by address, while the slot keeps the buffer it was made with. So the replaced copy stays as the new one's *twin* while a slot shows it or a list holds it.
     - Every refresh of either copy copies into both (`Shadow::copy`).
     - A draw that reads such a slot remakes it from the new copy (`HostHeap::shows_replaced`, `remake`).
     - The sweep lets the twin go once nothing shows or holds it.
     - If shaders ever write a twinned copy, the other takes that range again from title memory once it is back there (`twin_wrote`). A read of the other copy in the same submission would see the old bytes; it is counted on the residency line.
   - **Placement when made** (`80556df`): below the inherited 16 MiB, only writable copies go to the card when made (`VICTORY_CARD_BUFFERS_MIN_MB`). Read-only ones are left to the rule above. Placed first, they had taken the bytes the big writable ones needed.
   - **Copy-back noting** (`bb57383`, on by default, `VICTORY_BACK_NOTE=0` off): a 64 KiB page a copy-back wrote whole, and which the copy held as current before, is noted current instead of being copied back into the same copy at the next refresh.
   - Device tests: `a_window_asked_for_frame_after_frame_moves_to_the_card`, `a_hot_read_only_buffer_moves_and_a_writable_one_stays`, `a_slot_showing_a_copy_that_moved_is_made_again_from_the_card_copy`, `a_copy_back_notes_current_only_whole_pages_the_copy_held`.
   - Card merges note their pages too (`1f735a8`): in `frame_bench`'s best configuration, refreshes then copy only what the title wrote (20.5 instead of 31.5 MiB a frame).
   - Moves no longer wait for flights to land (`b0ec57b`). Only a writable copy that something holds stays.
   - **Demotion** (`ecdf692`): under pressure the reclaim, after idle textures, puts hot copies back into the PC's memory, least recently used first (`shadow::demote`), the same way they moved up. Textures come first, since a texture in the PC's memory is sampled about 9× slower. `pressure_stress` checks it with the card refilled to 93%.
   - **By use, when the budget is full** (`252f632`): each copy counts its asks per frame. A copy that would move up but finds the byte budget full has the least used hot copy put back at the next sweep, if it is used at least twice as much. Under pressure, the least used go back first.
   - **`VICTORY_HOT_WINDOWS_MB=auto`** (`9bffd07`): no byte cap of its own; the card's room (`promotable`) is the limit, and demotion guards the textures. The swap acts there too (`48e1306`): a copy kept out only by the card's room has the least used hot copy put back.
   - **Texture copies from host copies** (`78d67bc`, `VICTORY_TEXTURE_FROM_COPY=1`, off): a title copy from a buffer into a texture reads the buffer's host copy instead of filling an upload of its own from title memory. `late_bench` 70.6 → 24.7 ms; textures are read back byte-exact in four modes (`c6c84c3`).
   - Still to do: a writable copy that a list holds. It needs the move to happen when no list that is recorded but not yet submitted names it.

### Synthetic measurements (RTX 3070, 2026-11-26, through the host's own paths)

`residency_bench`: 512 read-only 4 MiB title buffers in write-watched memory, all read by a compute pass each frame (2 GiB), the title rewriting 1% of their pages:

| hot budget MiB | on the card | ms/frame | preparing | on the GPU |
|---|---|---|---|---|
| 0 | 0 | 89.1 | 5.8 | 82.7 |
| 512 | 128 | 70.9 | 6.4 | 63.2 |
| 1024 | 256 | 51.6 | 6.4 | 44.4 |
| 2048 | 512 | 15.8 | 9.5 | 8.0 |

At 25% rewritten the GPU time falls 82 → 28 ms (what changed goes up once), but preparing stays at 77 ms: the CPU copy of the rewritten pages.

`copyback_bench`: 512 MiB declared written per frame, 1% of its pages written. 4 MiB copies in the PC's memory cost 82 ms preparing (the snapshot) plus 66 ms copying back (the CPU compare). On the card, 13 + 5 + 14 ms: 4.5×. With 10% of pages written in a scattered pattern, every 64 KiB piece is fetched and the gain is small.

`frame_bench`, a frame shaped like the game's measured one:
- 1 GiB read-only buffers read whole, 2% rewritten by the title;
- 64 writable 4 MiB and 8 writable 16 MiB buffers, 2% written by shaders in dense blocks, every write checked in title memory;
- lists kept two frames, as the title keeps them.

| switches | ms/frame | preparing | on the GPU | copying back |
|---|---|---|---|---|
| defaults | 109.0 | 40.4 | 42.4 | 28.0 |
| `VICTORY_HOT_WINDOWS_MB=1024` | 77.4 | 45.1 | 4.3 | 30.6 |
| `VICTORY_CARD_BUFFERS_MIN_MB=1` (`_MB=1024`) | 54.0 | 7.0 | 43.2 | 3.8 |
| both (`ab.bat` `best`) | 18.5 | 8.3 | 5.6 | 3.9 |
| keep snapshots + wide copy-back | 66.0 | 6.5 | 42.2 | 17.4 |
| late wait 3 | 109.9 | 42.0 | 42.8 | 27.6 |

With card-merge noting, "both" is 16.5 ms. It is 6× faster in this model.

`late_bench`, submissions in flight through the title's own queue: eight submissions a frame, each copying eight 4 MiB buffers into textures, with 25% of the pages rewritten before each:

| mode | ms/frame | sent ahead |
|---|---|---|
| synchronous | 87.0 | 0 |
| `VICTORY_LATE_WAIT=1` | 64.1 | 8 a frame |
| `VICTORY_LATE_WAIT=3` | 61.2 | 8 a frame |
| `VICTORY_RELAY=1` | 63.4 | 8 a frame |

The flights tests (`a7faf52`) pass in all six modes on this machine. A later synchronous run gave 66 ms, so the benchmark varies from run to run. Whether the title's memory behaves like it is what the maintainer's A/B (`ab.bat`) says.

3. **Suballocation** of small copies (each is its own committed resource today), following D3D12MA's model of placed resources in large heaps.
4. **Registry-tracked residency.** Every use known before submission, then `Evict`/`MakeResident` with LRU by submission (D3DX12Residency's model) instead of release-and-remake.

## 6. Gates

- **Before any default changes:** the frame line's `host memory alive`, `residency:` and `INCOMPLETE FRAMES` lines from the maintainer's run.
- **Hot copies and card buffers:** `VICTORY_HOT_WINDOWS_MB` and `VICTORY_CARD_BUFFERS_MIN_MB=1` become defaults only after the maintainer's A/B (`ab.bat`: `baseline`, `best`, `hot1024`, `cardbuf1`, same scene), comparing `ms/frame`, `on the GPU`, `copying back`, `INCOMPLETE FRAMES`, errors and the picture.
- **Fallback:** it caused worse ones, so since `e847242` (2026-11-27) the inherited rule is the default again and `VICTORY_RESIDENCY=1` turns the fallback on (see §7).

## 7. The fallback on a title, and back off by default (2026-11-27)

**Measured on the title** by the maintainer with `ab.bat` on 2026-11-27 (`b5efb43`, fallback on by default, RTX 3070, 1080p). Median of the gameplay frame lines (at least 200 draws per frame, from 120 s on), one run each:

| | 2026-11-26, before `009dd7a` (no fallback) | 2026-11-27, defaults (fallback on) |
|---|---|---|
| Frame | 217.9 ms (4.59 FPS) | 1,438.3 ms (0.70 FPS) |
| CPU preparing | 82.3 ms | 370.8 ms |
| On the GPU | 86.6 ms | 263.7 ms (the title's own passes 224.0 ms) |
| Copying back | 44.7 ms | 286.3 ms |
| Textures in the PC's memory | none | 16,539 (6,269 MiB) |
| Process cores busy | not recorded | 1.23 of 16 |

The two runs are not the same scene or build, so this is not an A/B; but every part of the frame grew several times, with gigabytes of textures in the PC's memory and the card past its target.

**Measured on this machine, synthetic** (`flights::pcie_contention_bench`, 2026-11-27): the GPU reads the PC's memory at 25.2 GB/s against 403 GB/s on the card (16x slower), so a pass that samples textures there takes many times longer. While it does, the CPU's own copy rate (four threads) falls from 26.8 to 23.0 GB/s (-14%); a GPU reading the card does not slow it (+1%). Contention explains the GPU's part, and at most a seventh of the CPU's; the CPU's growth (several times, at about one busy core) looks more like a system short of physical memory, which the frame line did not show then.

**Decisions:**

- The fallback is **off by default** again (`crate::residency::on`): the default returns to the rule measured at 4.59 FPS. `VICTORY_RESIDENCY=1` turns it on (`ab.bat` configuration `resid`).
- With it on, a texture goes to the PC's memory only while 4 GiB of physical memory stay free (`Policy::ram_reserve`, `Refusal::RamLow`): the shared-memory budget the system reports is half of all the PC's memory, not what is free.
- The frame line now carries the PC's memory (in use, free, the system's commit and limit, the process's commit, working set and page faults per second), and the session line the lowest free memory seen, so the next run shows whether the system pages.
- `ab.bat` counts only gameplay frame lines (`-MinDraws`, default 200): the menus' 30 FPS no longer mixes into a configuration's figure.

**Next:** the maintainer's A/B `baseline` against `resid` on the same scene, with the new memory figures. A fallback that pays needs to place only textures the GPU rarely reads, which needs to know which slots draws read; that is not measured yet.
