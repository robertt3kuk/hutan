//! The Codex agent-tree feed over a spawned-subagent conversation. The fixture
//! `subagent-notifications.jsonl` is **hand-built in the shape of the 0.162.1 generated schema**
//! (`codex app-server generate-json-schema`): a real recording needs a model call and a
//! subagent-enabled account, which the default gate never has.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hutan_agents::codex::{NodeFeed, TREE_SOURCE};
use hutan_core::{NodeKind, NodeOutcome, NodeState, NodeUpdate, ObservationTree, Patch, SessionId};
use serde_json::{Value, json};

const MAIN: &str = "01a00000-0000-7000-8000-000000000001";
const CHILD: &str = "01a00000-0000-7000-8000-000000000002";

fn fixture() -> Vec<Value> {
    include_str!("../fixtures/codex-0.162/subagent-notifications.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn feed_all(feed: &mut NodeFeed) -> Vec<Vec<NodeUpdate>> {
    fixture()
        .iter()
        .map(|n| feed.notification(n["method"].as_str().unwrap(), &n["params"]))
        .collect()
}

#[test]
fn a_spawned_subagent_becomes_a_child_node_and_only_its_own_status_ends_it() {
    let mut feed = NodeFeed::new(MAIN.to_owned());
    let mut tree = ObservationTree::new(SessionId::new());
    tree.apply(feed.started("gpt-6.1-sol"), 1_000);
    let steps = feed_all(&mut feed);
    let mut now = 1_000;
    let mut after = Vec::new();
    for updates in steps {
        for update in updates {
            now += 10;
            assert_eq!(update.source, TREE_SOURCE);
            tree.apply(update, now);
        }
        after.push(tree.node(CHILD).cloned());
    }
    // thread/started: named by nickname and role, a child of the main thread.
    let child = after[0].clone().unwrap();
    assert_eq!(child.parent.as_deref(), Some(MAIN));
    assert_eq!(
        (child.name.as_deref(), child.agent_type.as_deref()),
        (Some("Hume"), Some("explorer"))
    );
    assert_eq!(child.kind, NodeKind::Sub);
    assert_eq!(child.model.as_deref(), Some("gpt-6.1-sol"));
    // spawnAgent pendingInit, then running.
    assert_eq!(after[1].as_ref().unwrap().state, NodeState::Working);
    // The spawn call's completion is not the child's: it is still working.
    assert_eq!(after[2].as_ref().unwrap().outcome, None);
    // active + a running command: working with that activity.
    assert_eq!(
        after[4].as_ref().unwrap().activity.as_deref(),
        Some("shell: ls -la")
    );
    // waitingOnApproval: blocked, and the session needs attention.
    assert_eq!(after[5].as_ref().unwrap().state, NodeState::Blocked);
    // Usage kind codex.total, fields as sent, none invented.
    let usage = after[6].as_ref().unwrap().usage.clone().unwrap();
    assert_eq!(usage.kind, "codex.total");
    assert_eq!(
        (usage.input, usage.output, usage.cache_read, usage.reasoning),
        (Some(1200), Some(90), Some(800), Some(20))
    );
    assert_eq!(
        (usage.cache_write, usage.cost_usd, usage.context_tokens),
        (None, None, None)
    );
    assert_eq!(usage.context_window, Some(200_000));
    // The command finished: activity cleared, still alive.
    assert_eq!(after[7].as_ref().unwrap().activity, None);
    // Back to active without flags: unblocked.
    assert_eq!(after[8].as_ref().unwrap().state, NodeState::Working);
    // A `wait` call that completed says the child is *still running*: not done.
    assert_eq!(after[9].as_ref().unwrap().outcome, None);
    // Its own turn completed: the child is done.
    assert_eq!(
        after[10].as_ref().unwrap().outcome,
        Some(NodeOutcome::Completed)
    );
    // The following idle status does not undo it.
    let last = after[11].as_ref().unwrap();
    assert_eq!(
        (last.outcome, last.state),
        (Some(NodeOutcome::Completed), NodeState::Idle)
    );
    assert_eq!(tree.root().unwrap().node_id, MAIN);
}

#[test]
fn collab_statuses_map_to_outcomes() {
    let call = |tool: &str, status: &str| {
        json!({"threadId": MAIN, "turnId": "t", "item": {
            "type": "collabAgentToolCall", "id": "c", "tool": tool, "status": "completed",
            "senderThreadId": MAIN, "receiverThreadIds": [CHILD],
            "agentsStates": {CHILD: {"status": status}}}})
    };
    for (status, outcome) in [
        ("completed", Some(NodeOutcome::Completed)),
        ("errored", Some(NodeOutcome::Failed)),
        ("interrupted", Some(NodeOutcome::Interrupted)),
        ("shutdown", Some(NodeOutcome::Interrupted)),
        ("notFound", Some(NodeOutcome::Disconnected)),
        ("running", None),
    ] {
        let mut feed = NodeFeed::new(MAIN.to_owned());
        let updates = feed.notification("item/completed", &call("closeAgent", status));
        assert_eq!(updates.len(), 1, "{status}");
        assert_eq!(updates[0].outcome, outcome, "{status}");
    }
    // A closeAgent without states says nothing about the receivers; spawnAgent still names one.
    let mut feed = NodeFeed::new(MAIN.to_owned());
    let bare = json!({"threadId": MAIN, "turnId": "t", "item": {
        "type": "collabAgentToolCall", "id": "c", "tool": "closeAgent", "status": "completed",
        "senderThreadId": MAIN, "receiverThreadIds": [CHILD], "agentsStates": {}}});
    assert!(feed.notification("item/completed", &bare).is_empty());
}

#[test]
fn sub_agent_activity_items_start_and_end_a_child() {
    let mut feed = NodeFeed::new(MAIN.to_owned());
    let item = |kind: &str| {
        json!({"threadId": MAIN, "turnId": "t", "item": {
            "type": "subAgentActivity", "id": "s", "kind": kind, "agentThreadId": CHILD,
            "agentPath": "/root/explorer", "model": "gpt-6.1-sol"}})
    };
    let started = feed.notification("item/started", &item("started"));
    assert_eq!(started[0].state, Some(NodeState::Working));
    assert_eq!(started[0].name.as_deref(), Some("explorer"));
    assert_eq!(started[0].parent.as_deref(), Some(MAIN));
    let again = feed.notification("item/completed", &item("interacted"));
    assert_eq!(again[0].name, None, "named once");
    let done = feed.notification("item/completed", &item("completed"));
    assert_eq!(done[0].outcome, Some(NodeOutcome::Completed));
    let _ = Patch::<String>::Keep;
}

#[test]
fn the_main_thread_is_the_root_and_a_system_error_does_not_end_it() {
    let mut feed = NodeFeed::new(MAIN.to_owned());
    let started = feed.started("m");
    assert_eq!((started.kind, started.parent), (NodeKind::Main, None));
    let error = feed.notification(
        "thread/status/changed",
        &json!({"threadId": MAIN, "status": {"type": "systemError"}}),
    );
    assert_eq!(
        (error[0].state, error[0].outcome),
        (Some(NodeState::Idle), None)
    );
    let sub_error = feed.notification(
        "thread/status/changed",
        &json!({"threadId": CHILD, "status": {"type": "systemError"}}),
    );
    assert_eq!(sub_error[0].outcome, Some(NodeOutcome::Failed));
}
