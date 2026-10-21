//! Integration tests of `hutan-lsp-client` against the scripted fake server
//! (`src/bin/fake_lsp.rs`, built by Cargo as `hutan-fake-lsp`).

// Test helpers outside `#[test]` functions unwrap freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bounds;
mod common;
mod flows;
mod supervision;
