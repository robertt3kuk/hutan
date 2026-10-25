//! Policy → spec, and the fail-closed check against what a runtime can honour.

use hutan_container::argv::create_args;
use hutan_container::{
    BindKind, Boundary, CapsError, ContainerPolicy, Dialect, EnvVar, MissingCap, Mount,
    NetworkPolicy, PolicyError, Product, Reliability, RuntimeCaps, RuntimeKind, RuntimeReport,
    RuntimeState, SpecError, User, VolumeMount, VolumeName,
};

use crate::common::{abs, image, user};

fn running(kind: RuntimeKind) -> RuntimeReport {
    RuntimeReport::new(kind, RuntimeState::Running, 0)
}

const APPLE: RuntimeKind = RuntimeKind::AppleContainer;
const ORBSTACK: RuntimeKind = RuntimeKind::DockerCompatible(Product::OrbStack);

/// A linked worktree: its checkout and the repository's common git directory live apart.
fn linked_worktree_policy() -> ContainerPolicy {
    let mut policy = ContainerPolicy::new("s1", image(), abs("/Users/me/wt/feature"), user());
    policy.git_dir = Some(abs("/Users/me/repo/.git"));
    policy
}

#[test]
fn a_linked_worktree_gets_both_trees_at_their_own_paths_and_protected_hooks() {
    let mut policy = linked_worktree_policy();
    policy.labels.insert("hutan.project", "proj-1").unwrap();
    policy.limits.memory_mib = Some(4096);
    let spec = policy.plan(&running(ORBSTACK)).unwrap();

    assert_eq!(
        spec.mounts,
        vec![
            Mount::Bind {
                path: abs("/Users/me/wt/feature"),
                kind: BindKind::Directory,
                readonly: false
            },
            Mount::Bind {
                path: abs("/Users/me/repo/.git"),
                kind: BindKind::Directory,
                readonly: false
            },
            Mount::Bind {
                path: abs("/Users/me/repo/.git/hooks"),
                kind: BindKind::Directory,
                readonly: true
            },
        ]
    );
    assert_eq!(spec.network, NetworkPolicy::None, "closed by default");
    assert_eq!(spec.user, Some(user()));
    assert_eq!(spec.workdir, Some(abs("/Users/me/wt/feature")));
    assert_eq!(spec.labels.get("hutan.session"), Some("s1"));
    assert_eq!(spec.labels.get("hutan.project"), Some("proj-1"));
    let env: Vec<_> = spec.env.iter().map(|e| (e.name(), e.value())).collect();
    assert_eq!(
        env,
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "core.hooksPath"),
            ("GIT_CONFIG_VALUE_0", "/dev/null")
        ]
    );
    assert!(spec.cap_drop_all && spec.init);

    // …and the spec is something the builder accepts, with the read-only mount last.
    let args = create_args(Dialect::Docker, &spec).unwrap();
    assert!(args.windows(2).any(|w| w == ["--network", "none"]));
    assert!(args.contains(&"--memory".to_string()) && args.contains(&"4096M".to_string()));
    let mounts: Vec<_> = args
        .windows(2)
        .filter(|w| w[0] == "--mount")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(mounts.len(), 3);
    assert!(mounts[2].ends_with(",readonly") && mounts[2].contains("repo/.git/hooks"));
}

#[test]
fn a_plain_checkout_needs_no_second_mount_for_its_git_directory() {
    let mut policy = ContainerPolicy::new("s2", image(), abs("/Users/me/repo"), user());
    policy.git_dir = Some(abs("/Users/me/repo/.git"));
    let spec = policy.plan(&running(ORBSTACK)).unwrap();
    let guests: Vec<_> = spec.mounts.iter().map(|m| m.guest().as_str()).collect();
    assert_eq!(guests, ["/Users/me/repo", "/Users/me/repo/.git/hooks"]);
}

#[test]
fn git_protection_is_optional_and_needs_a_git_dir() {
    let mut policy = linked_worktree_policy();
    policy.protect_git_hooks = false;
    let spec = policy.plan(&running(ORBSTACK)).unwrap();
    assert_eq!(spec.mounts.len(), 2);
    assert!(spec.env.is_empty());

    let policy = ContainerPolicy::new("s3", image(), abs("/Users/me/repo"), user());
    let spec = policy.plan(&running(ORBSTACK)).unwrap();
    assert_eq!(spec.mounts.len(), 1);
    assert!(spec.env.is_empty());
}

