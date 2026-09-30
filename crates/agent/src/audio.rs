//! Audio over the side channel (M3): the PC captures its system audio and sends it as QUIC
//! datagrams, and the Mac plays it. The Mac asks for the stream once it can play, so the PC only
//! captures while the Mac is listening.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use crossglide_audio::codec;
use crossglide_audio::device::{OutputStats, Playing, Sending, Source};
use crossglide_audio::player::Snapshot;
use quinn::Connection;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::clock::{Nanos, Sample, SessionClock};
use crate::control::{Control, Handled};
use crate::proto::{Request, Response};

/// Listed in the hello by agents that can stream audio.
pub const FEATURE: &str = "audio";

/// How often the Mac logs latency and buffer statistics, in seconds.
const REPORT_EVERY: u32 = 10;
/// How often the PC sends a latency mark, in seconds.
const MARK_EVERY: u32 = 2;

/// What the audio stream is doing, for the tray.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum AudioStatus {
    /// Off on this machine, or the other one can't do audio.
    #[default]
    Off,
    /// Waiting for the other side: the Mac to ask for audio, or the PC to answer.
    Waiting,
    Sending {
        device: String,
    },
    Playing {
        device: String,
        /// Median of the last report period, once measured.
        latency_ms: Option<f64>,
    },
    Failed(String),
}

/// One connection's audio, on either side.
pub struct Session {
    pub conn: Connection,
    pub control: Control,
    pub clock: SessionClock,
    /// The peer's clock offset and RTT, as measured on the control stream.
    pub peer_clock: watch::Receiver<Option<Sample>>,
    pub peer: String,
}

/// Plays what the PC sends, until the control stream closes.
pub async fn play(
    session: Session,
    mut requests: mpsc::Receiver<Handled>,
    report: impl Fn(AudioStatus),
) {
    let peer = &session.peer;
    report(AudioStatus::Waiting);
    let (playing, mut receiver) = match tokio::task::spawn_blocking(Playing::start).await {
        Ok(Ok(started)) => started,
        Ok(Err(e)) => {
            warn!("audio: {e:#}; no audio until the agent restarts");
            report(AudioStatus::Failed(format!("{e:#}")));
            return idle(requests, "the Mac can't play audio").await;
        }
        Err(e) => {
            warn!("audio: opening the output device failed: {e}");
            report(AudioStatus::Failed(format!(
                "opening the output device failed: {e}"
            )));
            return idle(requests, "the Mac can't play audio").await;
        }
    };
    info!("audio: playing on {}", playing.device);
    let playing_status = |latency_ms| AudioStatus::Playing {
        device: playing.device.clone(),
        latency_ms,
    };
    report(playing_status(None));
    // Why the PC isn't sending, if it said so; the periodic update doesn't hide it.
    let mut peer_failed = false;

    // Packets go straight from the network to the audio thread.
    let reader = AbortOnDrop(tokio::spawn({
        let conn = session.conn.clone();
        async move {
            let mut refused = false;
            while let Ok(datagram) = conn.read_datagram().await {
                if let Err(e) = receiver.push(&datagram)
                    && !refused
                {
                    warn!("audio: dropping packets from the PC: {e}");
                    refused = true;
                }
            }
        }
    }));

    let control = session.control.clone();
    let mut start = Some(tokio::spawn(async move {
        control.request(Request::AudioStart).await
    }));
    let mut mark: Option<(u32, Nanos)> = None;
    let mut latencies = Vec::new();
    let mut last = playing.stats.snapshot();
    let mut last_xruns = 0;
    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ticks = 0u32;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some((body, reply)) = request else { break };
                let answer = match body {
                    Request::AudioMark { ts, at } => {
                        mark = Some((ts, at));
                        Response::Ok
                    }
                    Request::AudioStop { reason } => {
                        warn!("audio: {peer} stopped sending: {reason}");
                        report(AudioStatus::Failed(format!("{peer} stopped sending: {reason}")));
                        peer_failed = true;
                        Response::Ok
                    }
                    _ => unsupported(),
                };
                let _ = reply.send(answer);
            }
            Some(started) = async { Some(start.as_mut()?.await) } => {
                start = None;
                // An error here means the connection is closing, which ends this anyway.
                if let Ok(Ok(reply)) = started {
                    match reply.body {
                        Response::Ok => info!("audio: {peer} is sending"),
                        Response::Error { message } => {
                            warn!("audio: {peer} can't send: {message}");
                            report(AudioStatus::Failed(format!("{peer} can't send: {message}")));
                            peer_failed = true;
                        }
                        other => warn!("audio: unexpected answer from {peer}: {other:?}"),
                    }
                }
            }
            _ = tick.tick() => {
                ticks += 1;
                if let Some(reason) = playing.failure() {
                    warn!(
                        "audio: playback on {} stopped: {reason}; restart the agent to play \
                         again",
                        playing.device
                    );
                    report(AudioStatus::Failed(format!("playback stopped: {reason}")));
                    let control = session.control.clone();
                    tokio::spawn(async move { control.request(Request::AudioStop { reason }).await });
                    drop(reader);
                    drop(playing);
                    return idle(requests, "the Mac's output device stopped").await;
                }
                let offset = session.peer_clock.borrow().map(|s| s.offset);
                if let Some(ms) = latency(&playing, &session.clock, mark, offset) {
                    latencies.push(ms);
                }
                if ticks.is_multiple_of(REPORT_EVERY) {
                    let now = playing.stats.snapshot();
                    let xruns = playing.output.xruns.load(Relaxed);
                    let median = median(&mut latencies);
                    if !peer_failed {
                        report(playing_status(median));
                    }
                    log_stats(&last, &now, &mut latencies, &playing.output, xruns - last_xruns);
                    (last, last_xruns) = (now, xruns);
                }
            }
        }
    }
}

