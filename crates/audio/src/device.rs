//! The sound devices, through cpal: loopback capture of the PC's default output device, and
//! playback on the Mac's default output device. Each runs on a thread of its own, which owns the
//! cpal stream and stops when its handle is dropped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};

use crate::packet::PacketError;
use crate::player::{Incoming, Player, Stats};
use crate::resample::Stereo;
use crate::sender::{Packetizer, SilenceFill};
use crate::{FRAME_LEN, FRAME_SAMPLES, SAMPLE_RATE_HZ};

/// What the PC sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Whatever the default output device plays.
    Loopback,
    /// A 440 Hz tone, for testing without a loopback device.
    Tone,
}

/// Why a device stopped: set by its error callback, or by the thread driving it.
#[derive(Clone, Default)]
struct Failure(Arc<Mutex<Option<String>>>);

impl Failure {
    fn set(&self, reason: String) {
        let mut failure = self.0.lock().unwrap_or_else(|e| e.into_inner());
        failure.get_or_insert(reason);
    }

    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Reports stream errors: an xrun (a late callback) is counted, anything else stops the stream.
fn on_error(failure: Failure, xruns: Arc<AtomicU64>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |err| {
        if err.kind() == cpal::ErrorKind::Xrun {
            xruns.fetch_add(1, Relaxed);
        } else {
            failure.set(err.to_string());
        }
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown device".into())
}

/// The default output device and its format; only f32 samples are handled.
fn default_output() -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .context("there's no default output device")?;
    // On Windows the default device is a virtual one that follows the system default, and
    // opening it a second time in the same process fails ("Cannot change thread mode after it
    // is set"). Use the device it points to now: a new default takes an agent restart anyway.
    let device = default
        .id()
        .ok()
        .and_then(|id| {
            host.output_devices()
                .ok()?
                .find(|d| d.id().is_ok_and(|d| d == id))
        })
        .unwrap_or(default);
    let config = device
        .default_output_config()
        .with_context(|| format!("can't read the format of '{}'", device_name(&device)))?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        bail!(
            "'{}' uses {} samples; only f32 is supported",
            device_name(&device),
            config.sample_format()
        );
    }
    Ok((device, config))
}

/// Runs `body` on a new thread. It reports whether it started before running on until `stop`
/// is set; this waits for that report.
fn spawn<T: Send + 'static>(
    name: &str,
    body: impl FnOnce(mpsc::Sender<Result<T>>, Arc<AtomicBool>) + Send + 'static,
) -> Result<(T, Arc<AtomicBool>, JoinHandle<()>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready, started) = mpsc::channel();
    let thread = thread::Builder::new().name(name.into()).spawn({
        let stop = stop.clone();
        move || body(ready, stop)
    })?;
    match started.recv() {
        Ok(Ok(value)) => Ok((value, stop, thread)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(anyhow!("the {name} thread stopped while starting"))
        }
    }
}

/// Counters for the PC's log.
#[derive(Default)]
pub struct SendStats {
    pub packets: AtomicU64,
    /// Silence frames sent while loopback delivered nothing.
    pub silent: AtomicU64,
    pub xruns: Arc<AtomicU64>,
    /// Captured frames dropped because the encoder fell behind.
    pub overflow: AtomicU64,
}

