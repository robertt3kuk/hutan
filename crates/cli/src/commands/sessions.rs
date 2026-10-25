//! Session commands: `new`, `ls`, `send`, `stop`, `close`, `reopen`, `restart`, `rename`, `history`, and the
//! target resolution they share with `attach`.

use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal as _;
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_client::Client;
use hutan_core::{SessionId, SessionState, TerminalId};
use hutan_protocol::messages::{
    AgentState, DaemonStatusInfo, Empty, Request, SessionCreate, SessionInfo, SessionList,
    SessionListFilter, SessionRef, SessionRename, SessionResult, SessionSpec, TerminalSize,
    TerminalSpec,
};
use hutan_protocol::worktree::PlacementKind;

use super::attach::end_message;
use super::{Ctx, print_line};
use crate::args::{HistoryArgs, LsArgs, NewArgs, RenameArgs, SendArgs};
use crate::error::CliError;
use hutan_cli::host;

/// Shortest session id prefix accepted as a target.
const MIN_PREFIX: usize = 4;

impl Ctx {
    pub(super) async fn new_session(&self, args: NewArgs) -> Result<(), CliError> {
        let size = if args.attach {
            require_tty()?;
            Some(host_size()?)
        } else {
            None
        };
        // The daemon resolves paths from its own working directory, not ours.
        let mut cwd = match args.cwd {
            Some(path) => std::path::absolute(path)?,
            None => std::env::current_dir()?,
        };
        let client = self.connect().await?;
        let mut workspace = match super::workspaces::workspace_target(args.workspace.clone()) {
            Some((target, source)) => Some(
                self.resolve_workspace_id(&client, &target)
                    .await
                    .map_err(|error| with_source(error, &target, source))?,
            ),
            None => None,
        };
        if let Some(branch) = &args.worktree {
            // The session works in the branch's worktree, made now when there is none.
            let placed = self
                .worktree_for_session(&client, args.workspace, branch, &cwd)
                .await?;
            workspace = Some(placed.workspace);
            cwd = placed.path;
        }
        // Subscribe before creating so an instantly exiting command is not missed by `attach`.
        let events = client.events();
        if args.attach {
            let _: hutan_protocol::messages::Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let created: SessionResult = self
            .call(
                &client,
                Request::SessionCreate(SessionCreate {
                    workspace_id: workspace,
                    spec: SessionSpec::Terminal(TerminalSpec {
                        contain: args.contain.then(|| hutan_core::Contain {
                            network: args.network,
                            extra_write: args.write,
                        }),
                        command: args.argv,
                        cwd: Some(cwd),
                        env: args.env.into_iter().collect::<BTreeMap<_, _>>(),
                        title: args.title,
                        size,
                    }),
                }),
            )
            .await?;
        self.announce(&created.session, &created)?;
        if !self.json
            && let Some(boundary) = &created.session.containment
        {
            // What the daemon applied to the spawn, with the holes named.
            print_line(&format!("contained: {}", boundary.summary()))?;
        }
        if let SessionState::Failed { reason } = &created.session.state {
            return Err(CliError::Failed(format!(
                "session {} failed to start: {reason}",
                created.session.id
            )));
        }
        if args.attach && !created.session.state.is_live() {
            // Ended before we could attach (a command that exits at once).
            return print_line(&end_message(&created.session.state));
        }
        if args.attach {
            self.attach_session(client, events, created.session).await
        } else {
            Ok(())
        }
    }

    pub(super) async fn ls(&self, args: LsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace = match &args.workspace {
            Some(target) => Some(self.resolve_workspace_id(&client, target).await?),
            None => None,
        };
        let mut list = self.session_list(&client, workspace).await?;
        list.sessions.sort_by_key(|s| s.created_at_ms);
        // Only the table names the projects; `--json` has the ids.
        let projects = if self.json {
            HashMap::new()
        } else {
            self.workspace_list(&client)
                .await?
                .workspaces
                .into_iter()
                .map(|w| (w.id, w.name))
                .collect()
        };
        self.print(&list, || session_table(&list.sessions, &projects, now_ms()))
    }

    pub(super) async fn send(&self, args: SendArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, &args.target).await?;
        let terminal = live_terminal(&session)?;
        let mut bytes = args.text.into_bytes();
        if args.enter {
            bytes.push(b'\r');
        }
        client
            .send_input(terminal, &bytes)
            .await
            .map_err(|e| self.fail(e))?;
        // Input frames have no response. A round trip on the same connection proves the daemon
        // read them, and keeps the process alive until the socket wrote them out.
        let _: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        self.print(
            &serde_json::json!({"session_id": session.id, "bytes": bytes.len()}),
            || format!("sent {} byte(s) to {}", bytes.len(), session.id),
        )
    }

