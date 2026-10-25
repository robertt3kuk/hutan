//! Argv builders for both dialects: exact argument lists, so a change is a visible diff.

use std::path::PathBuf;
use std::time::Duration;

use hutan_container::argv::{
    create_args, exec_args, image_inspect_args, inspect_args, list_args, remove_args, start_args,
    stop_args, volume_create_args, volume_inspect_args,
};
use hutan_container::{
    BindKind, Cli, ContainerId, Dialect, EnvVar, ExecIo, ExecSpec, Labels, Limits, Mount,
    NetworkPolicy, SpecError, VolumeName,
};

use crate::common::{abs, image, minimal_spec, strs, user};

const DIALECTS: [Dialect; 3] = [Dialect::Apple, Dialect::Docker, Dialect::Podman];

fn id(name: &str) -> ContainerId {
    ContainerId::new(name).unwrap()
}

#[test]
fn minimal_spec_is_closed_by_default() {
    let spec = minimal_spec();
    let common = [
        "create",
        "--name",
        "hutan-s1",
        "--label",
        "hutan.managed=true",
        "--label",
        "hutan.session=s1",
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--init",
        "ghcr.io/hutan/agent:1.0",
        "sleep",
        "infinity",
    ];
    assert_eq!(create_args(Dialect::Apple, &spec).unwrap(), strs(&common));
    // Docker and Podman refuse to pull on `create` (Apple has no such flag).
    let mut docker = strs(&common);
    docker.insert(3, "--pull=never".to_string());
    assert_eq!(create_args(Dialect::Docker, &spec).unwrap(), docker);
    assert_eq!(create_args(Dialect::Podman, &spec).unwrap(), docker);
}

fn full_spec() -> hutan_container::ContainerSpec {
    let mut spec = minimal_spec();
    spec.labels.insert("hutan.project", "proj-1").unwrap();
    // Deliberately out of order: a child before its parent.
    spec.mounts = vec![
        Mount::Bind {
            path: abs("/Users/me/My Projects/repo.git/hooks"),
            kind: BindKind::Directory,
            readonly: true,
        },
        Mount::Volume {
            name: VolumeName::new("hutan-target").unwrap(),
            guest: abs("/Users/me/My Projects/repo/target"),
            readonly: false,
        },
        Mount::Tmpfs { guest: abs("/tmp") },
        Mount::Bind {
            path: abs("/Users/me/My Projects/repo"),
            kind: BindKind::Directory,
            readonly: false,
        },
        Mount::Bind {
            path: abs("/Users/me/My Projects/repo.git"),
            kind: BindKind::Directory,
            readonly: false,
        },
    ];
    spec.network = NetworkPolicy::Open;
    spec.user = Some(user());
    spec.workdir = Some(abs("/Users/me/My Projects/repo"));
    spec.env = vec![
        EnvVar::new("GREETING", "hello world=1").unwrap(),
        EnvVar::new("EMPTY", "").unwrap(),
    ];
    spec.limits = Limits {
        cpus: Some(4),
        memory_mib: Some(2048),
    };
    spec.root_read_only = true;
    spec.command = strs(&["tail", "-f", "/dev/null"]);
    spec
}

#[test]
fn full_spec_orders_parents_first_keeps_spaces_in_one_argument_and_follows_the_dialect() {
    let spec = full_spec();
    let shared_head = strs(&["create", "--name", "hutan-s1"]);
    let labels = strs(&[
        "--label",
        "hutan.managed=true",
        "--label",
        "hutan.project=proj-1",
        "--label",
        "hutan.session=s1",
    ]);
    let mounts = strs(&[
        "--mount",
        "type=bind,source=/Users/me/My Projects/repo,target=/Users/me/My Projects/repo",
        "--mount",
        "type=bind,source=/Users/me/My Projects/repo.git,target=/Users/me/My Projects/repo.git",
        "--mount",
        "type=bind,source=/Users/me/My Projects/repo.git/hooks,target=/Users/me/My Projects/repo.git/hooks,readonly",
        "--mount",
        "type=volume,source=hutan-target,target=/Users/me/My Projects/repo/target",
        "--tmpfs",
        "/tmp",
    ]);
    let tail = strs(&[
        "--workdir",
        "/Users/me/My Projects/repo",
        "--env",
        "GREETING=hello world=1",
        "--env",
        "EMPTY=",
        "--cpus",
        "4",
        "--memory",
        "2048M",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--init",
        "ghcr.io/hutan/agent:1.0",
        "tail",
        "-f",
        "/dev/null",
    ]);

    let expected = |pull: bool, user: &[&str]| {
        let mut v = shared_head.clone();
        if pull {
            v.push("--pull=never".to_string());
        }
        v.extend(labels.clone());
        v.extend(mounts.clone());
        // `--network none` is absent: the network is open.
        v.extend(strs(user));
        v.extend(tail.clone());
        v
    };
    assert_eq!(
        create_args(Dialect::Apple, &spec).unwrap(),
        expected(false, &["--uid", "501", "--gid", "20"])
    );
    assert_eq!(
        create_args(Dialect::Docker, &spec).unwrap(),
        expected(true, &["--user", "501:20"])
    );
    assert_eq!(
        create_args(Dialect::Podman, &spec).unwrap(),
        expected(true, &["--user", "501:20"])
    );
}

