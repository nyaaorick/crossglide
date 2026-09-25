//! Stereo resampling with rubato's asynchronous sinc resampler: from the capture device's rate
//! to 48 kHz on the PC, and from 48 kHz to the output device's rate on the Mac, where the ratio
//! is also nudged to follow the jitter buffer.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Takes a fixed number of input frames per call and returns what it made from them. Doesn't
/// allocate after construction, so it can run on the audio thread.
pub struct Stereo {
    inner: Async<f32>,
    out: Vec<f32>,
    chunk: usize,
}

impl Stereo {
    /// `chunk` input frames per call, turned from `from` Hz into `to` Hz. The ratio can later be
    /// adjusted by up to `max_adjust` (0.01 is 1%) either way.
    pub fn new(from: u32, to: u32, chunk: usize, max_adjust: f64) -> Self {
        let mut params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
        params.interpolation = SincInterpolationType::Cubic;
        let inner = Async::new_sinc(
            f64::from(to) / f64::from(from),
            1.0 + max_adjust,
            &params,
            chunk,
            2,
            FixedAsync::Input,
        )
        .expect("valid resampler settings");
        let out = vec![0.0; inner.output_frames_max() * 2];
        Self { inner, out, chunk }
    }

    pub fn chunk(&self) -> usize {
        self.chunk
    }

    /// Most interleaved samples one call can return.
    pub fn max_output_len(&self) -> usize {
        self.out.len()
    }

    /// Delay the resampler adds, in output frames.
    pub fn delay(&self) -> usize {
        self.inner.output_delay()
    }

    /// Plays faster (`factor` < 1) or slower (`factor` > 1) than the nominal ratio.
    pub fn adjust(&mut self, factor: f64) {
        // Only fails outside the range given to `new`, which callers clamp to.
        let _ = self.inner.set_resample_ratio_relative(factor, true);
    }

    /// Resamples exactly `chunk` interleaved stereo frames.
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        debug_assert_eq!(input.len(), self.chunk * 2);
        let frames_out = self.out.len() / 2;
        let input = InterleavedSlice::new(input, 2, self.chunk).expect("input is one chunk");
        let mut output =
            InterleavedSlice::new_mut(&mut self.out, 2, frames_out).expect("sized in new");
        let (_, written) = self
            .inner
            .process_into_buffer(&input, &mut output, None)
            .expect("buffers are sized for the resampler");
        &self.out[..written * 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    #[test]
    fn converts_44k1_to_48k() {
        let mut rs = Stereo::new(44_100, 48_000, 441, 0.0);
        let input = sine(1000.0, 44_100, 44_100);
        let mut out = Vec::new();
        for chunk in input.as_chunks::<{ 441 * 2 }>().0 {
            out.extend_from_slice(rs.process(chunk));
        }
        // One second in, one second out (give or take a few frames).
        assert!((out.len() / 2).abs_diff(48_000) < 10, "{}", out.len() / 2);
        // Still a 0.5-amplitude sine once the filter has settled.
        let peak = out[2000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }

    #[test]
    fn adjusting_changes_the_output_length() {
        let mut rs = Stereo::new(48_000, 48_000, 480, 0.01);
        let input = sine(440.0, 48_000, 480);
        let nominal: usize = (0..100).map(|_| rs.process(&input).len() / 2).sum();
        rs.adjust(1.002);
        let slower: usize = (0..100).map(|_| rs.process(&input).len() / 2).sum();
        assert!(nominal.abs_diff(48_000) < 5, "{nominal}");
        // 2000 ppm more output: about 96 extra frames per second.
        assert!(slower.abs_diff(48_096) < 10, "{slower}");
    }
}
