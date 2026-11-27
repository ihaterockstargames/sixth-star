# Sixth Star — test status

Last updated: 2026-11-27
Build measured: `b5efb43`. Defaults changed since: `e847242` (texture fallback off again, see Settings); not measured on the title yet
Binary: sixth-star.exe

Sixth Star is a general Windows compatibility layer for Xbox Series X|S GDK
titles. No game files are included; the user supplies their own dump.

## Current result

| Field | Value |
|---|---|
| Stage | In-game (stage 6): gameplay is reached; **not playable yet** |
| Loading and menus | ~30 FPS (user-reported, approximate) |
| Gameplay | **~0.7 FPS** (median 1,438.3 ms/frame) with the default settings of `b5efb43`, which had the texture fallback on; measured by the maintainer with `ab.bat` on 2026-11-27 (gameplay frame lines only, from 120 s on, one run). This is **slower** than the 4.59 FPS measured on an earlier build without the fallback on 2026-11-26. From `e847242` the fallback is off by default again; that build is **not measured** on the title yet |
| Intro cutscene | 21.5–23.0 FPS (43.6–46.5 ms/frame), measured by the maintainer from the runner's frame line on 2026-11-26 |
| What limits it | on `b5efb43` in gameplay: CPU preparing (370.8 ms), the GPU (263.7 ms, of which 224.0 ms the title's own passes) and copying back (286.3 ms), in series, with about 6 GiB of textures in the PC's memory because the card was past its target. The GPU reads the PC's memory 16x slower than its own (synthetic, see Notes) |
| Stability | **not measured yet.** A 33-minute stability run on `b5efb43` on 2026-11-27 (with the `best` switches of `ab.bat`) ended by itself after a session of about 6 minutes (the session line's 6 min 3 s; exit code 1; no error, device removal or panic in the runner's lines); the maintainer saw it as a crash. In the A/B runs that followed, no run reported an error, a device removal or a panic. An earlier run on 2026-11-26 froze in the intro cutscene |
| Picture | experimental: a flickering intro, sometimes missing textures in gameplay (user-reported on 2026-11-27, `b5efb43`: "textures better, I think", still missing at times) |
| Output | 1920x1080 |
| Resizable BAR | enabled |
| DirectStorage / BCPack | console DirectStorage HLE active; BCPack tables found |

These are one machine's observations, not a benchmark. Other GPUs, drivers and
titles may behave differently. "Playable" here means 20 FPS or better for 30
minutes or more without a crash; that has not been reached or measured.

## Machine

| Field | Value |
|---|---|
| GPU | NVIDIA GeForce RTX 3070 8 GB |
| Driver | 610.88 (user-mode driver 32.0.16.1088, read from the device on 2026-11-26) |
| CPU | AMD Ryzen 7 5800X |
| OS | Windows 11 Pro 26100, UEFI |
| Resizable BAR | enabled (BAR1 8192 MiB / VRAM 8192 MiB) |
| VRAM budget for the process | 7.08 GiB (what Windows gives the process; read from the device on 2026-11-26); 6.72 GiB later that day with other programs open: it depends on what else runs |
| Output | 1920x1080 |

## Settings

- `run.bat doctor`: settings and dependencies usable; display 1920x1080,
  console DirectStorage HLE (CPU zlib/BCPack), BCPack tables found, ReBAR
  enabled.
- `run.bat rebar`: `rebar: BAR1 8192 MiB / VRAM 8192 MiB`, `rebar: enabled`.
- `DISPLAY_MODE`: 1920x1080 (the default for the measurements below). Since
  `77f603b` the runner also has `OUTPUT_MODE` (empty: the window follows the
  image) and `RENDER_SCALE` (1.0); every measurement so far is native
  1080p, render scale 1.0, no upscaling. A run at another scale is not
  measured yet.
- Other non-default settings: `DRED=1` (more detail on a device removal; a
  little slower). `CONSOLE=series-x`, `PRESET=safe`, `SHADER_OPT=0` are the
  defaults.
- Texture fallback to the PC's memory (`009dd7a`; **off by default since
  `e847242`**): with `VICTORY_RESIDENCY=1`, past nine tenths of the VRAM
  budget idle textures are given up first and new textures the title fills
  go to the PC's memory, which the GPU reads about 16x slower than its own
  (synthetic, see Notes) but which shows them; render targets stay on the
  card; and since `e847242` only while 4 GiB of the PC's physical memory stay
  free. It was on by default from `009dd7a` until `e847242`: the 2026-11-27
  A/B ran with it on and was several times slower than the 2026-11-26
  measurement without it, so the default went back to the old rule (such
  textures are not made while the card is full, and their slots wait). The
  2026-11-26 measurements predate it.
