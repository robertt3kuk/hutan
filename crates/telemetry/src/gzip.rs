//! gzip bodies, inflated under a limit (the spike's F9: a 64 MiB body of zeros gzips to 65 KB).

use std::io::Read as _;

use flate2::read::GzDecoder;

use crate::{DecodeError, MAX_INFLATED_BYTES};

/// Inflates a gzip `body`. Reads at most [`MAX_INFLATED_BYTES`] + 1 bytes out of the stream, so
/// a bomb costs that much and no more; an inflated size over the limit is an error.
pub fn inflate(body: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    GzDecoder::new(body)
        .take(MAX_INFLATED_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| DecodeError::Gzip)?;
    if out.len() > MAX_INFLATED_BYTES {
        return Err(DecodeError::Gzip);
    }
    Ok(out)
}
