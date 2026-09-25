//! The fixed MVP Opus settings: 48 kHz stereo, 128 kbps, 10 ms frames, restricted low delay.

use crate::SAMPLE_RATE_HZ;

pub const BITRATE: i32 = 128_000;

/// Samples the decoded audio lags the encoder's input by (2.5 ms; measured in M1).
pub const LOOKAHEAD: u32 = 120;

pub fn encoder() -> Result<opus::Encoder, opus::Error> {
    // `LowDelay` is libopus's OPUS_APPLICATION_RESTRICTED_LOWDELAY.
    let mut encoder = opus::Encoder::new(
        SAMPLE_RATE_HZ,
        opus::Channels::Stereo,
        opus::Application::LowDelay,
    )?;
    encoder.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
    Ok(encoder)
}

pub fn decoder() -> Result<opus::Decoder, opus::Error> {
    opus::Decoder::new(SAMPLE_RATE_HZ, opus::Channels::Stereo)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lookahead_matches_the_encoder() {
        let mut encoder = super::encoder().unwrap();
        assert_eq!(encoder.get_lookahead().unwrap(), super::LOOKAHEAD as i32);
    }
}
