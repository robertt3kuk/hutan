//! Which files an ACP agent's edit tools change, as the journal says it (FX-AGENT-FILES): every
//! file a completed call names, not only the ones the agent sent the old and new text of. OMP's
//! `edit` keeps the snapshot text of the first files of a call (32 KiB between them) and names the
//! rest by path; its `write` sends no text at all.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hutan_agents::acp::Mapper;
use hutan_core::agent::{FileChange, FileChangeKind};
use hutan_core::{AgentEventDraft, AgentEventKind};
use serde_json::{Value, json};

const SESSION: &str = "sess_1";
const CWD: &str = "/work/app";

fn mapper() -> Mapper {
    let mut m = Mapper::new("omp", SESSION.to_owned());
    m.set_cwd(CWD.to_owned());
    m.begin_turn("change things");
    m
}

fn update(m: &mut Mapper, update: Value) -> Vec<AgentEventDraft> {
    m.update(json!({"sessionId": SESSION, "update": update}))
}

/// Every change of every `file_diff` among `drafts`, in order.
fn changes(drafts: &[AgentEventDraft]) -> Vec<FileChange> {
    drafts
        .iter()
        .filter_map(|d| match &d.kind {
            AgentEventKind::FileDiff { changes, .. } => Some(changes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn paths(changes: &[FileChange]) -> Vec<&str> {
    changes.iter().map(|c| c.path.as_str()).collect()
}

fn start_edit(m: &mut Mapper, id: &str, kind: &str, title: &str) {
    update(
        m,
        json!({
            "sessionUpdate": "tool_call", "toolCallId": id, "title": title,
            "kind": kind, "status": "pending", "rawInput": {"input": "…"}
        }),
    );
}

#[test]
fn an_edit_naming_three_files_reports_three_though_only_two_carry_text() {
    let mut m = mapper();
    start_edit(&mut m, "call_1", "edit", "Adding a dev-dependency");
    // What OMP sends when the call ends: a `diff` block for the files whose snapshots fit the
    // budget, every file's path in `locations` and in the result's `perFileResults`.
    let drafts = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_1", "status": "completed",
            "content": [
                {"type": "diff", "path": "/work/app/Cargo.toml",
                 "oldText": "[workspace]\n", "newText": "[workspace]\nx = 1\n"},
                {"type": "diff", "path": "/work/app/crates/core/Cargo.toml",
                 "oldText": "[package]\n", "newText": "[package]\ny = 2\n"},
            ],
            "locations": [
                {"path": "/work/app/Cargo.toml"},
                {"path": "/work/app/crates/core/Cargo.toml"},
                {"path": "/work/app/crates/core/src/safety.rs"},
            ],
            "rawOutput": {"details": {"perFileResults": [
                {"path": "/work/app/Cargo.toml", "op": "update", "oldText": "a", "newText": "b"},
                {"path": "/work/app/crates/core/Cargo.toml", "op": "update", "oldText": "a", "newText": "b"},
                {"path": "/work/app/crates/core/src/safety.rs", "op": "update", "snapshotsPruned": true},
            ]}},
        }),
    );
    let changes = changes(&drafts);
    assert_eq!(
        paths(&changes),
        [
            "/work/app/Cargo.toml",
            "/work/app/crates/core/Cargo.toml",
            "/work/app/crates/core/src/safety.rs"
        ]
    );
    assert!(changes[0].patch.as_deref().unwrap().contains("+x = 1"));
    assert!(changes[1].patch.as_deref().unwrap().contains("+y = 2"));
    assert_eq!(changes[2].kind, FileChangeKind::Update);
    assert_eq!(changes[2].patch, None, "no text was sent for it");
    // The diffs and the named file travel in one event, before the tool ends.
    let kinds: Vec<&str> = drafts.iter().map(|d| d.kind.name()).collect();
    assert_eq!(kinds, ["file_diff", "tool_end"], "{kinds:?}");
}

#[test]
fn a_write_without_text_names_its_file_and_says_whether_it_was_created() {
    let mut m = mapper();
    start_edit(&mut m, "call_w", "edit", "write: notes.md");
    let drafts = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_w", "status": "completed",
            "locations": [{"path": "/work/app/notes.md"}],
            "rawOutput": {"details": {"path": "notes.md", "op": "create"}},
        }),
    );
    let changes = changes(&drafts);
    assert_eq!(paths(&changes), ["/work/app/notes.md"]);
    assert_eq!(changes[0].kind, FileChangeKind::Add);
    assert_eq!(changes[0].patch, None);
}