- Textures back to the card (since `e2453a4`): once the card is below 80% of
  its budget again, textures that went to the PC's memory are made again on
  the card, and the descriptor slots that showed them are remade.
  `VICTORY_PROMOTE=0` keeps them in the PC's memory. It only acts with the
  fallback on. Not measured on the title yet.
- The frame line carries the PC's memory since `e847242` (in use, free, the
  system's commit and its limit, the process's commit, working set and page
  faults per second), and the session line the lowest free memory seen; no
  run has them yet.
- Hot copies on the card (since `58a30ed`, one rule for windows and
  title-buffer copies since `274f873`, **off by default**):
  `VICTORY_HOT_WINDOWS_MB=1024` moves host copies that are used frame after
  frame from the PC's memory to the card, up to that many MB and only while
  the card has room. Title-buffer copies of 1 MiB or more that shaders write
  go to the card when they are made with `VICTORY_CARD_BUFFERS_MIN_MB=1`
  (also off by default; the default stays 16 MiB). Both are A/B switches:
  in a synthetic frame they make it about 6 times faster (see Notes), but
  they are not measured on the title yet. `ab.bat` runs the defaults and
  them (`best`) one after the other and writes a comparison report.
- Hot copies give way (since `ecdf692` and `252f632`): when the card fills,
  hot copies go back to the PC's memory before textures have to, the least
  used first; with the budget full, copies draws use much more take the
  place of ones they use little. `VICTORY_HOT_WINDOWS_MB=auto` limits hot
  copies only by the card's room (`ab.bat` `bestauto`).
- Texture copies from a buffer's host copy (since `78d67bc`, **off by
  default**): `VICTORY_TEXTURE_FROM_COPY=1` makes a title copy from a buffer
  into a texture read the buffer's host copy, which is kept up to date page
  by page, instead of filling a fresh upload buffer from title memory with
  every byte, every time. Part of `ab.bat`'s `best`. Measured once on the
  title on 2026-11-27 (see the A/B below).
- Wide fills (since `ae1ceaf`, **default on**): a command list's texture
  copies from title buffers in a row are done on several threads, in 1 MB
  pieces, before the next step of another kind. `VICTORY_WIDE_FILLS=0`
  turns it off. Measured only on the stand-in title (see "Automated runs");
  not measured on the test title yet.
- Copy-back noting (since `bb57383`, default on): pages a copy-back wrote
  whole are no longer copied back into the same copy at the next refresh.
  `VICTORY_BACK_NOTE=0` turns it off.
- A local pipeline skip list was active in every 2026-11-26 measurement (see
  Notes). Every figure on this page is from this machine with these
  settings.

## A/B on the title, 2026-11-27

Measured by the maintainer with `ab.bat` on 2026-11-27, build `b5efb43`
(texture fallback on, the default then), RTX 3070, 1080p, ReBAR on,
`DRED=1`, the local skip list active. One run each, about 5 minutes each.
Median of the gameplay frame lines: from 120 s after the first frame line,
and only lines with at least 200 draws per frame (menus and loading draw
far fewer; `ab.bat` counts this way since `e847242`, and the logs were read
again that way).

