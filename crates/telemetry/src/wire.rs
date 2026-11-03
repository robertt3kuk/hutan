//! The subset of OTLP's protobuf messages Hutan decodes (`opentelemetry/proto/collector/{logs,trace}/v1`
//! and what they contain), written by hand with `prost` derive (ADR 0026 decision 1).
//!
//! Only fields a mapper could read are declared; every other field (a log record's body, a
//! span's name and events, links, scopes, metrics) is skipped by the decoder without being
//! copied. An attribute's value stays an **undecoded slice** of the request until its key has
//! passed the allowlist ([`crate::allow`]), so the value of a prompt is never turned into text.
//! The messages encode as well: the tests and the fuzz target build payloads with them.

use prost::bytes::Bytes;

/// `ExportLogsServiceRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogs {
    #[prost(message, repeated, tag = "1")]
    pub resource_logs: Vec<ResourceLogs>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceLogs {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_logs: Vec<ScopeLogs>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ScopeLogs {
    #[prost(message, repeated, tag = "2")]
    pub log_records: Vec<LogRecord>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct LogRecord {
    #[prost(message, repeated, tag = "6")]
    pub attributes: Vec<KeyValue>,
}

/// `ExportTraceServiceRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportTraces {
    #[prost(message, repeated, tag = "1")]
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceSpans {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ScopeSpans {
    #[prost(message, repeated, tag = "2")]
    pub spans: Vec<Span>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Span {
    #[prost(bytes = "vec", tag = "2")]
    pub span_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "4")]
    pub parent_span_id: Vec<u8>,
    #[prost(fixed64, tag = "7")]
    pub start_time_unix_nano: u64,
    #[prost(fixed64, tag = "8")]
    pub end_time_unix_nano: u64,
    #[prost(message, repeated, tag = "9")]
    pub attributes: Vec<KeyValue>,
    #[prost(message, optional, tag = "15")]
    pub status: Option<Status>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Status {
    /// `STATUS_CODE_UNSET` 0, `STATUS_CODE_OK` 1, `STATUS_CODE_ERROR` 2.
    #[prost(int32, tag = "3")]
    pub code: i32,
}

/// The `STATUS_CODE_ERROR` of [`Status::code`].
pub const STATUS_ERROR: i32 = 2;

#[derive(Clone, PartialEq, prost::Message)]
pub struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

/// An attribute. `value` is the encoded `AnyValue`, not yet decoded: a `Bytes` slice of the
/// request (no copy), decoded by [`KeyValue::any_value`] only for a key on the allowlist.
#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(bytes = "bytes", optional, tag = "2")]
    pub value: Option<Bytes>,
}

impl KeyValue {
    /// The attribute `key` with `value` encoded (for building payloads).
    pub fn new(key: &str, value: AnyValue) -> Self {
        use prost::Message as _;
        Self {
            key: key.to_owned(),
            value: Some(Bytes::from(value.encode_to_vec())),
        }
    }

    /// The value, decoded now. `None` when there is none or it is not well formed.
    pub fn any_value(&self) -> Option<AnyValue> {
        use prost::Message as _;
        AnyValue::decode(self.value.clone()?).ok()
    }
}

/// `AnyValue`. A key-value list (6) and bytes (7) are not declared: they decode as no value.
#[derive(Clone, PartialEq, prost::Message)]
pub struct AnyValue {
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4, 5")]
    pub value: Option<any_value::Value>,
}

pub mod any_value {
    use super::ArrayValue;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        #[prost(string, tag = "1")]
        StringValue(String),
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        #[prost(int64, tag = "3")]
        IntValue(i64),
        #[prost(double, tag = "4")]
        DoubleValue(f64),
        #[prost(message, tag = "5")]
        ArrayValue(ArrayValue),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ArrayValue {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<AnyValue>,
}

impl AnyValue {
    pub fn string(text: &str) -> Self {
        Self {
            value: Some(any_value::Value::StringValue(text.to_owned())),
        }
    }

    pub fn int(n: i64) -> Self {
        Self {
            value: Some(any_value::Value::IntValue(n)),
        }
    }

    pub fn double(x: f64) -> Self {
        Self {
            value: Some(any_value::Value::DoubleValue(x)),
        }
    }

    pub fn bool(flag: bool) -> Self {
        Self {
            value: Some(any_value::Value::BoolValue(flag)),
        }
    }

    pub fn array(values: Vec<Self>) -> Self {
        Self {
            value: Some(any_value::Value::ArrayValue(ArrayValue { values })),
        }
    }
}
