//! Crossglide agent. Runs on both machines; owns the side channel and audio.

use crossglide_audio::{CHANNELS, FRAME_MS, SAMPLE_RATE_HZ};

fn main() {
    println!("crossglide-agent {}", env!("CARGO_PKG_VERSION"));
    println!("audio format: {SAMPLE_RATE_HZ} Hz, {CHANNELS} channels, {FRAME_MS} ms frames");
}
