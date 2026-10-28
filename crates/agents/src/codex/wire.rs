//! Typed structs for the Codex app-server messages Hutan uses, written against the schema
//! `codex app-server generate-json-schema` produces for 0.162.1 (`docs/agents.md` explains how
//! to regenerate and compare). Every field the adapter does not read is left out, and every
//! field the server may omit is optional: the structs must accept what newer servers add.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Requests we send -------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams<'a> {
    pub client_info: ClientInfo<'a>,
    pub capabilities: InitializeCapabilities,
}

#[derive(Debug, Serialize)]
pub struct ClientInfo<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub version: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    /// Opt out of the experimental surface: the adapter only uses stable methods.
    pub experimental_api: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams<'a> {
    pub model: &'a str,
    pub cwd: &'a str,
    pub approval_policy: &'a str,
    pub sandbox: &'a str,
    /// Approvals go to the client (us), never to Codex's own reviewer.
    pub approvals_reviewer: &'static str,
    /// Added to the thread's instructions; `baseInstructions` is left alone so that Codex keeps
    /// its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    pub cwd: &'a str,
    pub approval_policy: &'a str,
    pub sandbox: &'a str,
    pub approvals_reviewer: &'static str,
    /// Hutan's journal is the history; the provider's copy is not sent back.
    pub exclude_turns: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<&'a str>,
}

/// `thread/fork` (0.162.1 `ThreadForkParams`): a new thread with the history of `thread_id`
/// through `last_turn_id` (inclusive; omitted: all of it). The server answers like
/// `thread/start`: [`ThreadResult`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    pub cwd: &'a str,
    pub approval_policy: &'a str,
    pub sandbox: &'a str,
    pub approvals_reviewer: &'static str,
    /// Hutan's journal is the history; the provider's copy is not sent back.
    pub exclude_turns: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams<'a> {
    pub thread_id: &'a str,
    pub input: Vec<UserInput<'a>>,
    /// "Override for this turn and subsequent turns" (the schema's words): how the session's
    /// extra folders reach a `workspace-write` thread (`{type: workspaceWrite, writableRoots}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput<'a> {
    Text {
        text: &'a str,
        text_elements: Vec<Value>,
    },
    /// An image the app-server reads from disk (protocol 0.23).
    LocalImage { path: &'a str },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams<'a> {
    pub thread_id: &'a str,
    pub turn_id: &'a str,
}

/// `turn/steer`: more user input for the turn in progress, taken at its next step; fails when
/// `expected_turn_id` is not the active turn.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams<'a> {
    pub thread_id: &'a str,
    pub input: Vec<UserInput<'a>>,
    pub expected_turn_id: &'a str,
}

