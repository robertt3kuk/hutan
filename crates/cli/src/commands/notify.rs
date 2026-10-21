//! `hutan notify ls|ack|act|watch|mute|unmute`: the daemon's notification intents (protocol 0.13,
//! `docs/protocol.md` § Notification intents) and per-session mute (protocol 0.34).

use std::collections::HashSet;

use hutan_protocol::messages::{Empty, Event, Request, Subscribed};
use hutan_protocol::notify::{ActRoute, NotificationIntent, NotifyList};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use crate::args::{NotifyActArgs, NotifyCommand, NotifyIdArgs, TargetArgs};
use crate::error::CliError;

/// Shortest intent id prefix accepted.
const MIN_PREFIX: usize = 4;

impl Ctx {
    pub(super) async fn notify(&self, command: NotifyCommand) -> Result<(), CliError> {
        match command {
            NotifyCommand::Ls => self.notify_ls().await,
            NotifyCommand::Ack(args) => self.notify_ack(args).await,
            NotifyCommand::Act(args) => self.notify_act(args).await,
            NotifyCommand::Watch => self.notify_watch().await,
            NotifyCommand::Mute(args) => self.notify_mute(args, true).await,
            NotifyCommand::Unmute(args) => self.notify_mute(args, false).await,
        }
    }

    /// `hutan notify mute|unmute <session>`. Prints the session (`--json`: its `SessionInfo`).
    async fn notify_mute(&self, args: TargetArgs, muted: bool) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_notify_mute() {
            return Err(CliError::Failed(
                "this daemon cannot mute notifications (it predates protocol 0.34); restart it \
                 with `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let session = self.resolve(&client, &args.target).await?;
        let info = client
            .notify_mute(session.id, muted)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&info, || {
            if info.muted {
                format!("muted notifications of {} ({})", info.title, info.id)
            } else {
                format!("notifications of {} ({}) are on", info.title, info.id)
            }
        })
    }

    async fn notify_ls(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let intents = client.notify_list().await.map_err(|e| self.fail(e))?;
        let list = NotifyList { intents };
        self.print(&list, || intent_table(&list.intents))
    }

    async fn notify_ack(&self, args: NotifyIdArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let intents = client.notify_list().await.map_err(|e| self.fail(e))?;
        let id = resolve_intent(&intents, &args.intent)?.id.clone();
        client.notify_ack(&id).await.map_err(|e| self.fail(e))?;
        self.print(&json!({ "id": id, "acknowledged": true }), || {
            format!("acknowledged {id}")
        })
    }

    async fn notify_act(&self, args: NotifyActArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let intents = client.notify_list().await.map_err(|e| self.fail(e))?;
        let intent = resolve_intent(&intents, &args.intent)?;
        let (id, session) = (intent.id.clone(), intent.session_id);
        let acted = client
            .notify_act(&id, &args.action)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&acted, || match acted.route {
            ActRoute::Approval => format!("answered the approval of {id}: {}", args.action),
            ActRoute::UiOpen if acted.desktop_clients == 0 => {
                format!("no desktop is connected; session {session} was not opened")
            }
            ActRoute::UiOpen => format!(
                "asked {} desktop client(s) to open session {session}",
                acted.desktop_clients
            ),
        })
    }

    /// The pending intents first (so a late watcher misses nothing), then every new one, until
    /// the connection ends. `--json` prints one intent per line.
    async fn notify_watch(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        // Subscribed before the list is read: an intent raised in between shows up twice, and
        // `seen` prints it once.
        let mut events = client.events();
        let _: Subscribed = self
            .call(&client, Request::EventsSubscribe(Empty {}))
            .await?;
        let mut seen = HashSet::new();
        for intent in client.notify_list().await.map_err(|e| self.fail(e))? {
            self.print_intent(&intent, &mut seen)?;
        }
        loop {
            match events.recv().await {
                Ok((_, Event::NotifyIntent { intent })) => self.print_intent(&intent, &mut seen)?,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {
                    return Err(CliError::Unreachable(
                        "lost the connection to hutand".into(),
                    ));
                }
            }
        }
    }

