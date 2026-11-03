//! `hutan telemetry status|enable|disable` (TEL-1, protocol 0.74, ADR 0026) against a real
//! daemon: off until the user enables an agent, a socket on the loopback only while one is, and
//! the user's alone — refused for a command that runs inside an agent's session.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, command, finish};
use tokio::time::{sleep, timeout};

#[tokio::test(flavor = "multi_thread")]
async fn status_says_off_and_enable_and_disable_open_and_close_the_receiver() {
    let t = TestDaemon::start().await;
    let text = t.hutan(&["telemetry", "status"]).await.ok().stdout;
    assert!(text.contains("OpenTelemetry receiver: off"), "{text}");
    assert!(text.contains("agents on: none"), "{text}");
    assert!(text.contains("hutan telemetry enable <agent>"), "{text}");
    assert!(text.contains("no session has reported yet"), "{text}");
    let json = t
        .hutan(&["telemetry", "status", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["listener"], "off");
    assert!(json.get("port").is_none());
    assert_eq!(json["agents"].as_array().unwrap().len(), 9);

    let text = t
        .hutan(&["telemetry", "enable", "claude"])
        .await
        .ok()
        .stdout;
    assert!(text.contains("listening on 127.0.0.1:"), "{text}");
    assert!(text.contains("agents on: claude"), "{text}");
    assert!(text.contains("HUTAN_OTLP_ENDPOINT"), "{text}");
    let json = t
        .hutan(&["telemetry", "status", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["listener"], "listening");
    let port = json["port"].as_u64().unwrap();
    assert!((49152..=65535).contains(&port));
    // The port is where `daemon.toml` says, and `hutan config` agrees.
    let setting = t
        .hutan(&["config", "get", "telemetry.agents.claude", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(setting["value"], true);

    let text = t
        .hutan(&["telemetry", "disable", "claude"])
        .await
        .ok()
        .stdout;
    assert!(text.contains("OpenTelemetry receiver: off"), "{text}");

    // An agent Hutan has no mapper for is a usage error, not a setting.
    let out = t.hutan(&["telemetry", "enable", "nonesuch"]).await;
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("nonesuch"), "{}", out.stderr);
    t.stop().await;
}

/// `hutan` as typed in a Hutan session: `HUTAN_SESSION_ID` set.
fn in_session(socket: &Path, session: &str, args: &[&str]) -> tokio::process::Command {
    let mut command = command(Path::new(HUTAN), Some(socket), args);
    command.env("HUTAN_SESSION_ID", session);
    command
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_cannot_turn_the_receiver_on_whatever_the_policy() {
    let t = TestDaemon::start_with(|config| {
        std::fs::write(&config.paths.config_file, "[control]\nagents = \"allow\"\n").unwrap();
    })
    .await;
    let session = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    // An agent reports itself in the session, as its hooks do: from now on a command typed there
    // is the agent's.
    finish(in_session(
        &t.paths.socket,
        &session,
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    ))
    .await
    .ok();
    timeout(WAIT, async {
        loop {
            if t.session(&session).await.get("agent").is_some() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the report never showed up");

    let out = finish(in_session(
        &t.paths.socket,
        &session,
        &["telemetry", "enable", "claude"],
    ))
    .await;
    assert_ne!(out.code, 0, "{out:?}");
    assert!(
        out.stderr.contains("user") || out.stderr.contains("forbidden"),
        "{}",
        out.stderr
    );
    // Reading the status is allowed; the receiver stayed off.
    let status = finish(in_session(
        &t.paths.socket,
        &session,
        &["telemetry", "status", "--json"],
    ))
    .await
    .ok()
    .json();
    assert_eq!(status["listener"], "off");
    // The same command in a plain shell session is the user's.
    t.hutan(&["telemetry", "enable", "claude"]).await.ok();
    t.stop().await;
}
