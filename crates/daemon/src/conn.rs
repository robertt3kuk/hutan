//! Per-client connection task: peer check, handshake, request loop, bounded outbound queue.
//!
//! Each connection is a reader (this task) plus a writer task fed by a bounded queue. Nothing
//! ever awaits a client: producers use `try_send`, and a full queue closes that connection
//! (`slow_consumer`) without affecting the daemon or other clients.

use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use hutan_core::SessionId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientInfo, ClientKind, Hello, Request, Welcome};
use hutan_protocol::{
    ControlMessage, Frame, FrameCodec, FrameKind, MAX_PAYLOAD, PROTOCOL_VERSION, ProtocolError,
};
use tokio::io::AsyncRead;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bus::EventBus;
use crate::menu_bar::DesktopPresence;
use crate::router::Router;

/// Capabilities this daemon offers in `welcome`.
pub const CAPABILITIES: &[&str] = &[
    "terminal.render.v1",
    "terminal.colors",
    "session.restore",
    "agent.report",
    "terminal.blocks",
    "workspace.projects",
    "history.search",
    "chat.codex",
    "agent.tree",
    "chat.direct",
    "assistant",
    "notify.intents",
    "worktree",
    "chat.provenance",
    "task",
    "harness.acp",
    "accounts",
    "resources",
    "projects.auto",
    "assistant.backend",
    "control",
    "worktree.review",
    "chat.rich_input",
    "chat.questions",
    "task.review",
    "session.recovery",
    "task.chat_model",
    "daemon.settings",
    "history.sessions",
    "session.close",
    "control.grants",
];

/// How long a closing connection may take to flush already queued messages.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnId(pub u64);

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One slot of a connection's outbound queue.
#[derive(Debug)]
pub enum Outbound {
    Frame(Frame),
    /// Written back to back; used for replay so it costs one queue slot however long it is.
    Batch(Vec<Frame>),
}

/// Cheap, cloneable way to push messages to (or close) one connection.
#[derive(Debug, Clone)]
pub struct ConnHandle {
    id: ConnId,
    tx: mpsc::Sender<Outbound>,
    cancel: CancellationToken,
    client: Arc<OnceLock<ClientInfo>>,
    origin: Arc<OnceLock<SessionId>>,
}

impl ConnHandle {
    pub fn id(&self) -> ConnId {
        self.id
    }

    /// What kind of client this is, once it has said hello.
    pub fn client_kind(&self) -> Option<ClientKind> {
        self.client.get().map(|client| client.kind)
    }

    /// The Hutan session this client runs in, once `hello` named one that is live: the
    /// connection is an agent's (`[control]`, protocol 0.21).
    pub fn origin(&self) -> Option<SessionId> {
        self.origin.get().copied()
    }

    /// Queues without waiting. Returns `false` if the message was not queued: the connection is
    /// gone, or its queue was full, in which case the connection is closed as a slow consumer.
    pub fn enqueue(&self, out: Outbound) -> bool {
        match self.tx.try_send(out) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                warn!(
                    conn = %self.id,
                    client = ?self.client.get(),
                    queue_capacity = self.tx.max_capacity(),
                    "slow_consumer: outbound queue full, closing connection"
                );
                self.cancel.cancel();
                false
            }
            Err(TrySendError::Closed(_)) => false,
        }
    }

    /// Queues a frame of a periodic stream (`resources.updated`) without waiting; when the
    /// queue is full the frame is dropped and the connection stays: the next sample replaces
    /// it. Returns `false` when the connection is gone.
    pub fn enqueue_lossy(&self, frame: Frame) -> bool {
        match self.tx.try_send(Outbound::Frame(frame)) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Closed(_)) => false,
        }
    }

    pub fn send_control(&self, msg: &ControlMessage) -> bool {
        match msg.to_frame() {
            Ok(frame) => self.enqueue(Outbound::Frame(frame)),
            Err(e) => {
                warn!(conn = %self.id, error = %e, "failed to encode control message");
                false
            }
        }
    }

    fn reply_error(&self, id: u64, error: ErrorBody) {
        self.send_control(&ControlMessage::err(id, error));
    }

    /// Queues a response; one that cannot fit in a frame becomes an `internal` error instead of
    /// a write failure that would take the connection down.
    fn send_response(&self, id: u64, outcome: Result<serde_json::Value, ErrorBody>) {
        let frame = ControlMessage::Response { id, outcome }
            .to_frame()
            .ok()
            .filter(|frame| frame.payload.len() <= MAX_PAYLOAD);
        match frame {
            Some(frame) => {
                self.enqueue(Outbound::Frame(frame));
            }
            None => self.reply_error(
                id,
                ErrorBody::new(
                    ErrorCode::Internal,
                    "response does not fit in a protocol frame; ask for less data",
                ),
            ),
        }
    }
}

