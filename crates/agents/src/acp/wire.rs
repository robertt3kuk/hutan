//! The parts of ACP v1 (https://agentclientprotocol.com) the client reads and writes. Structs
//! list only the fields Hutan uses and tolerate everything else (`session/update` itself is
//! read as JSON by the mapper, which maps what it knows and keeps the rest as opaque events).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// JSON-RPC code of "authentication required" in ACP.
pub const AUTH_REQUIRED: i64 = -32000;

// Requests we send ------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u64,
    pub client_capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

/// Hutan serves no file system and no terminal to the agent: the agent uses its own tools
/// (ADR 0015), and Hutan's approvals gate them. It does answer form-mode elicitations
/// (`elicitation/create`): that is what turns on OMP's `ask` tool and Claude Code's
/// `AskUserQuestion`, and what makes OMP ask before it leaves plan mode (`docs/agents.md`
/// § Questions).
#[derive(Debug, Serialize)]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
    pub elicitation: ElicitationCapabilities,
}

#[derive(Debug, Serialize)]
pub struct ElicitationCapabilities {
    /// `{}`: form mode.
    pub form: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: &'static str,
    pub title: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticateParams<'a> {
    pub method_id: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams<'a> {
    pub cwd: &'a str,
    pub mcp_servers: &'a [Value],
    /// Only for an agent that advertised `sessionCapabilities.additionalDirectories`.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub additional_directories: &'a [String],
    /// Extension metadata the agent reads (`_meta`).
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<&'a Value>,
}

/// `session/load` and `session/resume`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionParams<'a> {
    pub session_id: &'a str,
    pub cwd: &'a str,
    pub mcp_servers: &'a [Value],
    /// ACP: omitted on load and resume means none, so the list is sent again every time.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub additional_directories: &'a [String],
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<&'a Value>,
}

/// `session/fork`: a new session with the history of `session_id` (the agent finds it in its
/// own storage, so the parent need not be loaded). The answer is a [`SessionResult`] with the
/// new `sessionId`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionParams<'a> {
    pub session_id: &'a str,
    pub cwd: &'a str,
    pub mcp_servers: &'a [Value],
    /// The fork's own folders, for an agent that advertised `additionalDirectories` (the list
    /// is sent again at every open, a fork's included).
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub additional_directories: &'a [String],
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<&'a Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigOptionParams<'a> {
    pub session_id: &'a str,
    pub config_id: &'a str,
    pub value: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModeParams<'a> {
    pub session_id: &'a str,
    pub mode_id: &'a str,
}

// Results we read -------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: u64,
    #[serde(default)]
    pub agent_info: Option<AgentInfo>,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthMethod {
    pub id: String,
    /// Absent for the plain "agent authenticates by itself" method; `terminal` and `env_var`
    /// methods need a client Hutan does not offer.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
}

#[derive(Debug, Default, Deserialize)]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub embedded_context: bool,
    #[serde(default)]
    pub image: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct SessionCapabilities {
    #[serde(default)]
    pub resume: Option<Value>,
    #[serde(default)]
    pub close: Option<Value>,
    /// `session/fork` (an ACP unstable method both OMP and the Claude Code adapter offer).
    #[serde(default)]
    pub fork: Option<Value>,
    /// The agent takes `additionalDirectories` on `session/new|load|resume`.
    #[serde(default, rename = "additionalDirectories")]
    pub additional_directories: Option<Value>,
}

/// The session a `session/new`, `session/load` or `session/resume` opened.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    /// Only `session/new` answers with it.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub config_options: Vec<ConfigOption>,
    #[serde(default)]
    pub modes: Option<ModeState>,
    /// The earlier `models` state some agents still send.
    #[serde(default)]
    pub models: Option<ModelState>,
}

/// A session setting the agent offers (`select` type): the model, the mode, the thinking level.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    pub id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub current_value: Value,
    /// Choices, flat or in groups.
    #[serde(default)]
    pub options: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeState {
    #[serde(default)]
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<Mode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Mode {
    pub id: String,
    /// The display name (`Plan Mode`); empty when the agent sent none.
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelState {
    #[serde(default)]
    pub current_model_id: String,
    #[serde(default)]
    pub available_models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub model_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    #[serde(default)]
    pub stop_reason: String,
    /// Per-turn token counts (an ACP extension some agents send).
    #[serde(default)]
    pub usage: Option<PromptUsage>,
    /// Claude Code's adapter also sends the turn's counts as `_meta.quota.token_count`; it is
    /// read when `usage` is absent.
    #[serde(default, rename = "_meta")]
    pub meta: Option<PromptMeta>,
}

#[derive(Debug, Deserialize)]
pub struct PromptMeta {
    #[serde(default)]
    pub quota: Option<PromptQuota>,
}

#[derive(Debug, Deserialize)]
pub struct PromptQuota {
    #[serde(default)]
    pub token_count: Option<PromptUsage>,
}

impl PromptResult {
    /// The turn's own counts: the prompt's `usage`, else the adapter's `_meta.quota`.
    pub fn counts(&self) -> Option<&PromptUsage> {
        self.usage
            .as_ref()
            .or_else(|| self.meta.as_ref()?.quota.as_ref()?.token_count.as_ref())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub thought_tokens: Option<u64>,
    #[serde(default)]
    pub cached_read_tokens: Option<u64>,
    #[serde(default)]
    pub cached_write_tokens: Option<u64>,
}
