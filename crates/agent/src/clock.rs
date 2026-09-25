//! Clock offset and round-trip time to the peer, measured NTP-style on the control stream.
//! Latency stats (M3) use the offset.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{Instant, sleep, timeout};
use tracing::{debug, info};

use crate::control::Control;
use crate::proto::{Request, Response};

/// Nanoseconds on a `SessionClock`.
pub type Nanos = i64;

/// The clock both sides measure with during one connection: this machine's wall time when the
/// connection started, advanced by the monotonic clock. Time services slew and set the wall
/// clock (in the M2 LAN test the Mac's ran 24 ppm and the PC's 145 ppm off their monotonic
/// clocks, and the rates changed), but not this one, so the offset between the two machines'
/// session clocks moves only as their oscillators drift: steadily enough to fit a line to.
#[derive(Clone, Copy, Debug)]
pub struct SessionClock {
    wall: Nanos,
    start: std::time::Instant,
}

impl SessionClock {
    pub fn start() -> Self {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as Nanos);
        Self {
            wall,
            start: std::time::Instant::now(),
        }
    }

    pub fn now(&self) -> Nanos {
        self.wall + self.start.elapsed().as_nanos() as Nanos
    }
}

/// Probes at connect, in quick succession, so there's an estimate within a second.
const BURST: u32 = 8;
const BURST_GAP: Duration = Duration::from_millis(100);
/// Then faster probes for a while, so the drift is known sooner.
const SETTLE_GAP: Duration = Duration::from_millis(500);
const SETTLE_FOR: Duration = Duration::from_secs(30);
/// Then one probe every few seconds.
const PROBE_EVERY: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Until the drift is known, the estimate can be about a millisecond off, so the logged
/// summaries start after this.
const WARM_UP: Duration = Duration::from_secs(60);
/// How often the offset, drift and jitter are logged.
const REPORT_EVERY: Duration = Duration::from_secs(60);

/// Samples the estimate is fitted from: five minutes' worth, so a minute of slow Wi-Fi (the M2
/// LAN test had one at about 90 ms RTT) still leaves plenty of fast samples.
const WINDOW: usize = 150;
/// Until the fast samples span this long, right after connecting, the estimate is simply the
/// fastest sample: the drift can't be measured yet.
const MIN_SPAN: Nanos = 5_000_000_000;
/// A sample further than this from the estimate, beyond what its RTT explains, means a session
/// clock jumped: the Mac slept for a few seconds without the connection timing out, say. The
/// older samples are then dropped.
const STEP: Nanos = 20_000_000;

/// The peer's clock relative to ours, from one exchange or a filtered estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Peer's clock minus ours.
    pub offset: Nanos,
    /// Network round trip, excluding the time the peer took to answer.
    pub rtt: Nanos,
}

impl Sample {
    /// We sent at `t1` and got the answer at `t4`; the peer got the request at `t2` and answered
    /// at `t3`, by its clock. Assumes the two directions take equally long, so the offset is off
    /// by at most `rtt / 2`.
    pub fn from_exchange(t1: Nanos, t2: Nanos, t3: Nanos, t4: Nanos) -> Self {
        Self {
            offset: ((t2 - t1) + (t3 - t4)) / 2,
            rtt: ((t4 - t1) - (t3 - t2)).max(0),
        }
    }
}

/// Estimates the offset from recent samples. The clocks drift apart (by about 80 ppm between the
/// PC's and the Mac's monotonic clocks), so the offset is a line, not a constant: a least-squares
/// line through the fast samples, evaluated now. Queueing delay (Wi-Fi retries, a busy peer) only
/// ever adds to a sample's RTT and makes its offset less certain by up to half the extra, so only
/// samples close to the lowest RTT are used.
#[derive(Default)]
pub struct Filter {
    /// (when the answer arrived, by our clock; the sample)
    recent: VecDeque<(Nanos, Sample)>,
}

impl Filter {
    /// Adds a sample whose answer arrived at `at`; returns the estimate for `at`.
    pub fn add(&mut self, at: Nanos, sample: Sample) -> Sample {
        if let Some(estimate) = self.estimate(at)
            && (sample.offset - estimate.offset).abs() > STEP + sample.rtt
        {
            self.recent.clear();
        }
        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back((at, sample));
        self.estimate(at).expect("just added a sample")
    }

