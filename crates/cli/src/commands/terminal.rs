//! `hutan terminal search`: find text in a session's terminal output, scrollback included
//! (protocol 0.43, `docs/protocol.md` § Terminal search).

use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchMatch};

use super::{Ctx, print_line};
use crate::args::{TerminalCommand, TerminalSearchArgs};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn terminal(&self, command: TerminalCommand) -> Result<(), CliError> {
        match command {
            TerminalCommand::Search(args) => self.terminal_search(args).await,
        }
    }

    async fn terminal_search(&self, args: TerminalSearchArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_terminal_search() {
            return Err(CliError::Failed(
                "this hutand cannot search a terminal (it is older than protocol 0.43); restart \
                 it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        let session = self.resolve(&client, &args.target).await?;
        if session.kind == hutan_core::SessionKind::Harness {
            return Err(CliError::Failed(format!(
                "session {} is a chat session; `hutan chat log` prints its history",
                session.id
            )));
        }
        let terminal = session
            .terminal_id
            .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))?;
        let result = client
            .terminal_search(TerminalSearch {
                terminal_id: terminal,
                query: args.query,
                case: args.case,
                regex: args.regex,
                limit: args.limit,
                from: args.from,
                block_id: args.block,
                text: true,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return self.print(
                &serde_json::json!({
                    "session_id": session.id,
                    "terminal_id": terminal,
                    "matches": result.matches,
                    "truncated": result.truncated,
                    "next_from": result.next_from,
                    "timed_out": result.timed_out,
                    "viewport_top": result.viewport_top,
                    "first_row": result.first_row,
                }),
                String::new,
            );
        }
        for line in lines(&result.matches) {
            print_line(&line)?;
        }
        if let Some(next) = result.next_from.filter(|_| result.truncated) {
            eprintln!(
                "{}: more to search; continue with --from {next}",
                if result.timed_out {
                    "stopped at the time limit"
                } else {
                    "stopped at the limit"
                }
            );
        }
        Ok(())
    }
}

/// `row: line` for each row a match starts on (its absolute number), the logical line it is in;
/// several matches on one row print once. Control characters of the line are replaced so that output
/// that was escape sequences cannot drive the reader's own terminal.
fn lines(matches: &[TerminalSearchMatch]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<u64> = None;
    for found in matches {
        let text = found.line.as_deref().unwrap_or_default();
        // Matches that start on one row are in one line.
        if last == Some(found.row) {
            continue;
        }
        last = Some(found.row);
        let safe: String = text
            .chars()
            .map(|c| if c.is_control() { '\u{fffd}' } else { c })
            .collect();
        out.push(format!("{}: {safe}", found.row));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(row: u64, line: &str) -> TerminalSearchMatch {
        TerminalSearchMatch {
            row,
            col: 0,
            end_row: row,
            end_col: 1,
            block_id: None,
            line: Some(line.to_owned()),
        }
    }

    #[test]
    fn a_row_with_several_matches_prints_once_and_controls_are_replaced() {
        let matches = [
            found(3, "a needle and a needle"),
            found(3, "a needle and a needle"),
            found(4, "needle"),
            found(5, "needle"),
            found(6, "x\u{1b}[31mneedle"),
        ];
        assert_eq!(
            lines(&matches),
            [
                "3: a needle and a needle",
                "4: needle",
                "5: needle",
                "6: x\u{fffd}[31mneedle"
            ]
        );
    }
}