// Responses ------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// `<originator>/<codex version> (<platform>) ...`
    pub user_agent: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResult {
    pub data: Vec<ModelWire>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelWire {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResult {
    pub thread: ThreadRef,
    #[serde(default)]
    pub model: Option<String>,
    /// What the server enforces for the thread: its approval policy (a name) and sandbox
    /// (`{type: readOnly | workspaceWrite | dangerFullAccess, networkAccess?}`), as 0.162.1
    /// answers `thread/start` and `thread/resume`. Kept as values: a newer server may shape
    /// them differently, and the daemon then says it could not confirm.
    #[serde(default)]
    pub approval_policy: Option<serde_json::Value>,
    #[serde(default)]
    pub sandbox: Option<serde_json::Value>,
}

impl ThreadResult {
    /// The safety the server reported, in Hutan's names.
    pub fn reported_safety(&self) -> Option<crate::adapter::ReportedSafety> {
        let approval_policy = self
            .approval_policy
            .as_ref()
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let sandbox = self.sandbox.as_ref();
        let kind = sandbox
            .and_then(|s| s.get("type"))
            .and_then(|t| t.as_str())
            .and_then(|t| match t {
                "readOnly" => Some("read-only"),
                "workspaceWrite" => Some("workspace-write"),
                "dangerFullAccess" => Some("danger-full-access"),
                _ => None,
            })
            .map(str::to_owned);
        let network = sandbox
            .and_then(|s| s.get("networkAccess"))
            .and_then(|n| n.as_bool());
        if approval_policy.is_none() && kind.is_none() && network.is_none() {
            return None;
        }
        Some(crate::adapter::ReportedSafety {
            approval_policy,
            sandbox: kind,
            network,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ThreadRef {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct TurnStartResult {
    pub turn: TurnRefWire,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResult {
    pub turn_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TurnRefWire {
    pub id: String,
}

// Notifications --------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub turn: TurnWire,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnWire {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<TurnError>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub turn_id: String,
    pub item: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaNotification {
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressNotification {
    pub turn_id: String,
    pub item_id: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchUpdatedNotification {
    pub turn_id: String,
    pub item_id: String,
    pub changes: Vec<FileUpdateChange>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiffNotification {
    pub turn_id: String,
    pub diff: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanNotification {
    pub turn_id: String,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub plan: Vec<PlanStepWire>,
}

#[derive(Debug, Deserialize)]
pub struct PlanStepWire {
    pub step: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageNotification {
    #[serde(default)]
    pub turn_id: Option<String>,
    pub token_usage: ThreadTokenUsage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    #[serde(default)]
    pub total: Option<TokenBreakdown>,
    #[serde(default)]
    pub last: Option<TokenBreakdown>,
    #[serde(default)]
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, rename = "cacheWriteInputTokens")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub error: TurnError,
    #[serde(default)]
    pub will_retry: bool,
}

#[derive(Debug, Deserialize)]
pub struct WarningNotification {
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolved {
    pub request_id: Value,
}

// Items ----------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UserMessageItem {
    pub id: String,
    #[serde(default)]
    pub content: Vec<UserInputWire>,
}

#[derive(Debug, Deserialize)]
pub struct UserInputWire {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentMessageItem {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub phase: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReasoningItem {
    pub id: String,
    #[serde(default)]
    pub summary: Vec<String>,
    #[serde(default)]
    pub content: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionItem {
    pub id: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub command_actions: Vec<Value>,
    #[serde(default)]
    pub aggregated_output: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FileChangeItem {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub changes: Vec<FileUpdateChange>,
}

#[derive(Debug, Deserialize)]
pub struct FileUpdateChange {
    pub path: String,
    #[serde(default)]
    pub diff: String,
    pub kind: PatchChangeKind,
}

#[derive(Debug, Deserialize)]
pub struct PatchChangeKind {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub move_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCallItem {
    pub id: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallItem {
    pub id: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct WebSearchItem {
    pub id: String,
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Deserialize)]
pub struct PlanItem {
    pub id: String,
    #[serde(default)]
    pub text: String,
}

// Requests the server makes ----------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApprovalParams {
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Not in the 0.162.1 schema, but sent by the server: the decisions it accepts.
    #[serde(default)]
    pub available_decisions: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApprovalParams {
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub grant_root: Option<String>,
    #[serde(default)]
    pub available_decisions: Option<Vec<Value>>,
}

/// `item/tool/requestUserInput` (EXPERIMENTAL in the 0.162 schema).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInputParams {
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub questions: Vec<ToolInputQuestion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInputQuestion {
    pub id: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub question: String,
    /// The user may type an answer that is none of the options.
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub options: Option<Vec<ToolInputOption>>,
}

#[derive(Debug, Deserialize)]
pub struct ToolInputOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// `mcpServer/elicitation/request`: an MCP server asks the user something (`mode` `form`,
/// `openai/form`, `openaiForm` or `url`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationParams {
    #[serde(default)]
    pub server_name: String,
    /// Absent when app-server could not tie the request to a turn.
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub requested_schema: Option<Value>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsApprovalParams {
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub permissions: Value,
}
