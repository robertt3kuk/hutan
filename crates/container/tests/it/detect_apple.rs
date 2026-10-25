//! Apple `container`: detection from recorded `system status` / `system version` output.

use hutan_container::detect::apple::{
    AppleStatus, CONTAINER_FLOOR, MACOS_FLOOR, ParseError, detect_apple, parse_cli_version,
    parse_status,
};
use hutan_container::probe::ProbeError;
use hutan_container::{Boundary, Dialect, RuntimeKind, RuntimeState, Version};

use crate::common::fake::{FakeProbe, fixtures as fx, output};

const CLI: &str = "/usr/local/bin/container";
const STATUS: &str = "/usr/local/bin/container system status --format json";
const VERSION: &str = "/usr/local/bin/container system version --format json";
const SW_VERS: &str = "/usr/bin/sw_vers -productVersion";

fn installed() -> FakeProbe {
    FakeProbe::mac().exe("container", CLI).macos("27.0.1")
}

#[test]
fn status_parsing_covers_every_state() {
    assert_eq!(
        parse_status(fx::APPLE_STATUS_UNREGISTERED),
        Ok(AppleStatus::Unregistered)
    );
    assert_eq!(
        parse_status(fx::APPLE_STATUS_NOT_RUNNING),
        Ok(AppleStatus::NotRunning)
    );
    assert_eq!(
        parse_status(fx::APPLE_STATUS_RUNNING),
        Ok(AppleStatus::Running {
            client_version: Some("1.6.1".to_string()),
            server_version: Some(
                "container-apiserver version 1.6.1 (build: release, commit: unspecified)"
                    .to_string()
            ),
        })
    );
    // Running without the optional members is still running.
    assert_eq!(
        parse_status(r#"{"status":"running"}"#),
        Ok(AppleStatus::Running {
            client_version: None,
            server_version: None
        })
    );
    assert_eq!(
        parse_status(fx::APPLE_STATUS_UNKNOWN),
        Err(ParseError::UnknownStatus("starting".to_string()))
    );
    for garbage in [
        "",
        "apiserver is not running",
        "[]",
        "{\"state\":\"running\"}",
    ] {
        assert!(
            matches!(parse_status(garbage), Err(ParseError::Json(_))),
            "{garbage:?}"
        );
    }
}

#[test]
fn cli_version_is_the_container_entry() {
    assert_eq!(
        parse_cli_version(fx::APPLE_VERSION_WITH_SERVER),
        Ok(Some(Version::new(1, 2, 3)))
    );
    assert_eq!(
        parse_cli_version(fx::APPLE_VERSION_CLIENT_ONLY),
        Ok(Some(Version::new(1, 6, 1)))
    );
    assert_eq!(parse_cli_version("[]"), Ok(None));
    assert!(parse_cli_version("not json").is_err());
}

#[test]
fn missing_when_there_is_no_cli() {
    let probe = FakeProbe::mac().macos("27.0.1");
    let report = detect_apple(&probe);
    assert_eq!(report.state, RuntimeState::Missing);
    assert_eq!(report.kind, RuntimeKind::AppleContainer);
    assert_eq!(report.id.as_str(), "apple-container");
    assert!(report.cli.is_none() && report.cli().is_none());
    assert!(
        probe.calls().is_empty(),
        "nothing is run when there is no binary"
    );
}

#[test]
fn running_reports_the_server_version_and_runs_only_two_queries() {
    let probe = installed().command(STATUS, output(0, fx::APPLE_STATUS_RUNNING));
    let report = detect_apple(&probe);
    assert_eq!(report.state, RuntimeState::Running);
    assert_eq!(report.version.as_deref(), Some("1.6.1"));
    assert_eq!(report.cli.as_deref(), Some(std::path::Path::new(CLI)));
    assert_eq!(report.caps.boundary, Boundary::VmPerContainer);
    assert_eq!(report.checked_at_ms, 1_790_000_000_000);
    assert_eq!(probe.calls(), [SW_VERS, STATUS]);

    let cli = report.cli().expect("a running runtime has a launcher");
    assert_eq!((cli.dialect, cli.endpoint), (Dialect::Apple, None));
}

#[test]
fn not_running_and_unregistered_exit_1_with_json_and_name_the_users_command() {
    for stdout in [fx::APPLE_STATUS_NOT_RUNNING, fx::APPLE_STATUS_UNREGISTERED] {
        let probe = installed()
            .command(STATUS, output(1, stdout))
            .command(VERSION, output(0, fx::APPLE_VERSION_CLIENT_ONLY));
        let report = detect_apple(&probe);
        match &report.state {
            RuntimeState::InstalledNotRunning { hint } => {
                assert!(hint.contains("container system start"), "{hint}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            report.version.as_deref(),
            Some("1.6.1"),
            "the CLI answers `system version` without the services"
        );
        assert!(
            report.cli().is_none(),
            "a runtime that is not running yields no launcher"
        );
        // Reads only: the command to start it is never run.
        assert_eq!(probe.calls(), [SW_VERS, STATUS, VERSION]);
    }
}

#[test]
fn not_running_without_a_readable_version_is_still_installed() {
    let probe = installed().command(STATUS, output(1, fx::APPLE_STATUS_NOT_RUNNING));
    let report = detect_apple(&probe);
    assert!(matches!(
        report.state,
        RuntimeState::InstalledNotRunning { .. }
    ));
    assert_eq!(report.version, None);
}

#[test]
fn old_macos_is_unsupported_and_the_runtime_is_not_even_queried() {
    let probe = FakeProbe::mac().exe("container", CLI).macos("15.6.1");
    let report = detect_apple(&probe);
    match &report.state {
        RuntimeState::Unsupported { reason } => {
            assert!(
                reason.contains("26") && reason.contains("15.6.1"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(probe.calls(), [SW_VERS]);
    assert_eq!(MACOS_FLOOR, Version::new(26, 0, 0));
}

#[test]
fn unknown_macos_version_fails_closed() {
    let probe = FakeProbe::mac().exe("container", CLI);
    assert!(matches!(
        detect_apple(&probe).state,
        RuntimeState::Unsupported { .. }
    ));
}

#[test]
fn versions_below_the_tested_floor_are_unsupported_running_or_not() {
    let old_running = fx::APPLE_STATUS_RUNNING.replace("1.6.1", "1.5.0");
    let report = detect_apple(&installed().command(STATUS, output(0, &old_running)));
    match &report.state {
        RuntimeState::Unsupported { reason } => assert!(
            reason.contains("1.5.0") && reason.contains("1.6.1"),
            "{reason}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(report.version.as_deref(), Some("1.5.0"));

    let old_client = fx::APPLE_VERSION_CLIENT_ONLY.replace("1.6.1", "1.2.0");
    let report = detect_apple(
        &installed()
            .command(STATUS, output(1, fx::APPLE_STATUS_NOT_RUNNING))
            .command(VERSION, output(0, &old_client)),
    );
    assert!(matches!(report.state, RuntimeState::Unsupported { .. }));
    assert_eq!(CONTAINER_FLOOR, Version::new(1, 6, 1));
}

#[test]
fn unreadable_or_failed_status_is_unsupported_not_a_guess() {
    for answer in [
        output(0, fx::APPLE_STATUS_UNKNOWN),
        output(1, "Error: something else entirely"),
        output(0, ""),
        Err(ProbeError::TimedOut),
        Err(ProbeError::NotFound),
    ] {
        let report = detect_apple(&installed().command(STATUS, answer.clone()));
        assert!(
            matches!(report.state, RuntimeState::Unsupported { .. }),
            "{answer:?} → {:?}",
            report.state
        );
    }
}
