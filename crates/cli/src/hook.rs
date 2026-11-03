//! Translation of Codex and Claude Code hook events (JSON on stdin) into agent reports
//! (`hutan agent hook`). Pure, so it is tested directly.
//!
//! Claude Code's subagents are nodes of the session's agent tree, keyed by the `agent_id` its
//! hooks carry: `SubagentStart` makes one, tool and permission hooks that carry an `agent_id`
//! update that node's activity and state, `SubagentStop` ends it. Hooks without an `agent_id`
//! are about the main agent, which is the node `root`.

use hutan_core::{NodeKind, NodeOutcome};
use serde_json::Value;

use hutan_core::insight::InsightReport;

use crate::args::{AgentStateArg, HookAgentArg};

pub mod insight;

pub const CODEX_SOURCE: &str = "codex-hooks";
pub const CLAUDE_SOURCE: &str = "claude-hooks";
/// Who reports what Claude Code's status line carries (`hutan agent statusline`).
pub const CLAUDE_STATUSLINE_SOURCE: &str = "claude-statusline";
/// The node id of the main agent in reports of Claude Code's hooks.
pub const CLAUDE_ROOT_NODE: &str = "root";

/// The node of the agent tree a report is about.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PlanNode {
    pub id: String,
    pub kind: Option<NodeKind>,
    pub name: Option<String>,
    pub agent_type: Option<String>,
    pub outcome: Option<NodeOutcome>,
    pub model: Option<String>,
    pub activity: Option<String>,
    pub transcript_path: Option<String>,
}

#[derive(Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // built once per hook event
pub enum Plan {
    Report {
        agent: &'static str,
        source: &'static str,
        /// `None`: only the node's model or activity changes.
        state: Option<AgentStateArg>,
        message: Option<String>,
        agent_session_id: Option<String>,
        resume_argv: Vec<String>,
        node: Option<PlanNode>,
        /// Tools, files and turn edges of this event (`insight::from_hook`).
        insight: Option<InsightReport>,
    },
    Release {
        source: &'static str,
        node: Option<String>,
    },
}

/// What the translation needs to know about where it runs.
#[derive(Debug)]
pub struct HookEnv<'a> {
    /// `CODEX_THREAD_ID`: the conversation of the Codex that spawned this hook's process tree.
    pub codex_thread_id: Option<&'a str>,
}

