//! `play`: play a sine or a WAV file on the default output device.

use std::sync::atomic::Ordering::Relaxed;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;
use ringbuf::HeapRb;
use ringbuf::traits::{Observer, Producer, Split};

use crate::audio_io::{SAMPLES_PER_MS, Source, start_playback};

pub fn run(mut source: Source, seconds: u64, volume: f32) -> Result<()> {
    let (mut prod, cons) = HeapRb::<f32>::new(200 * SAMPLES_PER_MS).split();
    let playback = start_playback(cons, 40, volume)?;
    println!(
        "playing {} on '{}' ({} channels) for {seconds} s at volume {volume}",
        source.describe(),
        playback.device,
        playback.channels
    );

    // Keep about 100 ms queued; the device pulls from the other end.
    let mut chunk = vec![0.0f32; 10 * SAMPLES_PER_MS];
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        while prod.occupied_len() < 100 * SAMPLES_PER_MS {
            source.fill(&mut chunk);
            prod.push_slice(&chunk);
        }
        sleep(Duration::from_millis(5));
    }

    let stats = &playback.stats;
    println!(
        "underruns {}, stream errors {}",
        stats.underruns.load(Relaxed),
        stats.errors.load(Relaxed)
    );
    Ok(())
}