/// Capture-to-speaker latency in ms, from the latest sample heard and the PC's latest mark.
fn latency(
    playing: &Playing,
    clock: &SessionClock,
    mark: Option<(u32, Nanos)>,
    offset: Option<Nanos>,
) -> Option<f64> {
    let (mark_ts, mark_at) = mark?;
    let offset = offset?;
    if !playing.stats.playing.load(Relaxed) {
        return None;
    }
    let anchor = playing.stats.anchor()?;
    let heard = clock.at(playing.epoch + Duration::from_nanos(anchor.heard_at));
    // ns per sample at 48 kHz is 62500 / 3.
    // What's heard lags the encoder's input by the codec's lookahead.
    let heard_ts = anchor.ts.wrapping_sub(codec::LOOKAHEAD);
    let since_mark = i64::from(heard_ts.wrapping_sub(mark_ts) as i32) * 62_500 / 3;
    // The mark is on the PC's clock; the offset is the PC's clock minus this one.
    let captured = mark_at + since_mark - offset;
    Some((heard - captured) as f64 / 1e6)
}

/// The median of `values`, sorting them.
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied()
}

fn log_stats(
    last: &Snapshot,
    now: &Snapshot,
    latencies: &mut Vec<f64>,
    output: &OutputStats,
    xruns: u64,
) {
    let latency = if latencies.is_empty() {
        "unknown".to_string()
    } else {
        latencies.sort_by(f64::total_cmp);
        let mid = latencies[latencies.len() / 2];
        let (low, high) = (latencies[0], latencies[latencies.len() - 1]);
        format!("{mid:.0} ms ({low:.0}-{high:.0})")
    };
    latencies.clear();
    let mut extra = String::new();
    for (name, n) in [
        ("skipped", now.skipped - last.skipped),
        ("overflow", now.overflow - last.overflow),
        ("decode errors", now.decode_errors - last.decode_errors),
        ("xruns", xruns),
    ] {
        if n > 0 {
            extra.push_str(&format!(", {name} {n}"));
        }
    }
    let line = format!(
        "audio: latency {latency}, delay {:.0} ms (target {:.0}), rate {:+.0} ppm (skew {:+.0}), \
         packets {}, lost {}, late {}, underruns {}, stalls {}, latest packet {:.0} ms late, \
         output {:.1} ms in {}-frame callbacks{extra}",
        now.delay_ms,
        now.target_ms,
        now.ppm,
        now.skew_ppm,
        now.received - last.received,
        now.lost - last.lost,
        now.late - last.late,
        now.underruns - last.underruns,
        now.stalls - last.stalls,
        now.max_lateness_ms,
        output.latency_us.load(Relaxed) as f64 / 1000.0,
        output.callback_frames.load(Relaxed),
    );
    if now.underruns > last.underruns || now.stalls > last.stalls {
        warn!("{line}");
    } else {
        info!("{line}");
    }
}

