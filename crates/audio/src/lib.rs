//! Streams the Windows PC's system audio to the MacBook's built-in speakers.
//!
//! This crate doesn't depend on the network: it produces and consumes packets,
//! and the agent moves them over the side channel.

/// Sample rate of the Opus stream. Device audio is resampled to this at the edges.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Number of audio channels (stereo).
pub const CHANNELS: u16 = 2;

/// Length of one Opus frame in milliseconds.
pub const FRAME_MS: u32 = 10;

/// Samples per channel in one frame.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ * FRAME_MS / 1000) as usize;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_a_valid_opus_frame_size() {
        // Opus accepts 2.5, 5, 10, 20, 40 and 60 ms frames; at 48 kHz that's these sizes.
        const OPUS_FRAME_SAMPLES_48K: [usize; 6] = [120, 240, 480, 960, 1920, 2880];
        assert_eq!(FRAME_SAMPLES, 480);
        assert!(OPUS_FRAME_SAMPLES_48K.contains(&FRAME_SAMPLES));
    }
}
