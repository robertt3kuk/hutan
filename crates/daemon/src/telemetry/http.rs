//! The receiver's HTTP side: one request at a time per connection (`hyper`, http1), every check
//! done before the body is read, and an answer that never waits for the mapper (ADR 0026
//! decisions 3, 5 and 6).
//!
//! Order of the checks, each refusing with its own status and an empty body: `POST` (405),
//! `Host` is `127.0.0.1` or `localhost` on this port (421: DNS rebinding), a content type of OTLP
//! (415: what a web page can send without a preflight is none of them), the token (401, the same
//! answer for a missing, a malformed and a wrong one), the signal in the path (404), the
//! declared size (413), the session's rates and concurrency (429). Then the body is read under a
//! limit and a timeout, handed to the mapper's bounded queue and answered `200` with the
//! signal's empty `Export*ServiceResponse`. A full queue is `429` with `Retry-After`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt as _, Full, Limited};
use hutan_core::SessionId;
use hutan_telemetry::{Encoding, MAX_BODY_BYTES, Signal};
use hyper::body::{Bytes, Incoming};
use hyper::header::{
    AUTHORIZATION, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HOST, RETRY_AFTER,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use super::{Job, Rate, Shared};
use crate::registry::now_ms;

/// Header block of a request.
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// How long the body of a request may take to arrive once its head has.
const BODY_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a connection may wait for a request head (the keep-alive idle time, and the limit on
/// a head that trickles in).
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Connections at once.
pub(super) const MAX_CONNECTIONS: usize = 16;
/// Requests of one session in flight at once.
pub(super) const MAX_PER_SESSION: u8 = 4;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves the state half updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A token bucket: `rate.burst` at most, refilled `rate.per_sec` a second.
#[derive(Debug)]
struct Bucket {
    rate: Rate,
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: Rate, now: Instant) -> Self {
        Self {
            rate,
            tokens: rate.burst,
            at: now,
        }
    }

    fn take(&mut self, amount: f64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + elapsed * self.rate.per_sec).min(self.rate.burst);
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }
}

/// What one session has used of its limits.
#[derive(Debug)]
pub(crate) struct SessionLimits {
    requests: Bucket,
    bytes: Bucket,
    in_flight: u8,
}

/// A request in flight of a session; leaving it frees the slot.
struct Slot<'a> {
    shared: &'a Shared,
    session: SessionId,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Some(limits) = lock(&self.shared.limits).get_mut(&self.session) {
            limits.in_flight = limits.in_flight.saturating_sub(1);
        }
    }
}

/// An empty answer.
fn status(code: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = code;
    response
}

/// An empty refusal of a request whose body was not read: the connection ends with it.
fn refuse(code: StatusCode) -> Response<Full<Bytes>> {
    let mut response = status(code);
    response
        .headers_mut()
        .insert(CONNECTION, hyper::header::HeaderValue::from_static("close"));
    response
}

/// `429`: try again in a second.
fn too_many() -> Response<Full<Bytes>> {
    let mut response = refuse(StatusCode::TOO_MANY_REQUESTS);
    response
        .headers_mut()
        .insert(RETRY_AFTER, hyper::header::HeaderValue::from_static("1"));
    response
}

/// Whether a `Host` header names this listener: `127.0.0.1` or `localhost`, on its port.
fn host_ok(value: &str, port: u16) -> bool {
    let (name, given) = match value.rsplit_once(':') {
        Some((name, given)) => (name, Some(given)),
        None => (value, None),
    };
    let name_ok = name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost");
    let port_ok = given.is_none_or(|given| given.parse() == Ok(port));
    name_ok && port_ok
}

/// What the path and headers say: the token presented and the signal, or neither.
struct Target<'a> {
    token: Option<&'a str>,
    tail: &'a str,
}

