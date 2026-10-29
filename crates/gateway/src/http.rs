//! The HTTP plumbing shared by the adapters: one client with timeouts, connect-only retry,
//! status → [`GatewayError`] mapping, and the loop that turns a response body into a
//! [`GatewayStream`].
//!
//! Nothing here logs headers or bodies: a request is one `debug!` line (method, URL, model-free)
//! and the status of its answer.

use std::collections::VecDeque;
use std::sync::Once;
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::error::GatewayError;
use crate::redact::Redactor;
use crate::secret::Secret;
use crate::types::{GatewayEvent, GatewayStream};

/// How much of an error reply is read to find the provider's message.
const ERROR_BODY_LIMIT: usize = 16 * 1024;
/// A provider message longer than this is cut.
const MESSAGE_LIMIT: usize = 400;

/// Timeouts and retry policy.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Per connection attempt.
    pub connect_timeout: Duration,
    /// Longest silence while waiting for the reply or between two chunks of a stream. A model
    /// that thinks before its first token needs this to be generous.
    pub read_timeout: Duration,
    /// Whole-request limit of the non-streaming calls (model lists).
    pub request_timeout: Duration,
    /// Extra connection attempts after a connect error, before any byte was sent. Nothing else
    /// is ever retried: a request the provider may have seen is not repeated.
    pub connect_retries: u32,
    /// Wait before retry `n` is `retry_backoff * n`.
    pub retry_backoff: Duration,
    /// Ignore `HTTP_PROXY` and friends (local probes and tests).
    pub no_proxy: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(30),
            connect_retries: 2,
            retry_backoff: Duration::from_millis(250),
            no_proxy: false,
        }
    }
}

/// The HTTP client of the gateway; cheap to clone.
#[derive(Debug, Clone)]
pub struct Http {
    client: Client,
    config: HttpConfig,
}

static INSTALL_CRYPTO: Once = Once::new();

impl Http {
    pub fn new(config: HttpConfig) -> Result<Self, GatewayError> {
        // reqwest is built without a TLS provider (see the workspace manifest); this is the
        // process-wide one. Another component having installed one first is fine.
        INSTALL_CRYPTO.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
        let mut builder = Client::builder()
            .user_agent(concat!("hutan/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            // A redirect could carry a vendor header (`x-api-key`) to another host.
            .redirect(reqwest::redirect::Policy::none());
        if config.no_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|e| GatewayError::Client(e.to_string()))?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &HttpConfig {
        &self.config
    }

    /// One `GET url` that is not a model provider's: the daemon's update check. Sends only the
    /// given `User-Agent` and `Accept`, never follows a redirect, never retries, and reads at
    /// most `max_bytes` of the body (a longer one is a [`GatewayError::Protocol`]). Any status is
    /// an answer: the caller decides what a 403 or a 404 means.
    pub async fn get_bounded(
        &self,
        url: &str,
        user_agent: &str,
        accept: &str,
        max_bytes: usize,
    ) -> Result<Fetched, GatewayError> {
        let url = Url::parse(url).map_err(|e| GatewayError::InvalidUrl(e.to_string()))?;
        debug!(host = url.host_str().unwrap_or(""), "bounded GET");
        let mut response = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .header(reqwest::header::ACCEPT, accept)
            .timeout(self.config.request_timeout)
            .send()
            .await
            .map_err(|error| fetch_error(&error))?;
        let status = response.status().as_u16();
        let rate_limit_exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0");
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| fetch_error(&error))?
        {
            if body.len() + chunk.len() > max_bytes {
                return Err(GatewayError::Protocol(format!(
                    "the reply is longer than {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Fetched {
            status,
            rate_limit_exhausted,
            body,
        })
    }
}

/// The answer of [`Http::get_bounded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    /// The server said no requests are left (`x-ratelimit-remaining: 0`).
    pub rate_limit_exhausted: bool,
    pub body: Vec<u8>,
}

fn fetch_error(error: &reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::Timeout
    } else if error.is_connect() {
        GatewayError::Connect {
            attempts: 1,
            message: describe(error),
        }
    } else {
        GatewayError::Stream {
            message: describe(error),
        }
    }
}

/// Where a provider lives and how to reach it: shared by the adapters.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    http: Http,
    base_url: String,
    key: Option<Secret>,
    redactor: Redactor,
}

impl Endpoint {
    pub(crate) fn new(http: Http, base_url: &str, key: Option<Secret>) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            redactor: Redactor::new(key.clone()),
            key,
        }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    pub(crate) fn key(&self) -> Option<&Secret> {
        self.key.as_ref()
    }

