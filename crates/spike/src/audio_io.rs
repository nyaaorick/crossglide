//! Device and file I/O shared by the spike commands: loopback capture, playback,
//! test sources and WAV files. All audio here is interleaved f32.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossglide_audio::SAMPLE_RATE_HZ;
use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};

/// Interleaved stereo samples per millisecond at 48 kHz.
pub const SAMPLES_PER_MS: usize = SAMPLE_RATE_HZ as usize / 1000 * 2;

/// Converts a count of interleaved 48 kHz stereo samples to milliseconds.
pub fn to_ms(samples: usize) -> f64 {
    samples as f64 / SAMPLES_PER_MS as f64
}

/// Appends audio with `channels` interleaved channels to `out` as interleaved stereo.
/// Mono is duplicated; channels after the first two are dropped.
pub fn to_stereo(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels == 1 {
        for &s in data {
            out.extend_from_slice(&[s, s]);
        }
    } else {
        for frame in data.chunks_exact(channels) {
            out.extend_from_slice(&frame[..2]);
        }
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown device".into())
}

#[derive(Default)]
pub struct CaptureStats {
    pub callbacks: AtomicU64,
    /// Frames delivered by the device, at the device's sample rate.
    pub frames: AtomicU64,
    /// Callbacks that arrived more than 50 ms after the previous one.
    pub gaps: AtomicU64,
    pub max_gap_us: AtomicU64,
    pub errors: AtomicU64,
}

/// A running loopback capture. Capture stops when this is dropped.
pub struct Loopback {
    _stream: cpal::Stream,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub stats: Arc<CaptureStats>,
}

/// Captures what the default output device is playing and passes it to `on_stereo`
/// as interleaved stereo at the device's sample rate. Runs on the audio thread.
pub fn start_loopback(mut on_stereo: impl FnMut(&[f32]) + Send + 'static) -> Result<Loopback> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no default output device")?;
    let config = device
        .default_output_config()
        .context("reading the output device's format")?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        bail!(
            "output device format is {}; the spike only handles f32",
            config.sample_format()
        );
    }
    let channels = config.channels();
    let stats = Arc::new(CaptureStats::default());
    let (cb_stats, err_stats) = (stats.clone(), stats.clone());
    let mut stereo = Vec::with_capacity(SAMPLE_RATE_HZ as usize * 2);
    let mut last: Option<Instant> = None;

    let stream = device.build_input_stream(
        config.config(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let now = Instant::now();
            if let Some(prev) = last {
                let gap_us = now.duration_since(prev).as_micros() as u64;
                cb_stats.max_gap_us.fetch_max(gap_us, Relaxed);
                if gap_us > 50_000 {
                    cb_stats.gaps.fetch_add(1, Relaxed);
                }
            }
            last = Some(now);
            cb_stats.callbacks.fetch_add(1, Relaxed);
            cb_stats
                .frames
                .fetch_add((data.len() / channels as usize) as u64, Relaxed);
            stereo.clear();
            to_stereo(data, channels as usize, &mut stereo);
            on_stereo(&stereo);
        },
        move |err| {
            err_stats.errors.fetch_add(1, Relaxed);
            eprintln!("capture error: {err}");
        },
        None,
    )?;
    stream.play()?;

    Ok(Loopback {
        _stream: stream,
        device: device_name(&device),
        sample_rate: config.sample_rate(),
        channels,
        stats,
    })
}

#[derive(Default)]
pub struct PlayStats {
    pub underruns: AtomicU64,
    /// Samples dropped because the buffer grew past the target plus 100 ms.
    pub skipped: AtomicU64,
    /// Interleaved stereo samples buffered at the last callback.
    pub buffered: AtomicUsize,
    pub playing: AtomicBool,
    pub errors: AtomicU64,
}

/// A running playback stream. Playback stops when this is dropped.
pub struct Playback {
    _stream: cpal::Stream,
    pub device: String,
    pub channels: u16,
    pub stats: Arc<PlayStats>,
}

