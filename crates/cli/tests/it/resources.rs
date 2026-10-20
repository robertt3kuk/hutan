//! `hutan ps | top | kill | agent interrupt` against a real daemon and real shells.
//! Only processes started inside the test's sessions (or the one bystander) are signalled.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, command, finish};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::time::{sleep, timeout};

const THREE: &str = "sleep 60 & sleep 60 & wait";

async fn processes(t: &TestDaemon, id: &str) -> Vec<Value> {
    let out = t.hutan(&["--json", "ps", id]).await.ok();
    out.json()["sessions"][0]["processes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn wait_sleepers(t: &TestDaemon, id: &str, count: usize) -> Vec<u64> {
    timeout(WAIT, async {
        loop {
            let sleepers: Vec<u64> = processes(t, id)
                .await
                .iter()
                .filter(|p| p["name"] == "sleep")
                .map(|p| p["pid"].as_u64().unwrap())
                .collect();
            if sleepers.len() == count {
                return sleepers;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never saw {count} sleeps"))
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn agent(socket: &Path, session: &str, args: &[&str]) -> tokio::process::Command {
    let mut full = vec!["agent"];
    full.extend_from_slice(args);
    let mut command = command(Path::new(HUTAN), Some(socket), &full);
    command.env("HUTAN_SESSION_ID", session);
    command
}

async fn report_node(t: &TestDaemon, session: &str, node: &str, pid: Option<u64>) {
    let pid = pid.map(|p| p.to_string());
    let mut args = vec![
        "report", "--source", "test", "--agent", "fake", "--state", "working", "--node", node,
        "--kind", "sub",
    ];
    if let Some(pid) = &pid {
        args.extend(["--pid", pid]);
    }
    finish(agent(&t.paths.socket, session, &args)).await.ok();
    // Reports are fire and forget; wait for the tree to hold the node.
    timeout(WAIT, async {
        loop {
            let tree = t.hutan(&["--json", "agents", session]).await.ok().json();
            if tree.to_string().contains(node) {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the node was reported");
}

#[tokio::test(flavor = "multi_thread")]
async fn ps_shows_the_tree_and_kill_signals_one_process() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", THREE]).await;
    let sleepers = wait_sleepers(&t, &id, 2).await;

    let ps = t.hutan(&["ps"]).await.ok();
    assert!(ps.stdout.contains(&id), "{}", ps.stdout);
    assert!(ps.stdout.contains("sleep 60"), "{}", ps.stdout);
    assert!(ps.stdout.contains("hutand  pid"), "{}", ps.stdout);
    assert!(ps.stdout.contains("3 process(es)"), "{}", ps.stdout);
    let json = t.hutan(&["--json", "ps", &id]).await.ok().json();
    assert_eq!(json["sessions"][0]["totals"]["processes"], 3);
    assert!(json["daemon"]["pid"].as_u64().unwrap() > 0);

    // Another session's process is refused, with the reason; the session is untouched.
    let other = t.new_session(&[], &["sh", "-c", THREE]).await;
    let theirs = wait_sleepers(&t, &other, 2).await[0].to_string();
    let refused = t.hutan(&["kill", &id, "--pid", &theirs]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("descendants"), "{}", refused.stderr);
    assert!(alive(theirs.parse().unwrap()));

    let victim = sleepers[0].to_string();
    let out = t
        .hutan(&["kill", &id, "--pid", &victim, "--signal", "term"])
        .await
        .ok();
    assert!(out.stdout.contains("sent TERM"), "{}", out.stdout);
    timeout(WAIT, async {
        while alive(sleepers[0] as u32) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    wait_sleepers(&t, &id, 1).await;

    // `--signal` needs `--pid`.
    let usage = t.hutan(&["kill", &id, "--signal", "kill"]).await;
    assert_eq!(usage.code, 2, "{usage:?}");

    // Without --pid the whole session goes.
    let killed = t.hutan(&["kill", &id]).await.ok();
    assert!(
        killed.stdout.contains("killed session"),
        "{}",
        killed.stdout
    );
    t.wait_state(&id, "stopped").await;
    assert!(!alive(sleepers[1] as u32));
    t.hutan(&["kill", &other]).await.ok();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_are_marked_and_interrupted() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", THREE]).await;
    let sleepers = wait_sleepers(&t, &id, 2).await;
    report_node(&t, &id, "worker", Some(sleepers[0])).await;
    report_node(&t, &id, "thread", None).await;

    let ps = t.hutan(&["ps", &id]).await.ok();
    let marked: Vec<&str> = ps
        .stdout
        .lines()
        .filter(|l| l.contains("◆ agent worker"))
        .collect();
    assert_eq!(marked.len(), 1, "{}", ps.stdout);
    assert!(
        marked[0].contains(&sleepers[0].to_string()),
        "{}",
        marked[0]
    );
    assert!(ps.stdout.contains("thread (sub, working)"), "{}", ps.stdout);

    // A subagent the harness cannot stop on its own: refused, with the way out.
    let refused = t.hutan(&["agent", "interrupt", &id, "thread"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("cannot stop a single subagent"),
        "{}",
        refused.stderr
    );
    // A process: SIGINT is ignored by this background job, --force follows with SIGTERM.
    let out: Out = t
        .hutan(&["agent", "interrupt", &id, "worker", "--force"])
        .await
        .ok();
    assert!(
        out.stdout.contains("SIGINT, then SIGTERM"),
        "{}",
        out.stdout
    );
    timeout(WAIT, async {
        while alive(sleepers[0] as u32) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let json = t
        .hutan(&["--json", "agent", "interrupt", &id, "nobody"])
        .await;
    assert_eq!(json.code, 1, "{json:?}");

    t.hutan(&["kill", &id]).await.ok();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_the_main_agent_of_a_terminal_types_ctrl_c() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["cat"]).await;
    let out = t.hutan(&["agent", "interrupt", &id]).await.ok();
    assert!(out.stdout.contains("^C was typed"), "{}", out.stdout);
    t.wait_state(&id, "exited").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn top_watches_and_prints_the_busiest_first() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "yes > /dev/null & wait"])
        .await;
    timeout(WAIT, async {
        loop {
            if processes(&t, &id).await.iter().any(|p| p["name"] == "yes") {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    let mut top = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["top", "--interval", "1000"],
    );
    top.stdout(Stdio::piped());
    let mut child = top.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    // Two prints: the one that answers the subscription and the first sample after it.
    let mut text = String::new();
    let mut prints = 0;
    timeout(WAIT, async {
        while let Some(line) = lines.next_line().await.unwrap() {
            text.push_str(&line);
            text.push('\n');
            if line.starts_with("hutand  pid") {
                prints += 1;
                if prints == 2 {
                    return;
                }
            }
        }
    })
    .await
    .expect("top printed twice");
    child.kill().await.unwrap();
    assert!(text.contains(&id), "{text}");
    // The second print has a CPU figure for `yes`.
    let second = text.rsplit_once(&id).unwrap().1;
    let yes = second
        .lines()
        .find(|l| l.trim_end().ends_with(" yes"))
        .unwrap();
    let cpu: f64 = yes
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap_or(0.0);
    assert!(cpu > 5.0, "{yes}");
    t.hutan(&["kill", &id]).await.ok();
    t.stop().await;
}
