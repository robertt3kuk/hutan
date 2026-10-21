//! JSON-RPC 2.0 over the server's stdio, framed by [`LspCodec`].
//!
//! **Outgoing** messages are serialized by the caller and queued for one writer task in a
//! queue bounded twice: at most [`OUTGOING_QUEUE`] messages and at most [`OUTGOING_BYTES`]
//! bytes (a message larger than the byte budget waits until the queue is empty and then takes
//! all of it). A sender waits for room — within its request's deadline — so a server that
//! stops reading slows its callers down instead of growing memory. `$/cancelRequest` is
//! advisory and dropped when the queue is full.
//!
//! **Incoming** messages are handled one at a time by the reader task, with no queue in
//! between: responses complete the waiting request (at most [`MAX_PENDING`] wait at once),
//! server requests are answered by the [`Handler`] (the answer waits for room in the outgoing
//! queue, which in turn stops reading the server: back-pressure, not buffering), and
//! notifications go to the handler, which keeps bounded state.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, warn};

use crate::codec::{Frame, LspCodec};
use crate::error::LspError;
use crate::process::{lock, stderr_tail};

/// Messages queued for the server at most.
pub const OUTGOING_QUEUE: usize = 64;
/// Bytes queued for the server at most.
pub const OUTGOING_BYTES: usize = 16 * 1024 * 1024;
/// Requests waiting for an answer at most.
pub const MAX_PENDING: usize = 1024;

/// JSON-RPC error codes the client answers with.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;

/// Answers server requests and takes notifications; `generation` tells which server instance
/// spoke (notifications of an instance that is being replaced are ignored).
pub(crate) trait Handler: Send + Sync + 'static {
    fn notification(&self, generation: u64, method: &str, params: Option<&RawValue>);
    fn request(
        &self,
        generation: u64,
        method: &str,
        params: Option<&RawValue>,
    ) -> Result<Value, (i64, String)>;
}

#[derive(Debug)]
enum Failure {
    Remote { code: i64, message: String },
    Closed,
}

/// The raw `result` of a response; `None` for `null` or no result.
type Reply = Result<Option<Box<RawValue>>, Failure>;

#[derive(Debug, Default)]
struct Pending {
    closed: bool,
    calls: HashMap<i64, oneshot::Sender<Reply>>,
}

#[derive(Debug)]
enum Outbound {
    Message(Bytes, OwnedSemaphorePermit),
    /// Close the server's stdin.
    Close,
}

#[derive(Serialize)]
struct RequestOut<'a, P> {
    jsonrpc: &'static str,
    id: i64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a P>,
}

#[derive(Serialize)]
struct NotificationOut<'a, P> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a P>,
}

#[derive(Serialize)]
struct ResultOut<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: Value,
}

#[derive(Serialize)]
struct ErrorOut<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    error: ErrorBody,
}

#[derive(Serialize, Deserialize)]
struct ErrorBody {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct Inbound<'a> {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default, borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    params: Option<&'a RawValue>,
    #[serde(default, borrow)]
    result: Option<&'a RawValue>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

/// Queue occupancy, for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueStats {
    /// Messages queued for the server (≤ [`OUTGOING_QUEUE`]).
    pub outgoing: usize,
    /// Bytes queued for the server (≤ [`OUTGOING_BYTES`]).
    pub outgoing_bytes: usize,
    /// Requests waiting for an answer (≤ [`MAX_PENDING`]).
    pub pending: usize,
}

/// The sending half of one server process's connection.
#[derive(Debug)]
pub(crate) struct Connection {
    outgoing: mpsc::Sender<Outbound>,
    budget: Arc<Semaphore>,
    pending: Arc<Mutex<Pending>>,
    next_id: AtomicI64,
    stderr: Arc<Mutex<VecDeque<String>>>,
    max_frame: usize,
    closed: watch::Receiver<bool>,
}

/// Removes an unanswered request from the pending map and tells the server to stop working on
/// it, whether the caller timed out or dropped the request future.
struct CallGuard<'a> {
    connection: &'a Connection,
    id: i64,
    answered: bool,
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        let waiting = lock(&self.connection.pending)
            .calls
            .remove(&self.id)
            .is_some();
        if waiting {
            self.connection
                .try_notify("$/cancelRequest", &serde_json::json!({ "id": self.id }));
        }
    }
}