#[test]
fn read_only_mounts_volumes_env_and_open_network_pass_through() {
    let mut policy = ContainerPolicy::new("s4", image(), abs("/w/repo"), user());
    policy.read_only_mounts = vec![abs("/opt/toolchains")];
    policy.volumes = vec![VolumeMount {
        name: VolumeName::new("hutan-cred-claude-work").unwrap(),
        guest: abs("/home/agent/.claude"),
    }];
    policy.network = NetworkPolicy::Open;
    policy.workdir = Some(abs("/w/repo/crates"));
    policy.env = vec![EnvVar::new("CLAUDE_CONFIG_DIR", "/home/agent/.claude").unwrap()];
    policy.root_read_only = true;
    let spec = policy.plan(&running(APPLE)).unwrap();
    assert_eq!(
        spec.mounts,
        vec![
            Mount::Bind {
                path: abs("/w/repo"),
                kind: BindKind::Directory,
                readonly: false
            },
            Mount::Bind {
                path: abs("/opt/toolchains"),
                kind: BindKind::Directory,
                readonly: true
            },
            Mount::Volume {
                name: VolumeName::new("hutan-cred-claude-work").unwrap(),
                guest: abs("/home/agent/.claude"),
                readonly: false
            },
        ]
    );
    assert_eq!(spec.network, NetworkPolicy::Open);
    assert_eq!(spec.workdir, Some(abs("/w/repo/crates")));
    assert!(spec.root_read_only);
    assert_eq!(spec.env.len(), 1);
}

#[test]
fn apple_refuses_nested_mounts_until_c2_has_tested_them() {
    // Protected hooks are a mount inside the repo mount.
    let err = linked_worktree_policy().plan(&running(APPLE)).unwrap_err();
    assert_eq!(
        err,
        PolicyError::Caps(CapsError {
            runtime: running(APPLE).id,
            missing: vec![MissingCap::NestedMounts]
        })
    );
    // So is a volume for `target/` under the worktree.
    let mut policy = ContainerPolicy::new("s5", image(), abs("/w/repo"), user());
    policy.volumes = vec![VolumeMount {
        name: VolumeName::new("hutan-target").unwrap(),
        guest: abs("/w/repo/target"),
    }];
    assert!(matches!(
        policy.plan(&running(APPLE)),
        Err(PolicyError::Caps(CapsError { missing, .. })) if missing == [MissingCap::NestedMounts]
    ));
    // The same policies are fine where nested mounts are documented to work.
    assert!(linked_worktree_policy().plan(&running(ORBSTACK)).is_ok());
    assert!(policy.plan(&running(ORBSTACK)).is_ok());
}

#[test]
fn a_policy_is_honoured_exactly_when_every_needed_capability_is_there() {
    // Needs `--network none`, a named volume, and a nested mount; try all 8 combinations.
    let mut policy = ContainerPolicy::new("s6", image(), abs("/w/repo"), user());
    policy.volumes = vec![VolumeMount {
        name: VolumeName::new("hutan-target").unwrap(),
        guest: abs("/w/repo/target"),
    }];
    for bits in 0u8..8 {
        let mut report = running(ORBSTACK);
        report.caps.network_none = bits & 1 != 0;
        report.caps.named_volumes = bits & 2 != 0;
        report.caps.nested_mounts = bits & 4 != 0;
        let mut expected = Vec::new();
        if bits & 1 == 0 {
            expected.push(MissingCap::NetworkNone);
        }
        if bits & 2 == 0 {
            expected.push(MissingCap::NamedVolumes);
        }
        if bits & 4 == 0 {
            expected.push(MissingCap::NestedMounts);
        }
        match policy.plan(&report) {
            Ok(_) => assert!(
                expected.is_empty(),
                "bits {bits:03b} was honoured with {expected:?} missing"
            ),
            Err(PolicyError::Caps(err)) => assert_eq!(err.missing, expected, "bits {bits:03b}"),
            Err(other) => panic!("{other:?}"),
        }
    }
}

