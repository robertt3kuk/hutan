//! `hutan grants ls|revoke`: what Mawas may do without asking (`docs/control.md` § Mawas).

use hutan_protocol::grants::{ControlGrant, ControlGrants};
use hutan_protocol::messages::{Empty, Request, WorkspaceList};
use serde_json::json;

use super::Ctx;
use super::workspaces::resolve_workspace;
use crate::args::{GrantsCommand, GrantsLsArgs, GrantsRevokeArgs};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn grants(&self, command: GrantsCommand) -> Result<(), CliError> {
        match command {
            GrantsCommand::Ls(args) => self.grants_ls(args).await,
            GrantsCommand::Revoke(args) => self.grants_revoke(args).await,
        }
    }

    async fn grants_client(&self) -> Result<hutan_client::Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_control_grants() {
            return Err(CliError::Failed(
                "this hutand has no grants (it is older than protocol 0.31); restart it with \
                 `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    async fn grants_ls(&self, args: GrantsLsArgs) -> Result<(), CliError> {
        let client = self.grants_client().await?;
        let workspace_id = match args.workspace {
            Some(target) => {
                let all: WorkspaceList =
                    self.call(&client, Request::WorkspaceList(Empty {})).await?;
                Some(resolve_workspace(&all.workspaces, &target)?.id)
            }
            None => None,
        };
        let grants = client
            .control_grants(workspace_id)
            .await
            .map_err(|e| self.fail(e))?;
        let list = ControlGrants { grants };
        self.print(&list, || grant_table(&list.grants))
    }

    async fn grants_revoke(&self, args: GrantsRevokeArgs) -> Result<(), CliError> {
        let client = self.grants_client().await?;
        let ids: Vec<u64> = match args.id {
            Some(id) => vec![id],
            None => client
                .control_grants(None)
                .await
                .map_err(|e| self.fail(e))?
                .into_iter()
                .map(|grant| grant.id)
                .collect(),
        };
        for id in &ids {
            client
                .control_grant_revoke(*id)
                .await
                .map_err(|e| self.fail(e))?;
        }
        self.print(&json!({"revoked": ids}), || match ids.as_slice() {
            [] => "no grants to revoke".to_owned(),
            [id] => format!("revoked grant {id}"),
            many => format!("revoked {} grants", many.len()),
        })
    }
}

fn grant_table(grants: &[ControlGrant]) -> String {
    if grants.is_empty() {
        return "no grants: Mawas asks before every change".to_owned();
    }
    let mut rows = vec![[
        "ID".to_owned(),
        "TOOL".to_owned(),
        "APPLIES TO".to_owned(),
        "GRANTED".to_owned(),
    ]];
    for grant in grants {
        let applies = match (grant.workspace_id, grant.chat_session_id) {
            (Some(workspace), _) => format!("workspace {workspace}"),
            (None, Some(chat)) => format!("chat {chat}"),
            (None, None) => "-".to_owned(),
        };
        rows.push([
            grant.id.to_string(),
            grant.tool.clone(),
            applies,
            grant.created_ms.to_string(),
        ]);
    }
    let widths: Vec<usize> = (0..3)
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
            for (cell, width) in row.iter().zip(&widths) {
                line.push_str(&format!("{cell:<width$}  "));
            }
            line.push_str(&row[3]);
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
