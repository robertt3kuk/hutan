//! `hutan search`, `hutan index …` and `hutan history sessions`: the session history index
//! (roadmap M17) and the session catalog built on it.

use std::collections::HashMap;
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_client::Client;
use hutan_client::time::local_time_text;
use hutan_core::WorkspaceId;
use hutan_protocol::history::{
    HistoryHit, HistoryHits, HistoryScope, HistorySearch, HistorySessionSort, HistorySessions,
    HistorySessionsRequest, HistorySetEnabled, HistorySource, HistoryStatusInfo,
};
use hutan_protocol::messages::{Empty, Request, WorkspaceInfo};
use serde_json::Value;

use super::Ctx;
use super::workspaces::{resolve_workspace, workspace_target};
use crate::args::{
    HistorySessionsArgs, IndexCommand, NewArgs, ScopeArg, SearchArgs, SessionSortArg, SourceArg,
};
use crate::error::CliError;

/// Longest title in the `history sessions` table, in characters.
const TITLE_WIDTH: usize = 60;

impl Ctx {
    /// A connection to a daemon that speaks the `history.*` requests.
    async fn connect_history(&self) -> Result<Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_history() {
            return Err(CliError::Failed(
                "this hutand does not offer history search (protocol 0.8); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    pub(super) async fn search(&self, args: SearchArgs) -> Result<(), CliError> {
        let client = self.connect_history().await?;
        let request = HistorySearch {
            query: args.query.join(" "),
            limit: args.limit,
            sources: args.source.iter().map(|s| source_of(*s)).collect(),
            roles: Vec::new(),
            project: None,
            since_ms: args.since.map(|age| now_ms().saturating_sub(age)),
            until_ms: None,
        };
        let found: HistoryHits = self.call(&client, Request::HistorySearch(request)).await?;
        if let Some(number) = args.resume {
            drop(client);
            return self.resume_hit(&found.hits, number).await;
        }
        self.print(&found, || {
            hits_text(&found.hits, std::io::stdout().is_terminal())
        })
    }

    /// `hutan history sessions`: the session catalog as a table (or JSON).
    pub(super) async fn history_sessions(&self, args: HistorySessionsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_history_sessions() {
            return Err(CliError::Failed(
                "this hutand has no session catalog (protocol 0.29); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        let workspaces = self.workspace_list(&client).await?.workspaces;
        let scope = match (args.scope, &args.workspace) {
            (Some(ScopeArg::All), _) | (None, None) => HistoryScope::All,
            (Some(ScopeArg::Workspace) | None, _) => HistoryScope::Workspace,
            (Some(ScopeArg::Project), _) => HistoryScope::Project,
        };
        let workspace_id = if scope == HistoryScope::All {
            None
        } else {
            let (target, _) = workspace_target(args.workspace).ok_or_else(|| {
                CliError::Usage(
                    "--scope workspace and --scope project need --ws <id|name> (or $HUTAN_WORKSPACE)"
                        .to_owned(),
                )
            })?;
            Some(resolve_workspace(&workspaces, &target)?.id)
        };
        let request = HistorySessionsRequest {
            scope,
            workspace_id,
            filter: args.filter.unwrap_or_default(),
            sort: match args.sort {
                SessionSortArg::Updated => HistorySessionSort::Updated,
                SessionSortArg::Created => HistorySessionSort::Created,
            },
            sources: args.source.iter().map(|s| source_of(*s)).collect(),
            limit: args.limit,
            refresh: args.refresh,
        };
        let answer: HistorySessions = self
            .call(&client, Request::HistorySessions(request))
            .await?;
        self.print(&answer, || sessions_text(&answer, &workspaces, now_ms()))
    }

    /// Opens hit `number` (1-based) in a new session: its working directory, its resume command.
    async fn resume_hit(&self, hits: &[HistoryHit], number: u32) -> Result<(), CliError> {
        let hit = usize::try_from(number)
            .ok()
            .and_then(|n| hits.get(n.checked_sub(1)?))
            .ok_or_else(|| {
                CliError::Failed(format!(
                    "no hit {number}: the search found {} session(s)",
                    hits.len()
                ))
            })?;
        let Some(argv) = hit.resume_argv.clone() else {
            return Err(CliError::Failed(format!(
                "hit {number} ({} session {}) cannot be resumed",
                hit.source.as_str(),
                hit.session_key
            )));
        };
        // The agent's recorded directory may be gone (a deleted worktree); the session then
        // starts where the command was run.
        let cwd = match hit.cwd.as_deref().map(PathBuf::from) {
            Some(dir) if dir.is_dir() => Some(dir),
            Some(dir) => {
                eprintln!(
                    "hutan: {} no longer exists; resuming in the current directory",
                    dir.display()
                );
                None
            }
            None => None,
        };
        let title = hit
            .title
            .as_deref()
            .map_or_else(|| argv.join(" "), |title| format!("resume: {title}"));
        let attach = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        self.new_session(NewArgs {
            dirs: Vec::new(),
            workspace: None,
            cwd,
            title: Some(title),
            env: Vec::new(),
            attach,
            worktree: None,
            contain: false,
            network: None,
            write: Vec::new(),
            argv,
        })
        .await
    }

    pub(super) async fn index(&self, command: IndexCommand) -> Result<(), CliError> {
        match command {
            IndexCommand::Status => {
                let client = self.connect_history().await?;
                let info: HistoryStatusInfo =
                    self.call(&client, Request::HistoryStatus(Empty {})).await?;
                self.print(&info, || status_text(&info))
            }
            IndexCommand::Enable | IndexCommand::Disable => {
                let enabled = matches!(command, IndexCommand::Enable);
                // Enabling is the one history command that may start the daemon.
                let (client, _) = self.ensure_daemon().await?;
                if !client.supports_history() {
                    return Err(CliError::Failed(
                        "this hutand does not offer history search (protocol 0.8); \
                         restart it with `hutan daemon restart`"
                            .to_owned(),
                    ));
                }
                let info: HistoryStatusInfo = self
                    .call(
                        &client,
                        Request::HistorySetEnabled(HistorySetEnabled { enabled }),
                    )
                    .await?;
                self.print(&info, || status_text(&info))
            }
            IndexCommand::Clear => {
                let client = self.connect_history().await?;
                let _: Value = self.call(&client, Request::HistoryClear(Empty {})).await?;
                self.print(&serde_json::json!({"cleared": true}), || {
                    "history index cleared".to_owned()
                })
            }
            IndexCommand::Rebuild => {
                let client = self.connect_history().await?;
                let _: Value = self
                    .call(&client, Request::HistoryRebuild(Empty {}))
                    .await?;
                self.print(&serde_json::json!({"rebuilding": true}), || {
                    "rebuilding the history index in the background; `hutan index status` shows progress"
                        .to_owned()
                })
            }
        }
    }
}

fn source_of(source: SourceArg) -> HistorySource {
    match source {
        SourceArg::Hutan => HistorySource::Hutan,
        SourceArg::Claude => HistorySource::Claude,
        SourceArg::Codex => HistorySource::Codex,
        SourceArg::Omp => HistorySource::Omp,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The hit list for people: one block per session, matches in bold on a terminal.
pub(crate) fn hits_text(hits: &[HistoryHit], color: bool) -> String {
    if hits.is_empty() {
        return "no matches".to_owned();
    }
    let mut out = String::new();
    for (index, hit) in hits.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let when = hit.ts_ms.map_or_else(|| "-".to_owned(), local_time_text);
        let title = hit.title.as_deref().unwrap_or("(untitled)");
        let more = if hit.matches > 1 {
            format!(", {} matches", hit.matches)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{:>2}  {:<6} {when}  {title}  ({}{more})\n",
            index + 1,
            hit.source.as_str(),
            hit.role.as_str(),
        ));
        if let Some(cwd) = &hit.cwd {
            out.push_str(&format!("    {cwd}\n"));
        }
        out.push_str(&format!("    {}\n", snippet_text(&hit.snippet, color)));
        if let Some(argv) = &hit.resume_argv {
            out.push_str(&format!("    resume: {}\n", argv.join(" ")));
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// One line; the daemon's `«match»` markers become bold on a terminal and stay as they are
/// otherwise. Control characters in indexed text never reach the terminal.
fn snippet_text(snippet: &str, color: bool) -> String {
    let mut out = String::with_capacity(snippet.len());
    for c in snippet.chars() {
        match c {
            '«' if color => out.push_str("\x1b[1m"),
            '»' if color => out.push_str("\x1b[0m"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Text from a transcript, safe to print: escape sequences (CSI, OSC such as OSC 52 clipboard
/// writes, two-character escapes) are dropped whole and every other control character,
/// line breaks included, becomes a space.
pub(crate) fn terminal_safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek().copied() {
                // CSI: parameters and intermediates, then a final byte 0x40-0x7e.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC / DCS / SOS / PM / APC: up to BEL or the string terminator `ESC \`.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// At most `max` characters of `text`, ending in `…` when it was cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `3m`, `2h`, `5d`: how long before `now_ms` the session was last active.
fn age_short(now_ms: u64, then_ms: u64) -> String {
    super::sessions::age_text(now_ms.saturating_sub(then_ms) / 1000)
}

/// The catalog for people: one line per session (agent, title, messages, age, model, project)
/// and a footer with the counts and the scan state. Everything that came from a transcript goes
/// through [`terminal_safe`].
fn sessions_text(answer: &HistorySessions, workspaces: &[WorkspaceInfo], now_ms: u64) -> String {
    let names: HashMap<WorkspaceId, &str> =
        workspaces.iter().map(|w| (w.id, w.name.as_str())).collect();
    let mut rows: Vec<[String; 6]> = vec![[
        "AGENT".to_owned(),
        "TITLE".to_owned(),
        "MSGS".to_owned(),
        "AGE".to_owned(),
        "MODEL".to_owned(),
        "PROJECT".to_owned(),
    ]];
    for session in &answer.sessions {
        let title = session
            .title
            .as_deref()
            .or(session.first_prompt.as_deref())
            .unwrap_or("(untitled)");
        rows.push([
            session.source.as_str().to_owned(),
            // A closed Hutan session: `hutan reopen <id>` brings it back.
            if session.closed {
                format!(
                    "{} [closed]",
                    clip(terminal_safe(title).trim(), TITLE_WIDTH)
                )
            } else {
                clip(terminal_safe(title).trim(), TITLE_WIDTH)
            },
            session.messages.to_string(),
            session
                .updated_ms
                .map_or_else(|| "-".to_owned(), |ms| age_short(now_ms, ms)),
            terminal_safe(session.model.as_deref().unwrap_or("-")),
            session
                .workspace_id
                .and_then(|id| names.get(&id))
                .map_or_else(|| "-".to_owned(), |name| terminal_safe(name)),
        ]);
    }
    let mut widths = [0_usize; 5];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    if answer.sessions.is_empty() {
        out.push_str("no sessions\n");
    } else {
        for row in &rows {
            let mut line = String::new();
            for (column, (cell, width)) in row.iter().zip(widths).enumerate() {
                if column == 2 {
                    // Message counts line up on the right.
                    line.push_str(&format!("{cell:>width$}  "));
                } else {
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            line.push_str(&row[5]);
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out.push_str(&format!("{} of {} sessions", answer.matched, answer.loaded));
    if (answer.sessions.len() as u64) < answer.matched {
        out.push_str(&format!(" (showing {})", answer.sessions.len()));
    }
    let scan = &answer.scan;
    if scan.scanning {
        out.push_str(&format!(
            " · scanning {} / {} files…",
            scan.done, scan.files
        ));
    } else if let Some(finished) = scan.finished_ms {
        out.push_str(&format!(
            " · scanned {} files {}",
            scan.files,
            hutan_client::time::relative_text(now_ms, finished)
        ));
    }
    out
}

fn status_text(info: &HistoryStatusInfo) -> String {
    let mut out = if info.enabled {
        "history index: enabled".to_owned()
    } else {
        "history index: disabled (turn it on with `hutan index enable`)".to_owned()
    };
    for source in &info.sources {
        let last = source
            .last_indexed_ms
            .map_or_else(|| "never".to_owned(), local_time_text);
        out.push_str(&format!(
            "\n  {:<7} {:>6} file(s) {:>9} row(s)  last {last}{}",
            source.name,
            source.files,
            source.rows,
            if source.errors > 0 {
                format!("  {} unreadable line(s)", source.errors)
            } else {
                String::new()
            }
        ));
    }
    out.push_str(&format!(
        "\n  database {:.1} MiB",
        info.db_bytes as f64 / (1024.0 * 1024.0)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::history::{
        HistoryRole, HistoryScanInfo, HistorySessionRow, HistorySourceFiles, HistoryTurn,
    };

    #[test]
    fn terminal_safe_drops_escape_sequences_and_controls() {
        assert_eq!(
            terminal_safe("a\u{1b}[31mred\u{1b}[0m b\u{1b}]52;c;aGVsbG8=\u{7}c\u{1b}]0;t\u{1b}\\d"),
            "ared bcd"
        );
        assert_eq!(
            terminal_safe("one\ntwo\r\tthree\u{0}x\u{9b}y"),
            "one two  three x y"
        );
        assert_eq!(terminal_safe("lone \u{1b}"), "lone ");
        assert_eq!(terminal_safe("ünïcode ✓"), "ünïcode ✓");
    }

    fn session(key: &str, title: Option<&str>, messages: u32, ago_ms: u64) -> HistorySessionRow {
        HistorySessionRow {
            source: HistorySource::Omp,
            session_key: key.into(),
            title: title.map(str::to_owned),
            cwd: None,
            branch: None,
            model: Some("gpt-5.6-luna".into()),
            messages,
            created_ms: None,
            updated_ms: Some(1_000_000_000_000 - ago_ms),
            first_prompt: Some("the first prompt".into()),
            latest: vec![HistoryTurn {
                role: HistoryRole::User,
                text: "x".into(),
                ts_ms: None,
            }],
            workspace_id: None,
            resume_argv: None,
            path: None,
            session_id: None,
            closed: false,
        }
    }

    fn answer(sessions: Vec<HistorySessionRow>, matched: u64, scanning: bool) -> HistorySessions {
        HistorySessions {
            sessions,
            matched,
            loaded: 250,
            scan: HistoryScanInfo {
                files: 986,
                done: 100,
                scanning,
                finished_ms: Some(1_000_000_000_000 - 120_000),
                per_source: vec![HistorySourceFiles {
                    name: "omp".into(),
                    files: 986,
                }],
            },
        }
    }

    #[test]
    fn the_catalog_table_aligns_columns_and_counts() {
        let long = "T".repeat(100);
        let mut bare = session("c", None, 0, 90_000_000);
        bare.first_prompt = None;
        let rows = vec![
            session("a", Some("Short"), 8, 180_000),
            session("b", Some(&long), 1234, 7_200_000),
            bare,
        ];
        let text = sessions_text(&answer(rows, 3, false), &[], 1_000_000_000_000);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert!(lines[0].starts_with("AGENT"), "{text}");
        assert!(
            lines[1].contains("Short") && lines[1].contains("3m"),
            "{text}"
        );
        assert!(lines[2].contains("2h"), "{text}");
        let clipped = format!("{}…", "T".repeat(TITLE_WIDTH - 1));
        assert!(lines[2].contains(&clipped), "{text}");
        assert!(
            lines[3].contains("1d") && lines[3].contains("(untitled)"),
            "{text}"
        );
        // The model column starts at the same place on every row.
        let at = |line: &str| {
            line.split("gpt-5.6-luna")
                .next()
                .map(|head| head.chars().count())
        };
        assert_eq!(at(lines[1]), at(lines[2]));
        assert_eq!(lines[4], "3 of 250 sessions · scanned 986 files 2 min ago");

        let cut = sessions_text(
            &answer(vec![session("a", Some("x"), 1, 0)], 5, true),
            &[],
            1_000_000_000_000,
        );
        assert!(
            cut.ends_with("5 of 250 sessions (showing 1) · scanning 100 / 986 files…"),
            "{cut}"
        );
        let empty = sessions_text(&answer(Vec::new(), 0, false), &[], 1_000_000_000_000);
        assert!(
            empty.starts_with("no sessions\n0 of 250 sessions"),
            "{empty}"
        );
    }

    fn hit() -> HistoryHit {
        HistoryHit {
            source: HistorySource::Claude,
            session_key: "s1".into(),
            title: Some("Scheduler stall".into()),
            cwd: Some("/work/app".into()),
            role: HistoryRole::User,
            ts_ms: Some(1_790_571_682_983),
            snippet: "why does the «quokka»\nscheduler stall".into(),
            score: 4.2,
            matches: 3,
            session_id: None,
            resume_argv: Some(vec!["claude".into(), "--resume".into(), "s1".into()]),
            path: None,
            terminal_id: None,
            block_id: None,
        }
    }

    #[test]
    fn hits_render_plain_and_with_color() {
        // The time is the hit's, in the reader's zone with the offset spelled out.
        let when = hutan_client::time::local_time_text(1_790_571_682_983);
        assert!(when.contains(" +") || when.contains(" -"), "{when}");
        let plain = hits_text(&[hit()], false);
        assert_eq!(
            plain,
            format!(
                " 1  claude {when}  Scheduler stall  (user, 3 matches)\n    /work/app\n    why does the «quokka» scheduler stall\n    resume: claude --resume s1"
            )
        );
        let color = hits_text(&[hit()], true);
        assert!(color.contains("\x1b[1mquokka\x1b[0m"), "{color:?}");
        assert_eq!(hits_text(&[], false), "no matches");
    }
}