    fn estimate(&self, now: Nanos) -> Option<Sample> {
        let mut fast: Vec<(Nanos, Sample)> = self.recent.iter().copied().collect();
        // Fastest first; the newest first among equally fast ones.
        fast.sort_by_key(|&(at, s)| (s.rtt, std::cmp::Reverse(at)));
        let (_, fastest) = *fast.first()?;
        // Samples within 1 ms or half the lowest RTT of it, whichever is more; but at least the
        // fastest quarter, so one unusually fast sample doesn't leave too few to fit a line to.
        let limit = fastest.rtt + (fastest.rtt / 2).max(1_000_000);
        let close = fast.iter().take_while(|(_, s)| s.rtt <= limit).count();
        fast.truncate(close.max(fast.len() / 4));
        let first = fast.iter().map(|&(at, _)| at).min()?;
        let last = fast.iter().map(|&(at, _)| at).max()?;
        if fast.len() < 3 || last - first < MIN_SPAN {
            return Some(fastest);
        }
        let points: Vec<(f64, f64)> = fast
            .iter()
            .map(|&(at, s)| ((at - now) as f64 / 1e9, s.offset as f64 / 1e6))
            .collect();
        let line = Line::fit(&points)?;
        Some(Sample {
            offset: (line.at(0.0) * 1e6).round() as Nanos,
            rtt: fastest.rtt,
        })
    }
}

/// Least-squares line through (x, y) points.
struct Line {
    mean_x: f64,
    mean_y: f64,
    slope: f64,
}

impl Line {
    fn fit(points: &[(f64, f64)]) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let n = points.len() as f64;
        let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
        let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
        let sxx: f64 = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum();
        let sxy: f64 = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum();
        let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        Some(Self {
            mean_x,
            mean_y,
            slope,
        })
    }

    fn at(&self, x: f64) -> f64 {
        self.mean_y + self.slope * (x - self.mean_x)
    }
}

/// Estimates over one report period, fitted to a straight line. The slope is how fast the two
/// clocks drift apart; the largest distance of an estimate from the line is the jitter.
#[derive(Default)]
pub struct Trend {
    /// (seconds since the first point, offset in ms)
    points: Vec<(f64, f64)>,
    start: Option<Nanos>,
    rtts: Vec<Nanos>,
}

#[derive(Debug, PartialEq)]
pub struct Fit {
    pub drift_ppm: f64,
    pub jitter_ms: f64,
}

impl Trend {
    pub fn add(&mut self, at: Nanos, estimate: Sample, rtt: Nanos) {
        let start = *self.start.get_or_insert(at);
        self.points
            .push(((at - start) as f64 / 1e9, estimate.offset as f64 / 1e6));
        self.rtts.push(rtt);
    }

    /// Needs at least three points.
    pub fn fit(&self) -> Option<Fit> {
        if self.points.len() < 3 {
            return None;
        }
        let line = Line::fit(&self.points)?;
        let jitter_ms = self
            .points
            .iter()
            .map(|&(x, y)| (y - line.at(x)).abs())
            .fold(0.0, f64::max);
        // The slope is in ms per s, i.e. thousandths; one ppm is a millionth.
        Some(Fit {
            drift_ppm: line.slope * 1e3,
            jitter_ms,
        })
    }

    /// Lowest, median and highest RTT in the period.
    pub fn rtt_range(&self) -> Option<(Nanos, Nanos, Nanos)> {
        let mut rtts = self.rtts.clone();
        rtts.sort_unstable();
        Some((*rtts.first()?, rtts[rtts.len() / 2], *rtts.last()?))
    }
}

