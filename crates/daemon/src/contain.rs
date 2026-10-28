//! Contain: Hutan's own Seatbelt boundary around a session (protocol 0.52, capability
//! `sandbox`, ADR 0021 decision 3).
//!
//! For the sessions whose harness has no OS sandbox of its own — a plain terminal, a terminal
//! agent, an OMP chat — the daemon renders a Seatbelt profile from a typed [`Policy`] and starts
//! the process as `sandbox-exec -p <profile> -- <argv>`. `sandbox-exec` executes the target in
//! place, so the process keeps its pid, its process group and its PTY: signals, `hutan ps` and
//! the resource manager see no wrapper.
//!
//! What the profile does (`docs/agents.md` § Contain):
//! - **writes** only in the session's checkout, its repository's git directory (a linked
//!   worktree commits there; `hooks` and `config` stay read-only), the temp folders, the
//!   harness's own home (OMP) and the folders the user added;
//! - **reads** stay open, except credential folders, Keychain files and Hutan's own data folder
//!   (`shell-integration` and what the session itself writes are carved out): this is damage
//!   containment, not confidentiality;
//! - **Keychain**: the base is `(deny default)`, which closes `securityd`, and it is denied again
//!   explicitly;
//! - **network** all on or all off. Off closes every unix socket as well (a socket is how DNS
//!   leaks out); on keeps DNS working and still refuses Hutan's own sockets, so the agent cannot
//!   act as the user through the daemon (`SANDBOX-2B` gives a contained session a socket of its
//!   own).
//!
//! Seatbelt does not nest: a process inside this boundary cannot start a harness that applies
//! its own sandbox ([`nested_reason`]). A boundary that cannot be applied is a refusal, never a
//! session started uncontained: [`Sandbox::plan`] runs a self-test once and says why it failed.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use hutan_core::{Contain, ContainKind, Containment};
use hutan_protocol::control::{ErrorBody, ErrorCode};

use crate::config::SandboxSettings;
use crate::{placement, projects};

/// The macOS tool every Seatbelt wrapper goes through.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Folders under the user's home that hold credentials: unreadable inside the boundary unless
/// `[sandbox] read_deny` says otherwise.
pub const DEFAULT_READ_DENY: &[&str] = &[
    "~/Library/Keychains",
    "~/.ssh",
    "~/.aws",
    "~/.gnupg",
    "~/.config/gh",
    "~/.netrc",
];

/// Why a session could not be contained.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainError {
    /// The request cannot be honoured as asked: no folder, an unusable folder, a harness that
    /// runs its own sandbox.
    #[error("{0}")]
    Refused(String),
    /// The boundary cannot be applied on this machine; nothing is started without it.
    #[error("{0}")]
    Unavailable(String),
}

impl ContainError {
    /// The reason in a sentence, for `Containment.reason`.
    pub fn reason(&self) -> &str {
        match self {
            Self::Refused(reason) | Self::Unavailable(reason) => reason,
        }
    }
}

impl From<ContainError> for ErrorBody {
    fn from(error: ContainError) -> Self {
        let code = match &error {
            ContainError::Refused(_) => ErrorCode::BadRequest,
            ContainError::Unavailable(_) => ErrorCode::Unsupported,
        };
        ErrorBody::new(code, error.to_string())
    }
}

/// What a boundary allows: the typed policy a profile is rendered from. Every path is absolute
/// and canonical — Seatbelt matches real paths, so `/tmp/x` would not match `/private/tmp/x`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Policy {
    /// Folders the session may write to.
    pub write_roots: Vec<PathBuf>,
    /// Paths inside `write_roots` that stay read-only (git hooks and config).
    pub write_deny: Vec<PathBuf>,
    /// Folders the session may not read.
    pub read_deny: Vec<PathBuf>,
    /// Paths inside `read_deny` that stay readable (shell integration, the harness's home).
    pub read_keep: Vec<PathBuf>,
    /// Unix sockets under these paths cannot be connected to even with the network on.
    pub socket_deny: Vec<PathBuf>,
    /// The session may use the network.
    pub network: bool,
}