#[test]
fn an_open_network_does_not_need_network_none() {
    let mut policy = ContainerPolicy::new("s7", image(), abs("/w/repo"), user());
    let mut report = running(ORBSTACK);
    report.caps.network_none = false;
    assert!(matches!(policy.plan(&report), Err(PolicyError::Caps(_))));
    policy.network = NetworkPolicy::Open;
    assert!(policy.plan(&report).is_ok());
}

#[test]
fn the_error_names_what_is_missing() {
    let mut policy = ContainerPolicy::new("s8", image(), abs("/w/repo"), user());
    policy.volumes = vec![VolumeMount {
        name: VolumeName::new("v").unwrap(),
        guest: abs("/w/repo/target"),
    }];
    policy.require_boundary = Some(Boundary::VmPerContainer);
    let mut report = running(ORBSTACK);
    report.caps = RuntimeCaps {
        network_none: false,
        named_volumes: false,
        nested_mounts: false,
        ..report.caps
    };
    let err = policy.plan(&report).unwrap_err();
    let text = err.to_string();
    for needle in [
        "orbstack",
        "network_none",
        "named_volumes",
        "nested_mounts",
        "VM per container",
        "shared VM",
    ] {
        assert!(text.contains(needle), "{needle} not in {text:?}");
    }
    let PolicyError::Caps(caps) = err else {
        panic!("{err:?}")
    };
    assert_eq!(caps.missing.len(), 4);
    assert!(caps.missing.contains(&MissingCap::Boundary {
        required: Boundary::VmPerContainer,
        actual: Boundary::SharedVm
    }));
}

#[test]
fn a_required_boundary_is_an_ordering_not_an_equality() {
    let mut policy = ContainerPolicy::new("s9", image(), abs("/w/repo"), user());
    let other = RuntimeKind::DockerCompatible(Product::Other);
    for (kind, required, ok) in [
        (APPLE, Boundary::VmPerContainer, true),
        (APPLE, Boundary::SharedVm, true),
        (ORBSTACK, Boundary::SharedVm, true),
        (ORBSTACK, Boundary::VmPerContainer, false),
        (other, Boundary::SharedVm, false),
        (other, Boundary::Unknown, true),
    ] {
        policy.require_boundary = Some(required);
        assert_eq!(
            policy.plan(&running(kind)).is_ok(),
            ok,
            "{kind:?} requires {required:?}"
        );
    }
    policy.require_boundary = None;
    assert!(policy.plan(&running(other)).is_ok());
}

#[test]
fn single_file_mounts_need_a_reliable_runtime() {
    let mut spec = linked_worktree_policy().to_spec().unwrap();
    spec.mounts.push(Mount::Bind {
        path: abs("/Users/me/.gitconfig"),
        kind: BindKind::File,
        readonly: true,
    });
    let apple = RuntimeCaps::for_kind(APPLE);
    assert_eq!(apple.single_file_mount, Reliability::Flaky);
    assert!(
        apple
            .missing_for(&spec, None)
            .contains(&MissingCap::SingleFileMount)
    );
    let docker = RuntimeCaps::for_kind(ORBSTACK);
    assert!(
        !docker
            .missing_for(&spec, None)
            .contains(&MissingCap::SingleFileMount)
    );
}

#[test]
fn only_a_running_runtime_is_planned_for() {
    let policy = ContainerPolicy::new("s10", image(), abs("/w/repo"), user());
    for state in [
        RuntimeState::Missing,
        RuntimeState::InstalledNotRunning {
            hint: "start it".to_string(),
        },
        RuntimeState::Unsupported {
            reason: "too old".to_string(),
        },
    ] {
        let report = RuntimeReport::new(APPLE, state.clone(), 0);
        assert_eq!(
            policy.plan(&report),
            Err(PolicyError::NotRunning {
                runtime: report.id.clone(),
                state
            })
        );
    }
}

#[test]
fn contained_sessions_are_never_root() {
    for bad in [
        User { uid: 0, gid: 20 },
        User { uid: 501, gid: 0 },
        User { uid: 0, gid: 0 },
    ] {
        let policy = ContainerPolicy::new("s11", image(), abs("/w/repo"), bad);
        assert_eq!(policy.plan(&running(ORBSTACK)), Err(PolicyError::RootUser));
        assert_eq!(policy.to_spec(), Err(PolicyError::RootUser));
    }
}