    fn print_intent(
        &self,
        intent: &NotificationIntent,
        seen: &mut HashSet<String>,
    ) -> Result<(), CliError> {
        if !seen.insert(intent.id.clone()) {
            return Ok(());
        }
        self.print(intent, || intent_line(intent))
    }
}

/// One pending intent: exact id, or a unique prefix of at least [`MIN_PREFIX`] characters.
fn resolve_intent<'a>(
    pending: &'a [NotificationIntent],
    target: &str,
) -> Result<&'a NotificationIntent, CliError> {
    if let Some(found) = pending.iter().find(|i| i.id == target) {
        return Ok(found);
    }
    let prefix = target.to_ascii_uppercase();
    let matches: Vec<&NotificationIntent> = if target.len() >= MIN_PREFIX {
        pending
            .iter()
            .filter(|i| i.id.to_ascii_uppercase().starts_with(&prefix))
            .collect()
    } else {
        Vec::new()
    };
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no pending notification matches `{target}`; `hutan notify ls` lists them"
        ))),
        many => {
            let mut message = format!(
                "`{target}` matches {} notifications; use a longer id:",
                many.len()
            );
            for intent in many {
                message.push_str(&format!("\n  {}  {}", intent.id, intent.title));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// `ID  kind  title: body  [actions]`
fn intent_line(intent: &NotificationIntent) -> String {
    let actions: Vec<&str> = intent.actions.iter().map(|a| a.id.as_str()).collect();
    format!(
        "{}  {}  {} — {}  [{}]",
        intent.id,
        intent.kind.as_str(),
        intent.title,
        intent.body,
        actions.join(" ")
    )
}

fn intent_table(intents: &[NotificationIntent]) -> String {
    if intents.is_empty() {
        return "no pending notifications".to_owned();
    }
    let rows: Vec<[String; 4]> = intents
        .iter()
        .map(|i| {
            let actions: Vec<&str> = i.actions.iter().map(|a| a.id.as_str()).collect();
            [
                i.id.clone(),
                i.kind.as_str().to_owned(),
                actions.join(" "),
                format!("{} — {}", i.title, i.body),
            ]
        })
        .collect();
    let header = ["ID", "KIND", "ACTIONS", "NOTIFICATION"].map(str::to_owned);
    let mut widths = [0; 3];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(widths) {
                line.push_str(&format!("{cell:<width$}  "));
            }
            line.push_str(&row[3]);
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use hutan_core::SessionId;
    use hutan_protocol::notify::{IntentAction, IntentKind};

    use super::*;

    fn intent(id: &str, title: &str) -> NotificationIntent {
        NotificationIntent {
            id: id.to_owned(),
            kind: IntentKind::AgentFinished,
            session_id: SessionId::new(),
            workspace_id: None,
            title: title.to_owned(),
            body: "body".to_owned(),
            actions: vec![IntentAction {
                id: "open".to_owned(),
                label: "Open".to_owned(),
            }],
            created_at_ms: 1,
            expires_at_ms: None,
        }
    }

    #[test]
    fn an_intent_is_found_by_exact_id_or_unique_prefix() {
        let pending = [
            intent("01JABCDEF0", "one"),
            intent("01JABCXYZ9", "two"),
            intent("01JZZZZZZZ", "three"),
        ];
        assert_eq!(resolve_intent(&pending, "01JABCDEF0").unwrap().title, "one");
        assert_eq!(resolve_intent(&pending, "01jzz").unwrap().title, "three");
        assert!(matches!(
            resolve_intent(&pending, "01JABC"),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            resolve_intent(&pending, "01J"),
            Err(CliError::Failed(_))
        ));
        assert!(matches!(
            resolve_intent(&pending, "NOPE0"),
            Err(CliError::Failed(_))
        ));
    }

    #[test]
    fn the_table_lists_kind_actions_and_text() {
        assert_eq!(intent_table(&[]), "no pending notifications");
        let table = intent_table(&[intent("01JABCDEF0", "api: claude finished")]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ID"));
        assert!(lines[1].contains("agent_finished"));
        assert!(lines[1].contains("api: claude finished — body"));
        assert!(intent_line(&intent("X", "t")).ends_with("[open]"));
    }
}