fn text<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// The transcript file the main agent writes its conversation to: Claude Code's hooks name it
/// (`transcript_path`) in every event of the main agent. Only a plain absolute path of a
/// sensible length is passed on; the daemon reads it only inside the folder the user enabled
/// for history (`docs/integrations.md` § Insight).
fn transcript_of(input: &Value) -> Option<String> {
    text(input, "transcript_path")
        .filter(|path| {
            path.starts_with('/') && path.len() <= 1024 && !path.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

/// `Tool: first line of what it does`, short enough for a table cell.
fn tool_activity(input: &Value) -> Option<String> {
    let tool = text(input, "tool_name")?;
    let detail = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
    ]
    .iter()
    .find_map(|key| input.get("tool_input")?.get(*key)?.as_str())
    .and_then(|detail| detail.lines().next());
    Some(match detail {
        Some(detail) => format!("{tool}: {detail}"),
        None => tool.to_owned(),
    })
}

/// The report (or release) one hook event stands for; `None` for events that say nothing about
/// the agent's state.
pub fn plan(agent: HookAgentArg, input: &Value, env: &HookEnv<'_>) -> Option<Plan> {
    let event = input.get("hook_event_name")?.as_str()?;
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| is_plain_id(id));
    if agent == HookAgentArg::Codex
        && let (Some(thread), Some(id)) = (env.codex_thread_id, session_id)
        && thread != id
    {
        // A Codex started by another Codex's tool: its hooks are not about this session's agent.
        return None;
    }
    let (agent_name, source, resume_program, resume_flag): (
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    ) = match agent {
        HookAgentArg::Codex => ("codex", CODEX_SOURCE, "codex", &["resume"]),
        HookAgentArg::Claude => ("claude", CLAUDE_SOURCE, "claude", &["--resume"]),
    };
    // Claude Code only: hooks inside a subagent carry its id.
    let agent_id = (agent == HookAgentArg::Claude)
        .then(|| text(input, "agent_id").filter(|id| is_plain_id(id)))
        .flatten();
    let agent_type = text(input, "agent_type").map(str::to_owned);
    // Only the main agent's events name its own transcript (a subagent's file is another one).
    let transcript = agent_id.is_none().then(|| transcript_of(input)).flatten();
    let node_of = |activity: Option<String>| -> Option<PlanNode> {
        (agent == HookAgentArg::Claude).then(|| match agent_id {
            Some(id) => PlanNode {
                id: id.to_owned(),
                kind: Some(NodeKind::Sub),
                name: agent_type.clone(),
                agent_type: agent_type.clone(),
                activity,
                ..PlanNode::default()
            },
            None => PlanNode {
                id: CLAUDE_ROOT_NODE.to_owned(),
                kind: Some(NodeKind::Main),
                agent_type: agent_type.clone(),
                transcript_path: transcript.clone(),
                activity,
                ..PlanNode::default()
            },
        })
    };
    let main_node = |model: Option<String>, activity: Option<String>| -> Option<PlanNode> {
        (agent == HookAgentArg::Claude).then(|| PlanNode {
            id: CLAUDE_ROOT_NODE.to_owned(),
            kind: Some(NodeKind::Main),
            agent_type: agent_type.clone(),
            model,
            transcript_path: transcript.clone(),
            activity,
            ..PlanNode::default()
        })
    };

    let mut message = None;
    let (state, node) = match event {
        "SessionStart" => (
            Some(AgentStateArg::Idle),
            main_node(text(input, "model").map(str::to_owned), None),
        ),
        "Stop" => (Some(AgentStateArg::Idle), main_node(None, None)),
        // The turn ended on an API error: the main agent shows it failed (a normal Stop counts as
        // finished for notifications), and the next prompt starts it again.
        "StopFailure" => (
            Some(AgentStateArg::Idle),
            main_node(None, None).map(|node| PlanNode {
                outcome: Some(NodeOutcome::Failed),
                ..node
            }),
        ),
        "SessionEnd" => {
            return Some(Plan::Release {
                source,
                node: (agent == HookAgentArg::Claude).then(|| CLAUDE_ROOT_NODE.to_owned()),
            });
        }
        "UserPromptSubmit" => (Some(AgentStateArg::Working), main_node(None, None)),
        // PostToolUse ends a permission prompt as far as hooks can tell.
        "PostToolUse" | "PostToolUseFailure" => (Some(AgentStateArg::Working), node_of(None)),
        "PreToolUse" => (Some(AgentStateArg::Working), node_of(tool_activity(input))),
        "PermissionRequest" => {
            let activity = match text(input, "tool_name") {
                Some(tool) => format!("permission: {tool}"),
                None => "permission requested".to_owned(),
            };
            if agent_id.is_none() {
                message = Some(activity.clone());
            }
            (Some(AgentStateArg::Blocked), node_of(Some(activity)))
        }
        "SubagentStart" => {
            let id = agent_id?;
            (
                Some(AgentStateArg::Working),
                Some(PlanNode {
                    id: id.to_owned(),
                    kind: Some(NodeKind::Sub),
                    name: agent_type.clone(),
                    agent_type: agent_type.clone(),
                    ..PlanNode::default()
                }),
            )
        }
        "SubagentStop" => {
            let id = agent_id?;
            (
                Some(AgentStateArg::Idle),
                Some(PlanNode {
                    id: id.to_owned(),
                    kind: Some(NodeKind::Sub),
                    name: agent_type.clone(),
                    agent_type: agent_type.clone(),
                    outcome: Some(NodeOutcome::Completed),
                    transcript_path: text(input, "agent_transcript_path").map(str::to_owned),
                    ..PlanNode::default()
                }),
            )
        }
        "PostModelSwitch" if agent == HookAgentArg::Claude => (
            None,
            main_node(Some(text(input, "to_model")?.to_owned()), None),
        ),
        _ => return None,
    };
    // Only the main agent's hooks carry the conversation and how to resume it.
    let about_main = agent_id.is_none();
    let resume_argv = session_id
        .filter(|_| about_main && state.is_some())
        .map(|id| {
            let mut argv = vec![resume_program.to_owned()];
            argv.extend(resume_flag.iter().map(|flag| (*flag).to_owned()));
            argv.push(id.to_owned());
            argv
        })
        .unwrap_or_default();
    Some(Plan::Report {
        agent: agent_name,
        source,
        state,
        message,
        agent_session_id: session_id.filter(|_| about_main).map(str::to_owned),
        resume_argv,
        node,
        insight: insight::from_hook(agent, event, input),
    })
}

/// Ids that are safe inside a resume command: the daemon rejects apostrophes and control
/// characters, and a hook must not lose its state report over an odd id.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn env() -> HookEnv<'static> {
        HookEnv {
            codex_thread_id: None,
        }
    }

    fn go(agent: HookAgentArg, input: Value) -> Option<Plan> {
        plan(agent, &input, &env())
    }

    fn claude(input: Value) -> Option<Plan> {
        go(HookAgentArg::Claude, input)
    }

    fn report(plan: Option<Plan>) -> (Option<AgentStateArg>, Option<PlanNode>) {
        match plan {
            Some(Plan::Report { state, node, .. }) => (state, node),
            other => panic!("expected a report, got {other:?}"),
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn main_agent_events_map_to_states_on_the_root_node_and_a_resume_command() {
        let event = |name: &str| json!({"hook_event_name": name, "session_id": "5f3e-1c"});
        let Some(Plan::Report {
            state,
            resume_argv,
            agent_session_id,
            node,
            ..
        }) = claude(json!({
            "hook_event_name": "SessionStart", "session_id": "5f3e-1c", "model": "claude-opus-5"
        }))
        else {
            panic!("report");
        };
        assert_eq!(state, Some(AgentStateArg::Idle));
        assert_eq!(resume_argv, argv(&["claude", "--resume", "5f3e-1c"]));
        assert_eq!(agent_session_id.as_deref(), Some("5f3e-1c"));
        let node = node.unwrap();
        assert_eq!(
            (node.id.as_str(), node.kind, node.model.as_deref()),
            ("root", Some(NodeKind::Main), Some("claude-opus-5"))
        );
        assert_eq!(
            report(claude(event("UserPromptSubmit"))).0,
            Some(AgentStateArg::Working)
        );
        assert_eq!(
            report(claude(event("PostToolUse"))).0,
            Some(AgentStateArg::Working)
        );
        assert_eq!(report(claude(event("Stop"))).0, Some(AgentStateArg::Idle));
        let (state, node) = report(claude(event("StopFailure")));
        assert_eq!(state, Some(AgentStateArg::Idle));
        assert_eq!(node.unwrap().outcome, Some(NodeOutcome::Failed));
        assert_eq!(
            claude(event("SessionEnd")),
            Some(Plan::Release {
                source: CLAUDE_SOURCE,
                node: Some("root".into())
            })
        );
        assert_eq!(claude(event("PreCompact")), None);
        assert_eq!(claude(json!({"session_id": "x"})), None);
    }

    #[test]
    fn a_permission_request_is_blocked_with_the_tool_name() {
        let (state, node) = report(claude(json!({
            "hook_event_name": "PermissionRequest", "session_id": "abc", "tool_name": "Bash",
            "tool_input": {"command": "rm -rf build"},
        })));
        assert_eq!(state, Some(AgentStateArg::Blocked));
        assert_eq!(node.unwrap().activity.as_deref(), Some("permission: Bash"));
    }

    #[test]
    fn subagents_are_nodes_keyed_by_agent_id() {
        let (state, node) = report(claude(json!({
            "hook_event_name": "SubagentStart", "session_id": "abc",
            "agent_id": "agent-abc123", "agent_type": "Explore",
        })));
        assert_eq!(state, Some(AgentStateArg::Working));
        let node = node.unwrap();
        assert_eq!(
            (
                node.id.as_str(),
                node.kind,
                node.agent_type.as_deref(),
                node.name.as_deref()
            ),
            (
                "agent-abc123",
                Some(NodeKind::Sub),
                Some("Explore"),
                Some("Explore")
            )
        );

        // Tool and permission hooks inside it update that node, not the main agent.
        let Some(Plan::Report {
            state,
            node,
            message,
            resume_argv,
            agent_session_id,
            ..
        }) = claude(json!({
            "hook_event_name": "PermissionRequest", "session_id": "abc",
            "agent_id": "agent-abc123", "agent_type": "Explore", "tool_name": "Bash",
        }))
        else {
            panic!("report");
        };
        assert_eq!(state, Some(AgentStateArg::Blocked));
        assert_eq!(node.unwrap().id, "agent-abc123");
        assert_eq!(message, None, "the session's message is the main agent's");
        assert!(resume_argv.is_empty() && agent_session_id.is_none());

        let (state, node) = report(claude(json!({
            "hook_event_name": "PreToolUse", "session_id": "abc", "agent_id": "agent-abc123",
            "tool_name": "Read", "tool_input": {"file_path": "/repo/src/lib.rs"},
        })));
        assert_eq!(state, Some(AgentStateArg::Working));
        assert_eq!(
            node.unwrap().activity.as_deref(),
            Some("Read: /repo/src/lib.rs")
        );

        let (state, node) = report(claude(json!({
            "hook_event_name": "SubagentStop", "session_id": "abc", "agent_id": "agent-abc123",
            "agent_type": "Explore", "agent_transcript_path": "/t/subagents/agent-abc123.jsonl",
        })));
        assert_eq!(state, Some(AgentStateArg::Idle));
        let node = node.unwrap();
        assert_eq!(node.outcome, Some(NodeOutcome::Completed));
        assert_eq!(
            node.transcript_path.as_deref(),
            Some("/t/subagents/agent-abc123.jsonl")
        );

        // A subagent event that cannot say which agent it is about says nothing.
        assert_eq!(
            claude(json!({"hook_event_name": "SubagentStop", "session_id": "abc"})),
            None
        );
    }

    #[test]
    fn a_model_switch_updates_the_model_and_leaves_the_state_alone() {
        let (state, node) = report(claude(json!({
            "hook_event_name": "PostModelSwitch", "session_id": "abc", "to_model": "claude-sonnet-5",
        })));
        assert_eq!(state, None);
        let node = node.unwrap();
        assert_eq!(
            (node.id.as_str(), node.model.as_deref()),
            ("root", Some("claude-sonnet-5"))
        );
        let Some(Plan::Report { resume_argv, .. }) = claude(json!({
            "hook_event_name": "PostModelSwitch", "session_id": "abc", "to_model": "m",
        })) else {
            panic!("report");
        };
        assert!(
            resume_argv.is_empty(),
            "a model change does not touch the resume command"
        );
    }

    #[test]
    fn codex_reports_the_main_agent_only_and_ignores_foreign_threads() {
        let input = json!({"hook_event_name": "SessionStart", "session_id": "0198-abc"});
        let Some(Plan::Report {
            agent,
            source,
            node,
            resume_argv,
            ..
        }) = go(HookAgentArg::Codex, input.clone())
        else {
            panic!("report");
        };
        assert_eq!((agent, source, node), ("codex", CODEX_SOURCE, None));
        assert_eq!(resume_argv, argv(&["codex", "resume", "0198-abc"]));
        let foreign = HookEnv {
            codex_thread_id: Some("another-thread"),
        };
        assert_eq!(plan(HookAgentArg::Codex, &input, &foreign), None);
    }

    #[test]
    fn ids_with_odd_characters_do_not_make_a_resume_command() {
        let Some(Plan::Report { resume_argv, .. }) = claude(json!({
            "hook_event_name": "SessionStart", "session_id": "it's;bad",
        })) else {
            panic!("report");
        };
        assert!(resume_argv.is_empty());
    }

    #[test]
    fn the_main_agents_transcript_path_rides_on_its_root_node_and_no_subagents() {
        let path = "/c/projects/-w-app/5f3e-1c.jsonl";
        for event in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
            let (_, node) = report(claude(json!({
                "hook_event_name": event, "session_id": "5f3e-1c", "transcript_path": path,
                "tool_name": "Read",
            })));
            let node = node.unwrap();
            assert_eq!(node.id, CLAUDE_ROOT_NODE, "{event}");
            assert_eq!(node.transcript_path.as_deref(), Some(path), "{event}");
        }
        // A subagent's hooks carry the main transcript's path too, but it is not theirs.
        let (_, node) = report(claude(json!({
            "hook_event_name": "PostToolUse", "session_id": "5f3e-1c", "agent_id": "agent-a",
            "transcript_path": path, "tool_name": "Read",
        })));
        assert_eq!(node.unwrap().transcript_path, None);
        // Only a plain absolute path is passed on.
        for odd in ["relative.jsonl", "/a/b\nc.jsonl", ""] {
            let (_, node) = report(claude(json!({
                "hook_event_name": "Stop", "session_id": "5f3e-1c", "transcript_path": odd,
            })));
            assert_eq!(node.unwrap().transcript_path, None, "{odd:?}");
        }
    }
}
