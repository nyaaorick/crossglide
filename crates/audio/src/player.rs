//! The Mac's half of the stream, without devices: packets in, audio at the output device's rate
//! out. `Player::fill` runs on the audio thread, so nothing in it allocates or blocks; packets
//! reach it through a lock-free queue from `Incoming`, which the network side owns.
//!
//! Time is passed in, in nanoseconds on any clock that both halves share, so tests can drive
//! the player through a simulated network.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::packet::{Header, MAX_PAYLOAD, PacketError, seq_diff};
use crate::resample::Stereo;
use crate::{FRAME_LEN, FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE_HZ, codec};

/// Frames the jitter buffer can hold: 640 ms, far more than the largest target.
const SLOTS: usize = 64;
/// Packets waiting between the network and the audio thread.
const QUEUE: usize = 128;

/// Target depth of the jitter buffer, in ms. It starts at `START`, then follows how late packets
/// arrive: the latest packet of the last `HOLD` plus `MARGIN`, within `MIN..=MAX`. After `HOLD`
/// it comes down by `DECAY` ms per second. M1 measured Wi-Fi delay spikes up to 85 ms, and in M3
/// packets 25-35 ms late came every minute or two, so the hold outlasts that.
pub(crate) const START_MS: f64 = 40.0;
pub(crate) const MIN_MS: f64 = 20.0;
const MAX_MS: f64 = 150.0;
const MARGIN_MS: f64 = 10.0;
pub(crate) const HOLD_NS: u64 = 120_000_000_000;
pub(crate) const DECAY_MS_PER_S: f64 = 0.5;
/// Lateness is measured against the earliest arrival over the last `BUCKETS` seconds.
const BUCKETS: usize = 8;
const BUCKET_NS: u64 = 1_000_000_000;

/// The playout delay (how far playback runs behind the earliest arrivals) is steered towards the
/// target by playing up to `MAX_PPM` faster or slower. Each packet measures it on arrival: the
/// time until it plays plus how late it was. A PI controller acts on its average over
/// `DELAY_TAU_MS`: `KP` ppm per ms of error, plus an integral (`KI` ppm per ms per second) that
/// settles on the clock skew between the two sound cards, up to `MAX_SKEW_PPM`. Playing 1000 ppm
/// fast drains 1 ms per second, so this settles in about 10 s, close to critically damped.
/// 2000 ppm is a pitch change of 3.5 cents.
const KP: f64 = 150.0;
const KI: f64 = 9.0;
const MAX_SKEW_PPM: f64 = 500.0;
/// The integral only learns while the delay is this close to the target: bigger errors come from
/// the target moving, not from the clocks.
const LEARN_WITHIN_MS: f64 = 3.0;
const MAX_PPM: f64 = 2000.0;
const DELAY_TAU_MS: f64 = 1000.0;

/// With nothing to play for this long, the stream is taken to have stopped: the buffer starts
/// over and fills to the target again before playing.
const STALL_FRAMES: u32 = 20;
/// More than this above the target (a burst after a long stall), frames are dropped instead.
const SKIP_ABOVE_MS: f64 = 150.0;

#[derive(Clone, Copy)]
struct Slot {
    seq: u16,
    ts: u32,
    arrived: u64,
    len: u16,
    data: [u8; MAX_PAYLOAD],
}

/// Counters and gauges the agent reads for its log.
#[derive(Default)]
pub struct Stats {
    pub received: AtomicU64,
    /// Frames missing when their turn came, with later ones already there: concealed.
    pub lost: AtomicU64,
    /// Packets that arrived after their frame was concealed: dropped.
    pub late: AtomicU64,
    /// Dropouts: the buffer ran dry and a frame came too late, or nothing came for more than a
    /// frame. A lost packet alone is `lost`, not an underrun.
    pub underruns: AtomicU64,
    /// Frames concealed while the buffer was dry.
    pub dry_frames: AtomicU64,
    /// Times the stream stopped for `STALL_FRAMES` and the buffer started over.
    pub stalls: AtomicU64,
    /// Frames dropped because the buffer was far above its target.
    pub skipped: AtomicU64,
    /// Packets dropped because the audio thread didn't keep up.
    pub overflow: AtomicU64,
    pub decode_errors: AtomicU64,
    pub playing: AtomicBool,
    depth_ms: AtomicU64,
    target_ms: AtomicU64,
    ppm: AtomicU64,
    delay_ms: AtomicU64,
    skew_ppm: AtomicU64,
    /// Latest packet since the last snapshot, in µs.
    max_lateness_us: AtomicU64,
    anchor: Mutex<Option<Anchor>>,
}

