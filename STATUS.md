# Sixth Star — test status

Last updated: 2026-11-26
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
| Picture | experimental: flickering, missing character textures (observed by the maintainer) |
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
| Output | 1920x1080 |

## Settings

- `run.bat doctor`: settings and dependencies usable; display 1920x1080,
  console DirectStorage HLE (CPU zlib/BCPack), BCPack tables found, ReBAR
  enabled.
- `run.bat rebar`: `rebar: BAR1 8192 MiB / VRAM 8192 MiB`, `rebar: enabled`.
- `DISPLAY_MODE`: 1920x1080 (the default).
- Other non-default settings: `DRED=1` (more detail on a device removal; a
  little slower). `CONSOLE=series-x`, `PRESET=safe`, `SHADER_OPT=0` are the
  defaults.
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
- The runner's frame line (logged every few seconds) splits each frame into
  time spent preparing submissions on the CPU and time on the GPU. In the
  intro cutscene the CPU side is more than twice the GPU side; see "Where the
  frame time goes".
