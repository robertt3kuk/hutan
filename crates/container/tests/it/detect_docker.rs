//! Docker Engine-compatible runtimes: identification and states from recorded API answers.

use std::path::{Path, PathBuf};

use hutan_container::detect::docker::{
    EndpointError, EngineFacts, identify, probe_endpoint, product_from_path,
};
use hutan_container::detect::podman::parse_machines;
use hutan_container::probe::ProbeError;
use hutan_container::{
    Boundary, Dialect, Product, RuntimeKind, RuntimeReport, RuntimeState,
    detect::detect_docker_family, detect_all,
};

use crate::common::fake::{FakeProbe, HOME, PODMAN_SOCKET, fixtures as fx, output};

const DOCKER_CLI: &str = "/usr/local/bin/docker";

fn sock(rel: &str) -> String {
    format!("{HOME}/{rel}")
}

fn docker_mac() -> FakeProbe {
    FakeProbe::mac().exe("docker", DOCKER_CLI)
}

fn product_report(reports: &[RuntimeReport], product: Product) -> &RuntimeReport {
    reports
        .iter()
        .find(|r| r.kind == RuntimeKind::DockerCompatible(product))
        .expect("listed product has a report")
}

fn assert_running(report: &RuntimeReport, product: Product, version: &str, socket: &str) {
    assert_eq!(report.kind, RuntimeKind::DockerCompatible(product));
    assert_eq!(report.state, RuntimeState::Running, "{report:?}");
    assert_eq!(report.version.as_deref(), Some(version));
    assert_eq!(report.endpoint.as_deref(), Some(Path::new(socket)));
    assert_eq!(report.caps.boundary, Boundary::SharedVm);
}

#[test]
fn every_product_is_recognised_at_its_documented_socket() {
    let cases = [
        (
            ".docker/run/docker.sock",
            fx::DOCKER_DESKTOP,
            Product::DockerDesktop,
            "29.3.1",
            "docker-desktop",
        ),
        (
            ".orbstack/run/docker.sock",
            fx::ORBSTACK,
            Product::OrbStack,
            "23.0.3",
            "orbstack",
        ),
        (
            ".rd/docker.sock",
            fx::RANCHER_DESKTOP,
            Product::RancherDesktop,
            "27.0.1",
            "rancher-desktop",
        ),
    ];
    for (rel, engine, product, version, id) in cases {
        let path = sock(rel);
        let probe = docker_mac().engine(&path, engine);
        let reports = detect_docker_family(&probe);
        let report = product_report(&reports, product);
        assert_running(report, product, version, &path);
        assert_eq!(report.id.as_str(), id);
        assert_eq!(report.cli.as_deref(), Some(Path::new(DOCKER_CLI)));
        // The launcher talks to this engine, not to whatever `docker context` selects.
        let launch = report
            .cli()
            .unwrap()
            .start(&hutan_container::ContainerId::new("c").unwrap());
        assert_eq!(
            launch.env,
            vec![("DOCKER_HOST".to_string(), format!("unix://{path}"))]
        );
        assert_eq!(report.cli().unwrap().dialect, Dialect::Docker);
    }
}

#[test]
fn colima_profiles_get_their_own_ids() {
    let default = sock(".colima/default/docker.sock");
    let dev = sock(".config/colima/dev/docker.sock");
    let probe = docker_mac()
        .listing(&sock(".colima"), &[&sock(".colima/default")])
        .listing(&sock(".config/colima"), &[&sock(".config/colima/dev")])
        .engine(&default, fx::COLIMA)
        .engine(&dev, fx::COLIMA);
    let reports = detect_docker_family(&probe);
    let colima: Vec<_> = reports
        .iter()
        .filter(|r| r.kind == RuntimeKind::DockerCompatible(Product::Colima))
        .collect();
    assert_eq!(
        colima.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["colima", "colima-dev"]
    );
    assert_running(colima[0], Product::Colima, "26.1.1", &default);
    assert_running(colima[1], Product::Colima, "26.1.1", &dev);
}

#[test]
fn who_answers_beats_where_it_answers() {
    // The same generic socket path, five different engines.
    let cases = [
        (fx::DOCKER_DESKTOP, Product::DockerDesktop),
        (fx::ORBSTACK, Product::OrbStack),
        (fx::COLIMA, Product::Colima),
        (fx::RANCHER_DESKTOP, Product::RancherDesktop),
        (fx::PODMAN, Product::Podman),
        (fx::LIMA, Product::Other),
    ];
    for (engine, expected) in cases {
        let probe = docker_mac()
            .exe("podman", "/opt/homebrew/bin/podman")
            .engine("/var/run/docker.sock", engine);
        let reports = detect_docker_family(&probe);
        let running: Vec<_> = reports.iter().filter(|r| r.is_running()).collect();
        assert_eq!(running.len(), 1, "{expected:?}: {reports:?}");
        assert_eq!(running[0].kind, RuntimeKind::DockerCompatible(expected));
    }
}