/// Capture and encoding, running. Stops when dropped.
pub struct Sending {
    /// The device and its rate, or the test tone.
    pub device: String,
    pub stats: Arc<SendStats>,
    failure: Failure,
    mark: Arc<Mutex<Option<(u32, Instant)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sending {
    /// Starts capturing and passes each packet to `emit`, on the capture thread. Blocks until
    /// the device is open.
    pub fn start(source: Source, emit: impl FnMut(&[u8]) + Send + 'static) -> Result<Self> {
        let stats = Arc::new(SendStats::default());
        let failure = Failure::default();
        let mark = Arc::new(Mutex::new(None));
        let capture = Capture {
            stats: stats.clone(),
            failure: failure.clone(),
            mark: mark.clone(),
        };
        let (device, stop, thread) = spawn("audio capture", move |ready, stop| match source {
            Source::Loopback => capture.loopback(ready, &stop, emit),
            Source::Tone => capture.tone(ready, &stop, emit),
        })?;
        Ok(Self {
            device,
            stats,
            failure,
            mark,
            stop,
            thread: Some(thread),
        })
    }

    /// Why capture stopped, if it did.
    pub fn failure(&self) -> Option<String> {
        self.failure.get()
    }

    /// The timestamp of a recently sent frame, and when its first sample was captured. The Mac
    /// uses it to measure latency.
    pub fn mark(&self) -> Option<(u32, Instant)> {
        *self.mark.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Capture {
    stats: Arc<SendStats>,
    failure: Failure,
    mark: Arc<Mutex<Option<(u32, Instant)>>>,
}

/// How often the capture thread wakes to encode what has arrived.
const POLL: Duration = Duration::from_millis(2);

impl Capture {
    fn loopback(
        self,
        ready: mpsc::Sender<Result<String>>,
        stop: &AtomicBool,
        mut emit: impl FnMut(&[u8]),
    ) {
        let opened = (|| -> Result<_> {
            let (device, config) = default_output()?;
            let name = device_name(&device);
            let rate = config.sample_rate();
            let channels = usize::from(config.channels());
            // A second of device audio between the callback and this thread.
            let (mut prod, cons) = HeapRb::<f32>::new(rate as usize * 2).split();
            // The index of the first frame of the latest callback, and when it was captured.
            let anchor = Arc::new(Mutex::new(None::<(u64, Instant)>));
            let callback_anchor = anchor.clone();
            let mut captured = 0u64;
            let stats = self.stats.clone();
            let stream = device.build_input_stream(
                config.config(),
                move |data: &[f32], info: &cpal::InputCallbackInfo| {
                    // Nothing here allocates or waits.
                    let t = info.timestamp();
                    let delay = t.callback.saturating_duration_since(t.capture);
                    if let Ok(mut a) = callback_anchor.try_lock() {
                        *a = Some((captured, Instant::now() - delay));
                    }
                    let frames = data.len() / channels;
                    captured += frames as u64;
                    // Whole callbacks only, so left and right never swap.
                    if prod.vacant_len() < frames * 2 {
                        stats.overflow.fetch_add(frames as u64, Relaxed);
                    } else if channels == 2 {
                        prod.push_slice(data);
                    } else {
                        for frame in data.chunks_exact(channels) {
                            let right = if channels == 1 { frame[0] } else { frame[1] };
                            prod.push_slice(&[frame[0], right]);
                        }
                    }
                },
                on_error(self.failure.clone(), self.stats.xruns.clone()),
                None,
            )?;
            stream.play()?;
            let packetizer =
                Packetizer::new().map_err(|e| anyhow!("can't create the Opus encoder: {e}"))?;
            Ok((stream, name, rate, cons, anchor, packetizer))
        })();
        let (stream, name, rate, cons, anchor, mut packetizer) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                let _ = ready.send(Err(e.context("can't capture the output device")));
                return;
            }
        };
        let _ = ready.send(Ok(format!("'{name}' ({rate} Hz)")));

        let mut encoder = Encoder::new(rate, cons);
        let mut silence = SilenceFill::new();
        let start = Instant::now();
        let mut emit = |packet: &[u8]| {
            self.stats.packets.fetch_add(1, Relaxed);
            emit(packet);
        };
        while !stop.load(Relaxed) && self.failure.get().is_none() {
            thread::sleep(POLL);
            let sent = match encoder.run(&mut packetizer, &mut emit) {
                Ok(sent) => sent,
                Err(e) => {
                    self.failure.set(format!("Opus encoding failed: {e}"));
                    break;
                }
            };
            let anchored = *anchor.lock().unwrap_or_else(|e| e.into_inner());
            if sent > 0
                && let Some((frame, at)) = anchored
            {
                let offset =
                    (encoder.newest_frame_start(&packetizer) - frame as f64) / f64::from(rate);
                let captured = if offset >= 0.0 {
                    at + Duration::from_secs_f64(offset)
                } else {
                    at.checked_sub(Duration::from_secs_f64(-offset))
                        .unwrap_or(at)
                };
                let ts = packetizer.next_ts().wrapping_sub(FRAME_SAMPLES as u32);
                *self.mark.lock().unwrap_or_else(|e| e.into_inner()) = Some((ts, captured));
            }
            for _ in 0..silence.after(start.elapsed(), sent) {
                if let Err(e) = packetizer.silence(&mut emit) {
                    self.failure.set(format!("Opus encoding failed: {e}"));
                    break;
                }
                self.stats.silent.fetch_add(1, Relaxed);
            }
        }
        drop(stream);
    }

    fn tone(
        self,
        ready: mpsc::Sender<Result<String>>,
        stop: &AtomicBool,
        mut emit: impl FnMut(&[u8]),
    ) {
        let mut packetizer = match Packetizer::new() {
            Ok(p) => p,
            Err(e) => {
                let _ = ready.send(Err(anyhow!("can't create the Opus encoder: {e}")));
                return;
            }
        };
        let _ = ready.send(Ok("a 440 Hz test tone".into()));
        let start = Instant::now();
        let mut frame = vec![0.0f32; FRAME_LEN];
        let mut phase = 0.0f32;
        for n in 0u64.. {
            if stop.load(Relaxed) {
                break;
            }
            // Made at `due`, as if it had just been captured.
            let due = start + Duration::from_millis(n * 10);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            for pair in frame.as_chunks_mut::<2>().0 {
                let s = 0.2 * (std::f32::consts::TAU * phase).sin();
                *pair = [s, s];
                phase = (phase + 440.0 / SAMPLE_RATE_HZ as f32).fract();
            }
            let ts = packetizer.next_ts();
            let stats = &self.stats;
            let sent = packetizer.push(&frame, &mut |packet| {
                stats.packets.fetch_add(1, Relaxed);
                emit(packet);
            });
            if let Err(e) = sent {
                self.failure.set(format!("Opus encoding failed: {e}"));
                break;
            }
            *self.mark.lock().unwrap_or_else(|e| e.into_inner()) = Some((ts, due));
        }
    }
}

/// Moves captured device audio through the resampler (if the device isn't at 48 kHz) into the
/// packetizer.
struct Encoder {
    cons: HeapCons<f32>,
    rate: u32,
    resampler: Option<Stereo>,
    /// Device audio waiting for a whole resampler chunk.
    pending: Vec<f32>,
    /// Device frames taken from the ring so far.
    taken: u64,
}

impl Encoder {
    fn new(rate: u32, cons: HeapCons<f32>) -> Self {
        let resampler = (rate != SAMPLE_RATE_HZ)
            .then(|| Stereo::new(rate, SAMPLE_RATE_HZ, rate as usize / 100, 0.0));
        Self {
            cons,
            rate,
            resampler,
            pending: Vec::with_capacity(rate as usize / 50),
            taken: 0,
        }
    }

