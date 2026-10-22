//! Which daemon answers, compared with the `hutand` this client would start
//! (`docs/architecture.md` § Daemon lifecycle; protocol 0.42, capability `daemon.identity`).
//!
//! A client starts the `hutand` beside its own executable; the build id (version and git
//! commit) is compiled into both (`build.rs`). Whatever else holds the socket — a daemon of an
//! older or newer build, one from another install or a development checkout — is told apart
//! by [`compare`], and the client decides what to do about it: the desktop restarts an idle
//! daemon of another build of its own install and asks otherwise, `hutan daemon status` says
//! it.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

use hutan_protocol::messages::Welcome;

/// The version this client was built as.
pub const OWN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The git commit this client was built from, when it was built in a checkout.
pub fn own_git_sha() -> Option<&'static str> {
    Some(env!("HUTAN_GIT_SHA")).filter(|sha| !sha.is_empty())
}

/// A daemon build: its version, commit and executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonIdentity {
    pub version: String,
    /// `None` for a build outside git, and for a daemon older than protocol 0.42.
    pub git_sha: Option<String>,
    /// The executable, symlinks resolved; `None` when unknown (a daemon older than protocol
    /// 0.42; a client whose `hutand` is looked up on `PATH`).
    pub exe_path: Option<PathBuf>,
    /// When the daemon started (Unix ms); `None` for a build that does not run.
    pub started_at_ms: Option<u64>,
}

impl DaemonIdentity {
    /// The daemon behind a `welcome`.
    pub fn of(welcome: &Welcome) -> Self {
        match &welcome.daemon_build {
            Some(build) => Self {
                version: build.version.clone(),
                git_sha: build.git_sha.clone(),
                exe_path: Some(build.exe_path.clone()).filter(|p| !p.as_os_str().is_empty()),
                started_at_ms: Some(build.started_at_ms),
            },
            None => Self {
                version: welcome.daemon_version.clone(),
                git_sha: None,
                exe_path: None,
                started_at_ms: None,
            },
        }
    }

    /// The daemon this client starts: `hutand` at `exe` (symlinks resolved when it exists) of
    /// this client's own build.
    pub fn own(exe: Option<&Path>) -> Self {
        Self {
            version: OWN_VERSION.to_owned(),
            git_sha: own_git_sha().map(str::to_owned),
            exe_path: exe
                .filter(|exe| exe.is_absolute())
                .map(|exe| exe.canonicalize().unwrap_or_else(|_| exe.to_owned())),
            started_at_ms: None,
        }
    }

    /// `0.2.0`, or `0.2.0 (0123456789ab)` with the commit.
    pub fn build_text(&self) -> String {
        match &self.git_sha {
            Some(sha) => format!("{} ({sha})", self.version),
            None => self.version.clone(),
        }
    }
}

impl fmt::Display for DaemonIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.build_text())?;
        if let Some(path) = &self.exe_path {
            write!(f, " from {}", path.display())?;
        }
        Ok(())
    }
}

/// How the running daemon differs from the one this client starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// It runs from another executable: another install, a development build.
    Foreign { path: PathBuf },
    /// It is this install's daemon, but of another build: older, or the same version from
    /// another commit. Restarting it starts this client's build.
    Different,
    /// It is a newer build than this client: the client is the one to update.
    Newer,
}

/// What differs between the running daemon and `own`; `None` when it is the same build (or
/// nothing tells them apart). A daemon from another executable is [`Mismatch::Foreign`]
/// whatever its version.
pub fn compare(own: &DaemonIdentity, running: &DaemonIdentity) -> Option<Mismatch> {
    if let (Some(own_path), Some(path)) = (&own.exe_path, &running.exe_path)
        && own_path != path
    {
        return Some(Mismatch::Foreign { path: path.clone() });
    }
    match compare_versions(&running.version, &own.version) {
        Ordering::Greater => Some(Mismatch::Newer),
        Ordering::Less => Some(Mismatch::Different),
        Ordering::Equal => match (&running.git_sha, &own.git_sha) {
            (Some(running), Some(own)) if running != own => Some(Mismatch::Different),
            _ => None,
        },
    }
}

/// Orders `major.minor.patch[-pre]` versions; a pre-release sorts before its release, and
/// what does not parse compares as text.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parse(version: &str) -> Option<([u64; 3], Option<&str>)> {
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version, None),
        };
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let numbers = [parts.next()??, parts.next()??, parts.next()??];
        parts.next().is_none().then_some((numbers, pre))
    }
    match (parse(a), parse(b)) {
        (Some((a_core, a_pre)), Some((b_core, b_pre))) => {
            a_core.cmp(&b_core).then_with(|| match (a_pre, b_pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
        }
        _ => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(version: &str, sha: Option<&str>, path: Option<&str>) -> DaemonIdentity {
        DaemonIdentity {
            version: version.to_owned(),
            git_sha: sha.map(str::to_owned),
            exe_path: path.map(PathBuf::from),
            started_at_ms: None,
        }
    }

    const APP: &str = "/Applications/Hutan.app/Contents/MacOS/hutand";

    #[test]
    fn versions_order_by_number_then_pre_release() {
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("dev", "dev"), Ordering::Equal);
    }

    #[test]
    fn the_same_build_at_the_same_path_matches() {
        let own = build("0.2.0", Some("aaaaaaaaaaaa"), Some(APP));
        assert_eq!(compare(&own, &own.clone()), None);
        // An older daemon says only its version: the same version is the same build.
        assert_eq!(compare(&own, &build("0.2.0", None, None)), None);
    }

    #[test]
    fn another_build_of_this_install_is_different_or_newer() {
        let own = build("0.2.0", Some("aaaaaaaaaaaa"), Some(APP));
        assert_eq!(
            compare(&own, &build("0.1.0", Some("bbbbbbbbbbbb"), Some(APP))),
            Some(Mismatch::Different)
        );
        // A daemon from before protocol 0.42 (no path) is judged by its version.
        assert_eq!(
            compare(&own, &build("0.1.0", None, None)),
            Some(Mismatch::Different)
        );
        assert_eq!(
            compare(&own, &build("0.2.0", Some("bbbbbbbbbbbb"), Some(APP))),
            Some(Mismatch::Different)
        );
        assert_eq!(
            compare(&own, &build("0.3.0", None, Some(APP))),
            Some(Mismatch::Newer)
        );
    }

    #[test]
    fn a_daemon_from_another_executable_is_foreign_whatever_its_version() {
        let own = build("0.2.0", Some("aaaaaaaaaaaa"), Some(APP));
        let dev = "/Users/me/hutan/target/debug/hutand";
        assert_eq!(
            compare(&own, &build("0.2.0", Some("aaaaaaaaaaaa"), Some(dev))),
            Some(Mismatch::Foreign { path: dev.into() })
        );
        // Without a path of its own the client cannot tell.
        let unknown = build("0.2.0", Some("aaaaaaaaaaaa"), None);
        assert_eq!(
            compare(&unknown, &build("0.2.0", Some("aaaaaaaaaaaa"), Some(dev))),
            None
        );
    }
}
