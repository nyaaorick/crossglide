# Contributing to Crossglide

Thanks for helping. Crossglide is alpha software that runs from source, so the fastest way to help is to try it on your own Mac and PC and report what you see.

## Set up

1. Install [Rust](https://rustup.rs) (the repo pins the toolchain in `rust-toolchain.toml`), [`just`](https://github.com/casey/just), CMake and git. On Windows also install the Visual Studio C++ Build Tools.
2. `git clone https://github.com/nyaaorick/crossglide.git && cd crossglide`
3. `just test` to check that everything builds and passes on your machine.

Building the Windows touchpad driver (only if you change `drivers/touchpad/touchpad.c`) also needs the WDK, which `drivers\touchpad\build.ps1` downloads for you. Installing the prebuilt driver doesn't: use the tray menu.

## Commands

<!-- AUTO-GENERATED from the justfile: edit the recipe comments there, not this table -->
| Command | What it does |
| --- | --- |
| `just` | List recipes |
| `just dev` | Run the agent |
| `just tray` | Build and run the tray app from source, with its log in the terminal |
| `just test` | Run all Rust tests |
| `just lint` | Check formatting and clippy warnings |
| `just spike` | Run the M1 audio spike, e.g. `just spike recv` (see ROADMAP.md) |
| `just deny` | Check dependencies for advisories, bans and unknown sources |
| `just touchpad-driver` | Build, sign and install the virtual precision touchpad driver (Windows, administrator) |
<!-- END AUTO-GENERATED -->

## Before you open a pull request

- [ ] `just lint` passes: `rustfmt` and `clippy`, with warnings as errors
- [ ] `just test` passes, and new logic has a unit test beside it (tests live in a `#[cfg(test)]` module in the same file)
- [ ] `just deny` passes if you added a dependency
- [ ] Docs are updated: [README.md](../README.md) for what users see, [docs/DESIGN.md](DESIGN.md) for how and why, [ROADMAP.md](../ROADMAP.md) for status
- [ ] One change per pull request, with a commit message like `feat(touch): …` or `fix(audio): …`

## Reporting problems

Say which Mac and which Windows build you have, and attach the relevant lines of `agent.log` (in `~/Library/Application Support/crossglide` on the Mac and `%APPDATA%\crossglide` on the PC).
