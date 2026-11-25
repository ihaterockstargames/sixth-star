# Contributing to Sixth Star

Thanks for helping. Sixth Star is an experimental compatibility layer, so small,
tested changes are much easier to review than large rewrites.

## Ground rules

- **Never commit or attach game files, dumps, extracted assets, keys, or logs
  that contain paths to them.** The [asset policy](docs/asset-policy.md)
  explains the rule; `cargo xtask policy` enforces it.
- Keep a change focused. Separate refactors from behaviour changes.
- Explain what you changed, why, and what you ran to test it.
- If a change affects rendering or performance, include the GPU, driver,
  Windows version, and before/after log lines.

## Before you start

You need:

- Windows 10 or 11, x64;
- [Rust](https://rustup.rs) — rustup reads `rust-toolchain.toml` and installs
  the pinned 1.98.1 toolchain;
- [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/vs/community/)
  with the "Desktop development with C++" workload and a Windows 10/11 SDK.

You do **not** need a game dump to build the project or run its tests.

## Build and test

```powershell
git clone https://github.com/ihaterockstargames/sixth-star.git
cd sixth-star
.\run.bat doctor
.\run.bat tests
cargo xtask policy
```

`run.bat tests` covers the launcher, the loader, the shader translator and the
GPU layer. `SIXTH_REQUIRE_GPU=1` makes the GPU tests mandatory instead of
skipping them when no suitable adapter is present.

Install the repository's git hooks once, so the asset policy runs before every
commit:

```powershell
cargo xtask install-hooks
```

## Pull requests

A good pull request:

1. says what problem it solves, in one or two paragraphs;
2. keeps unrelated changes out;
3. lists the commands you ran and their result;
4. includes a short log excerpt for crashes or rendering changes;
5. does not contain copyrighted content or personal paths.

Draft pull requests are fine if you want early feedback.

## Reporting a bug

Use the issue templates. Include:

- GPU and driver version;
- Windows version;
- what you ran and what happened;
- the first error in `logs\`, not the last line;
- a screenshot only if it contains no copyrighted material.

## Licence

By contributing, you agree that your contribution is licensed under the
GPL-3.0-or-later, the same licence as the project.