/// A path as an SBPL string literal: quoted, with `\` and `"` escaped.
fn literal(path: &Path) -> Result<String, ContainError> {
    let text = path.to_str().ok_or_else(|| {
        ContainError::Refused(format!(
            "`{}` is not valid UTF-8, so it cannot be named in a Seatbelt profile",
            path.display()
        ))
    })?;
    if text.contains('\0') || text.contains('\n') {
        return Err(ContainError::Refused(format!(
            "`{}` has a control character in it",
            path.display()
        )));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn subpaths(paths: &[PathBuf]) -> Result<String, ContainError> {
    let mut out = String::new();
    for path in paths {
        let _ = write!(out, " (subpath {})", literal(path)?);
    }
    Ok(out)
}

impl Policy {
    /// The Seatbelt profile. Later rules win, so the denies come after the allows they cut
    /// holes into and the carve-outs after the denies.
    pub fn profile(&self) -> Result<String, ContainError> {
        let mut p = String::from(
            "(version 1)\n\
             ; Hutan Contain (ADR 0021): deny by default, then what a session needs.\n\
             (deny default)\n\
             (import \"system.sb\")\n\
             (allow process-exec) (allow process-fork)\n\
             (allow signal (target same-sandbox))\n\
             (allow process-info* (target same-sandbox))\n\
             (allow system-sched)\n\
             (allow sysctl-read) (allow ipc-posix-shm) (allow user-preference-read)\n\
             (allow pseudo-tty) (allow file-ioctl)\n\
             (allow file-read*)\n\
             (allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\") \
             (literal \"/dev/dtracehelper\") (literal \"/dev/ptmx\") \
             (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/[0-9]+$\"))\n",
        );
        if !self.write_roots.is_empty() {
            let _ = writeln!(p, "(allow file-write*{})", subpaths(&self.write_roots)?);
        }
        if !self.write_deny.is_empty() {
            let _ = writeln!(p, "(deny file-write*{})", subpaths(&self.write_deny)?);
        }
        if !self.read_deny.is_empty() {
            let _ = writeln!(p, "(deny file-read*{})", subpaths(&self.read_deny)?);
            if !self.read_keep.is_empty() {
                let _ = writeln!(p, "(allow file-read*{})", subpaths(&self.read_keep)?);
            }
        }
        p.push_str(
            "(deny mach-lookup (global-name \"com.apple.SecurityServer\") \
             (global-name \"com.apple.securityd\") (global-name \"com.apple.secd\"))\n",
        );
        if self.network {
            p.push_str("(allow network*)\n");
            for path in &self.socket_deny {
                let _ = writeln!(
                    p,
                    "(deny network* (remote unix-socket (subpath {})))",
                    literal(path)?
                );
            }
        }
        Ok(p)
    }
}

/// What a harness needs beyond the session's checkout (OMP's home), found from how it is
/// started.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fragment {
    /// Writable folders (created when missing).
    pub write: Vec<PathBuf>,
    /// Paths inside `write` that stay read-only: the harness's settings, extensions, hooks.
    pub write_deny: Vec<PathBuf>,
}

impl Fragment {
    /// OMP's: its home (`~/.omp`, and the account's agent directory `PI_CODING_AGENT_DIR`)
    /// holds sessions, logs, caches and the databases its login is refreshed in, so it is
    /// writable; its settings, extensions, hooks, skills and commands are not, because a
    /// writable one would let the agent change what the **next, uncontained** launch runs.
    pub fn omp(user_home: Option<&Path>, env: &[(String, String)]) -> Self {
        let mut write = Vec::new();
        let mut homes = Vec::new();
        if let Some(home) = user_home {
            homes.push(home.join(".omp/agent"));
            write.push(home.join(".omp"));
        }
        if let Some((_, dir)) = env.iter().find(|(name, _)| name == "PI_CODING_AGENT_DIR")
            && !dir.is_empty()
        {
            homes.push(PathBuf::from(dir));
            write.push(PathBuf::from(dir));
        }
        let write_deny = homes
            .iter()
            .flat_map(|dir| {
                ["config.yml", "extensions", "hooks", "skills", "commands"]
                    .map(|name| dir.join(name))
            })
            .collect();
        Self { write, write_deny }
    }
}

/// A boundary ready to wrap a spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    bin: PathBuf,
    profile: String,
    /// What the session will say of itself: read from the policy that is applied.
    pub containment: Containment,
}

