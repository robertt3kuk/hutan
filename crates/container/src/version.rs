//! Tolerant version numbers: runtimes print versions inside longer strings
//! (`container-apiserver version 1.2.3 (build: release, commit: 1234abc)`, `6.12.76-linuxkit`).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The first `N.N` or `N.N.N` in `text`; a missing patch is `0`, suffixes (`-rc1`,
    /// `-linuxkit`, `+rd1`) are ignored.
    pub fn find(text: &str) -> Option<Version> {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let (major, after_major) = number(bytes, i);
            i = after_major;
            let Some((minor, after_minor)) = dotted(bytes, after_major) else {
                continue;
            };
            let patch = dotted(bytes, after_minor).map_or(0, |(patch, _)| patch);
            if let Some(major) = major {
                return Some(Version {
                    major,
                    minor,
                    patch,
                });
            }
            i = after_minor;
        }
        None
    }
}

/// Digits starting at `start` → (value if it fits, index after the digits).
fn number(bytes: &[u8], start: usize) -> (Option<u32>, usize) {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let value = std::str::from_utf8(&bytes[start..end])
        .ok()
        .and_then(|s| s.parse().ok());
    (value, end)
}

/// `.N` at `at` → (N, index after).
fn dotted(bytes: &[u8], at: usize) -> Option<(u32, usize)> {
    if bytes.get(at) == Some(&b'.') && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
        let (value, end) = number(bytes, at + 1);
        value.map(|v| (v, end))
    } else {
        None
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn finds_the_first_dotted_number() {
        let v = Version::find;
        assert_eq!(v("1.6.1"), Some(Version::new(1, 6, 1)));
        assert_eq!(v("v27.3"), Some(Version::new(27, 3, 0)));
        assert_eq!(
            v("container-apiserver version 1.2.3 (build: release, commit: 1234abc)"),
            Some(Version::new(1, 2, 3))
        );
        assert_eq!(v("6.12.76-linuxkit"), Some(Version::new(6, 12, 76)));
        assert_eq!(
            v("Docker Desktop 4.67.0 (222858)"),
            Some(Version::new(4, 67, 0))
        );
        assert_eq!(v("27.0.1"), Some(Version::new(27, 0, 1)));
        assert_eq!(v("unspecified"), None);
        assert_eq!(v("42"), None);
        assert_eq!(v(""), None);
        // A lone number is skipped; the next dotted one wins.
        assert_eq!(v("build 7 of 1.2.3"), Some(Version::new(1, 2, 3)));
        // Too large for u32 is not a version.
        assert_eq!(v("99999999999.1.1"), None);
    }

    #[test]
    fn orders_numerically() {
        assert!(Version::new(1, 10, 0) > Version::new(1, 9, 9));
        assert!(Version::new(26, 0, 0) > Version::new(15, 9, 9));
        assert_eq!(Version::new(1, 6, 1).to_string(), "1.6.1");
    }
}