#[test]
fn network_none_and_open_differ_by_exactly_the_flag() {
    for dialect in DIALECTS {
        let closed = create_args(dialect, &minimal_spec()).unwrap();
        let mut open_spec = minimal_spec();
        open_spec.network = NetworkPolicy::Open;
        let open = create_args(dialect, &open_spec).unwrap();
        assert!(closed.windows(2).any(|w| w == ["--network", "none"]));
        assert!(!open.iter().any(|a| a == "--network"));
        assert_eq!(closed.len(), open.len() + 2);
    }
}

#[test]
fn hardening_flags_follow_the_spec() {
    let mut spec = minimal_spec();
    spec.cap_drop_all = false;
    spec.init = false;
    for dialect in DIALECTS {
        let args = create_args(dialect, &spec).unwrap();
        assert!(!args.contains(&"--cap-drop".to_string()) && !args.contains(&"--init".to_string()));
        assert!(!args.contains(&"--read-only".to_string()));
    }
}

#[test]
fn create_refuses_an_invalid_spec_in_every_dialect() {
    for dialect in DIALECTS {
        let mut spec = minimal_spec();
        spec.command.clear();
        assert_eq!(create_args(dialect, &spec), Err(SpecError::EmptyCommand));

        let mut spec = minimal_spec();
        spec.labels = Labels::default();
        assert_eq!(
            create_args(dialect, &spec),
            Err(SpecError::MissingLabel("hutan.managed"))
        );

        let mut spec = minimal_spec();
        spec.mounts = vec![
            Mount::Bind {
                path: abs("/a"),
                kind: BindKind::Directory,
                readonly: false,
            },
            Mount::Tmpfs { guest: abs("/a") },
        ];
        assert_eq!(
            create_args(dialect, &spec),
            Err(SpecError::DuplicateMountTarget("/a".to_string()))
        );

        let mut spec = minimal_spec();
        spec.mounts = vec![Mount::Bind {
            path: abs("/Users/me/.colima/default/docker.sock"),
            kind: BindKind::File,
            readonly: false,
        }];
        assert!(matches!(
            create_args(dialect, &spec),
            Err(SpecError::ForbiddenMount(_))
        ));

        let mut spec = minimal_spec();
        spec.limits.cpus = Some(0);
        assert!(matches!(
            create_args(dialect, &spec),
            Err(SpecError::InvalidLimit(_))
        ));
    }
}

#[test]
fn exec_pty_is_it_and_stdio_is_i() {
    let mut cmd = ExecSpec::new(strs(&["/bin/zsh", "-l", "-c", "echo 'a b'"])).unwrap();
    cmd.workdir = Some(abs("/work/tree"));
    cmd.env = vec![EnvVar::new("TERM", "xterm-256color").unwrap()];
    cmd.user = Some(user());
    let c = id("hutan-s1");

    let apple_pty = exec_args(Dialect::Apple, &c, &cmd, ExecIo::Pty);
    assert_eq!(
        apple_pty,
        strs(&[
            "exec",
            "-it",
            "--uid",
            "501",
            "--gid",
            "20",
            "--workdir",
            "/work/tree",
            "--env",
            "TERM=xterm-256color",
            "hutan-s1",
            "/bin/zsh",
            "-l",
            "-c",
            "echo 'a b'",
        ])
    );
    let docker_stdio = exec_args(Dialect::Docker, &c, &cmd, ExecIo::Stdio);
    assert_eq!(
        docker_stdio,
        strs(&[
            "exec",
            "-i",
            "--user",
            "501:20",
            "--workdir",
            "/work/tree",
            "--env",
            "TERM=xterm-256color",
            "hutan-s1",
            "/bin/zsh",
            "-l",
            "-c",
            "echo 'a b'",
        ])
    );
    assert_eq!(
        exec_args(Dialect::Podman, &c, &cmd, ExecIo::Stdio),
        docker_stdio
    );
}

#[test]
fn exec_without_options_and_with_dash_arguments() {
    // Arguments after the container id are the command's, even when they look like options.
    let cmd = ExecSpec::new(strs(&["ls", "-la", "--color"])).unwrap();
    for dialect in DIALECTS {
        assert_eq!(
            exec_args(dialect, &id("c1"), &cmd, ExecIo::Stdio),
            strs(&["exec", "-i", "c1", "ls", "-la", "--color"])
        );
    }
    assert_eq!(ExecSpec::new(vec![]), Err(SpecError::EmptyExec));
    assert_eq!(ExecSpec::new(strs(&[""])), Err(SpecError::EmptyExec));
}

