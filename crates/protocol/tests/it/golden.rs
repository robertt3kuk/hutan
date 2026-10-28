//! Wire-format guard: every message has a committed fixture under `tests/fixtures/v0/`.
//! A diff here means a protocol change: bump `PROTOCOL_VERSION`, then `just update-golden`.
#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hutan_core::QuestionId;
use hutan_core::agent::{
    DecidedBy, FileChange, FileChangeKind, MessageRole, PermissionKind, PermissionOption,
    PermissionOptionKind, PermissionOutcome, TokenCounts, ToolKind, ToolStatus,
};
use hutan_core::question::{
    ChoiceOption, FieldKind, QuestionExpiry, QuestionField, QuestionOutcome, QuestionResponse,
    QuestionSpec,
};
use hutan_core::{
    AfterPolicy, DeliveryActor, DeliveryRecord, DeliveryState, ReviewDecision, Task, TaskId,
    TaskPlacement, TaskPlacementKind, TaskRun, TaskState,
};
use hutan_core::{
    AgentEvent, AgentEventDraft, AgentEventKind, AgentNode, AgentObservation, ApprovalId,
    ApprovalPolicy, ItemId, Liveness, ModelInfo, NodeKind, NodeOutcome, NodeState, SandboxMode,
    TurnId, Usage,
};
use hutan_core::{
    CheckpointId, Epoch, Seq, SessionId, SessionKind, SessionState, TerminalId, WorkspaceId,
};
use hutan_protocol::account::*;
use hutan_protocol::assistant::*;
use hutan_protocol::blocks::{
    Block, BlockPart, BlockState, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks,
    TerminalBlocksResult,
};
use hutan_protocol::chat::*;
use hutan_protocol::checkpoint::*;
use hutan_protocol::control::ControlMessage;
use hutan_protocol::fs::*;
use hutan_protocol::git::*;
use hutan_protocol::grants::{ControlGrant, ControlGrantRevoke, ControlGrants, ControlGrantsList};
use hutan_protocol::history::*;
use hutan_protocol::hooks::{
    HookEvent, HookInfo, HookOutcome, HookPayload, HookRun, HooksList, HooksLog, HooksLogRequest,
    HooksTest, HooksTested,
};
use hutan_protocol::lsp::*;
use hutan_protocol::memory::*;
use hutan_protocol::messages::*;
use hutan_protocol::notify::{
    ActRoute, ClientFocus, IntentAction, IntentKind, NotificationIntent, NotifyAck, NotifyAct,
    NotifyActed, NotifyList, NotifyMute,
};
use hutan_protocol::project::*;
use hutan_protocol::provider::*;
use hutan_protocol::recovery::*;
use hutan_protocol::render::*;
use hutan_protocol::resources::*;
use hutan_protocol::task::*;
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchMatch, TerminalSearchResult};
use hutan_protocol::tree::*;
use hutan_protocol::usage::*;
use hutan_protocol::worktree::*;
use hutan_protocol::{ErrorBody, ErrorCode, Frame, PROTOCOL_VERSION};
fn daemon_setting_entries() -> Vec<hutan_protocol::daemon_settings::DaemonSetting> {
    use hutan_protocol::daemon_settings::{Applies, DaemonSetting, DaemonSettingKind};
    let entry = |key: &str,
                 kind: DaemonSettingKind,
                 value: serde_json::Value,
                 default: serde_json::Value,
                 set_in_file: bool,
                 applies: Applies,
                 pending_restart: bool| DaemonSetting {
        key: key.into(),
        kind,
        value,
        default,
        set_in_file,
        applies,
        pending_restart,
    };
    vec![
        entry(
            "notify.enabled",
            DaemonSettingKind::Bool,
            serde_json::json!(false),
            serde_json::json!(true),
            true,
            Applies::Live,
            false,
        ),
        entry(
            "tasks.max_parallel",
            DaemonSettingKind::Int {
                min: Some(1),
                max: None,
            },
            serde_json::json!(3),
            serde_json::json!(3),
            false,
            Applies::Live,
            false,
        ),
        entry(
            "daemon.restore_sessions",
            DaemonSettingKind::Enum {
                choices: vec!["respawn".into(), "stopped".into()],
            },
            serde_json::json!("stopped"),
            serde_json::json!("respawn"),
            true,
            Applies::Restart,
            true,
        ),
        entry(
            "harness.omp.bin",
            DaemonSettingKind::Path,
            serde_json::json!(""),
            serde_json::json!(""),
            false,
            Applies::Restart,
            false,
        ),
        entry(
            "notify.kinds",
            DaemonSettingKind::StringList {
                choices: Some(vec!["approval_required".into(), "task_failed".into()]),
            },
            serde_json::json!(["approval_required"]),
            serde_json::json!(["approval_required", "task_failed"]),
            true,
            Applies::Live,
            false,
        ),
        entry(
            "codex.args",
            DaemonSettingKind::StringList { choices: None },
            serde_json::json!([]),
            serde_json::json!([]),
            false,
            Applies::Restart,
            false,
        ),
        entry(
            "harness.omp.env",
            DaemonSettingKind::StringMap,
            serde_json::json!({"OMP_LOG": "debug"}),
            serde_json::json!({}),
            true,
            Applies::Restart,
            false,
        ),
        entry(
            "assistant.model",
            DaemonSettingKind::String,
            serde_json::json!(""),
            serde_json::json!(""),
            false,
            Applies::Restart,
            false,
        ),
        entry(
            "mcp.docs",
            DaemonSettingKind::Table,
            serde_json::json!({"command": "npx", "args": ["-y", "docs-mcp"]}),
            serde_json::json!({}),
            true,
            Applies::Restart,
            false,
        ),
    ]
}
fn ws() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JA".parse().unwrap()
}
fn sid() -> SessionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap()
}
fn epoch() -> Epoch {
    "01K7F3B5N2Q9W8E4R6T1Y0V3PQ".parse().unwrap()
}
const TID: TerminalId = TerminalId(7);

fn size() -> TerminalSize {
    TerminalSize {
        cols: 120,
        rows: 40,
        pixel_width: 1440,
        pixel_height: 960,
    }
}

fn session_info() -> SessionInfo {
    SessionInfo {
        forked_from: None,
        containment: None,
        terminal_agent: None,
        id: sid(),
        workspace_id: ws(),
        kind: SessionKind::Terminal,
        title: "omp".into(),
        state: SessionState::Running,
        command: vec!["omp".into()],
        cwd: Some("/repo".into()),
        terminal_id: Some(TID),
        created_at_ms: 1_791_894_164_332,
        agent: None,
        restored: false,
        chat: None,
        placement: None,
        folder: None,
        recovery: None,
        muted: false,
    }
}

/// A session a daemon start restored.
fn recovered(kind: RecoveryKind, lost: Vec<Lost>) -> SessionInfo {
    SessionInfo {
        recovery: Some(Recovery {
            kind,
            previous_epoch: Some(epoch()),
            daemon_stop: DaemonStop::Crashed,
            restarted_at_ms: 1_791_894_300_000,
            lost,
        }),
        ..session_info()
    }
}

fn finished_block() -> Block {
    Block {
        id: 12,
        prompt_line: 1500,
        command_line: Some(1500),
        output_start: Some(1501),
        end_line: Some(1504),
        command: Some("cargo test".into()),
        exit_code: Some(101),
        started_at_ms: 1_791_894_170_000,
        finished_at_ms: Some(1_791_894_171_250),
        cwd: Some("/repo".into()),
        state: BlockState::Finished,
    }
}

fn prompt_block() -> Block {
    Block {
        id: 13,
        prompt_line: 1504,
        command_line: None,
        output_start: None,
        end_line: None,
        command: None,
        exit_code: None,
        started_at_ms: 1_791_894_171_300,
        finished_at_ms: None,
        cwd: Some("/repo".into()),
        state: BlockState::Prompt,
    }
}

fn turn() -> TurnId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3TV".parse().unwrap()
}
fn item() -> ItemId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JT".parse().unwrap()
}
fn approval() -> ApprovalId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3AP".parse().unwrap()
}

fn chat_info() -> ChatInfo {
    ChatInfo {
        fork: None,
        agent: "codex".into(),
        model: "gpt-6.1-sol".into(),
        approval_policy: ApprovalPolicy::Untrusted,
        sandbox: SandboxMode::ReadOnly,
        turn_active: true,
        pending_approvals: 1,
        pending_questions: 0,
        provider_id: None,
        agent_session_id: None,
        assistant: false,
        images: None,
        mode: None,
        checkpoints: None,
        account: None,
        safety: None,
    }
}

fn harness_chat_session_info() -> SessionInfo {
    SessionInfo {
        title: "omp · repo".into(),
        state: SessionState::Running,
        command: vec!["omp".into()],
        chat: Some(ChatInfo {
            fork: None,
            agent: "omp".into(),
            model: "anthropic/claude-sonnet-4-5".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: true,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: Some("01a140f0-8de4-70fb-9f43-aaa92aef0e15".into()),
            assistant: false,
            images: Some(true),
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        }),
        ..chat_session_info()
    }
}

fn harness_assistant_session_info() -> SessionInfo {
    SessionInfo {
        title: "Assistant · hutan".into(),
        state: SessionState::Idle,
        command: vec!["omp".into()],
        chat: Some(ChatInfo {
            fork: None,
            agent: "omp".into(),
            model: "anthropic/claude-sonnet-4-5".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: false,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: Some("01a140f0-8de4-70fb-9f43-aaa92aef0e15".into()),
            assistant: true,
            images: Some(false),
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        }),
        ..chat_session_info()
    }
}

fn direct_chat_session_info() -> SessionInfo {
    SessionInfo {
        title: "openrouter · llama-3.3-70b".into(),
        state: SessionState::Running,
        command: vec!["direct".into()],
        chat: Some(ChatInfo {
            fork: None,
            agent: "direct".into(),
            model: "meta-llama/llama-3.3-70b-instruct".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: true,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: Some("openrouter".into()),
            agent_session_id: None,
            assistant: false,
            images: None,
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        }),
        ..chat_session_info()
    }
}

fn provider_info() -> ProviderInfo {
    ProviderInfo {
        id: "openrouter".into(),
        kind: "openai-compatible".into(),
        name: "OpenRouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        default_model: Some("meta-llama/llama-3.3-70b-instruct".into()),
        has_credential: true,
        created_at_ms: 1_791_894_100_000,
    }
}

fn local_provider_info() -> ProviderInfo {
    ProviderInfo {
        id: "ollama".into(),
        kind: "ollama".into(),
        name: "ollama".into(),
        base_url: "http://127.0.0.1:11434".into(),
        default_model: None,
        has_credential: false,
        created_at_ms: 1_791_894_110_000,
    }
}

fn chat_session_info() -> SessionInfo {
    SessionInfo {
        kind: SessionKind::Harness,
        title: "codex".into(),
        state: SessionState::WaitingApproval,
        command: vec!["codex".into()],
        terminal_id: None,
        chat: Some(chat_info()),
        ..session_info()
    }
}

/// A draft stamped as the daemon does.
fn agent_event(seq: u64, kind: AgentEventKind) -> AgentEvent {
    let mut draft = AgentEventDraft::new(kind);
    draft.turn_id = Some(turn());
    draft.item_id = Some(item());
    draft.provider.thread_id = Some("thread-1".into());
    draft.provider.turn_id = Some("turn-1".into());
    draft.provider.item_id = Some("item-1".into());
    draft.into_event(sid(), seq, 1_791_894_170_000 + seq)
}

fn attachments_meta_fixture() -> Vec<AttachmentMeta> {
    vec![
        AttachmentMeta::text(AttachmentKind::ShellBlock, "cargo test", 1250, false),
        AttachmentMeta::text(AttachmentKind::File, "src/main.rs:10-40", 3000, true),
        AttachmentMeta {
            size: Some(120_345),
            media_type: Some("image/png".into()),
            path: Some("/Users/me/Desktop/shot.png".into()),
            sha256: Some("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into()),
            ..AttachmentMeta::text(AttachmentKind::Image, "shot.png", 0, false)
        },
        AttachmentMeta::text(AttachmentKind::PastedText, "pasted · 312 lines", 2000, true),
    ]
}

fn assistant_session_info() -> SessionInfo {
    SessionInfo {
        title: "Assistant · hutan".into(),
        state: SessionState::Idle,
        command: vec!["assistant".into()],
        chat: Some(ChatInfo {
            fork: None,
            agent: AGENT_ASSISTANT.into(),
            model: "llama3.3:70b".into(),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: false,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: Some("ollama".into()),
            agent_session_id: None,
            assistant: false,
            images: None,
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        }),
        ..chat_session_info()
    }
}

fn permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: "accept".into(),
            label: "Allow once".into(),
            kind: PermissionOptionKind::AllowOnce,
            provider: Some(serde_json::json!("accept")),
        },
        PermissionOption {
            id: "cancel".into(),
            label: "Deny and stop the turn".into(),
            kind: PermissionOptionKind::Cancel,
            provider: Some(serde_json::json!("cancel")),
        },
    ]
}

fn approval_info() -> ApprovalInfo {
    ApprovalInfo {
        id: approval(),
        session_id: sid(),
        turn_id: Some(turn()),
        kind: PermissionKind::Command,
        title: "/bin/zsh -lc 'touch hi.txt'".into(),
        detail: Some("in /repo".into()),
        options: permission_options(),
        requested_at_ms: 1_791_894_170_500,
    }
}

fn question() -> QuestionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3QA".parse().unwrap()
}

fn question_spec() -> QuestionSpec {
    QuestionSpec {
        title: "Codex has a question".into(),
        prompt: "Which database should the migration target?".into(),
        fields: vec![
            QuestionField {
                id: "db".into(),
                label: "Database".into(),
                description: Some("The first choice is recommended".into()),
                kind: FieldKind::Choice {
                    options: vec![
                        ChoiceOption {
                            value: "postgres".into(),
                            label: "PostgreSQL".into(),
                            description: Some("Production default".into()),
                        },
                        ChoiceOption {
                            value: "sqlite".into(),
                            label: "SQLite".into(),
                            description: None,
                        },
                    ],
                    multi: false,
                    allow_other: true,
                },
                required: true,
                default: Some(serde_json::json!("postgres")),
            },
            QuestionField {
                id: "retries".into(),
                label: "Retries".into(),
                description: None,
                kind: FieldKind::Number {
                    integer: true,
                    min: Some(0.0),
                    max: Some(10.0),
                },
                required: false,
                default: None,
            },
            QuestionField {
                id: "dry_run".into(),
                label: "Dry run".into(),
                description: None,
                kind: FieldKind::Boolean,
                required: false,
                default: Some(serde_json::json!(false)),
            },
            QuestionField {
                id: "token".into(),
                label: "Access token".into(),
                description: None,
                kind: FieldKind::Secret,
                required: false,
                default: None,
            },
            QuestionField {
                id: "notes".into(),
                label: "Notes".into(),
                description: None,
                kind: FieldKind::Multiline,
                required: false,
                default: None,
            },
        ],
        raw_schema: None,
        source: Some("MCP server migrations".into()),
        expires: QuestionExpiry::Turn,
    }
}

fn question_info() -> QuestionInfo {
    QuestionInfo {
        id: question(),
        session_id: sid(),
        turn_id: Some(turn()),
        spec: question_spec(),
        requested_at_ms: 1_791_894_170_600,
    }
}

fn question_answer_values() -> hutan_core::question::Values {
    [
        ("db".to_owned(), serde_json::json!("sqlite")),
        ("retries".to_owned(), serde_json::json!(3)),
    ]
    .into_iter()
    .collect()
}

fn ws2() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JB".parse().unwrap()
}

fn workspace_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws(),
        name: "hutan".into(),
        roots: vec!["/repo".into()],
        kind: WorkspaceKind::Project,
        pinned: true,
        position: 2,
        created_by: CreatedBy::Cli,
        last_active_ms: 1_791_894_170_000,
        live_sessions: 2,
        total_sessions: 5,
        parent_id: Some(shop_id()),
        project: Some(ProjectInfo {
            common_dir: Some("/repo/.git".into()),
            root: "/repo".into(),
        }),
    }
}

fn shop_id() -> WorkspaceId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3JC".parse().unwrap()
}

/// A workspace the user made: its roots are those of its projects.
fn container_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: shop_id(),
        name: "shop".into(),
        roots: vec!["/repo".into(), "/Users/me/notes".into()],
        kind: WorkspaceKind::Workspace,
        pinned: true,
        position: 1,
        created_by: CreatedBy::User,
        last_active_ms: 1_791_894_170_000,
        live_sessions: 2,
        total_sessions: 6,
        parent_id: None,
        project: None,
    }
}

/// A project that is a folder outside git, in no workspace.
fn folder_project_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3JD".parse().unwrap(),
        name: "notes".into(),
        roots: vec!["/Users/me/notes".into()],
        kind: WorkspaceKind::Project,
        pinned: false,
        position: 3,
        created_by: CreatedBy::User,
        last_active_ms: 1_791_894_150_000,
        live_sessions: 0,
        total_sessions: 1,
        parent_id: None,
        project: Some(ProjectInfo {
            common_dir: None,
            root: "/Users/me/notes".into(),
        }),
    }
}

fn home_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws2(),
        name: "Home".into(),
        roots: vec!["/Users/me/Hutan".into()],
        kind: WorkspaceKind::Home,
        pinned: false,
        position: 0,
        created_by: CreatedBy::User,
        last_active_ms: 1_791_894_160_000,
        live_sessions: 0,
        total_sessions: 0,
        parent_id: None,
        project: None,
    }
}

fn main_node() -> AgentNode {
    AgentNode {
        node_id: "root".into(),
        parent: None,
        kind: NodeKind::Main,
        source: "claude-hooks".into(),
        agent: "claude".into(),
        name: None,
        agent_type: None,
        state: NodeState::Blocked,
        outcome: None,
        liveness: Liveness::Alive,
        model: Some("claude-opus-5".into()),
        started_at_ms: Some(1_791_894_100_000),
        ended_at_ms: None,
        last_activity_ms: 1_791_894_170_000,
        activity: Some("permission: Bash".into()),
        transcript_path: None,
        pid: Some(48_213),
        usage: None,
    }
}

fn sub_node() -> AgentNode {
    AgentNode {
        node_id: "agent-abc123".into(),
        parent: Some("root".into()),
        kind: NodeKind::Sub,
        source: "claude-hooks".into(),
        agent: "claude".into(),
        name: Some("Explore".into()),
        agent_type: Some("Explore".into()),
        state: NodeState::Idle,
        outcome: Some(NodeOutcome::Completed),
        liveness: Liveness::Gone,
        model: Some("claude-sonnet-5".into()),
        started_at_ms: Some(1_791_894_120_000),
        ended_at_ms: Some(1_791_894_160_000),
        last_activity_ms: 1_791_894_160_000,
        activity: None,
        transcript_path: Some("/h/.claude/projects/p/subagents/agent-abc123.jsonl".into()),
        pid: None,
        usage: Some(Usage {
            kind: "claude.context".into(),
            context_tokens: Some(48_210),
            context_window: Some(200_000),
            ..Usage::default()
        }),
    }
}

fn observation() -> AgentObservation {
    AgentObservation {
        session_id: sid(),
        root: "root".into(),
        nodes: [main_node(), sub_node()]
            .into_iter()
            .map(|n| (n.node_id.clone(), n))
            .collect(),
    }
}

fn sid2() -> SessionId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3KN".parse().unwrap()
}

fn process_info(pid: u32, ppid: u32, name: &str, command: &str) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid,
        name: name.into(),
        command: command.into(),
        state: ProcessState::Sleeping,
        cpu_percent: Some(12.5),
        rss_bytes: 4_194_304,
        threads: 3,
        started_at_ms: Some(1_791_894_164_400),
        agent_node: None,
    }
}

fn resource_sample() -> ResourceSample {
    let shell = process_info(48_213, 1, "zsh", "-zsh");
    let build = ProcessInfo {
        state: ProcessState::Running,
        cpu_percent: Some(187.25),
        rss_bytes: 734_003_200,
        threads: 12,
        agent_node: Some("root".into()),
        ..process_info(
            48_215,
            48_213,
            "cargo",
            "cargo test --workspace --token=[REDACTED]",
        )
    };
    let first = ProcessInfo {
        cpu_percent: None,
        started_at_ms: None,
        ..process_info(48_216, 48_215, "rustc", "rustc --crate-name hutan")
    };
    ResourceSample {
        at_ms: 1_791_894_170_000,
        sessions: vec![
            SessionResources {
                session_id: sid(),
                totals: ResourceTotals {
                    processes: 3,
                    cpu_percent: 199.75,
                    rss_bytes: 4_194_304 * 2 + 734_003_200,
                },
                processes: vec![shell, build, first],
                agents: vec![
                    AgentProcess {
                        node_id: "root".into(),
                        parent: None,
                        kind: NodeKind::Main,
                        label: "omp".into(),
                        state: NodeState::Working,
                        pid: Some(48_215),
                    },
                    AgentProcess {
                        node_id: "agent-abc123".into(),
                        parent: Some("root".into()),
                        kind: NodeKind::Sub,
                        label: "Explore".into(),
                        state: NodeState::Idle,
                        pid: None,
                    },
                ],
                truncated: false,
            },
            SessionResources {
                session_id: sid2(),
                processes: vec![],
                totals: ResourceTotals::default(),
                agents: vec![],
                truncated: true,
            },
        ],
        services: vec![],
        daemon: process_info(912, 1, "hutand", "hutand"),
        truncated: false,
    }
}

fn lsp_server_info(state: LspServerState) -> LspServerInfo {
    LspServerInfo {
        server_id: "rust-analyzer:/Users/me/repo".into(),
        name: "rust-analyzer".into(),
        root: "/Users/me/repo".into(),
        languages: vec!["rust".into()],
        state,
        reason: None,
        pid: Some(51_204),
        restarts: 0,
        rss_bytes: Some(1_468_006_400),
        documents: 2,
        install: None,
        setting: None,
        trust_pending: false,
        capabilities: vec!["hover".into(), "definition".into(), "references".into()],
        completion_triggers: vec![],
        signature_triggers: vec![],
    }
}

fn lsp_not_installed() -> LspServerInfo {
    LspServerInfo {
        state: LspServerState::NotInstalled,
        reason: Some("`rust-analyzer` was not found on the login PATH".into()),
        pid: None,
        rss_bytes: None,
        documents: 1,
        install: Some("rustup component add rust-analyzer".into()),
        setting: Some("lsp.servers.rust-analyzer.command".into()),
        capabilities: vec![],
        ..lsp_server_info(LspServerState::NotInstalled)
    }
}

fn lsp_untrusted() -> LspServerInfo {
    LspServerInfo {
        state: LspServerState::Untrusted,
        reason: Some("the folder is not trusted yet: `hutan lsp trust /Users/me/repo`".into()),
        pid: None,
        rss_bytes: None,
        documents: 1,
        trust_pending: true,
        capabilities: vec![],
        ..lsp_server_info(LspServerState::Untrusted)
    }
}

fn lsp_diagnostic() -> LspDiagnostic {
    LspDiagnostic {
        range: LspRange {
            start: LspPosition {
                line: 41,
                character: 8,
            },
            end: LspPosition {
                line: 41,
                character: 15,
            },
        },
        severity: Some(LspSeverity::Error),
        code: Some("E0425".into()),
        source: Some("rustc".into()),
        message: "cannot find value `missing` in this scope".into(),
        related: vec![LspRelated {
            path: "/Users/me/repo/src/lib.rs".into(),
            range: LspRange::default(),
            message: "similarly named function `mising` defined here".into(),
        }],
    }
}

fn tid() -> TaskId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3TA".parse().unwrap()
}

fn tid2() -> TaskId {
    "01K7F3B5N2Q9W8E4R6T1Y0V3TB".parse().unwrap()
}

/// A task that ran in a worktree with a chat session and was started by hand.
fn running_task() -> Task {
    Task {
        id: tid(),
        workspace_id: ws(),
        title: "Fix the login bug".into(),
        prompt: Some("Users are logged out after a minute; find out why.".into()),
        state: TaskState::Running,
        depends_on: vec![tid2()],
        placement: TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/fix-login".into()),
            base: Some("main".into()),
        },
        run: Some(TaskRun::Chat {
            contain: None,
            agent: "codex".into(),
            model: None,
            account: None,
            safety: None,
        }),
        auto_start: true,
        sessions: vec![sid()],
        created_at_ms: 1_791_894_100_000,
        started_at_ms: Some(1_791_894_164_332),
        ended_at_ms: None,
        outcome: None,
        delivery: DeliveryState::None,
        after_policy: AfterPolicy::Accepted,
        delivery_history: vec![],
    }
}

/// A finished task whose work was reviewed, changed on request, accepted and merged.
fn merged_task() -> Task {
    let mut asked = DeliveryRecord::new(
        DeliveryState::ChangesRequested,
        DeliveryActor::Desktop,
        1_791_894_300_000,
    );
    asked.note = Some("Keep the old session cookie name.".into());
    let mut merged =
        DeliveryRecord::new(DeliveryState::Merged, DeliveryActor::Cli, 1_791_894_500_000);
    merged.commit = Some("9d8c7b6a5f4e3d2c1b0a99887766554433221100".into());
    merged.strategy = Some("squash".into());
    Task {
        state: TaskState::Done,
        ended_at_ms: Some(1_791_894_450_000),
        outcome: Some("turn completed".into()),
        delivery: DeliveryState::Merged,
        after_policy: AfterPolicy::Done,
        delivery_history: vec![
            DeliveryRecord::new(
                DeliveryState::AwaitingReview,
                DeliveryActor::Daemon,
                1_791_894_250_000,
            ),
            asked,
            DeliveryRecord::new(
                DeliveryState::Accepted,
                DeliveryActor::Desktop,
                1_791_894_400_000,
            ),
            merged,
        ],
        ..running_task()
    }
}

/// A task nobody has started: no prompt, no run, the main checkout.
fn minimal_task() -> Task {
    Task {
        id: tid2(),
        workspace_id: ws(),
        title: "Think about it".into(),
        prompt: None,
        state: TaskState::Ready,
        depends_on: vec![],
        placement: TaskPlacement::default(),
        run: None,
        auto_start: false,
        sessions: vec![],
        created_at_ms: 1_791_894_090_000,
        started_at_ms: None,
        ended_at_ms: None,
        outcome: None,
        delivery: DeliveryState::None,
        after_policy: AfterPolicy::Done,
        delivery_history: vec![],
    }
}

