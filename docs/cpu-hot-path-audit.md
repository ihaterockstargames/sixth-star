# CPU hot path audit (2026-11-26)

From the title's graphics calls to the GPU and back, as the code stands at `0dfdfa1`, with each step's reason, whether that reason still holds, and what replaces it. Figures are the maintainer's frame-line measurements of 2026-11-26 (`STATUS.md`) and the isolated benchmark in `host.rs` (`plain_copy_benchmark`). The new instruments (`gpuprof`, `cpuprof`, `census`, `session`, `watchdog`) exist to put a number on every row in the maintainer's next run. Rows without a number say so.

## The chain

**On the title's threads, as it records** (`list.rs` → `draw.rs`):

| Step | Per | What it does | Why it exists | Still valid? | Replacement |
|---|---|---|---|---|---|
| Translate each list call | call | records a host command; binds root arguments; resolves addresses to host copies (`shadow::find`) | the title's lists use raw addresses | yes, but the lookups go through several maps | one registry lookup (`sixth_memory::registry`, ADR 0002 §4.3) |
| Make descriptors for bound views | draw/dispatch | creates host views on the host copy that holds the bytes | the title writes console descriptors into its own memory | yes | views keyed by registry version; remade on map events, not per draw |
| Heap passes (`heap_buffers`, `heap_textures`) | draw that picks by index | walks the view heap's buffer views and the texture queue | shaders pick views by index from a heap of thousands | the walk, no: it re-finds what changed | event-driven slot invalidation (registry generations); 7–9 ms per frame in the intro |

**On the submitting thread, per submission** (`host.rs` `Host::submit`):

| Step | What it does | Why it exists | Still valid? | Replacement |
|---|---|---|---|---|
| `sweep` | lets go of idle objects (at most once a second) | upkeep | yes | stays; off the critical path |
| `make_waiting` | makes views that waited for a texture's host copy | textures appear after their views | no: a symptom of polling | registry map/create events |
| `ask_once` / `ask_ahead` | asks the write watch about every range the submission refreshes | to find which title pages the CPU wrote | only for CPU-written memory | contract-scoped watch: only regions whose contract says the CPU writes them (`sixth_memory::timeline::cpu_wrote`) |
| `run_deferred` → `Batch::flush` | brings host copies up to date range by range: watch answer, then copy the stale pages | the GPU reads host copies, not title memory | the copying, partly: moving bytes is cheap (32 MB ≈ 2.7 ms through the kernel); finding them is the cost | forward only the CPU's newest bytes that a submission reads (`timeline::submit` → `forward`); `sixth_scheduler::batch` places them before their first reader |
| Texture refreshes (`Step::Texture`) | hashes a texture's memory, detiles and uploads it if changed | textures may change under the title's writes | the whole-texture hash, no | upload on registry events and CPU-write ranges; detile only what changed |
| `take_snapshots` | copies every range shaders may write into a "before" buffer | to tell later which bytes shaders wrote | no: the GPU's writes are the GPU's, and the timeline knows which submission wrote what | timeline ownership; D4 merge only for late CPU writes to GPU pages |
| `card_before` | sends changed parts of copies kept in VRAM up, adds a compare pass | copies in the card's memory need the same change detection | no | GPU-owned VRAM residency by contract (§4.2) |
| `ExecuteCommandLists`, then **wait** (`WaitForSingleObject`, `INFINITE`) | runs the submission and waits for its fence before anything else | copy-back needs the GPU's results, and the next submission's snapshots need settled copies | the wait for everything, no | per-range waits (`timeline::cpu_access` waits only for submissions that touch the bytes); asynchronous submission (§4.4) |
| `card_back` / `hand_back` | compares host copies with their snapshots, copies differing bytes back into title memory (kernel writes, guards opened) | on the console the CPU sees the GPU's writes in place | only for bytes the CPU will read | demand fetch for guarded ranges; eager only where no guard is possible (`timeline::retire`) |
| Read-backs, bottom-of-pipe writes | the values that tell the title the GPU is done | the title's fences and memory values | yes | ordered on the timeline after the copy-back they cover |

## What is repeated that need not be

1. **Every submission asks about all refreshed ranges**, whether or not the CPU can have written them. The write watch walks page tables; the inherited fixes made the asking smaller (once per submission, per reservation), never unnecessary.
2. **Every submission snapshots every range shaders may write**, then compares all of it after the wait: twice the declared bytes moved and compared to find usually much fewer written ones.
3. **Every submission waits for the whole GPU**, so the CPU and the GPU never work at once (82.3 + 86.6 + 44.7 = 213.6 of 217.9 ms in gameplay).
4. **Every frame re-walks the view heap** to find slots whose texture arrived, because mapping changes are not events.
5. **Whole textures are hashed** to see whether they changed.

## What becomes unnecessary, in the order the replacement lands

| Replaced by | Removes |
|---|---|
| Registry events (`sixth_memory::registry`) | `make_waiting`, heap-texture polling and rechecks, `STALE` bookkeeping, the fault-filled raw-address directory |
| Timeline ownership and demand fetch (`sixth_memory::timeline`) | snapshots, compare-on-copy-back, eager copy-back for guarded ranges, card compares |
| Contract-scoped write tracking + forwarding | write-watch askings over GPU-only memory, refreshing copies nobody wrote |
| Scheduler (`sixth_scheduler`) | the per-group fence wait on the critical path; whole-buffer state guessing (`BufferStates`) |

## What to measure next (all instruments are in the build now)

- `GPU timestamps per frame`: how much of the 86.6 ms is the title's passes, our work, and idle time.
- `CPU time per frame in measured work`: write watch, guards, refresh, textures, compares (snapshots), descriptors, recording, copy-back, upkeep; the busiest threads.
- `Gen 2 census`: copy-back and refresh bytes by the console's memory kind. If most copy-back lands in write-combined memory, demand fetch there is a declared contract (ADR 0001 D3), the first class to move.
- `session:` every minute; `watchdog:` on a stall; `INCOMPLETE FRAMES` whenever draws were not run.
