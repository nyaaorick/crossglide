<div align="center">

# Crossglide

**Use your MacBook's trackpad, keyboard and speakers with your Windows PC.**

Glide the pointer off the edge of your Mac's screen and it lands on the PC, driven by the real MacBook trackpad.
The PC's audio plays out of the Mac. All of it runs over one encrypted connection on your local network, and it's written in Rust.

[![CI](https://github.com/nyaaorick/crossglide/actions/workflows/ci.yml/badge.svg)](https://github.com/nyaaorick/crossglide/actions/workflows/ci.yml)
[![License: GPL v2](https://img.shields.io/badge/license-GPL--2.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-2024-orange.svg)
![Status](https://img.shields.io/badge/status-alpha-yellow.svg)

</div>

---

## Why Crossglide

Software KVMs move a mouse pointer. Crossglide moves a **touchpad**.

- **A real precision touchpad on Windows.** The Mac's raw finger contacts are sent to a small virtual touchpad driver on the PC. Windows sees a genuine Precision Touchpad and does its own gesture recognition: two-finger scroll, pinch to zoom, three- and four-finger swipes, tap to click. There's no gesture code to maintain or get wrong.
- **Your Mac's sound, from the PC.** The PC's system audio is captured, encoded with Opus and played on the MacBook's speakers, with an adaptive jitter buffer and clock-drift correction. Measured **41–76 ms** end to end on Wi-Fi.
- **Switch by gliding.** Push the pointer through a screen edge to hand control to the PC, and a frosted-glass strip on that edge tells you where it went. A hotkey (`ctrl+option+cmd+space`) switches either way, always.
- **One private connection.** Everything travels over QUIC with pinned certificate fingerprints: touch as low-latency datagrams, keys on a reliable stream, audio as datagrams. No cloud, no account.
- **A tray app on both machines.** Status at a glance, an audio switch, *Start at login*, and on Windows a one-click **Install touchpad driver**.
- **Rust all the way down** (apart from the ~500-line Windows driver, which has to be C for now).

## Status

Crossglide is **alpha**, and it's built to run from source for now: there are no installers or releases yet.

| Feature | Status |
| --- | --- |
| Encrypted side channel (QUIC, pinned fingerprints, reconnect, clock sync) | Working |
| PC audio → Mac speakers | Working. Long-run drift and Ethernet numbers still to be measured |
| Tray app: status, audio switch, start at login | Working |
| Trackpad → virtual Windows Precision Touchpad | Working: pointer, switching by edge and hotkey, keyboard. Full gesture set being tested |
| Edge hint (frosted-glass strip) | Built, being tested |
| Install touchpad driver from the tray (Windows) | Working, checked on the PC |
| Pairing code (fingerprints are copied by hand today) | Planned |
| Clipboard sharing | Not started |
| Logs and config in one place, MCP server | Planned |

Tested on a MacBook Air (macOS 26) and a Windows 11 PC (build 26200), both on the same Wi-Fi network. The [roadmap](ROADMAP.md) has the measurements and what's next.

## Quick start

You need two machines on the same network: a **MacBook** and a **Windows 11 PC**.

**On both machines**

1. Install [Rust](https://rustup.rs), [`just`](https://github.com/casey/just), CMake and git. On Windows also install the Visual Studio C++ Build Tools (Rust's installer offers them).
2. `git clone https://github.com/nyaaorick/crossglide.git && cd crossglide`
3. Run `just dev fingerprint` and note this machine's fingerprint.
4. Run `just dev` once. It writes a commented `agent.toml` and stops. In each one, set `peer_fingerprint` to the *other* machine's fingerprint; on the PC, also set `server` to the Mac's IP address.

**Then start the tray app**

| Mac | Windows PC |
| --- | --- |
| `just tray`, or double-click `scripts/tray.command` | drag `scripts\tray.ps1` into PowerShell |
| Allow **Accessibility** for Terminal when asked (System Settings → Privacy & Security), then start it again | In the tray menu choose **Install touchpad driver** (once) |

Both tray icons turn green when they're connected, and PC audio starts playing on the Mac. Push the pointer through the left or right edge of the Mac's screen, or press `ctrl+option+cmd+space`, and the MacBook's trackpad and keyboard drive the PC.

Settings live in `agent.toml` in `~/Library/Application Support/crossglide` on the Mac and `%APPDATA%\crossglide` on the PC (`[touch] edges`, `hotkey`, and whether the Command key acts as Windows or Ctrl). Logs are in `agent.log` beside it.

## Repository

| Path | What's there |
| --- | --- |
| `crates/agent` | The agent: QUIC link, control messages, audio and touch sessions |
| `crates/touch` | Trackpad capture and input tap (Mac), virtual touchpad reports and installer (Windows) |
| `crates/audio` | Capture, Opus, jitter buffer, drift-compensating playback |
| `crates/ui` | The tray app |
| `drivers/touchpad` | The virtual Precision Touchpad driver (UMDF 2, C), with a signed build in `package/` |
| `docs/DESIGN.md` | Architecture and decisions · [ROADMAP.md](ROADMAP.md): milestones and measurements |

## Contributing

Issues and pull requests are welcome, especially reports from other Macs and PCs. [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) has the setup, the command list and a checklist. Before sending a change:

```sh
just lint   # rustfmt and clippy, warnings are errors
just test
just deny   # dependency advisories, bans, sources
```

## Thanks

Crossglide is an independent, pure-Rust project. The idea of gliding a pointer off one screen onto another machine comes from software KVMs such as [Deskflow](https://github.com/deskflow/deskflow) and Synergy, but no code or protocol is shared with them. It stands on [`quinn`](https://github.com/quinn-rs/quinn), [`cpal`](https://github.com/RustAudio/cpal), [`opus`](https://crates.io/crates/opus), [`rubato`](https://github.com/HEnquist/rubato), [`tao`](https://github.com/tauri-apps/tao) and [`tray-icon`](https://github.com/tauri-apps/tray-icon).

## License

[GPL-2.0-only](LICENSE).