#[test]
fn locations_alone_name_the_files_when_the_result_has_no_details() {
    let mut m = mapper();
    // The locations came with an earlier update; the end carries none.
    update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "call_l", "title": "edit: a.rs",
            "kind": "edit", "status": "pending",
            "locations": [{"path": "/work/app/src/a.rs"}, {"path": "src/b.rs"}],
        }),
    );
    let drafts = update(
        &mut m,
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_l", "status": "completed"}),
    );
    assert_eq!(
        paths(&changes(&drafts)),
        ["/work/app/src/a.rs", "/work/app/src/b.rs"],
        "a relative path is below the agent's folder"
    );
}

#[test]
fn a_file_with_a_diff_is_not_named_again_by_its_location() {
    let mut m = mapper();
    start_edit(&mut m, "call_d", "edit", "edit: src/lib.rs");
    let first = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_d", "status": "in_progress",
            "content": [{"type": "diff", "path": "/work/app/src/lib.rs", "oldText": "a\n", "newText": "b\n"}],
            "locations": [{"path": "/work/app/src/lib.rs"}],
        }),
    );
    assert_eq!(
        changes(&first).len(),
        1,
        "named by the diff, not completed yet"
    );
    let end = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_d", "status": "completed",
            "content": [{"type": "diff", "path": "/work/app/src/lib.rs", "oldText": "a\n", "newText": "b\n"}],
            "locations": [{"path": "/work/app/src/lib.rs"}],
        }),
    );
    assert!(changes(&end).is_empty(), "{end:?}");
}

#[test]
fn a_failed_edit_changed_nothing_and_a_read_names_files_without_changing_them() {
    let mut m = mapper();
    start_edit(&mut m, "call_f", "edit", "edit: a.rs");
    let failed = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_f", "status": "failed",
            "locations": [{"path": "/work/app/a.rs"}],
        }),
    );
    assert!(changes(&failed).is_empty());
    update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "call_r", "title": "read: a.rs",
            "kind": "read", "status": "pending", "locations": [{"path": "/work/app/a.rs"}],
        }),
    );
    let read = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_r", "status": "completed",
            "locations": [{"path": "/work/app/a.rs"}],
        }),
    );
    assert!(changes(&read).is_empty());
}

#[test]
fn a_delete_and_a_rename_say_what_became_of_the_file() {
    let mut m = mapper();
    start_edit(&mut m, "call_x", "delete", "delete: old.rs");
    let deleted = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_x", "status": "completed",
            "locations": [{"path": "/work/app/old.rs"}],
        }),
    );
    let deleted = changes(&deleted);
    assert_eq!(paths(&deleted), ["/work/app/old.rs"]);
    assert_eq!(deleted[0].kind, FileChangeKind::Delete);

    start_edit(&mut m, "call_m", "edit", "edit: a.rs → b.rs");
    let moved = update(
        &mut m,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call_m", "status": "completed",
            "locations": [{"path": "/work/app/b.rs"}],
            "rawOutput": {"details": {"path": "b.rs", "sourcePath": "a.rs", "op": "update", "move": "b.rs"}},
        }),
    );
    let moved = changes(&moved);
    assert_eq!(paths(&moved), ["/work/app/a.rs"]);
    assert_eq!(moved[0].move_path.as_deref(), Some("/work/app/b.rs"));
}