impl Connection {
    /// Starts the writer and reader tasks over the process's stdio.
    pub fn start(
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: Arc<Mutex<VecDeque<String>>>,
        codec: LspCodec,
        handler: Arc<dyn Handler>,
        generation: u64,
    ) -> Arc<Self> {
        let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_QUEUE);
        let (closed_tx, closed) = watch::channel(false);
        let max_frame = codec.max_frame();
        let connection = Arc::new(Self {
            outgoing,
            budget: Arc::new(Semaphore::new(OUTGOING_BYTES)),
            pending: Arc::default(),
            next_id: AtomicI64::new(1),
            stderr,
            max_frame,
            closed,
        });
        tokio::spawn(write_loop(
            FramedWrite::new(stdin, codec.clone()),
            outgoing_rx,
        ));
        tokio::spawn(read_loop(
            FramedRead::new(stdout, codec),
            Arc::downgrade(&connection),
            connection.pending.clone(),
            handler,
            generation,
            closed_tx,
        ));
        connection
    }

    pub fn stats(&self) -> QueueStats {
        QueueStats {
            outgoing: OUTGOING_QUEUE - self.outgoing.capacity(),
            outgoing_bytes: OUTGOING_BYTES - self.budget.available_permits(),
            pending: lock(&self.pending).calls.len(),
        }
    }

    /// Resolves once the server's output ended.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        // An error means the reader is gone, which is closed as well.
        let _ = closed.wait_for(|closed| *closed).await;
    }

    pub fn stderr_tail(&self) -> String {
        stderr_tail(&self.stderr)
    }

    fn closed_error(&self) -> LspError {
        LspError::Closed {
            detail: self.stderr_tail(),
        }
    }

    fn encode<T: Serialize>(&self, method: &str, message: &T) -> Result<Vec<u8>, LspError> {
        let body = serde_json::to_vec(message).map_err(|source| LspError::Malformed {
            method: method.to_owned(),
            source,
        })?;
        if body.len() > self.max_frame {
            return Err(LspError::TooLarge {
                method: method.to_owned(),
                len: body.len(),
                max: self.max_frame,
            });
        }
        Ok(body)
    }

    fn permits(len: usize) -> u32 {
        // OUTGOING_BYTES fits in u32; a larger message takes the whole budget.
        u32::try_from(len.clamp(1, OUTGOING_BYTES)).unwrap_or(u32::MAX)
    }

    async fn send(&self, body: Vec<u8>) -> Result<(), LspError> {
        let permit = self
            .budget
            .clone()
            .acquire_many_owned(Self::permits(body.len()))
            .await
            .map_err(|_| self.closed_error())?;
        self.outgoing
            .send(Outbound::Message(Bytes::from(body), permit))
            .await
            .map_err(|_| self.closed_error())
    }

    /// Queues a message if there is room right now; `false` when it was dropped.
    fn try_send(&self, body: Vec<u8>) -> bool {
        let Ok(permit) = self
            .budget
            .clone()
            .try_acquire_many_owned(Self::permits(body.len()))
        else {
            return false;
        };
        self.outgoing
            .try_send(Outbound::Message(Bytes::from(body), permit))
            .is_ok()
    }

    /// A notification; `None` params are left out (`exit`).
    pub async fn notify<P: Serialize>(
        &self,
        method: &str,
        params: Option<&P>,
    ) -> Result<(), LspError> {
        let body = self.encode(
            method,
            &NotificationOut {
                jsonrpc: "2.0",
                method,
                params,
            },
        )?;
        self.send(body).await
    }

    /// A notification that is dropped when the queue is full (advisory messages).
    pub fn try_notify<P: Serialize>(&self, method: &str, params: &P) -> bool {
        let Ok(body) = self.encode(
            method,
            &NotificationOut {
                jsonrpc: "2.0",
                method,
                params: Some(params),
            },
        ) else {
            return false;
        };
        let sent = self.try_send(body);
        if !sent {
            debug!(method, "outgoing queue full: advisory notification dropped");
        }
        sent
    }

    /// A request answered within `deadline` (queueing included); `None` params are left out
    /// (`shutdown`). On timeout, or when the returned future is dropped first, the request is
    /// withdrawn with `$/cancelRequest`.
    pub async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<&P>,
        deadline: Duration,
    ) -> Result<R, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = self.encode(
            method,
            &RequestOut {
                jsonrpc: "2.0",
                id,
                method,
                params,
            },
        )?;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = lock(&self.pending);
            if pending.closed {
                return Err(self.closed_error());
            }
            if pending.calls.len() >= MAX_PENDING {
                return Err(LspError::Busy(MAX_PENDING));
            }
            pending.calls.insert(id, tx);
        }
        let mut guard = CallGuard {
            connection: self,
            id,
            answered: false,
        };
        let exchange = async {
            self.send(body).await?;
            rx.await.map_err(|_| self.closed_error())
        };
        let reply = match timeout(deadline, exchange).await {
            Err(_) => {
                return Err(LspError::Timeout {
                    method: method.to_owned(),
                });
            }
            Ok(Err(error)) => return Err(error),
            Ok(Ok(reply)) => reply,
        };
        guard.answered = true;
        match reply {
            Ok(raw) => match raw {
                Some(raw) => serde_json::from_str(raw.get()),
                None => serde_json::from_value(Value::Null),
            }
            .map_err(|source| LspError::Malformed {
                method: method.to_owned(),
                source,
            }),
            Err(Failure::Remote { code, message }) => Err(LspError::Rpc {
                method: method.to_owned(),
                code,
                message,
            }),
            Err(Failure::Closed) => Err(self.closed_error()),
        }
    }

    /// Closes the server's stdin once everything queued before is written.
    pub async fn close_input(&self) {
        // Fails only when the writer is gone, which closed stdin already.
        let _ = self.outgoing.send(Outbound::Close).await;
    }

    async fn answer(&self, id: &Value, outcome: Result<Value, (i64, String)>) {
        let body = match outcome {
            Ok(result) => self.encode(
                "response",
                &ResultOut {
                    jsonrpc: "2.0",
                    id,
                    result,
                },
            ),
            Err((code, message)) => self.encode(
                "response",
                &ErrorOut {
                    jsonrpc: "2.0",
                    id,
                    error: ErrorBody { code, message },
                },
            ),
        };
        let sent = match body {
            Ok(body) => self.send(body).await,
            Err(error) => Err(error),
        };
        if let Err(error) = sent {
            debug!(%error, "could not answer a server request");
        }
    }
}

