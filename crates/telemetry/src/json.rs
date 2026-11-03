//! OTLP-JSON (`application/json`) → [`Decoded`].
//!
//! The JSON mapping of OTLP has its quirks, all seen in the spike's recordings (Gemini, Qwen,
//! OpenCode send only JSON): field names in camelCase (snake_case is accepted too), 64-bit
//! integers as decimal strings, trace and span ids as hex, enums as numbers or names, and an
//! `AnyValue` that is an object with one `…Value` member. An attribute whose key is not on the
//! allowlist has its value skipped token by token, never built.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::model::{Attrs, Decoded, LogRecord, Resource, Span, Value, id_bytes};
use crate::{DecodeError, MAX_ARRAY, MAX_RECORDS, Signal, allow};

/// Decodes the body of an export request. Metrics are not read.
pub fn decode(signal: Signal, body: &[u8]) -> Result<Decoded, DecodeError> {
    match signal {
        Signal::Logs => {
            let request: Logs = serde_json::from_slice(body)?;
            Ok(logs(request))
        }
        Signal::Traces => {
            let request: Traces = serde_json::from_slice(body)?;
            Ok(traces(request))
        }
        Signal::Metrics => Ok(Decoded::default()),
    }
}

fn logs(request: Logs) -> Decoded {
    let mut out = Decoded::default();
    let mut room = MAX_RECORDS;
    for resource_logs in request.resource_logs.0 {
        let mut resource = Resource {
            attrs: resource_logs
                .resource
                .map(|r| r.attributes.0)
                .unwrap_or_default(),
            ..Resource::default()
        };
        out.dropped += resource_logs.scope_logs.dropped;
        for scope in resource_logs.scope_logs.items {
            out.dropped += scope.log_records.dropped;
            for record in scope.log_records.items {
                if room == 0 {
                    out.dropped += 1;
                    continue;
                }
                room -= 1;
                resource.logs.push(LogRecord {
                    attrs: record.attributes.0,
                });
            }
        }
        out.resources.push(resource);
    }
    out.dropped += request.resource_logs.1;
    out
}

fn traces(request: Traces) -> Decoded {
    let mut out = Decoded::default();
    let mut room = MAX_RECORDS;
    for resource_spans in request.resource_spans.0 {
        let mut resource = Resource {
            attrs: resource_spans
                .resource
                .map(|r| r.attributes.0)
                .unwrap_or_default(),
            ..Resource::default()
        };
        out.dropped += resource_spans.scope_spans.dropped;
        for scope in resource_spans.scope_spans.items {
            out.dropped += scope.spans.dropped;
            for span in scope.spans.items {
                if room == 0 {
                    out.dropped += 1;
                    continue;
                }
                room -= 1;
                resource.spans.push(Span {
                    span_id: id_bytes(&span.span_id),
                    parent_span_id: id_bytes(&span.parent_span_id),
                    start_ns: span.start_time_unix_nano.0,
                    end_ns: span.end_time_unix_nano.0,
                    error: span.status.is_some_and(|status| status.code.0 == 2),
                    attrs: span.attributes.0,
                });
            }
        }
        out.resources.push(resource);
    }
    out.dropped += request.resource_spans.1;
    out
}

// The request shapes --------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct Logs {
    #[serde(default, rename = "resourceLogs", alias = "resource_logs")]
    resource_logs: Capped<ResourceLogs>,
}

#[derive(Deserialize, Default)]
struct ResourceLogs {
    #[serde(default, deserialize_with = "nullable")]
    resource: Option<ResourceAttrs>,
    #[serde(default, rename = "scopeLogs", alias = "scope_logs")]
    scope_logs: Items<ScopeLogs>,
}

#[derive(Deserialize, Default)]
struct ScopeLogs {
    #[serde(default, rename = "logRecords", alias = "log_records")]
    log_records: Items<JsonLogRecord>,
}

#[derive(Deserialize, Default)]
struct JsonLogRecord {
    #[serde(default)]
    attributes: JsonAttrs,
}

#[derive(Deserialize, Default)]
struct Traces {
    #[serde(default, rename = "resourceSpans", alias = "resource_spans")]
    resource_spans: Capped<ResourceSpans>,
}

#[derive(Deserialize, Default)]
struct ResourceSpans {
    #[serde(default, deserialize_with = "nullable")]
    resource: Option<ResourceAttrs>,
    #[serde(default, rename = "scopeSpans", alias = "scope_spans")]
    scope_spans: Items<ScopeSpans>,
}

#[derive(Deserialize, Default)]
struct ScopeSpans {
    #[serde(default)]
    spans: Items<JsonSpan>,
}

