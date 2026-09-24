//! `send` and `recv`: the naive stream. Each UDP packet is a 2-byte big-endian
//! sequence number followed by one 10 ms Opus frame. No QUIC, no jitter-buffer
//! adaptation and no drift compensation; those are M2 and M3.

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering::Relaxed;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossglide_audio::{FRAME_MS, SAMPLE_RATE_HZ};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};

use crate::audio_io::{SAMPLES_PER_MS, Source, start_loopback, start_playback, to_ms};
use crate::codec::{FRAME_LEN, MAX_PACKET, new_decoder, new_encoder};

const REPORT_EVERY: Duration = Duration::from_secs(5);

/// Missing frames concealed with Opus PLC before giving up and just resuming.
const MAX_CONCEAL: usize = 5;

struct Sender {
    socket: UdpSocket,
    encoder: opus::Encoder,
    seq: u16,
    packet: [u8; MAX_PACKET],
    sent: u64,
    bytes: u64,
    errors: u64,
    drop_every: Option<u64>,
    dropped: u64,
}

impl Sender {
    fn send(&mut self, frame: &[f32]) -> Result<()> {
        self.packet[..2].copy_from_slice(&self.seq.to_be_bytes());
        let n = self.encoder.encode_float(frame, &mut self.packet[2..])?;
        let packet_no = self.sent + self.errors + self.dropped + 1;
        if self
            .drop_every
            .is_some_and(|every| packet_no.is_multiple_of(every))
        {
            self.dropped += 1;
            self.seq = self.seq.wrapping_add(1);
            return Ok(());
        }
        match self.socket.send(&self.packet[..2 + n]) {
            Ok(_) => {
                self.sent += 1;
                self.bytes += (2 + n) as u64;
            }
            // No receiver yet: the OS reports the previous packet as refused (Windows may
            // say reset). Keep going.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset
                ) =>
            {
                self.errors += 1
            }
            Err(e) => return Err(e.into()),
        }
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }

    fn report(&self, elapsed: Duration, extra: &str) {
        println!(
            "t={:>4}s sent={} ({:.0} kbps) refused={} dropped={}{extra}",
            elapsed.as_secs(),
            self.sent,
            self.bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1000.0,
            self.errors,
            self.dropped
        );
    }
}