/// Probes the peer's clock until the control stream closes: a quick burst for a first estimate,
/// faster probes while the drift settles, then every `PROBE_EVERY`. Hands each new estimate to
/// `update`, and after the warm-up logs a summary every `REPORT_EVERY`.
pub async fn probe(
    control: &Control,
    clock: &SessionClock,
    peer: &str,
    mut update: impl FnMut(Sample),
) {
    let mut filter = Filter::default();
    let mut trend = Trend::default();
    let started = Instant::now();
    let mut next_report = started + WARM_UP;
    let mut warmed_up = false;
    for n in 1u32.. {
        let gap = if n <= BURST {
            BURST_GAP
        } else if started.elapsed() < SETTLE_FOR {
            SETTLE_GAP
        } else {
            PROBE_EVERY
        };
        sleep(gap).await;
        let request = Request::Time { t1: clock.now() };
        let reply = match timeout(PROBE_TIMEOUT, control.request(request)).await {
            Ok(Ok(reply)) => reply,
            // The control stream closed; the session is ending.
            Ok(Err(_)) => return,
            Err(_) => {
                debug!("clock probe got no answer within {PROBE_TIMEOUT:?}");
                continue;
            }
        };
        let Response::Time { t1, t2, t3 } = reply.body else {
            debug!("unexpected answer to a clock probe: {:?}", reply.body);
            continue;
        };
        let sample = Sample::from_exchange(t1, t2, t3, reply.arrived);
        let estimate = filter.add(reply.arrived, sample);
        update(estimate);
        trend.add(reply.arrived, estimate, sample.rtt);

        if n == BURST {
            info!(
                "clock: {peer} is {:+.3} ms from this machine, rtt {:.1} ms",
                ms(estimate.offset),
                ms(estimate.rtt)
            );
        }
        if Instant::now() >= next_report {
            // The first time round, the warm-up is over and the first period starts.
            if warmed_up
                && let (Some(fit), Some((low, mid, high))) = (trend.fit(), trend.rtt_range())
            {
                info!(
                    "clock: offset {:+.3} ms, drift {:+.1} ppm, jitter {:.2} ms, \
                     rtt {:.1} ms ({:.1}-{:.1}) over the last {} s",
                    ms(estimate.offset),
                    fit.drift_ppm,
                    fit.jitter_ms,
                    ms(mid),
                    ms(low),
                    ms(high),
                    REPORT_EVERY.as_secs()
                );
            }
            warmed_up = true;
            next_report = Instant::now() + REPORT_EVERY;
            trend = Trend::default();
        }
    }
}