| Measure | baseline (defaults) | `VICTORY_TEXTURE_FROM_COPY=1` |
|---|---|---|
| Gameplay frame lines counted | 19 of 42 | 27 of 49 |
| FPS (median) | 0.70 | 0.90 |
| ms/frame (median) | 1,438.3 | 1,076.4 |
| Worst frame line, ms | 12,383.4 | 4,870.2 |
| CPU preparing, ms | 370.8 | 330.6 |
| On the GPU (fence wait), ms | 263.7 | 211.5 |
| GPU timestamps: the title's work, ms | 224.0 | 186.3 |
| Copying back, ms (MB) | 286.3 (45.7) | 237.9 (64.9) |
| Draws per frame; dropped without a pipeline | 3,063; 778 | 1,649; 246 |
| Process cores busy | 1.23 | 1.09 |
| VRAM used of budget, GiB | 5.50 of 6.23 | 5.53 of 6.34 |
| Textures in the PC's memory for lack of room on the card | 16,539 (6,269 MiB) | 14,280 (4,917 MiB) |
| Errors / device removed / panics | 0 / 0 / 0 | 0 / 0 / 0 |

- The runner's session averages for these runs (11.54 and 7.48 FPS) count
  the menus at ~30 FPS as well; they are **not** gameplay figures.
- `VICTORY_HOT_WINDOWS_MB=1024` and the card-buffer switches were also
  started, but the maintainer closed those runs before gameplay (the 30 FPS
  in the `hot1024` run is the menus: 13 draws per frame; read with the
  gameplay filter, neither run has a gameplay frame line). They are **not
  measured** on gameplay.
- The stability run before them (`best`: card buffers from 1 MiB, hot
  copies 1024 MB, texture copies from host copies; fallback on) had 25
  gameplay frame lines at a median of 5,483.2 ms/frame (0.20 FPS), with the
  process keeping 7.25 cores busy and 1,443 draws per frame dropped while
  their pipelines were made; preparing (176.2 ms), the GPU (162.2 ms) and
  copying back (70.7 ms) add up to a small part of that frame.
- Compared with the 2026-11-26 gameplay figures (82.3 ms preparing, 86.6 ms
  on the GPU, 44.7 ms copying back), every part of the frame is several
  times larger, and the card was past its budget with gigabytes of textures
  in the PC's memory, which the GPU reads over PCIe at about a sixteenth of
  its own memory's rate (synthetic, see Notes). That explains the GPU's
  part; the CPU's parts grew too, at about one busy core, which the GPU's
  reads explain only in small part (they slow the CPU's copies by 14%). The
  fallback is off by default again since `e847242`; whether that brings the
  frame back is **not measured yet**, nor whether the PC ran short of
  physical memory (the frame line shows that since `e847242`). The scene may
  also have been heavier than on 2026-11-26.

## Automated runs of the runtime (stand-in title, no game)

Since `a6ab611`, `autotest.bat` tests the runtime without anyone at the
machine. Each cycle builds, runs the workspace's tests and the synthetic
benchmarks, then runs a stand-in title through the real `sixth-star.exe` in
six configurations. The stand-in is `tests\fixtures\test-title`, written
for this project: it uses no game code or data. Every run is watched for
hangs and crashes, and every saved frame is checked pixel by pixel against
the known picture.

These runs are **not** measurements of the test title. They show that the
runtime's path works end to end (loader, imports, the console's graphics
interfaces, title memory, copies, present), and what each configuration
costs on it.

Cycle 2, 2026-11-27, `d8eb853` plus the runner (committed as `a6ab611`),
RTX 3070:

- Build ok. Tests: 669 passed, 0 failed, 21 ignored.
- `synthetic_ab` (150 frames), every byte right:
  - game-shaped frame: `baseline` 119.9 ms, `best` 18.1 ms, `bestlate` 18.4 ms, `hot1024` 78.2 ms, `cardbuf1` 55.0 ms;
  - streaming: `baseline` 65.3 ms, `bestlate` 13.2 ms.