fn worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo-wt/feature-login".into(),
        repo_root: "/work/repo".into(),
        branch: Some("feature/login".into()),
        head: "3f786850e387550fdab836ed7e6dc881de23001b".into(),
        main: false,
        created_by_hutan: true,
        dirty: true,
        live_sessions: vec![sid()],
        locked: false,
        prunable: false,
        setup: None,
    }
}

fn setting_up_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        setup: Some(WorktreeSetup {
            state: WorktreeSetupState::NeedsTrust,
            session_id: None,
            commands: vec!["pnpm install".into()],
            teardown: vec!["docker compose down".into()],
            hash: Some("5f2b8c0a9d7e4a31b6c8e0f2a4d6b8c0e2f4a6b8c0d2e4f6a8b0c2d4e6f8a0b2".into()),
            copied: 2,
            message: Some("`.hutan/worktree.toml`: unknown key `retries`".into()),
        }),
        ..worktree_info()
    }
}

fn failed_setup_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        setup: Some(WorktreeSetup {
            state: WorktreeSetupState::Failed,
            session_id: Some(sid()),
            commands: vec!["pnpm install".into(), "pnpm build".into()],
            teardown: vec![],
            hash: None,
            copied: 0,
            message: Some("setup exited with code 1".into()),
        }),
        ..worktree_info()
    }
}

fn main_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo".into(),
        repo_root: "/work/repo".into(),
        branch: Some("master".into()),
        head: "89e6c98d92887913cadf06b2adb97f26cde4849b".into(),
        main: true,
        created_by_hutan: false,
        dirty: false,
        live_sessions: vec![],
        locked: false,
        prunable: false,
        setup: None,
    }
}

fn detached_worktree_info() -> WorktreeInfo {
    WorktreeInfo {
        path: "/work/repo-wt/bisect".into(),
        repo_root: "/work/repo".into(),
        branch: None,
        head: "6b3a3b8b5a0b7c8c0f4e4c1f8e6d5c4b3a291807".into(),
        main: false,
        created_by_hutan: false,
        dirty: false,
        live_sessions: vec![],
        locked: true,
        prunable: true,
        setup: None,
    }
}

fn ui_publish_fixture() -> hutan_protocol::ui::UiPublish {
    use hutan_protocol::ui::*;
    UiPublish {
        actions: vec![
            UiAction {
                id: "workbench::SplitRight".into(),
                title: "Layout: Split Right".into(),
                args_schema: None,
            },
            UiAction {
                id: "workbench::ApplyPreset".into(),
                title: "Layout: Apply Preset".into(),
                args_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"name": {"type": "string", "enum": ["default", "focus"]}},
                    "required": ["name"],
                })),
            },
        ],
        settings: vec![UiSetting {
            namespace: "terminal".into(),
            key: "cursor.style".into(),
            kind: "enum".into(),
            doc: "Shape of the cursor.".into(),
            layers: vec!["user".into(), "workspace".into(), "project".into()],
            values: vec!["block".into(), "bar".into(), "underline".into()],
            current: Some(serde_json::json!("block")),
        }],
        layout: layout_fixture(),
    }
}

fn layout_fixture() -> hutan_protocol::ui::LayoutSummary {
    use hutan_protocol::ui::*;
    LayoutSummary {
        window_id: "window-1".into(),
        workspace_id: Some(ws()),
        preset: "default".into(),
        docks: vec![
            DockSummary {
                position: "left".into(),
                visible: true,
                panels: vec!["sessions".into(), "history".into()],
                active: Some("sessions".into()),
            },
            DockSummary {
                position: "bottom".into(),
                visible: false,
                panels: vec![],
                active: None,
            },
        ],
        panes: PaneSummary::Split {
            axis: "horizontal".into(),
            children: vec![
                PaneSummary::Pane {
                    pane_id: "pane-1".into(),
                    items: vec![PaneItem {
                        kind: "terminal".into(),
                        session_id: Some(sid()),
                        panel: None,
                        path: None,
                        title: "omp".into(),
                    }],
                    active: Some(0),
                },
                PaneSummary::Pane {
                    pane_id: "pane-2".into(),
                    items: vec![PaneItem {
                        kind: "panel".into(),
                        session_id: None,
                        panel: Some("inspector".into()),
                        path: None,
                        title: "Inspector".into(),
                    }],
                    active: Some(0),
                },
            ],
        },
        focused: FocusSummary {
            kind: "pane".into(),
            target: "pane-1".into(),
        },
        zoomed: None,
        appearance: AppearanceSummary {
            chrome: "dark".into(),
            density: "compact".into(),
        },
        window_active: true,
    }
}

fn control_approval() -> ApprovalInfo {
    ApprovalInfo {
        id: approval(),
        session_id: sid(),
        turn_id: None,
        kind: PermissionKind::Control,
        title: "Agent in session “omp” asks Hutan to stop session “api”".into(),
        detail: Some("{\"session_id\":\"01K7F3B5N2Q9W8E4R6T1Y0V3KM\"}".into()),
        options: vec![
            PermissionOption {
                id: "allow_once".into(),
                label: "Allow once".into(),
                kind: PermissionOptionKind::AllowOnce,
                provider: None,
            },
            PermissionOption {
                id: "allow_session".into(),
                label: "Allow for this session".into(),
                kind: PermissionOptionKind::AllowAlways,
                provider: None,
            },
            PermissionOption {
                id: "deny".into(),
                label: "Deny".into(),
                kind: PermissionOptionKind::RejectOnce,
                provider: None,
            },
        ],
        requested_at_ms: 1_791_894_180_000,
    }
}

fn control_grants_approval() -> ApprovalInfo {
    let option = |id: &str, label: &str, kind| PermissionOption {
        id: id.into(),
        label: label.into(),
        kind,
        provider: None,
    };
    ApprovalInfo {
        id: approval(),
        session_id: sid(),
        turn_id: None,
        kind: PermissionKind::Control,
        title: "Mawas asks Hutan to rename session “build” to “release”".into(),
        detail: Some(
            "{\"session_id\":\"01K7F3B5N2Q9W8E4R6T1Y0V3KM\",\"title\":\"release\"}".into(),
        ),
        options: vec![
            option("allow_once", "Allow once", PermissionOptionKind::AllowOnce),
            option(
                "allow_chat",
                "Allow for this chat",
                PermissionOptionKind::AllowAlways,
            ),
            option(
                "allow_workspace",
                "Always allow this tool in this workspace",
                PermissionOptionKind::AllowAlways,
            ),
            option("deny", "Deny", PermissionOptionKind::RejectOnce),
        ],
        requested_at_ms: 1_791_894_180_000,
    }
}

fn memory_user_provenance() -> MemoryProvenance {
    MemoryProvenance {
        author: MemoryAuthor::User,
        client: Some("desktop".into()),
        session_id: None,
        turn_id: None,
        at_ms: 1_791_894_100_000,
        origin: None,
    }
}

fn memory_agent_provenance() -> MemoryProvenance {
    MemoryProvenance {
        author: MemoryAuthor::Agent,
        client: None,
        session_id: Some(sid()),
        turn_id: Some(turn()),
        at_ms: 1_791_894_180_000,
        origin: None,
    }
}

fn memory_entry() -> MemoryEntry {
    MemoryEntry {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
        scope: MemoryScope::Global,
        workspace_id: None,
        category: MemoryCategory::Profile,
        state: MemoryState::Active,
        revision: 1,
        body: "Prefers short answers.".into(),
        inferred: false,
        provenance: memory_user_provenance(),
        created_ms: 1_791_894_100_000,
        updated_ms: 1_791_894_100_000,
    }
}

fn memory_entry_inferred() -> MemoryEntry {
    MemoryEntry {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3KQ".into(),
        scope: MemoryScope::Workspace,
        workspace_id: Some(ws()),
        category: MemoryCategory::Fact,
        state: MemoryState::Active,
        revision: 2,
        body: "Uses jj for version control; main branch is `trunk`.".into(),
        inferred: true,
        provenance: memory_agent_provenance(),
        created_ms: 1_791_894_100_000,
        updated_ms: 1_791_894_200_000,
    }
}

fn memory_proposal() -> MemoryProposal {
    MemoryProposal {
        id: approval(),
        operation: MemoryOp::Update,
        state: ProposalState::Pending,
        scope: MemoryScope::Workspace,
        workspace_id: Some(ws()),
        category: MemoryCategory::Fact,
        entry_id: Some("01K7F3B5N2Q9W8E4R6T1Y0V3KQ".into()),
        expected_revision: Some(1),
        body: "Uses jj for version control; main branch is `trunk`.".into(),
        current_body: Some("Uses jj for version control.".into()),
        provenance: memory_agent_provenance(),
        created_ms: 1_791_894_180_000,
        resolved_ms: None,
    }
}

fn memory_approval() -> ApprovalInfo {
    let option = |id: &str, label: &str, kind| PermissionOption {
        id: id.into(),
        label: label.into(),
        kind,
        provider: None,
    };
    ApprovalInfo {
        id: approval(),
        session_id: sid(),
        turn_id: None,
        kind: PermissionKind::Control,
        title: "Mawas proposes to remember (update a fact of this workspace)".into(),
        detail: Some("Uses jj for version control; main branch is `trunk`.".into()),
        options: vec![
            option("approve", "Approve", PermissionOptionKind::AllowOnce),
            option("reject", "Reject", PermissionOptionKind::RejectOnce),
        ],
        requested_at_ms: 1_791_894_180_000,
    }
}

fn file_version() -> FileVersion {
    FileVersion {
        mtime_ns: 1_791_894_150_123_456_789,
        size: 6_291_456,
        ino: 8_834_113,
    }
}

fn fs_entry(name: &str, kind: FsKind, ignored: bool) -> FsEntry {
    FsEntry {
        name: name.into(),
        path: format!("/repo/{name}").into(),
        kind,
        size: if kind == FsKind::File { 1_204 } else { 96 },
        mtime_ns: 1_791_894_150_123_456_789,
        ino: 8_834_113,
        ignored,
    }
}

fn hook_run() -> HookRun {
    HookRun {
        id: 7,
        hook: 0,
        event: HookEvent::ApprovalRequested,
        test: false,
        started_at_ms: 1_791_894_200_000,
        duration_ms: 212,
        outcome: HookOutcome::Ok,
        exit_code: Some(0),
        signal: None,
        session_id: Some(sid()),
        stdout: "sent\n".into(),
        stderr: String::new(),
        output_truncated: false,
        error: None,
    }
}

fn hook_payload() -> HookPayload {
    HookPayload {
        session_id: Some(sid()),
        session_title: Some("api".into()),
        harness: Some("claude".into()),
        project_id: Some(ws()),
        project: Some("hutan".into()),
        workspace_id: None,
        workspace: None,
        state: Some("pending".into()),
        detail: Some("Run `cargo test`".into()),
        approval_id: Some("01K7F3B5N2Q9W8E4R6T1Y0V3KP".into()),
        ..HookPayload::new(HookEvent::ApprovalRequested, 1_791_894_200_000)
    }
}

fn finish_payload() -> HookPayload {
    HookPayload {
        state: Some("ok".into()),
        detail: Some("hutan-wt-feature".into()),
        branch: Some("feature".into()),
        target_branch: Some("main".into()),
        commit_step: Some("ok".into()),
        merge_step: Some("ok".into()),
        push_step: Some("skipped".into()),
        changed_files: Some(3),
        ..HookPayload::new(HookEvent::FinishDone, 1_791_894_200_000)
    }
}