fn ms(nanos: Nanos) -> f64 {
    nanos as f64 / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Nanos = 1_000_000;

    #[test]
    fn symmetric_exchange_gives_the_exact_offset() {
        // Peer is 1000 ms ahead; 5 ms each way; it takes 10 ms to answer.
        let t1 = 50 * MS;
        let t2 = t1 + 5 * MS + 1000 * MS;
        let t3 = t2 + 10 * MS;
        let t4 = t1 + 20 * MS;
        let sample = Sample::from_exchange(t1, t2, t3, t4);
        assert_eq!(sample.offset, 1000 * MS);
        assert_eq!(sample.rtt, 10 * MS);
    }

    #[test]
    fn asymmetric_delay_is_off_by_half_the_difference() {
        // No real offset; 9 ms out, 1 ms back.
        let sample = Sample::from_exchange(0, 9 * MS, 9 * MS, 10 * MS);
        assert_eq!(sample.offset, 4 * MS);
        assert_eq!(sample.rtt, 10 * MS);
    }

    #[test]
    fn filter_uses_the_fastest_sample_until_the_samples_span_a_few_seconds() {
        let mut filter = Filter::default();
        // A connect burst, 100 ms apart: too close together to measure drift. The slower a
        // sample, the further off its offset.
        let mut estimate = None;
        for (i, rtt) in [9, 4, 7, 12].into_iter().enumerate() {
            let sample = Sample {
                offset: (100 + rtt) * MS,
                rtt: rtt * MS,
            };
            estimate = Some(filter.add(i as Nanos * 100 * MS, sample));
        }
        let fastest = Sample {
            offset: 104 * MS,
            rtt: 4 * MS,
        };
        assert_eq!(estimate, Some(fastest));
    }

    /// A clock drifting at -220 ppm, more than the Mac and the PC measured, probed every
    /// 2 s over Wi-Fi: RTTs vary, queueing delays one direction at a time (so raw samples are up
    /// to 2.5 ms off), one answer takes 300 ms, and for a minute every answer takes 90 ms.
    #[test]
    fn filter_tracks_a_drifting_clock() {
        let truth = |at: Nanos| 2_000 * MS - at * 22 / 100_000;
        let mut filter = Filter::default();
        let mut worst = [0; 3];
        for i in 0..200 {
            let at = i * 2_000 * MS;
            // A minute of slow Wi-Fi from 200 s, with the delay all one way.
            let slow = (100..130).contains(&i);
            let rtt = if i == 40 {
                300 * MS
            } else if slow {
                90 * MS
            } else {
                [4, 7, 5, 9, 6][i as usize % 5] * MS
            };
            let direction = if i % 2 == 0 || slow { 1 } else { -1 };
            let sample = Sample {
                offset: truth(at) + direction * (rtt - 4 * MS) / 2,
                rtt,
            };
            let error = (filter.add(at, sample).offset - truth(at)).abs();
            let phase = match at / 1_000_000_000 {
                0..10 => 0,
                10..60 => 1,
                _ => 2,
            };
            worst[phase] = worst[phase].max(error);
        }
        // Few samples at first; after a minute the slow minute doesn't show.
        let bounds = [2 * MS, 3 * MS / 4, 3 * MS / 10];
        for (phase, (error, bound)) in worst.iter().zip(bounds).enumerate() {
            assert!(error < &bound, "phase {phase}: {error} ns off");
        }
    }

    #[test]
    fn one_unusually_fast_sample_does_not_freeze_the_estimate() {
        let truth = |at: Nanos| at / 12_500; // +80 ppm
        let mut filter = Filter::default();
        for i in 0..150 {
            let at = i * 2_000 * MS;
            // 5 ms round trips with ±0.5 ms of asymmetry, and one of 1 ms early on.
            let (rtt, error) = if i == 3 {
                (MS, 0)
            } else {
                (5 * MS, if i % 2 == 0 { MS / 2 } else { -MS / 2 })
            };
            let estimate = filter.add(
                at,
                Sample {
                    offset: truth(at) + error,
                    rtt,
                },
            );
            if at >= 60_000 * MS {
                let off = (estimate.offset - truth(at)).abs();
                assert!(off < MS / 2, "{off} ns off at {} s", at / 1_000_000_000);
            }
        }
    }

    #[test]
    fn filter_starts_over_when_a_clock_is_set() {
        let mut filter = Filter::default();
        let rtt = 4 * MS;
        for i in 0..20 {
            filter.add(i * 2_000 * MS, Sample { offset: 0, rtt });
        }
        // The Mac sleeps for 2 s and the connection survives: its session clock stood still.
        let stepped = Sample {
            offset: 2_000 * MS,
            rtt,
        };
        assert_eq!(filter.add(40_000 * MS, stepped), stepped);
        assert_eq!(filter.add(42_000 * MS, stepped).offset, 2_000 * MS);
    }

    #[test]
    fn a_slow_answer_is_not_a_clock_step() {
        let mut filter = Filter::default();
        let rtt = 4 * MS;
        for i in 0..20 {
            filter.add(i * 2_000 * MS, Sample { offset: 0, rtt });
        }
        // 300 ms stuck in one direction: 150 ms off, but its RTT says it may be.
        let slow = Sample {
            offset: 148 * MS,
            rtt: 300 * MS,
        };
        assert_eq!(filter.add(40_000 * MS, slow).offset, 0);
    }

    #[test]
    fn trend_finds_drift_and_jitter() {
        let mut trend = Trend::default();
        // 5 ms offset drifting at +20 ppm, with ±0.1 ms of alternating noise, every 2 s.
        for i in 0..30 {
            let t = i * 2_000 * MS;
            let noise = if i % 2 == 0 { MS / 10 } else { -MS / 10 };
            let offset = 5 * MS + t / 50_000 + noise;
            trend.add(t, Sample { offset, rtt: MS }, MS);
        }
        let fit = trend.fit().unwrap();
        assert!((fit.drift_ppm - 20.0).abs() < 0.5, "{fit:?}");
        assert!((fit.jitter_ms - 0.1).abs() < 0.02, "{fit:?}");
        assert_eq!(trend.rtt_range(), Some((MS, MS, MS)));
    }

    #[test]
    fn trend_needs_three_points() {
        let mut trend = Trend::default();
        let sample = Sample { offset: 0, rtt: 0 };
        trend.add(0, sample, 0);
        trend.add(MS, sample, 0);
        assert_eq!(trend.fit(), None);
    }

    #[test]
    fn session_clock_starts_at_the_wall_clock_and_runs_on() {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as Nanos;
        let clock = SessionClock::start();
        let a = clock.now();
        std::thread::sleep(Duration::from_millis(20));
        let b = clock.now();
        assert!((a - wall).abs() < 100 * MS, "starts near the wall clock");
        assert!(b - a >= 20 * MS, "advances with real time");
    }
}
