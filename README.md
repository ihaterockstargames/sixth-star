# Sixth Star

A Windows compatibility layer for Xbox Series X|S titles built with the
Microsoft GDK. The title's own x86-64 code runs natively on the PC. Sixth Star
supplies everything around it:

- the console's system libraries, as high-level replacements;
- the console GPU's shaders, translated to DXIL;
- the console's Direct3D 12 variant, mapped onto desktop Direct3D 12, with DXR for ray tracing.

**No game is included, and none ever will be.** This repository contains only
code and documentation written for it. To run anything you need your own dump
of a title you own, made from your own console. Sixth Star never downloads,
unlocks or decrypts anything.

> Status: experimental. Expect crashes, missing effects and low frame rates.
> The design is described in [docs/architecture.md](docs/architecture.md) and
> [docs/adr/0001-gen2-architecture.md](docs/adr/0001-gen2-architecture.md).

## What you need

| | |
|---|---|
| Windows 10 or 11, x64 | |
| A Direct3D 12 GPU with a current driver | developed on an NVIDIA RTX 3070 |
| [Git](https://git-scm.com/download/win) | to get the code |
| [Rust](https://rustup.rs) | rustup installs the pinned version (1.98.1) by itself |
| [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) | workload "Desktop development with C++", with a Windows 10/11 SDK (its `dxil.dll` is used by the shader tests) |
| Your own dump of a GDK title | the package folder, with `MicrosoftGame.config` and the executable |

## Quick start

1. Get the code:

   ```powershell
   git clone https://github.com/ihaterockstargames/sixth-star.git
   ```

2. Double-click `run.bat` in the `sixth-star` folder. The first time, it creates
   `run.config.bat` and stops.
3. Open `run.config.bat` and fill in two lines:

   ```bat
   set "GAME_DIR=D:\Dumps\MyTitle"
   set "TITLE_EXE=MyTitle.exe"
   ```

   `GAME_DIR` is the folder of your dump: the one with `MicrosoftGame.config`.
   It must be **outside** the `sixth-star` folder (the launcher refuses
   otherwise, so your files can never be committed by accident). `TITLE_EXE`
   is the executable, relative to `GAME_DIR`; it may also be in a subfolder,
   such as `bin\MyTitle.exe`.
4. Check the setup without starting anything:

   ```powershell
   .\run.bat doctor
   ```

5. Start the title:

   ```powershell
   .\run.bat
   ```

   The first start builds Sixth Star, which takes a few minutes. Later starts
   rebuild only what changed. Every run writes a log to `logs\`, and the
   console window stays open at the end so you can read what happened.

### Texture tables (BCPack)

Many titles store textures with BCPack compression. Decoding it needs the
format's constant tables, which are not our work and so are **not** in this
repository. If you have the file `bcpack-tables.bin`, you have two options:

- put it in `crates\victory-formats\src\` (the VIctory-based projects Sixth
  Star descends from publish it at that path; git ignores it there) or next
  to `target\release\victory.exe`;
- or set `VICTORY_BCPACK_TABLES` to its path before starting `run.bat`.

It is read when the title starts, so adding it needs no rebuild.

Without the tables everything else still works, and the log says when a
texture needed them.

## Commands

| Command | What it does |
|---|---|
| `run.bat` | build what changed, then run the title |
| `run.bat nobuild` | run the last build without building |
| `run.bat check` | build, then load and link the title without starting it |
| `run.bat doctor` | check the settings and the tools only |
| `run.bat pipelines` | pre-build every pipeline once from `SHADER_ARCHIVE` (saves stutter later) |
| `run.bat selftest` | test the launcher on synthetic files; no title needed |
| `run.bat tests` | the full test suite (`SIXTH_REQUIRE_GPU=1`: GPU tests must run) |
| `run.bat membench`, `vkbench`, `dxr-proto`, `vkrt` | GPU benchmarks; CSV results go to the log folder |
| `run.bat help` | this list |

`run.bat` exits with the title's own exit code, or 1 when a check or the build
fails. `NOPAUSE=1` stops it from waiting for a key at the end.

## Settings (`run.config.bat`)

[run.config.example.bat](run.config.example.bat) explains each setting.
Paths may be absolute or relative: `GAME_DIR`, `CACHE_DIR`, `DATA_DIR` and `LOG_DIR` relative to the settings file,
and `TITLE_EXE`, `WORK_DIR`, `MANIFEST` and `SHADER_ARCHIVE` relative to
`GAME_DIR`.

| Setting | Default | Meaning |
|---|---|---|
| `GAME_DIR` | (required) | the package root: the console's `G:` drive, where the manifest is |
| `TITLE_EXE` | (required) | the executable |
| `WORK_DIR` | `GAME_DIR` | the title's working directory |
| `MANIFEST` | `GAME_DIR\MicrosoftGame.config` | another manifest |
| `CONSOLE` | `series-x` | `series-s` renders at 720p with FSR and needs `FFX_DIR` |
| `PRESET` | `safe` | `fast` gives lower latency but can be less stable |
| `SKIP_PIPELINES` / `SKIP_LIST` | none | shader hashes to skip, for a driver that never finishes compiling one |
| `DRED` | `0` | `1`: more detail when the GPU device is removed |
| `LOG_DIR` | `logs` | where logs go |
| `CACHE_DIR` | `%LOCALAPPDATA%\victory\cache` | translated shaders and other caches; grows with play, so put it on a drive with room |
| `DATA_DIR` | `%LOCALAPPDATA%\victory\data` | the title's own data (saves, temporary files); when you change it, move the old folder too |

To keep the settings somewhere else, set `SIXTH_CONFIG` to that file.

### How the files are found

The runner keeps four things apart:

- the **executable**;
- the **package root** (`GAME_DIR`, mounted as `G:`, with the manifest);
- the **DLL search path**: the executable's folder first, then the package root;
- the **working directory**.

At startup it prints all four, so a wrong path shows up immediately. The
same can be done by hand:

```powershell
target\release\victory.exe run --package D:\Dumps\MyTitle D:\Dumps\MyTitle\bin\MyTitle.exe
```

That loads and links the title but does not start it; add `--execute` to
start it. `victory.exe run --help` lists the other options.

## Troubleshooting

- **"GAME_DIR is inside the Sixth Star folder"**: move your dump elsewhere.
- **"no cargo on PATH"**: install Rust with rustup, then open a new window.
- **The build fails at linking**: install the C++ build tools and a Windows SDK (see above).
- **Memory grows without end while a shader compiles**: put that shader's
  hash in `SKIP_PIPELINES`.
- **The GPU device is removed**: run again with `DRED=1` and keep the log.

## Contributing

Issues and pull requests are welcome. Never attach or commit game files,
dumps, extracted assets or anything else you did not write. The
[asset policy](docs/asset-policy.md) explains the rule. `cargo xtask policy`
checks it, and after `cargo xtask install-hooks` git runs that check on every
commit.

## Credits and licence

Sixth Star is a fork of [VIctory](https://github.com/Marvisak/VIctory) by
Marvisak, through the forks of [fxceless](https://github.com/fxceless) and
KLBF Lab. It keeps their copyright notices and, like them, is licensed under
the [GPL-3.0](LICENSE). Ideas from other open-source projects (shadPS4, GPURT,
vkd3d-proton, D3D12 Memory Allocator and others) are credited where they are
used.

Sixth Star is not affiliated with or endorsed by Microsoft or any game
publisher. Product names are used only to say what this works with; they
belong to their owners.
