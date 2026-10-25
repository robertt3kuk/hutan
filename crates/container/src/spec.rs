//! What Hutan asks a runtime to run: validated value types and [`ContainerSpec`].
//!
//! Every value that ends up as one argv element is validated when it is constructed, so an
//! argument can never be mistaken for an option (`-`-prefixed names), split by a separator of
//! the `--mount` syntax (`,`), or smuggle a secret into the environment. Nothing here touches
//! the file system: host paths must already be canonical (the caller resolves `/var` →
//! `/private/var`, symlinks and `..`), which is the caller's contract and is stated on
//! [`AbsPath`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// A spec, policy or exec request that cannot be turned into a command line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error(
        "invalid name {0:?}: use letters, digits, `_`, `.` and `-`, starting with a letter or digit (at most 128 characters)"
    )]
    InvalidName(String),
    #[error("invalid image reference {image:?}: {reason}")]
    InvalidImage { image: String, reason: &'static str },
    #[error("invalid path {path:?}: {reason}")]
    InvalidPath { path: String, reason: &'static str },
    #[error("invalid label {key:?}: {reason}")]
    InvalidLabel { key: String, reason: &'static str },
    #[error("invalid environment variable {name:?}: {reason}")]
    InvalidEnv { name: String, reason: &'static str },
    #[error("the spec has no `{0}` label")]
    MissingLabel(&'static str),
    #[error("the container needs a command that keeps it alive")]
    EmptyCommand,
    #[error("an exec needs a command")]
    EmptyExec,
    #[error("two mounts use the container path {0:?}")]
    DuplicateMountTarget(String),
    #[error("refusing to mount {0:?}: a container runtime socket is root on the host")]
    ForbiddenMount(String),
    #[error("invalid resource limit: {0}")]
    InvalidLimit(&'static str),
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

macro_rules! name_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Letters, digits, `_`, `.`, `-`; the first character is a letter or digit, so the
            /// name can never be read as an option.
            pub fn new(name: impl Into<String>) -> Result<Self, SpecError> {
                let name = name.into();
                if valid_name(&name) { Ok(Self(name)) } else { Err(SpecError::InvalidName(name)) }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// A container's name or id as the runtime prints it (Apple: the name is the id).
    ContainerId
);
name_type!(
    /// A named volume.
    VolumeName
);

/// An absolute path used on the host and/or in the guest.
///
/// Rules: starts with `/`, is not `/`, has no empty, `.` or `..` component (no trailing or
/// doubled slash), and contains no control character, `,` or `"` (the `--mount` syntax is
/// comma separated and Docker parses it as CSV). Host paths must be **canonical** — Hutan
/// mounts a worktree at its own absolute path, and `/var` or `/tmp` on macOS are symlinks
/// the runtime would resolve differently — which this type cannot check without touching the
/// file system; the caller canonicalises first.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AbsPath(String);

impl AbsPath {
    pub fn new(path: impl Into<String>) -> Result<Self, SpecError> {
        let path = path.into();
        let reason = if !path.starts_with('/') {
            Some("not absolute")
        } else if path == "/" {
            Some("the root directory cannot be mounted")
        } else if path.chars().any(|c| c.is_control()) {
            Some("contains a control character")
        } else if path.contains(',') || path.contains('"') {
            Some("contains `,` or `\"`, which the mount syntax cannot carry")
        } else if path[1..]
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        {
            Some("has an empty, `.` or `..` component")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(SpecError::InvalidPath { path, reason }),
            None => Ok(Self(path)),
        }
    }

    pub fn from_path(path: &Path) -> Result<Self, SpecError> {
        match path.to_str() {
            Some(s) => Self::new(s),
            None => Err(SpecError::InvalidPath {
                path: path.to_string_lossy().into_owned(),
                reason: "not valid UTF-8",
            }),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Number of components (`/a/b` → 2).
    pub fn depth(&self) -> usize {
        self.0[1..].split('/').count()
    }

    /// Whether `self` lies strictly inside `other` (component-wise, not a string prefix).
    pub fn is_inside(&self, other: &AbsPath) -> bool {
        self.0.len() > other.0.len()
            && self.0.starts_with(other.0.as_str())
            && self.0.as_bytes()[other.0.len()] == b'/'
    }

    /// `self/name` for a plain file or directory name.
    pub fn join(&self, name: &str) -> Result<AbsPath, SpecError> {
        AbsPath::new(format!("{}/{}", self.0, name))
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A fully qualified image reference: `registry/path:tag` or `registry/path@sha256:…`.
///
/// The registry is required (first component has a `.` or `:` or is `localhost`) and so is a
/// tag or digest, so what runs is named exactly; Hutan never pulls implicitly (the argv
/// builders pass `--pull=never` where the runtime has it, and the adapter checks the image
/// exists before creating).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef(String);

impl ImageRef {
    pub fn new(image: impl Into<String>) -> Result<Self, SpecError> {
        let image = image.into();
        let reason = if image.is_empty() || image.len() > 512 {
            Some("empty or longer than 512 characters")
        } else if image.starts_with('-') {
            Some("starts with `-`")
        } else if !image.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@' | '+')
        }) {
            Some("contains a character outside letters, digits and `._-/:@+`")
        } else {
            let (registry, rest) = image.split_once('/').unwrap_or((&image, ""));
            let qualified = !rest.is_empty()
                && (registry.contains('.') || registry.contains(':') || registry == "localhost");
            let last = image.rsplit('/').next().unwrap_or("");
            let pinned = last.contains('@') || last.contains(':');
            if !qualified {
                Some("not fully qualified (needs a registry such as `docker.io/library/…`)")
            } else if !pinned {
                Some("has neither a tag nor a digest")
            } else {
                None
            }
        };
        match reason {
            Some(reason) => Err(SpecError::InvalidImage { image, reason }),
            None => Ok(Self(image)),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a bind mount's source is. Single files are flaky on Apple `container` (apple/container
/// #1251), which [`crate::RuntimeCaps::single_file_mount`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindKind {
    Directory,
    File,
}

/// One mount of a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    /// A host path at **the same absolute path** in the guest (the contract of a worktree:
    /// its `.git` file holds an absolute `gitdir:`).
    Bind {
        path: AbsPath,
        kind: BindKind,
        readonly: bool,
    },
    /// A named volume at `guest`.
    Volume {
        name: VolumeName,
        guest: AbsPath,
        readonly: bool,
    },
    /// Memory-backed scratch at `guest`.
    Tmpfs { guest: AbsPath },
}

impl Mount {
    /// Where the mount appears in the guest.
    pub fn guest(&self) -> &AbsPath {
        match self {
            Mount::Bind { path, .. } => path,
            Mount::Volume { guest, .. } => guest,
            Mount::Tmpfs { guest } => guest,
        }
    }

    pub fn is_readonly(&self) -> bool {
        match self {
            Mount::Bind { readonly, .. } | Mount::Volume { readonly, .. } => *readonly,
            Mount::Tmpfs { .. } => false,
        }
    }
}

/// Network of the container. Egress allowlists are a later decision (C-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// `--network none`.
    #[default]
    None,
    /// The runtime's default network.
    Open,
}

/// Numeric user and group the process runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
}

/// CPU and memory caps (`0` is rejected: it means "unlimited" to some runtimes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    pub cpus: Option<u32>,
    pub memory_mib: Option<u32>,
}

impl Limits {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.cpus == Some(0) {
            return Err(SpecError::InvalidLimit("cpus must be at least 1"));
        }
        if self.memory_mib == Some(0) {
            return Err(SpecError::InvalidLimit("memory must be at least 1 MiB"));
        }
        Ok(())
    }
}

/// Labels Hutan puts on a container. Keys are `hutan.*`, values a conservative charset, so
/// `list_hutan` can find them and `k=v` is never ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Labels(BTreeMap<String, String>);

/// Label every Hutan container carries.
pub const LABEL_MANAGED: &str = "hutan.managed";
/// Label naming the session a container belongs to; `ensure` is idempotent by it.
pub const LABEL_SESSION: &str = "hutan.session";

impl Labels {
    pub fn insert(&mut self, key: &str, value: &str) -> Result<(), SpecError> {
        let bad_key = |reason| SpecError::InvalidLabel {
            key: key.to_string(),
            reason,
        };
        let tail = key
            .strip_prefix("hutan.")
            .ok_or_else(|| bad_key("keys start with `hutan.`"))?;
        if tail.is_empty()
            || !tail.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
        {
            return Err(bad_key(
                "use lowercase letters, digits, `.`, `_`, `-` after `hutan.`",
            ));
        }
        if value.is_empty()
            || value.len() > 256
            || !value.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@' | '+')
            })
        {
            return Err(bad_key(
                "values are 1–256 characters of letters, digits and `._-:/@+`",
            ));
        }
        self.0.insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// In key order, so the command line is deterministic.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// One plain environment variable. There is no variant that carries a secret: names that look
/// like credentials are refused (the value would be visible in `inspect` and process listings;
/// ADR 0015 — Hutan never forwards a token into a container).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    name: String,
    value: String,
}

impl EnvVar {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Result<Self, SpecError> {
        let (name, value) = (name.into(), value.into());
        let bad = |reason| SpecError::InvalidEnv {
            name: name.clone(),
            reason,
        };
        let mut chars = name.chars();
        let first_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(bad(
                "names are letters, digits and `_`, not starting with a digit",
            ));
        }
        if value.contains('\0') {
            return Err(bad("the value contains NUL"));
        }
        let upper = name.to_ascii_uppercase();
        let secretish = [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "CREDENTIAL",
            "APIKEY",
            "API_KEY",
            "PRIVATE_KEY",
        ]
        .iter()
        .any(|needle| upper.contains(needle))
            || upper.ends_with("_KEY");
        if secretish {
            return Err(bad(
                "looks like a credential; credentials never go through the environment",
            ));
        }
        Ok(Self { name, value })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Everything needed to create one container. Defaults are the closed ones: no network, all
/// capabilities dropped, an init process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    pub image: ImageRef,
    pub name: ContainerId,
    pub labels: Labels,
    pub mounts: Vec<Mount>,
    pub network: NetworkPolicy,
    /// `None` = the image's user.
    pub user: Option<User>,
    pub workdir: Option<AbsPath>,
    pub env: Vec<EnvVar>,
    pub limits: Limits,
    pub root_read_only: bool,
    /// `--cap-drop ALL`.
    pub cap_drop_all: bool,
    pub init: bool,
    /// The container's own process; it must stay alive for `exec` to work.
    pub command: Vec<String>,
}

impl ContainerSpec {
    /// A spec for `session`: named `hutan-<session>`, labelled `hutan.managed` and
    /// `hutan.session`, running `sleep infinity`.
    pub fn new(image: ImageRef, session: &str) -> Result<Self, SpecError> {
        let name = ContainerId::new(format!("hutan-{session}"))?;
        let mut labels = Labels::default();
        labels.insert(LABEL_MANAGED, "true")?;
        labels.insert(LABEL_SESSION, session)?;
        Ok(Self {
            image,
            name,
            labels,
            mounts: Vec::new(),
            network: NetworkPolicy::None,
            user: None,
            workdir: None,
            env: Vec::new(),
            limits: Limits::default(),
            root_read_only: false,
            cap_drop_all: true,
            init: true,
            command: vec!["sleep".to_string(), "infinity".to_string()],
        })
    }

    /// Cross-field checks the constructors cannot do. Called by the `create` builder.
    pub fn validate(&self) -> Result<(), SpecError> {
        for key in [LABEL_MANAGED, LABEL_SESSION] {
            if self.labels.get(key).is_none() {
                return Err(SpecError::MissingLabel(key));
            }
        }
        if self.command.is_empty() || self.command.iter().any(|a| a.contains('\0')) {
            return Err(SpecError::EmptyCommand);
        }
        self.limits.validate()?;
        let mut targets = std::collections::BTreeSet::new();
        for mount in &self.mounts {
            if let Mount::Bind { path, .. } = mount {
                let file = path.as_str().rsplit('/').next().unwrap_or("");
                if matches!(file, "docker.sock" | "podman.sock") {
                    return Err(SpecError::ForbiddenMount(path.to_string()));
                }
            }
            if !targets.insert(mount.guest().as_str()) {
                return Err(SpecError::DuplicateMountTarget(mount.guest().to_string()));
            }
        }
        Ok(())
    }
}

/// How an `exec` is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecIo {
    /// `-it`: the daemon's PTY is the exec's terminal (terminals, interactive logins).
    Pty,
    /// `-i`: stdin/stdout are pipes (ACP, `codex app-server`, the relay of C-3).
    Stdio,
}

/// A command run inside a running container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    pub workdir: Option<AbsPath>,
    pub env: Vec<EnvVar>,
    pub user: Option<User>,
}

impl ExecSpec {
    pub fn new(argv: Vec<String>) -> Result<Self, SpecError> {
        if argv.is_empty() || argv[0].is_empty() || argv.iter().any(|a| a.contains('\0')) {
            return Err(SpecError::EmptyExec);
        }
        Ok(Self {
            argv,
            workdir: None,
            env: Vec::new(),
            user: None,
        })
    }
}

/// What the runtime says about a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    Exited { code: i32 },
    Gone,
}

/// One Hutan-labelled container found by `list_hutan` (the daemon-start reaper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerRef {
    pub id: ContainerId,
    pub labels: Labels,
    pub running: bool,
}