#[derive(Deserialize, Default)]
struct JsonSpan {
    #[serde(default, rename = "spanId", alias = "span_id")]
    span_id: String,
    #[serde(default, rename = "parentSpanId", alias = "parent_span_id")]
    parent_span_id: String,
    #[serde(default, rename = "startTimeUnixNano", alias = "start_time_unix_nano")]
    start_time_unix_nano: Num64,
    #[serde(default, rename = "endTimeUnixNano", alias = "end_time_unix_nano")]
    end_time_unix_nano: Num64,
    #[serde(default)]
    attributes: JsonAttrs,
    #[serde(default, deserialize_with = "nullable")]
    status: Option<JsonStatus>,
}

#[derive(Deserialize, Default)]
struct JsonStatus {
    #[serde(default)]
    code: StatusCode,
}

#[derive(Deserialize, Default)]
struct ResourceAttrs {
    #[serde(default)]
    attributes: JsonAttrs,
}

/// A JSON `null` where a value is optional.
fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

// Arrays that stop at the cap --------------------------------------------------------------------

/// The elements of an array, at most [`MAX_RECORDS`]; the rest are skipped unread and counted.
struct Items<T> {
    items: Vec<T>,
    dropped: usize,
}

impl<T> Default for Items<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            dropped: 0,
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Items<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Items<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Items::default())
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Items::default();
                while out.items.len() < MAX_RECORDS {
                    match seq.next_element::<T>()? {
                        Some(item) => out.items.push(item),
                        None => return Ok(out),
                    }
                }
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    out.dropped += 1;
                }
                Ok(out)
            }
        }
        deserializer.deserialize_any(V(std::marker::PhantomData))
    }
}

/// `Items`, as the pair `(items, dropped)` the decoders read.
struct Capped<T>(Vec<T>, usize);

impl<T> Default for Capped<T> {
    fn default() -> Self {
        Self(Vec::new(), 0)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Capped<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Items::<T>::deserialize(deserializer)?;
        Ok(Self(items.items, items.dropped))
    }
}

// Numbers, enums ---------------------------------------------------------------------------------

/// A 64-bit unsigned number as JSON writes it: a decimal string, or a plain number.
#[derive(Default, Clone, Copy)]
struct Num64(u64);

