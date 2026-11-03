//! OTLP decoding and the mappers from agents' own OpenTelemetry to Hutan's insight reports
//! (ADR 0026, `docs/integrations.md` § Report your agent's stats to Hutan).
//!
//! A pure service crate: no sockets, no daemon types, no clock. `hutand` owns the listener, the
//! per-session tokens, the limits on the wire and the settings (`crates/daemon/src/telemetry`);
//! this crate turns the body of one export request into [`InsightReport`](hutan_core::insight::InsightReport)s:
//!
//! 1. [`decode`] reads protobuf (a hand-written `prost` subset, [`wire`]) or OTLP-JSON into
//!    [`Decoded`]. Both keep **only the attributes on the allowlist** ([`allow`]); every other
//!    attribute's value (a prompt, a tool's arguments or output, a system instruction, a command
//!    line) is skipped without being read. Bounds: [`MAX_RECORDS`] records per request,
//!    [`MAX_ATTRS`] attributes per record, strings cut at [`MAX_STRING_CHARS`] characters.
//! 2. A [`Mapper`] (one per session) maps the decoded records of each agent to reports with
//!    `source: otlp`: Claude Code's and Codex's log events, and spans that follow the
//!    OpenTelemetry GenAI semantic conventions for every other agent. A shape that does not match
//!    maps to nothing — "not reported", never zero.
//!
//! Telemetry carries no authority: the output is insight (tokens, cost, tools, turns, files,
//! context) and nothing else.

pub mod allow;
pub mod encode;
mod gzip;
mod json;
pub mod map;
pub mod model;
mod proto;
pub mod wire;

pub use gzip::inflate;
pub use map::{Agent, Mapped, Mapper};
pub use model::Decoded;

use prost::bytes::Bytes;

/// Longest request body the receiver accepts on the wire.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Longest body after gzip.
pub const MAX_INFLATED_BYTES: usize = 4 * 1024 * 1024;
/// Log records plus spans read from one request; the rest are counted and dropped.
pub const MAX_RECORDS: usize = 2_000;
/// Allowlisted attributes kept per record.
pub const MAX_ATTRS: usize = 64;
/// A string attribute is cut at this many characters.
pub const MAX_STRING_CHARS: usize = 256;
/// String elements kept of an array attribute.
pub const MAX_ARRAY: usize = 64;

/// What an export request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Traces,
    Logs,
    Metrics,
}

impl Signal {
    /// The signal of the request path's last two segments (`v1/logs`).
    pub fn from_path_tail(tail: &str) -> Option<Self> {
        match tail {
            "v1/traces" => Some(Self::Traces),
            "v1/logs" => Some(Self::Logs),
            "v1/metrics" => Some(Self::Metrics),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Logs => "logs",
            Self::Metrics => "metrics",
        }
    }
}

/// How the body is encoded (`Content-Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `application/x-protobuf`.
    Protobuf,
    /// `application/json`.
    Json,
}

impl Encoding {
    /// The encoding of a `Content-Type` header value (parameters such as `; charset=utf-8` are
    /// ignored); `None` for any other type.
    pub fn from_content_type(value: &str) -> Option<Self> {
        let essence = value.split(';').next().unwrap_or_default().trim();
        if essence.eq_ignore_ascii_case("application/x-protobuf") {
            Some(Self::Protobuf)
        } else if essence.eq_ignore_ascii_case("application/json") {
            Some(Self::Json)
        } else {
            None
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }

    /// The signal's empty `Export*ServiceResponse` in this encoding: no partial success to
    /// report. An empty protobuf message is zero bytes; in JSON it is `{}`.
    pub fn empty_response(self) -> &'static [u8] {
        match self {
            Self::Protobuf => b"",
            Self::Json => b"{}",
        }
    }
}

/// Why a body is not read.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("not a protobuf OTLP request: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("not an OTLP-JSON request: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the gzip body is not valid or inflates to more than {MAX_INFLATED_BYTES} bytes")]
    Gzip,
}

/// Reads the body of an export request of `signal`. Metrics are not read: nothing maps them.
pub fn decode(signal: Signal, encoding: Encoding, body: Bytes) -> Result<Decoded, DecodeError> {
    match encoding {
        Encoding::Protobuf => proto::decode(signal, body),
        Encoding::Json => json::decode(signal, &body),
    }
}
