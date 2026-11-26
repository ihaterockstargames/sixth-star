# Sixth Star — test status

Last updated: 2026-11-26 (night)
Build: current main
Binary: sixth-star.exe

Sixth Star is a general Windows compatibility layer for Xbox Series X|S GDK
titles. No game files are included; the user supplies their own dump.

## Current result

| Field | Value |
|---|---|
| Stage | In-game (stage 6): gameplay is reached; **not playable yet** |
| Loading and menus | ~30 FPS (user-reported, approximate) |
| Gameplay | **4.59 FPS** (217.9 ms/frame), measured by the maintainer from the runner's frame line on 2026-11-26 |
| Intro cutscene | 21.5–23.0 FPS (43.6–46.5 ms/frame), measured by the maintainer from the runner's frame line on 2026-11-26 |
| What limits it | in gameplay, **the serial path**: CPU preparing, waiting for the GPU and copying back follow one another (see below); in the intro cutscene, the CPU |
| Stability | in the maintainer's latest session on 2026-11-26 the title ran without a crash; the session's length was not recorded. An earlier run that day froze in the intro cutscene. From the next build the runner logs the exact session length every minute (see Notes) |
| Picture | experimental: flickering, missing character textures (observed by the maintainer). From `009dd7a` textures that found no room on the card are made in the PC's memory instead of being left out; whether that fixes the missing textures is not measured yet |
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
- Texture residency (since `009dd7a`, default on): past nine tenths of the
  VRAM budget, idle textures are given up first and new textures the title
  fills go to the PC's memory, which the GPU reads about 9x slower (synthetic
  measurement below) but which shows them; render targets stay on the card.
  `VICTORY_RESIDENCY=0` restores the old rule (such textures were not made
  while the card was full). Every measurement above predates it.
- Textures back to the card (since `e2453a4`, default on): once the card is
  below 80% of its budget again, textures that went to the PC's memory are
  made again on the card, and the descriptor slots that showed them are
  remade. `VICTORY_PROMOTE=0` keeps them in the PC's memory. Not measured on
  the title yet.
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
- Copy-back noting (since `bb57383`, default on): pages a copy-back wrote
  whole are no longer copied back into the same copy at the next refresh.
  `VICTORY_BACK_NOTE=0` turns it off.
- A local pipeline skip list was active in every 2026-11-26 measurement (see
  Notes). Every figure on this page is from this machine with these
  settings.

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
  the best synthetic frame to 16.5 ms.
- Synthetic, on this machine (not the title): 64 reads of a 2048x2048
  texture took 4.4 ms from the card and 41.1 ms from the PC's memory (9.3x);
  the card filled to 90% (6.08 of 6.72 GiB) placed a title-filled texture in
  the PC's memory and a render target on the card, both read back exactly.
- The runner's frame line (logged every few seconds) splits each frame into
  time spent preparing submissions on the CPU and time on the GPU. In the
  intro cutscene the CPU side is more than twice the GPU side; see "Where the
  frame time goes".
