//! Shared builders for the integration tests.

pub mod fake;

use hutan_container::{AbsPath, ContainerSpec, ImageRef, User};

pub fn image() -> ImageRef {
    ImageRef::new("ghcr.io/hutan/agent:1.0").unwrap()
}

pub fn abs(path: &str) -> AbsPath {
    AbsPath::new(path).unwrap()
}

pub fn user() -> User {
    User { uid: 501, gid: 20 }
}

pub fn minimal_spec() -> ContainerSpec {
    ContainerSpec::new(image(), "s1").unwrap()
}

pub fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}
