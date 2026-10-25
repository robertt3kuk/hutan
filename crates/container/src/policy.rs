//! Policy → [`ContainerSpec`], failing closed.
//!
//! A [`ContainerPolicy`] is what the daemon wants for one contained session (spike § 4.3): the
//! worktree at its own path, the repo's git directory with `hooks/` read-only, scratch volumes,
//! a network, a non-root user. [`ContainerPolicy::plan`] turns it into a spec **only** for a
//! runtime that is running and whose [`RuntimeCaps`] can honour every part of it; otherwise it
//! returns a typed error naming what is missing and the session must not start. Nothing is
//! degraded silently (ADR 0021 decision 3).

use crate::caps::{Boundary, CapsError};
use crate::detect::{RuntimeReport, RuntimeState};
use crate::kind::RuntimeId;
use crate::spec::{
    AbsPath, BindKind, ContainerSpec, EnvVar, ImageRef, Labels, Limits, Mount, NetworkPolicy,
    SpecError, User, VolumeName,
};

/// A named volume mounted at `guest` (build output such as `target/`, a harness home).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    pub name: VolumeName,
    pub guest: AbsPath,
}

#[derive(Debug, Clone)]
pub struct ContainerPolicy {
    /// Session id: names the container (`hutan-<session>`) and labels it.
    pub session: String,
    pub image: ImageRef,
    /// The checkout or worktree: bind-mounted read-write at its own absolute path, and the
    /// working directory unless `workdir` says otherwise.
    pub worktree: AbsPath,
    /// The repository's (common) git directory, as the worktree's `.git` points to it. Mounted
    /// read-write at its own path when it lies outside the worktree (a linked worktree).
    pub git_dir: Option<AbsPath>,
    /// With a `git_dir`: mount `<git_dir>/hooks` read-only on top and set `GIT_CONFIG_*` so that
    /// `core.hooksPath` is `/dev/null` in the container (a hook planted by a contained agent
    /// would otherwise run later on the host, unsandboxed; spike § 4.3 — the env part is
    /// `[INFERENCE]` there). `<git_dir>/hooks` must exist. Needs `nested_mounts`.
    pub protect_git_hooks: bool,
    /// More host paths mounted read-only at their own paths.
    pub read_only_mounts: Vec<AbsPath>,
    pub volumes: Vec<VolumeMount>,
    pub network: NetworkPolicy,
    /// Must not be root (uid or gid 0): Claude Code refuses to start as root and a non-root user
    /// is part of the boundary.
    pub user: User,
    pub workdir: Option<AbsPath>,
    pub env: Vec<EnvVar>,
    pub limits: Limits,
    pub root_read_only: bool,
    /// Extra labels (`hutan.project`, `hutan.account`, …).
    pub labels: Labels,
    /// Refuse runtimes whose isolation is weaker than this.
    pub require_boundary: Option<Boundary>,
}

impl ContainerPolicy {
    /// The closed defaults: no network, hooks protected, no boundary requirement.
    pub fn new(session: impl Into<String>, image: ImageRef, worktree: AbsPath, user: User) -> Self {
        Self {
            session: session.into(),
            image,
            worktree,
            git_dir: None,
            protect_git_hooks: true,
            read_only_mounts: Vec::new(),
            volumes: Vec::new(),
            network: NetworkPolicy::None,
            user,
            workdir: None,
            env: Vec::new(),
            limits: Limits::default(),
            root_read_only: false,
            labels: Labels::default(),
            require_boundary: None,
        }
    }

    /// The spec this policy asks for, independent of any runtime.
    pub fn to_spec(&self) -> Result<ContainerSpec, PolicyError> {
        if self.user.uid == 0 || self.user.gid == 0 {
            return Err(PolicyError::RootUser);
        }
        let mut spec = ContainerSpec::new(self.image.clone(), &self.session)?;
        for (key, value) in self.labels.iter() {
            spec.labels.insert(key, value)?;
        }
        spec.mounts.push(Mount::Bind {
            path: self.worktree.clone(),
            kind: BindKind::Directory,
            readonly: false,
        });
        if let Some(git_dir) = &self.git_dir {
            if !git_dir.is_inside(&self.worktree) && *git_dir != self.worktree {
                spec.mounts.push(Mount::Bind {
                    path: git_dir.clone(),
                    kind: BindKind::Directory,
                    readonly: false,
                });
            }
            if self.protect_git_hooks {
                spec.mounts.push(Mount::Bind {
                    path: git_dir.join("hooks")?,
                    kind: BindKind::Directory,
                    readonly: true,
                });
                for (key, value) in [
                    ("GIT_CONFIG_COUNT", "1"),
                    ("GIT_CONFIG_KEY_0", "core.hooksPath"),
                    ("GIT_CONFIG_VALUE_0", "/dev/null"),
                ] {
                    spec.env.push(EnvVar::new(key, value)?);
                }
            }
        }
        for path in &self.read_only_mounts {
            spec.mounts.push(Mount::Bind {
                path: path.clone(),
                kind: BindKind::Directory,
                readonly: true,
            });
        }
        for volume in &self.volumes {
            spec.mounts.push(Mount::Volume {
                name: volume.name.clone(),
                guest: volume.guest.clone(),
                readonly: false,
            });
        }
        spec.network = self.network;
        spec.user = Some(self.user);
        spec.workdir = Some(
            self.workdir
                .clone()
                .unwrap_or_else(|| self.worktree.clone()),
        );
        spec.env.extend(self.env.iter().cloned());
        spec.limits = self.limits;
        spec.root_read_only = self.root_read_only;
        spec.validate()?;
        Ok(spec)
    }

    /// The spec for `runtime`, or the reason it cannot be had. Fail-closed: a runtime that is
    /// not [`RuntimeState::Running`], or whose caps lack anything the policy needs, is refused.
    pub fn plan(&self, runtime: &RuntimeReport) -> Result<ContainerSpec, PolicyError> {
        if runtime.state != RuntimeState::Running {
            return Err(PolicyError::NotRunning {
                runtime: runtime.id.clone(),
                state: runtime.state.clone(),
            });
        }
        let spec = self.to_spec()?;
        let missing = runtime.caps.missing_for(&spec, self.require_boundary);
        if !missing.is_empty() {
            return Err(PolicyError::Caps(CapsError {
                runtime: runtime.id.clone(),
                missing,
            }));
        }
        Ok(spec)
    }
}

/// Why a policy produced no spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("runtime {runtime} is not running ({state:?})")]
    NotRunning {
        runtime: RuntimeId,
        state: RuntimeState,
    },
    #[error("a contained session must not run as root (uid and gid must be non-zero)")]
    RootUser,
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Caps(#[from] CapsError),
}