/// Sends `source` while the Mac asks for it, until the control stream closes.
pub async fn send(
    session: Session,
    source: Source,
    mut requests: mpsc::Receiver<Handled>,
    report: impl Fn(AudioStatus),
) {
    let peer = &session.peer;
    report(AudioStatus::Waiting);
    let mut sending: Option<Sending> = None;
    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ticks = 0u32;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some((body, reply)) = request else { break };
                let answer = match body {
                    Request::AudioStart => {
                        drop(sending.take());
                        let conn = session.conn.clone();
                        let emit = move |packet: &[u8]| {
                            // Only fails when the connection is gone, which ends the session.
                            let _ = conn.send_datagram(packet.to_vec().into());
                        };
                        match tokio::task::spawn_blocking(move || Sending::start(source, emit)).await {
                            Ok(Ok(started)) => {
                                info!("audio: sending {} to {peer}", started.device);
                                report(AudioStatus::Sending { device: started.device.clone() });
                                sending = Some(started);
                                Response::Ok
                            }
                            Ok(Err(e)) => {
                                warn!("audio: {e:#}");
                                report(AudioStatus::Failed(format!("{e:#}")));
                                Response::Error { message: format!("{e:#}") }
                            }
                            Err(e) => Response::Error { message: format!("capture failed: {e}") },
                        }
                    }
                    Request::AudioStop { reason } => {
                        if sending.take().is_some() {
                            info!("audio: {peer} stopped playing ({reason}); capture stopped");
                        }
                        report(AudioStatus::Failed(format!("{peer} stopped playing: {reason}")));
                        Response::Ok
                    }
                    _ => unsupported(),
                };
                let _ = reply.send(answer);
            }
            _ = tick.tick() => {
                ticks += 1;
                let Some(capture) = &sending else { continue };
                if let Some(reason) = capture.failure() {
                    warn!(
                        "audio: capture from {} stopped: {reason}; restart the agent to send \
                         again",
                        capture.device
                    );
                    report(AudioStatus::Failed(format!("capture stopped: {reason}")));
                    let control = session.control.clone();
                    tokio::spawn(async move { control.request(Request::AudioStop { reason }).await });
                    sending = None;
                    continue;
                }
                if ticks.is_multiple_of(MARK_EVERY)
                    && let Some((ts, captured)) = capture.mark()
                {
                    let at = session.clock.at(captured);
                    let control = session.control.clone();
                    tokio::spawn(async move { control.request(Request::AudioMark { ts, at }).await });
                }
                if ticks.is_multiple_of(60) {
                    let stats = &capture.stats;
                    debug!(
                        "audio: sent {} packets, {} of them silence; xruns {}, overflow {}",
                        stats.packets.load(Relaxed),
                        stats.silent.load(Relaxed),
                        stats.xruns.load(Relaxed),
                        stats.overflow.load(Relaxed),
                    );
                }
            }
        }
    }
}

/// Answers audio requests with an error until the control stream closes.
async fn idle(mut requests: mpsc::Receiver<Handled>, why: &str) {
    while let Some((_, reply)) = requests.recv().await {
        let _ = reply.send(Response::Error {
            message: why.to_string(),
        });
    }
}

fn unsupported() -> Response {
    Response::Error {
        message: "this agent doesn't support that request".into(),
    }
}

/// Stops a task when dropped.
pub(crate) struct AbortOnDrop(pub(crate) JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
