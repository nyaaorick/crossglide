//! Finding the Mac on the local network, so the PC doesn't depend on an IP address in agent.toml
//! that DHCP, or joining another Wi-Fi network, changes.
//!
//! The PC broadcasts a probe naming the fingerprint it pins; the Mac with that fingerprint answers
//! with its side channel port, and the PC connects to wherever the answer came from. Nothing here
//! is trusted: the PC still checks the Mac's certificate when it connects, so a forged answer only
//! costs one failed attempt.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep, timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::identity::Fingerprint;

const MAGIC: &[u8; 4] = b"CGLD";
const VERSION: u8 = 1;
const PROBE: u8 = 0;
const ANSWER: u8 = 1;
/// Probes are resent this often while waiting, in case one is lost.
const RESEND: Duration = Duration::from_millis(250);

/// The discovery port: the one after the side channel's.
pub fn port(side_channel: u16) -> u16 {
    side_channel.checked_add(1).unwrap_or(side_channel - 1)
}

#[derive(Debug, PartialEq)]
enum Message {
    /// "Whoever has this fingerprint, where are you?"
    Probe(Fingerprint),
    /// "I have this fingerprint, and the side channel is on this port."
    Answer(Fingerprint, u16),
}

impl Message {
    fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(VERSION);
        match self {
            Self::Probe(fingerprint) => {
                out.push(PROBE);
                out.extend_from_slice(fingerprint.as_bytes());
            }
            Self::Answer(fingerprint, port) => {
                out.push(ANSWER);
                out.extend_from_slice(fingerprint.as_bytes());
                out.extend_from_slice(&port.to_be_bytes());
            }
        }
        out
    }

    /// `None` for anything that isn't a message of this version.
    fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(MAGIC)?.strip_prefix(&[VERSION])?;
        let (&kind, rest) = rest.split_first()?;
        let fingerprint = Fingerprint::from_bytes(rest.get(..32)?.try_into().ok()?);
        match (kind, &rest[32..]) {
            (PROBE, []) => Some(Self::Probe(fingerprint)),
            (ANSWER, &[hi, lo]) => Some(Self::Answer(fingerprint, u16::from_be_bytes([hi, lo]))),
            _ => None,
        }
    }
}

/// Answers probes for `ours` on `addr` until `stop`, telling the PC the side channel is on
/// `side_channel`. Runs on the Mac. Without it the PC can still connect to `server`, so a port
/// that's taken is only a warning.
pub async fn answer(
    addr: SocketAddr,
    ours: Fingerprint,
    side_channel: u16,
    stop: CancellationToken,
) {
    let socket = match UdpSocket::bind(addr).await {
        Ok(socket) => socket,
        Err(e) => {
            warn!(
                "can't answer on UDP {addr}, so the PC won't find this machine by itself \
                 (set `server` on the PC instead): {e}"
            );
            return;
        }
    };
    let reply = Message::Answer(ours, side_channel).encode();
    let mut buf = [0; 64];
    loop {
        let received = tokio::select! {
            () = stop.cancelled() => return,
            received = socket.recv_from(&mut buf) => received,
        };
        match received {
            Ok((n, from)) => {
                if Message::decode(&buf[..n]) == Some(Message::Probe(ours)) {
                    debug!("discovery: answering {from}");
                    if let Err(e) = socket.send_to(&reply, from).await {
                        debug!("discovery: can't answer {from}: {e}");
                    }
                }
            }
            // On Windows a UDP socket reports an earlier send's ICMP error here; nothing to do.
            Err(e) => {
                debug!("discovery: {e}");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Broadcasts probes for `wanted` on every IPv4 network this machine is on, and returns the side
/// channel address of the first machine that answers within `wait`. Runs on the PC.
pub async fn find(port: u16, wanted: Fingerprint, wait: Duration) -> Result<Option<SocketAddr>> {
    find_at(&broadcast_targets(port), wanted, wait).await
}

async fn find_at(
    targets: &[SocketAddr],
    wanted: Fingerprint,
    wait: Duration,
) -> Result<Option<SocketAddr>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("can't open a UDP socket")?;
    socket.set_broadcast(true)?;
    let probe = Message::Probe(wanted).encode();
    let deadline = Instant::now() + wait;
    let mut buf = [0; 64];
    loop {
        let mut sent = false;
        for target in targets {
            match socket.send_to(&probe, target).await {
                Ok(_) => sent = true,
                Err(e) => debug!("discovery: can't probe {target}: {e}"),
            }
        }
        if !sent {
            return Ok(None);
        }
        let resend = (Instant::now() + RESEND).min(deadline);
        while let Ok(received) = timeout_at(resend, socket.recv_from(&mut buf)).await {
            let Ok((n, from)) = received else { continue };
            if let Some(Message::Answer(fingerprint, port)) = Message::decode(&buf[..n])
                && fingerprint == wanted
            {
                return Ok(Some(SocketAddr::new(from.ip(), port)));
            }
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
    }
}

/// The limited broadcast address, plus each network's own broadcast address, since Windows sends
/// 255.255.255.255 out of one network adapter only.
fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut targets = BTreeSet::from([Ipv4Addr::BROADCAST]);
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            for interface in interfaces {
                if let if_addrs::IfAddr::V4(v4) = &interface.addr
                    && !interface.is_loopback()
                    && interface.is_oper_up()
                    && v4.prefixlen < 31
                {
                    targets.insert(v4.broadcast.unwrap_or_else(|| {
                        Ipv4Addr::from_bits(v4.ip.to_bits() | !v4.netmask.to_bits())
                    }));
                }
            }
        }
        Err(e) => debug!("discovery: can't list network interfaces: {e}"),
    }
    targets
        .into_iter()
        .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn messages_round_trip_and_junk_is_ignored() {
        let fingerprint = Identity::generate().unwrap().fingerprint;
        for message in [
            Message::Probe(fingerprint),
            Message::Answer(fingerprint, 24800),
        ] {
            let bytes = message.encode();
            assert_eq!(Message::decode(&bytes), Some(message));
            assert_eq!(Message::decode(&bytes[..bytes.len() - 1]), None);
        }
        let mut other_version = Message::Probe(fingerprint).encode();
        other_version[4] = VERSION + 1;
        assert_eq!(Message::decode(&other_version), None);
        assert_eq!(Message::decode(b"hello"), None);
    }

    #[test]
    fn discovery_port_follows_the_side_channel() {
        assert_eq!(port(24800), 24801);
        assert_eq!(port(u16::MAX), u16::MAX - 1);
    }

    #[test]
    fn broadcast_targets_include_the_limited_broadcast() {
        let targets = broadcast_targets(24801);
        assert!(targets.contains(&SocketAddr::from((Ipv4Addr::BROADCAST, 24801))));
        assert!(targets.iter().all(|t| !t.ip().is_loopback()));
    }

    #[tokio::test]
    async fn finds_the_machine_with_the_wanted_fingerprint_only() {
        let [mac, stranger] = std::array::from_fn(|_| Identity::generate().unwrap().fingerprint);
        let addr = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let stop = CancellationToken::new();
        let answering = tokio::spawn(answer(addr, mac, 5000, stop.clone()));

        let found = find_at(&[addr], mac, Duration::from_secs(5)).await.unwrap();
        assert_eq!(found, Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 5000))));

        let started = Instant::now();
        let found = find_at(&[addr], stranger, Duration::from_millis(600))
            .await
            .unwrap();
        assert_eq!(found, None);
        assert!(started.elapsed() >= Duration::from_millis(600));

        stop.cancel();
        answering.await.unwrap();
    }
}
