//! `hutan account list|login|logout|add|remove|default`: the logins of the agent harnesses (protocol 0.17,
//! capability `accounts`; ADR 0015). Hutan lists what each harness reports and starts the
//! harness's own login or logout in a terminal session; it never sees a token.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

use hutan_protocol::account::{
    AccountInfo, AccountList, AccountSession, AccountStatus, DefaultSource, ManagedAccount,
    RemoveLogout,
};
use hutan_protocol::messages::{Empty, Request, Subscribed};

use super::{Ctx, print_line};
use crate::args::{
    AccountAddArgs, AccountCommand, AccountDefaultArgs, AccountLoginArgs, AccountRemoveArgs,
};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn account(&self, command: AccountCommand) -> Result<(), CliError> {
        match command {
            AccountCommand::List => self.account_ls().await,
            AccountCommand::Login(args) => self.account_flow(args, Flow::Login).await,
            AccountCommand::Logout(args) => self.account_flow(args, Flow::Logout).await,
            AccountCommand::Add(args) => self.account_add(args).await,
            AccountCommand::Remove(args) => self.account_remove(args).await,
            AccountCommand::Default(args) => self.account_default(args).await,
        }
    }

    async fn account_ls(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let list = client.account_overview().await.map_err(|e| self.fail(e))?;
        self.print(&list, || account_listing(&list))
    }

    async fn account_add(&self, args: AccountAddArgs) -> Result<(), CliError> {
        if args.attach {
            super::sessions::require_tty()?;
        }
        let client = self.connect().await?;
        let added = client
            .account_add(&args.harness, &args.name, args.label.clone())
            .await
            .map_err(|e| self.fail(e))?;
        if args.no_login {
            return self.print(&added, || {
                format!(
                    "added {} account {} ({}): log in with `hutan account login {} --account {}`",
                    added.harness, added.name, added.home, added.harness, added.name
                )
            });
        }
        if !self.json {
            print_line(&format!(
                "added {} account {} ({})",
                added.harness, added.name, added.home
            ))?;
        }
        self.account_flow(
            AccountLoginArgs {
                harness: args.harness,
                provider: None,
                account: Some(args.name),
                attach: args.attach,
            },
            Flow::Login,
        )
        .await
    }

    async fn account_remove(&self, args: AccountRemoveArgs) -> Result<(), CliError> {
        if !args.yes {
            confirm_remove(&args.harness, &args.name)?;
        }
        let client = self.connect().await?;
        let removed = client
            .account_remove(&args.harness, &args.name)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&removed, || {
            let mut text = format!("removed {} account {}", args.harness, args.name);
            match removed.logout {
                RemoveLogout::Done => text.push_str("; the harness logged it out"),
                RemoveLogout::Skipped => {}
                RemoveLogout::Failed => text.push_str("; the harness could not log it out"),
            }
            if let Some(detail) = &removed.detail {
                text.push_str(&format!("\n{detail}"));
            }
            text
        })
    }

    async fn account_default(&self, args: AccountDefaultArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let account = if args.clear {
            None
        } else {
            args.account.clone()
        };
        client
            .account_default(&args.harness, account.clone(), args.project.clone())
            .await
            .map_err(|e| self.fail(e))?;
        let scope = args.project.as_deref().map_or_else(
            || args.harness.clone(),
            |p| format!("{} in {p}", args.harness),
        );
        let what = account.map_or_else(
            || format!("no default for {scope}"),
            |a| format!("new {scope} sessions take {a}"),
        );
        if self.json {
            return self.print(&Empty {}, String::new);
        }
        print_line(&what)
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
                    .account_login_as(&args.harness, args.provider.clone(), args.account.clone())
                    .await
            }
            Flow::Logout => {
                client
                    .account_logout_as(&args.harness, args.provider.clone(), args.account.clone())
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
                print_line("`hutan account list` shows what changed")?;
            }
        }
        Ok(())
    }
}

/// Asks `[y/N]` before an account's folder is deleted; without a terminal nothing is removed.
fn confirm_remove(harness: &str, name: &str) -> Result<(), CliError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(CliError::Usage(format!(
            "nothing was removed: stdin is not a terminal; pass --yes to remove {harness} account {name}"
        )));
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "This logs {harness} out of {name} and deletes the account's folder under Hutan's data directory."
    )?;
    write!(stderr, "Remove {harness} account {name}? [y/N] ")?;
    stderr.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Failed("nothing was removed".to_owned()))
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

/// The own logins as a table, then the accounts added in Hutan and the defaults.
fn account_listing(list: &AccountList) -> String {
    let mut text = account_table(&list.accounts);
    if list.managed.is_empty() && list.defaults.is_empty() {
        return text;
    }
    text.push_str("\n\n");
    text.push_str(&managed_table(&list.managed, &list.defaults));
    text
}

/// `HARNESS NAME LABEL STATUS DEFAULT` per account added in Hutan, then defaults of projects.
fn managed_table(
    accounts: &[ManagedAccount],
    defaults: &[hutan_protocol::account::AccountDefault],
) -> String {
    let mut rows: Vec<[String; 5]> = vec![[
        "HARNESS".to_owned(),
        "ACCOUNT".to_owned(),
        "LABEL".to_owned(),
        "STATUS".to_owned(),
        "DEFAULT".to_owned(),
    ]];
    for account in accounts {
        let status = match account.logins.first() {
            Some(login) => {
                let mut text = status_text(login.status).to_owned();
                if let Some(plan) = &login.plan {
                    text.push_str(&format!(" ({plan})"));
                }
                if let Some(detail) = &login.detail {
                    text.push_str(&format!(" ({detail})"));
                }
                text
            }
            None => "unknown".to_owned(),
        };
        let default = defaults
            .iter()
            .find(|d| {
                d.harness == account.harness && d.project.is_none() && d.account == account.name
            })
            .map_or("", |d| match d.source {
                DefaultSource::Registry => "default",
                DefaultSource::Config => "default (daemon.toml)",
            });
        rows.push([
            account.harness.clone(),
            account.name.clone(),
            account.label.clone(),
            status,
            default.to_owned(),
        ]);
    }
    let mut out = pad(&rows);
    let own: Vec<String> = defaults
        .iter()
        .filter(|d| d.project.is_none() && d.account == hutan_protocol::account::SYSTEM_ACCOUNT)
        .map(|d| format!("{}: the harness's own login is the default", d.harness))
        .chain(defaults.iter().filter_map(|d| {
            d.project
                .as_ref()
                .map(|p| format!("{} in project {p}: {}", d.harness, d.account))
        }))
        .collect();
    for line in own {
        out.push('\n');
        out.push_str(&line);
    }
    out
}

/// Left-aligned columns, two spaces apart; the last column is not padded.
fn pad(rows: &[[String; 5]]) -> String {
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