    pub(crate) fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    pub(crate) fn config(&self) -> &HttpConfig {
        &self.http.config
    }

    pub(crate) fn get(&self, path: &str) -> RequestBuilder {
        self.http.client.get(self.url(path))
    }

    pub(crate) fn post(&self, path: &str) -> RequestBuilder {
        self.http.client.post(self.url(path))
    }

    /// Adds the credential as header `name` (with `prefix` before it). The value is marked
    /// sensitive, so it is left out of every debug print of the request.
    pub(crate) fn with_key(
        &self,
        request: RequestBuilder,
        name: &'static str,
        prefix: &str,
    ) -> Result<RequestBuilder, GatewayError> {
        let Some(key) = &self.key else {
            return Ok(request);
        };
        let mut value =
            HeaderValue::from_str(&format!("{prefix}{}", key.expose())).map_err(|_| {
                GatewayError::Protocol("the API key is not a valid header value".into())
            })?;
        value.set_sensitive(true);
        Ok(request.header(HeaderName::from_static(name), value))
    }

    /// Sends the request `build` makes; a connect error is retried (the request was never
    /// sent), nothing else. A non-2xx answer becomes the matching error.
    pub(crate) async fn send(
        &self,
        build: impl Fn() -> Result<RequestBuilder, GatewayError>,
    ) -> Result<Response, GatewayError> {
        let config = self.config();
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let request = build()?;
            debug!(attempt, "gateway request");
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    debug!(status = status.as_u16(), "gateway response");
                    if status.is_success() {
                        return Ok(response);
                    }
                    return Err(self.status_error(response).await);
                }
                Err(error) if error.is_connect() => {
                    if attempt > config.connect_retries {
                        return Err(GatewayError::Connect {
                            attempts: attempt,
                            message: self.redactor.apply(&describe(&error)).into_owned(),
                        });
                    }
                    tokio::time::sleep(config.retry_backoff * attempt).await;
                }
                Err(error) if error.is_timeout() => return Err(GatewayError::Timeout),
                Err(error) => {
                    return Err(GatewayError::Stream {
                        message: self.redactor.apply(&describe(&error)).into_owned(),
                    });
                }
            }
        }
    }

    /// GET `path` with the given extra headers, as JSON.
    pub(crate) async fn get_json(
        &self,
        path: &str,
        auth: impl Fn(RequestBuilder) -> Result<RequestBuilder, GatewayError>,
    ) -> Result<Value, GatewayError> {
        let timeout = self.config().request_timeout;
        let response = self
            .send(|| Ok(auth(self.get(path))?.timeout(timeout)))
            .await?;
        let bytes = response.bytes().await.map_err(|e| {
            if e.is_timeout() {
                GatewayError::Timeout
            } else {
                GatewayError::Stream {
                    message: self.redactor.apply(&describe(&e)).into_owned(),
                }
            }
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|e| GatewayError::Protocol(format!("the reply is not JSON: {e}")))
    }

    async fn status_error(&self, response: Response) -> GatewayError {
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let body = read_limited(response).await;
        let message = self.message_of(&body, status);
        map_status(status, retry_after, message)
    }

    /// The provider's own words for what went wrong, scrubbed and cut.
    pub(crate) fn message_of(&self, body: &str, status: StatusCode) -> String {
        let text = error_message(body).unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                status.canonical_reason().unwrap_or("no details").to_owned()
            } else {
                trimmed.to_owned()
            }
        });
        let text = self.redactor.apply(&text);
        truncate(&text, MESSAGE_LIMIT)
    }
}

/// `reqwest`'s message with the URL it carries left out: the URL is already known to the user
/// and a query string must never end up in an event.
fn describe(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "the provider");
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

async fn read_limited(response: Response) -> String {
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(Ok(chunk)) = chunks.next().await {
        body.extend_from_slice(&chunk);
        if body.len() >= ERROR_BODY_LIMIT {
            break;
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// `error.message`, `error` (a string), or `message` of a JSON error body.
fn error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let from = |v: &Value| v.as_str().map(str::to_owned);
    value
        .pointer("/error/message")
        .and_then(from)
        .or_else(|| value.get("error").and_then(from))
        .or_else(|| value.get("message").and_then(from))
        .filter(|m| !m.trim().is_empty())
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: f64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(86_400.0)))
}