/// Which sample was being heard when: the stream timestamp of a sample, and the time it came out
/// of the speakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub ts: u32,
    pub heard_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub underruns: u64,
    pub dry_frames: u64,
    pub stalls: u64,
    pub skipped: u64,
    pub overflow: u64,
    pub decode_errors: u64,
    pub playing: bool,
    /// Audio queued, from the next sample out to the newest frame received.
    pub depth_ms: f64,
    /// Playout delay behind the earliest arrivals, averaged; steered towards `target_ms`.
    pub delay_ms: f64,
    pub target_ms: f64,
    /// Current playback rate correction; positive plays faster.
    pub ppm: f64,
    /// The part of it that makes up for the clocks' skew.
    pub skew_ppm: f64,
    pub max_lateness_ms: f64,
}

impl Stats {
    /// Current values; the maximum lateness starts over.
    pub fn snapshot(&self) -> Snapshot {
        let f = |a: &AtomicU64| f64::from_bits(a.load(Relaxed));
        Snapshot {
            received: self.received.load(Relaxed),
            lost: self.lost.load(Relaxed),
            late: self.late.load(Relaxed),
            underruns: self.underruns.load(Relaxed),
            dry_frames: self.dry_frames.load(Relaxed),
            stalls: self.stalls.load(Relaxed),
            skipped: self.skipped.load(Relaxed),
            overflow: self.overflow.load(Relaxed),
            decode_errors: self.decode_errors.load(Relaxed),
            playing: self.playing.load(Relaxed),
            depth_ms: f(&self.depth_ms),
            delay_ms: f(&self.delay_ms),
            target_ms: f(&self.target_ms),
            ppm: f(&self.ppm),
            skew_ppm: f(&self.skew_ppm),
            max_lateness_ms: self.max_lateness_us.swap(0, Relaxed) as f64 / 1000.0,
        }
    }

