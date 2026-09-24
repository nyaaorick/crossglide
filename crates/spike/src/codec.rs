//! Opus setup shared by `send` and `recv`, and the `opus` round-trip command.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossglide_audio::{CHANNELS, FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE_HZ};

use crate::audio_io::{read_wav_48k_stereo, wav_writer};

/// Interleaved stereo samples in one Opus frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS as usize;

/// Upper bound for one encoded frame; 128 kbps at 10 ms is about 160 bytes.
pub const MAX_PACKET: usize = 1500;

/// The fixed MVP settings: 48 kHz stereo, 128 kbps, restricted low-delay mode.
pub fn new_encoder() -> Result<opus::Encoder> {
    let mut encoder = opus::Encoder::new(
        SAMPLE_RATE_HZ,
        opus::Channels::Stereo,
        opus::Application::LowDelay,
    )?;
    encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
    Ok(encoder)
}

pub fn new_decoder() -> Result<opus::Decoder> {
    Ok(opus::Decoder::new(SAMPLE_RATE_HZ, opus::Channels::Stereo)?)
}

pub fn run(wav: &Path, out: Option<&Path>) -> Result<()> {
    let input = read_wav_48k_stereo(wav)?;
    let frames = input.len() / FRAME_LEN;
    if frames == 0 {
        bail!("{} is shorter than one {FRAME_MS} ms frame", wav.display());
    }

    let mut encoder = new_encoder()?;
    let mut decoder = new_decoder()?;
    let lookahead = encoder.get_lookahead()? as usize;
    let mut packet = [0u8; MAX_PACKET];
    let mut pcm = vec![0.0f32; FRAME_LEN];
    let mut decoded = Vec::with_capacity(frames * FRAME_LEN);
    let (mut encode_time, mut decode_time) = (Duration::ZERO, Duration::ZERO);
    let (mut bytes, mut max_bytes) = (0usize, 0usize);

    for frame in input.as_chunks::<FRAME_LEN>().0 {
        let t = Instant::now();
        let n = encoder.encode_float(frame, &mut packet)?;
        encode_time += t.elapsed();
        bytes += n;
        max_bytes = max_bytes.max(n);

        let t = Instant::now();
        let got = decoder.decode_float(&packet[..n], &mut pcm, false)?;
        decode_time += t.elapsed();
        decoded.extend_from_slice(&pcm[..got * 2]);
    }

    // The decoded audio lags the input by the encoder's lookahead.
    let delay = lookahead * 2;
    let (mut signal, mut noise) = (0.0f64, 0.0f64);
    for (i, &d) in decoded.iter().enumerate().skip(delay) {
        let x = f64::from(input[i - delay]);
        signal += x * x;
        noise += (f64::from(d) - x).powi(2);
    }
    let snr = 10.0 * (signal / noise.max(1e-20)).log10();

    let audio_s = frames as f64 * f64::from(FRAME_MS) / 1000.0;
    let per_frame_us = |t: Duration| t.as_secs_f64() * 1e6 / frames as f64;
    let percent_of_realtime = |t: Duration| t.as_secs_f64() / audio_s * 100.0;
    println!("frames               {frames} × {FRAME_MS} ms ({audio_s:.1} s)");
    println!(
        "packet size          {:.0} bytes on average, {max_bytes} max",
        bytes as f64 / frames as f64
    );
    println!(
        "bitrate              {:.0} kbps",
        bytes as f64 * 8.0 / audio_s / 1000.0
    );
    println!(
        "encode               {:.0} µs per frame ({:.2}% of one core in real time)",
        per_frame_us(encode_time),
        percent_of_realtime(encode_time)
    );
    println!(
        "decode               {:.0} µs per frame ({:.2}% of one core in real time)",
        per_frame_us(decode_time),
        percent_of_realtime(decode_time)
    );
    println!(
        "encoder lookahead    {lookahead} samples ({:.1} ms)",
        lookahead as f64 * 1000.0 / f64::from(SAMPLE_RATE_HZ)
    );
    println!(
        "SNR                  {snr:.1} dB (a sanity check only; Opus is perceptual, so listen)"
    );

    if let Some(out) = out {
        let mut writer = wav_writer(out, SAMPLE_RATE_HZ)?;
        for &s in &decoded {
            writer.write_sample(s)?;
        }
        writer.finalize()?;
        println!("decoded audio        {}", out.display());
    }
    Ok(())
}
