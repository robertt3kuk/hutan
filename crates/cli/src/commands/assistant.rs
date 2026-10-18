//! `hutan ask`, `hutan assistant ls|open`: the assistant sessions of the daemon
//! (`docs/assistant.md`).

use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::assistant::{
    AssistantEntry, AssistantList, AssistantListRequest, AssistantOpen, AssistantScope,
};
use hutan_protocol::messages::{
    Empty, Request, SessionInfo, SessionResult, Subscribed, WorkspaceKind, WorkspaceList,
};
use serde_json::json;

use super::Ctx;
use super::workspaces::{resolve_workspace, workspace_target};
use crate::args::{
    AskArgs, AssistantCommand, AssistantLsArgs, AssistantOpenArgs, AssistantResetArgs,
};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn assistant(&self, command: AssistantCommand) -> Result<(), CliError> {
        match command {
            AssistantCommand::Ls(args) => self.assistant_ls(args).await,
            AssistantCommand::Open(args) => self.assistant_open(args).await,
            AssistantCommand::Reset(args) => self.assistant_reset(args).await,
        }
    }

    /// `--ws`, else `$HUTAN_WORKSPACE`, as a scope: the Home workspace and no workspace at all
    /// are the global assistant, any other workspace its own.
    async fn assistant_scope(
        &self,
        client: &Client,
        workspace: Option<String>,
    ) -> Result<(AssistantScope, Option<WorkspaceId>), CliError> {
        let Some((target, _)) = workspace_target(workspace) else {
            return Ok((AssistantScope::Global, None));
        };
        let all: WorkspaceList = self.call(client, Request::WorkspaceList(Empty {})).await?;
        let workspace = resolve_workspace(&all.workspaces, &target)?;
        if workspace.kind == WorkspaceKind::Home {
            Ok((AssistantScope::Global, None))
        } else {
            Ok((AssistantScope::Workspace, Some(workspace.id)))
        }
    }

    async fn open_assistant(
        &self,
        client: &Client,
        workspace: Option<String>,
        new: bool,
    ) -> Result<SessionInfo, CliError> {
        let (scope, workspace_id) = self.assistant_scope(client, workspace).await?;
        client
            .assistant_open(AssistantOpen {
                scope,
                workspace_id,
                new,
            })
            .await
            .map_err(|e| self.fail(e))
    }

    /// Sends the question with its attachments to the scope's default assistant and prints the
    /// answer as it streams; exits 1 when the turn fails.
    pub(super) async fn ask(&self, args: AskArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.open_assistant(&client, args.workspace, false).await?;
        // Subscribed before sending, so no event of the turn is missed.
        let mut events = client.events();
        let _: Subscribed = self
            .call(&client, Request::EventsSubscribe(Empty {}))
            .await?;
        let sent = client
            .chat_send_with(session.id, args.question, args.attach)
            .await
            .map_err(|e| self.fail(e))?;
        self.follow_turn(&mut events, session.id, sent.turn_id)
            .await
    }

    async fn assistant_open(&self, args: AssistantOpenArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self
            .open_assistant(&client, args.workspace, args.new)
            .await?;
        let result = SessionResult { session };
        self.print(&result, || {
            let session = &result.session;
            let model = session.chat.as_ref().map_or_else(
                || "?".to_owned(),
                |chat| {
                    format!(
                        "{} / {}",
                        chat.provider_id.as_deref().unwrap_or(&chat.agent),
                        chat.model
                    )
                },
            );
            format!("session {} assistant {model}", session.id)
        })
    }

    async fn assistant_reset(&self, args: AssistantResetArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_daemon_settings() {
            return Err(CliError::Failed(
                "this hutand cannot reset an assistant's backend (it is older than protocol \
                 0.28); restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        let (scope, workspace_id) = self.assistant_scope(&client, args.workspace).await?;
        client
            .assistant_reset(scope, workspace_id)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(
            &json!({"scope": scope.as_str(), "workspace_id": workspace_id}),
            || match workspace_id {
                Some(id) => format!("the assistants of workspace {id} inherit their backend again"),
                None => "the global assistant inherits its backend again".to_owned(),
            },
        )
    }

    async fn assistant_ls(&self, args: AssistantLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace_id = match args.workspace {
            Some(target) => {
                let all: WorkspaceList =
                    self.call(&client, Request::WorkspaceList(Empty {})).await?;
                Some(resolve_workspace(&all.workspaces, &target)?.id)
            }
            None => None,
        };
        let assistants = client
            .assistant_list(AssistantListRequest {
                scope: None,
                workspace_id,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let list = AssistantList { assistants };
        self.print(&list, || assistant_table(&list.assistants))
    }
}

fn assistant_table(assistants: &[AssistantEntry]) -> String {
    if assistants.is_empty() {
        return "no assistants yet (`hutan ask \"…\"` or `hutan assistant open` creates one)"
            .to_owned();
    }
    let mut rows = vec![[
        "SCOPE".to_owned(),
        "WORKSPACE".to_owned(),
        "SESSION".to_owned(),
        "STATE".to_owned(),
        "MODEL".to_owned(),
        "DEFAULT".to_owned(),
    ]];
    for entry in assistants {
        let session = &entry.session;
        rows.push([
            entry.scope.as_str().to_owned(),
            entry.workspace_id.to_string(),
            session.id.to_string(),
            super::sessions::state_text(&session.state),
            session.chat.as_ref().map_or_else(
                || "-".to_owned(),
                |chat| {
                    format!(
                        "{} / {}",
                        chat.provider_id.as_deref().unwrap_or(&chat.agent),
                        chat.model
                    )
                },
            ),
            if entry.default { "yes" } else { "-" }.to_owned(),
        ]);
    }
    let widths: Vec<usize> = (0..6)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    line.push_str("  ");
                }
                line.push_str(cell);
                if column + 1 < row.len() {
                    line.push_str(&" ".repeat(widths[column] - cell.chars().count()));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
