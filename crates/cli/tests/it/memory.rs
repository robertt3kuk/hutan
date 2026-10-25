//! `hutan memory ls|add|edit|archive|restore|history|proposals` (protocol 0.32,
//! `docs/assistant.md` § Memory) against a real daemon, and the same commands typed inside an
//! agent's session, where they are refused.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, command, finish};
use hutan_client::{Client, ConnectOptions};
use hutan_core::SessionId;
use hutan_protocol::memory::{MemoryCategory, MemoryOp, MemoryPropose, MemoryScope};
use hutan_protocol::messages::{ClientInfo, ClientKind, Origin};
use serde_json::Value;
use tokio::time::{sleep, timeout};

/// A terminal session in which an agent reported itself: what a connection from inside it is.
async fn agent_session(t: &TestDaemon) -> SessionId {
    let session = t.new_session(&[], &["sh", "-c", "sleep 60"]).await;
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
    session.parse().unwrap()
}

async fn agent_client(t: &TestDaemon, session: SessionId) -> Client {
    Client::connect(ConnectOptions {
        endpoint: Some(t.paths.socket.clone().into()),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Other,
            version: "test".into(),
            origin: Some(Origin {
                session_id: session,
            }),
        },
    })
    .await
    .unwrap()
}

fn proposal(op: &str, body: &str) -> MemoryPropose {
    MemoryPropose {
        operation_id: op.into(),
        op: MemoryOp::Create,
        scope: Some(MemoryScope::Global),
        workspace_id: None,
        category: Some(MemoryCategory::Fact),
        entry_id: None,
        expected_revision: None,
        body: Some(body.into()),
    }
}