- The stand-in title, 45 s per configuration, 8 buffers of 3 MB rewritten and copied each frame. Each run took about 2,690 frames, ended with exit code 0 and needed no restart, and 4 frames of each were checked and right. The window is held to 60 Hz, so the runtime's own costs are what tell the configurations apart:

| Configuration | CPU preparing, ms per frame | GPU work, ms per frame (timestamps) | Picture |
|---|---|---|---|
| `baseline` | 2.2 | 1.0 | right |
| `resid` | 2.2 | 1.0 | right |
| `texcopy` | 2.7 | 1.0 | right |
| `hot1024` | 2.1 | 1.0 | right |
| `cardbuf1` | 2.1 | 1.0 | right |
| `best` | 2.7 | 1.0 | right |

On this workload, copying textures from the buffers' host copies
(`texcopy`, part of `best`) costs about 0.5 ms more CPU per frame than
the default.

Cycles 4 to 8, 2026-11-27 (night runs):

- Cycle 4 (`5b80b92`): one test failed now and then (the hot-copy swap
  test ran out of frames when other tests had left hot copies on the
  card). Fixed in the test (`0875a24`); cycle 6 passed 672 tests.
- `6133e9d`: with `VICTORY_WIDE_REFRESH=1`, a list's texture copies from
  title buffers (fills) run on several threads. Before, the switch never
  applied to them. Stand-in title, 16 buffers of 3 MB copied a frame
  (49.2 MB), two runs each:

| Configuration | CPU preparing before, ms | after, ms | Picture |
|---|---|---|---|
| `baseline` | 4.2 | 4.2 | right |
| `wide` | 4.2 | 2.7 (both runs) | right |

  The switch stays **off by default**: it has not been measured on the
  test title.

Cycles 9 to 17, 2026-11-27 (night run):

- Cycle 9 (`5249822`): all green, 672 tests passed.
- `6334ca1`: the fills got a switch of their own, `VICTORY_WIDE_FILLS`
  (`ab.bat` configurations `fills` and `nofills`). `baseline` against
  `fills`, CPU preparing in ms, stand-in title:

| Buffers of 3 MB copied a frame | `baseline` | `fills` | Picture |
|---|---|---|---|
| 16 (two runs) | 3.8 / 3.9 | 2.6 / 2.6 | right |
| 8 | 2.0 | 1.4 | right |
| 2 | 0.5 | 0.4 | right |

- `ae1ceaf`: **wide fills are on by default** (`VICTORY_WIDE_FILLS=0`
  turns them off). Cycle 15 (full, 8 buffers): all green, 673 tests
  passed, every synthetic byte right, 6 of 6 configurations with the right
  picture; `baseline` CPU preparing 2.2 -> 1.4 ms, GPU work 1.0 ms
  unchanged. Then the new default against `nofills`: 8 buffers 1.4 against
  2.1 ms, 16 buffers 2.7 against 4.2 ms. Fills of less than 2 MiB in all
  stay on the submitting thread. Not measured on the test title yet.

Cycles 18 to 20, 2026-11-27 (night run):

- Cycle 18 (full, `b839955`): all green, 673 tests passed, every
  synthetic byte right, 6 of 6 configurations with the right picture.
- `549c761` (the runner): the comparison with the previous cycle had
  compared nothing since the runner began. A list of one came back from
  the saved state as its single item, so the last cycle's run was
  never found. It compares again, a resumed cycle keeps its finished
  runs, and named configurations (`name:VAR=v`) run.
- Measured, not a default: `texcopy` with `VICTORY_WIDE_REFRESH=1`
  (refreshes on several threads), 8 buffers, CPU preparing in ms:

| Configuration | Run 1 | Run 2 | GPU work, ms | Picture |
|---|---|---|---|---|
| `texcopy` | 2.6 | 2.7 | 1.0 | right |
| `texcopy` + `VICTORY_WIDE_REFRESH=1` | 2.0 | 2.1 | 1.0 | right |

  Both switches stay off by default: the refreshes of the test title are
  not measured with them.