/// State shared by all connections of one daemon.
#[derive(Debug)]
pub struct ConnContext {
    pub router: Router,
    pub bus: Arc<EventBus>,
    pub shutdown: CancellationToken,
    pub clients: Arc<AtomicU32>,
    pub outbound_queue: usize,
    pub daemon_uid: u32,
    /// The `desktop` clients attached now (the daemon's menu bar item hides while there is one).
    pub desktops: DesktopPresence,
    /// How long a new connection may stay silent before `hello` arrives.
    pub handshake_timeout: Duration,
    next_conn_id: AtomicU64,
}

impl ConnContext {
    pub fn new(
        router: Router,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        clients: Arc<AtomicU32>,
        outbound_queue: usize,
        daemon_uid: u32,
        handshake_timeout: Duration,
    ) -> Self {
        Self {
            router,
            bus,
            shutdown,
            clients,
            outbound_queue,
            daemon_uid,
            desktops: DesktopPresence::default(),
            handshake_timeout,
            next_conn_id: AtomicU64::new(1),
        }
    }
}

/// Counts handshaken clients for `daemon.status`.
struct ClientGuard(Arc<AtomicU32>);

impl ClientGuard {
    fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

type Reader = FramedRead<tokio::net::unix::OwnedReadHalf, FrameCodec>;

/// Serves one accepted connection until it closes.
pub async fn serve(stream: UnixStream, ctx: Arc<ConnContext>) {
    let id = ConnId(ctx.next_conn_id.fetch_add(1, Ordering::Relaxed));
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == ctx.daemon_uid => {}
        Ok(cred) => {
            warn!(conn = %id, peer_uid = cred.uid(), "rejecting connection from a different user");
            return;
        }
        Err(e) => {
            warn!(conn = %id, error = %e, "rejecting connection: peer credentials unavailable");
            return;
        }
    }

    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(read_half, FrameCodec);
    let (tx, rx) = mpsc::channel(ctx.outbound_queue);
    let cancel = CancellationToken::new();
    let handle = ConnHandle {
        id,
        tx,
        cancel: cancel.clone(),
        client: Arc::new(OnceLock::new()),
        origin: Arc::new(OnceLock::new()),
    };
    let mut writer = tokio::spawn(write_loop(
        FramedWrite::new(write_half, FrameCodec),
        rx,
        cancel,
        id,
    ));

    run(&mut reader, &handle, &ctx).await;

    ctx.bus.unsubscribe(id);
    // Render tasks hold connection handles; the writer below can only finish once they end.
    ctx.router.connection_closed(id);
    // Dropping the last sender lets the writer flush what is queued and finish.
    drop(handle);
    if timeout(DRAIN_TIMEOUT, &mut writer).await.is_err() {
        writer.abort();
    }
}

