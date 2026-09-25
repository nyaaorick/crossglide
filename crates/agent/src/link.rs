//! The side channel's QUIC connection. The Mac listens and the PC connects; the PC reconnects
//! with backoff when either side restarts or the network drops.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossglide_audio::device::Source;
use quinn::{ClientConfig, Connection, ConnectionError, Endpoint, Incoming, ServerConfig};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audio;
use crate::clock::{self, Sample, SessionClock};
use crate::config::Role;
use crate::control;
use crate::identity::{Fingerprint, Identity};
use crate::proto::{Hello, PROTOCOL};
use crate::tls;

/// This machine's end of the side channel.
#[derive(Clone, Debug)]
pub enum Mode {
    /// Accept connections on this address (the Mac).
    Listen(SocketAddr),
    /// Connect to this host and port (the PC).
    Connect { host: String, port: u16 },
}

pub struct Settings {
    pub mode: Mode,
    pub identity: Identity,
    /// Fingerprint of the one certificate the peer may use.
    pub peer: Fingerprint,
    /// Audio on or off, and what to send when this side sends (the PC).
    pub audio: Option<Source>,
}

/// What the rest of the agent sees of the side channel.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub peer: Option<Peer>,
    /// Why the last attempt or connection failed; cleared on connect.
    pub problem: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub conn: Connection,
    pub hello: Hello,
    /// Clock offset and RTT, once the first probes are back.
    pub clock: Option<Sample>,
}

/// QUIC application close codes: why this side closed a connection.
mod code {
    use quinn::VarInt;

    /// The agent is stopping.
    pub const STOPPING: VarInt = VarInt::from_u32(0);
    /// A newer connection from the same peer replaced this one.
    pub const REPLACED: VarInt = VarInt::from_u32(1);
    /// The peer's certificate isn't the pinned one.
    pub const NOT_TRUSTED: VarInt = VarInt::from_u32(2);
    /// The peer speaks another protocol version.
    pub const INCOMPATIBLE: VarInt = VarInt::from_u32(3);
    /// The control stream failed, or the peer sent no hello.
    pub const CONTROL: VarInt = VarInt::from_u32(4);
}

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Reconnect delays double from `BACKOFF_MIN` up to `BACKOFF_MAX`. An attempt that gets no
/// answer also waits `tls::IDLE_TIMEOUT` before it fails.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// How long a stopping agent waits for its close to reach the peer.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// Keeps the side channel up until `stop` is cancelled, publishing its state to `status`.
pub async fn run(
    settings: Settings,
    status: watch::Sender<Status>,
    stop: CancellationToken,
) -> Result<()> {
    let role = match settings.mode {
        Mode::Listen(_) => Role::Listen,
        Mode::Connect { .. } => Role::Connect,
    };
    let ctx = Arc::new(Ctx {
        role,
        peer: settings.peer,
        ours: settings.identity.fingerprint,
        audio: settings.audio,
        status,
    });
    match settings.mode {
        Mode::Listen(addr) => {
            listen(addr, tls::server_config(&settings.identity)?, ctx, stop).await
        }
        Mode::Connect { host, port } => {
            let config = tls::client_config(&settings.identity)?;
            connect(&host, port, config, &ctx, stop).await
        }
    }
}

/// What each connection's session needs.
struct Ctx {
    role: Role,
    peer: Fingerprint,
    ours: Fingerprint,
    audio: Option<Source>,
    status: watch::Sender<Status>,
}

impl Ctx {
    /// Logs a connection problem and keeps it in `Status`.
    fn problem(&self, message: String) {
        warn!("{message}");
        self.status.send_modify(|s| s.problem = Some(message));
    }
}

async fn listen(
    addr: SocketAddr,
    config: ServerConfig,
    ctx: Arc<Ctx>,
    stop: CancellationToken,
) -> Result<()> {
    // A port that's taken at startup is an error: another agent is probably running. Later, if
    // the socket fails, it's opened again.
    let mut endpoint = Endpoint::server(config.clone(), addr)
        .with_context(|| format!("can't listen on UDP {addr}"))?;
    let mut backoff = Backoff::default();
    loop {
        info!("listening on UDP {addr}");
        loop {
            tokio::select! {
                () = stop.cancelled() => {
                    close(&endpoint).await;
                    return Ok(());
                }
                incoming = endpoint.accept() => match incoming {
                    Some(incoming) => _ = tokio::spawn(accept(incoming, ctx.clone())),
                    None => break,
                },
            }
        }
        endpoint = loop {
            match Endpoint::server(config.clone(), addr) {
                Ok(endpoint) => break endpoint,
                Err(e) => ctx.problem(format!("can't listen on UDP {addr}: {e}")),
            }
            tokio::select! {
                () = sleep(backoff.next()) => {}
                () = stop.cancelled() => return Ok(()),
            }
        };
        backoff.reset();
    }
}