/// Plays interleaved 48 kHz stereo from `cons` on the default output device. Waits
/// until `buffer_ms` is buffered before starting, and again after every underrun.
pub fn start_playback(mut cons: HeapCons<f32>, buffer_ms: u32, volume: f32) -> Result<Playback> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no default output device")?;
    let config = device
        .default_output_config()
        .context("reading the output device's format")?;
    if config.sample_rate() != SAMPLE_RATE_HZ {
        bail!(
            "output device runs at {} Hz; set it to 48000 Hz (macOS: Audio MIDI Setup)",
            config.sample_rate()
        );
    }
    if config.sample_format() != cpal::SampleFormat::F32 {
        bail!(
            "output device format is {}; the spike only handles f32",
            config.sample_format()
        );
    }
    let channels = config.channels() as usize;
    let target = buffer_ms as usize * SAMPLES_PER_MS;
    let high_water = target + 100 * SAMPLES_PER_MS;
    let stats = Arc::new(PlayStats::default());
    let (cb_stats, err_stats) = (stats.clone(), stats.clone());
    let mut stereo = vec![0.0f32; 8192];
    let mut priming = true;

    let stream = device.build_output_stream(
        config.config(),
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            out.fill(0.0);
            let frames = out.len() / channels;
            let available = cons.occupied_len();
            cb_stats.buffered.store(available, Relaxed);
            if priming {
                if available < target {
                    return;
                }
                priming = false;
                cb_stats.playing.store(true, Relaxed);
            }
            if available > high_water {
                // Drop back to the target; keep an even count so L and R stay aligned.
                let excess = (available - target) & !1;
                cons.skip(excess);
                cb_stats.skipped.fetch_add(excess as u64, Relaxed);
            }
            if stereo.len() < frames * 2 {
                stereo.resize(frames * 2, 0.0);
            }
            let got = cons.pop_slice(&mut stereo[..frames * 2]);
            if got < frames * 2 {
                cb_stats.underruns.fetch_add(1, Relaxed);
                cb_stats.playing.store(false, Relaxed);
                priming = true;
            }
            for (frame, &[l, r]) in out
                .chunks_exact_mut(channels)
                .zip(stereo[..got].as_chunks::<2>().0)
            {
                if channels == 1 {
                    frame[0] = (l + r) * 0.5 * volume;
                } else {
                    frame[0] = l * volume;
                    frame[1] = r * volume;
                }
            }
        },
        move |err| {
            err_stats.errors.fetch_add(1, Relaxed);
            eprintln!("playback error: {err}");
        },
        None,
    )?;
    stream.play()?;

    Ok(Playback {
        _stream: stream,
        device: device_name(&device),
        channels: channels as u16,
        stats,
    })
}

/// A test signal producing interleaved 48 kHz stereo.
pub enum Source {
    Sine { freq: f32, phase: f32 },
    Wav { samples: Vec<f32>, pos: usize },
}

impl Source {
    /// Opens `wav` if given, otherwise a sine at `freq` Hz.
    pub fn open(wav: Option<&Path>, freq: f32) -> Result<Self> {
        Ok(match wav {
            Some(path) => Source::Wav {
                samples: read_wav_48k_stereo(path)?,
                pos: 0,
            },
            None => Source::Sine { freq, phase: 0.0 },
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Source::Sine { freq, .. } => format!("{freq} Hz sine"),
            Source::Wav { samples, .. } => {
                format!("WAV, {:.1} s, looped", to_ms(samples.len()) / 1000.0)
            }
        }
    }

    pub fn fill(&mut self, out: &mut [f32]) {
        match self {
            Source::Sine { freq, phase } => {
                let step = *freq / SAMPLE_RATE_HZ as f32;
                for lr in out.as_chunks_mut::<2>().0 {
                    let s = 0.5 * (std::f32::consts::TAU * *phase).sin();
                    *lr = [s, s];
                    *phase = (*phase + step).fract();
                }
            }
            Source::Wav { samples, pos } => {
                for s in out {
                    *s = samples[*pos];
                    *pos = (*pos + 1) % samples.len();
                }
            }
        }
    }
}

/// Reads a 48 kHz WAV file as interleaved stereo f32.
pub fn read_wav_48k_stereo(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE_HZ {
        bail!(
            "{} is {} Hz; convert it first, e.g. `afconvert -f WAVE -d LEF32@48000 in.wav out.wav`",
            path.display(),
            spec.sample_rate
        );
    }
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mut stereo = Vec::with_capacity(raw.len() * 2 / spec.channels as usize);
    to_stereo(&raw, spec.channels as usize, &mut stereo);
    if stereo.is_empty() {
        bail!("{} has no audio", path.display());
    }
    Ok(stereo)
}

pub fn wav_writer(
    path: &Path,
    sample_rate: u32,
) -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    hound::WavWriter::create(path, spec).with_context(|| format!("creating {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_stereo_duplicates_mono_and_drops_extra_channels() {
        let mut out = Vec::new();
        to_stereo(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);

        out.clear();
        // Two frames of 4-channel audio.
        to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 4, &mut out);
        assert_eq!(out, [1.0, 2.0, 5.0, 6.0]);
    }

    #[test]
    fn wav_source_loops() {
        let mut src = Source::Wav {
            samples: vec![1.0, 2.0, 3.0, 4.0],
            pos: 0,
        };
        let mut out = [0.0; 6];
        src.fill(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 1.0, 2.0]);
    }
}