    pub(super) async fn stop(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionStop(SessionRef {
                    session_id: session.id,
                }),
            )
            .await?;
        self.print(&result, || format!("stopped {}", result.session.id))
    }

    /// `hutan close`: ends the session and removes it from the lists. A session something runs
    /// in is refused, with what runs, unless `force`.
    pub(super) async fn close(&self, target: &str, force: bool) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_session_close() {
            return Err(CliError::Failed(
                "this hutand cannot close sessions (protocol 0.30); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        let session = self.resolve(&client, target).await?;
        match client.session_close(session.id, force).await {
            Ok(()) => self.print(
                &serde_json::json!({"session_id": session.id, "closed": true}),
                || format!("closed {} ({})", session.id, session.title),
            ),
            Err(error) => match error.busy() {
                Some(busy) => Err(CliError::Failed(format!(
                    "{} ({}) is busy: {}; `hutan close --force {target}` stops it and closes it",
                    session.id,
                    session.title,
                    busy_text(&busy)
                ))),
                None => Err(self.fail(error)),
            },
        }
    }

    /// `hutan reopen`: a closed session comes back (same id; a terminal starts its command
    /// again, a chat continues its conversation).
    pub(super) async fn reopen(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_session_close() {
            return Err(CliError::Failed(
                "this hutand cannot reopen sessions (protocol 0.30); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        let id = self.resolve_closed(&client, target).await?;
        let session = client.session_reopen(id).await.map_err(|e| self.fail(e))?;
        self.announce(
            &session,
            &SessionResult {
                session: session.clone(),
            },
        )
    }

    /// The id of a closed session: a full session id, or a prefix of one that the history
    /// catalog (`hutan history sessions`, needs the index) knows as closed.
    async fn resolve_closed(&self, client: &Client, target: &str) -> Result<SessionId, CliError> {
        if let Ok(id) = target.parse::<SessionId>() {
            return Ok(id);
        }
        if target.len() < MIN_PREFIX {
            return Err(CliError::Usage(format!(
                "`{target}` is not a session id; give the full id or a prefix of at least \
                 {MIN_PREFIX} characters"
            )));
        }
        let answer = client
            .history_sessions(hutan_protocol::history::HistorySessionsRequest {
                scope: hutan_protocol::history::HistoryScope::All,
                workspace_id: None,
                filter: target.to_owned(),
                sort: hutan_protocol::history::HistorySessionSort::Updated,
                sources: vec![hutan_protocol::history::HistorySource::Hutan],
                limit: hutan_protocol::history::SESSIONS_MAX_LIMIT,
                refresh: false,
            })
            .await
            .map_err(|_| {
                CliError::Failed(format!(
                    "cannot look up `{target}` among the closed sessions (that needs \
                     `hutan index enable`); give the full session id"
                ))
            })?;
        let prefix = target.to_ascii_uppercase();
        let mut found: Vec<SessionId> = answer
            .sessions
            .iter()
            .filter(|row| row.closed)
            .filter_map(|row| row.session_id)
            .filter(|id| id.to_string().starts_with(&prefix))
            .collect();
        found.dedup();
        match found.as_slice() {
            [id] => Ok(*id),
            [] => Err(not_found(target)),
            many => Err(CliError::Usage(format!(
                "`{target}` matches {} closed sessions: {}",
                many.len(),
                many.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    pub(super) async fn restart(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionRestart(SessionRef {
                    session_id: session.id,
                }),
            )
            .await?;
        self.announce(&result.session, &result)
    }

    pub(super) async fn rename(&self, args: RenameArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, &args.target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionRename(SessionRename {
                    session_id: session.id,
                    title: args.title,
                }),
            )
            .await?;
        self.print(&result, || {
            format!("renamed {} to {}", result.session.id, result.session.title)
        })
    }

    pub(super) async fn history(&self, args: HistoryArgs) -> Result<(), CliError> {
        let target = args
            .target
            .ok_or_else(|| CliError::Usage("history needs a session".to_owned()))?;
        let client = self.connect().await?;
        let session = self.resolve(&client, &target).await?;
        if session.kind == hutan_core::SessionKind::Harness {
            return Err(CliError::Failed(format!(
                "session {} is a chat session; `hutan chat log` prints its history",
                session.id
            )));
        }
        let terminal = session
            .terminal_id
            .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))?;
        let want = usize::try_from(args.lines).unwrap_or(usize::MAX);
        let lines = crate::tail::terminal_tail(&client, terminal, want)
            .await
            .map_err(|e| self.fail(e))?;
        let lines = &lines[..];
        if self.json {
            print_line(&serde_json::json!({"session_id": session.id, "lines": lines}).to_string())
        } else {
            lines.iter().try_for_each(|line| print_line(line))
        }
    }

    // ---- helpers shared with attach -------------------------------------------------------

    pub(super) async fn session_list(
        &self,
        client: &Client,
        workspace_id: Option<hutan_core::WorkspaceId>,
    ) -> Result<SessionList, CliError> {
        self.call(
            client,
            Request::SessionList(SessionListFilter { workspace_id }),
        )
        .await
    }

    pub(super) async fn resolve(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<SessionInfo, CliError> {
        let list = self.session_list(client, None).await?;
        resolve_target(&list.sessions, target).cloned()
    }

    /// `session <id> terminal <tid>` (or the JSON of `value`).
    fn announce(
        &self,
        session: &SessionInfo,
        value: &impl serde::Serialize,
    ) -> Result<(), CliError> {
        self.print(value, || match session.terminal_id {
            Some(terminal) => format!("session {} terminal {terminal}", session.id),
            None => format!("session {}", session.id),
        })
    }
}

/// Tells where an unusable workspace name came from when it was not typed on the command line.
pub(super) fn with_source(error: CliError, target: &str, source: &str) -> CliError {
    match error {
        CliError::Failed(message) if source != "--ws" => {
            CliError::Failed(format!("{message} (`{target}` comes from ${source})"))
        }
        other => other,
    }
}

/// The terminal of a session that can still take input and be attached live.
pub(super) fn live_terminal(session: &SessionInfo) -> Result<TerminalId, CliError> {
    if session.kind == hutan_core::SessionKind::Harness {
        return Err(CliError::Failed(format!(
            "session {} is a chat session; use `hutan chat send|attach|log`",
            session.id
        )));
    }
    if !session.state.is_live() {
        return Err(CliError::Failed(format!(
            "session {} is {}; `hutan restart` starts it again, `hutan history` shows its output",
            session.id,
            state_text(&session.state)
        )));
    }
    session
        .terminal_id
        .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))
}

pub(super) fn require_tty() -> Result<(), CliError> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(CliError::Usage(
            "attaching needs a terminal on stdin and stdout".to_owned(),
        ))
    }
}

pub(super) fn host_size() -> Result<TerminalSize, CliError> {
    let (cols, rows) = host::size()?;
    Ok(terminal_size(cols, rows))
}

pub(super) fn terminal_size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Picks one session. Tried in order: full session id, `t:<terminal id>`, exact title, session
/// id prefix of at least [`MIN_PREFIX`] characters. A title or prefix matching several sessions
/// is a usage error that lists the candidates; no match is a plain failure.
pub(super) fn resolve_target<'a>(
    sessions: &'a [SessionInfo],
    target: &str,
) -> Result<&'a SessionInfo, CliError> {
    if let Ok(id) = target.parse::<SessionId>()
        && let Some(session) = sessions.iter().find(|s| s.id == id)
    {
        return Ok(session);
    }
    if let Some(number) = target.strip_prefix("t:") {
        let terminal: TerminalId = number.parse().map_err(|_| {
            CliError::Usage(format!("`{target}` is not a terminal id (t:<number>)"))
        })?;
        return sessions
            .iter()
            .find(|s| s.terminal_id == Some(terminal))
            .ok_or_else(|| not_found(target));
    }
    let by_title: Vec<_> = sessions.iter().filter(|s| s.title == target).collect();
    if !by_title.is_empty() {
        return unique(target, by_title);
    }
    if target.len() >= MIN_PREFIX {
        let prefix = target.to_ascii_uppercase();
        let by_prefix: Vec<_> = sessions
            .iter()
            .filter(|s| s.id.to_string().starts_with(&prefix))
            .collect();
        if !by_prefix.is_empty() {
            return unique(target, by_prefix);
        }
    }
    Err(not_found(target))
}