async fn entries(t: &TestDaemon, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["memory", "ls", "--json"];
    args.extend_from_slice(extra);
    t.hutan(&args).await.ok().json()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn add_ls_edit_archive_restore_and_history_round_trip() {
    let t = TestDaemon::start().await;
    let added = t
        .hutan(&["memory", "add", "Prefers short answers.", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(added["status"], "applied");
    assert_eq!(
        (added["revision"].as_u64(), added["scope_revision"].as_u64()),
        (Some(1), Some(1))
    );
    let id = added["entry_id"].as_str().unwrap().to_owned();

    // Default category of a global entry is `profile`; the listing says so, and what it is.
    let listed = entries(&t, &[]).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], id.as_str());
    assert_eq!(listed[0]["category"], "profile");
    assert_eq!(listed[0]["scope"], "global");
    assert_eq!(listed[0]["inferred"], false);
    assert_eq!(listed[0]["provenance"]["client"], "cli");
    let table = t.hutan(&["memory", "ls"]).await.ok().stdout;
    assert!(
        table.contains("Prefers short answers.") && table.contains(&id),
        "{table}"
    );
    let human = t
        .hutan(&["memory", "add", "Uses jj.", "--category", "fact"])
        .await
        .ok()
        .stdout;
    assert!(
        human.starts_with("Saved: entry "),
        "only a receipt says saved: {human}"
    );
    assert!(
        human.contains("revision 1") && human.contains("scope revision 2"),
        "{human}"
    );

    // Edit by id prefix; the entry moves to revision 2.
    let prefix = &id[..16];
    let edited = t
        .hutan(&[
            "memory",
            "edit",
            prefix,
            "Prefers very short answers.",
            "--json",
        ])
        .await
        .ok()
        .json();
    assert_eq!(edited["revision"], 2);
    let listed = entries(&t, &["--category", "profile"]).await;
    assert_eq!(
        (listed.len(), listed[0]["body"].as_str()),
        (1, Some("Prefers very short answers."))
    );
    assert_eq!(entries(&t, &["--category", "fact"]).await.len(), 1);

    // Archive hides it from the default listing; restore brings it back; history has all of it.
    t.hutan(&["memory", "archive", &id, "--json"]).await.ok();
    assert_eq!(entries(&t, &[]).await.len(), 1);
    let all = entries(&t, &["--archived"]).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|e| e["state"] == "archived"));
    let flags = t.hutan(&["memory", "ls", "--archived"]).await.ok().stdout;
    assert!(flags.contains("archived"), "{flags}");
    t.hutan(&["memory", "restore", prefix, "--json"]).await.ok();
    assert_eq!(entries(&t, &[]).await.len(), 2);
    let history = t
        .hutan(&["memory", "history", &id, "--json"])
        .await
        .ok()
        .json();
    let ops: Vec<&str> = history["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["operation"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["restore", "archive", "update", "create"]);
    let text = t.hutan(&["memory", "history", &id]).await.ok().stdout;
    assert!(text.contains("entered by you in the cli"), "{text}");

    // Unknown and too-short ids are errors with a message.
    let unknown = t
        .hutan(&["memory", "archive", "01K7F3B5N2Q9W8E4R6T1Y0V3KM"])
        .await;
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert!(unknown.stderr.contains("no memory entry"), "{unknown:?}");
    let short = t.hutan(&["memory", "archive", &id[..3]]).await;
    assert_eq!(short.code, 1, "{short:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_refused_and_a_workspace_entry_needs_a_workspace() {
    let t = TestDaemon::start().await;
    let refused = t
        .hutan(&[
            "memory",
            "add",
            "token = ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        ])
        .await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("Memory is not a secret store"),
        "{refused:?}"
    );
    assert!(entries(&t, &[]).await.is_empty());

    let folder = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(folder.path()).unwrap();
    let ws = t
        .hutan(&["ws", "new", "alpha", folder.to_str().unwrap(), "--json"])
        .await
        .ok()
        .json();
    let ws_id = ws["id"].as_str().unwrap().to_owned();
    let added = t
        .hutan(&[
            "memory",
            "add",
            "Releases come from trunk.",
            "--ws",
            "alpha",
            "--json",
        ])
        .await
        .ok()
        .json();
    assert_eq!(added["scope_revision"], 1);
    let listed = entries(&t, &["--ws", &ws_id]).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["scope"], "workspace");
    assert_eq!(
        listed[0]["category"], "note",
        "the default of a workspace entry"
    );
    assert_eq!(listed[0]["workspace_id"], ws_id.as_str());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proposals_wait_for_the_user_who_approves_or_rejects_them_by_id() {
    let t = TestDaemon::start().await;
    let session = agent_session(&t).await;
    let agent = agent_client(&t, session).await;
    let none = t.hutan(&["memory", "proposals"]).await.ok().stdout;
    assert!(none.contains("no proposals"), "{none}");

    let first = agent
        .memory_propose(proposal("one", "Staging is called kite."))
        .await
        .unwrap();
    let second = agent
        .memory_propose(proposal("two", "REJECTED-CLAIM"))
        .await
        .unwrap();
    assert_eq!(first.status, "proposed");
    let listed = t
        .hutan(&["memory", "proposals", "--json"])
        .await
        .ok()
        .json();
    let proposals = listed["proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 2);
    assert_eq!(proposals[0]["state"], "pending");
    assert_eq!(
        proposals[0]["provenance"]["session_id"],
        session.to_string()
    );
    let table = t.hutan(&["memory", "proposals"]).await.ok().stdout;
    assert!(
        table.contains("Staging is called kite.") && table.contains("pending"),
        "{table}"
    );
    assert!(
        entries(&t, &[]).await.is_empty(),
        "nothing is saved by proposing"
    );

    // Approve by an id prefix: applied as listed, and the entry is inferred.
    let prefix = first.proposal_id.to_string()[..16].to_owned();
    let approved = t
        .hutan(&["memory", "proposals", "--approve", &prefix, "--json"])
        .await
        .ok()
        .json();
    assert_eq!(approved["proposal"]["state"], "applied");
    assert_eq!(approved["receipt"]["status"], "applied");
    let listed = entries(&t, &[]).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["body"], "Staging is called kite.");
    assert_eq!(listed[0]["inferred"], true);
    let table = t.hutan(&["memory", "ls"]).await.ok().stdout;
    assert!(table.contains("inferred"), "{table}");

    // Reject: nothing is saved, and the proposal is no longer pending.
    let rejected = t
        .hutan(&[
            "memory",
            "proposals",
            "--reject",
            &second.proposal_id.to_string(),
        ])
        .await
        .ok()
        .stdout;
    assert!(rejected.contains("nothing was saved"), "{rejected}");
    assert_eq!(entries(&t, &[]).await.len(), 1);
    let all = t
        .hutan(&["memory", "proposals", "--all", "--json"])
        .await
        .ok()
        .json();
    let states: Vec<&str> = all["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["applied", "rejected"]);
    let pending = t
        .hutan(&["memory", "proposals", "--json"])
        .await
        .ok()
        .json();
    assert!(pending["proposals"].as_array().unwrap().is_empty());
    // An answered proposal is not found among the pending ones.
    let again = t
        .hutan(&[
            "memory",
            "proposals",
            "--approve",
            &second.proposal_id.to_string(),
        ])
        .await;
    assert_eq!(again.code, 1, "{again:?}");
    assert!(again.stderr.contains("no pending proposal"), "{again:?}");

    // A proposal for an entry that moved on is a visible conflict and a failed command.
    let entry_id = listed[0]["id"].as_str().unwrap().to_owned();
    let stale = agent
        .memory_propose(MemoryPropose {
            operation_id: "three".into(),
            op: MemoryOp::Update,
            scope: None,
            workspace_id: None,
            category: None,
            entry_id: Some(entry_id.clone()),
            expected_revision: Some(1),
            body: Some("Staging is called heron.".into()),
        })
        .await
        .unwrap();
    t.hutan(&[
        "memory",
        "edit",
        &entry_id,
        "Staging is called kite (confirmed).",
    ])
    .await
    .ok();
    let conflict = t
        .hutan(&[
            "memory",
            "proposals",
            "--approve",
            &stale.proposal_id.to_string(),
        ])
        .await;
    assert_eq!(conflict.code, 1, "{conflict:?}");
    assert!(
        conflict.stdout.contains("could not be applied"),
        "{conflict:?}"
    );
    assert!(conflict.stderr.contains("conflict"), "{conflict:?}");
    let all = t
        .hutan(&["memory", "proposals", "--all", "--json"])
        .await
        .ok()
        .json();
    assert!(
        all["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["state"] == "conflict"),
        "{all}"
    );
    assert_eq!(
        entries(&t, &[]).await[0]["body"],
        "Staging is called kite (confirmed)."
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_typed_inside_an_agents_session_cannot_change_memory() {
    let t = TestDaemon::start().await;
    t.hutan(&["memory", "add", "Prefers short answers."])
        .await
        .ok();
    let session = agent_session(&t).await;
    let id = entries(&t, &[]).await[0]["id"].as_str().unwrap().to_owned();
    for args in [
        &["memory", "add", "x"][..],
        &["memory", "edit", id.as_str(), "x"][..],
        &["memory", "archive", id.as_str()][..],
        &["memory", "restore", id.as_str()][..],
        &["memory", "proposals"][..],
        &["memory", "import", "whatever.md"][..],
        &["memory", "files"][..],
    ] {
        let mut agent = command(Path::new(HUTAN), Some(&t.paths.socket), args);
        agent.env("HUTAN_SESSION_ID", session.to_string());
        let out = finish(agent).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(
            out.stderr.contains("only available to the user"),
            "{args:?}: {out:?}"
        );
    }
    // Reading is free for it.
    let mut read = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["memory", "ls", "--json"],
    );
    read.env("HUTAN_SESSION_ID", session.to_string());
    let out = finish(read).await.ok();
    assert_eq!(out.json()["entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        entries(&t, &["--archived"]).await.len(),
        1,
        "nothing changed"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn export_writes_markdown_or_json_and_import_makes_proposals_from_either() {
    let source = TestDaemon::start().await;
    source
        .hutan(&["memory", "add", "Prefers short answers."])
        .await
        .ok();
    source
        .hutan(&[
            "memory",
            "add",
            "Uses jj.\n\nSecond paragraph.",
            "--category",
            "fact",
        ])
        .await
        .ok();
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("memory.md");
    let json = dir.path().join("memory.json");
    let done = source
        .hutan(&["memory", "export", md.to_str().unwrap(), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(
        (
            done["format"].as_str(),
            done["entries"].as_u64(),
            done["scope"].as_str()
        ),
        (Some("md"), Some(2), Some("global"))
    );
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.starts_with("<!-- Written by Hutan"), "{text}");
    assert!(
        text.contains("# Profile")
            && text.contains("# Facts")
            && text.contains("Second paragraph.")
    );
    // The format follows the extension; an existing file is not replaced without --force.
    let human = source
        .hutan(&["memory", "export", json.to_str().unwrap()])
        .await
        .ok()
        .stdout;
    assert!(
        human.starts_with("Exported 2 entries (global, json) to "),
        "{human}"
    );
    let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(parsed["format"], "hutan-memory");
    assert_eq!(parsed["entries"].as_array().unwrap().len(), 2);
    let refused = source
        .hutan(&["memory", "export", md.to_str().unwrap()])
        .await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("--force"), "{refused:?}");
    source
        .hutan(&["memory", "export", md.to_str().unwrap(), "--force"])
        .await
        .ok();
    source.stop().await;

    for file in [&md, &json] {
        let target = TestDaemon::start().await;
        let imported = target
            .hutan(&["memory", "import", file.to_str().unwrap(), "--json"])
            .await
            .ok()
            .json();
        assert_eq!(imported["created"], 2, "{imported}");
        assert_eq!(
            entries(&target, &[]).await.len(),
            0,
            "nothing saved by the import"
        );
        let pending = target
            .hutan(&["memory", "proposals", "--json"])
            .await
            .ok()
            .json();
        let proposals = pending["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), 2);
        assert!(
            proposals
                .iter()
                .all(|p| p["provenance"]["origin"] == "import")
        );
        let human = target
            .hutan(&["memory", "import", file.to_str().unwrap()])
            .await
            .ok()
            .stdout;
        assert!(
            human.contains("0 new, 0 changes, 2 already there"),
            "a second import adds nothing: {human}"
        );
        // Approving one of them saves it.
        let prefix = proposals[0]["id"].as_str().unwrap()[..16].to_owned();
        target
            .hutan(&["memory", "proposals", "--approve", &prefix])
            .await
            .ok();
        assert_eq!(entries(&target, &[]).await.len(), 1);
        target.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_files_says_where_the_files_are_and_what_was_refused() {
    let t = TestDaemon::start().await;
    let none = t.hutan(&["memory", "files"]).await.ok().stdout;
    assert!(none.contains("Folder: "), "{none}");
    t.hutan(&["memory", "add", "Prefers short answers."])
        .await
        .ok();
    let status = t.hutan(&["memory", "files", "--json"]).await.ok().json();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["files"].as_array().unwrap().len(), 3);
    let folder = std::path::PathBuf::from(status["folder"].as_str().unwrap());
    assert!(folder.ends_with(".mawas/memory"), "{folder:?}");
    // An edit with a secret is a problem the command reports.
    let file = folder.join("profile.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("short", "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD"),
    )
    .unwrap();
    let human = t.hutan(&["memory", "files"]).await.ok().stdout;
    assert!(human.contains("Problem in profile.md"), "{human}");
    assert!(human.contains("not a secret store"), "{human}");
    assert!(
        !human.contains("sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD"),
        "{human}"
    );
    // A workspace's files are off until the user turns them on.
    let ws = t.hutan(&["memory", "files", "--scope", "workspace"]).await;
    assert_eq!(ws.code, 2, "{ws:?}");
    t.stop().await;
}