## Where the frame time goes (2026-11-26, gameplay)

Gameplay, measured by the maintainer from the runner's frame line on 2026-11-26:

| Measure | ms per frame |
|---|---|
| Frame time | 217.9 (4.59 FPS) |
| CPU preparing submissions | 82.3 |
| Waiting for the GPU ("on the GPU") | 86.6 |
| Copying back to title memory | 44.7 |

The three add up to 213.6 of 217.9 ms: they run one after another, with no
overlap. "On the GPU" is the wall time from handing the lists to the queue
until the fence is reached, so it also holds our own uploads and compares on
the card and the gaps between submissions; how much of it is the title's own
rendering is not measured yet (per-pass GPU timestamps are the next
measurement). Removing the CPU parts alone would not reach the 25 ms
(40 FPS) target; the GPU part must be taken apart as well.

## Where the frame time goes (2026-11-26, intro cutscene)

Intro cutscene, measured by the maintainer from the runner's frame line and
card-clock line on 2026-11-26
(build `3bc7b56`, 1080p, ReBAR on, `PRESET=safe`, `DRED=1`). Two frame lines
and about a dozen card-clock lines, over about 15 seconds:

| Measure | Value |
|---|---|
| Frame time | 43.6–46.5 ms (21.5–23.0 FPS) |
| CPU preparing submissions | 26.4–27.1 ms per frame; most of it is bringing copies of title memory up to date (about 32 MB per frame) |
| On the GPU | 10.7–12.1 ms per frame |
| Copying back to title memory | 4.4–4.7 ms, about 7.5–7.8 MB per frame |
| GPU busy, by the runner's count | about 23–27%; 28–32 ms of each frame the GPU is idle while the submitting thread works |
| Draws | 97 per frame |
| Textures picked by index | about 12,400–12,600 waiting, about 17,800 passed over as unmapped |

So the frame is **CPU-bound**: one submitting thread sets the pace and the
GPU waits for it. Making the copies themselves faster was measured and does
not help (moving the bytes is a few ms; finding what changed is the cost).
The plan removes that work instead: see `docs/adr/0002-structural-roadmap.md`.
Task Manager figures were not taken for this run.

## History

| Date | Build | Result | Notes |
|---|---|---|---|
| 2026-11-26 | current main | ~30 FPS loading/menus, ~5 FPS in-game | first public record; user-reported, approximate; 1080p, ReBAR on |
| 2026-11-26 | current main (build not recorded) | ran without a crash | latest session, observed by the maintainer; length not recorded |
| 2026-11-26 | current main (build not recorded) | gameplay 4.59 FPS (217.9 ms/frame) | measured by the maintainer from the runner's frame line on 2026-11-26: 82.3 ms CPU preparing, 86.6 ms waiting for the GPU, 44.7 ms copying back, in series |
| 2026-11-27 | `a6ab611` | autonomous test runner (`autotest.bat`) and a stand-in title through the runtime: 6 of 6 configurations ran and showed the right picture; 669 tests passed | automated, on this machine; not the test title |
| 2026-11-27 | `6133e9d` | stand-in title, 16 copies: CPU preparing 4.2 -> 2.7 ms with `VICTORY_WIDE_REFRESH=1` (fills on several threads); default unchanged; a flaky test fixed (`0875a24`) | automated night run, on this machine; not the test title |
| 2026-11-27 | `ae1ceaf` | wide fills on by default (`VICTORY_WIDE_FILLS=0` off): stand-in title, 8 copies, CPU preparing 2.2 -> 1.4 ms; 16 copies 4.2 -> 2.7 ms; same GPU work and picture; 673 tests passed | automated night run, on this machine; not the test title |
| 2026-11-27 | `549c761` | the runner's comparison with the previous cycle works again (it had compared nothing); stand-in title, 8 buffers: `texcopy` CPU preparing 2.6 / 2.7 -> 2.0 / 2.1 ms with `VICTORY_WIDE_REFRESH=1`; defaults unchanged | automated, on the stand-in title (no game) |
| 2026-11-27 | `e847242` | texture fallback off by default again; the PC's memory on the frame line; `ab.bat` counts gameplay frame lines only | not measured on the title yet |
| 2026-11-27 | `b5efb43` | stability run (`best` switches): ended by itself after a session of about 6 min (exit code 1), gameplay ~0.2 FPS | from the runner's lines; no error, device removal or panic logged; seen as a crash by the maintainer |
| 2026-11-27 | `b5efb43` | gameplay ~0.7 FPS (median 1,438.3 ms/frame) with the defaults (fallback on); ~0.9 FPS (1,076.4 ms/frame) with `VICTORY_TEXTURE_FROM_COPY=1` | measured by the maintainer with `ab.bat` (gameplay frame lines only, from 120 s on, one run each); slower than 2026-11-26; no errors, device removals or panics; sometimes missing textures (user-reported) |
| 2026-11-26 | `3bc7b56` | intro cutscene 21.5–23.0 FPS, then the picture froze | measured by the maintainer from the runner's frame line on 2026-11-26: ~27 ms CPU preparing, ~11 ms GPU, GPU ~25% busy (CPU-bound); flickering, missing character textures and a slow start-up observed by the maintainer |

