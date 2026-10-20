//! Integration tests of `hutan-syntax`. They exercise every grammar, so they only exist with the
//! `all-languages` feature (the default); a build with a subset of grammars compiles the library
//! alone.
#![cfg(feature = "all-languages")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod background;
#[cfg(feature = "buffer")]
mod buffer;
mod common;
mod highlights;
mod incremental;
mod smoke;
mod structure;
mod support;
mod vendored;
