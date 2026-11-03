//! The spike's recordings (`docs/spikes/2026-11-03-agent-telemetry/fixtures/`), read in both
//! encodings: the JSON as recorded, and the protobuf `encode::json_to_protobuf` builds from it.

use hutan_telemetry::model::Decoded;
use hutan_telemetry::{Agent, Encoding, Mapped, Mapper, Signal, decode, encode};
use prost::bytes::Bytes;
use serde_json::Value;

/// The request body of a recording (`body` of the wrapper the spike saved).
pub fn load(dir: &str, file: &str) -> Value {
    let path = format!(
        "{}/../../docs/spikes/2026-11-03-agent-telemetry/fixtures/{dir}/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let wrapper: Value = serde_json::from_str(&text).unwrap();
    wrapper["body"].clone()
}

/// The recording decoded from its JSON form.
pub fn json(signal: Signal, body: &Value) -> Decoded {
    let bytes = Bytes::from(serde_json::to_vec(body).unwrap());
    decode(signal, Encoding::Json, bytes).unwrap()
}

/// The recording decoded from the protobuf form of the same payload.
pub fn protobuf(signal: Signal, body: &Value) -> Decoded {
    let bytes = encode::json_to_protobuf_bytes(signal, body);
    decode(signal, Encoding::Protobuf, bytes).unwrap()
}

/// Both decodings of one recording, which must say the same.
pub fn both(signal: Signal, body: &Value) -> [Decoded; 2] {
    let (a, b) = (json(signal, body), protobuf(signal, body));
    assert_eq!(a, b, "JSON and protobuf must decode alike");
    [a, b]
}

/// Maps with every agent enabled, as a fresh mapper at `now_ms`.
pub fn map(decoded: &Decoded, now_ms: u64) -> Vec<Mapped> {
    Mapper::new().map(decoded, &|_: Agent| true, now_ms)
}