async fn write_loop(
    mut sink: FramedWrite<ChildStdin, LspCodec>,
    mut outgoing: mpsc::Receiver<Outbound>,
) {
    while let Some(message) = outgoing.recv().await {
        match message {
            Outbound::Message(body, permit) => {
                let written = sink.send(body).await;
                // The bytes left the queue (written or lost): give their budget back.
                drop(permit);
                if let Err(error) = written {
                    debug!(%error, "language server stdin closed");
                    return;
                }
            }
            Outbound::Close => return,
        }
    }
}

async fn read_loop(
    mut frames: FramedRead<ChildStdout, LspCodec>,
    connection: Weak<Connection>,
    pending: Arc<Mutex<Pending>>,
    handler: Arc<dyn Handler>,
    generation: u64,
    closed: watch::Sender<bool>,
) {
    while let Some(frame) = frames.next().await {
        let body = match frame {
            Ok(Frame::Message(body)) => body,
            Ok(Frame::Oversized { len }) => {
                warn!(len, "language server message over the frame limit skipped");
                continue;
            }
            Err(error) => {
                // The stream cannot be resynchronized after a framing error.
                warn!(%error, "language server output is not LSP framing");
                break;
            }
        };
        let message = match serde_json::from_slice::<Inbound<'_>>(&body) {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "language server sent a message that is not JSON-RPC");
                continue;
            }
        };
        match (message.method, message.id) {
            (Some(method), Some(id)) => {
                let outcome = handler.request(generation, &method, message.params);
                match connection.upgrade() {
                    Some(connection) => connection.answer(&id, outcome).await,
                    None => break,
                }
            }
            (Some(method), None) => handler.notification(generation, &method, message.params),
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else {
                    debug!("response with an id the client never sent");
                    continue;
                };
                let Some(waiter) = lock(&pending).calls.remove(&id) else {
                    debug!(id, "response to a request nobody waits for");
                    continue;
                };
                let reply = match message.error {
                    Some(error) => Err(Failure::Remote {
                        code: error.code,
                        message: error.message,
                    }),
                    None => Ok(message.result.map(RawValue::to_owned)),
                };
                let _ = waiter.send(reply);
            }
            (None, None) => debug!("message without method or id"),
        }
    }
    // Requests still waiting will never be answered.
    let waiting: Vec<_> = {
        let mut pending = lock(&pending);
        pending.closed = true;
        pending.calls.drain().map(|(_, waiter)| waiter).collect()
    };
    for waiter in waiting {
        let _ = waiter.send(Err(Failure::Closed));
    }
    let _ = closed.send(true);
}