fn requests() -> Vec<(&'static str, Request)> {
    vec![
        (
            "hello",
            Request::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                client: ClientInfo {
                    kind: ClientKind::Desktop,
                    version: "0.1.0".into(),
                    origin: None,
                },
                capabilities: vec!["terminal.render.v1".into()],
                resume: Some(ResumePoint {
                    epoch: epoch(),
                    seq: Seq(1042),
                }),
            }),
        ),
        ("daemon_status", Request::DaemonStatus(Empty {})),
        (
            "daemon_shutdown",
            Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget: false,
                restart: false,
            }),
        ),
        ("events_subscribe", Request::EventsSubscribe(Empty {})),
        (
            "workspace_create",
            Request::WorkspaceCreate(WorkspaceCreate {
                name: "hutan".into(),
                paths: vec!["/repo".into()],
                project_ids: vec![],
                created_by: None,
            }),
        ),
        ("workspace_list", Request::WorkspaceList(Empty {})),
        (
            "workspace_rename",
            Request::WorkspaceRename(WorkspaceRename {
                workspace_id: ws(),
                name: "core".into(),
            }),
        ),
        (
            "workspace_close",
            Request::WorkspaceClose(WorkspaceRef { workspace_id: ws() }),
        ),
        (
            "session_create",
            Request::SessionCreate(SessionCreate {
                workspace_id: Some(ws()),
                spec: SessionSpec::Terminal(TerminalSpec {
                    contain: None,
                    command: vec!["omp".into()],
                    cwd: Some("/repo".into()),
                    env: BTreeMap::from([("RUST_LOG".into(), "debug".into())]),
                    title: None,
                    size: Some(size()),
                }),
            }),
        ),
        (
            "session_create_shell",
            Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec::default()),
            }),
        ),
        (
            "session_list",
            Request::SessionList(SessionListFilter {
                workspace_id: Some(ws()),
            }),
        ),
        ("session_snapshot", Request::SessionSnapshot(Empty {})),
        (
            "session_stop",
            Request::SessionStop(SessionRef { session_id: sid() }),
        ),
        (
            "session_restart",
            Request::SessionRestart(SessionRef { session_id: sid() }),
        ),
        (
            "session_rename",
            Request::SessionRename(SessionRename {
                session_id: sid(),
                title: "agent".into(),
            }),
        ),
        (
            "terminal_attach",
            Request::TerminalAttach(TerminalAttach {
                terminal_id: TID,
                size: Some(size()),
            }),
        ),
        (
            "terminal_detach",
            Request::TerminalDetach(TerminalRef { terminal_id: TID }),
        ),
        (
            "terminal_resize",
            Request::TerminalResize(TerminalResize {
                terminal_id: TID,
                size: size(),
            }),
        ),
        (
            "terminal_ack",
            Request::TerminalAck(TerminalAck {
                terminal_id: TID,
                frame: 42,
            }),
        ),
        (
            "terminal_history",
            Request::TerminalHistory(TerminalHistory {
                terminal_id: TID,
                from_line: 0,
                count: 100,
            }),
        ),
        (
            "terminal_set_colors",
            Request::TerminalSetColors(TerminalSetColors {
                terminal_id: TID,
                colors: TerminalColors {
                    foreground: (212, 212, 212),
                    background: (30, 30, 30),
                    cursor: (212, 212, 212),
                },
            }),
        ),
        (
            "daemon_shutdown_forget",
            Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget: true,
                restart: false,
            }),
        ),
        (
            "agent_report",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                agent: "claude".into(),
                state: Some(AgentState::Blocked),
                message: Some("Bash: rm -rf build".into()),
                seq: 1_791_894_164_332_000_000,
                agent_session_id: Some("5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into()),
                resume_argv: Some(vec![
                    "claude".into(),
                    "--resume".into(),
                    "5f3e1c1a-8d51-4a6e-9a0c-2b7a3c5d9e10".into(),
                ]),
                node: None,
                pid: Some(48_213),
            }),
        ),
        (
            "agent_report_minimal",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: Some(AgentState::Working),
                message: None,
                seq: 7,
                agent_session_id: None,
                resume_argv: None,
                node: None,
                pid: None,
            }),
        ),
        (
            "agent_release",
            Request::AgentRelease(AgentReleaseRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                seq: 1_791_894_164_999_000_000,
                node: None,
                outcome: None,
            }),
        ),
        (
            "terminal_blocks",
            Request::TerminalBlocks(TerminalBlocks {
                terminal_id: TID,
                before_id: Some(13),
                limit: 50,
            }),
        ),
        (
            "terminal_blocks_newest",
            Request::TerminalBlocks(TerminalBlocks {
                terminal_id: TID,
                before_id: None,
                limit: 500,
            }),
        ),
        (
            "terminal_block_text",
            Request::TerminalBlockText(TerminalBlockText {
                terminal_id: TID,
                block_id: 12,
                part: BlockPart::Output,
                max_bytes: 65536,
            }),
        ),
        (
            "workspace_create_agent",
            Request::WorkspaceCreate(WorkspaceCreate {
                name: "scratch".into(),
                paths: vec![],
                project_ids: vec![],
                created_by: Some(CreatedBy::Agent),
            }),
        ),
        (
            "workspace_open",
            Request::WorkspaceOpen(WorkspaceOpen {
                path: "/repo/crates/daemon".into(),
                created_by: None,
            }),
        ),
        (
            "workspace_open_cli",
            Request::WorkspaceOpen(WorkspaceOpen {
                path: "/repo".into(),
                created_by: Some(CreatedBy::Cli),
            }),
        ),
        (
            "workspace_add_root",
            Request::WorkspaceAddRoot(WorkspaceRoot {
                workspace_id: ws(),
                path: "/other-repo".into(),
            }),
        ),
        (
            "workspace_remove_root",
            Request::WorkspaceRemoveRoot(WorkspaceRoot {
                workspace_id: ws(),
                path: "/other-repo".into(),
            }),
        ),
        (
            "workspace_recent",
            Request::WorkspaceRecent(WorkspaceRecent { limit: None }),
        ),
        (
            "workspace_recent_limit",
            Request::WorkspaceRecent(WorkspaceRecent { limit: Some(50) }),
        ),
        (
            "workspace_move",
            Request::WorkspaceMove(WorkspaceMove {
                workspace_id: ws(),
                before: Some(ws2()),
            }),
        ),
        (
            "workspace_move_last",
            Request::WorkspaceMove(WorkspaceMove {
                workspace_id: ws(),
                before: None,
            }),
        ),
        (
            "workspace_pin",
            Request::WorkspacePin(WorkspacePin {
                workspace_id: ws(),
                pinned: true,
            }),
        ),
        (
            "history_search",
            Request::HistorySearch(HistorySearch {
                query: "cargo test -p daemon".into(),
                limit: 20,
                sources: vec![HistorySource::Claude, HistorySource::Hutan],
                roles: vec![HistoryRole::Command, HistoryRole::User],
                project: Some("/repo".into()),
                since_ms: Some(1_791_000_000_000),
                until_ms: Some(1_791_900_000_000),
            }),
        ),
        (
            "history_search_minimal",
            Request::HistorySearch(HistorySearch {
                query: "quokka".into(),
                limit: SEARCH_DEFAULT_LIMIT,
                sources: vec![],
                roles: vec![],
                project: None,
                since_ms: None,
                until_ms: None,
            }),
        ),
        ("history_status", Request::HistoryStatus(Empty {})),
        (
            "history_set_enabled",
            Request::HistorySetEnabled(HistorySetEnabled { enabled: true }),
        ),
        ("history_clear", Request::HistoryClear(Empty {})),
        ("history_rebuild", Request::HistoryRebuild(Empty {})),
        (
            "chat_create",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: Some(ws()),
                cwd: "/repo".into(),
                agent: "codex".into(),
                model: Some("gpt-6.1-sol".into()),
                approval_policy: Some(ApprovalPolicy::Untrusted),
                sandbox: Some(SandboxMode::ReadOnly),
                title: Some("fix the build".into()),
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            }),
        ),
        (
            "chat_create_minimal",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: "/repo".into(),
                agent: "codex".into(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            }),
        ),
        (
            "chat_send",
            Request::ChatSend(ChatSend {
                session_id: sid(),
                text: "run the tests".into(),
                attachments: vec![],
            }),
        ),
        (
            "chat_interrupt",
            Request::ChatInterrupt(ChatInterrupt {
                session_id: sid(),
                keep_queue: false,
            }),
        ),
        (
            "chat_history",
            Request::ChatHistory(ChatHistoryRequest {
                session_id: sid(),
                before_seq: Some(120),
                limit: 50,
            }),
        ),
        (
            "chat_history_newest",
            Request::ChatHistory(ChatHistoryRequest {
                session_id: sid(),
                before_seq: None,
                limit: HISTORY_DEFAULT_LIMIT,
            }),
        ),
        (
            "chat_models",
            Request::ChatModels(ChatModelsRequest {
                agent: "codex".into(),
            }),
        ),
        (
            "approval_list",
            Request::ApprovalList(ApprovalListRequest { session_id: None }),
        ),
        (
            "approval_list_session",
            Request::ApprovalList(ApprovalListRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "approval_respond",
            Request::ApprovalRespond(ApprovalRespond {
                approval_id: approval(),
                option_id: "accept".into(),
            }),
        ),
        (
            "agent_report_node",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                agent: "claude".into(),
                state: Some(AgentState::Working),
                message: None,
                seq: 1_791_894_165_000_000_000,
                agent_session_id: None,
                resume_argv: None,
                node: Some(NodeReport {
                    id: "agent-abc123".into(),
                    parent: Some("root".into()),
                    kind: NodeKind::Sub,
                    name: Some("Explore".into()),
                    agent_type: Some("Explore".into()),
                    outcome: None,
                    model: Some("claude-sonnet-5".into()),
                    activity: Some("Bash: cargo test".into()),
                    transcript_path: None,
                    usage: Some(Usage {
                        kind: "claude.context".into(),
                        context_tokens: Some(48_210),
                        context_window: Some(200_000),
                        cost_usd: Some(0.0412),
                        ..Usage::default()
                    }),
                }),
                pid: Some(48_220),
            }),
        ),
        (
            "agent_report_node_minimal",
            Request::AgentReport(AgentReportRequest {
                session_id: sid(),
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: Some(AgentState::Idle),
                message: None,
                seq: 8,
                agent_session_id: None,
                resume_argv: None,
                node: Some(NodeReport {
                    id: "Main".into(),
                    parent: None,
                    kind: NodeKind::Main,
                    name: None,
                    agent_type: None,
                    outcome: None,
                    model: None,
                    activity: None,
                    transcript_path: None,
                    usage: None,
                }),
                pid: None,
            }),
        ),
        (
            "agent_release_node",
            Request::AgentRelease(AgentReleaseRequest {
                session_id: sid(),
                source: "claude-hooks".into(),
                seq: 1_791_894_166_000_000_000,
                node: Some("agent-abc123".into()),
                outcome: Some(NodeOutcome::Completed),
            }),
        ),
        (
            "agent_tree",
            Request::AgentTree(AgentTreeRequest { session_id: None }),
        ),
        (
            "agent_tree_session",
            Request::AgentTree(AgentTreeRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "chat_create_direct",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: "/repo".into(),
                agent: AGENT_DIRECT.into(),
                model: Some("meta-llama/llama-3.3-70b-instruct".into()),
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: Some("openrouter".into()),
                system_prompt: Some("Answer in one sentence.".into()),
                account: None,
                safety: None,
            }),
        ),
        (
            "chat_set_model",
            Request::ChatSetModel(ChatSetModel {
                session_id: sid(),
                provider_id: None,
                model: "llama3.3:70b".into(),
            }),
        ),
        (
            "chat_set_model_provider",
            Request::ChatSetModel(ChatSetModel {
                session_id: sid(),
                provider_id: Some("ollama".into()),
                model: "llama3.3:70b".into(),
            }),
        ),
        ("provider_list", Request::ProviderList(Empty {})),
        (
            "provider_add",
            Request::ProviderAdd(ProviderAdd {
                kind: "openai-compatible".into(),
                name: "OpenRouter".into(),
                base_url: Some("https://openrouter.ai/api/v1".into()),
                default_model: Some("meta-llama/llama-3.3-70b-instruct".into()),
            }),
        ),
        (
            "provider_add_minimal",
            Request::ProviderAdd(ProviderAdd {
                kind: "ollama".into(),
                name: "ollama".into(),
                base_url: None,
                default_model: None,
            }),
        ),
        (
            "provider_update",
            Request::ProviderUpdate(ProviderUpdate {
                provider_id: "openrouter".into(),
                name: Some("OpenRouter (work)".into()),
                base_url: None,
                default_model: Some(String::new()),
            }),
        ),
        (
            "provider_remove",
            Request::ProviderRemove(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        (
            // A placeholder, not a key of any account.
            "provider_set_credential",
            Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: "openrouter".into(),
                secret: "not-a-real-key".into(),
            }),
        ),
        (
            "provider_clear_credential",
            Request::ProviderClearCredential(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        (
            "provider_models",
            Request::ProviderModels(ProviderRef {
                provider_id: "openrouter".into(),
            }),
        ),
        ("provider_discover", Request::ProviderDiscover(Empty {})),
        (
            "chat_send_attachments",
            Request::ChatSend(ChatSend {
                session_id: sid(),
                text: "why does this fail?".into(),
                attachments: vec![
                    Attachment::ShellBlock {
                        terminal_id: TID,
                        block_id: 12,
                    },
                    Attachment::ShellRecent {
                        terminal_id: TID,
                        lines: 80,
                    },
                    Attachment::AgentTranscript {
                        session_id: "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                        last_turns: 3,
                    },
                    Attachment::File {
                        path: "src/main.rs".into(),
                        range: Some(LineRange { start: 10, end: 40 }),
                        picked: false,
                    },
                    Attachment::File {
                        path: "/repo/README.md".into(),
                        range: None,
                        picked: true,
                    },
                    Attachment::Image {
                        path: Some("/Users/me/Desktop/shot.png".into()),
                        data_base64: None,
                        media_type: "image/png".into(),
                    },
                    Attachment::Image {
                        path: None,
                        data_base64: Some("iVBORw0KGgp4eXo=".into()),
                        media_type: "image/png".into(),
                    },
                    Attachment::PastedText {
                        text: "line one\nline two\n".into(),
                    },
                    Attachment::Search {
                        query: "scheduler stall".into(),
                        limit: 5,
                    },
                ],
            }),
        ),
        (
            "assistant_open_global",
            Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }),
        ),
        (
            "assistant_open_workspace_new",
            Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(ws()),
                new: true,
            }),
        ),
        (
            "assistant_list",
            Request::AssistantList(AssistantListRequest {
                scope: Some(AssistantScope::Workspace),
                workspace_id: Some(ws()),
            }),
        ),
        (
            "assistant_list_all",
            Request::AssistantList(AssistantListRequest::default()),
        ),
        ("notify_list", Request::NotifyList(Empty {})),
        (
            "notify_ack",
            Request::NotifyAck(NotifyAck {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
            }),
        ),
        (
            "notify_act_approval",
            Request::NotifyAct(NotifyAct {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
                action_id: "accept".into(),
            }),
        ),
        (
            "notify_act_open",
            Request::NotifyAct(NotifyAct {
                id: "01K7F3B5N2Q9W8E4R6T1Y0V3NU".into(),
                action_id: hutan_protocol::notify::ACTION_OPEN.into(),
            }),
        ),
        (
            "client_focus",
            Request::ClientFocus(ClientFocus {
                session_id: Some(sid()),
            }),
        ),
        (
            "client_focus_none",
            Request::ClientFocus(ClientFocus::default()),
        ),
        (
            "worktree_create",
            Request::WorktreeCreate(WorktreeCreate {
                workspace_id: ws(),
                repo: Some("/work/repo".into()),
                branch: "feature/login".into(),
                base: Some("origin/main".into()),
                path: Some("/work/repo-wt/feature-login".into()),
                no_setup: false,
            }),
        ),
        (
            "worktree_create_minimal",
            Request::WorktreeCreate(WorktreeCreate {
                workspace_id: ws(),
                repo: None,
                branch: "fix-typo".into(),
                base: None,
                path: None,
                no_setup: false,
            }),
        ),
        (
            "worktree_list",
            Request::WorktreeList(WorktreeListRequest {
                workspace_id: Some(ws()),
            }),
        ),
        (
            "worktree_list_all",
            Request::WorktreeList(WorktreeListRequest::default()),
        ),
        (
            "worktree_remove",
            Request::WorktreeRemove(WorktreeRemove {
                path: "/work/repo-wt/feature-login".into(),
                force: false,
                delete_branch: false,
            }),
        ),
        (
            "worktree_remove_force_delete_branch",
            Request::WorktreeRemove(WorktreeRemove {
                path: "/work/repo-wt/feature-login".into(),
                force: true,
                delete_branch: true,
            }),
        ),
        (
            "task_create",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Fix the login bug".into(),
                prompt: Some("Users are logged out after a minute; find out why.".into()),
                depends_on: vec![tid2()],
                placement: Some(TaskPlacement {
                    kind: TaskPlacementKind::Worktree,
                    branch: Some("task/fix-login".into()),
                    base: Some("main".into()),
                }),
                run: Some(TaskRun::Chat {
                    contain: None,
                    agent: "codex".into(),
                    model: None,
                    account: None,
                    safety: None,
                }),
                auto_start: true,
                after_policy: AfterPolicy::Accepted,
            }),
        ),
        (
            "task_create_chat_model",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Fix the login bug".into(),
                prompt: Some("Users are logged out after a minute; find out why.".into()),
                depends_on: vec![],
                placement: None,
                run: Some(TaskRun::Chat {
                    contain: None,
                    agent: "omp".into(),
                    model: Some("anthropic/claude-opus-4-5".into()),
                    account: None,
                    safety: None,
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "task_create_terminal",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Run the tests".into(),
                prompt: None,
                depends_on: vec![],
                placement: None,
                run: Some(TaskRun::Terminal {
                    contain: None,
                    argv: vec!["cargo".into(), "test".into()],
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "task_create_minimal",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Think about it".into(),
                prompt: None,
                depends_on: vec![],
                placement: None,
                run: None,
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "task_list",
            Request::TaskList(TaskListRequest {
                workspace_id: Some(ws()),
                state: Some(TaskState::Ready),
            }),
        ),
        (
            "task_list_all",
            Request::TaskList(TaskListRequest::default()),
        ),
        (
            "task_update_state",
            Request::TaskUpdate(TaskUpdate {
                id: tid(),
                title: None,
                prompt: None,
                depends_on: None,
                placement: None,
                run: None,
                auto_start: None,
                after_policy: None,
                state: Some(TaskState::Cancelled),
            }),
        ),
        (
            "task_update_fields",
            Request::TaskUpdate(TaskUpdate {
                id: tid(),
                title: Some("Fix the logout bug".into()),
                prompt: Some(String::new()),
                depends_on: Some(vec![]),
                placement: Some(TaskPlacement::default()),
                run: Some(TaskRun::Terminal {
                    contain: None,
                    argv: vec!["sh".into(), "-c".into(), "make".into()],
                }),
                auto_start: Some(false),
                after_policy: Some(AfterPolicy::Merged),
                state: None,
            }),
        ),
        (
            "task_delete",
            Request::TaskDelete(TaskDelete {
                id: tid(),
                cascade: false,
            }),
        ),
        (
            "task_delete_cascade",
            Request::TaskDelete(TaskDelete {
                id: tid(),
                cascade: true,
            }),
        ),
        ("task_start", Request::TaskStart(TaskRef { id: tid() })),
        (
            "chat_create_harness",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: "/repo".into(),
                agent: "omp".into(),
                model: Some("anthropic/claude-sonnet-4-5".into()),
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: Some("plan".into()),
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            }),
        ),
        ("account_list", Request::AccountList(Empty {})),
        (
            "account_login",
            Request::AccountLogin(AccountLogin {
                harness: "omp".into(),
                provider: Some("anthropic".into()),
                account: None,
            }),
        ),
        (
            "account_login_any",
            Request::AccountLogin(AccountLogin {
                harness: "claude".into(),
                provider: None,
                account: None,
            }),
        ),
        (
            "account_logout",
            Request::AccountLogout(AccountLogout {
                harness: "codex".into(),
                provider: None,
                account: None,
            }),
        ),
        (
            "account_login_managed",
            Request::AccountLogin(AccountLogin {
                harness: "claude".into(),
                provider: None,
                account: Some("work".into()),
            }),
        ),
        (
            "task_create_chat_safety",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Refactor contained".into(),
                prompt: Some("Split the module".into()),
                depends_on: vec![],
                placement: None,
                run: Some(TaskRun::Chat {
                    contain: None,
                    agent: "codex".into(),
                    model: None,
                    account: None,
                    safety: Some(hutan_core::SafetyPreset::ReadOnly),
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "task_create_chat_account",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Review as work".into(),
                prompt: Some("Review the diff".into()),
                depends_on: vec![],
                placement: None,
                run: Some(TaskRun::Chat {
                    contain: None,
                    agent: "claude".into(),
                    model: None,
                    account: Some("work".into()),
                    safety: None,
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "account_add",
            Request::AccountAdd(AccountAdd {
                harness: "claude".into(),
                name: "work".into(),
                label: Some("Work".into()),
            }),
        ),
        (
            "account_add_unlabelled",
            Request::AccountAdd(AccountAdd {
                harness: "codex".into(),
                name: "home".into(),
                label: None,
            }),
        ),
        (
            "account_remove",
            Request::AccountRemove(AccountRemove {
                harness: "claude".into(),
                name: "work".into(),
            }),
        ),
        (
            "account_default_harness",
            Request::AccountDefault(AccountDefaultSet {
                harness: "claude".into(),
                account: Some("work".into()),
                project: None,
            }),
        ),
        (
            "account_default_project_system",
            Request::AccountDefault(AccountDefaultSet {
                harness: "claude".into(),
                account: Some("system".into()),
                project: Some("web".into()),
            }),
        ),
        (
            "account_default_clear",
            Request::AccountDefault(AccountDefaultSet {
                harness: "omp".into(),
                account: None,
                project: None,
            }),
        ),
        (
            "chat_create_account",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: "/work/repo".into(),
                agent: "claude".into(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: Some("work".into()),
                safety: None,
            }),
        ),
        (
            "chat_create_safety",
            Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: "/work/repo".into(),
                agent: "claude".into(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: Some(hutan_core::SafetyPreset::Contained),
            }),
        ),
        (
            "chat_create_contain",
            Request::ChatCreate(ChatCreate {
                contain: Some(hutan_core::Contain {
                    network: Some(false),
                    extra_write: vec!["/work/cache".into()],
                }),
                workspace_id: None,
                cwd: "/work/repo".into(),
                agent: "omp".into(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: Some(hutan_core::SafetyPreset::Contained),
            }),
        ),
        (
            "task_create_terminal_contain",
            Request::TaskCreate(TaskCreate {
                workspace_id: ws(),
                title: "Run the tests contained".into(),
                prompt: None,
                depends_on: vec![],
                placement: None,
                run: Some(TaskRun::Terminal {
                    argv: vec!["cargo".into(), "test".into()],
                    contain: Some(hutan_core::Contain::default()),
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            }),
        ),
        (
            "session_create_contain",
            Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    cwd: Some("/work/repo".into()),
                    contain: Some(hutan_core::Contain {
                        network: Some(false),
                        extra_write: vec![],
                    }),
                    ..TerminalSpec::default()
                }),
            }),
        ),
        (
            "resources_list",
            Request::ResourcesList(ResourcesListRequest { session_id: None }),
        ),
        (
            "resources_list_session",
            Request::ResourcesList(ResourcesListRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "resources_subscribe",
            Request::ResourcesSubscribe(ResourcesSubscribe { interval_ms: None }),
        ),
        (
            "resources_subscribe_interval",
            Request::ResourcesSubscribe(ResourcesSubscribe {
                interval_ms: Some(5_000),
            }),
        ),
        (
            "resources_unsubscribe",
            Request::ResourcesUnsubscribe(Empty {}),
        ),
        (
            "session_kill",
            Request::SessionKill(SessionRef { session_id: sid() }),
        ),
        (
            "process_signal",
            Request::ProcessSignal(ProcessSignal {
                session_id: sid(),
                pid: 48_215,
                signal: SignalName::Kill,
            }),
        ),
        (
            "agent_interrupt",
            Request::AgentInterrupt(AgentInterrupt {
                session_id: sid(),
                node_id: None,
                force: false,
            }),
        ),
        (
            "agent_interrupt_node_force",
            Request::AgentInterrupt(AgentInterrupt {
                session_id: sid(),
                node_id: Some("agent-abc123".into()),
                force: true,
            }),
        ),
        (
            "workspace_create_projects",
            Request::WorkspaceCreate(WorkspaceCreate {
                name: "shop".into(),
                paths: vec!["/repo".into(), "/Users/me/notes".into()],
                project_ids: vec![ws2()],
                created_by: Some(CreatedBy::Cli),
            }),
        ),
        (
            "workspace_assign",
            Request::WorkspaceAssign(WorkspaceAssign {
                project_id: ws(),
                workspace_id: Some(shop_id()),
            }),
        ),
        (
            "workspace_assign_none",
            Request::WorkspaceAssign(WorkspaceAssign {
                project_id: ws(),
                workspace_id: None,
            }),
        ),
        (
            "workspace_delete",
            Request::WorkspaceDelete(WorkspaceDelete {
                workspace_id: shop_id(),
            }),
        ),
        (
            "assistant_configure_global_omp",
            Request::AssistantConfigure(AssistantConfigure {
                scope: AssistantScope::Global,
                workspace_id: None,
                backend: AssistantBackend::Omp,
                model: Some("anthropic/claude-sonnet-4-5".into()),
                provider: None,
            }),
        ),
        (
            "assistant_configure_workspace_provider",
            Request::AssistantConfigure(AssistantConfigure {
                scope: AssistantScope::Workspace,
                workspace_id: Some(ws()),
                backend: AssistantBackend::Provider,
                model: Some("llama3.3:70b".into()),
                provider: Some("ollama".into()),
            }),
        ),
        (
            "hello_origin",
            Request::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                client: ClientInfo {
                    kind: ClientKind::Cli,
                    version: "0.1.0".into(),
                    origin: Some(Origin { session_id: sid() }),
                },
                capabilities: vec!["control".into()],
                resume: None,
            }),
        ),
        ("ui_publish", Request::UiPublish(ui_publish_fixture())),
        (
            "ui_actions",
            Request::UiActions(hutan_protocol::ui::UiTarget::default()),
        ),
        (
            "ui_actions_window",
            Request::UiActions(hutan_protocol::ui::UiTarget {
                window_id: Some("window-1".into()),
            }),
        ),
        (
            "ui_layout",
            Request::UiLayout(hutan_protocol::ui::UiTarget::default()),
        ),
        (
            "ui_run",
            Request::UiRun(hutan_protocol::ui::UiRun {
                window_id: None,
                action: "workbench::SplitRight".into(),
                args: None,
            }),
        ),
        (
            "ui_run_args",
            Request::UiRun(hutan_protocol::ui::UiRun {
                window_id: Some("window-1".into()),
                action: "workbench::ApplyPreset".into(),
                args: Some(serde_json::json!({"name": "focus"})),
            }),
        ),
        (
            "ui_set_setting",
            Request::UiSetSetting(hutan_protocol::ui::UiSetSetting {
                window_id: None,
                namespace: "terminal".into(),
                key: "cursor.style".into(),
                value: serde_json::json!("bar"),
                layer: "user".into(),
            }),
        ),
        (
            "ui_command_result_ok",
            Request::UiCommandResult(hutan_protocol::ui::UiCommandResult {
                request_id: 7,
                ok: Some(serde_json::json!({"preset": "focus"})),
                error: None,
            }),
        ),
        (
            "ui_command_result_error",
            Request::UiCommandResult(hutan_protocol::ui::UiCommandResult {
                request_id: 8,
                ok: None,
                error: Some(ErrorBody::new(ErrorCode::NotFound, "no such preset")),
            }),
        ),
        (
            "session_send_text",
            Request::SessionSendText(hutan_protocol::ui::SessionSendText {
                session_id: sid(),
                text: "cargo test".into(),
                enter: true,
            }),
        ),
        (
            "worktree_diff",
            Request::WorktreeDiff(WorktreeDiffRequest {
                path: "/work/repo-wt/feature-login".into(),
                base: Some("main".into()),
                include_uncommitted: false,
            }),
        ),
        (
            "worktree_diff_minimal",
            Request::WorktreeDiff(WorktreeDiffRequest {
                path: "/work/repo-wt/feature-login".into(),
                base: None,
                include_uncommitted: true,
            }),
        ),
        (
            "worktree_file_diff",
            Request::WorktreeFileDiff(WorktreeFileDiffRequest {
                path: "/work/repo-wt/feature-login".into(),
                file: "src/login.rs".into(),
                old_path: Some("src/signin.rs".into()),
                base: Some("main".into()),
                include_uncommitted: false,
            }),
        ),
        (
            "worktree_file_diff_minimal",
            Request::WorktreeFileDiff(WorktreeFileDiffRequest {
                path: "/work/repo-wt/feature-login".into(),
                file: "src/login.rs".into(),
                old_path: None,
                base: None,
                include_uncommitted: true,
            }),
        ),
        (
            "worktree_merge",
            Request::WorktreeMerge(WorktreeMerge {
                path: "/work/repo-wt/feature-login".into(),
                into: Some("main".into()),
                strategy: MergeStrategy::Squash,
                message: Some("Add login".into()),
            }),
        ),
        (
            "worktree_merge_minimal",
            Request::WorktreeMerge(WorktreeMerge {
                path: "/work/repo-wt/feature-login".into(),
                into: None,
                strategy: MergeStrategy::Merge,
                message: None,
            }),
        ),
        (
            "worktree_merge_rebase",
            Request::WorktreeMerge(WorktreeMerge {
                path: "/work/repo-wt/feature-login".into(),
                into: None,
                strategy: MergeStrategy::Rebase,
                message: None,
            }),
        ),
        (
            "question_list",
            Request::QuestionList(QuestionListRequest { session_id: None }),
        ),
        (
            "question_list_session",
            Request::QuestionList(QuestionListRequest {
                session_id: Some(sid()),
            }),
        ),
        (
            "question_answer_submit",
            Request::QuestionAnswer(QuestionAnswer {
                question_id: question(),
                response: QuestionResponse::Submit {
                    values: question_answer_values(),
                },
            }),
        ),
        (
            "question_answer_decline",
            Request::QuestionAnswer(QuestionAnswer {
                question_id: question(),
                response: QuestionResponse::Decline,
            }),
        ),
        (
            "question_answer_cancel",
            Request::QuestionAnswer(QuestionAnswer {
                question_id: question(),
                response: QuestionResponse::Cancel,
            }),
        ),
        (
            "task_review_accept",
            Request::TaskReview(TaskReview {
                id: tid(),
                decision: ReviewDecision::Accept,
                note: None,
            }),
        ),
        (
            "task_review_changes",
            Request::TaskReview(TaskReview {
                id: tid(),
                decision: ReviewDecision::RequestChanges,
                note: Some("Keep the old session cookie name.".into()),
            }),
        ),
        (
            "worktree_commit",
            Request::WorktreeCommit(WorktreeCommit {
                path: "/work/repo-wt/feature-login".into(),
                message: "Add login".into(),
                staged_only: false,
            }),
        ),
        (
            "session_recovery_ack",
            Request::SessionRecoveryAck(RecoveryAck {
                session_id: Some(sid()),
            }),
        ),
        (
            "session_recovery_ack_all",
            Request::SessionRecoveryAck(RecoveryAck { session_id: None }),
        ),
        ("daemon_settings_get", Request::DaemonSettingsGet(Empty {})),
        (
            "daemon_settings_set",
            Request::DaemonSettingsSet(hutan_protocol::daemon_settings::DaemonSettingSet {
                key: "notify.kinds".into(),
                value: Some(serde_json::json!(["approval_required", "task_failed"])),
            }),
        ),
        (
            "daemon_settings_set_reset",
            Request::DaemonSettingsSet(hutan_protocol::daemon_settings::DaemonSettingSet {
                key: "notify.enabled".into(),
                value: None,
            }),
        ),
        (
            "assistant_reset_global",
            Request::AssistantReset(AssistantReset {
                scope: AssistantScope::Global,
                workspace_id: None,
            }),
        ),
        (
            "assistant_reset_workspace",
            Request::AssistantReset(AssistantReset {
                scope: AssistantScope::Workspace,
                workspace_id: Some(ws()),
            }),
        ),
        (
            "history_sessions",
            Request::HistorySessions(HistorySessionsRequest {
                scope: HistoryScope::Project,
                workspace_id: Some(ws()),
                filter: "repo:hutan flaky".into(),
                sort: HistorySessionSort::Updated,
                sources: vec![HistorySource::Claude, HistorySource::Omp],
                limit: 250,
                refresh: true,
            }),
        ),
        (
            "session_close",
            Request::SessionClose(SessionClose {
                session_id: sid(),
                stop_running: false,
            }),
        ),
        (
            "session_close_stop_running",
            Request::SessionClose(SessionClose {
                session_id: sid(),
                stop_running: true,
            }),
        ),
        (
            "session_reopen",
            Request::SessionReopen(SessionRef { session_id: sid() }),
        ),
        (
            "control_grants_list",
            Request::ControlGrantsList(ControlGrantsList { workspace_id: None }),
        ),
        (
            "control_grants_list_workspace",
            Request::ControlGrantsList(ControlGrantsList {
                workspace_id: Some(ws()),
            }),
        ),
        (
            "control_grant_revoke",
            Request::ControlGrantRevoke(ControlGrantRevoke { id: 7 }),
        ),
        ("memory_list", Request::MemoryList(MemoryList::default())),
        (
            "memory_list_filtered",
            Request::MemoryList(MemoryList {
                scope: Some(MemoryScope::Workspace),
                workspace_id: Some(ws()),
                category: Some(MemoryCategory::Note),
                include_archived: true,
                query: Some("branch".into()),
            }),
        ),
        (
            "memory_history",
            Request::MemoryHistory(MemoryHistoryRequest {
                entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
            }),
        ),
        (
            "memory_write",
            Request::MemoryWrite(MemoryWrite {
                operation_id: "op-1".into(),
                scope: MemoryScope::Global,
                workspace_id: None,
                category: MemoryCategory::Profile,
                body: "Prefers short answers.".into(),
            }),
        ),
        (
            "memory_write_workspace",
            Request::MemoryWrite(MemoryWrite {
                operation_id: "op-2".into(),
                scope: MemoryScope::Workspace,
                workspace_id: Some(ws()),
                category: MemoryCategory::Note,
                body: "Releases are cut from `trunk`.".into(),
            }),
        ),
        (
            "memory_update",
            Request::MemoryUpdate(MemoryUpdate {
                operation_id: "op-3".into(),
                entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
                expected_revision: 2,
                body: "Prefers very short answers.".into(),
            }),
        ),
        (
            "memory_archive",
            Request::MemoryArchive(MemoryTransition {
                operation_id: "op-4".into(),
                entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
                expected_revision: 3,
            }),
        ),
        (
            "memory_restore",
            Request::MemoryRestore(MemoryTransition {
                operation_id: "op-5".into(),
                entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
                expected_revision: 4,
            }),
        ),
        (
            "memory_propose_create",
            Request::MemoryPropose(MemoryPropose {
                operation_id: "op-6".into(),
                op: MemoryOp::Create,
                scope: Some(MemoryScope::Global),
                workspace_id: None,
                category: Some(MemoryCategory::Fact),
                entry_id: None,
                expected_revision: None,
                body: Some("The staging cluster is called `kite`.".into()),
            }),
        ),
        (
            "memory_propose_update",
            Request::MemoryPropose(MemoryPropose {
                operation_id: "op-7".into(),
                op: MemoryOp::Update,
                scope: None,
                workspace_id: None,
                category: None,
                entry_id: Some("01K7F3B5N2Q9W8E4R6T1Y0V3KP".into()),
                expected_revision: Some(2),
                body: Some("Uses jj for version control; main branch is `trunk`.".into()),
            }),
        ),
        (
            "memory_proposals_list",
            Request::MemoryProposalsList(MemoryProposalsList::default()),
        ),
        (
            "memory_proposals_resolve_approve",
            Request::MemoryProposalsResolve(MemoryProposalResolve {
                proposal_id: approval(),
                decision: ProposalDecision::Approve,
                edited_body: None,
            }),
        ),
        (
            "memory_proposals_resolve_edit",
            Request::MemoryProposalsResolve(MemoryProposalResolve {
                proposal_id: approval(),
                decision: ProposalDecision::Edit,
                edited_body: Some("Uses jj; the main branch is `trunk`.".into()),
            }),
        ),
        (
            "fs_read",
            Request::FsRead(FsRead {
                path: "/repo/src/main.rs".into(),
                offset: None,
                max_bytes: None,
                picked: false,
            }),
        ),
        (
            "fs_read_chunk_picked",
            Request::FsRead(FsRead {
                path: "/Users/dev/Desktop/notes.md".into(),
                offset: Some(1_048_576),
                max_bytes: Some(65_536),
                picked: true,
            }),
        ),
        (
            "fs_write_begin",
            Request::FsWriteBegin(FsWriteBegin {
                path: "/repo/src/main.rs".into(),
                expect: Some(file_version()),
                create: false,
                size_hint: Some(6_291_456),
                picked: false,
            }),
        ),
        (
            "fs_write_begin_create",
            Request::FsWriteBegin(FsWriteBegin {
                path: "/repo/src/new.rs".into(),
                expect: None,
                create: true,
                size_hint: None,
                picked: false,
            }),
        ),
        (
            "fs_write_chunk",
            Request::FsWriteChunk(FsWriteChunk {
                upload_id: "01K7F3B5N2Q9W8E4R6T1Y0V3UP".into(),
                seq: 0,
                text: "fn main() {}\n".into(),
            }),
        ),
        (
            "fs_write_commit",
            Request::FsWriteCommit(FsWriteCommit {
                upload_id: "01K7F3B5N2Q9W8E4R6T1Y0V3UP".into(),
                total_bytes: 13,
            }),
        ),
        (
            "fs_write_abort",
            Request::FsWriteAbort(FsWriteAbort {
                upload_id: "01K7F3B5N2Q9W8E4R6T1Y0V3UP".into(),
            }),
        ),
        (
            "fs_stat",
            Request::FsStat(FsStat {
                path: "/repo/Cargo.toml".into(),
                picked: false,
            }),
        ),
        (
            "fs_list",
            Request::FsList(FsList {
                dir: "/repo".into(),
                ignore: true,
                limit: None,
                picked: false,
            }),
        ),
        (
            "fs_list_plain_limited",
            Request::FsList(FsList {
                dir: "/repo/src".into(),
                ignore: false,
                limit: Some(100),
                picked: false,
            }),
        ),
        (
            "fs_find",
            Request::FsFind(FsFind {
                root: "/repo".into(),
                query: "rtr".into(),
                limit: Some(20),
            }),
        ),
        (
            "fs_search",
            Request::FsSearch(FsSearch {
                search_id: "s1".into(),
                root: "/repo".into(),
                pattern: "fn main".into(),
                regex: false,
                case_sensitive: None,
                globs: vec![],
                limit: None,
            }),
        ),
        (
            "fs_search_regex_globs",
            Request::FsSearch(FsSearch {
                search_id: "s2".into(),
                root: "/repo".into(),
                pattern: r"TODO\(\w+\)".into(),
                regex: true,
                case_sensitive: Some(true),
                globs: vec!["*.rs".into(), "!target/**".into()],
                limit: Some(1000),
            }),
        ),
        (
            "fs_search_cancel",
            Request::FsSearchCancel(FsSearchCancel {
                search_id: "s2".into(),
            }),
        ),
        (
            "fs_watch",
            Request::FsWatch(FsWatch {
                paths: vec![
                    FsWatchPath {
                        path: "/repo/src/main.rs".into(),
                        recursive: false,
                    },
                    FsWatchPath {
                        path: "/repo/docs".into(),
                        recursive: true,
                    },
                ],
                picked: false,
            }),
        ),
        (
            "fs_unwatch",
            Request::FsUnwatch(FsUnwatch {
                paths: vec!["/repo/src/main.rs".into()],
            }),
        ),
        (
            "notify_mute",
            Request::NotifyMute(NotifyMute {
                session_id: sid(),
                muted: true,
            }),
        ),
        (
            "notify_unmute",
            Request::NotifyMute(NotifyMute {
                session_id: sid(),
                muted: false,
            }),
        ),
        (
            "chat_retry",
            Request::ChatRetry(hutan_protocol::chat::ChatRetry { session_id: sid() }),
        ),
        (
            "worktree_finish",
            Request::WorktreeFinish(WorktreeFinish {
                checkout: "/work/repo-wt/feature-login".into(),
                message: "Add login".into(),
                push: Some(true),
                into: Some("main".into()),
                strategy: MergeStrategy::Squash,
                staged_only: false,
            }),
        ),
        (
            "worktree_finish_main_checkout",
            Request::WorktreeFinish(WorktreeFinish {
                checkout: "/work/repo".into(),
                message: "Fix the login form".into(),
                push: None,
                into: None,
                strategy: MergeStrategy::Merge,
                staged_only: false,
            }),
        ),
        (
            "chat_interrupt_keep_queue",
            Request::ChatInterrupt(ChatInterrupt {
                session_id: sid(),
                keep_queue: true,
            }),
        ),
        (
            "chat_queue_add",
            Request::ChatQueueAdd(ChatQueueAdd {
                session_id: sid(),
                text: "Then run the tests".into(),
            }),
        ),
        (
            "chat_queue_edit",
            Request::ChatQueueEdit(ChatQueueEdit {
                session_id: sid(),
                item_id: 2,
                text: "Then run only the unit tests".into(),
            }),
        ),
        (
            "chat_queue_remove",
            Request::ChatQueueRemove(ChatQueueItemRef {
                session_id: sid(),
                item_id: 2,
            }),
        ),
        (
            "chat_queue_clear",
            Request::ChatQueueClear(SessionRef { session_id: sid() }),
        ),
        (
            "chat_queue_list",
            Request::ChatQueueList(SessionRef { session_id: sid() }),
        ),
        (
            "chat_send_now_text",
            Request::ChatSendNow(ChatSendNow {
                session_id: sid(),
                text: Some("Stop editing the lockfile".into()),
                item_id: None,
            }),
        ),
        (
            "chat_send_now_item",
            Request::ChatSendNow(ChatSendNow {
                session_id: sid(),
                text: None,
                item_id: Some(2),
            }),
        ),
        (
            "checkpoint_list_session",
            Request::CheckpointList(CheckpointListRequest {
                session_id: Some(sid()),
                ..CheckpointListRequest::default()
            }),
        ),
        (
            "checkpoint_list_workspace_page",
            Request::CheckpointList(CheckpointListRequest {
                session_id: None,
                workspace_id: Some(ws()),
                limit: Some(20),
                before: Some(checkpoint_id(2)),
            }),
        ),
        (
            "checkpoint_diff",
            Request::CheckpointDiff(CheckpointDiffRequest {
                checkpoint_id: checkpoint_id(1),
                against: DiffAgainst::End,
                path: None,
                stat: false,
            }),
        ),
        (
            "checkpoint_diff_now_path_stat",
            Request::CheckpointDiff(CheckpointDiffRequest {
                checkpoint_id: checkpoint_id(1),
                against: DiffAgainst::Now,
                path: Some("src".into()),
                stat: true,
            }),
        ),
        (
            "checkpoint_diff_through",
            Request::CheckpointDiff(CheckpointDiffRequest {
                checkpoint_id: checkpoint_id(1),
                against: DiffAgainst::Checkpoint(checkpoint_id(2)),
                path: None,
                stat: false,
            }),
        ),
        (
            "checkpoint_remove_checkpoint",
            Request::CheckpointRemove(CheckpointRemove {
                checkpoint_id: Some(checkpoint_id(1)),
                session_id: None,
            }),
        ),
        (
            "checkpoint_remove_session",
            Request::CheckpointRemove(CheckpointRemove {
                checkpoint_id: None,
                session_id: Some(sid()),
            }),
        ),
        (
            "checkpoint_preview",
            Request::CheckpointPreview(CheckpointPreviewRequest {
                checkpoint_id: checkpoint_id(1),
                through: None,
                paths: vec![],
            }),
        ),
        (
            "checkpoint_preview_through_paths",
            Request::CheckpointPreview(CheckpointPreviewRequest {
                checkpoint_id: checkpoint_id(1),
                through: Some(checkpoint_id(2)),
                paths: vec!["src".into(), "README.md".into()],
            }),
        ),
        (
            "checkpoint_restore",
            Request::CheckpointRestore(CheckpointRestore {
                checkpoint_id: checkpoint_id(1),
                through: None,
                paths: vec![],
                accept_conflicts: vec![],
                plan_digest: plan_digest().into(),
            }),
        ),
        (
            "checkpoint_restore_accept",
            Request::CheckpointRestore(CheckpointRestore {
                checkpoint_id: checkpoint_id(1),
                through: Some(checkpoint_id(2)),
                paths: vec!["src".into()],
                accept_conflicts: vec!["src/lib.rs".into()],
                plan_digest: plan_digest().into(),
            }),
        ),
        (
            "doc_open",
            Request::DocOpen(DocOpen {
                path: "/Users/me/repo/src/main.rs".into(),
                language: None,
                version: 1,
                text: "fn main() {}\n".into(),
            }),
        ),
        (
            "doc_open_language",
            Request::DocOpen(DocOpen {
                path: "/Users/me/repo/web/app.tsx".into(),
                language: Some("typescriptreact".into()),
                version: 0,
                text: String::new(),
            }),
        ),
        (
            "doc_change_edits",
            Request::DocChange(DocChange {
                doc_id: "d1".into(),
                version: 2,
                edits: vec![DocEdit {
                    start: LspPosition {
                        line: 0,
                        character: 11,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 11,
                    },
                    text: "println!()".into(),
                }],
                text: None,
            }),
        ),
        (
            "doc_change_full",
            Request::DocChange(DocChange {
                doc_id: "d1".into(),
                version: 3,
                edits: vec![],
                text: Some("fn main() {}\n".into()),
            }),
        ),
        (
            "doc_save",
            Request::DocSave(DocRef {
                doc_id: "d1".into(),
            }),
        ),
        (
            "doc_close",
            Request::DocClose(DocRef {
                doc_id: "d1".into(),
            }),
        ),
        (
            "lsp_status",
            Request::LspStatus(LspStatusRequest::default()),
        ),
        (
            "lsp_status_workspace",
            Request::LspStatus(LspStatusRequest {
                workspace_id: Some(ws()),
            }),
        ),
        (
            "lsp_diagnostics",
            Request::LspDiagnostics(LspDiagnosticsRequest::default()),
        ),
        (
            "lsp_diagnostics_path",
            Request::LspDiagnostics(LspDiagnosticsRequest {
                path: Some("/Users/me/repo/src/main.rs".into()),
                workspace_id: None,
                min_severity: Some(LspSeverity::Warning),
            }),
        ),
        (
            "lsp_diagnostics_workspace",
            Request::LspDiagnostics(LspDiagnosticsRequest {
                path: None,
                workspace_id: Some(ws()),
                min_severity: None,
            }),
        ),
        (
            "lsp_hover",
            Request::LspHover(LspPositionRequest {
                path: "/Users/me/repo/src/main.rs".into(),
                position: LspPosition {
                    line: 3,
                    character: 9,
                },
            }),
        ),
        (
            "lsp_definition",
            Request::LspDefinition(LspPositionRequest {
                path: "/Users/me/repo/src/main.rs".into(),
                position: LspPosition {
                    line: 3,
                    character: 9,
                },
            }),
        ),
        (
            "lsp_restart",
            Request::LspRestart(LspServerRef {
                server_id: "rust-analyzer:/Users/me/repo".into(),
            }),
        ),
        (
            "lsp_stop",
            Request::LspStop(LspServerRef {
                server_id: "rust-analyzer:/Users/me/repo".into(),
            }),
        ),
        (
            "lsp_trust",
            Request::LspTrust(LspTrustRequest {
                folder: "/Users/me/repo".into(),
                decision: LspTrust::Trusted,
            }),
        ),
        (
            "lsp_trust_ask",
            Request::LspTrust(LspTrustRequest {
                folder: "/Users/me/repo".into(),
                decision: LspTrust::Ask,
            }),
        ),
        (
            "lsp_completion",
            Request::LspCompletion(LspCompletionRequest {
                path: "/Users/me/repo/src/main.rs".into(),
                position: LspPosition {
                    line: 3,
                    character: 8,
                },
                trigger: Some(".".into()),
                incomplete: false,
            }),
        ),
        (
            "lsp_completion_resolve",
            Request::LspCompletionResolve(LspCompletionResolve {
                completion_id: 7,
                index: 0,
            }),
        ),
        (
            "lsp_signature_help",
            Request::LspSignatureHelp(LspSignatureHelpRequest {
                path: "/Users/me/repo/src/main.rs".into(),
                position: LspPosition {
                    line: 3,
                    character: 14,
                },
                trigger: Some("(".into()),
                retrigger: false,
            }),
        ),
        (
            "daemon_shutdown_restart",
            Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget: false,
                restart: true,
            }),
        ),
        (
            "terminal_search",
            Request::TerminalSearch(TerminalSearch {
                terminal_id: TID,
                query: "error".into(),
                case: false,
                regex: false,
                limit: 200,
                from: None,
                block_id: None,
                text: false,
            }),
        ),
        (
            "terminal_search_regex_block",
            Request::TerminalSearch(TerminalSearch {
                terminal_id: TID,
                query: r"FAIL(ED)?\b".into(),
                case: true,
                regex: true,
                limit: 1000,
                from: Some(1480),
                block_id: Some(12),
                text: true,
            }),
        ),
        (
            "project_git_init",
            Request::ProjectGitInit(ProjectGitInit { project_id: ws() }),
        ),
        (
            "usage_summary",
            Request::UsageSummary(UsageSummaryRequest::default()),
        ),
        (
            "usage_summary_refresh",
            Request::UsageSummary(UsageSummaryRequest { refresh: true }),
        ),
        (
            "memory_import_path",
            Request::MemoryImport(MemoryImport {
                scope: MemoryScope::Global,
                workspace_id: None,
                path: Some("/Users/me/notes/memory.md".into()),
                text: None,
                category: None,
            }),
        ),
        (
            "memory_import_text",
            Request::MemoryImport(MemoryImport {
                scope: MemoryScope::Workspace,
                workspace_id: Some(ws()),
                path: None,
                text: Some("# Facts\n\n<!-- mawas:new -->\nStaging is stage.example.com.\n".into()),
                category: Some(MemoryCategory::Fact),
            }),
        ),
        (
            "memory_files_status",
            Request::MemoryFilesStatus(MemoryFilesStatusRequest {
                scope: MemoryScope::Workspace,
                workspace_id: Some(ws()),
            }),
        ),
        (
            "git_log",
            Request::GitLog(GitLog {
                path: "/Users/me/proj/hutan".into(),
                rev: None,
                limit: 50,
                before: None,
            }),
        ),
        (
            "git_log_file_page",
            Request::GitLog(GitLog {
                path: "/Users/me/proj/hutan/src/lib.rs".into(),
                rev: Some("main".into()),
                limit: 200,
                before: Some("9d8c7b6a5f4e3d2c1b0a99887766554433221100".into()),
            }),
        ),
        (
            "git_blame",
            Request::GitBlame(GitBlame {
                path: "/Users/me/proj/hutan/src/lib.rs".into(),
                rev: None,
                contents: None,
            }),
        ),
        (
            "git_blame_rev",
            Request::GitBlame(GitBlame {
                path: "/Users/me/proj/hutan/src/lib.rs".into(),
                rev: Some("v0.2.0".into()),
                contents: Some("pub fn hi() {}\n".into()),
            }),
        ),
        (
            "git_show",
            Request::GitShow(GitShow {
                path: "/Users/me/proj/hutan".into(),
                rev: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                old_path: None,
                patch: true,
            }),
        ),
        (
            "git_show_file_renamed",
            Request::GitShow(GitShow {
                path: "/Users/me/proj/hutan/src/new.rs".into(),
                rev: "HEAD~3".into(),
                old_path: Some("/Users/me/proj/hutan/src/old.rs".into()),
                patch: false,
            }),
        ),
        (
            "git_file",
            Request::GitFile(GitFile {
                path: "/Users/me/proj/hutan/src/lib.rs".into(),
                rev: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
            }),
        ),
        ("hooks_list", Request::HooksList(Empty {})),
        (
            "hooks_log",
            Request::HooksLog(HooksLogRequest {
                hook: Some(1),
                limit: Some(20),
            }),
        ),
        (
            "hooks_log_all",
            Request::HooksLog(HooksLogRequest::default()),
        ),
        ("hooks_test", Request::HooksTest(HooksTest { hook: 1 })),
        (
            "worktree_create_no_setup",
            Request::WorktreeCreate(WorktreeCreate {
                workspace_id: ws(),
                repo: None,
                branch: "feature/login".into(),
                base: None,
                path: None,
                no_setup: true,
            }),
        ),
        (
            "worktree_setup",
            Request::WorktreeSetup(WorktreeSetupRequest {
                path: "/work/repo-wt/feature-login".into(),
            }),
        ),
        (
            "worktree_setup_trust",
            Request::WorktreeSetupTrust(WorktreeSetupTrust {
                path: "/work/repo-wt/feature-login".into(),
                hash: "5f2b8c0a9d7e4a31b6c8e0f2a4d6b8c0e2f4a6b8c0d2e4f6a8b0c2d4e6f8a0b2".into(),
            }),
        ),
        (
            "worktree_cleanup",
            Request::WorktreeCleanup(WorktreeCleanupRequest {
                workspace_id: Some(ws()),
                mode: Some(WorktreeCleanup::Idle),
                idle_days: Some(30),
            }),
        ),
        (
            "worktree_cleanup_default",
            Request::WorktreeCleanup(WorktreeCleanupRequest::default()),
        ),
        (
            "worktree_prune",
            Request::WorktreePrune(WorktreePrune {
                workspace_id: Some(ws()),
                mode: Some(WorktreeCleanup::Merged),
                idle_days: None,
                paths: vec!["/work/repo-wt/feature-login".into()],
            }),
        ),
        (
            "worktree_commit_staged_only",
            Request::WorktreeCommit(WorktreeCommit {
                path: "/work/repo-wt/feature-login".into(),
                message: "Add login form".into(),
                staged_only: true,
            }),
        ),
        (
            "worktree_hunks",
            Request::WorktreeHunks(WorktreeHunksRequest {
                path: "/work/repo-wt/feature-login".into(),
                files: vec!["src/login.rs".into()],
            }),
        ),
        (
            "worktree_hunks_all",
            Request::WorktreeHunks(WorktreeHunksRequest {
                path: "/work/repo-wt/feature-login".into(),
                files: vec![],
            }),
        ),
        (
            "worktree_stage_paths",
            Request::WorktreeStage(WorktreeIndexChange {
                checkout: "/work/repo-wt/feature-login".into(),
                paths: vec!["src/login.rs".into(), "README.md".into()],
                hunks: vec![],
                all: false,
            }),
        ),
        (
            "worktree_stage_hunks",
            Request::WorktreeStage(WorktreeIndexChange {
                checkout: "/work/repo-wt/feature-login".into(),
                paths: vec![],
                hunks: vec![
                    HunkRef {
                        path: "src/login.rs".into(),
                        hunk_id: "5f0c3a9d1e7b2a46".into(),
                        lines: vec![],
                    },
                    HunkRef {
                        path: "src/login.rs".into(),
                        hunk_id: "0a1b2c3d4e5f6071".into(),
                        lines: vec![2, 3],
                    },
                ],
                all: false,
            }),
        ),
        (
            "worktree_stage_all",
            Request::WorktreeStage(WorktreeIndexChange {
                checkout: "/work/repo".into(),
                paths: vec![],
                hunks: vec![],
                all: true,
            }),
        ),
        (
            "worktree_unstage_hunk",
            Request::WorktreeUnstage(WorktreeIndexChange {
                checkout: "/work/repo-wt/feature-login".into(),
                paths: vec![],
                hunks: vec![HunkRef {
                    path: "src/login.rs".into(),
                    hunk_id: "9d8c7b6a5f4e3d2c".into(),
                    lines: vec![],
                }],
                all: false,
            }),
        ),
        (
            "worktree_discard_paths",
            Request::WorktreeDiscard(WorktreeDiscard {
                checkout: "/work/repo-wt/feature-login".into(),
                paths: vec!["notes.txt".into()],
                hunks: vec![],
                staged: true,
            }),
        ),
        (
            "worktree_discard_hunk",
            Request::WorktreeDiscard(WorktreeDiscard {
                checkout: "/work/repo-wt/feature-login".into(),
                paths: vec![],
                hunks: vec![HunkRef {
                    path: "src/login.rs".into(),
                    hunk_id: "5f0c3a9d1e7b2a46".into(),
                    lines: vec![],
                }],
                staged: false,
            }),
        ),
        (
            "worktree_finish_staged_only",
            Request::WorktreeFinish(WorktreeFinish {
                checkout: "/work/repo".into(),
                message: "Fix the login form".into(),
                push: Some(false),
                into: None,
                strategy: MergeStrategy::Merge,
                staged_only: true,
            }),
        ),
        (
            "fs_create_file",
            Request::FsCreate(FsCreate {
                path: "/repo/src/new.rs".into(),
                kind: FsCreateKind::File,
                parents: false,
            }),
        ),
        (
            "fs_create_dir_parents",
            Request::FsCreate(FsCreate {
                path: "/repo/docs/guides/intro".into(),
                kind: FsCreateKind::Dir,
                parents: true,
            }),
        ),
        (
            "fs_rename",
            Request::FsRename(FsRename {
                from: "/repo/src/old.rs".into(),
                to: "/repo/src/new.rs".into(),
            }),
        ),
        (
            "fs_move",
            Request::FsMove(FsMove {
                items: vec![
                    FsMoveItem {
                        from: "/repo/a.txt".into(),
                        to: "/repo/src/a.txt".into(),
                    },
                    FsMoveItem {
                        from: "/repo/b".into(),
                        to: "/repo/src/b".into(),
                    },
                ],
            }),
        ),
        (
            "fs_copy",
            Request::FsCopy(FsCopy {
                items: vec![
                    FsCopyItem {
                        from: "/repo/src".into(),
                        to: "/repo/src copy".into(),
                        picked_from: false,
                    },
                    FsCopyItem {
                        from: "/Users/me/Downloads/logo.png".into(),
                        to: "/repo/assets/logo.png".into(),
                        picked_from: true,
                    },
                ],
            }),
        ),
        (
            "fs_trash",
            Request::FsTrash(FsTrash {
                paths: vec!["/repo/old.log".into(), "/repo/build".into()],
            }),
        ),
        (
            "fs_restore",
            Request::FsRestore(FsRestore {
                ticket: "01K7F3B5N2Q9W8E4R6T1Y0V3TK".into(),
            }),
        ),
        (
            "chat_fork",
            Request::ChatFork(ChatFork {
                session_id: sid(),
                turn_id: None,
                mode: hutan_core::ForkMode::Exact,
                title: None,
                where_: ForkWhere::ThisFolder,
            }),
        ),
        (
            "chat_fork_turn_summary",
            Request::ChatFork(ChatFork {
                session_id: sid(),
                turn_id: Some(turn()),
                mode: hutan_core::ForkMode::Summary,
                title: Some("Try the other approach".into()),
                where_: ForkWhere::ThisFolder,
            }),
        ),
    ]
}