impl<'de> Deserialize<'de> for Num64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Num64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a decimal string")
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Num64, E> {
                Ok(Num64(n))
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Num64, E> {
                Ok(Num64(u64::try_from(n).unwrap_or(0)))
            }

            fn visit_f64<E: de::Error>(self, x: f64) -> Result<Num64, E> {
                Ok(Num64(if x.is_finite() && x >= 0.0 {
                    x as u64
                } else {
                    0
                }))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Num64, E> {
                Ok(Num64(text.trim().parse().unwrap_or(0)))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Num64, E> {
                Ok(Num64(0))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// A span status code: a number, or the enum's name (`STATUS_CODE_ERROR`).
#[derive(Default, Clone, Copy)]
struct StatusCode(i32);

impl<'de> Deserialize<'de> for StatusCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = StatusCode;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a status code")
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<StatusCode, E> {
                Ok(StatusCode(i32::try_from(n).unwrap_or(0)))
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<StatusCode, E> {
                Ok(StatusCode(i32::try_from(n).unwrap_or(0)))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<StatusCode, E> {
                Ok(StatusCode(match text {
                    "STATUS_CODE_OK" => 1,
                    "STATUS_CODE_ERROR" => 2,
                    _ => 0,
                }))
            }

            fn visit_unit<E: de::Error>(self) -> Result<StatusCode, E> {
                Ok(StatusCode(0))
            }
        }
        deserializer.deserialize_any(V)
    }
}

// Attributes -------------------------------------------------------------------------------------

/// The allowlisted attributes of a record.
#[derive(Default)]
struct JsonAttrs(Attrs);

impl<'de> Deserialize<'de> for JsonAttrs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = JsonAttrs;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array of attributes")
            }

            fn visit_unit<E: de::Error>(self) -> Result<JsonAttrs, E> {
                Ok(JsonAttrs::default())
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JsonAttrs, A::Error> {
                let mut attrs = Attrs::default();
                while let Some(kv) = seq.next_element::<KeyValue>()? {
                    if let Some((key, value)) = kv.0 {
                        attrs.push(key, value);
                    }
                }
                Ok(JsonAttrs(attrs))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// One attribute; `None` when its key is not allowed or it has no usable value.
struct KeyValue(Option<(String, Value)>);

impl<'de> Deserialize<'de> for KeyValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = KeyValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an attribute")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyValue, A::Error> {
                let mut key: Option<String> = None;
                // A value that came before its key (JSON objects are unordered).
                let mut early: Option<Value> = None;
                let mut value: Option<Value> = None;
                while let Some(name) = map.next_key::<String>()? {
                    match name.as_str() {
                        "key" => key = map.next_value()?,
                        "value" => match &key {
                            Some(held) if !allow::attribute(held) => {
                                map.next_value::<IgnoredAny>()?;
                            }
                            Some(_) => value = map.next_value::<AnyValue>()?.0,
                            None => early = map.next_value::<AnyValue>()?.0,
                        },
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(KeyValue(key.and_then(|key| {
                    let value = value.or(early)?;
                    allow::attribute(&key).then_some((key, value))
                })))
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// An `AnyValue`: an object with one `…Value` member. Key-value lists and bytes are not read.
struct AnyValue(Option<Value>);

impl<'de> Deserialize<'de> for AnyValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = AnyValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value")
            }

            fn visit_unit<E: de::Error>(self) -> Result<AnyValue, E> {
                Ok(AnyValue(None))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AnyValue, A::Error> {
                let mut out = None;
                while let Some(name) = map.next_key::<String>()? {
                    match name.as_str() {
                        "stringValue" | "string_value" => {
                            out = map.next_value::<Option<String>>()?.map(Value::Str);
                        }
                        "boolValue" | "bool_value" => {
                            out = map.next_value::<Option<bool>>()?.map(Value::Bool);
                        }
                        "intValue" | "int_value" => {
                            out = map.next_value::<Int>()?.0.map(Value::Int);
                        }
                        "doubleValue" | "double_value" => {
                            out = map.next_value::<Float>()?.0.map(Value::Double);
                        }
                        "arrayValue" | "array_value" => {
                            out = map.next_value::<Array>()?.0.map(Value::Strs);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(AnyValue(out))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// `intValue`: a decimal string (what the JSON mapping says) or a number.
struct Int(Option<i64>);

impl<'de> Deserialize<'de> for Int {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Int;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer")
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Int, E> {
                Ok(Int(Some(n)))
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Int, E> {
                Ok(Int(i64::try_from(n).ok()))
            }

            fn visit_f64<E: de::Error>(self, x: f64) -> Result<Int, E> {
                Ok(Int((x.is_finite() && x.fract() == 0.0).then_some(x as i64)))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Int, E> {
                Ok(Int(text.trim().parse().ok()))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Int, E> {
                Ok(Int(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// `doubleValue`: a number, or text such as `"0.02"` or `"NaN"` (not usable).
struct Float(Option<f64>);

impl<'de> Deserialize<'de> for Float {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Float;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number")
            }

            fn visit_f64<E: de::Error>(self, x: f64) -> Result<Float, E> {
                Ok(Float(Some(x)))
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Float, E> {
                Ok(Float(Some(n as f64)))
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Float, E> {
                Ok(Float(Some(n as f64)))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Float, E> {
                Ok(Float(text.trim().parse().ok()))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Float, E> {
                Ok(Float(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// `arrayValue`: the string elements of `{"values": […]}`, at most [`MAX_ARRAY`].
struct Array(Option<Vec<String>>);

impl<'de> Deserialize<'de> for Array {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Array;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array value")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Array, E> {
                Ok(Array(None))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Array, A::Error> {
                let mut out = None;
                while let Some(name) = map.next_key::<String>()? {
                    if name == "values" {
                        out = Some(map.next_value::<Strings>()?.0);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(Array(out))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// The string elements of an array of `AnyValue`s.
struct Strings(Vec<String>);

impl<'de> Deserialize<'de> for Strings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Strings;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array of values")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Strings, E> {
                Ok(Strings(Vec::new()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strings, A::Error> {
                let mut out = Vec::new();
                while let Some(item) = seq.next_element::<AnyValue>()? {
                    if let Some(Value::Str(text)) = item.0
                        && out.len() < MAX_ARRAY
                    {
                        out.push(text);
                    }
                }
                Ok(Strings(out))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_str<T: serde::de::DeserializeOwned>(text: &str) -> T {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn a_value_before_its_key_is_read_when_the_key_is_allowed_and_dropped_otherwise() {
        let attrs: JsonAttrs = from_str(
            r#"[{"value":{"stringValue":"claude-opus-5-5"},"key":"model"},
                {"value":{"stringValue":"secret"},"key":"prompt"},
                {"key":"arguments","value":{"stringValue":"secret"}}]"#,
        );
        assert_eq!(attrs.0.len(), 1);
        assert_eq!(attrs.0.str("model"), Some("claude-opus-5-5"));
    }

    #[test]
    fn numbers_come_as_strings_numbers_or_doubles() {
        let attrs: JsonAttrs = from_str(
            r#"[{"key":"input_tokens","value":{"intValue":"1200"}},
                {"key":"output_tokens","value":{"intValue":30}},
                {"key":"cost_usd","value":{"doubleValue":0.0204}},
                {"key":"duration_ms","value":{"stringValue":"16"}}]"#,
        );
        assert_eq!(attrs.0.u64("input_tokens"), Some(1200));
        assert_eq!(attrs.0.u64("output_tokens"), Some(30));
        assert_eq!(attrs.0.f64("cost_usd"), Some(0.0204));
        assert_eq!(attrs.0.u64("duration_ms"), Some(16));
    }
}