    /// Encodes everything captured so far; returns how many frames went out.
    fn run(
        &mut self,
        packetizer: &mut Packetizer,
        emit: &mut dyn FnMut(&[u8]),
    ) -> Result<usize, opus::Error> {
        let mut buf = [0.0f32; 1024];
        let mut sent = 0;
        loop {
            // Whole frames only: left and right stay together.
            let n = (self.cons.occupied_len() & !1).min(buf.len());
            if n == 0 {
                break;
            }
            self.cons.pop_slice(&mut buf[..n]);
            self.taken += n as u64 / 2;
            match &mut self.resampler {
                None => sent += packetizer.push(&buf[..n], emit)?,
                Some(rs) => {
                    self.pending.extend_from_slice(&buf[..n]);
                    let chunk = rs.chunk() * 2;
                    let mut used = 0;
                    while self.pending.len() - used >= chunk {
                        let out = rs.process(&self.pending[used..used + chunk]);
                        sent += packetizer.push(out, emit)?;
                        used += chunk;
                    }
                    self.pending.drain(..used);
                }
            }
        }
        Ok(sent)
    }

    /// The device frame (fractional) that the first sample of the newest packet came from.
    fn newest_frame_start(&self, packetizer: &Packetizer) -> f64 {
        let per_48k = f64::from(self.rate) / f64::from(SAMPLE_RATE_HZ);
        let waiting = (self.pending.len() / 2) as f64;
        let resampler_delay = self
            .resampler
            .as_ref()
            .map_or(0.0, |rs| rs.delay() as f64 * per_48k);
        // The packetizer's partial frame and the newest frame all came after that sample.
        let after = (packetizer.pending() / 2 + FRAME_SAMPLES) as f64 * per_48k;
        self.taken as f64 - waiting - resampler_delay - after
    }
}

/// What the output device reports about itself while playing.
#[derive(Default)]
pub struct OutputStats {
    /// Late callbacks.
    pub xruns: Arc<AtomicU64>,
    /// Frames in the latest callback.
    pub callback_frames: AtomicU64,
    /// Time from the latest callback until its first sample is heard, in µs.
    pub latency_us: AtomicU64,
}

/// Playback, running. Stops when dropped.
pub struct Playing {
    /// The device and its rate.
    pub device: String,
    /// The player's clock counts nanoseconds from here.
    pub epoch: Instant,
    pub stats: Arc<Stats>,
    pub output: Arc<OutputStats>,
    failure: Failure,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The network side of `Playing`: hands packets to the audio thread.
pub struct Receiver {
    incoming: Incoming,
    epoch: Instant,
}

impl Receiver {
    pub fn push(&mut self, packet: &[u8]) -> Result<(), PacketError> {
        let now = self.epoch.elapsed().as_nanos() as u64;
        self.incoming.push(packet, now)
    }
}

/// Largest callback handled in one go; bigger ones are split.
const MAX_CALLBACK: usize = 4096;
/// Callback size asked of the output device, in frames: 5.3 ms at 48 kHz.
const PLAYBACK_BUFFER: u32 = 256;

impl Playing {
    /// Opens the default output device and starts playing (silence until packets arrive).
    pub fn start() -> Result<(Self, Receiver)> {
        let failure = Failure::default();
        let output = Arc::new(OutputStats::default());
        let epoch = Instant::now();
        let shared = (failure.clone(), output.clone());
        let ((device, stats, incoming), stop, thread) =
            spawn("audio playback", move |ready, stop| {
                let (failure, output) = shared;
                play(ready, &stop, failure, output, epoch);
            })?;
        Ok((
            Self {
                device,
                epoch,
                stats,
                output,
                failure,
                stop,
                thread: Some(thread),
            },
            Receiver { incoming, epoch },
        ))
    }

