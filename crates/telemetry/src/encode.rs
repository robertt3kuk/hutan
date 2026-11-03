//! Builds protobuf OTLP from the OTLP-JSON form, for tests and the fuzz target: the recordings
//! of the spike (`docs/spikes/2026-11-03-agent-telemetry/fixtures/`) are kept as JSON, and a
//! receiver must read the same payload in both encodings.
//!
//! Nothing is filtered here: an attribute the allowlist refuses is encoded like any other, so a
//! test can prove the receiver is the one that drops it.

use prost::Message as _;
use prost::bytes::Bytes;
use serde_json::Value;

use crate::Signal;
use crate::wire::{
    self, AnyValue, ExportLogs, ExportTraces, KeyValue, LogRecord, Resource, ResourceLogs,
    ResourceSpans, ScopeLogs, ScopeSpans, Span, Status,
};

/// The protobuf body of an export request given as OTLP-JSON (`resourceLogs`, `resourceSpans`).
/// Metrics encode as an empty request.
pub fn json_to_protobuf(signal: Signal, request: &Value) -> Vec<u8> {
    match signal {
        Signal::Logs => ExportLogs {
            resource_logs: items(request, "resourceLogs")
                .map(|rl| ResourceLogs {
                    resource: resource(rl),
                    scope_logs: items(rl, "scopeLogs")
                        .map(|sl| ScopeLogs {
                            log_records: items(sl, "logRecords")
                                .map(|record| LogRecord {
                                    attributes: attributes(record),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
        .encode_to_vec(),
        Signal::Traces => ExportTraces {
            resource_spans: items(request, "resourceSpans")
                .map(|rs| ResourceSpans {
                    resource: resource(rs),
                    scope_spans: items(rs, "scopeSpans")
                        .map(|ss| ScopeSpans {
                            spans: items(ss, "spans").map(span).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
        .encode_to_vec(),
        Signal::Metrics => Vec::new(),
    }
}

fn items<'a>(parent: &'a Value, name: &str) -> impl Iterator<Item = &'a Value> {
    parent
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn resource(parent: &Value) -> Option<Resource> {
    parent.get("resource").map(|resource| Resource {
        attributes: attributes(resource),
    })
}

fn attributes(parent: &Value) -> Vec<KeyValue> {
    items(parent, "attributes")
        .filter_map(|kv| {
            let key = kv.get("key")?.as_str()?;
            Some(KeyValue::new(key, any_value(kv.get("value")?)))
        })
        .collect()
}

fn any_value(value: &Value) -> AnyValue {
    if let Some(text) = value.get("stringValue").and_then(Value::as_str) {
        AnyValue::string(text)
    } else if let Some(n) = value.get("intValue") {
        let n = n
            .as_i64()
            .or_else(|| n.as_str().and_then(|text| text.parse().ok()))
            .unwrap_or(0);
        AnyValue::int(n)
    } else if let Some(x) = value.get("doubleValue").and_then(Value::as_f64) {
        AnyValue::double(x)
    } else if let Some(flag) = value.get("boolValue").and_then(Value::as_bool) {
        AnyValue::bool(flag)
    } else if let Some(array) = value.get("arrayValue") {
        AnyValue::array(items(array, "values").map(any_value).collect())
    } else {
        AnyValue::default()
    }
}

fn span(span: &Value) -> Span {
    let number = |name: &str| {
        span.get(name)
            .and_then(|n| n.as_u64().or_else(|| n.as_str()?.parse().ok()))
            .unwrap_or(0)
    };
    let id = |name: &str| -> Vec<u8> {
        crate::model::id_bytes(span.get(name).and_then(Value::as_str).unwrap_or_default())
    };
    let code = span
        .get("status")
        .and_then(|status| status.get("code"))
        .map_or(0, |code| match code {
            Value::String(name) if name == "STATUS_CODE_ERROR" => wire::STATUS_ERROR,
            Value::String(name) if name == "STATUS_CODE_OK" => 1,
            other => other
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(0),
        });
    Span {
        span_id: id("spanId"),
        parent_span_id: id("parentSpanId"),
        start_time_unix_nano: number("startTimeUnixNano"),
        end_time_unix_nano: number("endTimeUnixNano"),
        attributes: attributes(span),
        status: Some(Status { code }),
    }
}

/// The body as the `Bytes` the decoder takes.
pub fn json_to_protobuf_bytes(signal: Signal, request: &Value) -> Bytes {
    Bytes::from(json_to_protobuf(signal, request))
}