fn checkpoint_id(n: u8) -> CheckpointId {
    format!("01K7F3B5N2Q9W8E4R6T1Y0V3C{n}").parse().unwrap()
}

fn plan_digest() -> &'static str {
    "5b0c1e9f3a7d2e48c6f19a0b3d5e7f2146a8c9d0e1f2a3b4c5d6e7f8091a2b3c"
}

fn restore_entries() -> Vec<RestoreEntry> {
    vec![
        RestoreEntry {
            path: "assets/big.psd".into(),
            action: RestoreAction::Skip,
            state: RestoreState::Uncaptured,
            reason: Some("not captured: too large".into()),
            accept: None,
        },
        RestoreEntry {
            path: "src/lib.rs".into(),
            action: RestoreAction::Skip,
            state: RestoreState::After,
            reason: Some("changed since the turn".into()),
            accept: Some(RestoreAction::Write),
        },
        RestoreEntry {
            path: "src/main.rs".into(),
            action: RestoreAction::Write,
            state: RestoreState::Clean,
            reason: None,
            accept: None,
        },
        RestoreEntry {
            path: "src/new.rs".into(),
            action: RestoreAction::Delete,
            state: RestoreState::Clean,
            reason: None,
            accept: None,
        },
    ]
}

fn checkpoint_preview() -> CheckpointPreview {
    CheckpointPreview {
        plan_digest: plan_digest().into(),
        entries: restore_entries(),
        session_id: sid(),
        agent: Some("codex".into()),
        checkout: PathBuf::from("/Users/me/repo"),
        checkpoints: vec![checkpoint_id(1), checkpoint_id(2)],
        head_moved: Some(HeadMoved {
            from: Some("3f786850e387550fdab836ed7e6dc881de23001b".into()),
            to: Some("89e6c98d92887913cadf06b2adb97f26cde4849b".into()),
        }),
        shared_with: vec![SessionId::from_ulid(checkpoint_id(3).as_ulid())],
        busy: vec![],
    }
}

fn checkpoint_fixture() -> CheckpointInfo {
    CheckpointInfo {
        id: checkpoint_id(1),
        session_id: sid(),
        turn_id: Some(turn()),
        kind: CheckpointKind::Turn,
        state: CheckpointState::Ok,
        reason: None,
        created_ms: 1_760_000_000_000,
        checkout: Some(PathBuf::from("/Users/me/repo")),
        files_changed: 2,
        added: 1,
        removed: 1,
        bytes: 5321,
        skipped: vec![SkippedFile {
            path: ".env.local".into(),
            reason: SkipReason::SecretName,
            bytes: Some(120),
        }],
        restores: None,
        restores_through: None,
    }
}

/// A restore of turns 1 through 2 (protocol 0.40: `restores`, `restores_through`).
fn restore_checkpoint_fixture() -> CheckpointInfo {
    CheckpointInfo {
        id: checkpoint_id(3),
        turn_id: None,
        kind: CheckpointKind::Restore,
        files_changed: 2,
        added: 4,
        removed: 9,
        bytes: 0,
        skipped: vec![],
        restores: Some(checkpoint_id(1)),
        restores_through: Some(checkpoint_id(2)),
        ..checkpoint_fixture()
    }
}

fn chat_queue_fixture(held: Option<QueueHold>, steer: bool) -> ChatQueue {
    ChatQueue {
        session_id: sid(),
        items: vec![
            QueuedMessage {
                id: 1,
                text: "Then run the tests".into(),
                queued_at_ms: 1_760_000_000_000,
            },
            QueuedMessage {
                id: 2,
                text: "And update the changelog".into(),
                queued_at_ms: 1_760_000_001_000,
            },
        ],
        held,
        steer,
    }
}

