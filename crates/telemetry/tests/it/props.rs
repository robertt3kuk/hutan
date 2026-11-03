//! Property tests of the decoders (the brief's fuzz target): arbitrary and mutated bytes never
//! panic and never exceed the bounds; a payload and its protobuf encoding decode alike.

use hutan_telemetry::{
    Agent, Encoding, MAX_ATTRS, MAX_RECORDS, MAX_STRING_CHARS, Mapper, Signal, decode,
};
use proptest::prelude::*;
use prost::bytes::Bytes;

use crate::fixtures;

const RECORDINGS: [(&str, &str, Signal); 6] = [
    ("claude-code-2.1.316", "logs.json", Signal::Logs),
    ("codex-0.166.0", "logs-turn.json", Signal::Logs),
    ("copilot-cli-1.0.105", "traces.json", Signal::Traces),
    ("goose-1.57.0", "traces.json", Signal::Traces),
    ("gemini-cli-0.66.0", "logs-api.json", Signal::Logs),
    ("opencode-1.19.5", "traces-ai-spans.json", Signal::Traces),
];

/// Whatever came out of a decoder is within the bounds, and mapping it does not panic.
fn check(signal: Signal, encoding: Encoding, body: Vec<u8>) {
    let Ok(decoded) = decode(signal, encoding, Bytes::from(body)) else {
        return;
    };
    assert!(decoded.records() <= MAX_RECORDS);
    for resource in &decoded.resources {
        assert!(resource.attrs.len() <= MAX_ATTRS);
        for attrs in resource
            .logs
            .iter()
            .map(|l| &l.attrs)
            .chain(resource.spans.iter().map(|s| &s.attrs))
        {
            assert!(attrs.len() <= MAX_ATTRS);
            for (key, value) in attrs.iter() {
                assert!(hutan_telemetry::allow::attribute(key));
                if let Some(text) = value.as_str() {
                    assert!(text.chars().count() <= MAX_STRING_CHARS);
                }
            }
        }
    }
    let _ = Mapper::new().map(&decoded, &|_: Agent| true, 1);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_break_a_decoder(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        for signal in [Signal::Logs, Signal::Traces] {
            for encoding in [Encoding::Protobuf, Encoding::Json] {
                check(signal, encoding, body.clone());
            }
        }
    }

    #[test]
    fn a_recording_with_bytes_changed_or_cut_never_breaks_a_decoder(
        which in 0..RECORDINGS.len(),
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..8),
        cut in proptest::option::of(any::<usize>()),
    ) {
        let (dir, file, signal) = RECORDINGS[which];
        let body = fixtures::load(dir, file);
        for (encoding, mut bytes) in [
            (Encoding::Protobuf, hutan_telemetry::encode::json_to_protobuf(signal, &body)),
            (Encoding::Json, serde_json::to_vec(&body).unwrap()),
        ] {
            for (at, value) in &flips {
                if !bytes.is_empty() {
                    let at = at % bytes.len();
                    bytes[at] = *value;
                }
            }
            if let Some(cut) = cut
                && !bytes.is_empty()
            {
                bytes.truncate(cut % bytes.len());
            }
            check(signal, encoding, bytes);
        }
    }
}

#[test]
fn every_recording_decodes_alike_in_both_encodings() {
    for (dir, file, signal) in RECORDINGS {
        let body = fixtures::load(dir, file);
        fixtures::both(signal, &body);
    }
}
