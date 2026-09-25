//! Test harness: the pipeline through a simulated network, in simulated time. Packets from a
//! `Packetizer` cross a link with loss, jitter, reordering, delay spikes and outages, and a
//! `Player` plays them for a device whose clock runs at a different rate from the sender's.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::FRAME_SAMPLES;
use crate::packet::{HEADER_LEN, Header};
use crate::player::{DECAY_MS_PER_S, HOLD_NS, MIN_MS, Player, Snapshot};
use crate::sender::Packetizer;

const MS: f64 = 1e6;
const CALLBACK_FRAMES: usize = 512;
const AMPLITUDE: f32 = 0.3;

/// xorshift64*: deterministic, so a failing run can be replayed.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Link {
    loss: f64,
    delay_ms: f64,
    /// Extra delay, uniform between zero and this.
    jitter_ms: f64,
    /// Share of packets held back 15 ms, so they arrive after the next one.
    reorder: f64,
    /// (start s, length ms): nothing gets through from `start` for `length`, then everything
    /// sent meanwhile arrives at once, as when Wi-Fi stalls.
    spikes: Vec<(f64, f64)>,
    /// (start s, end s): the sender sends nothing.
    outages: Vec<(f64, f64)>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            loss: 0.0,
            delay_ms: 5.0,
            jitter_ms: 2.0,
            reorder: 0.0,
            spikes: Vec::new(),
            outages: Vec::new(),
        }
    }
}

struct Run {
    link: Link,
    seconds: f64,
    /// How much faster than nominal each device's clock runs.
    sender_ppm: f64,
    player_ppm: f64,
}

impl Run {
    fn new(seconds: f64, link: Link) -> Self {
        Self {
            link,
            seconds,
            sender_ppm: 0.0,
            player_ppm: 0.0,
        }
    }
}

struct Sample {
    at_s: f64,
    delay_ms: f64,
    target_ms: f64,
    /// Time from sending a sample to playing it.
    latency_ms: Option<f64>,
}

struct Outcome {
    stats: Snapshot,
    frames_sent: u64,
    samples: Vec<Sample>,
    /// Of everything played after the first second.
    rms: f64,
}

impl Outcome {
    fn after(&self, from_s: f64) -> impl Iterator<Item = &Sample> {
        self.samples.iter().filter(move |s| s.at_s >= from_s)
    }

    fn at(&self, at_s: f64) -> &Sample {
        self.after(at_s).next().expect("the run lasted that long")
    }
}

/// A second of sine, encoded once; runs reuse the payloads under new headers.
fn payloads() -> Vec<Vec<u8>> {
    let mut packetizer = Packetizer::new().unwrap();
    let audio: Vec<f32> = (0..100 * FRAME_SAMPLES)
        .flat_map(|i| {
            let s = AMPLITUDE * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
            [s, s]
        })
        .collect();
    let mut payloads = Vec::new();
    packetizer
        .push(&audio, &mut |packet| {
            payloads.push(packet[HEADER_LEN..].to_vec())
        })
        .unwrap();
    payloads
}

fn run(run: Run) -> Outcome {
    let payloads = payloads();
    let link = &run.link;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut player, mut incoming) = Player::new(48_000).unwrap();
    let stats = player.stats();

    let frame_ns = 10.0 * MS / (1.0 + run.sender_ppm * 1e-6);
    let callback_ns = CALLBACK_FRAMES as f64 * 1e9 / 48_000.0 / (1.0 + run.player_ppm * 1e-6);
    let end_ns = run.seconds * 1e9;

    let mut in_flight = BinaryHeap::new();
    let mut sent = 0u64;
    let mut out = vec![0.0f32; CALLBACK_FRAMES * 2];
    let mut samples = Vec::new();
    let mut next_sample = 0.0;
    let (mut energy, mut counted) = (0.0f64, 0u64);

    for callback in 0u64.. {
        let now = callback as f64 * callback_ns;
        if now >= end_ns {
            break;
        }
        // Send everything due by now.
        while (sent as f64) * frame_ns <= now {
            let sent_at = sent as f64 * frame_ns;
            let sent_s = sent_at / 1e9;
            let n = sent;
            sent += 1;
            if link.outages.iter().any(|&(a, b)| (a..b).contains(&sent_s)) {
                continue;
            }
            if rng.unit() < link.loss {
                continue;
            }
            let mut arrival = sent_at + link.delay_ms * MS + rng.unit() * link.jitter_ms * MS;
            if rng.unit() < link.reorder {
                arrival += 15.0 * MS;
            }
            for &(start, length) in &link.spikes {
                let (from, to) = (start * 1e9, start * 1e9 + length * MS);
                if (from..to).contains(&sent_at) {
                    arrival = arrival.max(to + link.delay_ms * MS);
                }
            }
            let mut packet = vec![0u8; HEADER_LEN];
            Header {
                seq: n as u16,
                ts: (n * FRAME_SAMPLES as u64) as u32,
            }
            .write(&mut packet);
            packet.extend_from_slice(&payloads[n as usize % payloads.len()]);
            in_flight.push(Reverse((arrival as u64, n, packet)));
        }
        while in_flight
            .peek()
            .is_some_and(|Reverse((arrival, _, _))| *arrival as f64 <= now)
        {
            let Reverse((arrival, _, packet)) = in_flight.pop().unwrap();
            incoming.push(&packet, arrival).unwrap();
        }

        player.fill(&mut out, now as u64, now as u64);
        assert!(out.iter().all(|s| s.is_finite()));
        if now >= 1e9 {
            energy += out.iter().map(|&s| f64::from(s * s)).sum::<f64>();
            counted += out.len() as u64;
        }

        if now >= next_sample {
            next_sample += 100.0 * MS;
            let snap = stats.snapshot();
            // Every timestamp in these runs fits 32 bits, so it doesn't wrap.
            let latency_ms = stats.anchor().map(|a| {
                let sent_at = f64::from(a.ts) / FRAME_SAMPLES as f64 * frame_ns;
                (a.heard_at as f64 - sent_at) / MS
            });
            samples.push(Sample {
                at_s: now / 1e9,
                delay_ms: snap.delay_ms,
                target_ms: snap.target_ms,
                latency_ms: snap.playing.then_some(latency_ms).flatten(),
            });
        }
    }
    Outcome {
        stats: stats.snapshot(),
        frames_sent: sent,
        samples,
        rms: (energy / counted.max(1) as f64).sqrt(),
    }
}

