//! What the decoders produce and the mappers read: only the allowlisted attributes of the log
//! records and spans of one request, grouped by resource.

use crate::{MAX_ATTRS, MAX_STRING_CHARS};

/// The value of an allowlisted attribute. Arrays keep their string elements only; a nested
/// structure, bytes and a key-value list are not read.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    Strs(Vec<String>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text),
            _ => None,
        }
    }

    /// A count: a non-negative whole number, however the agent wrote it (Claude Code writes
    /// some counts as integers and some as decimal text).
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Int(n) => u64::try_from(*n).ok(),
            Self::Double(x) if x.is_finite() && *x >= 0.0 && x.fract() == 0.0 && *x < 1.8e19 => {
                Some(*x as u64)
            }
            Self::Str(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    /// A non-negative finite number (a cost in US dollars, a count).
    pub fn as_f64(&self) -> Option<f64> {
        let x = match self {
            Self::Int(n) => *n as f64,
            Self::Double(x) => *x,
            Self::Str(text) => text.trim().parse().ok()?,
            _ => return None,
        };
        (x.is_finite() && x >= 0.0).then_some(x)
    }

    /// `true`/`false` as a flag or as text (Claude Code and Codex write `success` as text).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(flag) => Some(*flag),
            Self::Str(text) => match text.trim() {
                "true" | "True" | "TRUE" => Some(true),
                "false" | "False" | "FALSE" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The allowlisted attributes of one record, in the order they arrived; at most
/// [`MAX_ATTRS`], strings cut at [`MAX_STRING_CHARS`] characters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attrs(Vec<(String, Value)>);

impl Attrs {
    /// Keeps `key` if it is on the allowlist and there is room; the first of a repeated key
    /// wins (OTLP says a repeated key is invalid).
    pub fn push(&mut self, key: String, value: Value) {
        if self.0.len() >= MAX_ATTRS
            || !crate::allow::attribute(&key)
            || self.0.iter().any(|(held, _)| *held == key)
        {
            return;
        }
        let value = match value {
            Value::Str(text) => Value::Str(clip(text)),
            Value::Strs(items) => Value::Strs(items.into_iter().map(clip).collect()),
            other => other,
        };
        self.0.push((key, value));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find_map(|(held, value)| (held == key).then_some(value))
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.get(key)?.as_u64()
    }

    pub fn f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    pub fn strs(&self, key: &str) -> Option<&[String]> {
        match self.get(key)? {
            Value::Strs(items) => Some(items),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }
}

/// Cuts `text` to [`MAX_STRING_CHARS`] characters.
pub(crate) fn clip(text: String) -> String {
    match text.char_indices().nth(MAX_STRING_CHARS) {
        Some((at, _)) => text[..at].to_owned(),
        None => text,
    }
}

/// The bytes that identify a span in JSON, where an id is text: OTLP says hex; any other text
/// (the spike's recordings carry base64) stands for itself, since an id is only ever compared
/// for equality. Over 64 characters it is no id and reads as none.
pub fn id_bytes(text: &str) -> Vec<u8> {
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    if bytes.len() > 64 {
        return Vec::new();
    }
    if bytes.len().is_multiple_of(2)
        && let Some(decoded) = bytes
            .chunks(2)
            .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
            .collect::<Option<Vec<u8>>>()
    {
        return decoded;
    }
    bytes.to_vec()
}

/// A log record: its allowlisted attributes (the event name, counts, a tool's name).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogRecord {
    pub attrs: Attrs,
}

/// A span: its allowlisted attributes, its duration and whether it ended in error, and the ids
/// that tell a span from the span it wraps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub span_id: Vec<u8>,
    pub parent_span_id: Vec<u8>,
    pub start_ns: u64,
    pub end_ns: u64,
    /// The span's status is an error.
    pub error: bool,
    pub attrs: Attrs,
}

impl Span {
    /// How long the span ran, in whole milliseconds.
    pub fn duration_ms(&self) -> Option<u64> {
        (self.end_ns >= self.start_ns && self.start_ns > 0)
            .then(|| (self.end_ns - self.start_ns) / 1_000_000)
    }
}

/// The records of one resource (one agent process) in a request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resource {
    pub attrs: Attrs,
    pub logs: Vec<LogRecord>,
    pub spans: Vec<Span>,
}

/// One decoded request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoded {
    pub resources: Vec<Resource>,
    /// Records past [`MAX_RECORDS`](crate::MAX_RECORDS) that were not read.
    pub dropped: usize,
}

impl Decoded {
    /// Log records and spans read.
    pub fn records(&self) -> usize {
        self.resources
            .iter()
            .map(|resource| resource.logs.len() + resource.spans.len())
            .sum()
    }
}
