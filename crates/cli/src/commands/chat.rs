//! `hutan chat new|send|log|attach|interrupt|models|model`, `hutan approvals`, `hutan approve`.

use std::io::Write as _;

use hutan_client::Client;
use hutan_core::question::{FieldKind, QuestionField, QuestionResponse, Values};
use hutan_core::{AgentEvent, ApprovalId, QuestionId, SessionKind};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ChatCreate, ChatHistory, QuestionInfo, QuestionList,
};
use hutan_protocol::messages::{Empty, Event, Request, SessionInfo, SessionResult, Subscribed};
use hutan_protocol::provider::AGENT_DIRECT;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use super::{Ctx, print_line};
use crate::args::{
    AnswerArgs, ApproveArgs, ChatCommand, ChatModelArgs, ChatNewArgs, ChatSendArgs, TargetArgs,
};
use crate::error::CliError;
use hutan_cli::chat_render::ChatPrinter;

/// Shortest approval id prefix accepted.
const MIN_PREFIX: usize = 4;
/// Events of the journal shown first by `attach`.
const ATTACH_BACKLOG: u32 = 40;

impl Ctx {
    pub(super) async fn chat(&self, command: ChatCommand) -> Result<(), CliError> {
        match command {
            ChatCommand::New(args) => self.chat_new(args).await,
            ChatCommand::Send(args) => self.chat_send(args).await,
            ChatCommand::Log(TargetArgs { target }) => self.chat_log(&target).await,
            ChatCommand::Attach(TargetArgs { target }) => self.chat_attach(&target).await,
            ChatCommand::Interrupt(TargetArgs { target }) => self.chat_interrupt(&target).await,
            ChatCommand::Retry(TargetArgs { target }) => self.chat_retry(&target).await,
            ChatCommand::Models(args) => self.chat_models(&args.agent).await,
            ChatCommand::Model(args) => self.chat_model(args).await,
        }
    }

    /// The session a chat command targets, which must be a chat session.
    async fn resolve_chat(&self, client: &Client, target: &str) -> Result<SessionInfo, CliError> {
        let session = self.resolve(client, target).await?;
        if session.kind != SessionKind::Harness {
            return Err(CliError::Failed(format!(
                "session {} is a terminal session; `hutan chat new` starts a chat session",
                session.id
            )));
        }
        Ok(session)
    }

    async fn chat_new(&self, args: ChatNewArgs) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let cwd = match args.cwd {
            Some(path) => std::path::absolute(path)?,
            None => std::env::current_dir()?,
        };
        let client = self.connect().await?;
        let workspace = match super::workspaces::workspace_target(args.workspace) {
            Some((target, _)) => Some(self.resolve_workspace_id(&client, &target).await?),
            None => None,
        };
        let direct = args.direct;
        let agent = if direct {
            AGENT_DIRECT.to_owned()
        } else {
            args.agent.unwrap_or_else(|| "codex".to_owned())
        };
        let session = client
            .chat_create(ChatCreate {
                workspace_id: workspace,
                cwd,
                agent,
                model: args.model,
                approval_policy: args.approval_policy,
                sandbox: args.sandbox,
                title: args.title,
                mode: args.mode,
                provider_id: args.provider,
                system_prompt: args.system,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let result = SessionResult { session };
        self.print(&result, || {
            let session = &result.session;
            match &session.chat {
                Some(chat) if direct => format!(
                    "session {} direct {} / {}",
                    session.id,
                    chat.provider_id.as_deref().unwrap_or("?"),
                    chat.model
                ),
                // The harness decides what asks and what is sandboxed; the CLI does not show
                // Hutan's defaults as if they applied.
                Some(chat) if matches!(chat.agent.as_str(), "omp" | "claude") => {
                    format!("session {} {} {}", session.id, chat.agent, chat.model)
                }
                Some(chat) => format!(
                    "session {} {} {} (approvals: {}, sandbox: {})",
                    session.id, chat.agent, chat.model, chat.approval_policy, chat.sandbox
                ),
                None => format!("session {}", session.id),
            }
        })
    }

    async fn chat_model(&self, args: ChatModelArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, &args.target).await?;
        let session = client
            .chat_set_model(session.id, args.provider, args.model)
            .await
            .map_err(|e| self.fail(e))?;
        let result = SessionResult { session };
        self.print(&result, || {
            let session = &result.session;
            match &session.chat {
                Some(chat) if chat.provider_id.is_some() => format!(
                    "session {} direct {} / {}",
                    session.id,
                    chat.provider_id.as_deref().unwrap_or("?"),
                    chat.model
                ),
                Some(chat) => format!("session {} {} {}", session.id, chat.agent, chat.model),
                None => format!("session {}", session.id),
            }
        })
    }