    pub fn anchor(&self) -> Option<Anchor> {
        *self.anchor.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn store(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Relaxed);
}

/// The network side: parses packets and queues them for the audio thread.
pub struct Incoming {
    queue: HeapProd<Slot>,
    stats: Arc<Stats>,
}

impl Incoming {
    /// Queues a packet that arrived at `arrived`. Never blocks: if the audio thread has fallen
    /// behind, the packet is dropped.
    pub fn push(&mut self, packet: &[u8], arrived: u64) -> Result<(), PacketError> {
        let (header, payload) = Header::parse(packet)?;
        let mut slot = Slot {
            seq: header.seq,
            ts: header.ts,
            arrived,
            len: payload.len() as u16,
            data: [0; MAX_PAYLOAD],
        };
        slot.data[..payload.len()].copy_from_slice(payload);
        if self.queue.try_push(slot).is_err() {
            self.stats.overflow.fetch_add(1, Relaxed);
        }
        Ok(())
    }
}

pub struct Player {
    queue: HeapCons<Slot>,
    slots: Vec<Slot>,
    present: Vec<bool>,
    /// Next frame to play; `None` while filling up.
    next: Option<u16>,
    /// First frame of this fill.
    start: Option<u16>,
    newest: Option<u16>,
    /// Frames in a row with nothing to play.
    dry: u32,
    last_ts: u32,
    decoder: opus::Decoder,
    frame: Vec<f32>,
    resampler: Stereo,
    device_rate: u32,
    out: Vec<f32>,
    out_pos: usize,
    out_ts: Option<u32>,
    jitter: Jitter,
    /// Averaged playout delay in ms, once playing.
    delay: Option<f64>,
    /// Length of the last output callback, in ms.
    callback_ms: f64,
    skew_ppm: f64,
    steered_at: Option<u64>,
    stats: Arc<Stats>,
}

impl Player {
    /// A player for an output device running at `device_rate` Hz, and the network side that
    /// feeds it.
    pub fn new(device_rate: u32) -> Result<(Self, Incoming), opus::Error> {
        let (prod, cons) = HeapRb::<Slot>::new(QUEUE).split();
        let stats = Arc::new(Stats::default());
        let empty = Slot {
            seq: 0,
            ts: 0,
            arrived: 0,
            len: 0,
            data: [0; MAX_PAYLOAD],
        };
        let resampler = Stereo::new(SAMPLE_RATE_HZ, device_rate, FRAME_SAMPLES, 0.01);
        let out = Vec::with_capacity(resampler.max_output_len());
        let player = Self {
            queue: cons,
            slots: vec![empty; SLOTS],
            present: vec![false; SLOTS],
            next: None,
            start: None,
            newest: None,
            dry: 0,
            last_ts: 0,
            decoder: codec::decoder()?,
            frame: vec![0.0; FRAME_LEN],
            resampler,
            device_rate,
            out,
            out_pos: 0,
            out_ts: None,
            jitter: Jitter::new(),
            delay: None,
            callback_ms: 0.0,
            skew_ppm: 0.0,
            steered_at: None,
            stats: stats.clone(),
        };
        store(&player.stats.target_ms, START_MS);
        Ok((player, Incoming { queue: prod, stats }))
    }

    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }

    /// Fills `out` with interleaved stereo at the device rate. `now` is the current time and
    /// `heard_at` when the first of these samples will come out of the speakers.
    pub fn fill(&mut self, out: &mut [f32], now: u64, heard_at: u64) {
        self.callback_ms = (out.len() / 2) as f64 * 1000.0 / f64::from(self.device_rate);
        self.receive(now);
        if self.staged() == 0 {
            self.produce(now);
        }
        self.publish_anchor(heard_at);
        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames {
            if self.staged() == 0 {
                self.produce(now);
            }
            let n = (frames - done).min(self.staged());
            let from = self.out_pos * 2;
            out[done * 2..(done + n) * 2].copy_from_slice(&self.out[from..from + n * 2]);
            done += n;
            self.out_pos += n;
        }
    }

    /// Output frames ready to copy out.
    fn staged(&self) -> usize {
        self.out.len() / 2 - self.out_pos
    }

    fn staged_ms(&self) -> f64 {
        self.staged() as f64 * 1000.0 / f64::from(self.device_rate)
    }