async fn accept(incoming: Incoming, ctx: Arc<Ctx>) {
    let addr = incoming.remote_address();
    match incoming.await {
        Ok(conn) => _ = session(conn, &ctx).await,
        Err(e) => ctx.problem(explain(addr, &e, ctx.ours)),
    }
}

async fn connect(
    host: &str,
    port: u16,
    config: ClientConfig,
    ctx: &Ctx,
    stop: CancellationToken,
) -> Result<()> {
    let target = format!("{host}:{port}");
    let mut backoff = Backoff::default();
    let mut failures = Failures::default();
    loop {
        let started = Instant::now();
        let attempt = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            attempt = dial(host, port, &config) => attempt,
        };
        match attempt {
            Ok((endpoint, conn)) => {
                if let Some((count, since)) = failures.recovered() {
                    info!(
                        "reached {target} after {count} failed {} over {} s",
                        if count == 1 { "attempt" } else { "attempts" },
                        since.elapsed().as_secs()
                    );
                }
                tokio::select! {
                    up = session(conn.clone(), ctx) => if up { backoff.reset() },
                    () = stop.cancelled() => {
                        close(&endpoint).await;
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                let message = match e.downcast_ref::<ConnectionError>() {
                    Some(e) => explain_target(&target, e, ctx.ours),
                    None => format!("{e:#}"),
                };
                failures.failed(ctx, &target, started, message);
            }
        }
        tokio::select! {
            () = sleep(backoff.next()) => {}
            () = stop.cancelled() => return Ok(()),
        }
    }
}

/// One connection attempt: resolve `host`, then a QUIC handshake from a new UDP socket, so a
/// socket broken by a network change isn't reused.
async fn dial(host: &str, port: u16, config: &ClientConfig) -> Result<(Endpoint, Connection)> {
    let addr = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("can't resolve {host}"))?
        .next()
        .with_context(|| format!("{host} has no address"))?;
    let local = if addr.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let endpoint = Endpoint::client(local)?;
    let conn = endpoint
        .connect_with(config.clone(), addr, tls::SERVER_NAME)?
        .await?;
    Ok((endpoint, conn))
}

/// Closes every connection on `endpoint`, telling peers the agent is stopping, and gives that a
/// moment to be sent.
async fn close(endpoint: &Endpoint) {
    endpoint.close(code::STOPPING, b"agent stopping");
    let _ = timeout(CLOSE_GRACE, endpoint.wait_idle()).await;
}

