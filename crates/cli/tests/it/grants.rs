//! `hutan grants ls|revoke` (protocol 0.31, `docs/control.md` § Mawas) against a real daemon
//! whose database already holds grants, and the same commands typed inside an agent's session,
//! where they are refused.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, command, finish};
use hutan_core::{SessionId, WorkspaceId};
use hutan_storage::{GrantRow, Storage};
use tokio::time::{sleep, timeout};

fn workspace() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3WS".parse().unwrap()
}

fn chat() -> SessionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3CH".parse().unwrap()
}

/// A daemon that starts with three grants in its database.
async fn with_grants() -> TestDaemon {
    TestDaemon::start_with(|config| {
        let storage = Storage::open(&config.paths.database).unwrap();
        for (id, tool, workspace_id, chat_session_id) in [
            (1, "session.rename", Some(workspace()), None),
            (2, "ui.run:workbench::SplitRight", None, Some(chat())),
            (3, "task.create", Some(workspace()), None),
        ] {
            storage
                .control_grants()
                .insert(GrantRow {
                    id,
                    tool: tool.into(),
                    workspace_id,
                    chat_session_id,
                    target_session_id: None,
                    created_ms: 1_791_894_190_000 + id,
                    created_by: "desktop".into(),
                })
                .unwrap();
        }
        storage.close();
    })
    .await
}

async fn ids(t: &TestDaemon) -> Vec<u64> {
    let out = t.hutan(&["grants", "ls", "--json"]).await.ok().json();
    out["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_u64().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_shows_what_mawas_may_do_and_revoke_removes_one_or_all() {
    let t = with_grants().await;

    let json = t.hutan(&["grants", "ls", "--json"]).await.ok().json();
    let grants = json["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 3);
    assert_eq!(grants[0]["id"], 3, "newest first");
    assert_eq!(grants[0]["tool"], "task.create");
    assert_eq!(grants[0]["workspace_id"], workspace().to_string());
    assert!(grants[0].get("chat_session_id").is_none());
    assert_eq!(grants[1]["chat_session_id"], chat().to_string());
    assert!(grants[1].get("workspace_id").is_none());

    let table = t.hutan(&["grants", "ls"]).await.ok().stdout;
    assert!(table.contains("session.rename"), "{table}");
    assert!(
        table.contains(&format!("workspace {}", workspace())),
        "{table}"
    );
    assert!(table.contains(&format!("chat {}", chat())), "{table}");

    // One grant goes; the others stay.
    let revoked = t.hutan(&["grants", "revoke", "1", "--json"]).await.ok();
    assert_eq!(revoked.json()["revoked"], serde_json::json!([1]));
    assert_eq!(ids(&t).await, [3, 2]);
    // A grant that is gone is an error with a message.
    let again = t.hutan(&["grants", "revoke", "1"]).await;
    assert_eq!(again.code, 1, "{again:?}");
    assert!(again.stderr.contains("does not exist"), "{again:?}");
    // `--all` removes the rest; there is nothing left to list.
    t.hutan(&["grants", "revoke", "--all"]).await.ok();
    assert!(ids(&t).await.is_empty());
    let empty = t.hutan(&["grants", "ls"]).await.ok().stdout;
    assert!(empty.contains("no grants"), "{empty}");
    // Without an id or `--all` there is nothing to do.
    assert_ne!(t.hutan(&["grants", "revoke"]).await.code, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_typed_inside_an_agents_session_cannot_read_or_revoke_grants() {
    let t = with_grants().await;
    let session = t.new_session(&[], &["sh", "-c", "sleep 60"]).await;
    // An agent reports itself in the session, as its hooks do.
    let mut report = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    );
    report.env("HUTAN_SESSION_ID", &session);
    finish(report).await.ok();
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

    for args in [
        &["grants", "ls"][..],
        &["grants", "revoke", "1"][..],
        &["grants", "revoke", "--all"][..],
    ] {
        let mut agent = command(Path::new(HUTAN), Some(&t.paths.socket), args);
        agent.env("HUTAN_SESSION_ID", &session);
        let out = finish(agent).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(
            out.stderr.contains("only available to the user"),
            "{args:?}: {out:?}"
        );
    }
    assert_eq!(ids(&t).await, [3, 2, 1], "nothing was revoked");
    t.stop().await;
}
