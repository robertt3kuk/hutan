//! The LSP base protocol's framing: a header section (`Content-Length: N`, optionally
//! `Content-Type`, each line ended by `\r\n`), an empty line, then `N` bytes of JSON.
//!
//! The decoder never holds more than one frame: a frame over [`MAX_FRAME_BYTES`] is skipped
//! as it streams by (its bytes are dropped, not buffered) and reported as
//! [`Frame::Oversized`], so a server cannot make the client allocate more than the cap. A
//! header section longer than [`MAX_HEADER_BYTES`] or without a valid `Content-Length` is a
//! protocol error that ends the connection.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Largest message body accepted from or sent to a server (a `workspace/symbol` answer of a
/// large monorepo stays well below).
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// Largest header section, terminator included.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A complete message body.
    Message(Bytes),
    /// A message over the frame limit, skipped; `len` is its announced size.
    Oversized { len: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("header section over {MAX_HEADER_BYTES} bytes")]
    HeaderTooLong,
    #[error("malformed header line {0:?}")]
    BadHeader(String),
    #[error("header section without Content-Length")]
    MissingLength,
    #[error("message of {len} bytes over the {max}-byte frame limit")]
    TooLarge { len: usize, max: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Header,
    Body(usize),
    Skip { remaining: usize, len: usize },
}

/// `Content-Length` framing with a size cap (`tokio_util` codec).
#[derive(Debug, Clone)]
pub struct LspCodec {
    max_frame: usize,
    state: State,
}

impl Default for LspCodec {
    fn default() -> Self {
        Self::new(MAX_FRAME_BYTES)
    }
}

impl LspCodec {
    /// A codec that skips incoming and refuses outgoing bodies over `max_frame` bytes.
    pub fn new(max_frame: usize) -> Self {
        Self {
            max_frame,
            state: State::Header,
        }
    }

    pub fn max_frame(&self) -> usize {
        self.max_frame
    }
}

fn parse_length(header: &[u8]) -> Result<usize, CodecError> {
    let mut length = None;
    for line in header.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(line)
            .map_err(|_| CodecError::BadHeader(String::from_utf8_lossy(line).into_owned()))?;
        let Some((name, value)) = text.split_once(':') else {
            return Err(CodecError::BadHeader(text.to_owned()));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| CodecError::BadHeader(text.to_owned()))?;
            length = Some(parsed);
        }
        // `Content-Type` and unknown headers are ignored (the body is always UTF-8 JSON).
    }
    length.ok_or(CodecError::MissingLength)
}

fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

impl Decoder for LspCodec {
    type Item = Frame;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        loop {
            match self.state {
                State::Header => {
                    let Some(end) = header_end(src) else {
                        if src.len() >= MAX_HEADER_BYTES {
                            return Err(CodecError::HeaderTooLong);
                        }
                        return Ok(None);
                    };
                    if end + 4 > MAX_HEADER_BYTES {
                        return Err(CodecError::HeaderTooLong);
                    }
                    let len = parse_length(&src[..end])?;
                    src.advance(end + 4);
                    self.state = if len > self.max_frame {
                        State::Skip {
                            remaining: len,
                            len,
                        }
                    } else {
                        State::Body(len)
                    };
                }
                State::Body(len) => {
                    if src.len() < len {
                        src.reserve(len - src.len());
                        return Ok(None);
                    }
                    self.state = State::Header;
                    return Ok(Some(Frame::Message(src.split_to(len).freeze())));
                }
                State::Skip { remaining, len } => {
                    let dropped = remaining.min(src.len());
                    src.advance(dropped);
                    let remaining = remaining - dropped;
                    if remaining > 0 {
                        self.state = State::Skip { remaining, len };
                        return Ok(None);
                    }
                    self.state = State::Header;
                    return Ok(Some(Frame::Oversized { len }));
                }
            }
        }
    }
}

impl Encoder<Bytes> for LspCodec {
    type Error = CodecError;

    fn encode(&mut self, body: Bytes, dst: &mut BytesMut) -> Result<(), CodecError> {
        if body.len() > self.max_frame {
            return Err(CodecError::TooLarge {
                len: body.len(),
                max: self.max_frame,
            });
        }
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        dst.reserve(header.len() + body.len());
        dst.put_slice(header.as_bytes());
        dst.put_slice(&body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn decodes_frames_split_anywhere() {
        let mut stream = framed(r#"{"a":1}"#);
        stream.extend(
            b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n[]",
        );
        // Feed one byte at a time: every split point must work.
        let mut codec = LspCodec::default();
        let mut buf = BytesMut::new();
        let mut frames = Vec::new();
        for byte in stream {
            buf.put_u8(byte);
            while let Some(frame) = codec.decode(&mut buf).unwrap() {
                frames.push(frame);
            }
        }
        assert_eq!(
            frames,
            vec![
                Frame::Message(Bytes::from_static(br#"{"a":1}"#)),
                Frame::Message(Bytes::from_static(b"[]")),
            ]
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn oversized_frames_are_skipped_without_buffering() {
        let mut codec = LspCodec::new(8);
        let big = "x".repeat(1000);
        let mut buf = BytesMut::from(&framed(&big)[..40]);
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        // The body bytes seen so far were dropped, not kept.
        assert!(buf.is_empty());
        let mut rest = BytesMut::from(&framed(&big)[40..]);
        rest.extend_from_slice(&framed("{}"));
        assert_eq!(
            codec.decode(&mut rest).unwrap(),
            Some(Frame::Oversized { len: 1000 })
        );
        assert_eq!(
            codec.decode(&mut rest).unwrap(),
            Some(Frame::Message(Bytes::from_static(b"{}")))
        );
    }

    #[test]
    fn bad_headers_are_errors() {
        let mut codec = LspCodec::default();
        let mut buf = BytesMut::from(&b"Content-Type: x\r\n\r\n{}"[..]);
        assert!(matches!(
            codec.decode(&mut buf),
            Err(CodecError::MissingLength)
        ));
        let mut codec = LspCodec::default();
        let mut buf = BytesMut::from(&b"Content-Length: many\r\n\r\n"[..]);
        assert!(matches!(
            codec.decode(&mut buf),
            Err(CodecError::BadHeader(_))
        ));
        let mut codec = LspCodec::default();
        let mut buf = BytesMut::from(vec![b'a'; MAX_HEADER_BYTES].as_slice());
        assert!(matches!(
            codec.decode(&mut buf),
            Err(CodecError::HeaderTooLong)
        ));
    }

    #[test]
    fn encodes_with_a_header_and_refuses_oversized_bodies() {
        let mut codec = LspCodec::new(4);
        let mut out = BytesMut::new();
        codec.encode(Bytes::from_static(b"{}"), &mut out).unwrap();
        assert_eq!(&out[..], b"Content-Length: 2\r\n\r\n{}");
        assert!(matches!(
            codec.encode(Bytes::from_static(b"{\"a\":1}"), &mut out),
            Err(CodecError::TooLarge { len: 7, max: 4 })
        ));
    }
}