    /// Moves queued packets into the jitter buffer.
    fn receive(&mut self, now: u64) {
        while let Some(slot) = self.queue.try_pop() {
            self.stats.received.fetch_add(1, Relaxed);
            if let Some(base) = self.next.or(self.start) {
                let ahead = seq_diff(slot.seq, base);
                if ahead < 0 {
                    // Behind the next frame to play: too late. While filling up, it's just an
                    // earlier packet that arrived out of order.
                    let fits = self
                        .newest
                        .is_some_and(|n| seq_diff(n, slot.seq) < SLOTS as i32);
                    if self.next.is_some() || !fits {
                        self.stats.late.fetch_add(1, Relaxed);
                        continue;
                    }
                    self.start = Some(slot.seq);
                    self.last_ts = slot.ts.wrapping_sub(FRAME_SAMPLES as u32);
                } else if ahead >= SLOTS as i32 {
                    // Far ahead of the buffer: the stream jumped. Start over from here.
                    self.restart();
                }
            }
            if self.next.is_none() && self.start.is_none() {
                self.start = Some(slot.seq);
                self.last_ts = slot.ts.wrapping_sub(FRAME_SAMPLES as u32);
            }
            let i = usize::from(slot.seq) % SLOTS;
            self.slots[i] = slot;
            self.present[i] = true;
            if self.newest.is_none_or(|n| seq_diff(slot.seq, n) > 0) {
                self.newest = Some(slot.seq);
            }
            let lateness = self.jitter.on_packet(slot.ts, slot.arrived);
            self.stats
                .max_lateness_us
                .fetch_max((lateness * 1000.0) as u64, Relaxed);
            if let Some(next) = self.next {
                // It plays after what's staged and the frames before it, but it's needed at the
                // start of the callback that reaches it, up to a callback earlier. It had that
                // much time to spare when it arrived, plus however long it waited in the queue.
                let plays_in =
                    self.staged_ms() + f64::from(seq_diff(slot.seq, next) * FRAME_MS as i32);
                let needed_in = plays_in - self.callback_ms;
                let spare = needed_in + now.saturating_sub(slot.arrived) as f64 / 1e6;
                let delay = spare + lateness;
                let alpha = f64::from(FRAME_MS) / DELAY_TAU_MS;
                self.delay = Some(self.delay.map_or(delay, |d| d + (delay - d) * alpha));
            }
        }
    }

    /// Decodes (or conceals) the next frame and resamples it into `out`.
    fn produce(&mut self, now: u64) {
        self.jitter.update(now);
        let target = self.jitter.target();
        self.skip_if_too_deep(target);
        let ts = self.next_frame(target);
        let resampled = self.resampler.process(&self.frame);
        self.out.clear();
        self.out.extend_from_slice(resampled);
        self.out_pos = 0;
        self.out_ts = ts;
        self.steer(target, now);
    }

    /// Puts the next frame in `frame`; returns its timestamp, or `None` for filler that isn't
    /// part of the stream (silence while filling up, concealment while dry).
    fn next_frame(&mut self, target: f64) -> Option<u32> {
        let Some(mut seq) = self.next.or_else(|| self.try_start(target)) else {
            self.frame.fill(0.0);
            return None;
        };
        if self.dry > 0 && !self.has(seq) && self.has_later(seq) {
            // The frame being waited for was lost, not delayed: the concealment played while
            // waiting stood in for it. Go on to the next.
            self.stats.lost.fetch_add(1, Relaxed);
            self.last_ts = self.last_ts.wrapping_add(FRAME_SAMPLES as u32);
            seq = seq.wrapping_add(1);
            self.next = Some(seq);
            self.end_dry(false);
        }
        if self.has(seq) {
            if self.dry > 0 {
                self.end_dry(true);
            }
            let i = usize::from(seq) % SLOTS;
            self.present[i] = false;
            self.next = Some(seq.wrapping_add(1));
            self.stats.playing.store(true, Relaxed);
            let slot = &self.slots[i];
            let decoded = self.decoder.decode_float(
                &slot.data[..usize::from(slot.len)],
                &mut self.frame,
                false,
            );
            self.last_ts = slot.ts;
            if !matches!(decoded, Ok(n) if n == FRAME_SAMPLES) {
                self.stats.decode_errors.fetch_add(1, Relaxed);
                self.conceal();
            }
            return Some(self.last_ts);
        }
        if self.has_later(seq) {
            // Later frames are here, so this one is lost (or too late to wait for).
            self.stats.lost.fetch_add(1, Relaxed);
            self.next = Some(seq.wrapping_add(1));
            self.conceal();
            self.last_ts = self.last_ts.wrapping_add(FRAME_SAMPLES as u32);
            return Some(self.last_ts);
        }
        // Nothing to play: conceal, and wait for this frame rather than skip it. If it was only
        // delayed (a Wi-Fi spike), it plays when it arrives and the buffer has grown by the
        // delay; if it was lost, the next packet to arrive shows that.
        self.dry += 1;
        self.stats.dry_frames.fetch_add(1, Relaxed);
        self.stats.playing.store(false, Relaxed);
        self.conceal();
        if self.dry >= STALL_FRAMES {
            self.stats.underruns.fetch_add(1, Relaxed);
            self.stats.stalls.fetch_add(1, Relaxed);
            self.restart();
        }
        None
    }