async fn write_loop(
    mut sink: FramedWrite<OwnedWriteHalf, FrameCodec>,
    mut rx: mpsc::Receiver<Outbound>,
    cancel: CancellationToken,
    id: ConnId,
) {
    let drain = async {
        while let Some(first) = rx.recv().await {
            let mut next = Some(first);
            while let Some(out) = next {
                match out {
                    Outbound::Frame(frame) => sink.feed(frame).await?,
                    Outbound::Batch(frames) => {
                        for frame in frames {
                            sink.feed(frame).await?;
                        }
                    }
                }
                next = rx.try_recv().ok();
            }
            sink.flush().await?;
        }
        Ok::<(), ProtocolError>(())
    };
    tokio::select! {
        result = drain => {
            if let Err(e) = result {
                debug!(conn = %id, error = %e, "write failed, closing connection");
                cancel.cancel();
            }
        }
        _ = cancel.cancelled() => {}
    }
}

/// Next frame, or `None` once the connection must end (EOF, error, slow consumer, shutdown).
async fn next_frame<R: AsyncRead + Unpin>(
    reader: &mut FramedRead<R, FrameCodec>,
    handle: &ConnHandle,
    ctx: &ConnContext,
) -> Option<Frame> {
    let item = tokio::select! {
        biased;
        _ = handle.cancel.cancelled() => return None,
        _ = ctx.shutdown.cancelled() => return None,
        item = reader.next() => item,
    };
    match item {
        Some(Ok(frame)) => Some(frame),
        Some(Err(e)) => {
            warn!(conn = %handle.id, error = %e, "protocol violation, closing connection");
            None
        }
        None => None,
    }
}

async fn run(reader: &mut Reader, handle: &ConnHandle, ctx: &ConnContext) {
    let Some(client) = handshake(reader, handle, ctx).await else {
        return;
    };
    let _guard = ClientGuard::new(ctx.clients.clone());
    // Counted from the handshake, before it subscribes: the menu bar item hands over at once.
    let _desktop = (client.kind == ClientKind::Desktop).then(|| ctx.desktops.join());
    info!(conn = %handle.id, client = ?client, "client connected");

    while let Some(frame) = next_frame(reader, handle, ctx).await {
        if !handle_frame(frame, handle, ctx).await {
            break;
        }
    }
    info!(conn = %handle.id, client = ?client, "client disconnected");
}

/// Returns `false` if the connection must be closed.
async fn handle_frame(frame: Frame, handle: &ConnHandle, ctx: &ConnContext) -> bool {
    match frame.kind {
        FrameKind::Control => {}
        FrameKind::TerminalInput => return handle_input(&frame, handle, ctx).await,
        FrameKind::TerminalRender => {
            debug!(conn = %handle.id, "terminal render frame from a client ignored");
            return true;
        }
    }
    let msg = match ControlMessage::from_frame(&frame) {
        Ok(msg) => msg,
        Err(e) => {
            warn!(conn = %handle.id, error = %e, "malformed control frame, closing connection");
            return false;
        }
    };
    match msg {
        ControlMessage::Request { id, method, params } => {
            let outcome = match Request::decode(&method, params) {
                Ok(request) => ctx.router.dispatch(handle, request).await,
                Err(error) => Err(error),
            };
            handle.send_response(id, outcome);
        }
        ControlMessage::Response { .. } | ControlMessage::Event { .. } => {
            debug!(conn = %handle.id, "client sent a response/event envelope; ignored");
        }
    }
    true
}

/// Passes terminal input on. The reader waits here while the terminal's input queue is full,
/// which stalls only this connection; shutdown and a dying connection still interrupt it.
async fn handle_input(frame: &Frame, handle: &ConnHandle, ctx: &ConnContext) -> bool {
    let routed = tokio::select! {
        biased;
        _ = handle.cancel.cancelled() => return false,
        _ = ctx.shutdown.cancelled() => return false,
        routed = ctx.router.input(handle, frame) => routed,
    };
    match routed {
        Ok(()) => true,
        Err(e) => {
            warn!(conn = %handle.id, error = %e, "malformed terminal input frame, closing connection");
            false
        }
    }
}