/// Runs one connection until it closes. Returns whether it got through the hello exchange,
/// i.e. whether the peer accepted it.
async fn session(conn: Connection, ctx: &Ctx) -> bool {
    let addr = conn.remote_address();

    // Pinning: nothing is sent to or read from the peer before its certificate is checked.
    let presented = tls::peer_fingerprint(&conn);
    if presented != Some(ctx.peer) {
        let presented = presented.map_or_else(|| "missing".to_string(), |f| f.to_string());
        ctx.problem(format!(
            "refused {addr}: its certificate fingerprint is {presented}, but peer_fingerprint \
             in agent.toml is {}",
            ctx.peer
        ));
        conn.close(code::NOT_TRUSTED, b"certificate not trusted");
        return false;
    }

    let (stream, hello) = match timeout(HELLO_TIMEOUT, control::open(&conn, ctx.role)).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(e)) => {
            let message = match conn.close_reason() {
                Some(reason) => explain(addr, &reason, ctx.ours),
                None => format!("control stream to {addr} failed: {e:#}"),
            };
            ctx.problem(message);
            conn.close(code::CONTROL, b"control stream failed");
            return false;
        }
        Err(_) => {
            ctx.problem(format!("{addr} sent no hello within {HELLO_TIMEOUT:?}"));
            conn.close(code::CONTROL, b"no hello");
            return false;
        }
    };
    if hello.protocol != PROTOCOL {
        ctx.problem(format!(
            "refused {} at {addr}: it speaks protocol {} and this agent speaks {PROTOCOL}; \
             update crossglide on both machines",
            hello.host, hello.protocol
        ));
        conn.close(code::INCOMPATIBLE, b"incompatible protocol version");
        return false;
    }

    let ours = Hello::ours();
    if hello.agent != ours.agent {
        warn!(
            "{} runs crossglide-agent {} and this machine runs {}",
            hello.host, hello.agent, ours.agent
        );
    }
    let features = ours.shared_features(&hello);
    info!(
        "connected to {} ({}, agent {}) at {addr}; max datagram {} bytes; shared features: {}",
        hello.host,
        hello.os,
        hello.agent,
        conn.max_datagram_size()
            .map_or_else(|| "none".to_string(), |n| n.to_string()),
        if features.is_empty() {
            "none".to_string()
        } else {
            features.join(", ")
        }
    );

    let id = conn.stable_id();
    ctx.status.send_modify(|s| {
        s.problem = None;
        let new = Peer {
            conn: conn.clone(),
            hello: hello.clone(),
            clock: None,
        };
        if let Some(old) = s.peer.replace(new) {
            // The peer came back (a restart, or waking from sleep) before this side noticed the
            // old connection was dead.
            info!(
                "closing the previous connection from {} at {}",
                old.hello.host,
                old.conn.remote_address()
            );
            old.conn
                .close(code::REPLACED, b"replaced by a newer connection");
        }
    });

    let session_clock = SessionClock::start();
    let (control, requests, serve) = control::serve(stream, session_clock);
    let (peer_clock, peer_clock_rx) = watch::channel(None);
    let update_clock = |estimate| {
        peer_clock.send_replace(Some(estimate));
        ctx.status.send_modify(|s| {
            if let Some(peer) = &mut s.peer
                && peer.conn.stable_id() == id
            {
                peer.clock = Some(estimate);
            }
        })
    };
    let audio = async {
        let session = audio::Session {
            conn: conn.clone(),
            control: control.clone(),
            clock: session_clock,
            peer_clock: peer_clock_rx,
            peer: hello.host.clone(),
        };
        match (ctx.audio, ctx.role) {
            _ if !features.iter().any(|f| f == audio::FEATURE) => {
                if ctx.audio.is_some() {
                    info!(
                        "audio: {} doesn't support audio; update crossglide there",
                        hello.host
                    );
                }
                drop(requests);
            }
            (None, _) => drop(requests),
            (Some(_), Role::Listen) => audio::play(session, requests).await,
            (Some(source), Role::Connect) => audio::send(session, source, requests).await,
        }
        // Runs until the control stream closes, which `serve` reports.
        std::future::pending::<()>().await
    };
    let served = tokio::select! {
        biased;
        served = serve => served,
        () = clock::probe(&control, &session_clock, &hello.host, update_clock) => Ok(()),
        () = audio => Ok(()),
    };
    if conn.close_reason().is_none() {
        if let Err(e) = &served {
            warn!("control stream to {} failed: {e:#}", hello.host);
        }
        conn.close(code::CONTROL, b"control stream closed");
    }
    ctx.status.send_if_modified(|s| {
        let ours = s.peer.as_ref().is_some_and(|p| p.conn.stable_id() == id);
        if ours {
            s.peer = None;
        }
        ours
    });

    let reason = conn.close_reason().expect("closed above if not already");
    let (expected, why) = describe(&reason);
    if expected {
        info!("disconnected from {}: {why}", hello.host);
    } else {
        warn!("disconnected from {}: {why}", hello.host);
    }
    true
}

/// A connection that failed or closed before the hellos, explained for the log.
fn explain(addr: SocketAddr, reason: &ConnectionError, ours: Fingerprint) -> String {
    explain_target(&addr.to_string(), reason, ours)
}

fn explain_target(target: &str, reason: &ConnectionError, ours: Fingerprint) -> String {
    match reason {
        ConnectionError::ApplicationClosed(close) if close.error_code == code::NOT_TRUSTED => {
            format!(
                "{target} refused this machine's certificate: set peer_fingerprint in its \
                 agent.toml to {ours}"
            )
        }
        ConnectionError::ApplicationClosed(close) if close.error_code == code::INCOMPATIBLE => {
            format!(
                "{target} refused this agent's protocol version {PROTOCOL}; update crossglide on \
                 both machines"
            )
        }
        other => format!("can't connect to {target}: {}", describe(other).1),
    }
}

