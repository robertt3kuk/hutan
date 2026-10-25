//! Pure argv builders for both dialects: `container …` and `docker|podman …`.
//!
//! Each function returns the arguments **after** the program name, one element per argument:
//! nothing here goes through a shell, so a path with spaces stays one element and needs no
//! quoting; what *cannot* travel inside an argument (the `,` of `--mount`, a leading `-`) is
//! refused by the value types of [`crate::spec`] before it gets here.
//!
//! Choices that apply to both dialects:
//! - Mounts use `--mount type=…,source=…,target=…[,readonly]` (documented by both), never
//!   `-v`: Docker's `-v` silently *creates* a missing source as a root-owned directory, while
//!   `--mount type=bind` fails.
//! - Parents are mounted before children (sorted by guest path depth), so a read-only
//!   `hooks/` under a writable repo mount is applied on top of it.
//! - `create` + `start`, not `run`: `ensure` is idempotent by label and `exec` attaches later.
//!
//! Differences (see each function): Apple `--uid/--gid` vs `--user uid:gid`; Apple `delete` vs
//! `rm`; `--pull=never` (Docker, Podman; Apple has no such flag, so its adapter checks the
//! image with [`image_inspect_args`] first); how containers are listed and inspected.

use std::time::Duration;

use crate::kind::Dialect;
use crate::spec::{
    ContainerId, ContainerSpec, ExecIo, ExecSpec, ImageRef, LABEL_MANAGED, Labels, Mount,
    NetworkPolicy, SpecError, User, VolumeName,
};

fn push(args: &mut Vec<String>, items: &[&str]) {
    args.extend(items.iter().map(|s| (*s).to_string()));
}

fn user_args(dialect: Dialect, user: User, args: &mut Vec<String>) {
    match dialect {
        Dialect::Apple => {
            push(
                args,
                &[
                    "--uid",
                    &user.uid.to_string(),
                    "--gid",
                    &user.gid.to_string(),
                ],
            );
        }
        Dialect::Docker | Dialect::Podman => {
            push(args, &["--user", &format!("{}:{}", user.uid, user.gid)]);
        }
    }
}

fn mount_arg(mount: &Mount) -> Option<String> {
    let flag = |readonly: bool| if readonly { ",readonly" } else { "" };
    match mount {
        Mount::Bind { path, readonly, .. } => Some(format!(
            "type=bind,source={path},target={path}{}",
            flag(*readonly)
        )),
        Mount::Volume {
            name,
            guest,
            readonly,
        } => Some(format!(
            "type=volume,source={name},target={guest}{}",
            flag(*readonly)
        )),
        Mount::Tmpfs { .. } => None,
    }
}

/// `create` for `spec`. Validates the spec first.
pub fn create_args(dialect: Dialect, spec: &ContainerSpec) -> Result<Vec<String>, SpecError> {
    spec.validate()?;
    let mut args = Vec::new();
    push(&mut args, &["create", "--name", spec.name.as_str()]);
    if dialect.is_docker_like() {
        push(&mut args, &["--pull=never"]);
    }
    for (key, value) in spec.labels.iter() {
        push(&mut args, &["--label", &format!("{key}={value}")]);
    }
    let mut mounts: Vec<&Mount> = spec.mounts.iter().collect();
    mounts.sort_by_key(|m| m.guest().depth());
    for mount in &mounts {
        if let Some(arg) = mount_arg(mount) {
            push(&mut args, &["--mount", &arg]);
        }
    }
    for mount in &mounts {
        if let Mount::Tmpfs { guest } = mount {
            push(&mut args, &["--tmpfs", guest.as_str()]);
        }
    }
    if spec.network == NetworkPolicy::None {
        push(&mut args, &["--network", "none"]);
    }
    if let Some(user) = spec.user {
        user_args(dialect, user, &mut args);
    }
    if let Some(workdir) = &spec.workdir {
        push(&mut args, &["--workdir", workdir.as_str()]);
    }
    for var in &spec.env {
        push(
            &mut args,
            &["--env", &format!("{}={}", var.name(), var.value())],
        );
    }
    if let Some(cpus) = spec.limits.cpus {
        push(&mut args, &["--cpus", &cpus.to_string()]);
    }
    if let Some(mib) = spec.limits.memory_mib {
        push(&mut args, &["--memory", &format!("{mib}M")]);
    }
    if spec.root_read_only {
        push(&mut args, &["--read-only"]);
    }
    if spec.cap_drop_all {
        push(&mut args, &["--cap-drop", "ALL"]);
    }
    if spec.init {
        push(&mut args, &["--init"]);
    }
    args.push(spec.image.to_string());
    args.extend(spec.command.iter().cloned());
    Ok(args)
}