## Notes

- No game files are included or linked.
- In the 2026-11-26 runs a local pipeline skip list was active (a debugging
  aid kept outside the repository): the intro frame line shows 1 draw or
  dispatch per frame not run. Later measurements say whether it was on.
- The test title is not named in this repository.
- From commit `78d4389` on, the runner writes a `session:` line every
  minute: time since the first present, frames, exact average FPS, the worst
  second, the longest time between two presents, seconds in which draws were
  not run, the GPU and driver, the display mode and the `VICTORY_*` settings
  (without paths). A freeze gets a `watchdog:` line with the seconds since the
  first present. Those lines are the source for session length and stability
  here.
- From `c4a274f` on, the runner also logs per frame line: GPU time from
  timestamps in the command lists (the title's passes by kind, our own work,
  the busy span against the time waited for the fence), CPU time by kind of
  work and by thread, copy-back and refresh bytes by the console's memory
  kind, and `INCOMPLETE FRAMES` whenever draws were not run; and once, a
  `startup:` line at the first present. From `7868402` on, also the VRAM in
  use against that budget, texture copies refused for lack of room, and how
  many cores the process and the submitting thread keep busy. None of them
  adds a wait. The next measurement on this page comes from them.
- From `009dd7a` on, two more lines: `host memory alive` (every host
  resource alive now, by what it is for and whether it is on the card or in
  the PC's memory, from the device's own destruction notices) and
  `residency:` (pressure, textures made in the PC's memory, idle textures
  given up, passes with no room in either pool). See
  `docs/adr/0003-residency.md`. From `e2453a4` and `58a30ed` the `residency:`
  line also counts textures brought back to the card, the slots remade for
  them, and windows moved to the card.
