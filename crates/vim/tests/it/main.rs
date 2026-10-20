//! Tests of `hutan-vim` through its public API, against a reference `String` host
//! (`common`). The desktop's `TextInput` adapter runs the original V-1 suite against the same
//! state machine.

// Test helpers outside `#[test]` functions unwrap freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod fuzz;
mod host;
mod keys;