/// Streams `source`, or the default output device when `source` is `None`, to `to`.
/// `drop_every` skips every Nth packet to simulate loss.
pub fn send(
    to: SocketAddr,
    source: Option<Source>,
    seconds: Option<u64>,
    drop_every: Option<u64>,
) -> Result<()> {
    let socket = UdpSocket::bind(if to.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    socket.connect(to)?;
    let mut sender = Sender {
        socket,
        encoder: new_encoder()?,
        seq: 0,
        packet: [0; MAX_PACKET],
        sent: 0,
        bytes: 0,
        errors: 0,
        drop_every,
        dropped: 0,
    };
    let limit = seconds.map(Duration::from_secs);
    let mut frame = vec![0.0f32; FRAME_LEN];
    let start = Instant::now();
    let mut next_report = REPORT_EVERY;

    match source {
        Some(mut source) => {
            println!("sending {} to {to}", source.describe());
            // Pace frames by the clock, one every FRAME_MS.
            for tick in 0u64.. {
                let due = Duration::from_millis(tick * u64::from(FRAME_MS));
                if let Some(wait) = due.checked_sub(start.elapsed()) {
                    sleep(wait);
                }
                source.fill(&mut frame);
                sender.send(&frame)?;
                if start.elapsed() >= next_report {
                    sender.report(start.elapsed(), "");
                    next_report += REPORT_EVERY;
                }
                if limit.is_some_and(|l| start.elapsed() >= l) {
                    break;
                }
            }
        }
        None => {
            let (mut prod, mut cons) = HeapRb::<f32>::new(1000 * SAMPLES_PER_MS).split();
            let loopback = start_loopback(move |stereo| {
                prod.push_slice(stereo);
            })?;
            if loopback.sample_rate != SAMPLE_RATE_HZ {
                bail!(
                    "'{}' runs at {} Hz; set it to 48000 Hz (Windows: Settings > System > Sound > \
                     the device > Output settings > Format)",
                    loopback.device,
                    loopback.sample_rate
                );
            }
            println!(
                "sending loopback of '{}' ({} channels) to {to}",
                loopback.device, loopback.channels
            );
            // Paced by the capture device: send whenever a full frame is buffered.
            loop {
                while cons.occupied_len() >= FRAME_LEN {
                    cons.pop_slice(&mut frame);
                    sender.send(&frame)?;
                }
                sleep(Duration::from_millis(2));
                if start.elapsed() >= next_report {
                    let stats = &loopback.stats;
                    let extra = format!(
                        " | capture gaps>50ms={} longest={:.0}ms errors={}",
                        stats.gaps.load(Relaxed),
                        stats.max_gap_us.load(Relaxed) as f64 / 1000.0,
                        stats.errors.load(Relaxed)
                    );
                    sender.report(start.elapsed(), &extra);
                    next_report += REPORT_EVERY;
                }
                if limit.is_some_and(|l| start.elapsed() >= l) {
                    break;
                }
            }
        }
    }
    sender.report(start.elapsed(), " (done)");
    Ok(())
}

#[derive(Default)]
struct RecvStats {
    received: u64,
    lost: u64,
    late: u64,
    concealed: u64,
    overflow: u64,
    bytes: u64,
}

fn push(prod: &mut HeapProd<f32>, samples: &[f32], stats: &mut RecvStats) {
    let pushed = prod.push_slice(samples);
    stats.overflow += (samples.len() - pushed) as u64;
}

/// Receives a `send` stream on `listen` and plays it with a fixed `buffer_ms` buffer.
pub fn recv(listen: SocketAddr, buffer_ms: u32, volume: f32, seconds: Option<u64>) -> Result<()> {
    let socket = UdpSocket::bind(listen)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let (mut prod, cons) = HeapRb::<f32>::new(2000 * SAMPLES_PER_MS).split();
    let playback = start_playback(cons, buffer_ms, volume)?;
    println!(
        "listening on {listen}, playing on '{}' with a {buffer_ms} ms buffer",
        playback.device
    );

    let mut decoder = new_decoder()?;
    // Room for the longest Opus packet (120 ms), though the sender uses 10 ms frames.
    let mut pcm = vec![0.0f32; 120 * SAMPLES_PER_MS];
    let mut buf = [0u8; 2048];
    let mut sender: Option<SocketAddr> = None;
    let mut expected: Option<u16> = None;
    let mut stats = RecvStats::default();
    let limit = seconds.map(Duration::from_secs);
    let start = Instant::now();
    let mut next_report = REPORT_EVERY;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if sender != Some(from) {
                    println!("stream from {from}");
                    sender = Some(from);
                    expected = None;
                    decoder.reset_state()?;
                }
                if n < 2 {
                    continue;
                }
                let seq = u16::from_be_bytes([buf[0], buf[1]]);
                if let Some(exp) = expected {
                    let ahead = seq.wrapping_sub(exp) as i16;
                    if ahead < 0 {
                        stats.late += 1;
                        continue;
                    }
                    if ahead > 0 {
                        stats.lost += ahead as u64;
                        for _ in 0..(ahead as usize).min(MAX_CONCEAL) {
                            let got = decoder.decode_float(&[], &mut pcm[..FRAME_LEN], false)?;
                            push(&mut prod, &pcm[..got * 2], &mut stats);
                            stats.concealed += 1;
                        }
                    }
                }
                expected = Some(seq.wrapping_add(1));
                stats.received += 1;
                stats.bytes += n as u64;
                let got = decoder.decode_float(&buf[2..n], &mut pcm, false)?;
                push(&mut prod, &pcm[..got * 2], &mut stats);
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }

        let elapsed = start.elapsed();
        let done = limit.is_some_and(|l| elapsed >= l);
        if elapsed >= next_report || done {
            let play = &playback.stats;
            println!(
                "t={:>4}s rx={} lost={} late={} concealed={} | buffer={:.0}ms {} underruns={} \
                 skipped={:.0}ms overflow={:.0}ms errors={} | {:.0} kbps",
                elapsed.as_secs(),
                stats.received,
                stats.lost,
                stats.late,
                stats.concealed,
                to_ms(play.buffered.load(Relaxed)),
                if play.playing.load(Relaxed) {
                    "playing"
                } else {
                    "waiting"
                },
                play.underruns.load(Relaxed),
                to_ms(play.skipped.load(Relaxed) as usize),
                to_ms(stats.overflow as usize),
                play.errors.load(Relaxed),
                stats.bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1000.0,
            );
            next_report += REPORT_EVERY;
        }
        if done {
            return Ok(());
        }
    }
}
