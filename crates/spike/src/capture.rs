//! `capture`: record the default output device via loopback into a WAV file.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};

use crate::audio_io::{start_loopback, wav_writer};

pub fn run(seconds: u64, out: &Path) -> Result<()> {
    // Two seconds of headroom at up to 192 kHz stereo, so the writer can fall behind briefly.
    let (mut prod, mut cons) = HeapRb::<f32>::new(192_000 * 2 * 2).split();
    let dropped = Arc::new(AtomicU64::new(0));
    let cb_dropped = dropped.clone();
    let loopback = start_loopback(move |stereo| {
        let pushed = prod.push_slice(stereo);
        cb_dropped.fetch_add((stereo.len() - pushed) as u64, Relaxed);
    })?;
    let sample_rate = loopback.sample_rate;
    println!(
        "capturing '{}' ({sample_rate} Hz, {} channels) for {seconds} s into {}",
        loopback.device,
        loopback.channels,
        out.display()
    );
    println!("play something (a video or music) on this machine now");

    let mut writer = wav_writer(out, sample_rate)?;
    let mut buf = vec![0.0f32; 8192];
    let mut written = 0u64;
    let mut peak = 0.0f32;
    let mut drain = |cons: &mut HeapCons<f32>| -> Result<usize> {
        let n = cons.pop_slice(&mut buf);
        for &s in &buf[..n] {
            writer.write_sample(s)?;
            peak = peak.max(s.abs());
        }
        written += n as u64;
        Ok(n)
    };

    let start = Instant::now();
    let mut next_report = Duration::from_secs(10);
    while start.elapsed() < Duration::from_secs(seconds) {
        if drain(&mut cons)? == 0 {
            sleep(Duration::from_millis(5));
        }
        if start.elapsed() >= next_report {
            println!("  {} s", next_report.as_secs());
            next_report += Duration::from_secs(10);
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let stats = loopback.stats.clone();
    drop(loopback);
    while drain(&mut cons)? > 0 {}
    writer.finalize()?;

    let callbacks = stats.callbacks.load(Relaxed);
    let audio = written as f64 / 2.0 / f64::from(sample_rate);
    println!();
    println!("wall time            {wall:.1} s");
    println!(
        "audio captured       {audio:.1} s ({:.0}% of wall time)",
        audio / wall * 100.0
    );
    println!("callbacks            {callbacks}");
    if callbacks > 0 {
        println!(
            "frames per callback  {:.0} on average",
            stats.frames.load(Relaxed) as f64 / callbacks as f64
        );
    }
    println!(
        "gaps over 50 ms      {} (longest {:.0} ms)",
        stats.gaps.load(Relaxed),
        stats.max_gap_us.load(Relaxed) as f64 / 1000.0
    );
    println!("stream errors        {}", stats.errors.load(Relaxed));
    println!("samples dropped      {}", dropped.load(Relaxed));
    println!(
        "peak level           {:.1} dBFS",
        20.0 * f64::from(peak.max(1e-9)).log10()
    );
    println!();
    println!("Audio time well under wall time, or gaps while audio was playing, point to loopback");
    println!(
        "problems. Gaps only during silence are expected: WASAPI loopback may deliver nothing"
    );
    println!(
        "while nothing plays. Listen to {} to check quality.",
        out.display()
    );
    Ok(())
}