fn unique<'a>(
    target: &str,
    mut matches: Vec<&'a SessionInfo>,
) -> Result<&'a SessionInfo, CliError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => {
            let mut message = format!("`{target}` matches {n} sessions; use a longer id:");
            for session in matches {
                message.push_str(&format!(
                    "\n  {}  {:<5} {}",
                    session.id,
                    terminal_text(session),
                    session.title
                ));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// What `session.close` found running, in words.
fn busy_text(busy: &hutan_protocol::messages::SessionBusy) -> String {
    match (busy.state.as_str(), &busy.agent, &busy.command) {
        ("turn", ..) => "a reply is being written".to_owned(),
        ("approval", ..) => "a request is waiting for your answer".to_owned(),
        (state, Some(agent), command) => {
            let what = if state == "blocked" {
                "waiting for you"
            } else {
                "working"
            };
            match command {
                Some(command) => format!("{agent} is {what} (`{command}` is running)"),
                None => format!("{agent} is {what}"),
            }
        }
        (_, None, Some(command)) => format!("`{command}` is still running"),
        (_, None, None) => "something is running".to_owned(),
    }
}

fn not_found(target: &str) -> CliError {
    let hint = if target.len() < MIN_PREFIX && !target.starts_with("t:") {
        format!(" (an id prefix needs at least {MIN_PREFIX} characters)")
    } else {
        String::new()
    };
    CliError::Failed(format!(
        "no session matches `{target}`{hint}; `hutan ls` lists sessions"
    ))
}

fn terminal_text(session: &SessionInfo) -> String {
    session
        .terminal_id
        .map_or_else(|| "-".to_owned(), |t| format!("t:{t}"))
}

pub(super) fn state_text(state: &SessionState) -> String {
    match state {
        SessionState::Starting => "starting".to_owned(),
        SessionState::Running => "running".to_owned(),
        SessionState::Idle => "idle".to_owned(),
        SessionState::WaitingApproval => "waiting approval".to_owned(),
        SessionState::Exited {
            code: Some(code), ..
        } => format!("exited ({code})"),
        SessionState::Exited {
            code: None,
            signal: Some(signal),
        } => format!("exited (signal {signal})"),
        SessionState::Exited { .. } => "exited".to_owned(),
        SessionState::Failed { reason } => format!("failed: {reason}"),
        SessionState::Stopped => "stopped".to_owned(),
    }
}

/// The state column of `hutan ls`: ended sessions loaded from a previous daemon run say so,
/// because their final screen is gone; a session whose agent (or one of its subagents) waits
/// for the user says `blocked`, naming the subagent.
fn listed_state_text(session: &SessionInfo) -> String {
    let mut state = state_text(&session.state);
    if session.restored {
        state = format!("{state}, restored");
    }
    match session.agent.as_ref() {
        Some(agent) if agent.state == AgentState::Blocked => match &agent.blocked_sub {
            Some(sub) => format!("{state}, blocked (sub: {sub})"),
            None => format!("{state}, blocked"),
        },
        _ => state,
    }
}

/// The recovery column of `hutan ls`: what the last daemon restart did to the session
/// (`respawned`, `resumed`, `interrupted`, `not restarted`, `ended before`); `-` when it did
/// nothing worth saying or the user dismissed it.
fn recovery_text(session: &SessionInfo) -> String {
    match hutan_client::recovery::chip(session) {
        Some(chip) => chip.label.to_owned(),
        None if session.recovery.is_some() => "ended before".to_owned(),
        None => "-".to_owned(),
    }
}

/// The placement column of `hutan ls`: `main` or `wt <branch>` (`wt (detached)` without a
/// branch), `⚠ shared` added while another live agent works in the same checkout; `-` when the
/// session is not in a git checkout.
pub(super) fn placement_text(session: &SessionInfo) -> String {
    let Some(placement) = &session.placement else {
        return "-".to_owned();
    };
    let mut text = match placement.kind {
        PlacementKind::Main => "main".to_owned(),
        PlacementKind::Worktree => match &placement.branch {
            Some(branch) => format!("wt {branch}"),
            None => "wt (detached)".to_owned(),
        },
    };
    if !placement.shared_with.is_empty() {
        text.push_str(" ⚠ shared");
    }
    text
}

/// The folder column of `hutan ls`: where the session is now (its shell's reported directory),
/// else where it started; `-` while unknown.
fn folder_text(session: &SessionInfo) -> String {
    session
        .folder
        .as_ref()
        .or(session.cwd.as_ref())
        .map_or_else(|| "-".to_owned(), |folder| folder.display().to_string())
}

fn session_table(
    sessions: &[SessionInfo],
    projects: &HashMap<hutan_core::WorkspaceId, String>,
    now_ms: u64,
) -> String {
    if sessions.is_empty() {
        return "no sessions".to_owned();
    }
    let rows: Vec<[String; 9]> = sessions
        .iter()
        .map(|s| {
            [
                s.id.to_string(),
                terminal_text(s),
                listed_state_text(s),
                recovery_text(s),
                projects
                    .get(&s.workspace_id)
                    .cloned()
                    .unwrap_or_else(|| "-".to_owned()),
                placement_text(s),
                s.title.clone(),
                folder_text(s),
                age_text(now_ms.saturating_sub(s.created_at_ms) / 1000),
            ]
        })
        .collect();
    let header = [
        "ID", "TERMINAL", "STATE", "RECOVERY", "PROJECT", "PLACE", "TITLE", "FOLDER", "AGE",
    ]
    .map(str::to_owned);
    let mut widths = [0; 9];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut table = String::new();
    for (index, row) in std::iter::once(&header).chain(&rows).enumerate() {
        if index > 0 {
            table.push('\n');
        }
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths).take(8) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        line.push_str(&row[8]);
        table.push_str(&line);
    }
    table
}