- Synthetic, on this machine (not the title; 2026-11-26, through the
  runtime's own code, ADR 0003 §5): in a frame shaped like the game's
  measured one (1 GiB read-only buffers read whole, 2% rewritten; 640 MiB of
  writable buffers, 2% written by shaders; lists kept two frames), the
  defaults take 109 ms (40 preparing, 42 on the GPU, 28 copying back);
  `VICTORY_HOT_WINDOWS_MB=1024` with `VICTORY_CARD_BUFFERS_MIN_MB=1` takes
  18.5 ms (8, 6 and 4). Reading 2 GiB of copies from the PC's memory took
  83 ms of GPU time, from the card 8 ms. The late wait (`VICTORY_LATE_WAIT`)
  and the relay pass new tests with submissions in flight in all six modes
  on this machine. With one submission a frame they gain nothing; with
  eight submissions a frame through the title's queue they took the frame
  from 87 to 61-64 ms (`late_bench`). Card-merge noting (`1f735a8`) brings
  the best synthetic frame to 16.5 ms. In `late_bench`, reading texture
  copies from the buffers' host copies (`VICTORY_TEXTURE_FROM_COPY=1`) took
  the frame from 70.6 to 24.7 ms, and to 18.1 ms with the late wait on top.
- Synthetic A/B of `ab.bat`'s configurations (2026-11-26, `synthetic_ab`,
  on this machine, **not the title**): the same switches as `ab.bat`, over
  a game-shaped frame and texture streaming with submissions in flight,
  3,000 and 6,000 frames each, every byte checked, memory flat. Median
  (95th percentile) ms per frame: defaults 113.2 (121.3) and 63.6 (70.9);
  `best` 16.9 (19.3) and 15.4 (17.8); `bestlate` 16.7 (19.5) and 12.4
  (14.7). Whether the title gains the same is not measured.
- Correctness tests added on 2026-11-26 (synthetic, on this machine, all
  passing):
  - differential tests: whatever host copy the GPU reads (the PC's memory,
    the card, the old copy a descriptor slot still shows), it reads the
    title's bytes, including copies shaders write and title writes into
    the ranges they write;
  - a memory-pressure test: with the card filled to 94% of its budget, a
    new texture is still made (in the PC's memory) and nothing is refused;
    after the release the texture comes back to the card and hot copies
    move;
  - submissions in flight in all six late-wait and relay modes;
  - soaks: 4,000 frames (32,000 submissions in flight) of texture copies
    with hot copies under the late wait and the relay, and 1,500 frames
    of the game-shaped frame: no device lost, the host's memory alive
    flat (or shrinking as old copies are let go), every byte checked.
- The scenarios above also ran in a diagnostics build under the D3D12
  debug layer, and some under GPU-based validation
  (`VICTORY_D3D12_DEBUG=1`, `VICTORY_D3D12_GBV=1`): no errors reported
  (`ae0e444`). An AMD run on another machine lost the device with the
  late wait and card copies; that is not explained by an API error these
  checks find, and it is not tested on AMD here.
- Texture promotion waits two frames after the new copy's
  upload before a descriptor slot shows it (`94f5b63`): a list recorded
  earlier and submitted later could otherwise have sampled it unfilled.
- Synthetic, on this machine (not the title): 64 reads of a 2048x2048
  texture took 4.4 ms from the card and 41.1 ms from the PC's memory (9.3x);
  the card filled to 90% (6.08 of 6.72 GiB) placed a title-filled texture in
  the PC's memory and a render target on the card, both read back exactly.
- Synthetic, on this machine (not the title; 2026-11-27,
  `pcie_contention_bench`): the GPU reads 1 GiB in the PC's memory at
  25.2 GB/s and 1 GiB on the card at 403 GB/s (16x). While it reads the
  PC's memory, the CPU's own copies (four threads) run at 23.0 GB/s instead
  of 26.8 (-14%); while it reads the card, at 27.1 (+1%).
- Synthetic, on this machine (not the title; 2026-11-27,
  `pipeline_contention_bench`, driver 610.88): making pipelines from shaders
  the driver has not seen, on 2, 4 or 8 threads at the workers' priority
  (50-72 ms each), does not hold up the thread that records and submits:
  record and submit took 0.043 ms (median) alone and 0.051 ms with 8 makers,
  1.0 ms at worst. The 5.5 s gameplay frames of the stability run are not
  explained by the driver locking submission while pipelines are made.
- Texture promotion acts only with the fallback on (off by default since
  `e847242`).
- The runner's frame line (logged every few seconds) splits each frame into
  time spent preparing submissions on the CPU and time on the GPU. In the
  intro cutscene the CPU side is more than twice the GPU side; see "Where the
  frame time goes".
