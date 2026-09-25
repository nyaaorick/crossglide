//! The control stream: one bidirectional QUIC stream per connection, opened by the connecting
//! side. Each side sends a `Hello` first; after that either side sends requests and answers the
//! other's.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::clock::{Nanos, SessionClock};
use crate::config::Role;
use crate::proto::{self, Hello, Message, Request, Response};

/// Both halves of the control stream, once the hellos are exchanged.
pub struct Stream {
    send: SendStream,
    recv: RecvStream,
}

/// Sends requests to the peer over the control stream. Cheap to clone.
#[derive(Clone)]
pub struct Control {
    requests: mpsc::Sender<Outgoing>,
}

/// A response, and when it arrived by our clock.
pub struct Reply {
    pub body: Response,
    pub arrived: Nanos,
}

struct Outgoing {
    body: Request,
    reply: oneshot::Sender<Reply>,
}

impl Control {
    pub async fn request(&self, body: Request) -> Result<Reply> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Outgoing { body, reply })
            .await
            .map_err(|_| anyhow!("the control stream is closed"))?;
        answer
            .await
            .map_err(|_| anyhow!("the control stream closed before the answer"))
    }
}

/// Opens the control stream (connecting side) or accepts it (listening side), sends our hello
/// and reads the peer's.
pub async fn open(conn: &Connection, role: Role) -> Result<(Stream, Hello)> {
    let (mut send, mut recv) = match role {
        Role::Connect => conn.open_bi().await?,
        Role::Listen => conn.accept_bi().await?,
    };
    proto::write(&mut send, &Hello::ours()).await?;
    let hello = proto::read(&mut recv)
        .await?
        .context("the peer closed the control stream before its hello")?;
    Ok((Stream { send, recv }, hello))
}

/// Serves the control stream: answers the peer's requests and sends the ones made through the
/// returned `Control`. Arrival times and clock answers are on `clock`. The future ends when the
/// stream closes.
pub fn serve(stream: Stream, clock: SessionClock) -> (Control, impl Future<Output = Result<()>>) {
    let (requests, outgoing) = mpsc::channel(16);
    (Control { requests }, run(stream, clock, outgoing))
}

async fn run(
    stream: Stream,
    clock: SessionClock,
    mut outgoing: mpsc::Receiver<Outgoing>,
) -> Result<()> {
    let Stream { mut send, recv } = stream;
    // Reads get a task of their own: a read isn't cancel-safe, so it can't wait in `select!`.
    let (arrivals, mut incoming) = mpsc::channel(16);
    let reader = tokio::spawn(read_messages(recv, clock, arrivals));
    let mut pending: HashMap<u64, oneshot::Sender<Reply>> = HashMap::new();
    let mut next_id = 0u64;
    loop {
        tokio::select! {
            arrival = incoming.recv() => {
                let Some((message, arrived)) = arrival else { break };
                match message {
                    Message::Request { id, body } => {
                        let body = answer(body, arrived, &clock);
                        proto::write(&mut send, &Message::Response { id, body }).await?;
                    }
                    Message::Response { id, body } => match pending.remove(&id) {
                        // The requester may have given up waiting; that's fine.
                        Some(reply) => _ = reply.send(Reply { body, arrived }),
                        None => debug!("response {id} matches no request"),
                    },
                    Message::Unknown => debug!("ignored a message of a kind this agent doesn't know"),
                }
            }
            Some(Outgoing { body, reply }) = outgoing.recv() => {
                next_id += 1;
                pending.insert(next_id, reply);
                proto::write(&mut send, &Message::Request { id: next_id, body }).await?;
            }
        }
    }
    reader.await?
}

/// Forwards each message with its arrival time, until the stream ends or fails.
async fn read_messages(
    mut recv: RecvStream,
    clock: SessionClock,
    arrivals: mpsc::Sender<(Message, Nanos)>,
) -> Result<()> {
    while let Some(message) = proto::read(&mut recv).await? {
        if arrivals.send((message, clock.now())).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn answer(request: Request, arrived: Nanos, clock: &SessionClock) -> Response {
    match request {
        Request::Time { t1 } => Response::Time {
            t1,
            t2: arrived,
            t3: clock.now(),
        },
        Request::Unknown => Response::Error {
            message: "this agent doesn't support that request".into(),
        },
    }
}