    async fn chat_send(&self, args: ChatSendArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, &args.target).await?;
        // Subscribed before sending, so no event of the turn is missed.
        let mut events = client.events();
        if args.wait {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let sent = client
            .chat_send_with(session.id, args.text, args.attach)
            .await
            .map_err(|e| self.fail(e))?;
        if !args.wait {
            return self.print(
                &json!({"session_id": session.id, "turn_id": sent.turn_id}),
                || format!("sent to {} (turn {})", session.id, sent.turn_id),
            );
        }
        self.follow_turn(&mut events, session.id, sent.turn_id)
            .await
    }

    /// Prints the events of one turn as they stream, until it ends: `Ok` when it completed,
    /// else the reason is printed and the error is [`CliError::Reported`].
    pub(super) async fn follow_turn(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
        session_id: hutan_core::SessionId,
        turn_id: hutan_core::TurnId,
    ) -> Result<(), CliError> {
        let mut printer = ChatPrinter::without_user_messages();
        loop {
            match events.recv().await {
                Ok((
                    _,
                    Event::ChatEvent {
                        session_id: from,
                        event,
                    },
                )) if from == session_id && event.turn_id.is_none_or(|turn| turn == turn_id) => {
                    self.show(&mut printer, &event)?;
                    if event.kind.ends_turn() {
                        return match event.kind {
                            hutan_core::AgentEventKind::TurnCompleted { .. } => Ok(()),
                            // The line saying why is already printed.
                            _ => Err(CliError::Reported),
                        };
                    }
                }
                Ok((_, Event::SessionUpdated { session: updated }))
                    if updated.id == session_id && !updated.state.is_live() =>
                {
                    return Err(CliError::Failed(format!(
                        "the session ended before the turn did ({})",
                        super::sessions::state_text(&updated.state)
                    )));
                }
                Ok(_) => {}
                Err(error) => return Err(self.stream_error(error)),
            }
        }
    }

    /// Prints one event: its text, or its JSON line with `--json`.
    fn show(&self, printer: &mut ChatPrinter, event: &AgentEvent) -> Result<(), CliError> {
        if self.json {
            return print_line(&serde_json::to_string(event).map_err(super::io_error)?);
        }
        let text = printer.render(event);
        if !text.is_empty() {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            stdout.flush()?;
        }
        Ok(())
    }

    fn stream_error(&self, error: RecvError) -> CliError {
        match error {
            RecvError::Lagged(missed) => CliError::Failed(format!(
                "fell behind the event stream by {missed} events; `hutan chat log` has the rest"
            )),
            RecvError::Closed => self.fail(hutan_client::ClientError::Disconnected),
        }
    }