impl Plan {
    /// The program and arguments that run `program args…` inside the boundary.
    pub fn wrap(&self, program: &OsStr, args: &[String]) -> (PathBuf, Vec<String>) {
        let mut wrapped = vec![
            "-p".to_owned(),
            self.profile.clone(),
            "--".to_owned(),
            program.to_string_lossy().into_owned(),
        ];
        wrapped.extend_from_slice(args);
        (self.bin.clone(), wrapped)
    }

    /// The argv prefix a terminal puts before its command line.
    pub fn prefix(&self) -> Vec<String> {
        vec![
            self.bin.to_string_lossy().into_owned(),
            "-p".to_owned(),
            self.profile.clone(),
            "--".to_owned(),
        ]
    }

    /// The Seatbelt profile the process runs under.
    pub fn profile(&self) -> &str {
        &self.profile
    }
}

/// The daemon's Contain service: settings, the places it must keep a session out of, and the
/// result of the self-test.
#[derive(Debug)]
pub struct Sandbox {
    bin: PathBuf,
    network: bool,
    extra_write: Vec<PathBuf>,
    read_deny: Vec<PathBuf>,
    user_home: Option<PathBuf>,
    /// Hutan's data folder and the daemon's socket: unreadable and unreachable.
    base: PathBuf,
    socket: PathBuf,
    temp: Vec<PathBuf>,
    status: OnceLock<Result<(), String>>,
}

/// `~` and `~/x` against the user's home directory.
fn expand(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    match text.strip_prefix('~') {
        Some("") => home.map(Path::to_path_buf),
        Some(rest) => rest
            .strip_prefix('/')
            .and_then(|rest| home.map(|home| home.join(rest))),
        None => Some(PathBuf::from(text)),
    }
}

/// `path` with symlinks resolved; a path that does not exist (yet) is resolved through its
/// nearest existing ancestor, so a rule for a folder created later still names the real place.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if parent != path => canonical(parent).join(name),
        _ => path.to_path_buf(),
    }
}

impl Sandbox {
    /// `base` is Hutan's data folder, `socket` the daemon's socket.
    pub fn new(
        settings: &SandboxSettings,
        base: &Path,
        socket: &Path,
        user_home: Option<&Path>,
    ) -> Self {
        let expand_all = |items: &[String]| -> Vec<PathBuf> {
            items
                .iter()
                .filter_map(|item| expand(item, user_home))
                .filter(|path| path.is_absolute())
                .map(|path| canonical(&path))
                .collect()
        };
        let mut temp = vec![
            canonical(&std::env::temp_dir()),
            canonical(Path::new("/tmp")),
            canonical(Path::new("/var/tmp")),
        ];
        temp.sort();
        temp.dedup();
        Self {
            bin: settings
                .bin
                .as_ref()
                .filter(|bin| !bin.is_empty())
                .map_or_else(|| PathBuf::from(SANDBOX_EXEC), PathBuf::from),
            network: settings.network,
            extra_write: expand_all(&settings.extra_write),
            read_deny: expand_all(&settings.read_deny),
            user_home: user_home.map(Path::to_path_buf),
            base: canonical(base),
            socket: canonical(socket),
            temp,
            status: OnceLock::new(),
        }
    }

    /// Runs the self-test now when it has not run. Blocking (it starts two short processes);
    /// the daemon calls it at start from a blocking task so that the first contained session
    /// does not pay for it.
    pub fn warm_up(&self) {
        if let Err(reason) = self.available() {
            tracing::warn!(%reason, "Contain is unavailable: contained sessions are refused");
        }
    }

    /// Whether the boundary can be applied on this machine, and if not why not.
    pub fn available(&self) -> Result<(), String> {
        self.status
            .get_or_init(|| self_test(&self.bin, &self.temp))
            .clone()
    }

