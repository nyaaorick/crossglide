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
7. [Tray app (early M6)](#tray-app-early-m6)
8. [After the audio MVP](#after-the-audio-mvp)
9. [Later milestones](#later-milestones)
10. [Latency budget](#latency-budget)
11. [Risks](#risks)
12. [Open questions](#open-questions)

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

- [x] `quinn` endpoint in `crates/agent`: the Mac listens and the PC connects, on one configurable UDP port. The default is 24800: Deskflow uses TCP 24800, and TCP and UDP ports don't collide.
- [x] Self-signed certificates on each machine, stored next to the agent config: `cert.pem` and `key.pem` (ECDSA P-256; the key readable only by the user) beside `agent.toml`, in `~/Library/Application Support/crossglide` on the Mac and `%APPDATA%\crossglide` on the PC
- [x] Fingerprint pinning: each side trusts only the peer fingerprint in its config. Fingerprints are copied by hand for now; the pairing code replaces that in M5. TLS checks that the peer holds the key for its certificate; the fingerprint is checked right after the handshake, before any stream is used, so both sides can log a clear line when it's refused.
- [x] Control stream: versioned hello (protocol and agent version, host, OS, features), then typed request/response messages (`serde` JSON with a 4-byte length prefix). Either side can send requests. A different protocol version is refused; a different agent version only warns.
- [x] Datagrams enabled; log the negotiated max datagram size at connect
- [x] Clock offset and RTT estimate: an NTP-style exchange on the control stream: a burst of 8 at connect, one every 500 ms for the first 30 s, then one every 2 s. Both sides measure on a session clock (the wall time at connect, advanced by the monotonic clock), because time services slew the wall clocks. The estimate is a line fitted through the samples with the lowest RTTs from the last 5 minutes, since the clocks drift apart. From the second minute on, a summary (offset, drift, jitter, RTT) goes to the log every minute.
- [x] Reconnect with backoff when either side restarts or the network drops: the PC retries after 0.5 s, doubling up to 10 s; a stopping agent tells the other side; a new connection from the PC replaces a stale one on the Mac. Each agent also appends its log to `agent.log` beside `agent.toml`.

**Done when:** the two machines stay connected for 24 hours through sleep/wake and a Wi-Fi drop, a wrong fingerprint is refused with a clear log line, and the clock offset is stable to within 1 ms.

**Status, 2026-09-25:** built, and checked on the real machines over about 3 hours, 2 of them with the Mac asleep: wrong fingerprints are refused with a clear log line on both sides, the connection comes back after restarts, a network drop and every wake, and once settled the clock offset stays within 0.1 ms of a straight line. Still to do: the full 24-hour run, with the PC's agent started in a terminal on the PC rather than over SSH from the Mac.

**LAN results, 2026-09-25** (the same machines and Wi-Fi as M1):

| Check | Result |
| --- | --- |
| Connect | Within 0.1 s once both agents run. Max datagram 1162–1288 bytes at connect; quinn's path MTU discovery raises it later |
| Wrong fingerprint, either side | Refused before any stream opens. The refusing side logs the fingerprint it got and the one it expected; the refused side logs the fingerprint the other machine needs. The live connection isn't disturbed |
| Mac agent restarts | The PC reconnects 2 s after the Mac's agent is back |
| PC agent killed and restarted | The Mac drops the stale connection as soon as the new one arrives, without waiting for the 10 s timeout |
| UDP blocked for 25 s (PC firewall rule) | Both sides log "nothing heard for 10 s" after 10 s; connected again 3 s after the block ends |
| Mac asleep for 2 hours, with DarkWakes | Each time the Mac woke, even briefly, the PC was connected within a second and the Mac dropped the stale connection. While the Mac slept, the PC logged one warning per outage, then "reached … after 263 failed attempts over 5243 s" on reconnecting: about four log lines per sleep |
| RTT | 5–6 ms median on Wi-Fi, 4 ms at best; spikes to 340 ms, and one minute at about 90 ms |
| Wall clocks | The PC's is 2.3 s behind the Mac's, and they drift apart by about 100 ppm, unevenly: one minute measured 330 ppm. Neither is well synced: the PC's NTP requests time out, and the Mac's go through the proxy's fake-IP DNS (±0.56 s). Against their own monotonic clocks, the Mac's wall clock ran 24 ppm fast and the PC's 145 ppm |
| Clock offset | Measured first on the wall clocks, with the fastest recent sample as the estimate: jitter 0.8–1.1 ms, then 2.1 ms in the 330 ppm minute and 14.7 ms in the slow-Wi-Fi minute. On session clocks, fitted to a line: the drift is a steady 72–75 ppm, and jitter is 0.02–0.10 ms a minute once settled, with RTT spikes to 99 ms in most minutes. The first minute after connecting reached 1.2–1.7 ms when the network was slow, so the probes are faster then and the log's summaries start after it; in simulation the first summarised minute stays under 0.4 ms even then. The two sides agree on the offset to within 0.5 ms |

**Running it on the PC and the Mac:**

1. On each machine, `just dev fingerprint` prints that machine's fingerprint (and creates its certificate the first time).
2. On each machine, `just dev` once: it writes a commented `agent.toml` (role `listen` on the Mac, `connect` on the PC) and stops. Set `peer_fingerprint` to the other machine's fingerprint and, on the PC, `server` to the Mac's IP address or host name.
3. `just dev` on both. Each logs `connected to …` and a first clock estimate within a second, then, from the second minute on, a `clock:` line every minute.
4. For the 24-hour run, start the PC's agent in a terminal on the PC, not over SSH. Afterwards, read `agent.log` on both machines: every `disconnected` should be followed by `connected` once both are awake and online, and `jitter` in the `clock:` lines should stay under 1 ms.

## M3: Audio MVP, PC → Mac speakers

The real pipeline, with one fixed path: the PC's output device in, the MacBook's built-in speakers out. `crates/audio` doesn't depend on `quinn`: it produces and consumes packets, and the agent moves them. That lets tests run the pipeline over a simulated network.

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

**Fixed MVP settings:** 48 kHz stereo, Opus 128 kbps, 10 ms frames, `RESTRICTED_LOWDELAY`, adaptive jitter buffer (starts at 40 ms, 20–150 ms), 256-frame output callbacks on the Mac. Nothing here is configurable yet.

**Tasks**

- [x] Capture thread → lock-free ring buffer → encode thread. The capture callback never allocates or blocks: it copies into a `ringbuf` ring, and a thread wakes every 2 ms to resample and encode. The same holds for playback: the whole player runs in the output callback without allocating or locking, fed by a lock-free queue of packets.
- [x] Resample the device rate to 48 kHz at the edges with `rubato` (sinc, 128 taps): on the PC when its device isn't at 48 kHz, and on the Mac to the output device's rate, in the same resampler that corrects drift.
- [x] Silence: WASAPI loopback sends nothing while nothing plays. When capture has been quiet for 25 ms, the PC sends Opus-encoded silence every 10 ms on its own clock, so the Mac sees a steady stream.
- [x] Adaptive jitter buffer: each packet's lateness is measured against the earliest arrival in the last 8 s (relative to its timestamp). The target is the latest lateness of the last 2 minutes plus a 10 ms margin, within 20–150 ms; it starts at 40 ms. A packet within 10% of the held level holds it again; after that it comes down 0.5 ms/s. In the M3 run, packets 25–35 ms late came every minute or two, which a 1-minute hold missed.
- [x] Loss and late packets: Opus concealment for lost frames. **Changed from the plan:** when the buffer runs dry, the Mac conceals and *waits* for the missing frame instead of skipping it. A delay spike then grows the buffer by the spike instead of throwing audio away; if the frame turns out to be lost, the concealment already played stands in for it, so a lost packet adds no delay. Packets that arrive after their frame was skipped are dropped. With nothing for 200 ms, the buffer starts over.
- [x] Clock drift: a PI controller steers the playback resampler so the playout delay (how far playback runs behind the earliest arrivals) meets the target. The integral learns the sound cards' skew within ±500 ppm, only while the delay is within 3 ms of the target; with the proportional part and the 500 ppm needed to follow a falling target, the total is at most ±2000 ppm (3.5 cents). The delay is measured against the start of the callback that needs each frame, not its playing time, which is a callback earlier.
- [x] Start and stop over the control stream: agents list an `audio` feature in the hello. The Mac opens its output device, then asks the PC to start (`audio_start`); the PC captures only after that. Either side sends `audio_stop` with a reason when its device fails, and the connection ending stops both.
- [x] If either device disappears, stop cleanly with a clear log line (`audio: playback on … stopped: …; restart the agent to play again`) and tell the other side; the connection stays up and restarting the agent recovers. Written to the error callbacks cpal documents, but not yet tested by unplugging a device.
- [x] Test harness (`crates/audio/src/sim.rs`): the real packetizer output and player through a simulated link with loss, jitter, reordering, delay spikes, outages and clock skew, in simulated time (about 6 s for the lot). Asserts: 1% loss with 10 ms jitter plays with no underruns and the latency expected; ±300 ppm skew at either end keeps the delay within 3 ms of the target on average; 5% of packets 15 ms late plays with none late or lost; a 90 ms spike causes one underrun, a second one none, and the target comes back down; a 1 s outage restarts the buffer once.

Also in M3:

- **Latency measurement:** every 2 s the PC sends `audio_mark`: the timestamp of a recent frame and when its first sample was captured, on its session clock. Every second, the Mac maps the sample it's playing back through the mark and the M2 clock offset, including CoreAudio's output latency and Opus's 2.5 ms lookahead. Every 10 s it logs latency, delay and target, rate and skew, packets, loss, lateness and underruns.
- **Windows default device:** cpal's default output on Windows is a virtual device that follows the system default, and opening it a second time in one process fails ("Cannot change thread mode after it is set"). Capture opens the device the default points to instead; switching devices takes an agent restart, as planned for the MVP.
- **Test tone:** `just dev run --test-tone` on the PC sends a 440 Hz tone instead of loopback.

**Done when**, measured on the real machines:

- end-to-end latency is 75 ms or less on Wi-Fi between delay spikes, and 55 ms or less on Ethernet ([budget](#latency-budget))
- one hour of music has no audible dropouts
- two hours of playback show no drift (buffer fill stays within target)

**Status, 2026-09-25:** built and running between the Mac and the PC. Over 10 minutes of the test WAV on Wi-Fi, latency was 41–76 ms outside the minutes after a large spike (target 75 ms), with nothing lost and no drift. Each delay spike bigger than the buffer still causes a dropout: three spikes, four dropouts in 10 minutes. Still to do: the hour of music listened to for dropouts, the two-hour drift run, the Ethernet latency, and unplugging a device. Whether a dropout every few minutes is acceptable, or the buffer should stay deeper for longer (more latency), is for the listening test to decide.

**LAN results, 2026-09-25** (Mac on Wi-Fi 6, as in M1 and M2; PC capturing its 'Digital Output' device at 48 kHz):

| Check | Result |
| --- | --- |
| Latency, capture to speaker | 41–76 ms, usually 45–71 ms; 133 ms for two minutes after a 105 ms spike, then back down at 0.5 ms/s. First reading 53–55 ms |
| Packets | 60,000 of 60,000 in 10 minutes, none lost or late. Wi-Fi RTT 3–4 ms |
| Packet lateness | Up to 10–35 ms most minutes; spikes of 43, 48 and 105 ms in the 10-minute run |
| Dropouts | 4 (1 each for the 43 and 48 ms spikes, 2 for the 105 ms one). Each time the buffer grew by the spike and the next spikes that size played cleanly. In an earlier run with a 1-minute hold, a 35 ms spike right after the target came down caused one; the hold is now 2 minutes |
| Drift | The delay stayed within 1 ms of the target in every 10 s report; the sound cards' skew settles at −80 ppm (±10) |
| Reconnect | Restarting the Mac's agent: audio back within a second of reconnecting. The second capture in one PC agent used to fail on Windows' virtual default device; fixed |
| Output device | MacBook Air Speakers at 48 kHz: 256-frame callbacks, 7.6 ms output latency, no xruns |

**Running it:** with M2 set up, `just dev` on both machines; audio starts on its own once they connect. Play something on the PC; the Mac logs an `audio:` line every 10 s. To keep the PC itself silent, make a device with nothing connected (here Digital Output) its default output, as in M1.

## M4: Audio controls and stats

There's no UI yet, so controls are a config file and CLI flags. The typed shared config in M7 takes this file over unchanged.

- [ ] `audio` section in the agent's TOML config: `enabled` (true/false) and `volume`
- [ ] CLI: `crossglide-agent audio status` prints current latency, jitter-buffer depth, loss %, bitrate, and device names
- [ ] Stats go to the log through `tracing` every 10 s, and immediately on dropouts
- [ ] Document the PC's own speakers: whether they can be muted without muting the loopback ([open question](#open-questions))

**Done when:** you can turn audio on and off and change the volume without restarting, and the log alone is enough to explain a dropout afterwards.

## Tray app (early M6)

A minimal tray app, brought forward from M6 so the Mac and PC setup can be used day to day and give feedback before M4 and the full UI. `crates/ui` builds `crossglide`, which runs the agent in-process (the agent is now a library as well as the `crossglide-agent` command) and shows a coloured dot in the Windows notification area or the Mac menu bar: green connected, amber connecting or something to look at, grey waiting, red stopped.

The menu shows the connection and the audio state (device, and latency on the Mac), and has: Audio on/off (reconnects; not remembered after a restart), Open log, Open config folder, Start at login (a `Run` registry value on Windows, a LaunchAgent on macOS, pointing at the exe that set it), and Quit, which tells the other machine it's stopping. Release builds have no console window on Windows; everything goes to `agent.log`. Only one copy runs per user.

Not in it yet: settings or pairing in the UI (edit `agent.toml`), remembering audio off, an app icon or installer.

**Running it:** `just tray` in the repo on each machine (a release build). On Windows it can also be started directly: `target\release\crossglide.exe`. Run either the tray app or `just dev` on a machine, not both.

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

Estimated before M3 for the fixed MVP settings, and measured in M3 on Wi-Fi.

| Stage | Estimate | Measured in M3 | Notes |
| --- | --- | --- | --- |
| WASAPI capture period | 10 ms | 10 ms callbacks | Shared-mode default |
| Opus frame + lookahead | 12.5 ms | 12.5 ms | 10 ms frame + 2.5 ms in `RESTRICTED_LOWDELAY` |
| Network | 1–5 ms | about 2 ms | Half the RTT of 3–4 ms; lateness beyond that is the jitter buffer's job |
| Jitter buffer | 20 ms on Ethernet, 40 ms on Wi-Fi | 25–55 ms playout delay | Follows how late packets arrive: 25–35 ms on this Wi-Fi, 45–55 ms for a couple of minutes after a spike |
| Frame deadline | – | 5.3 ms | A frame is needed at the start of the callback that plays it; 256-frame callbacks |
| CoreAudio output | 5–10 ms | 7.6 ms | Built-in speakers, 256-frame buffer |
| **Total** | **about 55 ms on Ethernet, 75 ms on Wi-Fi** | **41–76 ms on Wi-Fi**, 133 ms after a 105 ms spike | Measured end to end; 5 ms frames would save about 10 ms at higher CPU and bitrate (after the MVP) |

Measuring: the in-band timestamp plus the clock offset from M2 gives capture-to-playback latency continuously (M3 logs it every 10 s). The offset is between the two agents' session clocks, not their wall clocks, so both ends of the audio stream have to read times from the connection's session clock. Check it once acoustically: record a click through both machines' speakers with a phone and compare.

## Risks

| Risk | Why it matters | Mitigation |
| --- | --- | --- |
| `cpal` loopback misbehaves | Blocks the whole feature | Retired: M1 showed it works on the target PC |
| Wi-Fi jitter | M1 measured delay spikes up to 85 ms a few times in 10 minutes; each one caused a dropout with a fixed 40 ms buffer | Adaptive jitter buffer with concealment (M3): a spike bigger than the buffer still causes one dropout, after which the buffer covers spikes that size for a few minutes. An Ethernet adapter for the Mac; the 10 s stats line shows the cause |
| Capture from a Windows service | The M5 agent runs as a service in session 0 | Probably fine: in M1 a session-0 process (over SSH, as the user's account) captured the desktop's audio. A real service runs as a different account, so M5 checks it; the fallback is a helper in the user's session |
| Drift compensation artefacts | Bad resampler steering sounds like wow or flutter | Corrections within ±2000 ppm (3.5 cents), changed smoothly by the resampler; skew tested in the harness. Listening over hours is part of M3's done-when |

## Open questions

- [x] Does `cpal` WASAPI loopback work reliably on the target PC? Yes ([M1](#m1-audio-spike)).
- [ ] Can the PC's speakers be silenced while loopback still captures audio, or does muting the endpoint also mute the capture? Not tested yet. Workaround from M1: make a device with nothing connected (here Digital Output) the default output, so the PC stays silent.
- [ ] Should audio capture on Windows run in a user-session helper spawned by the service? Probably not: session-0 capture worked in M1 ([Risks](#risks)). Confirm with the real service in M5.
- [x] One QUIC port, or share one with Deskflow's port number (24800/UDP)? Its own port, 24800/UDP by default: the same number as Deskflow's TCP port, which doesn't collide with it. It's configurable ([M2](#m2-side-channel)).
