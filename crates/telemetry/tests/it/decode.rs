//! The decoders' bounds and refusals.

use std::io::Write as _;

use hutan_telemetry::wire::{
    AnyValue, ExportLogs, ExportTraces, KeyValue, LogRecord, Resource, ResourceLogs, ScopeLogs,
};
use hutan_telemetry::{
    DecodeError, Encoding, MAX_ATTRS, MAX_INFLATED_BYTES, MAX_RECORDS, MAX_STRING_CHARS, Signal,
    decode, inflate,
};
use prost::Message as _;
use prost::bytes::Bytes;
use serde_json::json;

fn log_request(records: usize, attrs: impl Fn(usize) -> Vec<KeyValue>) -> Bytes {
    Bytes::from(
        ExportLogs {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource::default()),
                scope_logs: vec![ScopeLogs {
                    log_records: (0..records)
                        .map(|n| LogRecord {
                            attributes: attrs(n),
                        })
                        .collect(),
                }],
            }],
        }
        .encode_to_vec(),
    )
}

#[test]
fn records_past_the_cap_are_counted_and_not_read() {
    let body = log_request(MAX_RECORDS + 7, |_| {
        vec![KeyValue::new("model", AnyValue::string("m"))]
    });
    let decoded = decode(Signal::Logs, Encoding::Protobuf, body).unwrap();
    assert_eq!(decoded.records(), MAX_RECORDS);
    assert_eq!(decoded.dropped, 7);

    let record = json!({"attributes": [{"key": "model", "value": {"stringValue": "m"}}]});
    let json =
        json!({"resourceLogs": [{"scopeLogs": [{"logRecords": vec![record; MAX_RECORDS + 7]}]}]});
    let decoded = decode(
        Signal::Logs,
        Encoding::Json,
        Bytes::from(serde_json::to_vec(&json).unwrap()),
    )
    .unwrap();
    assert_eq!(decoded.records(), MAX_RECORDS);
    assert_eq!(decoded.dropped, 7);
}

#[test]
fn attributes_are_capped_and_strings_are_cut() {
    let long = "é".repeat(MAX_STRING_CHARS * 3);
    let body = log_request(1, |_| {
        let mut attrs = vec![KeyValue::new("model", AnyValue::string(&long))];
        // 200 more names on the list, repeated: only 64 are kept (and a name once).
        attrs.extend((0..200).map(|n| {
            KeyValue::new(
                ["input_tokens", "output_tokens", "tool_name"][n % 3],
                AnyValue::int(n as i64),
            )
        }));
        attrs
    });
    let decoded = decode(Signal::Logs, Encoding::Protobuf, body).unwrap();
    let attrs = &decoded.resources[0].logs[0].attrs;
    assert!(attrs.len() <= MAX_ATTRS);
    assert_eq!(
        attrs.str("model").unwrap().chars().count(),
        MAX_STRING_CHARS
    );
    // The first of a repeated key wins.
    assert_eq!(attrs.u64("input_tokens"), Some(0));
}

#[test]
fn a_gzip_bomb_is_refused_without_inflating_it() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // 64 MiB of zeros gzips to about 64 KB (the spike's F9).
    let block = vec![0_u8; 1024 * 1024];
    for _ in 0..64 {
        encoder.write_all(&block).unwrap();
    }
    let bomb = encoder.finish().unwrap();
    assert!(bomb.len() < 1024 * 1024, "the bomb is small on the wire");
    assert!(matches!(inflate(&bomb), Err(DecodeError::Gzip)));

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"{\"resourceLogs\":[]}").unwrap();
    assert_eq!(
        inflate(&encoder.finish().unwrap()).unwrap(),
        b"{\"resourceLogs\":[]}"
    );
    assert!(matches!(inflate(b"not gzip"), Err(DecodeError::Gzip)));
    assert_eq!(MAX_INFLATED_BYTES, 4 * 1024 * 1024);
}

#[test]
fn malformed_bodies_are_errors_not_panics() {
    for garbage in [
        &b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff"[..],
        b"\x0a\x05abc",
        b"\x0a\xff\xff\xff\x0f",
        b"{",
        b"[]",
        b"\"x\"",
        b"",
    ] {
        for encoding in [Encoding::Protobuf, Encoding::Json] {
            for signal in [Signal::Logs, Signal::Traces] {
                // Some of these are valid empty requests: the point is that none panics.
                let _ = decode(signal, encoding, Bytes::copy_from_slice(garbage));
            }
        }
    }
    assert!(decode(Signal::Logs, Encoding::Json, Bytes::from_static(b"{")).is_err());
    assert!(
        decode(
            Signal::Logs,
            Encoding::Protobuf,
            Bytes::from_static(b"\x0a\x05abc")
        )
        .is_err()
    );
    // Metrics are not even parsed.
    assert!(
        decode(
            Signal::Metrics,
            Encoding::Protobuf,
            Bytes::from_static(b"\xff")
        )
        .unwrap()
        .resources
        .is_empty()
    );
}

#[test]
fn deeply_nested_json_is_refused() {
    let nested = format!("{}1{}", "[".repeat(5_000), "]".repeat(5_000));
    let body = format!(
        "{{\"resourceLogs\":[{{\"scopeLogs\":[{{\"logRecords\":[{{\"attributes\":[{{\"key\":\"model\",\"value\":{nested}}}]}}]}}]}}]}}"
    );
    assert!(decode(Signal::Logs, Encoding::Json, Bytes::from(body)).is_err());
}

#[test]
fn protobuf_unknown_fields_and_an_empty_request_are_fine() {
    // An empty request is valid: no records.
    let decoded = decode(Signal::Traces, Encoding::Protobuf, Bytes::new()).unwrap();
    assert!(decoded.resources.is_empty());
    // A field Hutan does not declare (a body, field 5 of a record) is skipped, not read.
    let mut record = LogRecord::default().encode_to_vec();
    record.extend_from_slice(&[0x2a, 0x03, b'a', b'b', b'c']);
    let mut scope = vec![0x12, record.len() as u8];
    scope.extend_from_slice(&record);
    let mut resource_logs = vec![0x12, scope.len() as u8];
    resource_logs.extend_from_slice(&scope);
    let mut request = vec![0x0a, resource_logs.len() as u8];
    request.extend_from_slice(&resource_logs);
    let decoded = decode(Signal::Logs, Encoding::Protobuf, Bytes::from(request)).unwrap();
    assert_eq!(decoded.records(), 1);
    let _ = ExportTraces::default();
}

#[test]
fn content_types_and_paths_are_recognised() {
    assert_eq!(
        Encoding::from_content_type("application/x-protobuf"),
        Some(Encoding::Protobuf)
    );
    assert_eq!(
        Encoding::from_content_type("application/json; charset=utf-8"),
        Some(Encoding::Json)
    );
    assert_eq!(Encoding::from_content_type("text/plain"), None);
    assert_eq!(Encoding::from_content_type(""), None);
    assert_eq!(Signal::from_path_tail("v1/logs"), Some(Signal::Logs));
    assert_eq!(Signal::from_path_tail("v1/other"), None);
    assert_eq!(Encoding::Protobuf.empty_response(), b"");
    assert_eq!(Encoding::Json.empty_response(), b"{}");
}