/// The error for a non-success status.
pub(crate) fn map_status(
    status: StatusCode,
    retry_after: Option<Duration>,
    message: String,
) -> GatewayError {
    let code = status.as_u16();
    match code {
        401 | 403 => GatewayError::Unauthorized {
            status: code,
            message,
        },
        404 => GatewayError::NotFound { message },
        429 => GatewayError::RateLimited {
            retry_after,
            message,
        },
        500..=599 => GatewayError::Server {
            status: code,
            message,
        },
        _ => GatewayError::Rejected {
            status: code,
            message,
        },
    }
}

/// Turns the body of one provider's streamed reply into events.
pub(crate) trait Decoder: Send + 'static {
    /// More body bytes arrived.
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<GatewayEvent>, GatewayError>;
    /// The body ended: flush what is left. A reply that never completed is an error.
    fn finish(&mut self) -> Result<Vec<GatewayEvent>, GatewayError>;
}

struct Driver<D> {
    body: BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    decoder: D,
    redactor: Redactor,
    pending: VecDeque<GatewayEvent>,
    cancel: CancellationToken,
    /// The body is over (or the stream failed): drain `pending`, then end.
    done: bool,
}

/// Streams the events of `response`; ends after `Completed`, an error, or cancellation (the
/// body is dropped then, which closes the connection).
fn drive<D: Decoder>(
    response: Response,
    decoder: D,
    redactor: Redactor,
    cancel: CancellationToken,
) -> GatewayStream {
    let driver = Driver {
        body: response.bytes_stream().boxed(),
        decoder,
        redactor,
        pending: VecDeque::new(),
        cancel,
        done: false,
    };
    stream::unfold(driver, |mut d| async move {
        loop {
            if !d.done && d.cancel.is_cancelled() {
                d.done = true;
                d.pending.clear();
                return Some((Err(GatewayError::Cancelled), d));
            }
            if let Some(event) = d.pending.pop_front() {
                if matches!(event, GatewayEvent::Completed { .. }) {
                    d.pending.clear();
                    d.done = true;
                }
                return Some((Ok(event), d));
            }
            if d.done {
                return None;
            }
            let chunk = tokio::select! {
                biased;
                () = d.cancel.cancelled() => continue,
                chunk = d.body.next() => chunk,
            };
            let step = match chunk {
                Some(Ok(bytes)) => d.decoder.feed(&bytes),
                Some(Err(error)) if error.is_timeout() => Err(GatewayError::Timeout),
                Some(Err(error)) => Err(GatewayError::Stream {
                    message: d.redactor.apply(&describe(&error)).into_owned(),
                }),
                None => {
                    d.done = true;
                    d.decoder.finish()
                }
            };
            match step {
                Ok(events) => d.pending.extend(events),
                Err(error) => {
                    d.done = true;
                    d.pending.clear();
                    return Some((Err(error), d));
                }
            }
        }
    })
    .boxed()
}

/// A stream that first opens the connection (`open`), then follows the body with `decoder`.
/// Cancelling before the answer arrives ends the stream with [`GatewayError::Cancelled`].
pub(crate) fn stream_response<D: Decoder>(
    open: BoxFuture<'static, Result<Response, GatewayError>>,
    decoder: D,
    redactor: Redactor,
    cancel: CancellationToken,
) -> GatewayStream {
    stream::once(async move {
        let opened = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(GatewayError::Cancelled),
            opened = open => opened,
        };
        match opened {
            Ok(response) => drive(response, decoder, redactor, cancel),
            Err(error) => stream::once(futures::future::ready(Err(error))).boxed(),
        }
    })
    .flatten()
    .boxed()
}

/// Parses a base URL: http(s) only, no credentials in it, a trailing slash removed.
pub fn validate_base_url(text: &str) -> Result<String, GatewayError> {
    let text = text.trim();
    let url = Url::parse(text).map_err(|e| GatewayError::InvalidUrl(format!("`{text}`: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(GatewayError::InvalidUrl(format!(
            "`{text}` must be an http:// or https:// URL"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GatewayError::InvalidUrl(
            "a base URL must not contain a user name or password; use `hutan provider key`".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(GatewayError::InvalidUrl(
            "a base URL must not contain a query or a fragment".into(),
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