    /// Why playback stopped, if it did.
    pub fn failure(&self) -> Option<String> {
        self.failure.get()
    }
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Started = (String, Arc<Stats>, Incoming);

fn play(
    ready: mpsc::Sender<Result<Started>>,
    stop: &AtomicBool,
    failure: Failure,
    output: Arc<OutputStats>,
    epoch: Instant,
) {
    let opened = (|| -> Result<_> {
        let (device, config) = default_output()?;
        let name = device_name(&device);
        let rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let mut stream_config = config.config();
        // A short callback: a frame must be ready a callback before it plays, and the device's
        // own buffer grows with it too.
        if let cpal::SupportedBufferSize::Range { min, max } = config.buffer_size()
            && (*min..=*max).contains(&PLAYBACK_BUFFER)
        {
            stream_config.buffer_size = cpal::BufferSize::Fixed(PLAYBACK_BUFFER);
        }
        let xruns = output.xruns.clone();
        let callback_output = output.clone();
        let (mut player, incoming) =
            Player::new(rate).map_err(|e| anyhow!("can't create the Opus decoder: {e}"))?;
        let stats = player.stats();
        let mut stereo = vec![0.0f32; MAX_CALLBACK * 2];
        let stream = device.build_output_stream(
            stream_config,
            move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                // Nothing here allocates or waits.
                let t = info.timestamp();
                let latency = t.playback.saturating_duration_since(t.callback);
                let output = &callback_output;
                output
                    .callback_frames
                    .store((out.len() / channels) as u64, Relaxed);
                output.latency_us.store(latency.as_micros() as u64, Relaxed);
                let now = epoch.elapsed().as_nanos() as u64;
                let mut heard_at = now + latency.as_nanos() as u64;
                for block in out.chunks_mut(MAX_CALLBACK * channels) {
                    let frames = block.len() / channels;
                    let stereo = &mut stereo[..frames * 2];
                    player.fill(stereo, now, heard_at);
                    heard_at += frames as u64 * 1_000_000_000 / u64::from(rate);
                    for (frame, &[l, r]) in block
                        .chunks_exact_mut(channels)
                        .zip(stereo.as_chunks::<2>().0)
                    {
                        if channels == 1 {
                            frame[0] = (l + r) * 0.5;
                        } else {
                            frame[0] = l;
                            frame[1] = r;
                            frame[2..].fill(0.0);
                        }
                    }
                }
            },
            on_error(failure.clone(), xruns),
            None,
        )?;
        stream.play()?;
        Ok((stream, format!("'{name}' ({rate} Hz)"), stats, incoming))
    })();
    match opened {
        Ok((stream, name, stats, incoming)) => {
            let _ = ready.send(Ok((name, stats, incoming)));
            while !stop.load(Relaxed) && failure.get().is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            drop(stream);
        }
        Err(e) => {
            let _ = ready.send(Err(e.context("can't play on the output device")));
        }
    }
}