    fn has(&self, seq: u16) -> bool {
        let i = usize::from(seq) % SLOTS;
        self.present[i] && self.slots[i].seq == seq
    }

    fn has_later(&self, seq: u16) -> bool {
        self.newest.is_some_and(|n| seq_diff(n, seq) > 0)
    }

    /// Something plays again after `dry` frames of concealment. That was a dropout if the frame
    /// came late (the buffer really ran dry) or the wait was longer than the one frame a lost
    /// packet takes to show.
    fn end_dry(&mut self, came_late: bool) {
        if came_late || self.dry > 1 {
            self.stats.underruns.fetch_add(1, Relaxed);
        }
        self.dry = 0;
    }

    /// Starts playing once the buffer holds `target` ms.
    fn try_start(&mut self, target: f64) -> Option<u16> {
        let (start, newest) = (self.start?, self.newest?);
        let span = f64::from(seq_diff(newest, start) + 1) * f64::from(FRAME_MS);
        if span < target {
            return None;
        }
        self.next = Some(start);
        Some(start)
    }

    fn conceal(&mut self) {
        let concealed = self.decoder.decode_float(&[], &mut self.frame, false);
        if !matches!(concealed, Ok(n) if n == FRAME_SAMPLES) {
            self.frame.fill(0.0);
        }
    }

    /// Empties the buffer; it fills up to the target again before playing.
    fn restart(&mut self) {
        self.present.fill(false);
        self.next = None;
        self.start = None;
        self.newest = None;
        self.dry = 0;
        self.delay = None;
        let _ = self.decoder.reset_state();
        self.stats.playing.store(false, Relaxed);
    }

    /// Queued audio in ms, from the next sample to be copied out to the newest frame received.
    fn depth_ms(&self) -> f64 {
        let queued = match (self.next.or(self.start), self.newest) {
            (Some(base), Some(newest)) if seq_diff(newest, base) >= 0 => {
                f64::from(seq_diff(newest, base) + 1) * f64::from(FRAME_MS)
            }
            _ => 0.0,
        };
        queued + self.staged_ms()
    }

    fn skip_if_too_deep(&mut self, target: f64) {
        if self.next.is_none() || self.depth_ms() - target <= SKIP_ABOVE_MS {
            return;
        }
        for _ in 0..SLOTS {
            if self.depth_ms() <= target {
                break;
            }
            self.next_frame(target);
            self.stats.skipped.fetch_add(1, Relaxed);
        }
        // Measure the delay afresh.
        self.delay = None;
    }

    /// Nudges the playback rate so the playout delay settles at the target.
    fn steer(&mut self, target: f64, now: u64) {
        store(&self.stats.depth_ms, self.depth_ms());
        store(&self.stats.target_ms, target);
        let dt = self
            .steered_at
            .map_or(0.0, |t| (now.saturating_sub(t) as f64 / 1e9).min(0.1));
        self.steered_at = Some(now);
        let Some(delay) = self.delay.filter(|_| self.next.is_some()) else {
            return;
        };
        let error = delay - target;
        let proportional = KP * error;
        // While the target comes down, drain at the same rate (1000 ppm drains 1 ms per second),
        // so the integral doesn't mistake following it for clock skew.
        let follow = if self.jitter.decaying() {
            DECAY_MS_PER_S * 1000.0
        } else {
            0.0
        };
        if error.abs() < LEARN_WITHIN_MS {
            self.skew_ppm = (self.skew_ppm + KI * error * dt).clamp(-MAX_SKEW_PPM, MAX_SKEW_PPM);
        }
        let ppm = (proportional + follow + self.skew_ppm).clamp(-MAX_PPM, MAX_PPM);
        // Too much delay: play faster, i.e. make fewer output samples per input sample.
        self.resampler.adjust(1.0 - ppm * 1e-6);
        store(&self.stats.ppm, ppm);
        store(&self.stats.skew_ppm, self.skew_ppm);
        store(&self.stats.delay_ms, delay);
    }