    async fn chat_log(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, target).await?;
        let history: ChatHistory = client
            .chat_history_all(session.id)
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return print_line(&serde_json::to_string(&history).map_err(super::io_error)?);
        }
        let mut printer = ChatPrinter::new();
        for event in &history.events {
            self.show(&mut printer, event)?;
        }
        // A message that is still streaming ends the log in the middle of a line.
        let end = printer.finish();
        if !end.is_empty() {
            print!("{end}");
        }
        Ok(())
    }

    async fn chat_attach(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, target).await?;
        let mut events = client.events();
        let _: Subscribed = self
            .call(&client, Request::EventsSubscribe(Empty {}))
            .await?;
        // History after the subscription: what happens in between arrives live and is
        // recognised by its seq.
        let history = client
            .chat_history(session.id, None, ATTACH_BACKLOG)
            .await
            .map_err(|e| self.fail(e))?;
        let mut printer = ChatPrinter::new();
        for event in &history.events {
            self.show(&mut printer, event)?;
        }
        let current = self.resolve_chat(&client, &session.id.to_string()).await?;
        if !current.state.is_live() {
            return self.ended(&current);
        }
        loop {
            match events.recv().await {
                Ok((_, Event::ChatEvent { session_id, event }))
                    if session_id == session.id && event.seq > history.last_seq =>
                {
                    self.show(&mut printer, &event)?;
                }
                Ok((_, Event::SessionUpdated { session: updated }))
                    if updated.id == session.id && !updated.state.is_live() =>
                {
                    return self.ended(&updated);
                }
                Ok(_) => {}
                Err(error) => return Err(self.stream_error(error)),
            }
        }
    }

    fn ended(&self, session: &SessionInfo) -> Result<(), CliError> {
        if self.json {
            return Ok(());
        }
        print_line(&format!(
            "session {} {}",
            session.id,
            super::sessions::state_text(&session.state)
        ))
    }

    async fn chat_interrupt(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, target).await?;
        client
            .chat_interrupt(session.id)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({"session_id": session.id}), || {
            format!("interrupted {}", session.id)
        })
    }

    /// `hutan chat retry`: `chat.retry` (protocol 0.34). Prints the new turn's id.
    async fn chat_retry(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve_chat(&client, target).await?;
        if !client.supports_chat_retry() {
            return Err(CliError::Failed(
                "this daemon cannot retry a message (it predates protocol 0.34); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let sent = client
            .chat_retry(session.id)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&sent, || {
            format!("sent the last message again: turn {}", sent.turn_id)
        })
    }

    async fn chat_models(&self, agent: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let models = client.chat_models(agent).await.map_err(|e| self.fail(e))?;
        self.print(&models, || {
            let mut out = String::new();
            for model in models.models.iter().filter(|m| !m.hidden) {
                let marker = if model.is_default { " (default)" } else { "" };
                out.push_str(&format!("{}{marker}\t{}\n", model.id, model.display_name));
            }
            out.trim_end().to_owned()
        })
    }

    pub(super) async fn approvals(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let approvals = client.approvals(None).await.map_err(|e| self.fail(e))?;
        let list = ApprovalList { approvals };
        self.print(&list, || approval_table(&list.approvals))
    }

    pub(super) async fn approve(&self, args: ApproveArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let pending = client.approvals(None).await.map_err(|e| self.fail(e))?;
        let approval = resolve_approval(&pending, &args.approval)?.id;
        client
            .approve(approval, args.option.clone())
            .await
            .map_err(|e| self.fail(e))?;
        self.print(
            &json!({"approval_id": approval, "option_id": args.option}),
            || format!("approved {approval}: {}", args.option),
        )
    }
}

impl Ctx {
    pub(super) async fn questions(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let questions = client.questions(None).await.map_err(|e| self.fail(e))?;
        let list = QuestionList { questions };
        self.print(&list, || question_table(&list.questions))
    }

    pub(super) async fn answer(&self, args: AnswerArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let pending = client.questions(None).await.map_err(|e| self.fail(e))?;
        let question = resolve_question(&pending, &args.question)?;
        let (response, summary) = if args.decline {
            (QuestionResponse::Decline, "declined".to_owned())
        } else if args.cancel {
            (QuestionResponse::Cancel, "cancelled".to_owned())
        } else {
            let values = parse_values(&question.spec.fields, &args.values)?;
            let secret = |id: &str| {
                question
                    .spec
                    .fields
                    .iter()
                    .any(|f| f.id == id && matches!(f.kind, FieldKind::Secret))
            };
            // What is printed names the fields; a value, a secret's above all, is not echoed.
            let named: Vec<String> = values
                .keys()
                .map(|id| {
                    if secret(id) {
                        format!("{id} (secret)")
                    } else {
                        id.clone()
                    }
                })
                .collect();
            (
                QuestionResponse::Submit { values },
                format!("answered: {}", named.join(", ")),
            )
        };
        let action = match &response {
            QuestionResponse::Submit { .. } => "submit",
            QuestionResponse::Decline => "decline",
            QuestionResponse::Cancel => "cancel",
        };
        let id = question.id;
        client
            .answer_question(id, response)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({"question_id": id, "action": action}), || {
            format!("{id} {summary}")
        })
    }
}