#[test]
fn identification_uses_the_engines_own_words() {
    // Docker Desktop is told apart from stock Docker Engine by `/info`, not by the path.
    let facts = |engine: fx::Engine| {
        let probe = FakeProbe::mac().engine("/s", engine);
        probe_endpoint(&probe, Path::new("/s")).expect("fixture engine answers")
    };
    assert_eq!(
        identify(Path::new("/s"), &facts(fx::DOCKER_DESKTOP)),
        Product::DockerDesktop
    );
    assert_eq!(
        identify(Path::new("/s"), &facts(fx::COLIMA)),
        Product::Colima
    );
    assert_eq!(
        identify(Path::new("/s"), &facts(fx::ORBSTACK)),
        Product::OrbStack
    );
    assert_eq!(
        identify(Path::new("/s"), &facts(fx::PODMAN)),
        Product::Podman
    );
    // With nothing to go on but the path, the path decides.
    let bare = EngineFacts::default();
    assert_eq!(
        identify(Path::new("/Users/me/.orbstack/run/docker.sock"), &bare),
        Product::OrbStack
    );
    assert_eq!(
        identify(Path::new("/var/run/docker.sock"), &bare),
        Product::Other
    );
}

#[test]
fn path_hints_are_the_documented_locations_only() {
    let hint = |p: &str| product_from_path(Path::new(p));
    assert_eq!(
        hint("/Users/me/.orbstack/run/docker.sock"),
        Some(Product::OrbStack)
    );
    assert_eq!(
        hint("/Users/me/.colima/default/docker.sock"),
        Some(Product::Colima)
    );
    assert_eq!(
        hint("/Users/me/.config/colima/dev/docker.sock"),
        Some(Product::Colima)
    );
    assert_eq!(
        hint("/Users/me/.rd/docker.sock"),
        Some(Product::RancherDesktop)
    );
    assert_eq!(
        hint("/Users/me/.docker/run/docker.sock"),
        Some(Product::DockerDesktop)
    );
    assert_eq!(
        hint("/Users/me/Library/Containers/com.docker.docker/Data/docker-cli.sock"),
        Some(Product::DockerDesktop)
    );
    assert_eq!(hint(PODMAN_SOCKET), Some(Product::Podman));
    assert_eq!(hint("/var/run/docker.sock"), None);
    assert_eq!(hint("/tmp/x.sock"), None);
}

#[test]
fn var_run_symlink_is_followed_and_not_probed_twice() {
    let orb = sock(".orbstack/run/docker.sock");
    let probe = docker_mac()
        .engine(&orb, fx::ORBSTACK)
        .symlink("/var/run/docker.sock", &orb);
    let reports = detect_docker_family(&probe);
    assert_eq!(reports.iter().filter(|r| r.is_running()).count(), 1);
    assert_running(
        product_report(&reports, Product::OrbStack),
        Product::OrbStack,
        "23.0.3",
        &orb,
    );
    let gets = probe.calls();
    assert!(
        gets.iter().all(|c| c.starts_with(&format!("GET {orb} "))),
        "{gets:?}"
    );
}

#[test]
fn docker_host_unix_socket_is_probed_other_schemes_are_ignored() {
    let custom = "/Users/me/custom/engine.sock";
    let probe = docker_mac()
        .env("DOCKER_HOST", &format!("unix://{custom}"))
        .engine(custom, fx::LIMA);
    let reports = detect_docker_family(&probe);
    let other = reports
        .iter()
        .find(|r| r.kind == RuntimeKind::DockerCompatible(Product::Other))
        .unwrap();
    assert_eq!(other.state, RuntimeState::Running);
    assert_eq!(other.version.as_deref(), Some("27.0.1"));
    assert_eq!(other.endpoint.as_deref(), Some(Path::new(custom)));
    assert_eq!(other.caps.boundary, Boundary::Unknown);
    assert_eq!(other.id.as_str(), "docker");

    let probe = docker_mac().env("DOCKER_HOST", "tcp://10.0.0.5:2375");
    assert!(detect_docker_family(&probe).iter().all(|r| !r.is_running()));
    assert!(
        probe.calls().is_empty(),
        "a TCP endpoint is never contacted"
    );
}

