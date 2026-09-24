# Crossglide roadmap

*Draft as of 2026-09-25 · builds on the design in [README.md](README.md)*

Audio streaming (PC → Mac) is the first feature. It comes before the Deskflow core, pairing and UI work for four reasons:

- **It needs nothing from the C++ core.** Audio runs entirely in the Rust workspace, so work starts without trimming or integrating upstream.
- **It's useful by itself.** Hearing the PC's audio on the MacBook helps even while keyboard and mouse still run on stock Deskflow.
- **It builds the side channel.** The QUIC connection built for audio is the one logs, config, MCP and touch frames use later.
- **It settles a big risk early.** Whether `cpal` WASAPI loopback works on the target PC is an open question. The answer decides between `cpal` and miniaudio before anything else depends on it.

**MVP scope:** the PC's system audio plays on the MacBook's built-in speakers, in one direction only. Headphones, Bluetooth, device switching, quality presets and the Mac mic are [after the MVP](#after-the-audio-mvp). A single fixed path keeps the MVP cheap to build and easy to debug: when something sounds wrong, there's only one capture device, one codec setting and one output to check.

## Contents

1. [Overview](#overview)
2. [M0: Workspace](#m0-workspace)
3. [M1: Audio spike](#m1-audio-spike)
4. [M2: Side channel](#m2-side-channel)
5. [M3: Audio MVP, PC → Mac speakers](#m3-audio-mvp-pc--mac-speakers)
6. [M4: Audio controls and stats](#m4-audio-controls-and-stats)
7. [After the audio MVP](#after-the-audio-mvp)
8. [Later milestones](#later-milestones)
9. [Latency budget](#latency-budget)
10. [Risks](#risks)
11. [Open questions](#open-questions)

## Overview

| # | Milestone | Delivers | Depends on |
| --- | --- | --- | --- |
| M0 | Workspace | Cargo workspace, `just dev`, CI build on macOS and Windows | – |
| M1 | Audio spike | Evidence that loopback capture, Opus and playback work on the real machines | M0 |
| M2 | Side channel | QUIC connection with pinned fingerprints, a control stream and datagrams | M0 |
| M3 | Audio MVP | PC audio on the MacBook's built-in speakers, with a jitter buffer and drift compensation | M1, M2 |
| M4 | Audio controls | On/off and volume in a config file, stats in the log | M3 |
| M5+ | Everything else | Core integration, pairing, UI, logs and config, MCP, Esparrier, touchpad | M2 |

M1 and M2 are independent and can run in parallel.

Each milestone lists its tasks and a **Done when** condition. A milestone is finished when that condition has been checked on the real Mac and PC, not only in tests.

## M0: Workspace

Set up only what audio needs. Trimming the core build and the UI crates wait until [M5](#later-milestones).

- [x] crossglide is its own git repo, with Deskflow as a submodule at `upstream/`
- [x] Upstream builds on the Mac: Deskflow 1.26.0 at `9ac5464e`, Qt 6.11.2 from Homebrew. 27 of 28 unit-test suites pass; `OSXKeyStateTests` needs Accessibility permission because it sends real keystrokes.
- [x] Cargo workspace with `crates/audio` (library) and `crates/agent` (binary), following the layout in the README
- [x] `justfile` with `just dev`, `just test`, `just lint` (`cargo fmt --check`, `cargo clippy -D warnings`), `just deny`, and `just core` (fetches the submodule and builds the C++ core; macOS only for now)
- [x] CI builds and tests on `macos-latest` and `windows-latest` (`.github/workflows/ci.yml`); the first run passed on both
- [x] Dependency check (`cargo deny`: advisories, bans, sources). Licences aren't checked ([Decisions](README.md#decisions)).
- [x] `LICENSE` (GPL-2.0) at the repo root

**Done when:** a fresh clone builds and passes `just test` on both machines and in CI. Done: it passes on the Mac, on the PC and in CI.

## M1: Audio spike

Small, throwaway binaries that answer "does this work on our hardware?" before any architecture is built on it. Findings go into [Decisions](README.md#decisions) in the README.

The spike is one binary, `crates/spike`, run as `just spike <command>`: `capture`, `play`, `opus`, `send` and `recv`. It uses `cpal` 0.18 and `opus` 0.4. `send` can also stream a sine or a WAV instead of loopback, and `--drop-every N` simulates loss.

- [x] **Capture (PC):** record 60 s of system audio from the default output device via `cpal` WASAPI loopback into a WAV file, while playing a video. Works (below). The PC's output was already 48 kHz stereo, so no setting changed.
- [x] **Playback (Mac):** play a WAV and a generated sine through `cpal` on CoreAudio, on the built-in speakers. Works; no underruns, and it sounded good during the PC → Mac listening test.
- [x] **Opus:** round-trip that WAV through `opus` at 128 kbps stereo, 10 ms frames, `RESTRICTED_LOWDELAY` mode. Measure encode CPU on the PC. Done on both machines, on the PC with a real capture (below).
- [x] **Build:** confirm the Opus crate builds libopus from source on both OSes with no system library installed. Works on both: `opus` 0.4 builds it through `opusic-sys` and CMake, linked statically.
- [x] **Naive stream:** send Opus frames over plain UDP from PC to Mac with a fixed 40 ms buffer. No QUIC yet. Listen for 10 minutes. Done: nothing lost in 10 minutes and it sounded good, with two brief dropouts caused by Wi-Fi delay spikes (below).

**Done when:** the naive stream sounds clean on the target LAN, or the failures are written down with a decision. If `cpal` loopback fails, the fallback is miniaudio through `cc`.

**Result: done, 2026-09-25.** `cpal` loopback works, so miniaudio isn't needed. The only weak spot is the Mac's Wi-Fi; the decision for it is below.

**Mac results, 2026-09-25** (MacBook Air, macOS 26.5, built-in speakers at 48 kHz):

| Check | Result |
| --- | --- |
| Opus packet size | 161 bytes on average, 292 max (129 kbps); fits one datagram easily |
| Opus CPU | Encode 97 µs and decode 22 µs per 10 ms frame: about 1% and 0.2% of one core |
| Opus lookahead | 120 samples (2.5 ms), as the [latency budget](#latency-budget) assumed |
| Stream, no loss | 30 s over localhost: 3001 of 3001 frames, buffer steady at 33–47 ms, no underruns |
| Stream, 2% loss | Every lost frame concealed by Opus PLC, no underruns |
| Buffer creep | In one run the buffer settled at 117–125 ms instead of 40 ms and stayed there. A fixed buffer never drains extra audio it builds up, so M3's jitter buffer must shrink back toward its target, not only grow |

**PC and LAN results, 2026-09-25** (PC: Windows 11 build 26200, i5-12400F. Mac on Wi-Fi 6, 5 GHz):

| Check | Result |
| --- | --- |
| Loopback capture | 100% of wall time while audio plays, in 10 ms (480-frame) callbacks; longest gap 11–14 ms. Nothing is delivered while nothing plays. WASAPI reports one harmless discontinuity at stream start |
| Capture from session 0 | Commands over SSH run in session 0 (the desktop is session 1), and loopback there still captures the desktop's audio |
| Opus CPU on the PC | Encode 204–241 µs, decode 48–58 µs per 10 ms frame: about 2% and 0.5% of one core |
| Stream, 60 s | 6001 of 6001 frames; longest packet gap per 5 s window 13–29 ms; no underruns |
| Stream, 10 min | 60,003 of 60,003 frames, none lost or late. 2 underruns, right after packet gaps of 42 and 85 ms; gaps of 50–85 ms happened a few times. After the 85 ms spike the buffer sat at 83–91 ms for the rest of the run. No measurable clock drift over 8 minutes |

**Decision for M3:** the Mac's Wi-Fi, not the PC or the codec, limits the stream. M3's jitter buffer starts at 40 ms on Wi-Fi, grows quickly toward about 100 ms when a packet gap exceeds its depth, and shrinks back slowly once gaps settle. When it runs dry it plays Opus concealment instead of silence, and drops the late packets whose time was already concealed. The M3 tasks, the done-when latency and the latency budget are updated to match.

**Running it on the PC and the Mac:**

1. On the PC, install Rust (rustup, with the MSVC build tools it offers), CMake, `just` and git, then clone the repo. Set the output device to 48 kHz (Settings > System > Sound > the device > Output settings > Format).
2. PC: `just spike capture --seconds 60` while a video plays; listen to `capture.wav`, then run `just spike opus capture.wav`.
3. Mac: `just spike recv`. PC: `just spike send --loopback --to <Mac IP>:5004`. Listen for 10 minutes and watch `lost`, `maxgap`, `underruns` and `buffer` in the Mac's output.

Tips: `python3 scripts/make-test-audio.py` makes a 10-minute test WAV (a quiet tone, a beep every second and the elapsed time spoken every 10 seconds), so a dropout can be located. To keep the PC silent while the Mac plays, set the PC's default output to a device with nothing connected (here Digital Output); the audio has to play on the default output device, because that's the one `send` records.

## M2: Side channel

The QUIC connection every later feature shares. The PC connects to the Mac, the same direction as the Deskflow client and server.

- [ ] `quinn` endpoint in `crates/agent`: the Mac listens and the PC connects, on one configurable UDP port
- [ ] Self-signed certificates on each machine, stored next to the agent config
- [ ] Fingerprint pinning: each side trusts only the peer fingerprint in its config. Fingerprints are copied by hand for now; the pairing code replaces that in M5.
- [ ] Control stream: versioned hello (agent version, features), then typed request/response messages (`serde` + a length prefix)
- [ ] Datagrams enabled; log the negotiated max datagram size at connect
- [ ] Clock offset and RTT estimate: an NTP-style exchange on the control stream every few seconds, for latency stats
- [ ] Reconnect with backoff when either side restarts or the network drops

**Done when:** the two machines stay connected for 24 hours through sleep/wake and a Wi-Fi drop, a wrong fingerprint is refused with a clear log line, and the clock offset is stable to within 1 ms.

## M3: Audio MVP, PC → Mac speakers

The real pipeline, with one fixed path: the PC's default output device in, the MacBook's built-in speakers out. `crates/audio` doesn't depend on `quinn`: it produces and consumes packets, and the agent moves them. That lets tests run the pipeline over a simulated network.

```mermaid
flowchart LR
  A[WASAPI loopback<br/>cpal] --> B[Resample to 48 kHz]
  B --> C[Opus encode<br/>10 ms frames]
  C -->|QUIC datagram<br/>seq + timestamp| D[Jitter buffer]
  D --> E[Opus decode<br/>PLC on loss]
  E --> F[Drift resampler]
  F --> G[Built-in speakers<br/>CoreAudio via cpal]
```

**Packet format**

| Field | Size | Notes |
| --- | --- | --- |
| Version | 1 byte | Rejects mismatched builds; bumped if later features change the format |
| Sequence number | 2 bytes | Wraps; detects loss and reordering |
| Timestamp | 4 bytes | Sample index at 48 kHz; drives jitter buffer and latency stats |
| Opus payload | about 160 bytes | 10 ms at 128 kbps |

**Fixed MVP settings:** 48 kHz stereo, Opus 128 kbps, 10 ms frames, `RESTRICTED_LOWDELAY`, adaptive jitter buffer (40 ms on Wi-Fi, up to about 100 ms after a spike). Nothing here is configurable yet.

**Tasks**

- [ ] Capture thread → lock-free ring buffer → encode thread. The capture callback never allocates or blocks.
- [ ] Resample the device rate (44.1 or 48 kHz) to 48 kHz at the edges with `rubato` (MIT)
- [ ] Silence: WASAPI loopback sends no data while nothing plays (confirmed in M1). Detect the gap and send silence or Opus DTX so the Mac doesn't read it as loss.
- [ ] Adaptive jitter buffer: start at 40 ms on Wi-Fi (20 ms on Ethernet); grow quickly toward about 100 ms when a packet gap exceeds the current depth (M1 measured Wi-Fi gaps up to 85 ms); shrink back slowly once gaps settle, so one spike doesn't leave latency high (M1's fixed buffer stayed at about 90 ms)
- [ ] Loss and late packets: Opus packet-loss concealment for missing frames and whenever the buffer runs dry; drop packets that arrive after their time was concealed
- [ ] Clock drift: steer the playback resampler by jitter-buffer fill level, within ±500 ppm
- [ ] Start and stop over the control stream, so the PC only captures while the Mac is listening
- [ ] If either device disappears, stop cleanly with a clear log line; restarting the agent recovers
- [ ] Test harness: run the pipeline through a simulated link with configurable loss, jitter, reordering and clock skew. Assert no underruns at 1% loss and 10 ms jitter, and bounded buffer size under ±300 ppm skew.

**Done when**, measured on the real machines:

- end-to-end latency is 75 ms or less on Wi-Fi between delay spikes, and 55 ms or less on Ethernet ([budget](#latency-budget))
- one hour of music has no audible dropouts
- two hours of playback show no drift (buffer fill stays within target)

## M4: Audio controls and stats

There's no UI yet, so controls are a config file and CLI flags. The typed shared config in M7 takes this file over unchanged.

- [ ] `audio` section in the agent's TOML config: `enabled` (true/false) and `volume`
- [ ] CLI: `crossglide-agent audio status` prints current latency, jitter-buffer depth, loss %, bitrate, and device names
- [ ] Stats go to the log through `tracing` every 10 s, and immediately on dropouts
- [ ] Document the PC's own speakers: whether they can be muted without muting the loopback ([open question](#open-questions))

**Done when:** you can turn audio on and off and change the volume without restarting, and the log alone is enough to explain a dropout afterwards.

## After the audio MVP

Not planned in detail. Each item is picked up only once the MVP has been used day to day and the need is real.

| Item | Why it's deferred |
| --- | --- |
| Headphones and other Mac outputs | Each output has its own buffer size and latency; the MVP proves the pipeline on one |
| Bluetooth output (AirPods) | Adds 100–200 ms we can't remove; needs latency shown in the UI |
| Device switching without restart (both sides) | Hot-plug handling is fiddly and easy to get wrong; restarting is fine for an MVP |
| Quality presets, FEC, 5 ms frames | Tune only after real latency and loss numbers exist |
| Mac mic → PC | Needs a virtual microphone driver on Windows or ESP32 USB audio |

## Later milestones

These keep the README's order after audio. Each gets its own task list when it's next.

| # | Milestone | Summary |
| --- | --- | --- |
| M5 | Core and agent | Trimmed build config, agent spawns and supervises `deskflow-core`, pairing code replaces hand-copied fingerprints, Windows service and firewall rules |
| M6 | UI | `tray-icon` menu bar and tray, `egui` settings window, macOS permissions onboarding; audio controls move here |
| M7 | Logs and config | Merged log timeline, typed shared config, health checks |
| M8 | MCP | MCP server on the Mac agent, including `audio_status` / `audio_route` |
| M9 | Esparrier | Detect, configure and flash an ESP32-S3 |
| M10 | Touchpad | MultitouchSupport capture, contact frames over datagrams, virtual precision touchpad (riskiest, so last) |

## Latency budget

Estimated for the fixed MVP settings, with the Wi-Fi numbers from M1. M3 measures each stage and replaces these numbers.

| Stage | Estimate | Notes |
| --- | --- | --- |
| WASAPI capture period | 10 ms | Shared-mode default |
| Opus frame + lookahead | 12.5 ms | 10 ms frame + 2.5 ms in `RESTRICTED_LOWDELAY` |
| Network | 1–5 ms | LAN. On Wi-Fi, M1 measured packet gaps usually under 30 ms, with spikes to 85 ms a few times in 10 minutes |
| Jitter buffer | 20 ms on Ethernet, 40 ms on Wi-Fi | Grows toward about 100 ms after a spike, then shrinks back |
| CoreAudio output buffer | 5–10 ms | Built-in speakers |
| **Total** | **about 55 ms on Ethernet, 75 ms on Wi-Fi** | 5 ms frames would save about 10 ms at higher CPU and bitrate (after the MVP) |

Measuring: the in-band timestamp plus the clock offset from M2 gives capture-to-playback latency continuously. Check it once acoustically: record a click through both machines' speakers with a phone and compare.

## Risks

| Risk | Why it matters | Mitigation |
| --- | --- | --- |
| `cpal` loopback misbehaves | Blocks the whole feature | Retired: M1 showed it works on the target PC |
| Wi-Fi jitter | M1 measured delay spikes up to 85 ms a few times in 10 minutes; each one caused a dropout with a fixed 40 ms buffer | Adaptive jitter buffer with concealment (M3); an Ethernet adapter for the Mac; stats show the cause |
| Capture from a Windows service | The M5 agent runs as a service in session 0 | Probably fine: in M1 a session-0 process (over SSH, as the user's account) captured the desktop's audio. A real service runs as a different account, so M5 checks it; the fallback is a helper in the user's session |
| Drift compensation artefacts | Bad resampler steering sounds like wow or flutter | Small, slow corrections; test over hours with skew in the harness |

## Open questions

- [x] Does `cpal` WASAPI loopback work reliably on the target PC? Yes ([M1](#m1-audio-spike)).
- [ ] Can the PC's speakers be silenced while loopback still captures audio, or does muting the endpoint also mute the capture? Not tested yet. Workaround from M1: make a device with nothing connected (here Digital Output) the default output, so the PC stays silent.
- [ ] Should audio capture on Windows run in a user-session helper spawned by the service? Probably not: session-0 capture worked in M1 ([Risks](#risks)). Confirm with the real service in M5.
- [ ] One QUIC port, or share one with Deskflow's port number (24800/UDP)?