    /// The boundary for a session working in `cwd`.
    ///
    /// `fragment` is what the harness needs beyond the checkout (OMP's home). Errors:
    /// [`ContainError::Refused`] for a folder that does not exist, [`ContainError::Unavailable`]
    /// when the self-test failed.
    pub fn plan(
        &self,
        cwd: &Path,
        request: &Contain,
        fragment: &Fragment,
    ) -> Result<Plan, ContainError> {
        self.available().map_err(|reason| {
            ContainError::Unavailable(format!(
                "Contain is not available: {reason}. The session was not started uncontained."
            ))
        })?;
        let cwd = cwd.canonicalize().map_err(|error| {
            ContainError::Refused(format!(
                "Contain needs the folder the session works in: {}: {error}",
                cwd.display()
            ))
        })?;
        if !cwd.is_dir() {
            return Err(ContainError::Refused(format!(
                "{} is not a folder",
                cwd.display()
            )));
        }
        let root = projects::git_root(&cwd).map_or_else(|| cwd.clone(), |root| canonical(&root));
        let mut write_roots = vec![root.clone()];
        let mut write_deny = Vec::new();
        if let Some(repository) = placement::repository_of(&root) {
            write_deny.push(repository.common_dir.join("hooks"));
            write_deny.push(repository.common_dir.join("config"));
            write_roots.push(repository.common_dir);
        }
        write_roots.extend(self.temp.iter().cloned());
        write_roots.extend(self.extra_write.iter().cloned());
        for path in &request.extra_write {
            if !path.is_absolute() {
                return Err(ContainError::Refused(format!(
                    "`extra_write` takes absolute folders: {}",
                    path.display()
                )));
            }
            let real = path.canonicalize().map_err(|error| {
                ContainError::Refused(format!("`extra_write` {}: {error}", path.display()))
            })?;
            write_roots.push(real);
        }
        for dir in &fragment.write {
            // A harness home that does not exist yet is made so that the rule names a real
            // folder; nothing is written into it.
            let _ = std::fs::create_dir_all(dir);
            write_roots.push(canonical(dir));
        }
        write_deny.extend(fragment.write_deny.iter().map(|path| canonical(path)));
        write_roots = dedup(write_roots);
        write_deny = dedup(write_deny);

        // Credentials and Hutan's own folder cannot be read; what the session itself works
        // with stays readable.
        let mut read_deny = self.read_deny.clone();
        read_deny.push(self.base.clone());
        let mut read_keep = write_roots.clone();
        read_keep.push(self.base.join("shell-integration"));
        let policy = Policy {
            write_roots: write_roots.clone(),
            write_deny,
            read_deny: dedup(read_deny),
            read_keep: dedup(read_keep),
            socket_deny: dedup(vec![self.base.clone(), self.socket.clone()]),
            network: request.network.unwrap_or(self.network),
        };
        let profile = policy.profile()?;
        // The folders a person reads in the chip and in `hutan ps`: the session's own, not
        // the plumbing (temp, harness home).
        let shown = std::iter::once(root.clone())
            .chain(
                write_roots
                    .iter()
                    .filter(|path| {
                        !self.temp.contains(path)
                            && !fragment.write.iter().any(|dir| canonical(dir) == **path)
                    })
                    .cloned(),
            )
            .collect::<Vec<_>>();
        Ok(Plan {
            bin: self.bin.clone(),
            profile,
            containment: Containment {
                kind: ContainKind::Seatbelt,
                network: policy.network,
                write_roots: dedup(shown),
                applied: true,
                reason: None,
            },
        })
    }

    /// The user's home directory, for the harness fragments.
    pub fn user_home(&self) -> Option<&Path> {
        self.user_home.as_deref()
    }

    /// Why `path` (canonical) cannot be a session folder (protocol 0.57): it is Hutan's own
    /// data folder or below it, or it is, or lies below, a folder `[sandbox] read_deny` keeps
    /// out of reach (credentials). `None` when it may be one. The rule holds for every
    /// session, contained or not: a folder the boundary would hide is not given to an agent.
    pub fn refuses_folder(&self, path: &Path) -> Option<String> {
        if path.starts_with(&self.base) {
            return Some(format!(
                "{} is Hutan's own data folder",
                self.base.display()
            ));
        }
        self.read_deny
            .iter()
            .find(|denied| path.starts_with(denied))
            .map(|denied| {
                format!(
                    "{} is kept out of reach (`[sandbox] read_deny`: {})",
                    path.display(),
                    denied.display()
                )
            })
    }
}

fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// A contained `/usr/bin/true` must succeed, a write inside the boundary must land, and a write
/// outside it must be refused: the three things a session relies on.
fn self_test(bin: &Path, temp: &[PathBuf]) -> Result<(), String> {
    if !bin.is_file() {
        return Err(format!(
            "{} is missing (Seatbelt is part of macOS)",
            bin.display()
        ));
    }
    let base = temp
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = base.join(format!(
        "hutan-contain-selftest-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let (inside, outside) = (dir.join("inside"), dir.join("outside"));
    std::fs::create_dir_all(&inside)
        .and_then(|()| std::fs::create_dir_all(&outside))
        .map_err(|error| format!("preparing the self-test in {}: {error}", dir.display()))?;
    let result = (|| {
        let inside = canonical(&inside);
        let outside = canonical(&outside);
        let policy = Policy {
            write_roots: vec![inside.clone()],
            ..Policy::default()
        };
        let profile = policy.profile().map_err(|error| error.to_string())?;
        let run = |argv: &[&str]| {
            Command::new(bin)
                .arg("-p")
                .arg(&profile)
                .arg("--")
                .args(argv)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|error| format!("running {}: {error}", bin.display()))
        };
        let out = run(&["/usr/bin/true"])?;
        if !out.status.success() {
            return Err(format!(
                "{} could not run a command ({}): {}",
                bin.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let probe = |dir: &Path| {
            let target = dir.join("probe");
            run(&[
                "/bin/sh",
                "-c",
                "echo x > \"$0\"",
                &target.to_string_lossy(),
            ])
            .map(|out| (out.status.success(), target))
        };
        let (wrote, target) = probe(&inside)?;
        if !wrote || !target.is_file() {
            return Err("a write inside the boundary was refused".to_owned());
        }
        let (wrote, target) = probe(&outside)?;
        if wrote || target.exists() {
            return Err("a write outside the boundary was not refused".to_owned());
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Why `argv` starts a harness that applies its own OS sandbox, which cannot run inside this
/// boundary (Seatbelt does not nest: `sandbox_apply: Operation not permitted`). Best effort: it
/// reads the command line the session starts with, not what a shell starts later.
pub fn nested_reason(argv: &[String]) -> Option<String> {
    let program = Path::new(argv.first()?).file_name()?.to_str()?;
    let args = &argv[1..];
    let value_of = |names: &[&str]| {
        args.iter()
            .enumerate()
            .find(|(_, arg)| names.contains(&arg.as_str()))
            .and_then(|(i, _)| args.get(i + 1))
            .map(String::as_str)
    };
    match program {
        "codex" => {
            let open = args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--dangerously-bypass-approvals-and-sandbox" | "--yolo"
                )
            }) || value_of(&["-s", "--sandbox"]) == Some("danger-full-access");
            (!open).then(|| {
                "Codex applies its own Seatbelt sandbox (workspace-write unless told otherwise), \
                 and Seatbelt does not nest. Use Codex's own Contained preset, or run it with \
                 `-s danger-full-access` to let Contain be the boundary"
                    .to_owned()
            })
        }
        "claude" => {
            let sandboxed = value_of(&["--settings"]).is_some_and(|settings| {
                serde_json::from_str::<serde_json::Value>(settings)
                    .ok()
                    .and_then(|v| v.pointer("/sandbox/enabled")?.as_bool())
                    .unwrap_or(false)
            });
            sandboxed.then(|| {
                "this Claude Code session turns its own Bash sandbox on, and Seatbelt does not \
                 nest. Use Claude Code's own Contained preset, or leave its sandbox off to let \
                 Contain be the boundary"
                    .to_owned()
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            write_roots: vec!["/work/repo".into(), "/private/tmp".into()],
            write_deny: vec!["/work/repo/.git/hooks".into()],
            read_deny: vec!["/Users/x/.ssh".into(), "/hutan".into()],
            read_keep: vec!["/hutan/shell-integration".into()],
            socket_deny: vec!["/hutan".into()],
            network: false,
        }
    }

    #[test]
    fn the_profile_is_snapshot_stable() {
        let profile = policy().profile().unwrap();
        assert_eq!(
            profile,
            r##"(version 1)
; Hutan Contain (ADR 0021): deny by default, then what a session needs.
(deny default)
(import "system.sb")
(allow process-exec) (allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow system-sched)
(allow sysctl-read) (allow ipc-posix-shm) (allow user-preference-read)
(allow pseudo-tty) (allow file-ioctl)
(allow file-read*)
(allow file-write* (literal "/dev/null") (literal "/dev/tty") (literal "/dev/dtracehelper") (literal "/dev/ptmx") (regex #"^/dev/ttys[0-9]+$") (regex #"^/dev/fd/[0-9]+$"))
(allow file-write* (subpath "/work/repo") (subpath "/private/tmp"))
(deny file-write* (subpath "/work/repo/.git/hooks"))
(deny file-read* (subpath "/Users/x/.ssh") (subpath "/hutan"))
(allow file-read* (subpath "/hutan/shell-integration"))
(deny mach-lookup (global-name "com.apple.SecurityServer") (global-name "com.apple.securityd") (global-name "com.apple.secd"))
"##
        );
        let on = Policy {
            network: true,
            ..policy()
        }
        .profile()
        .unwrap();
        assert!(on.ends_with(
            "(allow network*)\n(deny network* (remote unix-socket (subpath \"/hutan\")))\n"
        ));
    }

    #[test]
    fn paths_are_escaped_in_the_profile() {
        let policy = Policy {
            write_roots: vec![PathBuf::from("/a \"b\"\\c")],
            ..Policy::default()
        };
        assert!(
            policy
                .profile()
                .unwrap()
                .contains(r#"(subpath "/a \"b\"\\c")"#)
        );
        let bad = Policy {
            write_roots: vec![PathBuf::from("/a\nb")],
            ..Policy::default()
        };
        assert!(bad.profile().is_err());
    }

    #[test]
    fn tilde_expands_against_the_user_home_only() {
        let home = Path::new("/Users/me");
        assert_eq!(expand("~", Some(home)), Some(home.to_path_buf()));
        assert_eq!(
            expand("~/.ssh", Some(home)),
            Some(PathBuf::from("/Users/me/.ssh"))
        );
        assert_eq!(expand("~/.ssh", None), None);
        assert_eq!(expand("~other/x", Some(home)), None);
        assert_eq!(expand("/abs", None), Some(PathBuf::from("/abs")));
    }

    #[test]
    fn omp_gets_its_home_writable_and_its_settings_not() {
        let fragment = Fragment::omp(
            Some(Path::new("/Users/me")),
            &[(
                "PI_CODING_AGENT_DIR".into(),
                "/hutan/accounts/omp/work".into(),
            )],
        );
        assert_eq!(
            fragment.write,
            [
                PathBuf::from("/Users/me/.omp"),
                PathBuf::from("/hutan/accounts/omp/work")
            ]
        );
        for dir in ["/Users/me/.omp/agent", "/hutan/accounts/omp/work"] {
            for name in ["config.yml", "extensions", "hooks"] {
                assert!(
                    fragment.write_deny.contains(&Path::new(dir).join(name)),
                    "{dir}/{name}"
                );
            }
        }
    }

    #[test]
    fn harnesses_with_their_own_sandbox_are_refused() {
        let argv = |items: &[&str]| items.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(nested_reason(&argv(&["codex"])).is_some());
        assert!(nested_reason(&argv(&["/opt/bin/codex", "-s", "workspace-write"])).is_some());
        assert!(nested_reason(&argv(&["codex", "-s", "danger-full-access"])).is_none());
        assert!(
            nested_reason(&argv(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox"
            ]))
            .is_none()
        );
        assert!(nested_reason(&argv(&["claude"])).is_none());
        assert!(
            nested_reason(&argv(&[
                "claude",
                "--settings",
                r#"{"sandbox":{"enabled":true}}"#
            ]))
            .is_some()
        );
        assert!(nested_reason(&argv(&["omp"])).is_none());
        assert!(nested_reason(&[]).is_none());
    }
}