/// `5s`, `3m`, `2h`, `4d`: one unit, rounded down.
pub(super) fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_core::WorkspaceId;

    use super::*;

    fn session(id: &str, terminal: u64, title: &str) -> SessionInfo {
        SessionInfo {
            containment: None,
            id: id.parse().unwrap(),
            workspace_id: WorkspaceId::new(),
            kind: hutan_core::SessionKind::Terminal,
            title: title.to_owned(),
            state: SessionState::Running,
            command: vec![],
            cwd: Some(PathBuf::from("/tmp")),
            terminal_id: Some(TerminalId(terminal)),
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";
    const C: &str = "01K0000000000000000000000C";

    fn fixture() -> Vec<SessionInfo> {
        vec![
            session(A, 1, "alpha"),
            session(B, 2, "beta"),
            session(C, 3, "alpha"),
        ]
    }

    #[test]
    fn resolves_full_id_terminal_title_and_prefix() {
        let sessions = fixture();
        assert_eq!(resolve_target(&sessions, A).unwrap().id.to_string(), A);
        assert_eq!(resolve_target(&sessions, "t:2").unwrap().id.to_string(), B);
        assert_eq!(resolve_target(&sessions, "beta").unwrap().id.to_string(), B);
        assert_eq!(
            resolve_target(&sessions, "01k0").unwrap().id.to_string(),
            C,
            "prefixes are case-insensitive"
        );
    }

    #[test]
    fn ambiguous_targets_are_usage_errors_listing_candidates() {
        let sessions = fixture();
        for target in ["01J0", "alpha"] {
            let error = resolve_target(&sessions, target).unwrap_err();
            assert!(matches!(error, CliError::Usage(_)), "{target}: {error:?}");
            assert_eq!(error.exit_code(), 2);
            let text = error.to_string();
            assert!(text.contains(A) && text.contains("t:1"), "{text}");
        }
    }

    #[test]
    fn unmatched_targets_fail_without_guessing() {
        let sessions = fixture();
        for target in ["nope", "t:9", "01J", "gamma"] {
            let error = resolve_target(&sessions, target).unwrap_err();
            assert!(matches!(error, CliError::Failed(_)), "{target}: {error:?}");
        }
        assert!(matches!(
            resolve_target(&sessions, "t:x"),
            Err(CliError::Usage(_))
        ));
        assert!(
            resolve_target(&sessions, "01J")
                .unwrap_err()
                .to_string()
                .contains("at least 4")
        );
    }

    #[test]
    fn placement_badges_say_main_worktree_detached_and_shared() {
        use hutan_protocol::worktree::Placement;
        let mut s = session(A, 1, "t");
        assert_eq!(placement_text(&s), "-");
        let mut placement = Placement {
            kind: PlacementKind::Main,
            root: PathBuf::from("/repo"),
            branch: Some("master".to_owned()),
            shared_with: vec![],
        };
        s.placement = Some(placement.clone());
        assert_eq!(placement_text(&s), "main");
        placement.kind = PlacementKind::Worktree;
        placement.branch = Some("feature/x".to_owned());
        s.placement = Some(placement.clone());
        assert_eq!(placement_text(&s), "wt feature/x");
        placement.branch = None;
        s.placement = Some(placement.clone());
        assert_eq!(placement_text(&s), "wt (detached)");
        placement.shared_with = vec![B.parse().unwrap()];
        s.placement = Some(placement);
        assert_eq!(placement_text(&s), "wt (detached) ⚠ shared");

        s.folder = Some(PathBuf::from("/tmp/deeper"));
        let projects = HashMap::from([(s.workspace_id, "hutan".to_owned())]);
        let table = session_table(&[s], &projects, 0);
        let header: Vec<_> = table.lines().next().unwrap().split_whitespace().collect();
        assert_eq!(
            header,
            [
                "ID", "TERMINAL", "STATE", "RECOVERY", "PROJECT", "PLACE", "TITLE", "FOLDER", "AGE"
            ]
        );
        let row = table.lines().nth(1).unwrap();
        assert!(
            row.contains("hutan") && row.contains("/tmp/deeper"),
            "{table}"
        );
        assert!(
            !row.contains("/tmp "),
            "the folder, not the start directory: {table}"
        );
        assert!(table.contains("wt (detached) ⚠ shared"), "{table}");
    }

    #[test]
    fn ages_round_down_to_one_unit() {
        assert_eq!(age_text(5), "5s");
        assert_eq!(age_text(125), "2m");
        assert_eq!(age_text(7200), "2h");
        assert_eq!(age_text(200_000), "2d");
    }

    #[test]
    fn restored_sessions_are_marked_in_the_state_column() {
        let mut restored = session("01K7F3B5N2Q9W8E4R6T1Y0V3KM", 1, "t");
        restored.state = SessionState::Stopped;
        assert_eq!(listed_state_text(&restored), "stopped");
        restored.restored = true;
        assert_eq!(listed_state_text(&restored), "stopped, restored");
    }

    #[test]
    fn states_show_exit_codes() {
        assert_eq!(
            state_text(&SessionState::Exited {
                code: Some(3),
                signal: None
            }),
            "exited (3)"
        );
        assert_eq!(
            state_text(&SessionState::Exited {
                code: None,
                signal: Some(9)
            }),
            "exited (signal 9)"
        );
    }
}