pub fn start_args(id: &ContainerId) -> Vec<String> {
    vec!["start".to_string(), id.to_string()]
}

/// `stop` with a grace period, rounded up to whole seconds (both CLIs take seconds).
pub fn stop_args(id: &ContainerId, grace: Duration) -> Vec<String> {
    let secs = grace.as_secs() + u64::from(grace.subsec_nanos() > 0);
    vec![
        "stop".to_string(),
        "--time".to_string(),
        secs.to_string(),
        id.to_string(),
    ]
}

pub fn remove_args(dialect: Dialect, id: &ContainerId, force: bool) -> Vec<String> {
    let mut args = vec![
        if dialect == Dialect::Apple {
            "delete"
        } else {
            "rm"
        }
        .to_string(),
    ];
    if force {
        args.push("--force".to_string());
    }
    args.push(id.to_string());
    args
}

/// Low-level JSON of one container (state, exit code).
pub fn inspect_args(dialect: Dialect, id: &ContainerId) -> Vec<String> {
    match dialect {
        Dialect::Apple => vec!["inspect".to_string(), id.to_string()],
        // `--type container`: a name that is also an image or volume must not match those.
        Dialect::Docker | Dialect::Podman => {
            vec![
                "inspect".to_string(),
                "--type".to_string(),
                "container".to_string(),
                id.to_string(),
            ]
        }
    }
}

/// Every container, stopped ones included, for the daemon-start reaper.
///
/// Docker/Podman filter by the `hutan.managed` label and print one JSON object per line; Apple
/// has no label filter, so it lists everything as JSON and the caller keeps the `hutan.*` ones
/// (counts and unrelated containers are never logged).
pub fn list_args(dialect: Dialect) -> Vec<String> {
    match dialect {
        Dialect::Apple => ["list", "--all", "--format", "json"]
            .map(String::from)
            .to_vec(),
        Dialect::Docker | Dialect::Podman => [
            "ps",
            "--all",
            "--no-trunc",
            "--filter",
            &format!("label={LABEL_MANAGED}=true"),
            "--format",
            "{{json .}}",
        ]
        .map(String::from)
        .to_vec(),
    }
}

/// Exists locally? (exit status only; never pulls.)
pub fn image_inspect_args(image: &ImageRef) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        image.to_string(),
    ]
}

/// `volume create`. Docker and Podman return the existing volume; Apple fails when it exists,
/// so its adapter inspects first ([`volume_inspect_args`]).
pub fn volume_create_args(name: &VolumeName, labels: &Labels) -> Vec<String> {
    let mut args = vec!["volume".to_string(), "create".to_string()];
    for (key, value) in labels.iter() {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push(name.to_string());
    args
}

pub fn volume_inspect_args(name: &VolumeName) -> Vec<String> {
    vec![
        "volume".to_string(),
        "inspect".to_string(),
        name.to_string(),
    ]
}

/// `exec` of `cmd` in `id`: `-it` for a PTY, `-i` for pipes.
pub fn exec_args(dialect: Dialect, id: &ContainerId, cmd: &ExecSpec, io: ExecIo) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        if io == ExecIo::Pty { "-it" } else { "-i" }.to_string(),
    ];
    if let Some(user) = cmd.user {
        user_args(dialect, user, &mut args);
    }
    if let Some(workdir) = &cmd.workdir {
        push(&mut args, &["--workdir", workdir.as_str()]);
    }
    for var in &cmd.env {
        push(
            &mut args,
            &["--env", &format!("{}={}", var.name(), var.value())],
        );
    }
    args.push(id.to_string());
    // After the container id both CLIs treat everything as the command (Docker stops option
    // parsing at the first positional; Apple captures the rest for passthrough).
    args.extend(cmd.argv.iter().cloned());
    args
}