/// One pending question: exact id, or a unique prefix of at least [`MIN_PREFIX`] characters.
fn resolve_question<'a>(
    pending: &'a [QuestionInfo],
    target: &str,
) -> Result<&'a QuestionInfo, CliError> {
    if let Ok(id) = target.parse::<QuestionId>()
        && let Some(found) = pending.iter().find(|q| q.id == id)
    {
        return Ok(found);
    }
    let prefix = target.to_ascii_uppercase();
    let matches: Vec<&QuestionInfo> = if target.len() >= MIN_PREFIX {
        pending
            .iter()
            .filter(|q| q.id.to_string().starts_with(&prefix))
            .collect()
    } else {
        Vec::new()
    };
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no pending question matches `{target}`; `hutan questions` lists them"
        ))),
        many => {
            let mut message = format!(
                "`{target}` matches {} questions; use a longer id:",
                many.len()
            );
            for question in many {
                message.push_str(&format!("\n  {}  {}", question.id, question.spec.title));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// `field=value` arguments as the answer's values, typed by the question's fields.
fn parse_values(fields: &[QuestionField], given: &[String]) -> Result<Values, CliError> {
    let mut values = Values::new();
    for entry in given {
        let (name, text) = entry.split_once('=').ok_or_else(|| {
            CliError::Usage(format!(
                "`{entry}` is not field=value (`hutan questions` lists the fields)"
            ))
        })?;
        let field = fields.iter().find(|f| f.id == name).ok_or_else(|| {
            let known: Vec<&str> = fields.iter().map(|f| f.id.as_str()).collect();
            CliError::Usage(format!(
                "the question has no field `{name}`; its fields: {}",
                known.join(", ")
            ))
        })?;
        let text = if matches!(field.kind, FieldKind::Secret) && text == "-" {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| CliError::Failed(format!("reading `{name}` from stdin: {e}")))?;
            line.trim_end_matches(['\n', '\r']).to_owned()
        } else {
            text.to_owned()
        };
        let value = match &field.kind {
            FieldKind::Text | FieldKind::Multiline | FieldKind::Secret => {
                serde_json::Value::String(text)
            }
            FieldKind::Boolean => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "1" | "on" => serde_json::Value::Bool(true),
                "false" | "no" | "n" | "0" | "off" => serde_json::Value::Bool(false),
                _ => {
                    return Err(CliError::Usage(format!(
                        "`{name}` is true or false, not `{text}`"
                    )));
                }
            },
            FieldKind::Number { .. } => text
                .parse::<i64>()
                .map(serde_json::Value::from)
                .or_else(|_| {
                    text.parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(serde_json::Value::Number)
                        .ok_or(())
                })
                .map_err(|()| CliError::Usage(format!("`{name}` is a number, not `{text}`")))?,
            FieldKind::Choice { multi: true, .. } => {
                let mut items = match values.remove(name) {
                    Some(serde_json::Value::Array(items)) => items,
                    _ => Vec::new(),
                };
                items.push(serde_json::Value::String(text));
                serde_json::Value::Array(items)
            }
            FieldKind::Choice { .. } => serde_json::Value::String(text),
        };
        if values.insert(name.to_owned(), value).is_some()
            && !matches!(field.kind, FieldKind::Choice { multi: true, .. })
        {
            return Err(CliError::Usage(format!("`{name}` is given twice")));
        }
    }
    Ok(values)
}

fn question_table(questions: &[QuestionInfo]) -> String {
    if questions.is_empty() {
        return "no pending questions".to_owned();
    }
    let rows: Vec<[String; 4]> = questions
        .iter()
        .map(|q| {
            let title = q.spec.title.lines().next().unwrap_or("").to_owned();
            let fields: Vec<String> = q
                .spec
                .fields
                .iter()
                .map(|f| match &f.kind {
                    FieldKind::Choice { options, multi, .. } => format!(
                        "{}{}={}{}",
                        f.id,
                        if f.required { "" } else { "?" },
                        options
                            .iter()
                            .map(|o| o.value.as_str())
                            .collect::<Vec<_>>()
                            .join("|"),
                        if *multi { "+" } else { "" }
                    ),
                    FieldKind::Secret => {
                        format!("{}{}=(secret)", f.id, if f.required { "" } else { "?" })
                    }
                    _ => format!("{}{}", f.id, if f.required { "" } else { "?" }),
                })
                .collect();
            [
                q.id.to_string(),
                q.session_id.to_string(),
                title,
                fields.join(" "),
            ]
        })
        .collect();
    let header = ["ID", "SESSION", "QUESTION", "FIELDS"].map(str::to_owned);
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

/// One pending approval: exact id, or a unique prefix of at least [`MIN_PREFIX`] characters.
fn resolve_approval<'a>(
    pending: &'a [ApprovalInfo],
    target: &str,
) -> Result<&'a ApprovalInfo, CliError> {
    if let Ok(id) = target.parse::<ApprovalId>()
        && let Some(found) = pending.iter().find(|a| a.id == id)
    {
        return Ok(found);
    }
    let prefix = target.to_ascii_uppercase();
    let matches: Vec<&ApprovalInfo> = if target.len() >= MIN_PREFIX {
        pending
            .iter()
            .filter(|a| a.id.to_string().starts_with(&prefix))
            .collect()
    } else {
        Vec::new()
    };
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no pending approval matches `{target}`; `hutan approvals` lists them"
        ))),
        many => {
            let mut message = format!(
                "`{target}` matches {} approvals; use a longer id:",
                many.len()
            );
            for approval in many {
                message.push_str(&format!("\n  {}  {}", approval.id, approval.title));
            }
            Err(CliError::Usage(message))
        }
    }
}

fn approval_table(approvals: &[ApprovalInfo]) -> String {
    if approvals.is_empty() {
        return "no pending approvals".to_owned();
    }
    let rows: Vec<[String; 4]> = approvals
        .iter()
        .map(|a| {
            let title = a.title.lines().next().unwrap_or("").to_owned();
            let options: Vec<&str> = a.options.iter().map(|o| o.id.as_str()).collect();
            [
                a.id.to_string(),
                a.session_id.to_string(),
                title,
                options.join(" "),
            ]
        })
        .collect();
    let header = ["ID", "SESSION", "REQUEST", "OPTIONS"].map(str::to_owned);
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