/// First frame must be `hello`, and must arrive within the handshake timeout. On success the
/// welcome (and any replay) is queued and the client's info is returned.
async fn handshake(
    reader: &mut Reader,
    handle: &ConnHandle,
    ctx: &ConnContext,
) -> Option<ClientInfo> {
    let Ok(first) = timeout(ctx.handshake_timeout, next_frame(reader, handle, ctx)).await else {
        debug!(
            conn = %handle.id,
            timeout_ms = ctx.handshake_timeout.as_millis(),
            "closing connection: no hello within the handshake timeout"
        );
        return None;
    };
    let frame = first?;
    let (id, hello) = match parse_hello(&frame) {
        Ok(parsed) => parsed,
        Err((id, error)) => {
            warn!(conn = %handle.id, code = ?error.code, message = %error.message, "handshake rejected");
            if let Some(id) = id {
                handle.reply_error(id, error);
            }
            return None;
        }
    };

    if !PROTOCOL_VERSION.is_compatible_with(hello.protocol) {
        let message = format!(
            "daemon speaks protocol {}.{}, client speaks {}.{}",
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            hello.protocol.major,
            hello.protocol.minor
        );
        warn!(conn = %handle.id, client = ?hello.client, %message, "unsupported_version");
        let mut error = ErrorBody::new(ErrorCode::UnsupportedVersion, message);
        error.data = Some(serde_json::json!({ "supported": PROTOCOL_VERSION }));
        handle.reply_error(id, error);
        return None;
    }

    // Set before `open` so slow-consumer logs can name the client.
    let _ = handle.client.set(hello.client.clone());
    // An origin is only believed while its session is live: the connection then counts as an
    // agent's (`[control]`). A claim that does not hold is ignored, and logged.
    if let Some(origin) = hello.client.origin {
        if ctx.router.origin_is_live(origin.session_id).await {
            let _ = handle.origin.set(origin.session_id);
        } else {
            warn!(
                conn = %handle.id,
                session = %origin.session_id,
                "ignoring the origin of a client: no such live session"
            );
        }
    }
    let epoch = ctx.bus.epoch();
    let opened = ctx.bus.open(handle, hello.resume, |resume| {
        ControlMessage::ok(
            id,
            &Welcome {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                epoch,
                capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
                resume,
            },
        )?
        .to_frame()
    });
    match opened {
        Ok(_) => Some(hello.client),
        Err(e) => {
            warn!(conn = %handle.id, error = %e, "failed to build welcome");
            handle.reply_error(id, ErrorBody::new(ErrorCode::Internal, "handshake failed"));
            None
        }
    }
}

fn parse_hello(frame: &Frame) -> Result<(u64, Hello), (Option<u64>, ErrorBody)> {
    if frame.kind != FrameKind::Control {
        return Err((
            None,
            ErrorBody::new(ErrorCode::BadRequest, "first frame must be a hello request"),
        ));
    }
    let msg = ControlMessage::from_frame(frame).map_err(|e| {
        (
            None,
            ErrorBody::new(ErrorCode::BadRequest, format!("malformed first frame: {e}")),
        )
    })?;
    let ControlMessage::Request { id, method, params } = msg else {
        return Err((
            None,
            ErrorBody::new(ErrorCode::BadRequest, "first frame must be a hello request"),
        ));
    };
    if method != "hello" {
        return Err((
            Some(id),
            ErrorBody::new(
                ErrorCode::BadRequest,
                format!("first request must be `hello`, got `{method}`"),
            ),
        ));
    }
    match Request::decode(&method, params) {
        Ok(Request::Hello(hello)) => Ok((id, hello)),
        Ok(_) => Err((
            Some(id),
            ErrorBody::new(ErrorCode::Internal, "hello decoded as another request"),
        )),
        Err(error) => Err((Some(id), error)),
    }
}