#[test]
fn lifecycle_commands() {
    let c = id("hutan-s1");
    for dialect in DIALECTS {
        assert_eq!(start_args(&c), strs(&["start", "hutan-s1"]));
        assert_eq!(
            stop_args(&c, Duration::from_secs(5)),
            strs(&["stop", "--time", "5", "hutan-s1"])
        );
        // Rounded up: never a shorter grace than asked.
        assert_eq!(stop_args(&c, Duration::from_millis(1500))[2], "2");
        assert_eq!(stop_args(&c, Duration::ZERO)[2], "0");
        assert_eq!(stop_args(&c, Duration::from_millis(1))[2], "1");
        let rm = if dialect == Dialect::Apple {
            "delete"
        } else {
            "rm"
        };
        assert_eq!(remove_args(dialect, &c, false), strs(&[rm, "hutan-s1"]));
        assert_eq!(
            remove_args(dialect, &c, true),
            strs(&[rm, "--force", "hutan-s1"])
        );
    }
    assert_eq!(
        inspect_args(Dialect::Apple, &c),
        strs(&["inspect", "hutan-s1"])
    );
    assert_eq!(
        inspect_args(Dialect::Docker, &c),
        strs(&["inspect", "--type", "container", "hutan-s1"])
    );
    assert_eq!(
        inspect_args(Dialect::Podman, &c),
        inspect_args(Dialect::Docker, &c)
    );
}

#[test]
fn list_is_all_containers_in_json() {
    assert_eq!(
        list_args(Dialect::Apple),
        strs(&["list", "--all", "--format", "json"])
    );
    let docker = strs(&[
        "ps",
        "--all",
        "--no-trunc",
        "--filter",
        "label=hutan.managed=true",
        "--format",
        "{{json .}}",
    ]);
    assert_eq!(list_args(Dialect::Docker), docker);
    assert_eq!(list_args(Dialect::Podman), docker);
}

#[test]
fn image_and_volume_commands() {
    assert_eq!(
        image_inspect_args(&image()),
        strs(&["image", "inspect", "ghcr.io/hutan/agent:1.0"])
    );
    let mut labels = Labels::default();
    labels.insert("hutan.account", "claude-work").unwrap();
    labels.insert("hutan.managed", "true").unwrap();
    let name = VolumeName::new("hutan-cred-claude-work").unwrap();
    assert_eq!(
        volume_create_args(&name, &labels),
        strs(&[
            "volume",
            "create",
            "--label",
            "hutan.account=claude-work",
            "--label",
            "hutan.managed=true",
            "hutan-cred-claude-work",
        ])
    );
    assert_eq!(
        volume_create_args(&name, &Labels::default()),
        strs(&["volume", "create", "hutan-cred-claude-work"])
    );
    assert_eq!(
        volume_inspect_args(&name),
        strs(&["volume", "inspect", "hutan-cred-claude-work"])
    );
}

#[test]
fn cli_binds_program_and_endpoint_to_the_dialect() {
    let c = id("hutan-s1");
    let cmd = ExecSpec::new(strs(&["sh"])).unwrap();

    let apple = Cli::new(Dialect::Apple, "/usr/local/bin/container");
    let launch = apple.exec_pty(&c, &cmd);
    assert_eq!(launch.program, PathBuf::from("/usr/local/bin/container"));
    assert_eq!(launch.args, strs(&["exec", "-it", "hutan-s1", "sh"]));
    assert!(launch.env.is_empty() && launch.cwd.is_none());
    // Apple has no endpoint variable even when one is recorded.
    assert!(
        apple
            .clone()
            .with_endpoint("/x.sock")
            .start(&c)
            .env
            .is_empty()
    );

    let docker = Cli::new(Dialect::Docker, "/usr/local/bin/docker")
        .with_endpoint("/Users/me/.orbstack/run/docker.sock");
    let launch = docker.exec_stdio(&c, &cmd);
    assert_eq!(launch.args, strs(&["exec", "-i", "hutan-s1", "sh"]));
    assert_eq!(
        launch.env,
        vec![(
            "DOCKER_HOST".to_string(),
            "unix:///Users/me/.orbstack/run/docker.sock".to_string()
        )]
    );

    let podman = Cli::new(Dialect::Podman, "/opt/homebrew/bin/podman").with_endpoint("/tmp/p.sock");
    assert_eq!(
        podman.stop(&c, Duration::from_secs(3)).env,
        vec![(
            "CONTAINER_HOST".to_string(),
            "unix:///tmp/p.sock".to_string()
        )]
    );

    // Builders that can fail surface the spec error instead of a half command line.
    let mut bad = minimal_spec();
    bad.command.clear();
    assert_eq!(docker.create(&bad), Err(SpecError::EmptyCommand));
    assert_eq!(docker.create(&minimal_spec()).unwrap().args[0], "create");
    assert_eq!(docker.list().args[0], "ps");
    assert_eq!(apple.list().args[0], "list");
    assert_eq!(
        docker.remove(&c, true).args,
        strs(&["rm", "--force", "hutan-s1"])
    );
    assert_eq!(docker.inspect(&c).args[0], "inspect");
    assert_eq!(docker.image_inspect(&image()).args[0], "image");
    let vol = VolumeName::new("v").unwrap();
    assert_eq!(
        docker.volume_create(&vol, &Labels::default()).args,
        strs(&["volume", "create", "v"])
    );
    assert_eq!(
        docker.volume_inspect(&vol).args,
        strs(&["volume", "inspect", "v"])
    );
}