#[test]
fn something_that_is_not_an_engine_is_not_offered() {
    let path = sock(".docker/run/docker.sock");
    let probe = docker_mac().engine(&path, fx::NOT_AN_ENGINE);
    let reports = detect_docker_family(&probe);
    assert!(reports.iter().all(|r| !r.is_running()));
    // Only `/_ping` was asked: nothing is read from a peer that failed it.
    assert_eq!(probe.calls(), [format!("GET {path} /_ping")]);
    assert_eq!(
        probe_endpoint(&probe, Path::new(&path)).unwrap_err(),
        EndpointError::NotAnEngine("`/_ping` answered 404 \"404 page not found\"".to_string())
    );
}

#[test]
fn a_dead_socket_means_installed_not_running() {
    let path = sock(".orbstack/run/docker.sock");
    let probe = docker_mac().stale_socket(&path);
    let reports = detect_docker_family(&probe);
    match &product_report(&reports, Product::OrbStack).state {
        RuntimeState::InstalledNotRunning { hint } => {
            assert!(hint.contains("open it yourself"), "{hint}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        product_report(&reports, Product::DockerDesktop).state,
        RuntimeState::Missing
    );
    assert_eq!(
        probe.calls(),
        [format!("GET {path} /_ping")],
        "a refused ping ends the probe of that socket"
    );
}

#[test]
fn installed_apps_and_binaries_are_reported_never_launched() {
    let probe = docker_mac()
        .dir("/Applications/OrbStack.app")
        .dir("/Applications/Docker.app")
        .dir("/Applications/Rancher Desktop.app")
        .exe("colima", "/opt/homebrew/bin/colima");
    let reports = detect_docker_family(&probe);
    for (product, needle) in [
        (Product::OrbStack, "open it yourself"),
        (Product::DockerDesktop, "open it yourself"),
        (Product::RancherDesktop, "open it yourself"),
        (Product::Colima, "colima start"),
    ] {
        match &product_report(&reports, product).state {
            RuntimeState::InstalledNotRunning { hint } => {
                assert!(hint.contains(needle), "{product:?}: {hint}")
            }
            other => panic!("{product:?}: {other:?}"),
        }
    }
    assert_eq!(
        product_report(&reports, Product::Podman).state,
        RuntimeState::Missing
    );
    assert!(
        probe.calls().is_empty(),
        "stat only: nothing run, nothing requested"
    );
}

#[test]
fn nothing_installed_lists_every_product_as_missing() {
    let probe = FakeProbe::mac();
    let reports = detect_docker_family(&probe);
    let kinds: Vec<_> = reports.iter().map(|r| r.id.as_str().to_string()).collect();
    assert_eq!(
        kinds,
        [
            "orbstack",
            "docker-desktop",
            "colima",
            "rancher-desktop",
            "podman"
        ]
    );
    assert!(
        reports
            .iter()
            .all(|r| r.state == RuntimeState::Missing && r.cli.is_none() && r.endpoint.is_none())
    );
}

#[test]
fn a_running_engine_without_its_cli_is_unsupported() {
    let path = sock(".orbstack/run/docker.sock");
    let probe = FakeProbe::mac().engine(&path, fx::ORBSTACK);
    let reports = detect_docker_family(&probe);
    match &product_report(&reports, Product::OrbStack).state {
        RuntimeState::Unsupported { reason } => assert!(reason.contains("docker"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(product_report(&reports, Product::OrbStack).cli().is_none());
}

#[test]
fn a_non_linux_engine_is_unsupported() {
    let windows = fx::Engine {
        version: Some(r#"{"Version":"27.0.1","Os":"windows"}"#),
        ..fx::LIMA
    };
    let probe = docker_mac().engine("/var/run/docker.sock", windows);
    let reports = detect_docker_family(&probe);
    let other = reports
        .iter()
        .find(|r| r.kind == RuntimeKind::DockerCompatible(Product::Other))
        .unwrap();
    assert!(
        matches!(&other.state, RuntimeState::Unsupported { reason } if reason.contains("windows"))
    );
}

#[test]
fn podman_machine_inspect_json_is_parsed_as_documented() {
    let machines = parse_machines(fx::PODMAN_MACHINE_RUNNING);
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].name, "podman-machine-default");
    assert!(machines[0].running);
    assert_eq!(
        machines[0].socket.as_deref(),
        Some(Path::new(PODMAN_SOCKET))
    );

    let stopped = parse_machines(fx::PODMAN_MACHINE_STOPPED);
    assert!(!stopped[0].running);
    assert_eq!(stopped[0].socket.as_deref(), Some(Path::new(PODMAN_SOCKET)));

    for nothing in [
        "",
        "[]",
        "Error: podman-machine-default: VM does not exist",
        "{}",
        "null",
    ] {
        assert!(parse_machines(nothing).is_empty(), "{nothing:?}");
    }
    // A relative or missing socket path is not a socket.
    let odd = parse_machines(
        r#"[{"Name":"m","State":"Running","ConnectionInfo":{"PodmanSocket":{"Path":"rel.sock"}}},{"Name":"n"}]"#,
    );
    assert_eq!((odd[0].running, odd[0].socket.clone()), (true, None));
    assert_eq!((odd[1].running, odd[1].socket.clone()), (false, None));
}

const PODMAN_BIN: &str = "/opt/homebrew/bin/podman";
const PODMAN_INSPECT: &str = "/opt/homebrew/bin/podman machine inspect";

fn podman_mac() -> FakeProbe {
    FakeProbe::mac()
        .exe("podman", PODMAN_BIN)
        .exe("docker", DOCKER_CLI)
}

#[test]
fn a_running_podman_machine_is_found_through_its_inspect_socket() {
    let probe = podman_mac()
        .command(PODMAN_INSPECT, output(0, fx::PODMAN_MACHINE_RUNNING))
        .engine(PODMAN_SOCKET, fx::PODMAN);
    let reports = detect_docker_family(&probe);
    let podman = product_report(&reports, Product::Podman);
    assert_running(podman, Product::Podman, "5.6.2", PODMAN_SOCKET);
    assert_eq!(podman.cli.as_deref(), Some(Path::new(PODMAN_BIN)));
    let cli = podman.cli().unwrap();
    assert_eq!(cli.dialect, Dialect::Podman);
    assert_eq!(
        cli.list().env,
        vec![(
            "CONTAINER_HOST".to_string(),
            format!("unix://{PODMAN_SOCKET}")
        )]
    );
    let calls = probe.calls();
    assert_eq!(calls[0], PODMAN_INSPECT);
    assert!(calls.contains(&format!("GET {PODMAN_SOCKET} /_ping")));
    assert!(calls.contains(&format!("GET {PODMAN_SOCKET} /version")));
}

#[test]
fn a_stopped_or_missing_podman_machine_is_hinted_not_started() {
    let stopped = podman_mac().command(PODMAN_INSPECT, output(0, fx::PODMAN_MACHINE_STOPPED));
    let reports = detect_docker_family(&stopped);
    assert!(matches!(&product_report(&reports, Product::Podman).state,
        RuntimeState::InstalledNotRunning { hint } if hint.contains("podman machine start")));
    assert_eq!(
        stopped.calls(),
        [PODMAN_INSPECT],
        "a stopped machine's socket is not even requested"
    );

    let none = podman_mac().command(PODMAN_INSPECT, output(125, ""));
    let reports = detect_docker_family(&none);
    assert!(matches!(&product_report(&reports, Product::Podman).state,
        RuntimeState::InstalledNotRunning { hint } if hint.contains("podman machine init")));

    let hung = podman_mac().command(PODMAN_INSPECT, Err(ProbeError::TimedOut));
    assert!(matches!(
        &product_report(&detect_docker_family(&hung), Product::Podman).state,
        RuntimeState::InstalledNotRunning { .. }
    ));

    let running_but_dead = podman_mac()
        .command(PODMAN_INSPECT, output(0, fx::PODMAN_MACHINE_RUNNING))
        .stale_socket(PODMAN_SOCKET);
    let reports = detect_docker_family(&running_but_dead);
    assert!(matches!(&product_report(&reports, Product::Podman).state,
        RuntimeState::InstalledNotRunning { hint } if hint.contains("does not answer")));
}

#[test]
fn detect_all_lists_apple_first_and_only_reads() {
    let orb = sock(".orbstack/run/docker.sock");
    let probe = FakeProbe::mac()
        .macos("27.0.1")
        .exe("container", "/usr/local/bin/container")
        .exe("docker", DOCKER_CLI)
        .command(
            "/usr/local/bin/container system status --format json",
            output(0, fx::APPLE_STATUS_RUNNING),
        )
        .engine(&orb, fx::ORBSTACK);
    let reports = detect_all(&probe);
    let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
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
    let running: Vec<_> = reports
        .iter()
        .filter(|r| r.is_running())
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(running, ["apple-container", "orbstack"]);
    assert_eq!(reports[0].caps.boundary, Boundary::VmPerContainer);
    // The fake panics on any command or request outside the read-only allow-list, so reaching
    // this line also proves detection never started, pulled, selected or ran anything.
    let calls = probe.calls();
    assert!(
        calls
            .iter()
            .all(|c| c.contains("sw_vers") || c.contains("system status") || c.starts_with("GET ")),
        "{calls:?}"
    );
    assert_eq!(PathBuf::from(&orb), reports[1].endpoint.clone().unwrap());
}
