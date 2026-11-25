# Sixth Star — test status

Last updated: 2026-11-25 23:25 UTC
Build: current main
Binary: sixth-star.exe

Sixth Star is a general Windows compatibility layer for Xbox Series X|S GDK
titles. No game files are included; the user supplies their own dump.

## Current result

| Field | Value |
|---|---|
| Stage | In-game (stage 6): gameplay is reached; **not playable yet** |
| Loading and menus | ~30 FPS (user-reported, approximate) |
| Gameplay | ~5 FPS (user-reported, approximate) |
| Stability | not measured |
| Picture | experimental; effects may be missing |
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
| Driver | 610.88 |
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

## History

| Date | Build | Result | Notes |
|---|---|---|---|
| 2026-11-26 | current main | ~30 FPS loading/menus, ~5 FPS in-game | first public record; user-reported, approximate; 1080p, ReBAR on |

## Notes

- No game files are included or linked.
- The test title is not named in this repository.
- What limits the frame rate is not measured yet. The runner's frame line
  (logged every few seconds) splits each frame into time spent preparing
  submissions on the CPU and time on the GPU; that split is the next
  measurement.
