//! Integration tests of `hutan-container`: argv builders, value validation, detection against
//! recorded vendor output (`tests/fixtures/SOURCES.md`) through a scripted `HostProbe`, and the
//! real `SystemProbe` against sockets and scripts the tests create themselves. No test touches
//! the machine's container runtimes.

// Test helpers outside `#[test]` functions unwrap freely.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod argv;
mod common;
mod detect_apple;
mod detect_docker;
mod policy;
mod runtime_trait;
mod spec;
mod system_probe;