    /// Records which stream sample the next copied-out sample is.
    fn publish_anchor(&self, heard_at: u64) {
        let Some(ts) = self.out_ts else { return };
        // Output frame k of a chunk comes from input frame (k - delay) / ratio of it.
        let input_frames = (self.out_pos as f64 - self.resampler.delay() as f64)
            * f64::from(SAMPLE_RATE_HZ)
            / f64::from(self.device_rate);
        let ts = ts.wrapping_add_signed(input_frames.round() as i32);
        if let Ok(mut anchor) = self.stats.anchor.try_lock() {
            *anchor = Some(Anchor { ts, heard_at });
        }
    }
}

/// How late packets arrive, and the buffer depth that covers it.
struct Jitter {
    /// Last timestamp seen, and the same unwrapped to 64 bits.
    unwrapped: Option<(u32, i64)>,
    /// Earliest arrival relative to the timestamp, per second, for the last `BUCKETS` seconds.
    buckets: [i64; BUCKETS],
    bucket: usize,
    bucket_start: Option<u64>,
    /// Depth needed by the latest packet recently.
    held: f64,
    hold_until: u64,
    decayed_at: u64,
}

impl Jitter {
    fn new() -> Self {
        Self {
            unwrapped: None,
            buckets: [i64::MAX; BUCKETS],
            bucket: 0,
            bucket_start: None,
            held: START_MS,
            hold_until: 0,
            decayed_at: 0,
        }
    }

    /// Returns how late, in ms, a packet with timestamp `ts` arrived at `arrived`, compared with
    /// the earliest recent arrival.
    fn on_packet(&mut self, ts: u32, arrived: u64) -> f64 {
        let unwrapped = match self.unwrapped {
            Some((last, u)) => u + i64::from(ts.wrapping_sub(last) as i32),
            None => i64::from(ts),
        };
        self.unwrapped = Some((ts, unwrapped));
        // ns per sample at 48 kHz is 62500 / 3.
        let relative = arrived as i64 - unwrapped * 62_500 / 3;

        let start = *self.bucket_start.get_or_insert(arrived);
        if arrived.saturating_sub(start) >= BUCKETS as u64 * BUCKET_NS {
            self.buckets = [i64::MAX; BUCKETS];
            self.bucket_start = Some(arrived);
        } else {
            let mut start = start;
            while arrived >= start + BUCKET_NS {
                self.bucket = (self.bucket + 1) % BUCKETS;
                self.buckets[self.bucket] = i64::MAX;
                start += BUCKET_NS;
            }
            self.bucket_start = Some(start);
        }
        let bucket = &mut self.buckets[self.bucket];
        *bucket = (*bucket).min(relative);
        let earliest = self.buckets.iter().copied().min().unwrap_or(relative);

        let lateness = (relative - earliest) as f64 / 1e6;
        let needed = (lateness + MARGIN_MS).min(MAX_MS);
        if needed >= self.held * 0.9 {
            // Nearly as late as the latest recently: hold that depth longer.
            self.held = self.held.max(needed);
            self.hold_until = arrived + HOLD_NS;
        }
        lateness
    }

    fn update(&mut self, now: u64) {
        let from = self.decayed_at.max(self.hold_until);
        if now > from {
            self.held = (self.held - DECAY_MS_PER_S * (now - from) as f64 / 1e9).max(0.0);
        }
        self.decayed_at = self.decayed_at.max(now);
    }

    fn target(&self) -> f64 {
        self.held.clamp(MIN_MS, MAX_MS)
    }

    /// Whether the target is coming down (at `DECAY_MS_PER_S`).
    fn decaying(&self) -> bool {
        self.decayed_at > self.hold_until && self.held > MIN_MS
    }
}
