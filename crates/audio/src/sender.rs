//! The PC's half of the stream, without devices: 48 kHz stereo in, packets out.

use std::time::Duration;

use crate::packet::{HEADER_LEN, Header, MAX_PAYLOAD};
use crate::{FRAME_LEN, FRAME_SAMPLES, codec};

/// Cuts 48 kHz stereo into 10 ms frames, encodes each and wraps it in a packet.
pub struct Packetizer {
    encoder: opus::Encoder,
    seq: u16,
    ts: u32,
    pending: Vec<f32>,
    packet: Vec<u8>,
}

impl Packetizer {
    pub fn new() -> Result<Self, opus::Error> {
        Ok(Self {
            encoder: codec::encoder()?,
            seq: 0,
            ts: 0,
            pending: Vec::with_capacity(FRAME_LEN),
            packet: vec![0; HEADER_LEN + MAX_PAYLOAD],
        })
    }

    /// Timestamp the next frame will carry.
    pub fn next_ts(&self) -> u32 {
        self.ts
    }

    /// Interleaved samples buffered towards the next frame.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Adds samples and emits a packet for each whole frame. Returns how many were emitted.
    pub fn push(
        &mut self,
        mut samples: &[f32],
        emit: &mut dyn FnMut(&[u8]),
    ) -> Result<usize, opus::Error> {
        let mut frames = 0;
        while !samples.is_empty() {
            let take = (FRAME_LEN - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == FRAME_LEN {
                self.emit_pending(emit)?;
                frames += 1;
            }
        }
        Ok(frames)
    }

    /// Emits one frame of silence, after whatever part of a frame is still buffered.
    pub fn silence(&mut self, emit: &mut dyn FnMut(&[u8])) -> Result<(), opus::Error> {
        self.pending.resize(FRAME_LEN, 0.0);
        self.emit_pending(emit)
    }

    fn emit_pending(&mut self, emit: &mut dyn FnMut(&[u8])) -> Result<(), opus::Error> {
        let len = self
            .encoder
            .encode_float(&self.pending, &mut self.packet[HEADER_LEN..])?;
        Header {
            seq: self.seq,
            ts: self.ts,
        }
        .write(&mut self.packet);
        emit(&self.packet[..HEADER_LEN + len]);
        self.pending.clear();
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(FRAME_SAMPLES as u32);
        Ok(())
    }
}

/// WASAPI loopback delivers nothing while nothing plays. The Mac would read that as a stalled
/// stream, so while the capture is quiet, silence frames go out on the clock instead.
pub struct SilenceFill {
    /// When the last frame went out (or would have, for silence), on the caller's clock.
    last: Option<Duration>,
}

/// How long past a frame's due time capture may be quiet before silence fills in. Capture
/// callbacks came up to 14 ms apart in M1, so this doesn't fire while audio plays.
const GRACE: Duration = Duration::from_millis(15);
const FRAME: Duration = Duration::from_millis(crate::FRAME_MS as u64);
/// Behind by more than this (the thread was stalled), it skips ahead instead of catching up.
const MAX_BEHIND: Duration = Duration::from_millis(100);

impl SilenceFill {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Call after each round of sending: `sent` frames of captured audio went out at `now`.
    /// Returns how many silence frames to send now.
    pub fn after(&mut self, now: Duration, sent: usize) -> usize {
        let last = self.last.get_or_insert(now);
        if sent > 0 {
            *last = now;
            return 0;
        }
        if now.saturating_sub(*last) > MAX_BEHIND {
            *last = now - FRAME - GRACE;
        }
        let mut silent = 0;
        while now.saturating_sub(*last) >= FRAME + GRACE {
            *last += FRAME;
            silent += 1;
        }
        silent
    }
}

impl Default for SilenceFill {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Header;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn packets_carry_consecutive_sequence_numbers_and_timestamps() {
        let mut p = Packetizer::new().unwrap();
        let mut headers = Vec::new();
        let mut emit = |packet: &[u8]| headers.push(Header::parse(packet).unwrap().0);
        // 2.5 frames, then silence completes the third.
        let audio = vec![0.25f32; FRAME_LEN * 5 / 2];
        assert_eq!(p.push(&audio, &mut emit).unwrap(), 2);
        assert_eq!(p.pending(), FRAME_LEN / 2);
        p.silence(&mut emit).unwrap();
        assert_eq!(p.pending(), 0);
        let expected: Vec<Header> = (0..3)
            .map(|i| Header {
                seq: i,
                ts: u32::from(i) * FRAME_SAMPLES as u32,
            })
            .collect();
        assert_eq!(headers, expected);
    }

    #[test]
    fn silence_fills_in_only_when_capture_stops() {
        let mut fill = SilenceFill::new();
        // Capture delivers a frame every 10-14 ms: no silence.
        for t in [0, 10, 24, 34, 45] {
            assert_eq!(fill.after(ms(t), 1), 0);
        }
        assert_eq!(fill.after(ms(60), 0), 0);
        // Then nothing: a silence frame once 25 ms have passed, then every 10 ms.
        assert_eq!(fill.after(ms(70), 0), 1);
        assert_eq!(fill.after(ms(75), 0), 0);
        assert_eq!(fill.after(ms(80), 0), 1);
        assert_eq!(fill.after(ms(101), 0), 2);
        // Audio again.
        assert_eq!(fill.after(ms(103), 1), 0);
        assert_eq!(fill.after(ms(110), 0), 0);
    }

    #[test]
    fn silence_skips_ahead_after_a_stall() {
        let mut fill = SilenceFill::new();
        fill.after(ms(0), 1);
        // The thread didn't run for a second: one frame, not a hundred.
        assert_eq!(fill.after(ms(1000), 0), 1);
        assert_eq!(fill.after(ms(1010), 0), 1);
    }
}