/// Why a connection closed, and whether that's expected: the peer stopping, or this agent
/// closing it.
fn describe(reason: &ConnectionError) -> (bool, String) {
    match reason {
        ConnectionError::ApplicationClosed(close) => (
            close.error_code == code::STOPPING,
            format!(
                "it closed the connection ({})",
                String::from_utf8_lossy(&close.reason)
            ),
        ),
        ConnectionError::LocallyClosed => (true, "closed by this agent".into()),
        ConnectionError::TimedOut => (
            false,
            format!(
                "nothing heard for {} s (asleep, or the network dropped)",
                tls::IDLE_TIMEOUT.as_secs()
            ),
        ),
        ConnectionError::Reset => (false, "it reset the connection (did it restart?)".into()),
        other => (false, other.to_string()),
    }
}

/// Delay before the next connection attempt.
struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: BACKOFF_MIN }
    }
}

impl Backoff {
    fn next(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(BACKOFF_MAX);
        delay
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Consecutive failed attempts. A long outage (the Mac asleep) logs its error once, not once per
/// attempt.
#[derive(Default)]
struct Failures {
    count: u32,
    /// When the first failed attempt began.
    since: Option<Instant>,
    last: Option<String>,
}

impl Failures {
    fn failed(&mut self, ctx: &Ctx, target: &str, started: Instant, message: String) {
        self.count += 1;
        self.since.get_or_insert(started);
        if self.last.as_ref() == Some(&message) {
            debug!("still failing to reach {target} (attempt {})", self.count);
        } else {
            ctx.problem(message.clone());
            self.last = Some(message);
        }
    }

    /// Clears the record after a successful attempt; returns how many attempts failed and since
    /// when.
    fn recovered(&mut self) -> Option<(u32, Instant)> {
        let since = self.since.take()?;
        self.last = None;
        Some((std::mem::take(&mut self.count), since))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::task::JoinHandle;

    const WAIT: Duration = Duration::from_secs(20);

    struct Agent {
        status: watch::Receiver<Status>,
        stop: CancellationToken,
        task: JoinHandle<Result<()>>,
    }

    impl Agent {
        fn start(mode: Mode, identity: &Identity, peer: &Identity) -> Self {
            let (sender, status) = watch::channel(Status::default());
            let stop = CancellationToken::new();
            let settings = Settings {
                mode,
                identity: identity.clone(),
                peer: peer.fingerprint,
                audio: None,
            };
            let task = tokio::spawn(run(settings, sender, stop.clone()));
            Self { status, stop, task }
        }

        async fn wait_for(&mut self, what: &str, f: impl FnMut(&Status) -> bool) -> Status {
            timeout(WAIT, self.status.wait_for(f))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("the agent stopped")
                .clone()
        }

        async fn stop(self) {
            self.stop.cancel();
            self.task.await.unwrap().unwrap();
        }
    }

    fn listen(port: u16) -> Mode {
        Mode::Listen(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    fn connect(port: u16) -> Mode {
        Mode::Connect {
            host: "127.0.0.1".into(),
            port,
        }
    }

    /// A UDP port that was free a moment ago.
    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn wait_until_free(port: u16) {
        while std::net::UdpSocket::bind(("127.0.0.1", port)).is_err() {
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn identities<const N: usize>() -> [Identity; N] {
        std::array::from_fn(|_| Identity::generate().unwrap())
    }

    fn measured(s: &Status) -> bool {
        s.peer.as_ref().is_some_and(|p| p.clock.is_some())
    }

    #[tokio::test]
    async fn connects_measures_the_clock_and_carries_datagrams() {
        let [mac, pc] = identities();
        let port = free_port();
        let mut m = Agent::start(listen(port), &mac, &pc);
        let mut p = Agent::start(connect(port), &pc, &mac);
        let on_mac = m
            .wait_for("the Mac to measure the PC", measured)
            .await
            .peer
            .unwrap();
        let on_pc = p
            .wait_for("the PC to measure the Mac", measured)
            .await
            .peer
            .unwrap();

        assert_eq!(on_pc.hello, Hello::ours());
        // Both agents read the same clock here, so the offset is only measurement error.
        let clock = on_pc.clock.unwrap();
        assert!(clock.offset.abs() < 2_000_000, "{clock:?}");
        assert!(on_pc.conn.max_datagram_size().is_some());

        on_pc
            .conn
            .send_datagram(b"from the pc".to_vec().into())
            .unwrap();
        let got = timeout(WAIT, on_mac.conn.read_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&got[..], b"from the pc");
        on_mac
            .conn
            .send_datagram(b"from the mac".to_vec().into())
            .unwrap();
        let got = timeout(WAIT, on_pc.conn.read_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&got[..], b"from the mac");

        // A stopping agent says so, so the Mac notices well before the idle timeout.
        p.stop().await;
        timeout(
            Duration::from_secs(3),
            m.wait_for("the Mac to see the PC leave", |s| s.peer.is_none()),
        )
        .await
        .expect("the Mac noticed within 3 s");
        m.stop().await;
    }

    #[tokio::test]
    async fn the_mac_refuses_an_untrusted_pc() {
        let [mac, pc, stranger] = identities();
        let port = free_port();
        let mut m = Agent::start(listen(port), &mac, &stranger);
        let mut p = Agent::start(connect(port), &pc, &mac);

        let problem = m
            .wait_for("a refusal", |s| s.problem.is_some())
            .await
            .problem
            .unwrap();
        assert!(problem.starts_with("refused 127.0.0.1:"), "{problem}");
        assert!(problem.contains(&pc.fingerprint.to_string()), "{problem}");
        assert!(
            problem.contains(&stranger.fingerprint.to_string()),
            "{problem}"
        );

        let problem = p
            .wait_for("the refusal", |s| s.problem.is_some())
            .await
            .problem
            .unwrap();
        assert!(
            problem.contains("refused this machine's certificate"),
            "{problem}"
        );
        assert!(problem.contains(&pc.fingerprint.to_string()), "{problem}");

        assert!(m.status.borrow().peer.is_none());
        assert!(p.status.borrow().peer.is_none());
        p.stop().await;
        m.stop().await;
    }

    #[tokio::test]
    async fn the_pc_refuses_an_untrusted_mac() {
        let [mac, pc, stranger] = identities();
        let port = free_port();
        let mut m = Agent::start(listen(port), &mac, &pc);
        let mut p = Agent::start(connect(port), &pc, &stranger);

        let problem = p
            .wait_for("a refusal", |s| s.problem.is_some())
            .await
            .problem
            .unwrap();
        assert!(problem.contains(&mac.fingerprint.to_string()), "{problem}");
        assert!(
            problem.contains(&stranger.fingerprint.to_string()),
            "{problem}"
        );

        let problem = m
            .wait_for("the refusal", |s| s.problem.is_some())
            .await
            .problem
            .unwrap();
        assert!(
            problem.contains("refused this machine's certificate"),
            "{problem}"
        );
        assert!(problem.contains(&mac.fingerprint.to_string()), "{problem}");

        assert!(m.status.borrow().peer.is_none());
        p.stop().await;
        m.stop().await;
    }

    #[tokio::test]
    async fn the_pc_reconnects_when_the_mac_restarts() {
        let [mac, pc] = identities();
        let port = free_port();
        let m = Agent::start(listen(port), &mac, &pc);
        let mut p = Agent::start(connect(port), &pc, &mac);
        p.wait_for("the first connection", |s| s.peer.is_some())
            .await;

        m.stop().await;
        p.wait_for("the PC to see the Mac leave", |s| s.peer.is_none())
            .await;
        wait_until_free(port).await;
        let m = Agent::start(listen(port), &mac, &pc);
        p.wait_for("the second connection", |s| s.peer.is_some())
            .await;

        p.stop().await;
        m.stop().await;
    }

    #[tokio::test]
    async fn a_new_connection_from_the_pc_replaces_the_old_one() {
        let [mac, pc] = identities();
        let port = free_port();
        let mut m = Agent::start(listen(port), &mac, &pc);
        let first = Agent::start(connect(port), &pc, &mac);
        let old = m
            .wait_for("the first connection", |s| s.peer.is_some())
            .await;
        let old = old.peer.unwrap().conn;

        // Like the PC coming back from sleep while the Mac still holds the old connection.
        let second = Agent::start(connect(port), &pc, &mac);
        m.wait_for("the second connection", |s| {
            s.peer
                .as_ref()
                .is_some_and(|p| p.conn.remote_address() != old.remote_address())
        })
        .await;
        assert!(matches!(
            old.close_reason(),
            Some(ConnectionError::LocallyClosed)
        ));

        first.stop().await;
        second.stop().await;
        m.stop().await;
    }

    #[test]
    fn backoff_doubles_up_to_the_cap_and_resets() {
        let mut backoff = Backoff::default();
        let delays: Vec<_> = (0..7).map(|_| backoff.next().as_millis()).collect();
        assert_eq!(delays, [500, 1000, 2000, 4000, 8000, 10000, 10000]);
        backoff.reset();
        assert_eq!(backoff.next(), BACKOFF_MIN);
    }
}
