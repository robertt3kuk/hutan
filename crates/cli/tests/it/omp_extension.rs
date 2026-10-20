//! The OMP extension (`integrations/omp/hutan-agent-state.ts`) driven by a fake OMP event
//! source under Bun, with a stand-in `hutan` that records its command lines. Skipped when
//! `bun` is not installed; the gate does not depend on it.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn find_bun() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("bun"))
        .find(|candidate| candidate.is_file())
}

fn extension() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/omp/hutan-agent-state.ts")
}

const HARNESS: &str = r#"
import extension from "EXTENSION";

type Handlers = Record<string, (event: unknown, ctx: unknown) => void>;
// OMP binds the extension factory to every session: one instance for the main session, one per
// subagent.
const bind = (): Handlers => {
  const handlers: Handlers = {};
  extension({ on: (name: string, handler: (event: unknown, ctx: unknown) => void) => { handlers[name] = handler; } });
  return handlers;
};
const handlers = bind();
const subHandlers = bind();
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const main = { kind: "main", id: "Main", name: "main", depth: 0 };
const ui = { hasUI: true, isIdle: () => true, sessionManager: { getSessionId: () => "0f4c7d52-aaaa" }, agent: main, model: { id: "claude-opus-5" } };
const subagent = { hasUI: false, agent: { kind: "sub", id: "0-Explore", name: "explore", depth: 1, parentId: "Main" }, model: { id: "claude-sonnet-5" } };
const printMode = { hasUI: false };

if (Object.keys(handlers).length > 0) {
  handlers.session_start?.({}, ui);
  await sleep(150);
  handlers.agent_start?.({}, printMode);  // neither main nor a subagent: ignored
  handlers.agent_start?.({}, ui);
  subHandlers.agent_start?.({}, subagent);
  await sleep(150);
  handlers.tool_approval_requested?.({ reason: "Bash approval", toolName: "Bash" }, ui);
  await sleep(150);
  handlers.tool_approval_resolved?.({}, ui);
  await sleep(150);
  handlers.agent_end?.({}, ui);
  subHandlers.agent_end?.({ willContinue: true }, subagent);   // the loop goes on: no report
  await sleep(150);
  subHandlers.agent_end?.({ willContinue: false }, subagent);
  // Wait until every queued report has run (the extension serializes them, each bounded by its
  // own timeout), instead of guessing a sleep long enough for a loaded machine.
  const want = Number(process.env.HARNESS_EXPECT_CALLS ?? "0");
  const log = process.env.HARNESS_CALL_LOG ?? "";
  const deadline = Date.now() + 20_000;
  while (want > 0 && Date.now() < deadline) {
    const text = await Bun.file(log).text().catch(() => "");
    if (text.split("\n").filter(Boolean).length >= want) break;
    await sleep(25);
  }
  await sleep(200);
}
"#;

/// Runs the harness and returns the lines the fake `hutan` recorded.
fn run(env: &[(&str, &str)]) -> Option<Vec<String>> {
    let bun = find_bun()?;
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("calls");
    let fake = dir.path().join("hutan");
    fs::write(
        &fake,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.path().join("harness.ts");
    let extension = extension().canonicalize().unwrap();
    fs::write(
        &harness,
        HARNESS.replace("EXTENSION", &extension.to_string_lossy()),
    )
    .unwrap();

    let mut command = Command::new(bun);
    command
        .arg("run")
        .arg(&harness)
        .env_remove("HUTAN_ENV")
        .env_remove("HUTAN_SESSION_ID")
        .env("HUTAN_BIN", &fake)
        .env("HARNESS_CALL_LOG", &log);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

#[test]
fn the_extension_reports_turns_approvals_and_the_resume_command() {
    let Some(calls) = run(&[
        ("HUTAN_ENV", "1"),
        ("HUTAN_SESSION_ID", "01K7F3B5N2Q9W8E4R6T1Y0V3KM"),
        ("HARNESS_EXPECT_CALLS", "7"),
    ]) else {
        eprintln!("skipping: bun is not installed");
        return;
    };

    // The daemon orders reports by `seq`, not by arrival: a report whose `hutan` process is
    // slow to start is overtaken once the extension's per-call timeout lets the next one go.
    let seq_of = |call: &String| -> u64 {
        call.split_whitespace()
            .find_map(|word| word.strip_prefix("--seq="))
            .unwrap()
            .parse()
            .unwrap()
    };
    let mut calls = calls;
    calls.sort_by_key(seq_of);
    let seqs: Vec<u64> = calls.iter().map(seq_of).collect();
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");

    let node_of = |call: &String| -> String {
        call.split_whitespace()
            .find_map(|word| word.strip_prefix("--node="))
            .unwrap()
            .to_owned()
    };
    let state_of = |call: &String| -> String {
        call.split_whitespace()
            .skip_while(|word| *word != "--state")
            .nth(1)
            .unwrap()
            .to_owned()
    };
    let (main, sub): (Vec<String>, Vec<String>) = calls
        .iter()
        .cloned()
        .partition(|call| node_of(call) == "Main");
    let states: Vec<String> = main.iter().map(state_of).collect();
    assert_eq!(
        states,
        ["idle", "working", "blocked", "working", "idle"],
        "{calls:#?}"
    );
    for call in &main {
        assert!(
            call.starts_with("agent report --source omp-extension --agent omp --state "),
            "{call}"
        );
        assert!(
            call.contains("--kind=main --name=main --model=claude-opus-5"),
            "{call}"
        );
        assert!(!call.contains("--parent"), "{call}");
        assert!(
            call.ends_with("--agent-session-id=0f4c7d52-aaaa -- omp --resume=0f4c7d52-aaaa"),
            "{call}"
        );
    }
    assert!(main[2].contains("--message=Bash approval"), "{}", main[2]);

    // The subagent is a node of its own under the main agent: working, then finished. It
    // carries no resume command, and `willContinue: true` did not end it.
    let sub_states: Vec<String> = sub.iter().map(state_of).collect();
    assert_eq!(sub_states, ["working", "idle"], "{calls:#?}");
    for call in &sub {
        assert!(
            call.contains(
                "--node=0-Explore --kind=sub --parent=Main --name=explore --model=claude-sonnet-5"
            ),
            "{call}"
        );
        assert!(!call.contains("--resume"), "{call}");
    }
    assert!(sub[1].contains("--outcome=completed"), "{}", sub[1]);
}

#[test]
fn outside_hutan_the_extension_does_nothing() {
    let Some(calls) = run(&[]) else {
        eprintln!("skipping: bun is not installed");
        return;
    };
    assert!(calls.is_empty(), "{calls:?}");
}
