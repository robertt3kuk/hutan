//! Protobuf OTLP (`application/x-protobuf`) → [`Decoded`].

use prost::Message as _;
use prost::bytes::Bytes;

use crate::model::{Attrs, Decoded, LogRecord, Resource, Span, Value};
use crate::wire::{self, any_value};
use crate::{DecodeError, MAX_RECORDS, Signal, allow};

/// Decodes the body of an export request. Metrics are not read (nothing maps them), so a
/// metrics body is not even parsed.
pub fn decode(signal: Signal, body: Bytes) -> Result<Decoded, DecodeError> {
    match signal {
        Signal::Logs => {
            let request = wire::ExportLogs::decode(body)?;
            Ok(logs(request))
        }
        Signal::Traces => {
            let request = wire::ExportTraces::decode(body)?;
            Ok(traces(request))
        }
        Signal::Metrics => Ok(Decoded::default()),
    }
}

fn logs(request: wire::ExportLogs) -> Decoded {
    let mut out = Decoded::default();
    let mut room = MAX_RECORDS;
    for resource_logs in request.resource_logs {
        let mut resource = Resource {
            attrs: attrs(resource_logs.resource.as_ref().map(|r| &r.attributes[..])),
            ..Resource::default()
        };
        for scope in resource_logs.scope_logs {
            for record in scope.log_records {
                if room == 0 {
                    out.dropped += 1;
                    continue;
                }
                room -= 1;
                resource.logs.push(LogRecord {
                    attrs: attrs(Some(&record.attributes)),
                });
            }
        }
        out.resources.push(resource);
    }
    out
}

fn traces(request: wire::ExportTraces) -> Decoded {
    let mut out = Decoded::default();
    let mut room = MAX_RECORDS;
    for resource_spans in request.resource_spans {
        let mut resource = Resource {
            attrs: attrs(resource_spans.resource.as_ref().map(|r| &r.attributes[..])),
            ..Resource::default()
        };
        for scope in resource_spans.scope_spans {
            for span in scope.spans {
                if room == 0 {
                    out.dropped += 1;
                    continue;
                }
                room -= 1;
                resource.spans.push(Span {
                    span_id: span.span_id,
                    parent_span_id: span.parent_span_id,
                    start_ns: span.start_time_unix_nano,
                    end_ns: span.end_time_unix_nano,
                    error: span
                        .status
                        .is_some_and(|status| status.code == wire::STATUS_ERROR),
                    attrs: attrs(Some(&span.attributes)),
                });
            }
        }
        out.resources.push(resource);
    }
    out
}

/// The allowlisted attributes of `list`; the value of any other is never decoded.
fn attrs(list: Option<&[wire::KeyValue]>) -> Attrs {
    let mut out = Attrs::default();
    for kv in list.unwrap_or_default() {
        if out.len() >= crate::MAX_ATTRS {
            break;
        }
        if !allow::attribute(&kv.key) {
            continue;
        }
        if let Some(value) = kv.any_value().and_then(|any| value(any.value)) {
            out.push(kv.key.clone(), value);
        }
    }
    out
}

fn value(value: Option<any_value::Value>) -> Option<Value> {
    Some(match value? {
        any_value::Value::StringValue(text) => Value::Str(text),
        any_value::Value::BoolValue(flag) => Value::Bool(flag),
        any_value::Value::IntValue(n) => Value::Int(n),
        any_value::Value::DoubleValue(x) => Value::Double(x),
        any_value::Value::ArrayValue(array) => Value::Strs(
            array
                .values
                .into_iter()
                .filter_map(|item| match item.value {
                    Some(any_value::Value::StringValue(text)) => Some(text),
                    _ => None,
                })
                .take(crate::MAX_ARRAY)
                .collect(),
        ),
    })
}
