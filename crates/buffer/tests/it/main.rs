//! Property and scenario tests of `hutan-buffer` through its public API.

// Test helpers outside `#[test]` functions unwrap freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod anchors;
mod common;
mod coords;
mod external;
mod files;
mod model;
mod search;
mod undo;