/// `/s/<token>/v1/<signal>` carries the token in the path; a bare `/v1/<signal>` takes it from
/// `x-hutan-token` or `Authorization: Bearer`.
fn target<'a>(path: &'a str, request: &'a Request<Incoming>) -> Target<'a> {
    let path = path.trim_start_matches('/');
    if let Some(rest) = path.strip_prefix("s/")
        && let Some((token, tail)) = rest.split_once('/')
    {
        return Target {
            token: Some(token),
            tail,
        };
    }
    let header = |name: &str| request.headers().get(name)?.to_str().ok();
    let token = header("x-hutan-token").or_else(|| {
        let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
        value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
    });
    Target {
        token: token.map(str::trim),
        tail: path,
    }
}

/// Answers one request.
async fn respond(
    shared: &Arc<Shared>,
    port: u16,
    request: Request<Incoming>,
) -> Response<Full<Bytes>> {
    let counters = &shared.counters;
    let rejected = |response: Response<Full<Bytes>>| {
        counters.rejected.fetch_add(1, Ordering::Relaxed);
        response
    };
    if request.method() != Method::POST {
        return rejected(refuse(StatusCode::METHOD_NOT_ALLOWED));
    }
    let host = request.headers().get(HOST).and_then(|h| h.to_str().ok());
    if !host.is_some_and(|host| host_ok(host, port)) {
        return rejected(refuse(StatusCode::MISDIRECTED_REQUEST));
    }
    let encoding = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(Encoding::from_content_type);
    let Some(encoding) = encoding else {
        return rejected(refuse(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    };
    let gzip = match request
        .headers()
        .get(CONTENT_ENCODING)
        .map(|value| value.to_str().map(|v| v.trim().to_ascii_lowercase()))
    {
        None => false,
        Some(Ok(value)) if value == "identity" => false,
        Some(Ok(value)) if value == "gzip" => true,
        Some(_) => return rejected(refuse(StatusCode::UNSUPPORTED_MEDIA_TYPE)),
    };

    let path = request.uri().path().to_owned();
    let target = target(&path, &request);
    let session = target.token.and_then(|token| shared.env.resolve(token));
    let Some(session) = session else {
        // The same answer for a missing, a malformed and a wrong token.
        return rejected(refuse(StatusCode::UNAUTHORIZED));
    };
    let Some(signal) = Signal::from_path_tail(target.tail) else {
        return rejected(refuse(StatusCode::NOT_FOUND));
    };

    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return rejected(refuse(StatusCode::PAYLOAD_TOO_LARGE));
    }

    // The session's rates and how many of its requests are in flight.
    let _slot = {
        let now = Instant::now();
        let mut limits = lock(&shared.limits);
        let entry = limits.entry(session).or_insert_with(|| SessionLimits {
            requests: Bucket::new(shared.tuning.requests, now),
            bytes: Bucket::new(shared.tuning.bytes, now),
            in_flight: 0,
        });
        if entry.in_flight >= MAX_PER_SESSION || !entry.requests.take(1.0, now) {
            drop(limits);
            return rejected(too_many());
        }
        // A declared size is charged now; a chunked body, once it has been read.
        if let Some(len) = declared
            && !entry.bytes.take(len as f64, now)
        {
            return rejected(too_many());
        }
        entry.in_flight += 1;
        Slot { shared, session }
    };

    let collected = tokio::time::timeout(
        BODY_TIMEOUT,
        Limited::new(request.into_body(), MAX_BODY_BYTES).collect(),
    )
    .await;
    let body = match collected {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(error)) if error.is::<http_body_util::LengthLimitError>() => {
            return rejected(refuse(StatusCode::PAYLOAD_TOO_LARGE));
        }
        Ok(Err(_)) => return rejected(refuse(StatusCode::BAD_REQUEST)),
        Err(_) => return rejected(refuse(StatusCode::REQUEST_TIMEOUT)),
    };
    if declared.is_none() {
        let mut limits = lock(&shared.limits);
        if let Some(entry) = limits.get_mut(&session)
            && !entry.bytes.take(body.len() as f64, Instant::now())
        {
            drop(limits);
            return rejected(too_many());
        }
    }

    let job = Job {
        session,
        signal,
        encoding,
        gzip,
        body,
        at_ms: now_ms(),
    };
    match shared.queue.try_send(job) {
        Ok(()) => {
            counters.accepted.fetch_add(1, Ordering::Relaxed);
            let mut response =
                Response::new(Full::new(Bytes::from_static(encoding.empty_response())));
            response.headers_mut().insert(
                CONTENT_TYPE,
                hyper::header::HeaderValue::from_static(encoding.content_type()),
            );
            response
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            too_many()
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            status(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

/// Accepts connections until `cancel` fires, at most [`MAX_CONNECTIONS`] at once.
pub(super) async fn serve(
    listener: TcpListener,
    port: u16,
    shared: Arc<Shared>,
    cancel: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::debug!(%error, "OTLP accept failed");
                // A descriptor shortage or a reset: do not spin on it.
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            shared.counters.rejected.fetch_add(1, Ordering::Relaxed);
            drop(stream);
            continue;
        };
        let (shared, cancel) = (shared.clone(), cancel.clone());
        tokio::spawn(async move {
            connection(stream, port, shared, cancel).await;
            drop(permit);
        });
    }
}

async fn connection(stream: TcpStream, port: u16, shared: Arc<Shared>, cancel: CancellationToken) {
    let service = service_fn(move |request: Request<Incoming>| {
        let shared = shared.clone();
        async move { Ok::<_, Infallible>(respond(&shared, port, request).await) }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIMEOUT)
        .max_buf_size(MAX_HEADER_BYTES)
        .keep_alive(true)
        .serve_connection(TokioIo::new(stream), service);
    tokio::select! {
        () = cancel.cancelled() => {}
        result = connection => {
            if let Err(error) = result {
                tracing::debug!(%error, "OTLP connection ended");
            }
        }
    }
}

/// The per-session maps this module owns, created empty.
pub(super) fn new_limits() -> Mutex<HashMap<SessionId, SessionLimits>> {
    Mutex::new(HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_names_on_this_port_are_a_host() {
        assert!(host_ok("127.0.0.1:51234", 51234));
        assert!(host_ok("localhost:51234", 51234));
        assert!(host_ok("LOCALHOST:51234", 51234));
        assert!(host_ok("127.0.0.1", 51234));
        assert!(!host_ok("127.0.0.1:1", 51234));
        assert!(!host_ok("evil.example:51234", 51234));
        assert!(!host_ok("evil.example", 51234));
        assert!(!host_ok("127.0.0.1.evil.example:51234", 51234));
        assert!(!host_ok("[::1]:51234", 51234));
        assert!(!host_ok("0.0.0.0:51234", 51234));
        assert!(!host_ok("", 51234));
    }

    #[test]
    fn a_bucket_refills_by_the_second_up_to_its_burst() {
        let t0 = Instant::now();
        let mut bucket = Bucket::new(
            Rate {
                burst: 2.0,
                per_sec: 1.0,
            },
            t0,
        );
        assert!(bucket.take(1.0, t0));
        assert!(bucket.take(1.0, t0));
        assert!(!bucket.take(1.0, t0));
        assert!(bucket.take(1.0, t0 + Duration::from_secs(1)));
        assert!(!bucket.take(1.0, t0 + Duration::from_secs(1)));
        // Never more than the burst, however long it rested.
        let later = t0 + Duration::from_secs(100);
        assert!(bucket.take(2.0, later));
        assert!(!bucket.take(1.0, later));
        // A bucket that never refills (the tests' tuning) stays empty.
        let mut dead = Bucket::new(
            Rate {
                burst: 1.0,
                per_sec: 0.0,
            },
            t0,
        );
        assert!(dead.take(1.0, t0));
        assert!(!dead.take(1.0, later));
    }
}