/// Mean and range of the playout delay minus the target.
fn error_range<'a>(samples: impl Iterator<Item = &'a Sample>) -> (f64, f64, f64) {
    let errors: Vec<f64> = samples.map(|s| s.delay_ms - s.target_ms).collect();
    let mean = errors.iter().sum::<f64>() / errors.len() as f64;
    let min = errors.iter().copied().fold(f64::INFINITY, f64::min);
    let max = errors.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, min, max)
}

#[test]
fn one_percent_loss_and_10ms_jitter_play_without_underruns() {
    let out = run(Run::new(
        120.0,
        Link {
            loss: 0.01,
            jitter_ms: 10.0,
            ..Link::default()
        },
    ));
    let s = &out.stats;
    assert_eq!(
        (s.underruns, s.late, s.stalls, s.skipped),
        (0, 0, 0, 0),
        "{s:?}"
    );
    let loss = s.lost as f64 / out.frames_sent as f64;
    assert!((0.005..0.015).contains(&loss), "{loss}");
    assert!(s.playing);
    // Concealment is a little quieter than the sine it stands in for.
    let sine_rms = f64::from(AMPLITUDE) / 2f64.sqrt();
    assert!(
        (out.rms / sine_rms - 1.0).abs() < 0.1,
        "{} vs {sine_rms}",
        out.rms
    );
    // The target settles just above 10 ms of lateness plus the 10 ms margin.
    let settled = out.at(100.0);
    assert!(
        (20.0..22.5).contains(&settled.target_ms),
        "{}",
        settled.target_ms
    );
    // Latency: 5 ms of network plus the playout delay (the target, which counts from the
    // earliest arrivals), plus up to one callback between a frame's deadline and its playing.
    let latency = settled.latency_ms.unwrap();
    let low = 5.0 + settled.target_ms;
    assert!((low..low + 12.0).contains(&latency), "{latency} vs {low}");
}

#[test]
fn clock_skew_keeps_the_buffer_at_its_target() {
    for (sender_ppm, player_ppm) in [(300.0, 0.0), (-300.0, 0.0), (0.0, 300.0), (0.0, -300.0)] {
        let out = run(Run {
            sender_ppm,
            player_ppm,
            ..Run::new(180.0, Link::default())
        });
        let s = &out.stats;
        assert_eq!((s.underruns, s.stalls, s.skipped), (0, 0, 0), "{s:?}");
        // The controller settles within a minute and then tracks the skew: the sender faster
        // means playing faster.
        let (mean, min, max) = error_range(out.after(90.0));
        assert!(mean.abs() < 3.0, "{sender_ppm}/{player_ppm}: mean {mean}");
        assert!(
            min > -12.0 && max < 15.0,
            "{sender_ppm}/{player_ppm}: {min}..{max}"
        );
        let expected = sender_ppm - player_ppm;
        assert!((s.ppm - expected).abs() < 60.0, "{} vs {expected}", s.ppm);
    }
}

#[test]
fn reordered_packets_are_played_in_order() {
    let out = run(Run::new(
        60.0,
        Link {
            reorder: 0.05,
            jitter_ms: 0.0,
            ..Link::default()
        },
    ));
    let s = &out.stats;
    assert_eq!((s.underruns, s.lost, s.late), (0, 0, 0), "{s:?}");
}

#[test]
fn a_delay_spike_grows_the_buffer_once_then_it_shrinks_back() {
    let out = run(Run::new(
        420.0,
        Link {
            spikes: vec![(30.0, 90.0), (60.0, 90.0)],
            ..Link::default()
        },
    ));
    let s = &out.stats;
    // The first spike runs the buffer dry once; the buffer grows by the delay and the target
    // rises, so the second doesn't.
    assert_eq!(
        (s.underruns, s.lost, s.late, s.stalls),
        (1, 0, 0, 0),
        "{s:?}"
    );
    let after_second = out.at(61.0);
    assert!(after_second.target_ms >= 95.0, "{}", after_second.target_ms);
    // Held after the second spike, then down to the minimum.
    let hold_s = HOLD_NS as f64 / 1e9;
    let back = 60.0 + hold_s + (after_second.target_ms - MIN_MS) / DECAY_MS_PER_S;
    assert!(back < 400.0, "{back}");
    assert!(out.at(back - 10.0).target_ms > MIN_MS);
    let (mean, min, max) = error_range(out.after(back + 10.0));
    assert!(
        mean.abs() < 3.0 && min > -12.0 && max < 15.0,
        "{mean} {min}..{max}"
    );
    assert_eq!(out.at(back + 10.0).target_ms, MIN_MS);
}

#[test]
fn a_stopped_stream_starts_over() {
    let out = run(Run::new(
        20.0,
        Link {
            outages: vec![(10.0, 11.0)],
            ..Link::default()
        },
    ));
    let s = &out.stats;
    assert_eq!((s.underruns, s.stalls, s.late), (1, 1, 0), "{s:?}");
    assert!(s.playing);
    let (mean, _, _) = error_range(out.after(15.0));
    assert!(mean.abs() < 5.0, "{mean}");
}
