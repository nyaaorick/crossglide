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

/// A request from the peer that this side's features answer (anything but a clock probe), and
/// where the answer goes.
pub type Handled = (Request, oneshot::Sender<Response>);

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

/// Serves the control stream: answers the peer's clock probes, passes its other requests to
/// the returned receiver, and sends the requests made through the returned `Control`. Arrival
/// times and clock answers are on `clock`. The future ends when the stream closes.
pub fn serve(
    stream: Stream,
    clock: SessionClock,
) -> (
    Control,
    mpsc::Receiver<Handled>,
    impl Future<Output = Result<()>>,
) {
    let (requests, outgoing) = mpsc::channel(16);
    let (handler, handled) = mpsc::channel(16);
    (
        Control { requests },
        handled,
        run(stream, clock, outgoing, handler),
    )
}

async fn run(
    stream: Stream,
    clock: SessionClock,
    mut outgoing: mpsc::Receiver<Outgoing>,
    handler: mpsc::Sender<Handled>,
) -> Result<()> {
    let Stream { mut send, recv } = stream;
    // Reads get a task of their own: a read isn't cancel-safe, so it can't wait in `select!`.
    let (arrivals, mut incoming) = mpsc::channel(16);
    let reader = tokio::spawn(read_messages(recv, clock, arrivals));
    // Answers from the handler, which may take a while (opening a sound device).
    let (answered, mut answers) = mpsc::channel::<(u64, Response)>(16);
    let mut pending: HashMap<u64, oneshot::Sender<Reply>> = HashMap::new();
    let mut next_id = 0u64;
    loop {
        tokio::select! {
            arrival = incoming.recv() => {
                let Some((message, arrived)) = arrival else { break };
                match message {
                    Message::Request { id, body: Request::Time { t1 } } => {
                        let body = Response::Time { t1, t2: arrived, t3: clock.now() };
                        proto::write(&mut send, &Message::Response { id, body }).await?;
                    }
                    Message::Request { id, body: Request::Unknown } => {
                        let body = unsupported();
                        proto::write(&mut send, &Message::Response { id, body }).await?;
                    }
                    Message::Request { id, body } => {
                        let (reply, answer) = oneshot::channel();
                        if handler.try_send((body, reply)).is_err() {
                            let body = unsupported();
                            proto::write(&mut send, &Message::Response { id, body }).await?;
                            continue;
                        }
                        let answered = answered.clone();
                        tokio::spawn(async move {
                            let body = answer.await.unwrap_or_else(|_| unsupported());
                            let _ = answered.send((id, body)).await;
                        });
                    }
                    Message::Response { id, body } => match pending.remove(&id) {
                        // The requester may have given up waiting; that's fine.
                        Some(reply) => _ = reply.send(Reply { body, arrived }),
                        None => debug!("response {id} matches no request"),
                    },
                    Message::Unknown => debug!("ignored a message of a kind this agent doesn't know"),
                }
            }
            Some((id, body)) = answers.recv() => {
                proto::write(&mut send, &Message::Response { id, body }).await?;
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

fn unsupported() -> Response {
    Response::Error {
        message: "this agent doesn't support that request".into(),
    }
}