fn events() -> Vec<(&'static str, Event)> {
    vec![
        (
            "workspace_updated",
            Event::WorkspaceUpdated {
                workspace: workspace_info(),
            },
        ),
        (
            "workspace_closed",
            Event::WorkspaceClosed { workspace_id: ws() },
        ),
        (
            "session_updated",
            Event::SessionUpdated {
                session: session_info(),
            },
        ),
        (
            "session_exited",
            Event::SessionExited {
                session_id: sid(),
                code: Some(0),
                signal: None,
            },
        ),
        ("terminal_bell", Event::TerminalBell { terminal_id: TID }),
        (
            "terminal_title",
            Event::TerminalTitle {
                terminal_id: TID,
                title: Some("vim".into()),
            },
        ),
        (
            "terminal_notification",
            Event::TerminalNotification {
                terminal_id: TID,
                title: Some("build".into()),
                body: "done".into(),
            },
        ),
        (
            "session_updated_restored",
            Event::SessionUpdated {
                session: SessionInfo {
                    state: SessionState::Stopped,
                    terminal_id: None,
                    restored: true,
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_agent",
            Event::SessionUpdated {
                session: SessionInfo {
                    agent: Some(AgentReport {
                        source: "omp-extension".into(),
                        agent: "omp".into(),
                        state: AgentState::Working,
                        message: None,
                        agent_session_id: Some("0f4c7d52".into()),
                        updated_at_ms: 1_791_894_170_000,
                        blocked_sub: None,
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "terminal_block",
            Event::TerminalBlock {
                terminal_id: TID,
                block: finished_block(),
            },
        ),
        (
            "terminal_block_prompt",
            Event::TerminalBlock {
                terminal_id: TID,
                block: prompt_block(),
            },
        ),
        (
            "workspace_updated_home",
            Event::WorkspaceUpdated {
                workspace: home_info(),
            },
        ),
        (
            "session_updated_chat",
            Event::SessionUpdated {
                session: chat_session_info(),
            },
        ),
        (
            "chat_event_message_delta",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    41,
                    AgentEventKind::MessageDelta { text: "Hel".into() },
                )),
            },
        ),
        (
            "chat_event_message_end",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        44,
                        AgentEventKind::MessageEnd {
                            role: MessageRole::Assistant,
                            phase: Some("final_answer".into()),
                            text: "Hello".into(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
        (
            "chat_event_tool_start",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    45,
                    AgentEventKind::ToolStart {
                        tool: ToolKind::Command,
                        name: "shell".into(),
                        title: "cargo test".into(),
                        input: Some(serde_json::json!({"command": "cargo test", "cwd": "/repo"})),
                    },
                )),
            },
        ),
        (
            "chat_event_tool_end",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    47,
                    AgentEventKind::ToolEnd {
                        tool: ToolKind::Command,
                        status: ToolStatus::Completed,
                        output: Some("ok\n".into()),
                        truncated: false,
                        exit_code: Some(0),
                        duration_ms: Some(1250),
                    },
                )),
            },
        ),
        (
            "chat_event_file_diff",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    48,
                    AgentEventKind::FileDiff {
                        changes: vec![FileChange {
                            path: "/repo/hello.txt".into(),
                            kind: FileChangeKind::Add,
                            move_path: None,
                            patch: Some("hi\n".into()),
                            before: None,
                            after: None,
                        }],
                        patch: None,
                    },
                )),
            },
        ),
        (
            "chat_event_permission_requested",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    49,
                    AgentEventKind::PermissionRequested {
                        approval_id: approval(),
                        kind: PermissionKind::Command,
                        title: "touch hi.txt".into(),
                        detail: None,
                        options: permission_options(),
                    },
                )),
            },
        ),
        (
            "chat_event_permission_resolved",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    50,
                    AgentEventKind::PermissionResolved {
                        approval_id: approval(),
                        outcome: PermissionOutcome::Selected {
                            option_id: "accept".into(),
                        },
                        decided_by: Some(DecidedBy {
                            client_kind: "desktop".into(),
                            client_name: None,
                        }),
                    },
                )),
            },
        ),
        (
            "chat_event_usage",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    51,
                    AgentEventKind::Usage {
                        total: Some(TokenCounts {
                            total_tokens: Some(41_338),
                            input_tokens: Some(41_285),
                            output_tokens: Some(53),
                            ..TokenCounts::default()
                        }),
                        last: None,
                        context_window: Some(258_400),
                    },
                )),
            },
        ),
        (
            "chat_event_opaque",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new({
                    let mut event = agent_event(
                        52,
                        AgentEventKind::Opaque {
                            method: "future/thing".into(),
                        },
                    );
                    event.payload = Some(serde_json::json!({"x": 1}));
                    event
                }),
            },
        ),
        (
            "chat_event_turn_completed",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    53,
                    AgentEventKind::TurnCompleted {
                        duration_ms: Some(6784),
                    },
                )),
            },
        ),
        (
            "approval_required",
            Event::ApprovalRequired {
                approval: Box::new(approval_info()),
            },
        ),
        (
            "approval_resolved_selected",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        ),
        (
            "approval_resolved_expired",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Expired,
                decided_by: None,
            },
        ),
        (
            "agent_tree_updated",
            Event::AgentTreeUpdated {
                session_id: sid(),
                nodes_changed: vec![sub_node(), main_node()],
                nodes_removed: vec!["agent-old".into()],
            },
        ),
        (
            "session_updated_agent_blocked_sub",
            Event::SessionUpdated {
                session: SessionInfo {
                    agent: Some(AgentReport {
                        source: "claude-hooks".into(),
                        agent: "claude".into(),
                        state: AgentState::Blocked,
                        message: None,
                        agent_session_id: Some("5f3e1c1a".into()),
                        updated_at_ms: 1_791_894_170_000,
                        blocked_sub: Some("Explore".into()),
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_chat_direct",
            Event::SessionUpdated {
                session: direct_chat_session_info(),
            },
        ),
        (
            "chat_event_message_end_attachments",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new({
                    let mut draft = AgentEventDraft::new(AgentEventKind::MessageEnd {
                        role: MessageRole::User,
                        phase: None,
                        text: "why does this fail?\n\n[shell: cargo test · exit 101 · 3 lines]\nerror[E0308]".into(),
                    });
                    draft.turn_id = Some(turn());
                    draft.item_id = Some(item());
                    draft.payload = Some(serde_json::json!({
                        ATTACHMENTS_META_KEY: attachments_meta_fixture(),
                    }));
                    draft.into_event(sid(), 43, 1_791_894_170_043)
                }),
            },
        ),
        (
            "notify_intent_approval",
            Event::NotifyIntent {
                intent: approval_intent(),
            },
        ),
        (
            "notify_intent_agent_finished",
            Event::NotifyIntent {
                intent: open_intent(),
            },
        ),
        (
            "ui_open",
            Event::UiOpen {
                session_id: sid(),
                workspace_id: Some(ws()),
                at_ms: 1_791_894_180_000,
            },
        ),
        (
            "ui_open_without_workspace",
            Event::UiOpen {
                session_id: sid(),
                workspace_id: None,
                at_ms: 1_791_894_180_000,
            },
        ),
        (
            "worktree_changed",
            Event::WorktreeChanged { workspace_id: ws() },
        ),
        (
            "session_updated_placement_main",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Main,
                        root: "/work/repo".into(),
                        branch: Some("master".into()),
                        shared_with: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_placement_worktree_shared",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Worktree,
                        root: "/work/repo-wt/feature-login".into(),
                        branch: Some("feature/login".into()),
                        shared_with: vec![sid2()],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_placement_detached",
            Event::SessionUpdated {
                session: SessionInfo {
                    placement: Some(Placement {
                        kind: PlacementKind::Worktree,
                        root: "/work/repo-wt/bisect".into(),
                        branch: None,
                        shared_with: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "approval_resolved_selected_named",
            Event::ApprovalResolved {
                approval_id: approval(),
                session_id: sid(),
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "desktop".into(),
                    client_name: Some("window-2".into()),
                }),
            },
        ),
        (
            "chat_event_message_partial",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        52,
                        AgentEventKind::MessagePartial {
                            message_id: item(),
                            phase: Some("final_answer".into()),
                            text: "Hel".into(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
        (
            "chat_event_reasoning_partial",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(
                    agent_event(
                        53,
                        AgentEventKind::ReasoningPartial {
                            reasoning_id: item(),
                            summary: "Planning the test run".into(),
                            content: String::new(),
                        },
                    )
                    .replayed(),
                ),
            },
        ),
        (
            "notify_intent_task_done",
            Event::NotifyIntent {
                intent: NotificationIntent {
                    id: "01K7F3B5N2Q9W8E4R6T1Y0V3NV".into(),
                    kind: IntentKind::TaskDone,
                    session_id: sid(),
                    workspace_id: Some(ws()),
                    title: "Fix the login bug: task done".into(),
                    body: "Finished after 4m 12s (exited 0).".into(),
                    actions: vec![IntentAction {
                        id: "open".into(),
                        label: "Open".into(),
                    }],
                    created_at_ms: 1_791_894_190_000,
                    expires_at_ms: Some(1_791_897_790_000),
                },
            },
        ),
        (
            "task_updated",
            Event::TaskUpdated {
                task: running_task(),
            },
        ),
        (
            "task_updated_minimal",
            Event::TaskUpdated {
                task: minimal_task(),
            },
        ),
        ("task_removed", Event::TaskRemoved { id: tid() }),
        (
            "account_changed",
            Event::AccountChanged {
                harness: "omp".into(),
            },
        ),
        (
            "session_updated_chat_harness",
            Event::SessionUpdated {
                session: harness_chat_session_info(),
            },
        ),
        (
            "session_updated_chat_account",
            Event::SessionUpdated {
                session: SessionInfo {
                    chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                        agent: "claude".into(),
                        account: Some(ChatAccount {
                            name: "work".into(),
                            label: "Work".into(),
                        }),
                        ..chat
                    }),
                    ..harness_chat_session_info()
                },
            },
        ),
        (
            "session_updated_contained",
            Event::SessionUpdated {
                session: SessionInfo {
                    containment: Some(hutan_core::Containment {
                        kind: hutan_core::ContainKind::Seatbelt,
                        network: false,
                        write_roots: vec!["/work/repo".into(), "/work/repo.git".into()],
                        applied: true,
                        reason: None,
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_contain_refused",
            Event::SessionUpdated {
                session: SessionInfo {
                    state: SessionState::Failed {
                        reason: "Contain is not available: /usr/bin/sandbox-exec is missing".into(),
                    },
                    containment: Some(hutan_core::Containment {
                        kind: hutan_core::ContainKind::Seatbelt,
                        network: true,
                        write_roots: vec![],
                        applied: false,
                        reason: Some(
                            "Contain is not available: /usr/bin/sandbox-exec is missing".into(),
                        ),
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_chat_contained_omp",
            Event::SessionUpdated {
                session: SessionInfo {
                    chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                        safety: Some(ChatSafety {
                            preset: hutan_core::SafetyPreset::Contained,
                            enforced_by: hutan_core::SafetyEnforcer::Hutan,
                            network: Some(true),
                            note: Some("The boundary is Hutan's Seatbelt: reads stay open.".into()),
                        }),
                        ..chat
                    }),
                    containment: Some(hutan_core::Containment {
                        kind: hutan_core::ContainKind::Seatbelt,
                        network: true,
                        write_roots: vec!["/work/repo".into()],
                        applied: true,
                        reason: None,
                    }),
                    ..harness_chat_session_info()
                },
            },
        ),
        (
            "session_updated_chat_safety",
            Event::SessionUpdated {
                session: SessionInfo {
                    chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                        agent: "codex".into(),
                        safety: Some(ChatSafety {
                            preset: hutan_core::SafetyPreset::Contained,
                            enforced_by: hutan_core::SafetyEnforcer::Harness,
                            network: Some(false),
                            note: None,
                        }),
                        ..chat
                    }),
                    ..harness_chat_session_info()
                },
            },
        ),
        (
            "session_updated_chat_safety_unconfirmed",
            Event::SessionUpdated {
                session: SessionInfo {
                    chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                        agent: "claude".into(),
                        safety: Some(ChatSafety {
                            preset: hutan_core::SafetyPreset::Contained,
                            enforced_by: hutan_core::SafetyEnforcer::None,
                            network: None,
                            note: Some("Not applied: Claude Code reports mode default".into()),
                        }),
                        ..chat
                    }),
                    ..harness_chat_session_info()
                },
            },
        ),
        (
            "resources_updated",
            Event::ResourcesUpdated {
                sample: Box::new(resource_sample()),
            },
        ),
        (
            "workspace_updated_workspace",
            Event::WorkspaceUpdated {
                workspace: container_info(),
            },
        ),
        (
            "workspace_updated_folder_project",
            Event::WorkspaceUpdated {
                workspace: folder_project_info(),
            },
        ),
        (
            "session_updated_folder",
            Event::SessionUpdated {
                session: SessionInfo {
                    cwd: Some("/repo".into()),
                    folder: Some("/repo/crates/daemon".into()),
                    ..session_info()
                },
            },
        ),
        (
            "ui_command",
            Event::UiCommand {
                request_id: 7,
                window_id: "window-1".into(),
                action: "workbench::ApplyPreset".into(),
                args: serde_json::json!({"name": "focus"}),
            },
        ),
        (
            "ui_command_set_setting",
            Event::UiCommand {
                request_id: 8,
                window_id: "window-1".into(),
                action: "ui::SetSetting".into(),
                args: serde_json::json!({
                    "namespace": "terminal",
                    "key": "cursor.style",
                    "value": "bar",
                    "layer": "user"
                }),
            },
        ),
        (
            "approval_required_control",
            Event::ApprovalRequired {
                approval: Box::new(control_approval()),
            },
        ),
        (
            "session_updated_chat_question",
            Event::SessionUpdated {
                session: {
                    let mut session = chat_session_info();
                    if let Some(chat) = session.chat.as_mut() {
                        chat.pending_approvals = 0;
                        chat.pending_questions = 1;
                    }
                    session
                },
            },
        ),
        (
            "chat_event_question_requested",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    54,
                    AgentEventKind::QuestionRequested {
                        question_id: question(),
                        spec: question_spec(),
                    },
                )),
            },
        ),
        (
            "chat_event_question_resolved_answered",
            Event::ChatEvent {
                session_id: sid(),
                event: Box::new(agent_event(
                    55,
                    AgentEventKind::QuestionResolved {
                        question_id: question(),
                        outcome: QuestionOutcome::answered(&question_spec().fields, &{
                            let mut values = question_answer_values();
                            values.insert("token".into(), serde_json::json!("never journaled"));
                            values
                        }),
                        decided_by: Some(DecidedBy {
                            client_kind: "desktop".into(),
                            client_name: None,
                        }),
                    },
                )),
            },
        ),
        (
            "question_required",
            Event::QuestionRequired {
                question: Box::new(question_info()),
            },
        ),
        (
            "question_resolved_answered",
            Event::QuestionResolved {
                question_id: question(),
                session_id: sid(),
                outcome: QuestionOutcome::answered(
                    &question_spec().fields,
                    &question_answer_values(),
                ),
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        ),
        (
            "question_resolved_declined",
            Event::QuestionResolved {
                question_id: question(),
                session_id: sid(),
                outcome: QuestionOutcome::Declined,
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        ),
        (
            "question_resolved_expired",
            Event::QuestionResolved {
                question_id: question(),
                session_id: sid(),
                outcome: QuestionOutcome::Expired,
                decided_by: None,
            },
        ),
        (
            "notify_intent_question",
            Event::NotifyIntent {
                intent: NotificationIntent {
                    id: "01K7F3B5N2Q9W8E4R6T1Y0V3NW".into(),
                    kind: IntentKind::QuestionRequired,
                    session_id: sid(),
                    workspace_id: Some(ws()),
                    title: "Codex has a question".into(),
                    body: "Which database should the migration target?".into(),
                    actions: vec![IntentAction {
                        id: "open".into(),
                        label: "Open".into(),
                    }],
                    created_at_ms: 1_791_894_170_600,
                    expires_at_ms: None,
                },
            },
        ),
        (
            "task_updated_merged",
            Event::TaskUpdated {
                task: merged_task(),
            },
        ),
        (
            "session_updated_recovery_shell",
            Event::SessionUpdated {
                session: recovered(
                    RecoveryKind::ShellRespawned {
                        cwd: Some("/repo".into()),
                        was_running: Some("npm run dev".into()),
                    },
                    vec![],
                ),
            },
        ),
        (
            "session_updated_recovery_agent",
            Event::SessionUpdated {
                session: recovered(
                    RecoveryKind::AgentResumed {
                        resume_argv: vec!["claude".into(), "--resume".into(), "abc".into()],
                    },
                    vec![],
                ),
            },
        ),
        (
            "session_updated_recovery_chat_interrupted",
            Event::SessionUpdated {
                session: recovered(
                    RecoveryKind::ChatResumed {
                        thread_id: Some("thr_1".into()),
                        last_turn: LastTurn::Interrupted,
                    },
                    vec![
                        Lost::Turn,
                        Lost::Approvals { count: 2 },
                        Lost::Questions { count: 1 },
                        Lost::Task {
                            task_id: "01K7F3B5N2Q9W8E4R6T1Y0V3TK".parse().unwrap(),
                            title: "fix the build".into(),
                        },
                    ],
                ),
            },
        ),
        (
            "session_updated_recovery_not_restarted",
            Event::SessionUpdated {
                session: SessionInfo {
                    state: SessionState::Stopped,
                    terminal_id: None,
                    restored: true,
                    recovery: Some(Recovery {
                        kind: RecoveryKind::NotRestarted {
                            reason: "restore_sessions is \"stopped\"".into(),
                        },
                        previous_epoch: None,
                        daemon_stop: DaemonStop::Clean {
                            at_ms: 1_791_894_290_000,
                        },
                        restarted_at_ms: 1_791_894_300_000,
                        lost: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_recovery_ended_before",
            Event::SessionUpdated {
                session: SessionInfo {
                    state: SessionState::Exited {
                        code: Some(0),
                        signal: None,
                    },
                    terminal_id: None,
                    restored: true,
                    recovery: Some(Recovery {
                        kind: RecoveryKind::EndedBefore,
                        previous_epoch: Some(epoch()),
                        daemon_stop: DaemonStop::Unknown,
                        restarted_at_ms: 1_791_894_300_000,
                        lost: vec![],
                    }),
                    ..session_info()
                },
            },
        ),
        ("session_closed", Event::SessionClosed { session_id: sid() }),
        (
            "approval_required_control_grants",
            Event::ApprovalRequired {
                approval: Box::new(control_grants_approval()),
            },
        ),
        (
            "memory_changed_global",
            Event::MemoryChanged {
                scope: MemoryScope::Global,
                workspace_id: None,
                scope_revision: 4,
            },
        ),
        (
            "memory_changed_workspace",
            Event::MemoryChanged {
                scope: MemoryScope::Workspace,
                workspace_id: Some(ws()),
                scope_revision: 2,
            },
        ),
        (
            "approval_required_memory",
            Event::ApprovalRequired {
                approval: Box::new(memory_approval()),
            },
        ),
        (
            "fs_changed",
            Event::FsChanged {
                changes: vec![
                    FsChange {
                        path: "/repo/src/main.rs".into(),
                        kind: ChangeKind::Modified,
                        from: None,
                        version: Some(file_version()),
                        origin: Some(FsOrigin::Own {
                            write_id: "01K7F3B5N2Q9W8E4R6T1Y0V3WR".into(),
                        }),
                    },
                    FsChange {
                        path: "/repo/src/router.rs".into(),
                        kind: ChangeKind::Modified,
                        from: None,
                        version: Some(file_version()),
                        origin: Some(FsOrigin::Agent {
                            session_id: sid(),
                            tool_call_id: Some(item()),
                            confidence: AgentConfidence::ToolCall,
                        }),
                    },
                    FsChange {
                        path: "/repo/src/lib.rs".into(),
                        kind: ChangeKind::Created,
                        from: None,
                        version: Some(file_version()),
                        origin: Some(FsOrigin::Agent {
                            session_id: sid(),
                            tool_call_id: None,
                            confidence: AgentConfidence::Session,
                        }),
                    },
                    FsChange {
                        path: "/repo/src/old.rs".into(),
                        kind: ChangeKind::Removed,
                        from: None,
                        version: None,
                        origin: Some(FsOrigin::External),
                    },
                    FsChange {
                        path: "/repo/src/renamed.rs".into(),
                        kind: ChangeKind::Renamed,
                        from: Some("/repo/src/before.rs".into()),
                        version: Some(file_version()),
                        origin: None,
                    },
                ],
                overflow: false,
            },
        ),
        (
            "fs_changed_overflow",
            Event::FsChanged {
                changes: vec![FsChange {
                    path: "/repo/target/debug/hutand".into(),
                    kind: ChangeKind::Modified,
                    from: None,
                    version: None,
                    origin: Some(FsOrigin::External),
                }],
                overflow: true,
            },
        ),
        (
            "fs_generation",
            Event::FsGeneration {
                root: "/repo".into(),
                generation: 42,
            },
        ),
        (
            "fs_search_progress",
            Event::FsSearchProgress {
                search_id: "s2".into(),
                files_searched: 1_280,
                hits: 37,
            },
        ),
        (
            "chat_queue_added",
            Event::ChatQueueChanged {
                queue: chat_queue_fixture(None, true),
                change: QueueChange::Added { item_id: 2 },
            },
        ),
        (
            "chat_queue_delivered",
            Event::ChatQueueChanged {
                queue: ChatQueue {
                    items: vec![],
                    ..chat_queue_fixture(None, false)
                },
                change: QueueChange::Delivered {
                    item_id: 1,
                    turn_id: turn(),
                },
            },
        ),
        (
            "chat_queue_cleared_by_stop",
            Event::ChatQueueChanged {
                queue: ChatQueue {
                    items: vec![],
                    ..chat_queue_fixture(None, false)
                },
                change: QueueChange::Cleared {
                    count: 2,
                    stopped: true,
                },
            },
        ),
        (
            "chat_queue_held",
            Event::ChatQueueChanged {
                queue: chat_queue_fixture(Some(QueueHold::Stopped), false),
                change: QueueChange::Held,
            },
        ),
        (
            "checkpoint_created",
            Event::CheckpointCreated {
                checkpoint: Box::new(checkpoint_fixture()),
            },
        ),
        (
            "checkpoint_removed",
            Event::CheckpointRemoved {
                ids: vec![checkpoint_id(1), checkpoint_id(2)],
            },
        ),
        (
            "checkpoint_state_unsupported",
            Event::CheckpointStateChanged {
                session_id: sid(),
                state: CheckpointAvailability::Unsupported,
                reason: Some("/Users/me/notes is not in a git repository".into()),
            },
        ),
        (
            "checkpoint_state_on",
            Event::CheckpointStateChanged {
                session_id: sid(),
                state: CheckpointAvailability::On,
                reason: None,
            },
        ),
        (
            "fs_changed_restore",
            Event::FsChanged {
                changes: vec![
                    FsChange {
                        path: "/repo/src/main.rs".into(),
                        kind: ChangeKind::Modified,
                        from: None,
                        version: Some(file_version()),
                        origin: Some(FsOrigin::Restore {
                            checkpoint_id: checkpoint_id(3),
                        }),
                    },
                    FsChange {
                        path: "/repo/src/new.rs".into(),
                        kind: ChangeKind::Removed,
                        from: None,
                        version: None,
                        origin: Some(FsOrigin::Restore {
                            checkpoint_id: checkpoint_id(3),
                        }),
                    },
                ],
                overflow: false,
            },
        ),
        (
            "resources_updated_services",
            Event::ResourcesUpdated {
                sample: Box::new(ResourceSample {
                    services: vec![ServiceResources {
                        service_id: "lsp:rust-analyzer:/Users/me/repo".into(),
                        kind: "lsp".into(),
                        label: "rust-analyzer /Users/me/repo".into(),
                        processes: vec![
                            process_info(
                                51_204,
                                912,
                                "rust-analyzer",
                                "/opt/homebrew/bin/rust-analyzer",
                            ),
                            process_info(
                                51_230,
                                51_204,
                                "rust-analyzer-proc-macro-srv",
                                "rust-analyzer-proc-macro-srv",
                            ),
                        ],
                        totals: ResourceTotals {
                            processes: 2,
                            cpu_percent: 0.0,
                            rss_bytes: 8_388_608,
                        },
                        truncated: false,
                    }],
                    ..resource_sample()
                }),
            },
        ),
        (
            "lsp_state",
            Event::LspState {
                server: Box::new(lsp_server_info(LspServerState::Ready)),
            },
        ),
        (
            "lsp_state_not_installed",
            Event::LspState {
                server: Box::new(lsp_not_installed()),
            },
        ),
        (
            "lsp_state_untrusted",
            Event::LspState {
                server: Box::new(lsp_untrusted()),
            },
        ),
        (
            "lsp_diagnostics",
            Event::LspDiagnosticsChanged {
                path: "/Users/me/repo/src/main.rs".into(),
                version: Some(4),
                diagnostics: vec![lsp_diagnostic()],
                truncated: false,
            },
        ),
        (
            "lsp_diagnostics_cleared",
            Event::LspDiagnosticsChanged {
                path: "/Users/me/repo/src/main.rs".into(),
                version: None,
                diagnostics: vec![],
                truncated: false,
            },
        ),
        (
            "checkpoint_created_restore",
            Event::CheckpointCreated {
                checkpoint: Box::new(restore_checkpoint_fixture()),
            },
        ),
        (
            "daemon_stopping",
            Event::DaemonStopping {
                by: StopBy::MenuBar,
                restart: false,
            },
        ),
        (
            "daemon_stopping_restart",
            Event::DaemonStopping {
                by: StopBy::Cli,
                restart: true,
            },
        ),
        (
            "usage_changed",
            Event::UsageChanged {
                summary: Box::new(usage_summary_fixture()),
            },
        ),
        (
            "session_updated_terminal_agent_blocked",
            Event::SessionUpdated {
                session: SessionInfo {
                    terminal_agent: Some(hutan_protocol::messages::TerminalAgent {
                        app: "omp".into(),
                        state: hutan_protocol::messages::TerminalAgentState::Blocked,
                        kind: Some("permission".into()),
                        message: Some("Run the tests?".into()),
                        since_ms: 1_791_894_170_000,
                        title: Some("Fix the build".into()),
                        source: hutan_protocol::messages::TerminalAgentSource::Osc,
                    }),
                    ..session_info()
                },
            },
        ),
        (
            "session_updated_terminal_agent_process",
            Event::SessionUpdated {
                session: SessionInfo {
                    terminal_agent: Some(hutan_protocol::messages::TerminalAgent {
                        app: "claude".into(),
                        state: hutan_protocol::messages::TerminalAgentState::Unknown,
                        kind: None,
                        message: None,
                        since_ms: 1_791_894_165_000,
                        title: None,
                        source: hutan_protocol::messages::TerminalAgentSource::Process,
                    }),
                    ..session_info()
                },
            },
        ),
    ]
}

fn usage_summary_fixture() -> UsageSummary {
    let turns = |n: u64, input: u64, output: u64| UsageTotals {
        turns: n,
        input_tokens: input,
        cached_input_tokens: input / 2,
        output_tokens: output,
        reasoning_output_tokens: output / 4,
        total_tokens: input + output,
        ..UsageTotals::default()
    };
    UsageSummary {
        generated_at_ms: 1_791_894_200_000,
        day_start_ms: 1_791_849_600_000,
        week_start_ms: 1_791_590_400_000,
        harnesses: vec![
            HarnessUsage {
                harness: "codex".into(),
                limits: vec![
                    UsageLimit {
                        account: None,
                        account_label: None,
                        provider: "openai".into(),
                        id: "5h".into(),
                        label: "5-hour".into(),
                        used_percent: 72.0,
                        window_minutes: Some(300),
                        resets_at_ms: Some(1_791_904_800_000),
                        plan: Some("plus".into()),
                        observed_at_ms: 1_791_894_100_000,
                    },
                    UsageLimit {
                        account: Some("work".into()),
                        account_label: Some("Work".into()),
                        provider: "openai".into(),
                        id: "7d".into(),
                        label: "weekly".into(),
                        used_percent: 31.0,
                        window_minutes: Some(10_080),
                        resets_at_ms: None,
                        plan: None,
                        observed_at_ms: 1_791_894_100_000,
                    },
                ],
                today: turns(4, 120_000, 8_000),
                week: turns(19, 900_000, 60_000),
            },
            HarnessUsage {
                harness: "omp".into(),
                limits: vec![],
                today: UsageTotals {
                    cost_usd: Some(0.42),
                    cost_is_estimate: true,
                    ..turns(2, 50_000, 3_000)
                },
                week: UsageTotals {
                    cost_usd: Some(3.1),
                    cost_is_estimate: true,
                    ..turns(9, 400_000, 30_000)
                },
            },
        ],
        providers: vec![ProviderUsage {
            provider: "anthropic".into(),
            today: turns(2, 50_000, 3_000),
            week: turns(9, 400_000, 30_000),
        }],
    }
}

fn approval_intent() -> NotificationIntent {
    NotificationIntent {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3NT".into(),
        kind: IntentKind::ApprovalRequired,
        session_id: sid(),
        workspace_id: Some(ws()),
        title: "Run `touch hi.txt`?".into(),
        body: "Codex wants to run a command in /repo".into(),
        actions: vec![
            IntentAction {
                id: "accept".into(),
                label: "Allow once".into(),
            },
            IntentAction {
                id: "cancel".into(),
                label: "Cancel".into(),
            },
        ],
        created_at_ms: 1_791_894_170_000,
        expires_at_ms: None,
    }
}

fn open_intent() -> NotificationIntent {
    NotificationIntent {
        id: "01K7F3B5N2Q9W8E4R6T1Y0V3NU".into(),
        kind: IntentKind::AgentFinished,
        session_id: sid(),
        workspace_id: None,
        title: "omp finished".into(),
        body: "Refactored the scheduler.".into(),
        actions: vec![IntentAction {
            id: hutan_protocol::notify::ACTION_OPEN.into(),
            label: "Open".into(),
        }],
        created_at_ms: 1_791_894_170_000,
        expires_at_ms: Some(1_791_897_770_000),
    }
}

fn styled_row() -> Row {
    Row {
        runs: vec![
            Run {
                text: "$ ls".into(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::BOLD,
                underline_color: None,
                link: None,
            },
            Run {
                text: "界".into(),
                fg: Color::Indexed(196),
                bg: Color::Rgb(4, 5, 6),
                flags: CellFlags::WIDE | CellFlags::UNDERCURL,
                underline_color: Some(Color::Rgb(255, 0, 0)),
                link: Some(1),
            },
        ],
        wrapped: false,
    }
}

fn cursor() -> Cursor {
    Cursor {
        col: 4,
        row: 0,
        shape: CursorShape::Bar,
        visible: true,
        blinking: false,
    }
}

fn renders() -> Vec<(&'static str, TerminalRender)> {
    vec![
        (
            "render_snapshot",
            TerminalRender::Snapshot(TerminalSnapshot {
                frame: 1,
                cols: 80,
                rows: 2,
                cursor: cursor(),
                modes: Modes {
                    bracketed_paste: true,
                    mouse: MouseMode::Drag,
                    mouse_sgr: true,
                    kitty_keyboard: 1,
                    ..Modes::default()
                },
                palette: Palette {
                    indexed: vec![(1, (200, 0, 0))],
                    background: Some((0, 0, 0)),
                    ..Palette::default()
                },
                title: Some("zsh".into()),
                lines: vec![styled_row(), Row::default()],
                links: vec![Link {
                    id: 1,
                    uri: "https://example.com".into(),
                }],
                history_len: 120,
                viewport_top: 1480,
            }),
        ),
        (
            "render_frame",
            TerminalRender::Frame(TerminalFrame {
                frame: 2,
                scrolled: 1,
                dirty: vec![(1, styled_row())],
                cursor: cursor(),
                modes: None,
                title: Some(None),
                links: vec![],
                history_len: 121,
                viewport_top: 1481,
            }),
        ),
    ]
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0")
}

/// JSON objects are unordered on the wire, and key order depends on whether some crate in the
/// build enables `serde_json/preserve_order` (GPUI does). Fixtures therefore store, and tests
/// compare, a canonical form with recursively sorted keys. Hex fixtures are compared verbatim.
fn canonical(name: &str, actual: &str) -> String {
    if !name.ends_with(".json") {
        return actual.to_owned();
    }
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: BTreeMap<String, serde_json::Value> =
                    map.into_iter().map(|(k, v)| (k, sort(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sort).collect())
            }
            other => other,
        }
    }
    let value: serde_json::Value = serde_json::from_str(actual).unwrap();
    serde_json::to_string(&sort(value)).unwrap()
}

/// Compares against the committed fixture, or rewrites it when `UPDATE_GOLDEN=1`.
fn check_golden(name: &str, actual: &str) {
    let path = fixture_dir().join(name);
    let actual = canonical(name, actual);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(fixture_dir()).unwrap();
        std::fs::write(&path, format!("{actual}\n")).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing fixture {}: {e}; run `just update-golden`",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected.trim_end(),
        "wire format changed for {name}"
    );
}

#[test]
fn requests_match_fixtures_and_roundtrip() {
    for (i, (name, request)) in requests().into_iter().enumerate() {
        let id = i as u64 + 1;
        let json = String::from_utf8(
            ControlMessage::request(id, &request)
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
        check_golden(&format!("request_{name}.json"), &json);

        let ControlMessage::Request {
            id: got_id,
            method,
            params,
        } = ControlMessage::from_json(json.as_bytes()).unwrap()
        else {
            panic!("{name}: not classified as request");
        };
        assert_eq!(got_id, id);
        assert_eq!(Request::decode(&method, params).unwrap(), request, "{name}");
    }
}

#[test]
fn events_match_fixtures_and_roundtrip() {
    for (i, (name, event)) in events().into_iter().enumerate() {
        let seq = Seq(1000 + i as u64);
        let json = String::from_utf8(
            ControlMessage::event(epoch(), seq, &event)
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
        check_golden(&format!("event_{name}.json"), &json);

        let ControlMessage::Event {
            event: tag,
            data,
            seq: got_seq,
            ..
        } = ControlMessage::from_json(json.as_bytes()).unwrap()
        else {
            panic!("{name}: not classified as event");
        };
        assert_eq!(got_seq, seq);
        assert_eq!(Event::decode(&tag, data).unwrap(), Some(event), "{name}");
    }
}

#[test]
fn responses_match_fixtures() {
    let cases: Vec<(&str, ControlMessage)> = vec![
        (
            "welcome",
            ControlMessage::ok(
                1,
                &Welcome {
                    protocol: PROTOCOL_VERSION,
                    daemon_version: "0.1.0".into(),
                    epoch: epoch(),
                    capabilities: vec!["terminal.render.v1".into()],
                    resume: ResumeOutcome::SnapshotRequired,
                    daemon_build: None,
                },
            )
            .unwrap(),
        ),
        (
            "session_snapshot",
            ControlMessage::ok(
                2,
                &SessionSnapshot {
                    workspaces: vec![workspace_info()],
                    sessions: vec![session_info()],
                    seq: Seq(1042),
                },
            )
            .unwrap(),
        ),
        (
            "terminal_history",
            ControlMessage::ok(
                3,
                &TerminalHistoryRows {
                    from_line: 0,
                    rows: vec![styled_row()],
                },
            )
            .unwrap(),
        ),
        (
            "terminal_blocks",
            ControlMessage::ok(
                6,
                &TerminalBlocksResult {
                    blocks: vec![prompt_block(), finished_block()],
                    viewport_top: 1480,
                },
            )
            .unwrap(),
        ),
        (
            "terminal_block_text",
            ControlMessage::ok(
                7,
                &TerminalBlockTextResult {
                    text: "running 3 tests\ntest result: FAILED".into(),
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "terminal_search",
            ControlMessage::ok(
                9,
                &TerminalSearchResult {
                    matches: vec![
                        TerminalSearchMatch {
                            row: 1481,
                            col: 12,
                            end_row: 1482,
                            end_col: 3,
                            block_id: Some(12),
                            line: Some("test result: FAILED. 2 passed; 1 failed".into()),
                        },
                        TerminalSearchMatch {
                            row: 1490,
                            col: 0,
                            end_row: 1490,
                            end_col: 5,
                            block_id: None,
                            line: None,
                        },
                    ],
                    truncated: true,
                    next_from: Some(1491),
                    timed_out: false,
                    viewport_top: 1480,
                    first_row: 0,
                },
            )
            .unwrap(),
        ),
        (
            "terminal_search_empty",
            ControlMessage::ok(
                9,
                &TerminalSearchResult {
                    matches: vec![],
                    truncated: false,
                    next_from: None,
                    timed_out: false,
                    viewport_top: 24,
                    first_row: 0,
                },
            )
            .unwrap(),
        ),
        (
            "workspace_info",
            ControlMessage::ok(8, &workspace_info()).unwrap(),
        ),
        (
            "workspace_list",
            ControlMessage::ok(
                9,
                &WorkspaceList {
                    workspaces: vec![
                        home_info(),
                        container_info(),
                        workspace_info(),
                        folder_project_info(),
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "workspace_opened",
            ControlMessage::ok(
                10,
                &WorkspaceOpened {
                    workspace: workspace_info(),
                    created: true,
                },
            )
            .unwrap(),
        ),
        (
            "recent_projects",
            ControlMessage::ok(
                11,
                &RecentProjects {
                    projects: vec![
                        RecentProject {
                            path: "/repo".into(),
                            name: "hutan".into(),
                            last_opened_ms: 1_791_894_170_000,
                            workspace_id: Some(ws()),
                        },
                        RecentProject {
                            path: "/old/project".into(),
                            name: "project".into(),
                            last_opened_ms: 1_791_800_000_000,
                            workspace_id: None,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        ("ack", ControlMessage::ok(4, &Empty {}).unwrap()),
        (
            "history_hits",
            ControlMessage::ok(
                7,
                &HistoryHits {
                    hits: vec![
                        HistoryHit {
                            source: HistorySource::Claude,
                            session_key: "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                            title: Some("Scheduler stall investigation".into()),
                            cwd: Some("/work/app".into()),
                            role: HistoryRole::User,
                            ts_ms: Some(1_791_894_164_332),
                            snippet: "why does the «quokka» scheduler stall".into(),
                            score: 4.25,
                            matches: 3,
                            resume_argv: Some(vec![
                                "claude".into(),
                                "--resume".into(),
                                "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".into(),
                            ]),
                            session_id: None,
                            path: Some(
                                "/Users/u/.claude/projects/-work-app/5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60.jsonl"
                                    .into(),
                            ),
                            terminal_id: None,
                            block_id: None,
                        },
                        HistoryHit {
                            source: HistorySource::Hutan,
                            session_key: sid().to_string(),
                            title: None,
                            cwd: None,
                            role: HistoryRole::Output,
                            ts_ms: None,
                            snippet: "test result: ok".into(),
                            score: 1.5,
                            matches: 1,
                            resume_argv: None,
                            session_id: Some(sid()),
                            path: None,
                            terminal_id: Some(TID),
                            block_id: Some(12),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "history_status",
            ControlMessage::ok(
                8,
                &HistoryStatusInfo {
                    enabled: true,
                    sources: vec![
                        HistorySourceStatus {
                            name: "claude".into(),
                            files: 12,
                            rows: 1790,
                            last_indexed_ms: Some(1_791_894_164_332),
                            errors: 0,
                        },
                        HistorySourceStatus {
                            name: "hutan".into(),
                            files: 0,
                            rows: 4,
                            last_indexed_ms: None,
                            errors: 0,
                        },
                    ],
                    db_bytes: 11_534_336,
                },
            )
            .unwrap(),
        ),
        (
            "history_sessions",
            ControlMessage::ok(
                9,
                &HistorySessions {
                    sessions: vec![HistorySessionRow {
                        source: HistorySource::Omp,
                        session_key: "4f1c2a9e".into(),
                        title: Some("Fix the flaky test".into()),
                        cwd: Some("/Users/dev/proj/hutan".into()),
                        branch: Some("main".into()),
                        model: Some("openai-codex/gpt-5.6-luna".into()),
                        messages: 8,
                        created_ms: Some(1_791_894_100_000),
                        updated_ms: Some(1_791_894_164_332),
                        first_prompt: Some("Fix the flaky test in launcher_gpui".into()),
                        latest: vec![
                            HistoryTurn {
                                role: HistoryRole::User,
                                text: "and the second one?".into(),
                                ts_ms: Some(1_791_894_160_000),
                            },
                            HistoryTurn {
                                role: HistoryRole::Assistant,
                                text: "Both pass now.".into(),
                                ts_ms: Some(1_791_894_164_332),
                            },
                        ],
                        workspace_id: Some(ws()),
                        resume_argv: Some(vec![
                            "omp".into(),
                            "--resume".into(),
                            "/Users/dev/.omp/agent/sessions/x/4f1c2a9e.jsonl".into(),
                        ]),
                        path: Some("/Users/dev/.omp/agent/sessions/x/4f1c2a9e.jsonl".into()),
                        session_id: None,
                        closed: false,
                    }],
                    matched: 2,
                    loaded: 250,
                    scan: HistoryScanInfo {
                        files: 986,
                        done: 986,
                        scanning: false,
                        finished_ms: Some(1_791_894_164_000),
                        per_source: vec![
                            HistorySourceFiles {
                                name: "claude".into(),
                                files: 610,
                            },
                            HistorySourceFiles {
                                name: "omp".into(),
                                files: 376,
                            },
                        ],
                    },
                },
            )
            .unwrap(),
        ),
        (
            "agent_applied",
            ControlMessage::ok(6, &AgentApplied { applied: false }).unwrap(),
        ),
        (
            "error",
            ControlMessage::err(5, ErrorBody::new(ErrorCode::NotFound, "session not found")),
        ),
        (
            "error_option_not_offered",
            ControlMessage::err(
                6,
                ErrorBody::new(
                    ErrorCode::OptionNotOffered,
                    "option `decline` was not offered; offered: accept, cancel",
                ),
            ),
        ),
        (
            "error_forbidden",
            ControlMessage::err(
                7,
                ErrorBody::new(
                    ErrorCode::Forbidden,
                    "attachment `file` is not allowed: `assistant.context.files` is false for the global assistant",
                ),
            ),
        ),
        (
            "error_unsupported",
            ControlMessage::err(
                8,
                ErrorBody::new(
                    ErrorCode::Unsupported,
                    "omp does not accept images (its handshake offers no `promptCapabilities.image`)",
                ),
            ),
        ),
        (
            "assistant_list",
            ControlMessage::ok(
                14,
                &AssistantList {
                    assistants: vec![
                        AssistantEntry {
                            scope: AssistantScope::Global,
                            workspace_id: ws(),
                            default: true,
                            backend: AssistantBackend::Provider,
                            session: assistant_session_info(),
                        },
                        AssistantEntry {
                            scope: AssistantScope::Workspace,
                            workspace_id: ws(),
                            default: false,
                            backend: AssistantBackend::Provider,
                            session: assistant_session_info(),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "assistant_list_backends",
            ControlMessage::ok(
                16,
                &AssistantList {
                    assistants: vec![
                        AssistantEntry {
                            scope: AssistantScope::Global,
                            workspace_id: ws(),
                            default: false,
                            backend: AssistantBackend::Provider,
                            session: assistant_session_info(),
                        },
                        AssistantEntry {
                            scope: AssistantScope::Global,
                            workspace_id: ws(),
                            default: true,
                            backend: AssistantBackend::Omp,
                            session: harness_assistant_session_info(),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "assistant_configured",
            ControlMessage::ok(
                17,
                &AssistantConfig {
                    scope: AssistantScope::Workspace,
                    workspace_id: Some(ws()),
                    backend: AssistantBackend::Omp,
                    model: Some("anthropic/claude-sonnet-4-5".into()),
                    provider: None,
                },
            )
            .unwrap(),
        ),
        (
            "assistant_session_result",
            ControlMessage::ok(
                15,
                &SessionResult {
                    session: assistant_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result",
            ControlMessage::ok(
                12,
                &SessionResult {
                    session: chat_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "chat_sent",
            ControlMessage::ok(13, &ChatSent { turn_id: turn() }).unwrap(),
        ),
        (
            "chat_forked",
            ControlMessage::ok(
                74,
                &ChatForked {
                    session: SessionInfo {
                        title: "omp · repo (fork)".into(),
                        chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                            fork: Some(ChatForkSupport {
                                support: hutan_core::ForkSupport::Head,
                                note: Some(
                                    "OMP forks only at its newest turn over ACP".into(),
                                ),
                            }),
                            ..chat
                        }),
                        forked_from: Some(hutan_core::ForkOrigin {
                            session_id: sid(),
                            turn_id: turn(),
                            ordinal: 2,
                            mode: hutan_core::ForkMode::Head,
                            at_ms: 1_791_894_400_000,
                        }),
                        ..harness_chat_session_info()
                    },
                    fork: ForkDone {
                        mode: hutan_core::ForkMode::Head,
                        support: hutan_core::ForkSupport::Head,
                        turn_id: turn(),
                        ordinal: 2,
                        copied_events: 41,
                    },
                },
            )
            .unwrap(),
        ),
        (
            "error_unsupported_fork_head_only",
            ControlMessage::err(75, {
                let mut error = ErrorBody::new(
                    ErrorCode::Unsupported,
                    "OMP forks only at its newest turn: fork at the latest turn or with a summary",
                );
                error.data = Some(serde_json::json!({
                    "support": "head",
                    "alternatives": ["head", "summary"],
                }));
                error
            }),
        ),
        (
            "chat_history",
            ControlMessage::ok(
                14,
                &ChatHistory {
                    events: vec![
                        agent_event(
                            44,
                            AgentEventKind::MessageEnd {
                                role: MessageRole::User,
                                phase: None,
                                text: "run the tests".into(),
                            },
                        )
                        .replayed(),
                        agent_event(46, AgentEventKind::TurnCancelled).replayed(),
                        agent_event(
                            58,
                            AgentEventKind::MessagePartial {
                                message_id: item(),
                                phase: None,
                                text: "All three".into(),
                            },
                        )
                        .replayed(),
                    ],
                    has_more: true,
                    last_seq: 60,
                },
            )
            .unwrap(),
        ),
        (
            "chat_models",
            ControlMessage::ok(
                15,
                &ChatModels {
                    agent: "codex".into(),
                    models: vec![
                        ModelInfo {
                            id: "gpt-6.1-sol".into(),
                            display_name: "GPT-6.1-Sol".into(),
                            description: "Latest workhorse model".into(),
                            is_default: true,
                            hidden: false,
                            default_reasoning_effort: Some("low".into()),
                            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
                        },
                        ModelInfo {
                            id: "gpt-5.6-luna".into(),
                            display_name: "GPT-5.6-Luna".into(),
                            description: String::new(),
                            is_default: false,
                            hidden: false,
                            default_reasoning_effort: None,
                            reasoning_efforts: vec![],
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "approval_list",
            ControlMessage::ok(
                16,
                &ApprovalList {
                    approvals: vec![approval_info()],
                },
            )
            .unwrap(),
        ),
        (
            "agent_tree",
            ControlMessage::ok(
                17,
                &AgentTree {
                    observations: vec![observation()],
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result_harness_mode",
            ControlMessage::ok(
                36,
                &SessionResult {
                    session: SessionInfo {
                        chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                            mode: Some(ChatMode::new(
                                "bypassPermissions",
                                Some("Bypass Permissions".into()),
                            )),
                            ..chat
                        }),
                        ..harness_chat_session_info()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result_direct",
            ControlMessage::ok(
                18,
                &SessionResult {
                    session: direct_chat_session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "provider_list",
            ControlMessage::ok(
                19,
                &ProviderList {
                    providers: vec![provider_info(), local_provider_info()],
                },
            )
            .unwrap(),
        ),
        (
            "provider_result",
            ControlMessage::ok(
                20,
                &ProviderResult {
                    provider: provider_info(),
                },
            )
            .unwrap(),
        ),
        (
            "provider_models",
            ControlMessage::ok(
                21,
                &ProviderModels {
                    provider_id: "openrouter".into(),
                    models: vec![ModelInfo {
                        id: "meta-llama/llama-3.3-70b-instruct".into(),
                        display_name: "meta-llama/llama-3.3-70b-instruct".into(),
                        description: "meta-llama".into(),
                        is_default: true,
                        hidden: false,
                        default_reasoning_effort: None,
                        reasoning_efforts: vec![],
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "provider_discovery",
            ControlMessage::ok(
                22,
                &ProviderDiscovery {
                    found: vec![
                        DiscoveredProvider {
                            kind: "ollama".into(),
                            name: "Ollama".into(),
                            base_url: "http://127.0.0.1:11434".into(),
                            models: vec!["llama3.3:70b".into(), "qwen3:8b".into()],
                            provider_id: Some("ollama".into()),
                        },
                        DiscoveredProvider {
                            kind: "openai-compatible".into(),
                            name: "LM Studio".into(),
                            base_url: "http://127.0.0.1:1234/v1".into(),
                            models: vec![],
                            provider_id: None,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "notify_list",
            ControlMessage::ok(
                16,
                &NotifyList {
                    intents: vec![approval_intent(), open_intent()],
                },
            )
            .unwrap(),
        ),
        (
            "notify_acted_approval",
            ControlMessage::ok(
                17,
                &NotifyActed {
                    route: ActRoute::Approval,
                    desktop_clients: 0,
                },
            )
            .unwrap(),
        ),
        (
            "notify_acted_ui_open",
            ControlMessage::ok(
                18,
                &NotifyActed {
                    route: ActRoute::UiOpen,
                    desktop_clients: 1,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_created",
            ControlMessage::ok(
                23,
                &WorktreeCreated {
                    worktree: worktree_info(),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_setup_result_needs_trust",
            ControlMessage::ok(
                23,
                &WorktreeSetupResult {
                    worktree: setting_up_worktree_info(),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_setup_result_failed",
            ControlMessage::ok(
                23,
                &WorktreeSetupResult {
                    worktree: failed_setup_worktree_info(),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_cleanup_list",
            ControlMessage::ok(
                24,
                &WorktreeCleanupList {
                    mode: WorktreeCleanup::Merged,
                    idle_days: 14,
                    candidates: vec![
                        CleanupCandidate {
                            path: "/work/repo-wt/feature-login".into(),
                            repo_root: "/work/repo".into(),
                            branch: Some("feature/login".into()),
                            reason: CleanupReason::Merged,
                            idle_days: 3,
                            teardown: vec!["docker compose down".into()],
                        },
                        CleanupCandidate {
                            path: "/work/repo-wt/old".into(),
                            repo_root: "/work/repo".into(),
                            branch: None,
                            reason: CleanupReason::Idle,
                            idle_days: 40,
                            teardown: vec![],
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "worktree_pruned",
            ControlMessage::ok(
                25,
                &WorktreePruned {
                    removed: vec!["/work/repo-wt/feature-login".into()],
                    failed: vec![PruneFailure {
                        path: "/work/repo-wt/old".into(),
                        reason: "teardown failed: `docker compose down` exited with 1".into(),
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "worktree_list",
            ControlMessage::ok(
                24,
                &WorktreeList {
                    worktrees: vec![
                        main_worktree_info(),
                        worktree_info(),
                        detached_worktree_info(),
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "worktree_diff",
            ControlMessage::ok(
                27,
                &WorktreeDiff {
                    base: "main".into(),
                    merge_base: "5b1c0f3e8f6d2a4c7b9e0d1f2a3b4c5d6e7f8091".into(),
                    files: vec![
                        DiffFile {
                            path: "src/login.rs".into(),
                            old_path: Some("src/signin.rs".into()),
                            status: DiffStatus::Renamed,
                            additions: 12,
                            deletions: 3,
                            binary: false,
                            committed: true,
                            uncommitted: false,
                            staged: false,
                            unstaged: false,
                        },
                        DiffFile {
                            path: "assets/logo.png".into(),
                            old_path: None,
                            status: DiffStatus::Added,
                            additions: 0,
                            deletions: 0,
                            binary: true,
                            committed: true,
                            uncommitted: false,
                            staged: false,
                            unstaged: false,
                        },
                        DiffFile {
                            path: "old.txt".into(),
                            old_path: None,
                            status: DiffStatus::Deleted,
                            additions: 0,
                            deletions: 7,
                            binary: false,
                            committed: false,
                            uncommitted: true,
                            staged: false,
                            unstaged: false,
                        },
                        DiffFile {
                            path: "README.md".into(),
                            old_path: None,
                            status: DiffStatus::Modified,
                            additions: 1,
                            deletions: 1,
                            binary: false,
                            committed: true,
                            uncommitted: true,
                            staged: false,
                            unstaged: false,
                        },
                    ],
                    patch_truncated: true,
                    commits: vec![
                        CommitSummary {
                            id: "3f786850e387550fdab836ed7e6dc881de23001b".into(),
                            subject: "Add the login form".into(),
                        },
                        CommitSummary {
                            id: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".into(),
                            subject: "Rename signin to login".into(),
                        },
                    ],
                    commits_truncated: true,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_diff_index",
            ControlMessage::ok(
                35,
                &WorktreeDiff {
                    base: "main".into(),
                    merge_base: "5b1c0f3e8f6d2a4c7b9e0d1f2a3b4c5d6e7f8091".into(),
                    files: [("a.txt", true, false), ("b.txt", false, true), ("c.txt", true, true)]
                        .into_iter()
                        .map(|(path, staged, unstaged)| DiffFile {
                            path: path.into(),
                            old_path: None,
                            status: DiffStatus::Modified,
                            additions: 1,
                            deletions: 0,
                            binary: false,
                            committed: false,
                            uncommitted: true,
                            staged,
                            unstaged,
                        })
                        .collect(),
                    patch_truncated: false,
                    commits: Vec::new(),
                    commits_truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_diff_empty",
            ControlMessage::ok(
                28,
                &WorktreeDiff {
                    base: "main".into(),
                    merge_base: "5b1c0f3e8f6d2a4c7b9e0d1f2a3b4c5d6e7f8091".into(),
                    files: vec![],
                    patch_truncated: false,
                    commits: vec![],
                    commits_truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_file_diff",
            ControlMessage::ok(
                29,
                &WorktreeFileDiff {
                    patch: "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n".into(),
                    truncated: false,
                    binary: false,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_file_diff_truncated_binary",
            ControlMessage::ok(
                30,
                &WorktreeFileDiff {
                    patch: "Binary files /dev/null and b/assets/logo.png differ\n".into(),
                    truncated: true,
                    binary: true,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_merged",
            ControlMessage::ok(
                31,
                &WorktreeMerged {
                    commit: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                    target: "main".into(),
                    branch: "feature/login".into(),
                    strategy: MergeStrategy::Squash,
                    source_dirty: true,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_merged_clean",
            ControlMessage::ok(
                32,
                &WorktreeMerged {
                    commit: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                    target: "main".into(),
                    branch: "feature/login".into(),
                    strategy: MergeStrategy::Merge,
                    source_dirty: false,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_committed",
            ControlMessage::ok(
                33,
                &WorktreeCommitted {
                    commit: "7a6b5c4d3e2f10987a6b5c4d3e2f10987a6b5c4d".into(),
                    branch: "feature/login".into(),
                    files: 3,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_hunks",
            ControlMessage::ok(
                34,
                &WorktreeHunks {
                    files: vec![
                        FileHunks {
                            path: "assets/logo.png".into(),
                            binary: true,
                            unstaged: vec![],
                            staged: vec![],
                        },
                        FileHunks {
                            path: "src/login.rs".into(),
                            binary: false,
                            unstaged: vec![DiffHunk {
                                id: "5f0c3a9d1e7b2a46".into(),
                                header: "@@ -10,3 +10,4 @@ fn login() {".into(),
                                old_start: 10,
                                old_lines: 3,
                                new_start: 10,
                                new_lines: 4,
                                body: " let user = form.user();\n+let name = user.trim();\n let ok = check(user);\n return ok;".into(),
                            }],
                            staged: vec![DiffHunk {
                                id: "9d8c7b6a5f4e3d2c".into(),
                                header: "@@ -1,2 +1,2 @@".into(),
                                old_start: 1,
                                old_lines: 2,
                                new_start: 1,
                                new_lines: 2,
                                body: "-use std::io;\n+use std::fmt;\n use std::fs;".into(),
                            }],
                        },
                    ],
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_indexed",
            ControlMessage::ok(
                35,
                &WorktreeIndexed {
                    paths: 1,
                    hunks: 2,
                },
            )
            .unwrap(),
        ),
        (
            "worktree_discarded",
            ControlMessage::ok(
                36,
                &WorktreeDiscarded {
                    paths: 0,
                    hunks: 1,
                    checkpoint: restore_checkpoint_fixture(),
                },
            )
            .unwrap(),
        ),
        (
            "error_worktree_hunk_stale",
            ControlMessage::err(
                37,
                ErrorBody::new(
                    ErrorCode::Conflict,
                    "hunk 5f0c3a9d1e7b2a46 of src/login.rs is gone: the file changed since the \
                     hunks were listed",
                ),
            ),
        ),
        (
            "error_worktree_merge_conflict",
            ControlMessage::err(
                33,
                ErrorBody {
                    code: ErrorCode::Conflict,
                    message: "merging `feature/login` into `main` conflicts in 2 file(s): a.txt, src/lib.rs; nothing was changed".into(),
                    data: Some(serde_json::json!({"conflicts": ["a.txt", "src/lib.rs"]})),
                },
            ),
        ),
        (
            "worktree_removed",
            ControlMessage::ok(25, &WorktreeRemoved::default()).unwrap(),
        ),
        (
            "worktree_removed_branch_kept",
            ControlMessage::ok(
                26,
                &WorktreeRemoved {
                    branch_kept: Some(
                        "error: the branch 'feature/login' is not fully merged".into(),
                    ),
                },
            )
            .unwrap(),
        ),
        (
            "session_result_placement",
            ControlMessage::ok(
                27,
                &SessionResult {
                    session: SessionInfo {
                        placement: Some(Placement {
                            kind: PlacementKind::Worktree,
                            root: "/work/repo-wt/feature-login".into(),
                            branch: Some("feature/login".into()),
                            shared_with: vec![sid2()],
                        }),
                        ..session_info()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "task_result",
            ControlMessage::ok(
                28,
                &TaskResult {
                    task: running_task(),
                },
            )
            .unwrap(),
        ),
        (
            "task_result_minimal",
            ControlMessage::ok(
                29,
                &TaskResult {
                    task: minimal_task(),
                },
            )
            .unwrap(),
        ),
        (
            "task_reviewed",
            ControlMessage::ok(
                34,
                &TaskReviewed {
                    task: merged_task(),
                    session: None,
                },
            )
            .unwrap(),
        ),
        (
            "task_reviewed_resumed",
            ControlMessage::ok(
                35,
                &TaskReviewed {
                    task: running_task(),
                    session: Some(sid()),
                },
            )
            .unwrap(),
        ),
        (
            "task_list",
            ControlMessage::ok(
                30,
                &TaskList {
                    tasks: vec![minimal_task(), running_task()],
                },
            )
            .unwrap(),
        ),
        (
            "task_deleted",
            ControlMessage::ok(
                31,
                &TaskDeleted {
                    removed: vec![tid(), tid2()],
                },
            )
            .unwrap(),
        ),
        (
            "task_started",
            ControlMessage::ok(
                32,
                &TaskStarted {
                    task: running_task(),
                    session: SessionInfo {
                        placement: Some(Placement {
                            kind: PlacementKind::Worktree,
                            root: "/work/repo-wt/task-fix-login".into(),
                            branch: Some("task/fix-login".into()),
                            shared_with: vec![],
                        }),
                        ..session_info()
                    },
                    warnings: vec![],
                },
            )
            .unwrap(),
        ),
        (
            "task_started_warning",
            ControlMessage::ok(
                33,
                &TaskStarted {
                    task: running_task(),
                    session: session_info(),
                    warnings: vec![
                        "another agent already works in /work/repo: session 01K7F3B5N2Q9W8E4R6T1Y0V3KN"
                            .into(),
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "account_list",
            ControlMessage::ok(
                34,
                &AccountList {
                    accounts: vec![
                        AccountInfo {
                            harness: "omp".into(),
                            provider: "anthropic".into(),
                            label: "anthropic".into(),
                            plan: Some("max".into()),
                            status: AccountStatus::LoggedIn,
                            detail: None,
                        },
                        AccountInfo {
                            harness: "claude".into(),
                            provider: "anthropic".into(),
                            label: "Claude Code".into(),
                            plan: None,
                            status: AccountStatus::Unknown,
                            detail: Some(
                                "`claude` was not found; install Claude Code".into(),
                            ),
                        },
                        AccountInfo {
                            harness: "codex".into(),
                            provider: "openai".into(),
                            label: "ChatGPT".into(),
                            plan: Some("plus".into()),
                            status: AccountStatus::LoggedOut,
                            detail: None,
                        },
                    ],
                    managed: Vec::new(),
                    defaults: Vec::new(),
                },
            )
            .unwrap(),
        ),
        (
            "account_list_managed",
            ControlMessage::ok(
                35,
                &AccountList {
                    accounts: vec![AccountInfo {
                        harness: "claude".into(),
                        provider: "anthropic".into(),
                        label: "Claude subscription".into(),
                        plan: Some("max".into()),
                        status: AccountStatus::LoggedIn,
                        detail: None,
                    }],
                    managed: vec![
                        ManagedAccount {
                            harness: "claude".into(),
                            name: "work".into(),
                            label: "Work".into(),
                            home: "/Users/me/Library/Application Support/Hutan/accounts/claude/work"
                                .into(),
                            created_at_ms: 1_761_350_000_000,
                            logins: vec![AccountInfo {
                                harness: "claude".into(),
                                provider: "anthropic".into(),
                                label: "Claude subscription".into(),
                                plan: Some("team".into()),
                                status: AccountStatus::LoggedIn,
                                detail: None,
                            }],
                        },
                        ManagedAccount {
                            harness: "codex".into(),
                            name: "home".into(),
                            label: "home".into(),
                            home: "/Users/me/Library/Application Support/Hutan/accounts/codex/home"
                                .into(),
                            created_at_ms: 1_761_350_100_000,
                            logins: Vec::new(),
                        },
                    ],
                    defaults: vec![
                        AccountDefault {
                            harness: "claude".into(),
                            project: None,
                            account: "work".into(),
                            source: DefaultSource::Registry,
                        },
                        AccountDefault {
                            harness: "claude".into(),
                            project: Some("01K7F3B5N2Q9W8E4R6T1Y0V3KN".into()),
                            account: "system".into(),
                            source: DefaultSource::Registry,
                        },
                        AccountDefault {
                            harness: "codex".into(),
                            project: None,
                            account: "home".into(),
                            source: DefaultSource::Config,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "account_added",
            ControlMessage::ok(
                36,
                &ManagedAccount {
                    harness: "claude".into(),
                    name: "work".into(),
                    label: "Work".into(),
                    home: "/Users/me/Library/Application Support/Hutan/accounts/claude/work"
                        .into(),
                    created_at_ms: 1_761_350_000_000,
                    logins: Vec::new(),
                },
            )
            .unwrap(),
        ),
        (
            "account_removed",
            ControlMessage::ok(
                37,
                &AccountRemoved {
                    logout: RemoveLogout::Failed,
                    detail: Some("the harness did not answer in time".into()),
                },
            )
            .unwrap(),
        ),
        (
            "account_removed_done",
            ControlMessage::ok(
                38,
                &AccountRemoved {
                    logout: RemoveLogout::Done,
                    detail: None,
                },
            )
            .unwrap(),
        ),
        (
            "ui_action_list",
            ControlMessage::ok(
                60,
                &hutan_protocol::ui::UiActionList {
                    window_id: "window-1".into(),
                    actions: ui_publish_fixture().actions,
                    settings: ui_publish_fixture().settings,
                },
            )
            .unwrap(),
        ),
        (
            "account_session",
            ControlMessage::ok(
                35,
                &AccountSession {
                    session: session_info(),
                    instructions: None,
                },
            )
            .unwrap(),
        ),
        (
            "ui_layout",
            ControlMessage::ok(
                61,
                &hutan_protocol::ui::UiLayoutResult {
                    layout: layout_fixture(),
                },
            )
            .unwrap(),
        ),
        (
            "account_session_instructions",
            ControlMessage::ok(
                36,
                &AccountSession {
                    session: session_info(),
                    instructions: Some("type /logout in omp".into()),
                },
            )
            .unwrap(),
        ),
        (
            "resources_list",
            ControlMessage::ok(34, &resource_sample()).unwrap(),
        ),
        (
            "resources_subscribed",
            ControlMessage::ok(
                35,
                &ResourcesSubscribed {
                    interval_ms: 2_000,
                    sample: resource_sample(),
                },
            )
            .unwrap(),
        ),
        (
            "ui_ran",
            ControlMessage::ok(
                62,
                &hutan_protocol::ui::UiRan {
                    window_id: "window-1".into(),
                    result: serde_json::json!({"preset": "focus"}),
                },
            )
            .unwrap(),
        ),
        (
            "process_signaled",
            ControlMessage::ok(
                36,
                &ProcessSignaled {
                    session_id: sid(),
                    pid: 48_215,
                    signal: SignalName::Term,
                },
            )
            .unwrap(),
        ),
        (
            "session_text_sent",
            ControlMessage::ok(63, &hutan_protocol::ui::SessionTextSent { bytes: 11 }).unwrap(),
        ),
        (
            "approval_list_control",
            ControlMessage::ok(
                64,
                &ApprovalList {
                    approvals: vec![control_approval()],
                },
            )
            .unwrap(),
        ),
        (
            "agent_interrupted_turn",
            ControlMessage::ok(
                37,
                &AgentInterrupted {
                    session_id: sid(),
                    node_id: None,
                    via: InterruptVia::Turn,
                    pid: None,
                },
            )
            .unwrap(),
        ),
        (
            "agent_interrupted_pid",
            ControlMessage::ok(
                38,
                &AgentInterrupted {
                    session_id: sid(),
                    node_id: Some("agent-abc123".into()),
                    via: InterruptVia::SigintTerm,
                    pid: Some(48_220),
                },
            )
            .unwrap(),
        ),
        (
            "error_forbidden_control",
            ControlMessage::err(65, {
                let mut error = ErrorBody::new(
                    ErrorCode::Forbidden,
                    "the user denied `session.stop`: it was not run",
                );
                error.data = Some(serde_json::json!({"control": {"decision": "deny"}}));
                error
            }),
        ),
        (
            "question_list",
            ControlMessage::ok(
                19,
                &QuestionList {
                    questions: vec![question_info()],
                },
            )
            .unwrap(),
        ),
        (
            "daemon_settings",
            ControlMessage::ok(
                15,
                &hutan_protocol::daemon_settings::DaemonSettings {
                    file: "/Users/me/Library/Application Support/Hutan/daemon.toml".into(),
                    restart_pending: true,
                    entries: daemon_setting_entries(),
                },
            )
            .unwrap(),
        ),
        (
            "recovery_acked",
            ControlMessage::ok(
                14,
                &RecoveryAcked {
                    sessions: vec![sid()],
                },
            )
            .unwrap(),
        ),
        (
            "session_reopened",
            ControlMessage::ok(
                16,
                &SessionResult {
                    session: session_info(),
                },
            )
            .unwrap(),
        ),
        (
            "session_result_muted",
            ControlMessage::ok(
                34,
                &SessionResult {
                    session: SessionInfo {
                        muted: true,
                        ..session_info()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "error_session_busy",
            ControlMessage::err(
                17,
                ErrorBody {
                    code: ErrorCode::InvalidState,
                    message: "session is busy: `npm run dev` is still running".into(),
                    data: Some(
                        serde_json::to_value(serde_json::json!({
                            "busy": SessionBusy {
                                command: Some("npm run dev".into()),
                                agent: None,
                                state: "working".into(),
                            }
                        }))
                        .unwrap(),
                    ),
                },
            ),
        ),
        (
            "history_sessions_closed",
            ControlMessage::ok(
                18,
                &HistorySessions {
                    sessions: vec![HistorySessionRow {
                        source: HistorySource::Hutan,
                        session_key: sid().to_string(),
                        title: Some("cargo watch".into()),
                        cwd: Some("/Users/dev/proj/hutan".into()),
                        branch: None,
                        model: None,
                        messages: 3,
                        created_ms: Some(1_791_894_100_000),
                        updated_ms: Some(1_791_894_164_332),
                        first_prompt: None,
                        latest: vec![],
                        workspace_id: Some(ws()),
                        resume_argv: None,
                        path: None,
                        session_id: Some(sid()),
                        closed: true,
                    }],
                    matched: 1,
                    loaded: 1,
                    scan: HistoryScanInfo {
                        files: 0,
                        done: 0,
                        scanning: false,
                        finished_ms: None,
                        per_source: vec![],
                    },
                },
            )
            .unwrap(),
        ),
        (
            "control_grants",
            ControlMessage::ok(
                19,
                &ControlGrants {
                    grants: vec![
                        ControlGrant {
                            id: 7,
                            tool: "session.rename".into(),
                            workspace_id: Some(ws()),
                            chat_session_id: None,
                            created_ms: 1_791_894_190_000,
                        },
                        ControlGrant {
                            id: 6,
                            tool: "ui.run:workbench::SplitRight".into(),
                            workspace_id: None,
                            chat_session_id: Some(sid()),
                            created_ms: 1_791_894_185_000,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "memory_entries",
            ControlMessage::ok(
                20,
                &MemoryEntries {
                    entries: vec![memory_entry(), memory_entry_inferred()],
                },
            )
            .unwrap(),
        ),
        (
            "memory_history",
            ControlMessage::ok(
                21,
                &MemoryHistory {
                    entry: memory_entry_inferred(),
                    revisions: vec![
                        MemoryRevision {
                            revision: 2,
                            operation: MemoryOp::Update,
                            body: "Uses jj for version control; main branch is `trunk`.".into(),
                            content_hash: "5b0e0e56c1d8a4c3b2b6f4f0d8c2a1e7f3d9b5a7c1e3f5a7b9d1c3e5f7a9b1c3".into(),
                            author: MemoryAuthor::Agent,
                            provenance: memory_agent_provenance(),
                            approval_id: Some(approval()),
                            operation_id: "op-2".into(),
                            scope_revision: 5,
                            created_ms: 1_791_894_200_000,
                        },
                        MemoryRevision {
                            revision: 1,
                            operation: MemoryOp::Create,
                            body: "Uses jj for version control.".into(),
                            content_hash: "9c1d3e5f7a9b1c3d5e7f9a1b3c5d7e9f1a3b5c7d9e1f3a5b7c9d1e3f5a7b9c1d".into(),
                            author: MemoryAuthor::User,
                            provenance: memory_user_provenance(),
                            approval_id: None,
                            operation_id: "op-1".into(),
                            scope_revision: 4,
                            created_ms: 1_791_894_100_000,
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "memory_receipt",
            ControlMessage::ok(
                22,
                &MemoryReceipt {
                    status: "applied".into(),
                    operation_id: "op-1".into(),
                    entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
                    revision: 1,
                    scope_revision: 4,
                },
            )
            .unwrap(),
        ),
        (
            "memory_proposed",
            ControlMessage::ok(
                23,
                &MemoryProposed {
                    status: "proposed".into(),
                    proposal_id: approval(),
                    applied: false,
                },
            )
            .unwrap(),
        ),
        (
            "memory_proposals",
            ControlMessage::ok(
                24,
                &MemoryProposals {
                    proposals: vec![memory_proposal()],
                },
            )
            .unwrap(),
        ),
        (
            "memory_proposal_resolved",
            ControlMessage::ok(
                25,
                &MemoryProposalResolved {
                    proposal: MemoryProposal {
                        state: ProposalState::Applied,
                        resolved_ms: Some(1_791_894_260_000),
                        ..memory_proposal()
                    },
                    receipt: Some(MemoryReceipt {
                        status: "applied".into(),
                        operation_id: "proposal:01K7F3B5N2Q9W8E4R6T1Y0V3KM".into(),
                        entry_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KP".into(),
                        revision: 2,
                        scope_revision: 5,
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "memory_proposal_conflict",
            ControlMessage::ok(
                26,
                &MemoryProposalResolved {
                    proposal: MemoryProposal {
                        state: ProposalState::Conflict,
                        resolved_ms: Some(1_791_894_260_000),
                        ..memory_proposal()
                    },
                    receipt: None,
                },
            )
            .unwrap(),
        ),
        (
            "fs_file",
            ControlMessage::ok(
                27,
                &FsFile {
                    path: "/repo/src/main.rs".into(),
                    text: "\u{feff}fn main() {\r\n}\r\n".into(),
                    offset: 0,
                    size: 20,
                    eof: true,
                    version: FileVersion {
                        mtime_ns: 1_791_894_150_123_456_789,
                        size: 20,
                        ino: 8_834_113,
                    },
                    line_ending: LineEnding::Crlf,
                    bom: true,
                },
            )
            .unwrap(),
        ),
        (
            "fs_upload",
            ControlMessage::ok(
                28,
                &FsUpload {
                    upload_id: "01K7F3B5N2Q9W8E4R6T1Y0V3UP".into(),
                },
            )
            .unwrap(),
        ),
        (
            "fs_written",
            ControlMessage::ok(
                29,
                &FsWritten {
                    path: "/repo/src/main.rs".into(),
                    version: file_version(),
                    write_id: "01K7F3B5N2Q9W8E4R6T1Y0V3WR".into(),
                },
            )
            .unwrap(),
        ),
        (
            "fs_entry",
            ControlMessage::ok(30, &fs_entry("Cargo.toml", FsKind::File, false)).unwrap(),
        ),
        (
            "fs_op_results",
            ControlMessage::ok(
                70,
                &FsOpResults {
                    items: vec![
                        FsOpItem {
                            path: "/repo/a.txt".into(),
                            entry: Some(fs_entry("src/a.txt", FsKind::File, false)),
                            error: None,
                        },
                        FsOpItem {
                            path: "/repo/b".into(),
                            entry: None,
                            error: Some({
                                let mut error = ErrorBody::new(
                                    ErrorCode::Conflict,
                                    "/repo/src/b exists already",
                                );
                                error.data = Some(serde_json::json!({"reason": "exists"}));
                                error
                            }),
                        },
                    ],
                },
            )
            .unwrap(),
        ),
        (
            "fs_trashed",
            ControlMessage::ok(
                71,
                &FsTrashed {
                    ticket: Some("01K7F3B5N2Q9W8E4R6T1Y0V3TK".into()),
                    items: vec![FsOpItem {
                        path: "/repo/old.log".into(),
                        entry: Some(fs_entry("old.log", FsKind::File, false)),
                        error: None,
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "fs_trashed_nothing",
            ControlMessage::ok(
                72,
                &FsTrashed {
                    ticket: None,
                    items: vec![FsOpItem {
                        path: "/repo/gone".into(),
                        entry: None,
                        error: Some(ErrorBody::new(ErrorCode::NotFound, "/repo/gone: not found")),
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "error_unsupported_no_trash",
            ControlMessage::err(73, {
                let mut error =
                    ErrorBody::new(ErrorCode::Unsupported, "this volume has no Trash");
                error.data = Some(serde_json::json!({"reason": "no_trash"}));
                error
            }),
        ),
        (
            "fs_listing",
            ControlMessage::ok(
                31,
                &FsListing {
                    dir: "/repo".into(),
                    entries: vec![
                        fs_entry("src", FsKind::Dir, false),
                        fs_entry("target", FsKind::Dir, true),
                        fs_entry("Cargo.toml", FsKind::File, false),
                        fs_entry("current", FsKind::Symlink, false),
                    ],
                    truncated: true,
                },
            )
            .unwrap(),
        ),
        (
            "fs_found",
            ControlMessage::ok(
                32,
                &FsFound {
                    root: "/repo".into(),
                    matches: vec![FsFindMatch {
                        path: "crates/daemon/src/router.rs".into(),
                        score: 214,
                        matched: vec![[21, 22], [24, 26]],
                    }],
                    indexed: 5_412,
                    index_truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "fs_search_result",
            ControlMessage::ok(
                33,
                &FsSearchResult {
                    search_id: "s1".into(),
                    hits: vec![SearchHit {
                        path: "src/main.rs".into(),
                        line: 12,
                        text: "fn main() {".into(),
                        matched: vec![[0, 7]],
                    }],
                    files_searched: 412,
                    truncated: false,
                    cancelled: true,
                },
            )
            .unwrap(),
        ),
        (
            "fs_watching",
            ControlMessage::ok(
                34,
                &FsWatching {
                    watching: 2,
                    roots: vec!["/repo".into()],
                },
            )
            .unwrap(),
        ),
        (
            "fs_error_conflict",
            ControlMessage::err(35, {
                let mut error = ErrorBody::new(
                    ErrorCode::Conflict,
                    "/repo/src/main.rs changed on disk since it was read",
                );
                error.data = Some(serde_json::json!({
                    "reason": "version",
                    "current": file_version(),
                }));
                error
            }),
        ),
        (
            "fs_error_forbidden",
            ControlMessage::err(36, {
                let mut error = ErrorBody::new(
                    ErrorCode::Forbidden,
                    "/etc/hosts is outside the workspace roots",
                );
                error.data = Some(serde_json::json!({"reason": "outside_roots"}));
                error
            }),
        ),
        (
            "worktree_finished",
            ControlMessage::ok(
                37,
                &WorktreeFinished {
                    commit: FinishCommit {
                        status: StepStatus::Ok,
                        reason: None,
                        message: None,
                        branch: "feature/login".into(),
                        commit: Some("7a6b5c4d3e2f10987a6b5c4d3e2f10987a6b5c4d".into()),
                        files: 3,
                    },
                    merge: Some(FinishMerge {
                        status: StepStatus::Ok,
                        reason: None,
                        message: None,
                        branch: "feature/login".into(),
                        target: "main".into(),
                        strategy: MergeStrategy::Squash,
                        commit: Some("9d8c7b6a5f4e3d2c1b0a99887766554433221100".into()),
                        conflicts: vec![],
                        source_dirty: false,
                    }),
                    push: Some(FinishPush {
                        status: StepStatus::Ok,
                        reason: None,
                        message: None,
                        branch: "main".into(),
                        upstream: Some("origin/main".into()),
                        commit: Some("9d8c7b6a5f4e3d2c1b0a99887766554433221100".into()),
                        ahead: Some(1),
                        behind: Some(0),
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_finished_push_rejected",
            ControlMessage::ok(
                38,
                &WorktreeFinished {
                    commit: FinishCommit {
                        status: StepStatus::Ok,
                        reason: None,
                        message: None,
                        branch: "main".into(),
                        commit: Some("7a6b5c4d3e2f10987a6b5c4d3e2f10987a6b5c4d".into()),
                        files: 1,
                    },
                    merge: None,
                    push: Some(FinishPush {
                        status: StepStatus::Failed,
                        reason: Some(StepReason::Rejected),
                        message: Some(
                            "origin/main has 2 commits that main does not have; bring them in \
                             (pull, then merge or rebase) and Finish again — Hutan never \
                             force-pushes"
                                .into(),
                        ),
                        branch: "main".into(),
                        upstream: Some("origin/main".into()),
                        commit: None,
                        ahead: Some(1),
                        behind: Some(2),
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "worktree_finished_conflict",
            ControlMessage::ok(
                39,
                &WorktreeFinished {
                    commit: FinishCommit {
                        status: StepStatus::Skipped,
                        reason: Some(StepReason::NothingToCommit),
                        message: Some("/work/repo-wt/feature-login has nothing to commit".into()),
                        branch: "feature/login".into(),
                        commit: None,
                        files: 0,
                    },
                    merge: Some(FinishMerge {
                        status: StepStatus::Failed,
                        reason: Some(StepReason::Conflict),
                        message: Some(
                            "merging `feature/login` into `main` conflicts; nothing was changed"
                                .into(),
                        ),
                        branch: "feature/login".into(),
                        target: "main".into(),
                        strategy: MergeStrategy::Merge,
                        commit: None,
                        conflicts: vec!["src/login.rs".into()],
                        source_dirty: false,
                    }),
                    push: Some(FinishPush {
                        status: StepStatus::Skipped,
                        reason: Some(StepReason::EarlierStepFailed),
                        message: None,
                        branch: "main".into(),
                        upstream: None,
                        commit: None,
                        ahead: None,
                        behind: None,
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "error_worktree_finish_forbidden",
            ControlMessage::err(40, {
                let mut error = ErrorBody::new(
                    ErrorCode::Forbidden,
                    "`worktree.finish` is only available to the user, not to agents",
                );
                error.data = Some(serde_json::json!({"control": {"decision": "user_only"}}));
                error
            }),
        ),
        (
            "ui_layout_file_tab",
            ControlMessage::ok(41, &hutan_protocol::ui::UiLayoutResult {
                layout: hutan_protocol::ui::LayoutSummary {
                    panes: hutan_protocol::ui::PaneSummary::Pane {
                        pane_id: "pane-1".into(),
                        items: vec![hutan_protocol::ui::PaneItem {
                            kind: "file".into(),
                            session_id: None,
                            panel: None,
                            path: Some("/work/repo/src/main.rs".into()),
                            title: "main.rs".into(),
                        }],
                        active: Some(0),
                    },
                    ..layout_fixture()
                },
            })
            .unwrap(),
        ),
        (
            "chat_queue",
            ControlMessage::ok(42, &chat_queue_fixture(Some(QueueHold::AgentGone), false)).unwrap(),
        ),
        (
            "chat_queued",
            ControlMessage::ok(
                43,
                &ChatQueued {
                    item: QueuedMessage {
                        id: 3,
                        text: "Then run the tests".into(),
                        queued_at_ms: 1_760_000_002_000,
                    },
                },
            )
            .unwrap(),
        ),
        (
            "chat_sent_now_steered",
            ControlMessage::ok(44, &ChatSentNow::Steered { turn_id: turn() }).unwrap(),
        ),
        (
            "chat_sent_now_interrupting",
            ControlMessage::ok(45, &ChatSentNow::Interrupting).unwrap(),
        ),
        (
            "chat_sent_now_started",
            ControlMessage::ok(46, &ChatSentNow::Started { turn_id: turn() }).unwrap(),
        ),
        (
            "checkpoint_list",
            ControlMessage::ok(
                47,
                &CheckpointList {
                    checkpoints: vec![checkpoint_fixture()],
                    next: Some(checkpoint_id(2)),
                },
            )
            .unwrap(),
        ),
        (
            "checkpoint_diff",
            ControlMessage::ok(
                48,
                &CheckpointDiff {
                    files: vec![
                        CheckpointFile {
                            path: "assets/logo.png".into(),
                            status: CheckpointFileStatus::Added,
                            added: 0,
                            removed: 0,
                            binary: true,
                            truncated: false,
                        },
                        CheckpointFile {
                            path: "src/lib.rs".into(),
                            status: CheckpointFileStatus::Modified,
                            added: 1,
                            removed: 1,
                            binary: false,
                            truncated: false,
                        },
                    ],
                    patch: Some(
                        "diff --git a/assets/logo.png b/assets/logo.png\nnew file mode 100644\n\
                         index 0000000..3f78685\nBinary files /dev/null and b/assets/logo.png differ\n\
                         diff --git a/src/lib.rs b/src/lib.rs\nindex 3f78685..4b825dc 100644\n\
                         --- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn b() {}\n"
                            .into(),
                    ),
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "checkpoint_diff_stat_truncated",
            ControlMessage::ok(
                49,
                &CheckpointDiff {
                    files: vec![CheckpointFile {
                        path: "data/big.csv".into(),
                        status: CheckpointFileStatus::Deleted,
                        added: 0,
                        removed: 90_000,
                        binary: false,
                        truncated: true,
                    }],
                    patch: None,
                    truncated: true,
                },
            )
            .unwrap(),
        ),
        (
            "checkpoint_removed",
            ControlMessage::ok(
                50,
                &CheckpointRemoved {
                    removed: vec![checkpoint_id(1), checkpoint_id(2)],
                },
            )
            .unwrap(),
        ),
        (
            "chat_session_result_checkpoints_unsupported",
            ControlMessage::ok(
                51,
                &SessionResult {
                    session: SessionInfo {
                        chat: harness_chat_session_info().chat.map(|chat| ChatInfo {
                            checkpoints: Some(CheckpointStatus::unsupported(
                                "/Users/me/notes is not in a git repository",
                            )),
                            ..chat
                        }),
                        ..harness_chat_session_info()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "checkpoint_preview",
            ControlMessage::ok(52, &checkpoint_preview()).unwrap(),
        ),
        (
            "checkpoint_restored",
            ControlMessage::ok(
                53,
                &CheckpointRestored {
                    checkpoint: CheckpointInfo {
                        id: checkpoint_id(3),
                        turn_id: None,
                        kind: CheckpointKind::Restore,
                        files_changed: 2,
                        added: 4,
                        removed: 9,
                        bytes: 0,
                        skipped: vec![],
                        ..checkpoint_fixture()
                    },
                    written: vec!["src/main.rs".into()],
                    deleted: vec!["src/new.rs".into()],
                    skipped: restore_entries()[..2].to_vec(),
                },
            )
            .unwrap(),
        ),
        (
            "error_checkpoint_restore_stale",
            ControlMessage::err(54, {
                let mut error = ErrorBody::new(
                    ErrorCode::Conflict,
                    "the checkout changed since the preview: nothing was restored; review the new \
                     plan",
                );
                error.data = Some(serde_json::to_value(checkpoint_preview()).unwrap());
                error
            }),
        ),
        (
            "doc_opened",
            ControlMessage::ok(
                55,
                &DocOpened {
                    doc_id: "d1".into(),
                    owner: true,
                    language: Some("rust".into()),
                    servers: vec![DocServer {
                        server_id: "rust-analyzer:/Users/me/repo".into(),
                        name: "rust-analyzer".into(),
                        state: LspServerState::Starting,
                        reason: None,
                        install: None,
                        setting: None,
                        trust_pending: false,
                    }],
                    reason: None,
                },
            )
            .unwrap(),
        ),
        (
            "doc_opened_not_installed",
            ControlMessage::ok(
                56,
                &DocOpened {
                    doc_id: "d2".into(),
                    owner: false,
                    language: Some("rust".into()),
                    servers: vec![DocServer {
                        server_id: "rust-analyzer:/Users/me/repo".into(),
                        name: "rust-analyzer".into(),
                        state: LspServerState::NotInstalled,
                        reason: Some("`rust-analyzer` was not found on the login PATH".into()),
                        install: Some("rustup component add rust-analyzer".into()),
                        setting: Some("lsp.servers.rust-analyzer.command".into()),
                        trust_pending: false,
                    }],
                    reason: None,
                },
            )
            .unwrap(),
        ),
        (
            "doc_opened_no_server",
            ControlMessage::ok(
                57,
                &DocOpened {
                    doc_id: "d3".into(),
                    owner: true,
                    language: None,
                    servers: vec![],
                    reason: Some("no language server is configured for `.txt` files".into()),
                },
            )
            .unwrap(),
        ),
        (
            "lsp_status",
            ControlMessage::ok(
                58,
                &LspStatus {
                    enabled: true,
                    trust_policy: LspTrustPolicy::Ask,
                    max_servers: 4,
                    servers: vec![
                        lsp_server_info(LspServerState::Ready),
                        lsp_untrusted(),
                        lsp_not_installed(),
                    ],
                    trust: vec![LspTrustEntry {
                        folder: "/Users/me/repo".into(),
                        decision: LspTrust::Trusted,
                        decided_at_ms: 1_791_894_170_000,
                        decided_by: "cli".into(),
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "lsp_diagnostics",
            ControlMessage::ok(
                59,
                &LspDiagnosticsResult {
                    files: vec![LspFileDiagnostics {
                        path: "/Users/me/repo/src/main.rs".into(),
                        version: Some(4),
                        server_id: "rust-analyzer:/Users/me/repo".into(),
                        diagnostics: vec![lsp_diagnostic()],
                        truncated: false,
                    }],
                    truncated: false,
                    stale: true,
                    servers: vec![LspServerBrief {
                        server_id: "rust-analyzer:/Users/me/repo".into(),
                        name: "rust-analyzer".into(),
                        state: LspServerState::Indexing,
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "lsp_hover",
            ControlMessage::ok(
                60,
                &LspHover {
                    contents: Some("```rust\nfn main()\n```".into()),
                    range: Some(LspRange {
                        start: LspPosition {
                            line: 3,
                            character: 4,
                        },
                        end: LspPosition {
                            line: 3,
                            character: 8,
                        },
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "lsp_locations",
            ControlMessage::ok(
                61,
                &LspLocations {
                    locations: vec![LspLocation {
                        path: "/Users/me/repo/src/lib.rs".into(),
                        range: LspRange {
                            start: LspPosition {
                                line: 10,
                                character: 7,
                            },
                            end: LspPosition {
                                line: 10,
                                character: 12,
                            },
                        },
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "lsp_trust_result",
            ControlMessage::ok(
                62,
                &LspTrustResult {
                    folder: "/Users/me/repo".into(),
                    decision: LspTrust::Trusted,
                    servers: vec![lsp_server_info(LspServerState::Starting)],
                },
            )
            .unwrap(),
        ),
        (
            "error_lsp_trust_forbidden",
            ControlMessage::err(
                63,
                ErrorBody::new(
                    ErrorCode::Forbidden,
                    "`lsp.trust` is the user's alone: an agent cannot trust a folder",
                ),
            ),
        ),
        (
            "checkpoint_list_restores",
            ControlMessage::ok(
                64,
                &CheckpointList {
                    checkpoints: vec![
                        CheckpointInfo {
                            restores_through: None,
                            ..restore_checkpoint_fixture()
                        },
                        checkpoint_fixture(),
                    ],
                    next: None,
                },
            )
            .unwrap(),
        ),
        (
            "lsp_completion_list",
            ControlMessage::ok(
                65,
                &LspCompletionList {
                    completion_id: 7,
                    items: vec![
                        LspCompletionItem {
                            label: "push".into(),
                            kind: Some(LspCompletionKind::Method),
                            detail: Some("fn(&mut self, value: T)".into()),
                            documentation: None,
                            sort_text: Some("0001".into()),
                            filter_text: None,
                            insert_text: None,
                            snippet: true,
                            edit: Some(LspTextEdit {
                                range: LspRange {
                                    start: LspPosition {
                                        line: 3,
                                        character: 6,
                                    },
                                    end: LspPosition {
                                        line: 3,
                                        character: 8,
                                    },
                                },
                                new_text: "push(${1:value})$0".into(),
                            }),
                            additional_edits: vec![],
                            preselect: true,
                            deprecated: false,
                            resolvable: true,
                        },
                        LspCompletionItem {
                            label: "HashMap".into(),
                            kind: Some(LspCompletionKind::Struct),
                            detail: None,
                            documentation: Some("A hash map.".into()),
                            sort_text: None,
                            filter_text: Some("HashMap".into()),
                            insert_text: Some("HashMap".into()),
                            snippet: false,
                            edit: None,
                            additional_edits: vec![LspTextEdit {
                                range: LspRange {
                                    start: LspPosition {
                                        line: 0,
                                        character: 0,
                                    },
                                    end: LspPosition {
                                        line: 0,
                                        character: 0,
                                    },
                                },
                                new_text: "use std::collections::HashMap;\n".into(),
                            }],
                            preselect: false,
                            deprecated: true,
                            resolvable: false,
                        },
                    ],
                    incomplete: true,
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "lsp_signature_help",
            ControlMessage::ok(
                66,
                &LspSignatureHelp {
                    signatures: vec![LspSignature {
                        label: "fn insert(&mut self, k: K, v: V) -> Option<V>".into(),
                        documentation: Some("Inserts a key-value pair.".into()),
                        parameters: vec![
                            LspParameter {
                                start: 10,
                                end: 19,
                                documentation: None,
                            },
                            LspParameter {
                                start: 21,
                                end: 25,
                                documentation: Some("the key".into()),
                            },
                            LspParameter {
                                start: 27,
                                end: 31,
                                documentation: None,
                            },
                        ],
                        active_parameter: Some(1),
                    }],
                    active_signature: 0,
                },
            )
            .unwrap(),
        ),
        (
            "lsp_server_info_triggers",
            ControlMessage::ok(
                67,
                &LspServerInfo {
                    capabilities: vec![
                        "hover".into(),
                        "completion".into(),
                        "signature_help".into(),
                    ],
                    completion_triggers: vec![".".into(), ":".into()],
                    signature_triggers: vec!["(".into(), ",".into()],
                    ..lsp_server_info(LspServerState::Ready)
                },
            )
            .unwrap(),
        ),
        (
            "error_lsp_completion_superseded",
            ControlMessage::err(
                68,
                ErrorBody {
                    code: ErrorCode::Conflict,
                    message: "superseded by a newer `lsp.completion`".into(),
                    data: Some(serde_json::json!({"reason": "superseded"})),
                },
            ),
        ),
        (
            "hooks_list",
            ControlMessage::ok(
                40,
                &HooksList {
                    hooks: vec![
                        HookInfo {
                            index: 0,
                            name: Some("Phone".into()),
                            on: HookEvent::ApprovalRequested,
                            harness: None,
                            project: Some("hutan".into()),
                            workspace: None,
                            run: vec!["curl".into(), "-d".into(), "an approval waits".into()],
                            shell: false,
                            timeout_secs: 30,
                            when_focused: false,
                            runs: 4,
                            failures: 1,
                            dropped: 0,
                            skipped: 2,
                            last_run: Some(hook_run()),
                        },
                        HookInfo {
                            index: 1,
                            name: None,
                            on: HookEvent::SessionExited,
                            harness: Some("claude".into()),
                            project: None,
                            workspace: Some("work".into()),
                            run: vec!["echo done >> ~/journal.txt".into()],
                            shell: true,
                            timeout_secs: 300,
                            when_focused: true,
                            runs: 0,
                            failures: 0,
                            dropped: 3,
                            skipped: 0,
                            last_run: None,
                        },
                    ],
                    running: 1,
                    max_concurrent: 4,
                    queue_capacity: 8,
                },
            )
            .unwrap(),
        ),
        (
            "hooks_log",
            ControlMessage::ok(
                41,
                &HooksLog {
                    runs: vec![
                        hook_run(),
                        HookRun {
                            id: 6,
                            outcome: HookOutcome::TimedOut,
                            exit_code: None,
                            signal: None,
                            stdout: String::new(),
                            stderr: String::new(),
                            error: Some("killed after 30 s".into()),
                            ..hook_run()
                        },
                    ],
                    capacity: 200,
                },
            )
            .unwrap(),
        ),
        (
            "hooks_tested",
            ControlMessage::ok(
                42,
                &HooksTested {
                    run: HookRun {
                        test: true,
                        ..hook_run()
                    },
                    payload: HookPayload {
                        test: true,
                        ..hook_payload()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "hooks_tested_finish",
            ControlMessage::ok(
                43,
                &HooksTested {
                    run: HookRun {
                        test: true,
                        event: HookEvent::FinishDone,
                        session_id: None,
                        outcome: HookOutcome::Failed,
                        exit_code: Some(2),
                        stderr: "no such topic\n".into(),
                        ..hook_run()
                    },
                    payload: HookPayload {
                        test: true,
                        ..finish_payload()
                    },
                },
            )
            .unwrap(),
        ),
        (
            "usage_summary",
            ControlMessage::ok(7, &usage_summary_fixture()).unwrap(),
        ),
        (
            "usage_summary_empty",
            ControlMessage::ok(
                8,
                &UsageSummary {
                    generated_at_ms: 1_791_894_200_000,
                    day_start_ms: 1_791_849_600_000,
                    week_start_ms: 1_791_590_400_000,
                    harnesses: vec![],
                    providers: vec![],
                },
            )
            .unwrap(),
        ),
        (
            "welcome_identity",
            ControlMessage::ok(
                1,
                &Welcome {
                    protocol: PROTOCOL_VERSION,
                    daemon_version: "0.2.0".into(),
                    epoch: epoch(),
                    capabilities: vec!["daemon.identity".into(), "daemon.stopping".into()],
                    resume: ResumeOutcome::SnapshotRequired,
                    daemon_build: Some(DaemonBuild {
                        version: "0.2.0".into(),
                        git_sha: Some("0123456789ab".into()),
                        exe_path: "/Applications/Hutan.app/Contents/MacOS/hutand".into(),
                        started_at_ms: 1_791_894_170_000,
                    }),
                },
            )
            .unwrap(),
        ),
        (
            "project_git_initialized",
            ControlMessage::ok(
                34,
                &ProjectGitInitialized {
                    root: "/Users/me/Hutan".into(),
                    branch: "main".into(),
                    commit: "5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into(),
                    files: 12,
                    gitignore_written: true,
                },
            )
            .unwrap(),
        ),
        (
            "memory_imported",
            ControlMessage::ok(
                35,
                &MemoryImported {
                    created: 2,
                    updated: 1,
                    unchanged: 3,
                    proposals: vec![approval()],
                    skipped: vec![MemoryFileProblem {
                        file: "import".into(),
                        section: Some("line 14".into()),
                        reason: "Memory is not a secret store: the text looks like a credential".into(),
                        at_ms: 1_791_894_300_000,
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "memory_files_status",
            ControlMessage::ok(
                36,
                &MemoryFilesStatus {
                    scope: MemoryScope::Global,
                    workspace_id: None,
                    enabled: true,
                    folder: Some("/Users/me/Hutan/.mawas/memory".into()),
                    note: None,
                    files: vec![
                        MemoryFileInfo {
                            name: "facts.md".into(),
                            bytes: 412,
                        },
                        MemoryFileInfo {
                            name: "profile.md".into(),
                            bytes: 230,
                        },
                    ],
                    last_write_ms: Some(1_791_894_290_000),
                    tracked: true,
                    pending: 1,
                    problems: vec![MemoryFileProblem {
                        file: "notes.md".into(),
                        section: Some("01K7F3B5N2Q9W8E4R6T1Y0V3KP".into()),
                        reason: "the text is longer than 2 KiB".into(),
                        at_ms: 1_791_894_295_000,
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "git_log",
            ControlMessage::ok(
                35,
                &GitLogResult {
                    root: "/Users/me/proj/hutan".into(),
                    commits: vec![
                        GitCommit {
                            id: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                            author: "Ada Lovelace".into(),
                            email: "ada@example.com".into(),
                            time: 1_791_894_170,
                            subject: "feat: add the engine".into(),
                            merge: false,
                            path: None,
                            old_path: None,
                        },
                        GitCommit {
                            id: "5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into(),
                            author: "Grace Hopper".into(),
                            email: "grace@example.com".into(),
                            time: 1_791_800_000,
                            subject: "Merge branch 'feature'".into(),
                            merge: true,
                            path: None,
                            old_path: None,
                        },
                    ],
                    more: true,
                    next_before: Some("5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into()),
                },
            )
            .unwrap(),
        ),
        (
            "memory_files_status_off",
            ControlMessage::ok(
                37,
                &MemoryFilesStatus {
                    scope: MemoryScope::Workspace,
                    workspace_id: Some(ws()),
                    enabled: false,
                    folder: None,
                    note: Some("Turn on `assistant.memory_files_in_project` to keep this workspace's memory in its project.".into()),
                    files: vec![],
                    last_write_ms: None,
                    tracked: false,
                    pending: 0,
                    problems: vec![],
                },
            )
            .unwrap(),
        ),
        (
            "git_log_file_renamed",
            ControlMessage::ok(
                36,
                &GitLogResult {
                    root: "/Users/me/proj/hutan".into(),
                    commits: vec![GitCommit {
                        id: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                        author: "Ada Lovelace".into(),
                        email: "ada@example.com".into(),
                        time: 1_791_894_170,
                        subject: "refactor: rename old to new".into(),
                        merge: false,
                        path: Some("src/new.rs".into()),
                        old_path: Some("src/old.rs".into()),
                    }],
                    more: false,
                    next_before: None,
                },
            )
            .unwrap(),
        ),
        (
            "git_blame",
            ControlMessage::ok(
                37,
                &GitBlameResult {
                    root: "/Users/me/proj/hutan".into(),
                    path: "src/lib.rs".into(),
                    rev: None,
                    lines: 6,
                    hunks: vec![
                        BlameHunk {
                            line: 1,
                            count: 3,
                            commit: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                        },
                        BlameHunk {
                            line: 4,
                            count: 1,
                            commit: GIT_UNCOMMITTED.into(),
                        },
                        BlameHunk {
                            line: 5,
                            count: 2,
                            commit: "5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into(),
                        },
                    ],
                    commits: vec![
                        BlameCommit {
                            id: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                            author: "Ada Lovelace".into(),
                            email: "ada@example.com".into(),
                            time: 1_791_894_170,
                            subject: "feat: add the engine".into(),
                            path: "src/old.rs".into(),
                            boundary: false,
                        },
                        BlameCommit {
                            id: GIT_UNCOMMITTED.into(),
                            author: "Not Committed Yet".into(),
                            email: "not.committed.yet".into(),
                            time: 1_791_900_000,
                            subject: String::new(),
                            path: "src/lib.rs".into(),
                            boundary: false,
                        },
                        BlameCommit {
                            id: "5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into(),
                            author: "Grace Hopper".into(),
                            email: "grace@example.com".into(),
                            time: 1_791_800_000,
                            subject: "Initial commit".into(),
                            path: "src/lib.rs".into(),
                            boundary: true,
                        },
                    ],
                    truncated: true,
                },
            )
            .unwrap(),
        ),
        (
            "memory_proposal_from_file",
            ControlMessage::ok(
                38,
                &MemoryProposals {
                    proposals: vec![MemoryProposal {
                        provenance: MemoryProvenance {
                            author: MemoryAuthor::User,
                            client: None,
                            session_id: None,
                            turn_id: None,
                            at_ms: 1_791_894_200_000,
                            origin: Some(ORIGIN_FILE.into()),
                        },
                        ..memory_proposal()
                    }],
                },
            )
            .unwrap(),
        ),
        (
            "git_show",
            ControlMessage::ok(
                38,
                &GitShowResult {
                    root: "/Users/me/proj/hutan".into(),
                    commit: GitCommit {
                        id: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                        author: "Ada Lovelace".into(),
                        email: "ada@example.com".into(),
                        time: 1_791_894_170,
                        subject: "fix: the greeting".into(),
                        merge: false,
                        path: None,
                        old_path: None,
                    },
                    message: "fix: the greeting\n\nIt said old.\n".into(),
                    parent: Some("5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d".into()),
                    files: vec![DiffFile {
                        path: "README.md".into(),
                        old_path: None,
                        status: DiffStatus::Modified,
                        additions: 1,
                        deletions: 1,
                        binary: false,
                        committed: true,
                        uncommitted: false,
                        staged: false,
                        unstaged: false,
                    }],
                    patch: "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n".into(),
                    truncated: false,
                },
            )
            .unwrap(),
        ),
        (
            "git_file",
            ControlMessage::ok(
                39,
                &GitFileResult {
                    root: "/Users/me/proj/hutan".into(),
                    path: "src/lib.rs".into(),
                    rev: "9d8c7b6a5f4e3d2c1b0a99887766554433221100".into(),
                    size: 14,
                    content: "pub fn hi() {}".into(),
                    binary: false,
                    too_large: false,
                },
            )
            .unwrap(),
        ),
    ];
    for (name, message) in cases {
        let json = String::from_utf8(message.to_json().unwrap()).unwrap();
        check_golden(&format!("response_{name}.json"), &json);
        assert_eq!(
            ControlMessage::from_json(json.as_bytes()).unwrap(),
            message,
            "{name}"
        );
    }
}

#[test]
fn render_frames_match_fixtures_and_roundtrip() {
    for (name, render) in renders() {
        let frame = Frame::terminal_render(TID, &render).unwrap();
        let hex: String = frame.payload.iter().map(|b| format!("{b:02x}")).collect();
        check_golden(&format!("{name}.hex"), &hex);
        assert_eq!(
            frame.decode_terminal_render().unwrap(),
            (TID, render),
            "{name}"
        );
    }
}

#[test]
fn unknown_method_and_bad_params_are_bad_requests() {
    let err = Request::decode("session.teleport", serde_json::json!({})).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let err = Request::decode("terminal.ack", serde_json::json!({"terminal_id": 1})).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn unknown_events_are_ignored_not_errors() {
    assert_eq!(
        Event::decode("agent.teleported", serde_json::json!({"x": 1})).unwrap(),
        None
    );
}

#[test]
fn workspaces_of_an_older_daemon_decode_with_defaults() {
    // Protocol 0.6 and older sent only these three fields.
    let old = serde_json::json!({"id": ws(), "name": "legacy", "roots": ["/a"]});
    let decoded: WorkspaceInfo = serde_json::from_value(old).unwrap();
    assert_eq!(
        decoded,
        WorkspaceInfo {
            id: ws(),
            name: "legacy".into(),
            roots: vec!["/a".into()],
            kind: WorkspaceKind::Project,
            pinned: false,
            position: 0,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: None,
            project: None,
        }
    );
}

#[test]
fn tasks_of_an_older_daemon_have_no_delivery() {
    // Protocol 0.23 and older knew nothing of delivery or dependency policies.
    let old = serde_json::json!({
        "id": tid(), "workspace_id": ws(), "title": "Old", "state": "done",
        "depends_on": [], "placement": {"kind": "main"}, "sessions": [],
        "created_at_ms": 1, "auto_start": false
    });
    let decoded: Task = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.delivery, DeliveryState::None);
    assert_eq!(decoded.after_policy, AfterPolicy::Done);
    assert!(decoded.delivery_history.is_empty());
    // And a 0.23 daemon's worktree.diff has no scope information.
    let old = serde_json::json!({
        "base": "main", "merge_base": "abc",
        "files": [{"path": "a", "status": "modified", "additions": 1, "deletions": 0}]
    });
    let diff: WorktreeDiff = serde_json::from_value(old).unwrap();
    assert!(diff.commits.is_empty() && !diff.files[0].committed && !diff.files[0].uncommitted);
}

#[test]
fn sessions_of_an_older_daemon_have_no_chat_info() {
    let old = serde_json::json!({
        "id": sid(), "workspace_id": ws(), "kind": "terminal", "title": "t",
        "state": {"state": "running"}, "command": [], "cwd": null, "terminal_id": 1,
        "created_at_ms": 1
    });
    let decoded: SessionInfo = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.chat, None);
}

#[test]
fn sessions_of_an_older_daemon_have_no_placement() {
    let old = serde_json::json!({
        "id": sid(), "workspace_id": ws(), "kind": "terminal", "title": "t",
        "state": {"state": "running"}, "command": [], "cwd": null, "terminal_id": 1,
        "created_at_ms": 1
    });
    let decoded: SessionInfo = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.placement, None);
    assert_eq!(decoded.folder, None);
}

#[test]
fn workspace_create_still_reads_roots_from_an_older_client() {
    let old = serde_json::json!({"name": "legacy", "roots": ["/a", "/b"]});
    let decoded: WorkspaceCreate = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.paths, [PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(decoded.project_ids.is_empty());
}

#[test]
fn history_requests_default_their_limit() {
    let request: ChatHistoryRequest =
        serde_json::from_value(serde_json::json!({"session_id": sid()})).unwrap();
    assert_eq!(request.limit, HISTORY_DEFAULT_LIMIT);
    assert_eq!(request.before_seq, None);
}

#[test]
fn events_of_a_newer_agent_decode_as_opaque_kinds_only_when_known() {
    // A `chat.event` whose kind this build does not know is a decode error for that event
    // (the client ignores it), never a panic.
    let data = serde_json::json!({
        "session_id": sid(),
        "event": {"session_id": sid(), "seq": 1, "ts_ms": 1, "type": "from_the_future"}
    });
    assert!(Event::decode("chat.event", data).is_err());
}

#[test]
fn a_credential_request_never_prints_its_secret() {
    let request = Request::ProviderSetCredential(ProviderSetCredential {
        provider_id: "openrouter".into(),
        secret: "super-secret-value".into(),
    });
    let printed = format!("{request:?}");
    assert!(!printed.contains("super-secret-value"), "{printed}");
    assert!(printed.contains("openrouter"));
}

fn has_key(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key(key) || map.values().any(|v| has_key(v, key))
        }
        serde_json::Value::Array(items) => items.iter().any(|v| has_key(v, key)),
        _ => false,
    }
}

#[test]
fn no_response_or_event_fixture_has_a_secret_field() {
    // Only `provider.set_credential` carries one, and only as a request.
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("request_") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            assert!(!has_key(&value, "secret"), "{name} has a secret field");
        }
        assert!(!text.contains("not-a-real-key"), "{name}");
    }
}

#[test]
fn fs_requests_default_what_they_omit() {
    let list = Request::decode("fs.list", serde_json::json!({"dir": "/repo"})).unwrap();
    assert_eq!(
        list,
        Request::FsList(FsList {
            dir: "/repo".into(),
            ignore: true,
            limit: None,
            picked: false,
        })
    );
    let read = Request::decode("fs.read", serde_json::json!({"path": "/repo/a"})).unwrap();
    assert_eq!(
        read,
        Request::FsRead(FsRead {
            path: "/repo/a".into(),
            offset: None,
            max_bytes: None,
            picked: false,
        })
    );
}

#[test]
fn only_fs_resource_and_diagnostics_events_are_connection_local() {
    for (name, event) in events() {
        assert_eq!(
            event.is_connection_local(),
            name.starts_with("fs_")
                || name.starts_with("resources_updated")
                || name.starts_with("lsp_diagnostics"),
            "{name}"
        );
    }
}