#[test]
fn invalid_policy_input_is_a_spec_error_not_a_command_line() {
    let policy = ContainerPolicy::new("not a session id", image(), abs("/w/repo"), user());
    assert!(matches!(
        policy.plan(&running(ORBSTACK)),
        Err(PolicyError::Spec(SpecError::InvalidName(_)))
    ));

    let mut policy = ContainerPolicy::new("s12", image(), abs("/w/repo"), user());
    policy.read_only_mounts = vec![abs("/var/run/docker.sock")];
    assert!(matches!(
        policy.plan(&running(ORBSTACK)),
        Err(PolicyError::Spec(SpecError::ForbiddenMount(_)))
    ));

    let mut policy = ContainerPolicy::new("s13", image(), abs("/w/repo"), user());
    policy.limits.cpus = Some(0);
    assert!(matches!(
        policy.plan(&running(ORBSTACK)),
        Err(PolicyError::Spec(SpecError::InvalidLimit(_)))
    ));

    // The same path asked for twice.
    let mut policy = ContainerPolicy::new("s14", image(), abs("/w/repo"), user());
    policy.read_only_mounts = vec![abs("/w/repo")];
    assert_eq!(
        policy.plan(&running(ORBSTACK)),
        Err(PolicyError::Spec(SpecError::DuplicateMountTarget(
            "/w/repo".to_string()
        )))
    );
}

#[test]
fn the_capability_table_states_what_the_research_found() {
    let apple = RuntimeCaps::for_kind(APPLE);
    assert_eq!(apple.boundary, Boundary::VmPerContainer);
    assert!(apple.network_none && apple.host_socket_mount && apple.named_volumes && apple.rosetta);
    assert!(
        apple.host_reachable_from_internal,
        "Apple's gateway stays reachable from --internal (#1320)"
    );
    // Closed until C-2 tests them on a real runtime.
    assert!(!apple.nested_mounts && !apple.exec_resize);

    for product in [
        Product::OrbStack,
        Product::DockerDesktop,
        Product::Colima,
        Product::RancherDesktop,
        Product::Podman,
    ] {
        let caps = RuntimeCaps::for_kind(RuntimeKind::DockerCompatible(product));
        assert_eq!(caps.boundary, Boundary::SharedVm, "{product:?}");
        assert!(
            !caps.host_socket_mount,
            "no portable host-socket mount on the Docker family"
        );
        assert!(caps.network_none && caps.named_volumes && caps.nested_mounts && caps.exec_resize);
        assert_eq!(caps.single_file_mount, Reliability::Reliable);
        assert_eq!(
            caps.rosetta,
            matches!(product, Product::OrbStack | Product::DockerDesktop),
            "{product:?}"
        );
    }
    assert_eq!(
        RuntimeCaps::for_kind(RuntimeKind::DockerCompatible(Product::Other)).boundary,
        Boundary::Unknown
    );
    assert_eq!(Boundary::VmPerContainer.label(), "VM per container");
    assert_eq!(Boundary::SharedVm.label(), "shared VM");
    assert!(
        Boundary::VmPerContainer > Boundary::SharedVm && Boundary::SharedVm > Boundary::Unknown
    );
}

#[test]
fn runtime_kinds_map_to_dialects_and_ids() {
    let kinds: Vec<_> = RuntimeKind::listed().collect();
    let ids: Vec<_> = kinds.iter().map(|k| k.id()).collect();
    assert_eq!(
        ids,
        [
            "apple-container",
            "orbstack",
            "docker-desktop",
            "colima",
            "rancher-desktop",
            "podman"
        ]
    );
    let dialects: Vec<_> = kinds.iter().map(|k| k.dialect()).collect();
    assert_eq!(
        dialects,
        [
            Dialect::Apple,
            Dialect::Docker,
            Dialect::Docker,
            Dialect::Docker,
            Dialect::Docker,
            Dialect::Podman
        ]
    );
    assert_eq!(Dialect::Apple.binary(), "container");
    assert_eq!(Dialect::Docker.binary(), "docker");
    assert_eq!(Dialect::Podman.binary(), "podman");
}
