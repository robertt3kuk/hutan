//! `hutan account ls|login|logout`: the logins of the agent harnesses (protocol 0.17,
//! capability `accounts`; ADR 0015). Hutan lists what each harness reports and starts the
//! harness's own login or logout in a terminal session; it never sees a token.

use hutan_protocol::account::{AccountInfo, AccountList, AccountSession, AccountStatus};
use hutan_protocol::messages::{Empty, Request, Subscribed};

use super::{Ctx, print_line};
use crate::args::{AccountCommand, AccountLoginArgs};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn account(&self, command: AccountCommand) -> Result<(), CliError> {
        match command {
            AccountCommand::Ls => self.account_ls().await,
            AccountCommand::Login(args) => self.account_flow(args, Flow::Login).await,
            AccountCommand::Logout(args) => self.account_flow(args, Flow::Logout).await,
        }
    }

    async fn account_ls(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let accounts = client.account_list().await.map_err(|e| self.fail(e))?;
        let list = AccountList {
            accounts,
            ..AccountList::default()
        };
        self.print(&list, || account_table(&list.accounts))
    }

    async fn account_flow(&self, args: AccountLoginArgs, flow: Flow) -> Result<(), CliError> {
        if args.attach {
            super::sessions::require_tty()?;
        }
        let client = self.connect().await?;
        // Subscribed before the session exists, so its end cannot slip by `attach`.
        let events = client.events();
        if args.attach {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let started: AccountSession = match flow {
            Flow::Login => {
                client
                    .account_login(&args.harness, args.provider.clone())
                    .await
            }
            Flow::Logout => {
                client
                    .account_logout(&args.harness, args.provider.clone())
                    .await
            }
        }
        .map_err(|e| self.fail(e))?;
        let attach = args.attach && started.session.state.is_live();
        self.print(&started, || {
            let mut text = format!(
                "session {}: complete the {} in it (`hutan attach {}`)",
                started.session.id,
                flow.noun(),
                started.session.id
            );
            if let Some(instructions) = &started.instructions {
                text.push_str(&format!("\n{instructions}"));
            }
            text
        })?;
        if attach {
            self.attach_session(client, events, started.session).await?;
            if !self.json {
                print_line("`hutan account ls` shows what changed")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum Flow {
    Login,
    Logout,
}

impl Flow {
    fn noun(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Logout => "logout",
        }
    }
}

fn status_text(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::LoggedIn => "logged in",
        AccountStatus::LoggedOut => "logged out",
        AccountStatus::Unknown => "unknown",
    }
}

/// One line per account: harness, provider, label, plan, status, and what the harness said
/// when it could not tell.
fn account_table(accounts: &[AccountInfo]) -> String {
    let mut rows: Vec<[String; 5]> = vec![[
        "HARNESS".to_owned(),
        "PROVIDER".to_owned(),
        "ACCOUNT".to_owned(),
        "PLAN".to_owned(),
        "STATUS".to_owned(),
    ]];
    for account in accounts {
        let mut status = status_text(account.status).to_owned();
        if let Some(detail) = &account.detail {
            status.push_str(&format!(" ({detail})"));
        }
        rows.push([
            account.harness.clone(),
            account.provider.clone(),
            account.label.clone(),
            account.plan.clone().unwrap_or_else(|| "-".to_owned()),
            status,
        ]);
    }
    let widths: Vec<usize> = (0..4)
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
                if column < 4 {
                    line.push_str(&format!("{cell:<width$}  ", width = widths[column]));
                } else {
                    line.push_str(cell);
                }
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(harness: &str, provider: &str, status: AccountStatus) -> AccountInfo {
        AccountInfo {
            harness: harness.into(),
            provider: provider.into(),
            label: provider.into(),
            plan: None,
            status,
            detail: None,
        }
    }

    #[test]
    fn the_table_aligns_columns_and_shows_what_a_harness_said() {
        let mut claude = account("claude", "anthropic", AccountStatus::Unknown);
        claude.detail = Some("`claude` was not found".to_owned());
        let mut codex = account("codex", "openai", AccountStatus::LoggedIn);
        codex.plan = Some("plus".to_owned());
        let table = account_table(&[
            account("omp", "openai-codex", AccountStatus::LoggedIn),
            claude,
            codex,
        ]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0],
            "HARNESS  PROVIDER      ACCOUNT       PLAN  STATUS"
        );
        assert_eq!(
            lines[1],
            "omp      openai-codex  openai-codex  -     logged in"
        );
        assert!(
            lines[2].ends_with("unknown (`claude` was not found)"),
            "{table}"
        );
        assert!(lines[3].contains("plus  logged in"), "{table}");
    }
}
