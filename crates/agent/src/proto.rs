//! Control-stream messages and their framing: each is JSON with a 4-byte big-endian length in
//! front. JSON keeps the stream readable when debugging and lets a newer agent add fields.

use std::io;

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::clock::Nanos;

/// Version of the control-stream protocol. Agents only talk to the same version.
pub const PROTOCOL: u32 = 1;

/// Optional features this agent supports. One is used only when both sides list it.
const FEATURES: &[&str] = &[crate::audio::FEATURE];

/// Largest message either side accepts.
const MAX_FRAME: usize = 1 << 20;

/// First message on the control stream, sent by both sides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// crossglide-agent version, e.g. "0.1.0". A mismatch only warns.
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub features: Vec<String>,
}

impl Hello {
    pub fn ours() -> Self {
        Self {
            protocol: PROTOCOL,
            agent: env!("CARGO_PKG_VERSION").to_string(),
            host: crate::hostname(),
            os: std::env::consts::OS.to_string(),
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        }
    }

    /// Features both this hello and `other` list.
    pub fn shared_features(&self, other: &Hello) -> Vec<String> {
        self.features
            .iter()
            .filter(|f| other.features.contains(f))
            .cloned()
            .collect()
    }
}

/// Every message after the hellos. Either side can send requests; each gets one response with
/// the same id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Request {
        id: u64,
        body: Request,
    },
    Response {
        id: u64,
        body: Response,
    },
    /// A kind this agent doesn't know, from a newer agent. Ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Clock probe; `t1` is the sender's clock when it sent this.
    Time { t1: Nanos },
    /// From the Mac, once it can play: start sending audio.
    AudioStart,
    /// Either way: this side has stopped audio (its device failed, say), and why.
    AudioStop { reason: String },
    /// From the PC, every few seconds while sending: the audio frame with timestamp `ts` was
    /// captured at `at` on the PC's session clock. The Mac measures latency from it.
    AudioMark { ts: u32, at: Nanos },
    /// A request this agent doesn't know. Answered with an error.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Answer to `Time`: `t2` when the request arrived and `t3` when this was sent, both on the
    /// responder's clock.
    Time {
        t1: Nanos,
        t2: Nanos,
        t3: Nanos,
    },
    Ok,
    Error {
        message: String,
    },
    /// A response this agent doesn't know.
    #[serde(other)]
    Unknown,
}

pub async fn write<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    if body.len() > MAX_FRAME {
        bail!("message too large to send ({} bytes)", body.len());
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame).await?;
    Ok(())
}

/// Reads one message; `None` if the stream ended where a message would start.
pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<Option<T>> {
    let mut len = [0; 4];
    match stream.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("peer sent a {len}-byte message; the limit is {MAX_FRAME}");
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let sent = [
            Message::Request {
                id: 7,
                body: Request::Time { t1: -5 },
            },
            Message::Response {
                id: 7,
                body: Response::Time {
                    t1: -5,
                    t2: 1_000_000_000_000_000_000,
                    t3: i64::MAX,
                },
            },
            Message::Response {
                id: 8,
                body: Response::Error {
                    message: "no".into(),
                },
            },
        ];
        for message in &sent {
            write(&mut a, message).await.unwrap();
        }
        drop(a);
        for message in &sent {
            assert_eq!(
                read::<Message>(&mut b).await.unwrap().as_ref(),
                Some(message)
            );
        }
        assert_eq!(read::<Message>(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_frames_are_refused() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME as u32 + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read::<Message>(&mut b).await.is_err());
    }

    #[test]
    fn newer_peers_messages_still_parse() {
        let hello: Hello = serde_json::from_str(
            r#"{"protocol":1,"agent":"9.0.0","host":"pc","os":"windows","features":["x"],"new":1}"#,
        )
        .unwrap();
        assert_eq!(hello.agent, "9.0.0");

        let unknown_kind: Message = serde_json::from_str(r#"{"kind":"event","what":1}"#).unwrap();
        assert_eq!(unknown_kind, Message::Unknown);

        let unknown_request: Message =
            serde_json::from_str(r#"{"kind":"request","id":3,"body":{"type":"logs","n":5}}"#)
                .unwrap();
        assert_eq!(
            unknown_request,
            Message::Request {
                id: 3,
                body: Request::Unknown
            }
        );
    }

    #[test]
    fn shared_features_are_the_intersection() {
        let mut a = Hello::ours();
        let mut b = Hello::ours();
        a.features = vec!["audio".into(), "logs".into()];
        b.features = vec!["logs".into(), "touch".into()];
        assert_eq!(a.shared_features(&b), vec!["logs".to_string()]);
    }
}
